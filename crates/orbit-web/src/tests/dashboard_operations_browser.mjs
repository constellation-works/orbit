// Usage: node dashboard_operations_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const assets = fileURLToPath(new URL('../../assets/dashboard/', import.meta.url));
const test = fileURLToPath(new URL('./dashboard_operations.mjs', import.meta.url));
const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const file = name === '/test.mjs' ? test : path.join(assets, name === '/' ? 'index.html' : path.basename(name));
  if (!fs.existsSync(file)) { res.writeHead(404); res.end(); return; }
  let data = fs.readFileSync(file);
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', file.endsWith('.html') ? 'text/html' : file.endsWith('.css') ? 'text/css' : 'text/javascript');
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const pageOverflow = () => document.documentElement.scrollWidth > window.innerWidth + 1;
const assertNoOverflow = async (label) => {
  const overflow = await page.evaluate(pageOverflow);
  if (overflow) {
    const width = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, inner: window.innerWidth }));
    throw new Error(`Horizontal overflow at ${label}: scrollWidth=${width.scroll} innerWidth=${width.inner}`);
  }
};
let browser;
let page;
try {
  browser = await chromium.launch({headless:true});
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  // Serve the actual markup/styles with only the Operations module initialized.
  // All API traffic is fixture data; no live scheduler or dashboard is contacted.
  page.on('pageerror', error => console.error(error));
  await page.goto(`http://127.0.0.1:${server.address().port}/#operations/routines`);
  await page.addScriptTag({ type: 'module', url: '/test.mjs' });
  await page.waitForFunction(() => globalThis.operationsTestsPassed, undefined, { timeout: 15000 });
  await page.evaluate(() => {
    const workspace = document.getElementById('rail-workspace');
    if (workspace && !document.getElementById('workspace-select')) {
      workspace.innerHTML = '<select class="workspace-select" id="workspace-select" aria-label="Workspace"><option selected>ws_orbit</option></select>';
    }
    const tasks = document.getElementById('tasks-body');
    if (tasks) {
      tasks.innerHTML = `
        <div class="row"><span class="id">ORB-11559</span><span class="title">Make Operations compact and usable on narrow dashboard widths</span><span class="status-cell"><select class="task-status-select"><option>in-progress</option></select></span><span class="crew-cell"><select class="task-crew-select"><option>grok</option></select></span></div>
        <div class="row"><span class="id">ORB-00001-with-a-very-long-identifier</span><span class="title">A deliberately long title that must wrap instead of forcing horizontal page scroll</span><span class="status-cell"><select class="task-status-select"><option>blocked</option></select></span><span class="crew-cell"><select class="task-crew-select"><option>claude</option></select></span></div>`;
    }
  });
  await page.evaluate(async () => {
    const { initRouter, initTabs } = await import('/router.js');
    let tab = 'operations';
    let diag = 'runs';
    let operations = 'routines';
    let knowledge = 'frictions';
    initRouter({
      getTab: () => tab, setTab: (value) => { tab = value; },
      getDiagSubtab: () => diag, setDiagSubtab: (value) => { diag = value; },
      getOperationsSubtab: () => operations, setOperationsSubtab: (value) => { operations = value; },
      getKnowledgeSubtab: () => knowledge, setKnowledgeSubtab: (value) => { knowledge = value; },
      getRunId: () => null, setRunId: () => {}, getRunSubtab: () => 'steps', setRunSubtab: () => {},
      getRunDetail: () => null, setRunDetail: () => {}, getRunEvents: () => [], setRunEvents: () => {},
      getRunLogs: () => [], setRunLogs: () => {}, getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
      getLastRuns: () => [], refreshDashboard: () => {}, renderDiagnostics: () => {},
      fitLogPanelToViewport: () => {},
      getActiveAuditSubtab: () => 'events', setAuditSubtab: () => {}, applyAuditHashQuery: () => {},
      syncAuditControls: () => {}, buildAuditHash: () => '#audit/events',
      setActiveAuditSubtabFromButton: () => {},
    });
    initTabs();
  });
  const viewports = [
    { name: '1440', width: 1440, height: 1000 },
    { name: '672', width: 672, height: 900 },
    { name: '390', width: 390, height: 844 },
    { name: '375x812', width: 375, height: 812 },
  ];
  for (const viewport of viewports) {
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    await page.evaluate(() => {
      document.querySelectorAll('.operation-details').forEach((node) => { node.open = false; });
    });
    for (const tab of ['routines', 'auto-tasks', 'auto-drain']) {
      await page.click(`#operations-subtabs .subtab[data-subtab="${tab}"]`);
      await page.waitForTimeout(200);
      const reachable = await page.evaluate((name) => {
        const button = document.querySelector(`#operations-subtabs .subtab[data-subtab="${name}"]`);
        const panel = document.getElementById(`operations-${name}-main`);
        const workspace = document.getElementById('workspace-select');
        const bounds = (node) => node.getBoundingClientRect();
        const onscreen = (node) => {
          const box = bounds(node);
          return box.width > 0 && box.height > 0 && box.right > 0 && box.left < window.innerWidth;
        };
        const clipped = Array.from(panel.querySelectorAll('button, select, .operation-row-head, .operation-clock-summary, .auto-drain-task-head, .auto-drain-evidence-row')).some((node) => {
          const box = bounds(node);
          return box.right > window.innerWidth + 1;
        });
        const readinessRows = name === 'auto-drain' ? panel.querySelectorAll('.auto-drain-task').length : null;
        return {
          subtab: onscreen(button),
          panel: panel && !panel.hidden,
          workspace: workspace && onscreen(workspace),
          clipped,
          readinessRows,
        };
      }, tab);
      if (!reachable.subtab) throw new Error(`${tab} subtab not reachable at ${viewport.name}`);
      if (!reachable.panel) throw new Error(`${tab} panel hidden at ${viewport.name}`);
      if (!reachable.workspace) throw new Error(`workspace selector not reachable at ${viewport.name} / ${tab}`);
      if (reachable.clipped) throw new Error(`Clipped Operations control at ${viewport.name} / ${tab}`);
      if (tab === 'auto-drain' && reachable.readinessRows !== 9) throw new Error(`Auto-drain diagnostics missing at ${viewport.name}: ${reachable.readinessRows}`);
      await assertNoOverflow(`${viewport.name} / ${tab}`);
      await page.screenshot({ path: path.join(evidence, `${tab}-${viewport.name}.png`), fullPage: true });
    }
  }

  await page.setViewportSize({ width: 375, height: 812 });
  await page.click('.tab[data-tab="tasks"]');
  await page.waitForTimeout(200);
  const taskLayout = await page.evaluate(() => {
    const row = document.querySelector('#tasks-body .row');
    const list = document.getElementById('tasks-panel');
    const style = row ? getComputedStyle(row) : null;
    return {
      listWidth: list?.getBoundingClientRect().width || 0,
      inner: window.innerWidth,
      areas: style?.gridTemplateAreas || '',
      columns: style?.gridTemplateColumns || '',
    };
  });
  // 216px rail would leave ~159px. Main padding is 20px on each side, so a
  // full-width list in a 375px viewport is about 335px.
  if (taskLayout.listWidth < taskLayout.inner - 80) {
    throw new Error(`task list is not full-width at 375px: ${taskLayout.listWidth} vs ${taskLayout.inner}`);
  }
  if (!taskLayout.areas.includes('id title') || !taskLayout.areas.includes('status crew')) {
    throw new Error(`375px task row must use the 520px two-row areas, got ${JSON.stringify(taskLayout)}`);
  }
  await assertNoOverflow('375x812 / tasks');
  await page.screenshot({ path: path.join(evidence, 'tasks-375x812.png'), fullPage: true });

  const navReachable = await page.evaluate(() => {
    const unique = [...document.querySelectorAll('.rail .tab')];
    const subtabs = [...document.querySelectorAll('#diag-subtabs .subtab')];
    const onscreen = (node) => {
      node.scrollIntoView({ inline: 'nearest', block: 'nearest' });
      const box = node.getBoundingClientRect();
      return box.width > 0 && box.height > 0 && box.bottom > 0 && box.top < window.innerHeight && box.right > 0 && box.left < window.innerWidth + 8;
    };
    return {
      tabs: unique.map((node) => ({ tab: node.dataset.tab, onscreen: onscreen(node) })),
      subtabs: subtabs.map((node) => ({ subtab: node.dataset.subtab, onscreen: onscreen(node) })),
    };
  });
  for (const tab of navReachable.tabs) {
    if (!tab.onscreen) throw new Error(`top-level tab ${tab.tab} not reachable at 375px`);
  }
  for (const subtab of navReachable.subtabs) {
    if (!subtab.onscreen) throw new Error(`diagnostics subtab ${subtab.subtab} not reachable at 375px`);
  }

  await page.click('.tab[data-tab="operations"]');
  await page.click('#operations-subtabs .subtab[data-subtab="auto-tasks"]');
  await page.waitForTimeout(150);
  await page.locator('.auto-task-card .operation-details summary').first().focus();
  await page.keyboard.press('Enter');
  const expanded = await page.evaluate(() => document.querySelector('.auto-task-card .operation-details')?.open);
  if (!expanded) throw new Error('keyboard Enter did not expand auto-task details');
  await page.screenshot({ path: path.join(evidence, 'auto-tasks-375x812-expanded.png'), fullPage: true });

  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.addScriptTag({ type: 'module', url: '/test.mjs' });
  await page.waitForFunction(() => globalThis.operationsTestsPassed, undefined, { timeout: 15000 });
  await page.evaluate(async () => {
    const { initRouter, initTabs } = await import('/router.js');
    let tab = 'tasks';
    let diag = 'runs';
    let operations = 'routines';
    let knowledge = 'frictions';
    initRouter({
      getTab: () => tab, setTab: (value) => { tab = value; },
      getDiagSubtab: () => diag, setDiagSubtab: (value) => { diag = value; },
      getOperationsSubtab: () => operations, setOperationsSubtab: (value) => { operations = value; },
      getKnowledgeSubtab: () => knowledge, setKnowledgeSubtab: (value) => { knowledge = value; },
      getRunId: () => null, setRunId: () => {}, getRunSubtab: () => 'steps', setRunSubtab: () => {},
      getRunDetail: () => null, setRunDetail: () => {}, getRunEvents: () => [], setRunEvents: () => {},
      getRunLogs: () => [], setRunLogs: () => {}, getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
      getLastRuns: () => [], refreshDashboard: () => {}, renderDiagnostics: () => {},
      fitLogPanelToViewport: () => {},
      getActiveAuditSubtab: () => 'events', setAuditSubtab: () => {}, applyAuditHashQuery: () => {},
      syncAuditControls: () => {}, buildAuditHash: () => '#audit/events',
      setActiveAuditSubtabFromButton: () => {},
    });
    initTabs();
  });
  const afterReload = await page.evaluate(() => ({
    hash: location.hash,
    autoTasks: !document.getElementById('operations-auto-tasks-main')?.hidden,
  }));
  if (!afterReload.hash.includes('operations/auto-tasks') || !afterReload.autoTasks) {
    throw new Error(`reload did not restore auto-tasks subtab: ${JSON.stringify(afterReload)}`);
  }
  await page.click('#operations-subtabs .subtab[data-subtab="routines"]');
  await page.waitForFunction(() => location.hash.includes('operations/routines'));
  await page.click('#operations-subtabs .subtab[data-subtab="auto-tasks"]');
  await page.waitForFunction(() => location.hash.includes('operations/auto-tasks'));
  await page.goBack();
  await page.waitForFunction(() => location.hash.includes('operations/routines'));
  const afterBack = await page.evaluate(() => ({
    hash: location.hash,
    routines: !document.getElementById('operations-routines-main')?.hidden,
  }));
  if (!afterBack.hash.includes('operations/routines') || !afterBack.routines) {
    throw new Error(`back did not restore routines: ${JSON.stringify(afterBack)}`);
  }
  await page.goForward();
  await page.waitForFunction(() => location.hash.includes('operations/auto-tasks'));
  const afterForward = await page.evaluate(() => ({
    hash: location.hash,
    autoTasks: !document.getElementById('operations-auto-tasks-main')?.hidden,
  }));
  if (!afterForward.hash.includes('operations/auto-tasks') || !afterForward.autoTasks) {
    throw new Error(`forward did not restore auto-tasks: ${JSON.stringify(afterForward)}`);
  }
  console.log(`PASS: Chromium Operations fixture; 1440/672/390/375; subtabs, reload, history. Screenshots: ${evidence}`);
} finally {
  await browser?.close(); server.close();
}
