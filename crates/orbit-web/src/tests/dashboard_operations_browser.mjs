// Usage: node dashboard_operations_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const test = fileURLToPath(new URL('./dashboard_operations.mjs', import.meta.url));
const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = name === '/test.mjs' ? { data: fs.readFileSync(test), type: 'text/javascript' } : dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', served.type);
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
    const { initRouter, initTabs } = await import('/js/router.js');
    const { setDockMode } = await import('/js/log-tail.js');
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
      fitLogPanelToViewport: () => {}, showDrainDock: () => setDockMode('drain'),
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
    for (const tab of ['routines', 'auto-tasks', 'jobs']) {
      const selector = `#operations-subtabs .subtab[data-subtab="${tab}"]`;
      await page.click(selector);
      await page.waitForTimeout(200);
      const reachable = await page.evaluate(([name, selector]) => {
        const button = document.querySelector(selector);
        const panel = document.getElementById(`operations-${name}-main`);
        const workspace = document.getElementById('workspace-select');
        const bounds = (node) => node.getBoundingClientRect();
        const onscreen = (node) => {
          const box = bounds(node);
          return box.width > 0 && box.height > 0 && box.right > 0 && box.left < window.innerWidth;
        };
        const clipped = Array.from(panel.querySelectorAll('button, select, .operation-row-head, .operation-clock-summary')).some((node) => {
          const box = bounds(node);
          return box.right > window.innerWidth + 1;
        });
        return {
          subtab: onscreen(button),
          panel: panel && !panel.hidden,
          workspace: workspace && onscreen(workspace),
          clipped,
        };
      }, [tab, selector]);
      if (!reachable.subtab) throw new Error(`${tab} subtab not reachable at ${viewport.name}`);
      if (!reachable.panel) throw new Error(`${tab} panel hidden at ${viewport.name}`);
      if (!reachable.workspace) throw new Error(`workspace selector not reachable at ${viewport.name} / ${tab}`);
      if (reachable.clipped) throw new Error(`Clipped Operations control at ${viewport.name} / ${tab}`);
      await assertNoOverflow(`${viewport.name} / ${tab}`);
      await page.screenshot({ path: path.join(evidence, `${tab}-${viewport.name}.png`), fullPage: true });
    }
  }

  // ORB-12898: the retired #auto-drain destination opens Tasks with the Drain
  // dock, and the card fits the dock at its 336px minimum (and the narrower
  // mid-width column) with no horizontal scroll.
  await page.evaluate(async () => {
    const { setDockMode } = await import('/js/log-tail.js');
    const { setActiveTab } = await import('/js/router.js');
    setDockMode('log');
    setActiveTab('auto-drain');
  });
  await page.waitForFunction(() => location.hash.startsWith('#tasks'));
  const drainCheck = async (label, dockWidth) => {
    const result = await page.evaluate((width) => {
      const layout = document.querySelector('main.tasks-layout');
      if (width) layout.style.setProperty('--dock-w', `${width}px`); else layout.style.removeProperty('--dock-w');
      const dock = document.getElementById('side-dock');
      const card = document.getElementById('auto-drain-panel');
      const cardBox = card.getBoundingClientRect();
      const overflowing = Array.from(card.querySelectorAll('*'))
        .filter((node) => node.getClientRects().length > 0 && node.getBoundingClientRect().right > cardBox.right + 1)
        .map((node) => node.className || node.tagName);
      return {
        mode: dock.dataset.mode,
        dockWidth: Math.round(dock.getBoundingClientRect().width),
        cardVisible: cardBox.height > 0,
        firstPanel: dock.querySelector('.dock-pane[data-pane="drain"] > .panel')?.id,
        scroll: card.scrollWidth > card.clientWidth + 1,
        overflowing,
        durations: card.querySelectorAll('.drain-duration[aria-pressed]').length,
        text: card.textContent,
      };
    }, dockWidth);
    if (result.mode !== 'drain' || !result.cardVisible) throw new Error(`#auto-drain did not open the Drain dock at ${label}: ${JSON.stringify(result)}`);
    if (result.firstPanel !== 'auto-drain-panel') throw new Error(`auto-drain card is not the first dock card at ${label}: ${result.firstPanel}`);
    if (result.scroll || result.overflowing.length) throw new Error(`Drain card overflows at ${label} (dock ${result.dockWidth}px): ${result.overflowing}`);
    if (result.durations !== 6 || !result.text.includes('Blocked by running')) throw new Error(`Drain card incomplete at ${label}: ${result.text}`);
    await page.screenshot({ path: path.join(evidence, `drain-${label}.png`), fullPage: true });
    return result.dockWidth;
  };
  await page.setViewportSize({ width: 1440, height: 1000 });
  const minimum = await drainCheck('1440-dock336', 336);
  if (minimum !== 336) throw new Error(`dock did not sit at its 336px minimum: ${minimum}`);
  await page.setViewportSize({ width: 900, height: 900 });
  await drainCheck('900', null);
  await page.setViewportSize({ width: 375, height: 812 });
  await drainCheck('375x812', null);
  await page.evaluate(() => document.querySelector('main.tasks-layout').style.removeProperty('--dock-w'));

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
    const { initRouter, initTabs } = await import('/js/router.js');
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
  console.log(`PASS: Chromium Operations fixture; 1440/672/390/375; subtabs, Drain dock card at 336/900/375, reload, history. Screenshots: ${evidence}`);
} finally {
  await browser?.close(); server.close();
}
