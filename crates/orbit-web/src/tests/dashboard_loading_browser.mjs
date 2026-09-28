// Usage: node dashboard_loading_browser.mjs /path/to/playwright/index.mjs /evidence/directory
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const scenarios = fileURLToPath(new URL('./dashboard_loading.mjs', import.meta.url));

async function assertVisibleTaskRow(page, viewport, pageName) {
  const visible = await page.evaluate(() => {
    const row = document.querySelector('#tasks-body .row[data-key^="task-"]:not(.header)');
    const body = document.getElementById('tasks-body');
    if (!row || !body) return { visible: false, reason: 'task row or body missing' };

    const rowRect = row.getBoundingClientRect();
    const bodyRect = body.getBoundingClientRect();
    const viewportRect = { top: 0, right: window.innerWidth, bottom: window.innerHeight, left: 0 };
    const intersection = (rect, bounds) => ({
      left: Math.max(rect.left, bounds.left),
      top: Math.max(rect.top, bounds.top),
      right: Math.min(rect.right, bounds.right),
      bottom: Math.min(rect.bottom, bounds.bottom),
    });
    const area = (rect) => Math.max(0, rect.right - rect.left) * Math.max(0, rect.bottom - rect.top);
    const bodyIntersection = intersection(rowRect, bodyRect);
    const viewportIntersection = intersection(rowRect, viewportRect);
    const visibleIntersection = intersection(bodyIntersection, viewportIntersection);
    const paintPoint = {
      x: (visibleIntersection.left + visibleIntersection.right) / 2,
      y: (visibleIntersection.top + visibleIntersection.bottom) / 2,
    };
    const topmost = area(visibleIntersection) > 0 ? document.elementFromPoint(paintPoint.x, paintPoint.y) : null;

    return {
      visible: area(bodyIntersection) > 0 && area(viewportIntersection) > 0 && row.contains(topmost),
      row: rowRect.toJSON(),
      body: bodyRect.toJSON(),
      viewport: viewportRect,
      paintTarget: topmost?.className || topmost?.id || null,
    };
  });
  if (!visible.visible) throw new Error(`First task row is clipped on ${pageName} at ${viewport.width}px: ${JSON.stringify(visible)}`);
}

// Every cell of a failed run step must lie inside the run-detail panel and the
// viewport, have width, paint on top, and show its whole text: the panel
// clips overflow, so anything outside it is unreadable.
async function assertReadableFailedStep(page, width) {
  const cells = await page.evaluate(() => {
    const panel = document.getElementById('run-detail-panel').getBoundingClientRect();
    const row = document.querySelector('#run-steps-body .step-row');
    const named = {
      index: row.querySelector('.idx'),
      target: row.querySelector('.target'),
      state: row.querySelector('.state-label'),
      duration: row.querySelector('.duration'),
      exit: row.querySelector('.exit'),
    };
    return Object.entries(named).map(([name, cell]) => {
      const rect = cell.getBoundingClientRect();
      const topmost = document.elementFromPoint((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
      return {
        name,
        text: cell.textContent,
        className: cell.className,
        rect: rect.toJSON(),
        panel: { left: panel.left, right: panel.right },
        inside: rect.width > 0 && rect.height > 0 && rect.left >= panel.left - 0.5 && rect.right <= panel.right + 0.5
          && rect.left >= 0 && rect.right <= window.innerWidth,
        complete: cell.scrollWidth <= cell.clientWidth + 1,
        painted: cell.contains(topmost),
      };
    });
  });
  for (const cell of cells) {
    if (!cell.inside || !cell.complete || !cell.painted) throw new Error(`Failed step ${cell.name} unreadable at ${width}px: ${JSON.stringify(cell)}`);
  }
  const text = Object.fromEntries(cells.map(cell => [cell.name, cell.text]));
  if (text.target !== 'activity:agent_implement' || text.state !== 'failed' || text.duration === '' || text.exit !== '1') {
    throw new Error(`Failed step values missing at ${width}px: ${JSON.stringify(text)}`);
  }
  if (!cells.find(cell => cell.name === 'exit').className.includes('fail')) throw new Error('Nonzero exit code must be marked failed');
}

async function assertRunStepLayout(page) {
  await page.evaluate(async () => {
    const detail = await import('/js/run-detail.js');
    detail.clearExpandedStepIndices();
    detail.setActiveRunDetail({
      run: { run_id: 'jrun-layout', state: 'failed' },
      steps: [{ step_index: 3, target_type: 'activity', target_id: 'agent_implement', state: 'failed', duration_ms: 125000, exit_code: 1, error_code: 'agent_failed', error_message: 'exit status 1' }],
    });
    for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.dataset.tab === 'run-detail');
    document.getElementById('run-steps-body').style.display = '';
    detail.renderRunSteps();
  });
  for (const width of [1280, 480, 390]) {
    await page.setViewportSize({ width, height: 900 });
    await page.locator('#run-steps-body .step-row').scrollIntoViewIfNeeded();
    await assertReadableFailedStep(page, width);
    const row = page.locator('#run-steps-body .step-row');
    await row.click();
    const stepDetail = page.locator('#run-steps-body .step-detail');
    await stepDetail.waitFor({ state: 'visible' });
    if (await row.getAttribute('aria-expanded') !== 'true') throw new Error(`Step row must report expansion at ${width}px`);
    if (!(await stepDetail.textContent()).includes('exit status 1')) throw new Error('Expanded step must show its error');
    await assertReadableFailedStep(page, width);
    if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw new Error(`Expanded step overflows the page at ${width}px`);
    await page.screenshot({ path: path.join(evidence, `run-step-${width}.png`) });
    await row.focus();
    await page.keyboard.press('Enter');
    await stepDetail.waitFor({ state: 'detached' });
    if (await row.getAttribute('aria-expanded') !== 'false') throw new Error(`Step row must collapse from the keyboard at ${width}px`);
  }
}

const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = name === '/test.mjs' ? { data: fs.readFileSync(scenarios), type: 'text/javascript' } : dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const failures = [];
  page.on('pageerror', error => failures.push(error.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=one#tasks`);
  await page.evaluate(() => {
    globalThis.nativeFetch = globalThis.fetch;
    globalThis.setInterval = () => 0;
    globalThis.EventSource = class { close() {} };
  });
  await page.addScriptTag({ type: 'module', url: '/test.mjs' });
  await page.waitForFunction(() => globalThis.loadingTestsPassed, undefined, { timeout: 15000 }).catch(error => {
    throw new Error(`${error.message}\nPage errors: ${failures.join('\n')}`);
  });
  if (failures.length) throw new Error(failures.join('\n'));
  await page.evaluate(() => globalThis.showTaskPaginationEvidence());
  await page.waitForFunction(() => document.getElementById('tasks-count').textContent === '1–20 of 55');
  for (const viewport of [{ name: 'desktop', width: 1280 }, { name: 'mobile', width: 390 }]) {
    await page.setViewportSize({ width: viewport.width, height: 900 });
    await page.evaluate(() => {
      window.scrollTo(0, 0);
      document.getElementById('tasks-body').scrollTop = 0;
    });
    const pager = page.locator('.task-pagination');
    if (!(await pager.isVisible())) throw new Error(`Task pagination invisible at ${viewport.width}px`);
    if (!(await page.locator('#tasks-next').isEnabled())) throw new Error('First task page must enable Next');
    if (await page.locator('#tasks-previous').isEnabled()) throw new Error('First task page must disable Previous');
    await assertVisibleTaskRow(page, viewport, 'page 1');
    await page.screenshot({ path: path.join(evidence, `task-pagination-${viewport.name}.png`), fullPage: true });
  }
  await page.evaluate(() => { document.getElementById('tasks-body').scrollTop = 120; });
  await page.locator('#tasks-next').click();
  await page.waitForFunction(() => document.getElementById('tasks-count').textContent === '21–40 of 55');
  if (await page.locator('#tasks-body').evaluate((body) => body.scrollTop !== 0)) throw new Error('Task page navigation must reset the task-body scroll position');
  if (!(await page.locator('#tasks-previous').isEnabled())) throw new Error('Second task page must enable Previous');
  for (const viewport of [{ name: 'desktop', width: 1280 }, { name: 'mobile', width: 390 }]) {
    await page.setViewportSize({ width: viewport.width, height: 900 });
    await page.evaluate(() => {
      window.scrollTo(0, 0);
      document.getElementById('tasks-body').scrollTop = 0;
    });
    await assertVisibleTaskRow(page, viewport, 'page 2');
    await page.screenshot({ path: path.join(evidence, `task-pagination-${viewport.name}-page-2.png`), fullPage: true });
  }
  await page.locator('#task-filter .chip[data-status="done"]').click();
  await page.waitForFunction(() => document.getElementById('task-filter-summary').textContent.includes('done'));
  await page.locator('#task-filter .chip[data-role="all"]').click();
  const firstTask = page.locator('#tasks-body .row[data-key^="task-"]:not(.header)').first();
  // Open it the way a person does, by its title: on a narrow row the centre
  // of the box is the crew select, which takes the click for itself.
  await firstTask.locator('.title').click();
  const detail = page.locator('#tasks-body .row-detail').first();
  await detail.waitFor({ state: 'visible' });
  const pageOverflowsHorizontally = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  if (pageOverflowsHorizontally) throw new Error('Task filters or expanded details introduced horizontal page clipping');
  await page.evaluate(() => globalThis.showDiagnosticsEvidence());
  // Hold a real visible panel in refresh, then inspect its rendered accessible
  // feedback and retry affordance at desktop and narrow widths.
  await page.evaluate(() => {
    globalThis.fetch = (_path, options) => new Promise((_resolve, reject) => {
      options?.signal?.addEventListener('abort', () => reject(new DOMException('Timed out', 'AbortError')));
    });
    document.getElementById('refresh-btn').click();
  });
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 900 });
    const feedback = page.locator('#diag-body [role="status"]');
    if (!(await feedback.isVisible())) throw new Error(`Refresh feedback invisible at ${width}px`);
    if (!(await feedback.textContent()).includes('Refreshing')) throw new Error('Missing retained-data feedback');
    if (await feedback.getAttribute('aria-live') !== 'polite') throw new Error('Missing accessible live feedback');
    if (!(await page.locator('#refresh-btn').isEnabled())) throw new Error('Retry disabled');
    await page.screenshot({ path: path.join(evidence, `refresh-${width}.png`) });
  }
  await assertRunStepLayout(page);
  await new Promise(resolve => server.close(resolve));
  await page.evaluate(() => {
    globalThis.fetch = globalThis.nativeFetch;
    document.getElementById('refresh-btn').click();
  });
  await page.waitForFunction(() => document.getElementById('meta-text').textContent.includes('offline'));
  if (!(await page.locator('#conn-status').getAttribute('class')).includes('red')) throw new Error('Stopped server must show red connection status');
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, scenarios: 'Task pagination page 1/page 2 with visible, unoccluded first rows and accessible Previous/Next at 1280px and 390px; failed run step target, state, duration and exit code readable with click and keyboard expansion at 1280px, 480px and 390px; Tasks, Recent runs, Errors, Operations: cold, stale refresh, scope changes, reordered responses, empty success, network error; Metrics HTTP failure isolation and network offline/recovery' }, null, 2));
  console.log('Chromium dashboard lifecycle and accessible visible feedback passed.');
} finally {
  await browser?.close();
  if (server.listening) await new Promise(resolve => server.close(resolve));
}
