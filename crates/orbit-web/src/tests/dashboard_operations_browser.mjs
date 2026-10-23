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
const cronZoneTest = fileURLToPath(new URL('./dashboard_operations_cron_zone.mjs', import.meta.url));
const routineTriggersTest = fileURLToPath(new URL('./dashboard_operations_routine_triggers.mjs', import.meta.url));
const feedbackHarness = `
globalThis.review = {
  mint: async (failsReadback = false) => {
    readbackError = failsReadback;
    button('auto-tasks-body', 'Mint now').click();
    for (let i = 0; i < 60; i++) await Promise.resolve();
  },
  refresh: async () => {
    readbackError = false;
    await fetchAndRenderOperations();
  },
  switchWorkspace: async (id) => {
    readbackError = false;
    setWorkspace(id);
    await fetchAndRenderOperations();
  },
  failMint: async () => {
    responseError = 'mutation refused';
    button('auto-tasks-body', 'Mint now').click();
    for (let i = 0; i < 60; i++) await Promise.resolve();
    responseError = null;
  },
};`;
// The Start-card switches for the Tasks dock check: each window POST succeeds,
// is refused with HTTP 500, or loses its response, and can be held in flight.
const dockHarness = `
let legacyCheckout = false;
const dockFetch = globalThis.fetch;
globalThis.fetch = async (...args) => {
  const result = await dockFetch(...args);
  if (legacyCheckout && String(args[0]).includes('/api/workflows/auto/readiness')) {
    const payload = await result.json();
    delete payload.start_options;
    return response(payload);
  }
  return result;
};
globalThis.dockReview = {
  checkout: async (replica, legacy = false) => {
    replicaWorkspace = replica;
    legacyCheckout = legacy;
    await fetchAndRenderOperations();
  },
  localRefusal: () => LOCAL_REFUSAL,
  next: (kind) => {
    responseError = kind === 'refusal' ? 'fixture submission refused' : null;
    loseAutoResponse = kind === 'lost';
  },
  posts: () => requests.filter(r => r.method === 'POST' && r.path === '/api/workflows/auto').length,
  lastBody: () => requests.filter(r => r.method === 'POST' && r.path === '/api/workflows/auto').at(-1)?.body,
  hold: () => { delayPost = true; releasePost = null; },
  held: () => releasePost !== null,
  release: () => { delayPost = false; const resolve = releasePost; releasePost = null; resolve?.(); },
};`;
const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = name === '/test.mjs' ? { data: fs.readFileSync(test), type: 'text/javascript' }
    : name === '/cron-zone.mjs' ? { data: fs.readFileSync(cronZoneTest), type: 'text/javascript' }
    : name === '/routine-triggers.mjs' ? { data: fs.readFileSync(routineTriggersTest), type: 'text/javascript' }
    : name === '/feedback-timer.mjs' ? { data: fs.readFileSync(test) + '\n' + feedbackHarness, type: 'text/javascript' }
    : name === '/dock-feedback.mjs' ? { data: fs.readFileSync(test) + '\n' + dockHarness, type: 'text/javascript' }
    : dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const pageOverflow = () => document.scrollingElement.scrollWidth > window.innerWidth;
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
  browser = await chromium.launch({headless:true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined});
  // A fixed zone away from UTC, so local times cannot pass as UTC.
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, timezoneId: 'America/Los_Angeles' });
  const sharedDrainDeadline = new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString();
  await page.addInitScript({ content: `window.__drainDeadline = ${JSON.stringify(sharedDrainDeadline)};` });
  // Serve the actual markup/styles with only the Operations module initialized.
  // All API traffic is fixture data; no live scheduler or dashboard is contacted.
  const pageErrors = [];
  page.on('pageerror', error => { pageErrors.push(String(error)); console.error(error); });
  await page.goto(`http://127.0.0.1:${server.address().port}/#operations/routines`);
  await page.addScriptTag({ type: 'module', url: '/dock-feedback.mjs' });
  await page.waitForFunction(() => globalThis.operationsTestsPassed, undefined, { timeout: 15000 });
  await page.evaluate(() => {
    const workspace = document.getElementById('rail-workspace');
    if (workspace && !document.getElementById('workspace-select')) {
      workspace.innerHTML = '<select class="workspace-select" id="workspace-select" aria-label="Workspace"><option selected>ws_orbit</option></select>';
      const note = document.createElement('span');
      note.id = 'workspace-scope-note';
      note.className = 'workspace-scope-note';
      note.textContent = 'Fleet-wide on Reliability';
      note.hidden = true;
      workspace.appendChild(note);
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
    const { setDockMode, placeDock } = await import('/js/log-tail.js');
    let tab = 'operations';
    let diag = 'incidents';
    let jobs = 'live';
    let operations = 'routines';
    let knowledge = 'frictions';
    let incidentsView = 'incidents';
    let analytics = 'metrics';
    let reliabilityView = 'pipeline';
    let agentsRole = 'orchestrators';
    let config = 'effective';
    let runId = null;
    let runSubtab = 'steps';
    initRouter({
      getTab: () => tab, setTab: (value) => { tab = value; },
      getJobsSubtab: () => jobs, setJobsSubtab: (value) => { jobs = value; },
      getDiagSubtab: () => diag, setDiagSubtab: (value) => { diag = value; },
      getOperationsSubtab: () => operations, setOperationsSubtab: (value) => { operations = value; },
      getKnowledgeSubtab: () => knowledge, setKnowledgeSubtab: (value) => { knowledge = value; },
      getConfigSubtab: () => config, setConfigSubtab: (value) => { config = value; },
      getRunId: () => runId, setRunId: (value) => { runId = value; },
      getRunSubtab: () => runSubtab, setRunSubtab: (value) => { runSubtab = value; },
      getRunDetail: () => null, setRunDetail: () => {}, getRunEvents: () => [], setRunEvents: () => {},
      getRunLogs: () => [], setRunLogs: () => {}, getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
      getLastRuns: () => [], refreshDashboard: () => {}, renderDiagnostics: () => {},
      fitLogPanelToViewport: () => {}, placeDock, setDrainView: (mode) => setDockMode(mode),
      getIncidentsView: () => incidentsView, setIncidentsView: (value) => { incidentsView = value; },
      getAnalyticsSubtab: () => analytics, setAnalyticsSubtab: (value) => { analytics = value; },
      getReliabilityView: () => reliabilityView, setReliabilityView: (value) => { reliabilityView = value; },
      getAgentsRole: () => agentsRole, setAgentsRole: (value) => { agentsRole = value; },
      applyAuditHashQuery: () => {}, syncAuditControls: () => {},
      buildAuditHash: () => '#diagnostics/incidents/events?status=non_success',
      applyCodebaseHashQuery: () => {}, buildCodebaseHash: () => '#analytics/codebase',
    });
    initTabs();
  });
  const viewports = [
    { name: '1440', width: 1440, height: 1000 },
    { name: '672', width: 672, height: 900 },
    { name: '390', width: 390, height: 844 },
    { name: '375x812', width: 375, height: 812 },
  ];

  // ORB-14442: task dock breakpoints must not override Health's router-owned
  // list grid. These checks exercise the shipped CSS and router at the widths
  // where the shared tasks-layout rules used to leave an empty column.
  const diagnosticLayout = async (view, width) => {
    await page.setViewportSize({ width, height: 730 });
    await page.click('.tab[data-tab="diagnostics"]');
    await page.click('#diag-subtabs .subtab[data-subtab="incidents"]');
    await page.click(`#incidents-views .subtab[data-view="${view}"]`);
    const layout = await page.evaluate(() => {
      const main = document.getElementById('diagnostics-main');
      const panel = document.getElementById('diagnostics-panel');
      const style = getComputedStyle(main);
      const mainBox = main.getBoundingClientRect();
      const panelBox = panel.getBoundingClientRect();
      const paddingLeft = parseFloat(style.paddingLeft);
      const contentWidth = mainBox.width - paddingLeft - parseFloat(style.paddingRight);
      return {
        tracks: getComputedStyle(main).gridTemplateColumns.trim().split(/\s+/).length,
        contentWidth,
        panelWidth: panelBox.width,
        panelLeft: panelBox.left,
        contentLeft: mainBox.left + paddingLeft,
      };
    });
    if (layout.tracks !== 1 || Math.abs(layout.panelWidth - layout.contentWidth) > 1
      || Math.abs(layout.panelLeft - layout.contentLeft) > 1) {
      throw new Error(`Health ${view} should fill one column at ${width}px: ${JSON.stringify(layout)}`);
    }
  };
  // Events, Metrics and Tools each bring one full-width <main>.
  const fullWidthView = async (label, clicks, mainId, panelId) => {
    for (const selector of clicks) await page.click(selector);
    return page.evaluate(([mainId, panelId]) => {
      const main = document.getElementById(mainId);
      const style = getComputedStyle(main);
      const box = main.getBoundingClientRect();
      return {
        shown: main.checkVisibility(),
        tracks: style.gridTemplateColumns.trim().split(/\s+/).length,
        contentWidth: box.width - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight),
        panelWidth: document.getElementById(panelId).getBoundingClientRect().width,
      };
    }, [mainId, panelId]).then(layout => {
      if (!layout.shown || layout.tracks !== 1 || Math.abs(layout.panelWidth - layout.contentWidth) > 1) {
        throw new Error(`${label} should fill one full-width column: ${JSON.stringify(layout)}`);
      }
    });
  };
  for (const width of [1073, 1250]) {
    for (const view of ['incidents', 'errors']) await diagnosticLayout(view, width);
    await fullWidthView(`Health › Events at ${width}px`, ['#incidents-views .subtab[data-view="events"]'], 'diagnostics-events-main', 'audit-pane');
    // ORB-15746: Metrics is its own full-width view under Analytics.
    await fullWidthView(`Analytics › Metrics at ${width}px`, ['.tab[data-tab="analytics"]', '#analytics-subtabs .subtab[data-subtab="metrics"]'], 'analytics-metrics-main', 'metrics-panel');
    await fullWidthView(`Analytics › Reliability › Tools at ${width}px`, ['#analytics-subtabs .subtab[data-subtab="reliability"]', '#reliability-views .subtab[data-view="tools"]'], 'analytics-tools-main', 'audit-summary-panel');
  }

  // ORB-16962: a Start from the Tasks dock keeps its result beside the Start
  // card, on Tasks and again on Drain. The router places the shipped dock, as
  // app.js does, and the harness holds, refuses or loses the window's POST.
  // ORB-17503: the Tasks dock keeps the simple Start card in its first view and
  // offers no Start a window view; a browser that last chose that view on
  // Drain opens the first view. The composer stays on #drain/start.
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.evaluate(async () => (await import('/js/log-tail.js')).setDockMode('start'));
  await page.click('.tab[data-tab="tasks"]');
  const dockStart = page.locator('#tasks-dock #auto-drain-quick-start button.drain-start');
  const dockStatus = page.locator('#tasks-dock #auto-drain-operation-feedback');
  await dockStart.waitFor({ state: 'visible' });
  const tasksDock = await page.evaluate(() => ({
    mode: document.getElementById('side-dock').dataset.mode,
    tabs: [...document.querySelectorAll('#dock-mode-toggle .dock-seg')].filter(tab => tab.getClientRects().length > 0).map(tab => tab.dataset.mode).join(' '),
    composer: document.getElementById('dock-pane-start').getClientRects().length + document.getElementById('auto-drain-start-body').childElementCount,
    column: document.getElementById('auto-drain-quick-start').closest('.dock-pane')?.id,
    durations: document.querySelectorAll('#auto-drain-quick-start .drain-durations .drain-duration[aria-pressed]').length,
    fields: ['auto-drain-concurrency', 'auto-drain-mode', 'auto-drain-complete', 'auto-drain-approve', 'auto-drain-auto-fix', 'auto-drain-review']
      .filter(id => !document.getElementById('auto-drain-quick-start-body').contains(document.getElementById(id))),
    feedback: document.getElementById('auto-drain-quick-start').nextElementSibling?.id,
  }));
  if (tasksDock.mode !== 'drain' || tasksDock.tabs !== 'drain log' || tasksDock.composer !== 0 || tasksDock.column !== 'dock-pane-drain'
    || tasksDock.durations !== 6 || tasksDock.fields.length || tasksDock.feedback !== 'auto-drain-operation-feedback') {
    throw new Error(`The Tasks dock must keep the simple Start card and no Start a window view: ${JSON.stringify(tasksDock)}`);
  }
  // A replica can only start a pull drain. Tasks keeps its local Start
  // visible but refused, with a working route to the pull composer.
  await page.evaluate(() => globalThis.dockReview.checkout(true));
  const replicaPosts = await page.evaluate(() => globalThis.dockReview.posts());
  await dockStart.scrollIntoViewIfNeeded();
  await page.screenshot({ path: path.join(evidence, 'tasks-replica-start-dark-1440.png') });
  if (!await dockStart.isDisabled()) throw new Error('Replica Tasks must disable its local-only Start');
  const refusal = page.locator('#auto-drain-quick-start .drain-field-hint.warn').filter({ hasText: await page.evaluate(() => globalThis.dockReview.localRefusal()) });
  if (!await refusal.isVisible()) throw new Error('Replica Tasks must display the runtime checkout refusal');
  // Native disabled clicks and a dispatched event must both avoid submission.
  await dockStart.evaluate(button => { button.click(); button.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
  if (await page.evaluate(() => globalThis.dockReview.posts()) !== replicaPosts) throw new Error('Refused replica Tasks Start sent a POST');
  await page.evaluate(() => globalThis.dockReview.checkout(true, true));
  if (!await dockStart.isDisabled() || !await page.locator('#auto-drain-quick-start .drain-start-foot .drain-field-hint.warn').isVisible()) {
    throw new Error('Older replica readiness must keep Start disabled with a visible checkout explanation');
  }
  await dockStart.evaluate(button => button.dispatchEvent(new MouseEvent('click', { bubbles: true })));
  if (await page.evaluate(() => globalThis.dockReview.posts()) !== replicaPosts) throw new Error('Legacy replica Tasks Start sent a POST');
  await page.evaluate(() => globalThis.dockReview.checkout(true));
  const pullRoute = page.locator('#auto-drain-quick-start a[href="#drain/start"]');
  await pullRoute.click();
  await page.waitForFunction(() => location.hash === '#drain/start');
  const pullStart = page.locator('#auto-drain-start-body button.drain-start');
  await pullStart.waitFor({ state: 'visible' });
  if (await pullStart.isDisabled() || !(await pullStart.textContent()).includes('pulling')) throw new Error('The Tasks remedy must open a supported pull start');
  await pullStart.click();
  await page.waitForFunction(before => globalThis.dockReview.posts() === before + 1, replicaPosts);
  const pullBody = await page.evaluate(() => globalThis.dockReview.lastBody());
  if (pullBody.pull !== 'ws-one' || pullBody.host !== 'server' || ['complete', 'approve_proposed', 'auto_fix'].some(key => Object.hasOwn(pullBody, key))) {
    throw new Error(`The Drain remedy must preserve the supported pull request: ${JSON.stringify(pullBody)}`);
  }
  await page.click('.tab[data-tab="tasks"]');
  await page.setViewportSize({ width: 375, height: 812 });
  if (await dockStart.isVisible()) throw new Error('The Tasks dock must give way to the list at phone width');
  await assertNoOverflow('replica Tasks at 375px');
  await page.waitForTimeout(350);
  await page.screenshot({ path: path.join(evidence, 'tasks-replica-start-dark-375.png'), animations: 'disabled' });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.evaluate(() => globalThis.dockReview.checkout(false));
  if (await dockStart.isDisabled()) throw new Error('Owner Tasks Start must remain enabled after a replica');
  await dockStart.scrollIntoViewIfNeeded();
  await page.screenshot({ path: path.join(evidence, 'tasks-owner-start-dark-1440.png') });
  await page.setViewportSize({ width: 375, height: 812 });
  await page.waitForTimeout(350);
  await page.screenshot({ path: path.join(evidence, 'tasks-owner-start-dark-375.png'), animations: 'disabled' });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.evaluate(() => globalThis.dockReview.next('success'));
  const postsBeforeSuccess = await page.evaluate(() => globalThis.dockReview.posts());
  await page.evaluate(() => globalThis.dockReview.hold());
  await dockStart.click();
  await page.waitForFunction(() => globalThis.dockReview.held());
  await page.locator('#tasks-dock #auto-drain-operation-feedback.pending').waitFor({ state: 'visible' });
  await page.evaluate(() => globalThis.dockReview.release());
  await page.locator('#tasks-dock #auto-drain-operation-feedback.success').waitFor({ state: 'visible' });
  const receipt = await dockStatus.textContent();
  if (!receipt.includes('submitted') || !receipt.includes('Warning: Admissions throttled')) {
    throw new Error(`Tasks dock receipt lost its confirmation or warnings: ${receipt}`);
  }
  if (await page.evaluate(() => globalThis.dockReview.posts()) !== postsBeforeSuccess + 1) {
    throw new Error('A Start from Tasks must send exactly one POST');
  }
  // The simple card posts only the fields it offers.
  const dockBody = await page.evaluate(() => globalThis.dockReview.lastBody());
  if (Object.keys(dockBody).sort().join(' ') !== 'approve_proposed auto_fix complete for_duration' || dockBody.for_duration !== '1h') {
    throw new Error(`The Tasks dock Start card must post its own fields: ${JSON.stringify(dockBody)}`);
  }
  if (await page.locator('#side-dock').getAttribute('data-mode') !== 'drain' || await page.evaluate(() => location.hash.startsWith('#drain'))) {
    throw new Error('A Start from Tasks stays on the Tasks dock\'s first view');
  }
  await page.screenshot({ path: path.join(evidence, 'tasks-dock-feedback-success-dark-1440.png') });
  // An HTTP refusal and a lost response each name their remedy beside Start,
  // send one POST and keep the operator on Tasks.
  for (const [kind, expected] of [
    ['refusal', ['Auto-delivery window failed to start', 'fixture submission refused']],
    ['lost', ['Submission outcome unknown', 'Check the window and run history before starting again', 'this request was not retried']],
  ]) {
    await page.evaluate(kind => globalThis.dockReview.next(kind), kind);
    const before = await page.evaluate(() => globalThis.dockReview.posts());
    await dockStart.click();
    await page.locator('#tasks-dock #auto-drain-operation-feedback.error').waitFor({ state: 'visible' });
    const text = await dockStatus.textContent();
    if (!expected.every(part => text.includes(part))) throw new Error(`Tasks dock ${kind} feedback lacks its remedy: ${text}`);
    await page.waitForTimeout(300);
    if (await page.evaluate(() => globalThis.dockReview.posts()) !== before + 1) throw new Error(`A ${kind} from Tasks must not repeat the POST`);
    if (await page.evaluate(() => location.hash.startsWith('#drain'))) throw new Error(`A ${kind} from Tasks navigated to Drain`);
    await page.screenshot({ path: path.join(evidence, `tasks-dock-feedback-${kind}-dark-1440.png`) });
  }
  // Drain shows the same status line above its views, so the unknown
  // outcome and its remedy stay visible after the dock moves there.
  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('auto-drain'));
  await page.waitForFunction(() => location.hash.startsWith('#drain'));
  const drainStatus = page.locator('#drain-main #auto-drain-operation-feedback.error');
  await drainStatus.waitFor({ state: 'visible' });
  if (await page.locator('#drain-main #auto-drain-quick-start').isVisible() || !(await page.locator('#drain-main #dock-tab-start').isVisible())) {
    throw new Error('Drain offers Start a window as its own view, not the Tasks dock card');
  }
  const drainText = await drainStatus.textContent();
  if (!drainText.includes('Submission outcome unknown') || !drainText.includes('not retried')) {
    throw new Error(`Drain lost the unknown-outcome remedy: ${drainText}`);
  }
  await drainStatus.scrollIntoViewIfNeeded();
  await page.waitForTimeout(350);
  await page.screenshot({ path: path.join(evidence, 'drain-feedback-lost-dark-1440.png'), animations: 'disabled' });
  await page.setViewportSize({ width: 375, height: 812 });
  await drainStatus.scrollIntoViewIfNeeded();
  await page.waitForTimeout(350);
  await page.screenshot({ path: path.join(evidence, 'drain-feedback-lost-dark-375.png'), animations: 'disabled' });
  await page.evaluate(() => globalThis.dockReview.next('success'));

  await page.click('.tab[data-tab="tasks"]');
  for (const width of [1251, 1250, 1073, 901, 900]) {
    await page.setViewportSize({ width, height: 730 });
    const tasks = await page.evaluate(() => {
      const main = document.querySelector('.tab-pane[data-tab="tasks"] > main.tasks-layout');
      const list = document.getElementById('tasks-panel').getBoundingClientRect();
      const style = getComputedStyle(main);
      return {
        tracks: style.gridTemplateColumns.trim().split(/\s+/).length,
        listWidth: list.width,
        dockWidth: document.getElementById('tasks-dock').getBoundingClientRect().width,
        gap: parseFloat(style.columnGap) || 0,
        mainWidth: main.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight),
        drainStatusShown: document.getElementById('auto-drain-panel').getClientRects().length > 0,
      };
    });
    // The Tasks dock sits beside the list above 1000px, leading with the
    // Auto-drain status card, and gives way below it.
    const docked = width > 1000;
    const dockSpace = docked ? tasks.gap + tasks.dockWidth : 0;
    if (tasks.tracks !== (docked ? 2 : 1) || (tasks.dockWidth > 0) !== docked || tasks.drainStatusShown !== docked || Math.abs(tasks.listWidth + dockSpace - tasks.mainWidth) > 1) {
      throw new Error(`Tasks should fill its page beside the dock and its Auto-drain card above 1000px and alone below it, at ${width}px: ${JSON.stringify(tasks)}`);
    }
  }
  await page.click('.tab[data-tab="operations"]');

  for (const viewport of viewports) {
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    await page.evaluate(() => {
      document.querySelectorAll('.operation-details').forEach((node) => { node.open = false; });
    });
    for (const tab of ['routines', 'auto-tasks', 'live', 'history', 'catalogue']) {
      const group = ['routines', 'auto-tasks'].includes(tab) ? 'operations' : 'jobs';
      await page.click(`.tab[data-tab="${group}"]`);
      const selector = `#${group}-subtabs .subtab[data-subtab="${tab}"]`;
      await page.click(selector);
      await page.waitForTimeout(200);
      const reachable = await page.evaluate(([name, selector, group]) => {
        const button = document.querySelector(selector);
        const panel = document.getElementById(`${group}-${name}-main`);
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
      }, [tab, selector, group]);
      if (!reachable.subtab) throw new Error(`${tab} subtab not reachable at ${viewport.name}`);
      if (!reachable.panel) throw new Error(`${tab} panel hidden at ${viewport.name}`);
      if (!reachable.workspace) throw new Error(`workspace selector not reachable at ${viewport.name} / ${tab}`);
      if (reachable.clipped) throw new Error(`Clipped Operations control at ${viewport.name} / ${tab}`);
      await assertNoOverflow(`${viewport.name} / ${tab}`);
      await page.screenshot({ path: path.join(evidence, `${tab}-${viewport.name}.png`), fullPage: true });
    }
  }

  const menus = await page.evaluate(() => ({
    work: [...document.querySelector('.rail-group').querySelectorAll('.tab')].map(button => button.dataset.tab),
    automation: [...document.querySelectorAll('#operations-subtabs .subtab')].map(button => button.dataset.subtab),
  }));
  if (menus.work.join() !== 'tasks,jobs,drain' || menus.automation.join() !== 'routines,auto-tasks') throw new Error(`Navigation menus: ${JSON.stringify(menus)}`);
  const jobsNavigation = [];
  for (const subtab of ['live', 'history', 'catalogue']) {
    await page.click(`.tab[data-tab="jobs"]`);
    await page.click(`#jobs-subtabs [data-subtab="${subtab}"]`);
    const state = await page.evaluate(() => ({
      hash: location.hash,
      mains: [...document.querySelectorAll('.tab-pane[data-tab="jobs"] > main')].filter(main => getComputedStyle(main).display !== 'none').map(main => main.id),
      filters: [...document.querySelectorAll('#runs-body .runs-filter button')].map(button => button.textContent),
      selected: document.querySelector('.rail .tab.active')?.dataset.tab,
    }));
    if (state.hash.split("?")[0] !== `#jobs/${subtab}` || state.selected !== 'jobs' || state.mains.join() !== `jobs-${subtab}-main`) throw new Error(`Jobs navigation: ${JSON.stringify(state)}`);
    if (subtab === 'history' && state.filters.join() !== 'All,Failed') throw new Error(`History filters: ${state.filters}`);
    jobsNavigation.push(state);
  }
  const routine = page.locator('#jobs-body a.operation-chip[href="#operations/routines"]').first();
  if (!await routine.isVisible()) throw new Error('Catalogue has no scheduling routine link');
  await routine.click();
  await page.waitForFunction(() => location.hash === '#operations/routines');
  for (const retired of ['runs', 'diagnostics/runs', 'operations/jobs']) {
    await page.evaluate(async route => (await import('/js/router.js')).setActiveTab(route), retired);
    if (await page.locator('.tab-pane[data-tab="jobs"]').evaluate(pane => pane.classList.contains('active'))) throw new Error(`Retired route opens Jobs: ${retired}`);
  }
  fs.writeFileSync(path.join(evidence, 'jobs-navigation.json'), JSON.stringify({ passed: true, menus, tabs: jobsNavigation, routineLink: '#operations/routines', retiredRoutes: ['runs', 'diagnostics/runs', 'operations/jobs'] }, null, 2));

  // ORB-15361: a task's running run with an unbreakable job id wraps inside its
  // Running now card at the grid's 280px minimum. The grouped card keeps its
  // task and run links, and an untasked card still fits its own layout.
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.click('.tab[data-tab="jobs"]');
  await page.click('#jobs-subtabs .subtab[data-subtab="live"]');
  await page.waitForTimeout(200);
  const longTaskJobId = 'task_' + 'a'.repeat(58);
  const runningLayout = await page.evaluate((longJobId) => {
    const grid = document.querySelector('#jobs-live-main .operation-running-grid');
    grid.style.gridTemplateColumns = '280px';
    const lineCount = (node) => {
      const range = document.createRange();
      range.selectNodeContents(node);
      return range.getClientRects().length;
    };
    const box = (node) => node.getBoundingClientRect();
    const grouped = grid.querySelector('.operation-running-task[data-task="ORB-15001"]');
    const untasked = Array.from(grid.querySelectorAll('.operation-running-card:not(.operation-running-task)'));
    const token = Array.from(grouped.querySelectorAll('.mono')).find(node => node.textContent === longJobId);
    const step = token.closest('.operation-running-step');
    return {
      grouped: {
        clientWidth: grouped.clientWidth,
        scrollWidth: grouped.scrollWidth,
        tokenLines: lineCount(token),
        tokenInsideCard: box(token).right <= box(grouped).right,
        runs: grouped.querySelectorAll('.operation-running-step').length,
        taskLinked: Boolean(grouped.querySelector('a[href*="open=ORB-15001"]')),
        runLinked: Array.from(step.querySelectorAll('a')).some(link => link.textContent === 'jrun-task-long'),
      },
      untasked: {
        cards: untasked.length,
        overflowing: untasked.filter(card => card.scrollWidth > card.clientWidth).length,
        runLinked: untasked.some(card => card.textContent.includes('jrun-fixture-running')),
      },
    };
  }, longTaskJobId);
  const { grouped, untasked } = runningLayout;
  if (grouped.scrollWidth > grouped.clientWidth || !grouped.tokenInsideCard) {
    throw new Error(`task-grouped job id overflows its Running now card at 280px: ${JSON.stringify(grouped)}`);
  }
  if (grouped.tokenLines < 2) throw new Error(`task-grouped job id does not wrap at 280px: ${JSON.stringify(grouped)}`);
  if (grouped.runs !== 3 || !grouped.taskLinked || !grouped.runLinked) {
    throw new Error(`task grouping or scoped links lost at 280px: ${JSON.stringify(grouped)}`);
  }
  if (untasked.overflowing || !untasked.runLinked) {
    throw new Error(`untasked Running now card changed at 280px: ${JSON.stringify(untasked)}`);
  }
  await page.locator('#jobs-live-main .operation-running').screenshot({ path: path.join(evidence, 'running-now-280.png') });

  // The task ID shares a flex row with its title. Across each generated grid
  // column count, the ID stays one line while the title consumes and clips to
  // the remaining width. Then repeat at a real 375px viewport.
  const assertRunningTaskHeader = async (label, expectedColumns = null) => {
    const layout = await page.evaluate(() => {
      const grid = document.querySelector('#jobs-live-main .operation-running-grid');
      const grouped = grid.querySelector('.operation-running-task[data-task="ORB-15001"]');
      const head = grouped.querySelector('.operation-running-head');
      const id = head.querySelector('.operation-task-link');
      const title = head.querySelector('.operation-running-task-title');
      const probe = id.cloneNode(true);
      probe.style.cssText = 'position:absolute;visibility:hidden;white-space:nowrap;flex-shrink:0';
      head.appendChild(probe);
      const oneLineHeight = probe.getBoundingClientRect().height;
      probe.remove();
      const range = document.createRange();
      range.selectNodeContents(id);
      const columns = getComputedStyle(grid).gridTemplateColumns.trim().split(/\s+/).length;
      return {
        columns,
        idHeight: id.getBoundingClientRect().height,
        oneLineHeight,
        idLines: range.getClientRects().length,
        titleWidth: title.clientWidth,
        titleScrollWidth: title.scrollWidth,
        titleWhiteSpace: getComputedStyle(title).whiteSpace,
        titleOverflow: getComputedStyle(title).textOverflow,
      };
    });
    if (expectedColumns != null && layout.columns !== expectedColumns) {
      throw new Error(`Running now grid has ${layout.columns} columns at ${label}, expected ${expectedColumns}: ${JSON.stringify(layout)}`);
    }
    if (layout.idLines !== 1 || Math.abs(layout.idHeight - layout.oneLineHeight) > 0.5) {
      throw new Error(`Running now task ID is not one rendered line at ${label}: ${JSON.stringify(layout)}`);
    }
    if (layout.titleWhiteSpace !== 'nowrap' || layout.titleOverflow !== 'ellipsis' || layout.titleScrollWidth <= layout.titleWidth) {
      throw new Error(`Running now title does not truncate with an ellipsis at ${label}: ${JSON.stringify(layout)}`);
    }
  };
  for (const { width, columns } of [{ width: 280, columns: 1 }, { width: 576, columns: 2 }, { width: 872, columns: 3 }]) {
    await page.evaluate((gridWidth) => {
      const grid = document.querySelector('#jobs-live-main .operation-running-grid');
      grid.style.width = `${gridWidth}px`;
      grid.style.gridTemplateColumns = '';
    }, width);
    await assertRunningTaskHeader(`${columns}-column layout`, columns);
  }
  await page.setViewportSize({ width: 375, height: 812 });
  await page.evaluate(() => {
    const grid = document.querySelector('#jobs-live-main .operation-running-grid');
    grid.style.width = '';
    grid.style.gridTemplateColumns = '';
  });
  await assertRunningTaskHeader('375px viewport', 1);
  await page.evaluate(() => { document.querySelector('#jobs-live-main .operation-running-grid').style.gridTemplateColumns = ''; });

  // Retired drain routes reach the full-page Drain controls without overflow.
  await page.evaluate(async () => {
    const { setDockMode } = await import('/js/log-tail.js');
    const { setActiveTab } = await import('/js/router.js');
    setDockMode('log');
    setActiveTab('auto-drain');
  });
  await page.waitForFunction(() => location.hash.startsWith('#drain'));
  const setDock = (mode) => page.evaluate(async (mode) => (await import('/js/log-tail.js')).setDockMode(mode), mode);
  const overflowOf = (box) => {
    const bounds = box.getBoundingClientRect();
    return Array.from(box.querySelectorAll('*'))
      .filter((node) => node.getClientRects().length > 0 && node.getBoundingClientRect().right > bounds.right + 1)
      .map((node) => node.className || node.tagName);
  };
  // Overview holds the live window and its context; Start a window is its own
  // view. Each fits its width with nothing spilling sideways.
  const drainCheck = async (label, dockWidth) => {
    const result = await page.evaluate(() => {
      const dock = document.getElementById('side-dock');
      const card = document.getElementById('auto-drain-panel');
      const cardBox = card.getBoundingClientRect();
      return {
        mode: dock.dataset.mode,
        dockWidth: Math.round(dock.getBoundingClientRect().width),
        cardVisible: cardBox.height > 0,
        panels: [...dock.querySelectorAll('.dock-pane[data-pane="drain"] .panel')].map(panel => panel.id),
        scroll: card.scrollWidth > card.clientWidth + 1,
        clipped: card.scrollHeight > card.clientHeight + 1,
        pageScroll: document.scrollingElement.scrollWidth > window.innerWidth,
        bar: Boolean(card.querySelector('.drain-capacity-bar[aria-label]')),
      };
    });
    result.overflowing = await page.locator('#auto-drain-panel').evaluate(overflowOf);
    if (result.mode !== 'drain' || !result.cardVisible) throw new Error(`#auto-drain did not open the Drain dock at ${label}: ${JSON.stringify(result)}`);
    // The main column (status, how it started, pull claims, waits) comes
    // before the side column (fleet, locks) in reading and tab order.
    // The Tasks dock's Start card stays empty and hidden on Drain.
    if (result.panels.join(' ') !== 'auto-drain-panel drain-started-panel drain-pull-panel drain-waits-panel drain-fleet-panel auto-drain-quick-start locks-panel') throw new Error(`Drain dock cards out of order at ${label}: ${result.panels}`);
    if (result.scroll || result.clipped || result.overflowing.length || result.pageScroll) throw new Error(`Drain card overflows at ${label} (dock ${result.dockWidth}px): ${JSON.stringify(result)}`);
    if (!result.bar) throw new Error(`Drain card incomplete at ${label}: ${JSON.stringify(result)}`);
    await page.waitForTimeout(350);
    await page.screenshot({ path: path.join(evidence, `drain-${label}.png`), fullPage: true });
    await setDock('start');
    const compose = await page.evaluate(() => {
      const start = document.getElementById('auto-drain-start');
      return {
        mode: document.getElementById('side-dock').dataset.mode,
        visible: start.getBoundingClientRect().height > 0,
        scroll: start.scrollWidth > start.clientWidth + 1,
        pageScroll: document.scrollingElement.scrollWidth > window.innerWidth,
        durations: start.querySelectorAll('.drain-durations .drain-duration[aria-pressed]').length,
        command: document.getElementById('auto-drain-command')?.textContent || '',
        // The footer holds its parts only: an absent part leaves no text.
        footText: [...start.querySelector('.drain-start-foot').childNodes]
          .filter(node => node.nodeType === Node.TEXT_NODE && node.textContent.trim())
          .map(node => node.textContent),
      };
    });
    compose.overflowing = await page.locator('#auto-drain-start').evaluate(overflowOf);
    if (compose.mode !== 'start' || !compose.visible || compose.scroll || compose.overflowing.length || compose.pageScroll) throw new Error(`Start a window does not fit at ${label}: ${JSON.stringify(compose)}`);
    if (compose.footText.length) throw new Error(`Start a window footer renders stray text at ${label}: ${JSON.stringify(compose.footText)}`);
    if (compose.durations !== 7 || !compose.command.startsWith('orbit run auto')) throw new Error(`Start a window incomplete at ${label}: ${JSON.stringify(compose)}`);
    await page.waitForTimeout(200);
    await page.screenshot({ path: path.join(evidence, `drain-start-${label}.png`) });
    await setDock('drain');
    return result.dockWidth;
  };
  await page.setViewportSize({ width: 1440, height: 1000 });
  const fullWidth = await drainCheck('1440', null);
  if (fullWidth < 1000) throw new Error(`Drain does not use the full page: ${fullWidth}`);
  for (const [phase, label] of [['draining', 'Draining'], ['winding_down', 'Winding down'], ['idle', 'idle']]) {
    await page.evaluate(state => globalThis.setDrainFixturePhase(state), phase);
    const rendered = await page.evaluate(() => ({
      card: document.getElementById('auto-drain-panel').dataset.drainState,
      header: document.getElementById('auto-drain-phase-label').textContent,
      live: `${document.getElementById('auto-drain-when').textContent} ${document.getElementById('auto-drain-run')?.textContent || ''}`,
      admitted: document.querySelector('#auto-drain-live [data-stat="admitted"] dd')?.textContent,
      running: document.querySelector('#auto-drain-live [data-stat="running"] dd')?.textContent,
      global: document.getElementById('global-drain-state').textContent,
      globalHidden: document.getElementById('global-drain-state').hidden,
      tab: document.getElementById('dock-drain-state').textContent,
      tabLabel: document.getElementById('dock-tab-drain').getAttribute('aria-label'),
      status: document.getElementById('auto-drain-operation-feedback').textContent,
    }));
    if (rendered.card !== phase || rendered.header.toLowerCase() !== label.toLowerCase()) throw new Error(`Drain ${phase} header: ${JSON.stringify(rendered)}`);
    if (phase === 'draining' && (!rendered.live.includes('left') || !rendered.live.includes('jrun-'))) throw new Error(`Missing server deadline or run link: ${rendered.live}`);
    if (phase !== 'idle' && (rendered.admitted !== '3' || rendered.running !== '1')) throw new Error(`Missing scoped window count: ${JSON.stringify(rendered)}`);
    if (rendered.globalHidden || !rendered.global.includes('1 of 2 workspaces draining') || (phase === 'idle' ? rendered.tab : !rendered.tabLabel.includes(label))) throw new Error(`Drain ${phase} indicators: ${JSON.stringify(rendered)}`);
    if (phase === 'draining' && rendered.tab) throw new Error(`ORB-14489: Drain tab repeats draining state: ${rendered.tab}`);
    if (!rendered.status.includes(label)) throw new Error(`Drain ${phase} status announcement: ${rendered.status}`);
    await drainCheck(`state-${phase}`, 336);
  }

  // Operation feedback timeout in real Chromium browser: start a window,
  // observe confirmation and warnings, trigger clear, verify confirmation
  // cleared while warning lines stay.
  await page.evaluate(() => {
    const start = document.querySelector('#auto-drain-start-body .drain-start');
    if (start) start.click();
  });
  await page.waitForFunction(() => {
    const node = document.getElementById('auto-drain-operation-feedback');
    return node && node.textContent.includes('submitted') && node.textContent.includes('Warning:');
  });
  const beforeFeedbackTimeout = await page.evaluate(() => document.getElementById('auto-drain-operation-feedback').textContent);
  if (!beforeFeedbackTimeout.includes('submitted') || !beforeFeedbackTimeout.includes('Warning: Admissions throttled')) {
    throw new Error(`Auto-drain start feedback missing confirmation or warnings: ${beforeFeedbackTimeout}`);
  }
  await page.evaluate(() => {
    globalThis.triggerFeedbackClear('auto-drain-operation-feedback');
  });
  const afterFeedbackTimeout = await page.evaluate(() => document.getElementById('auto-drain-operation-feedback').textContent);
  if (afterFeedbackTimeout.includes('submitted') || afterFeedbackTimeout.includes('Auto-concurrency:')) {
    throw new Error(`Confirmation lines not cleared after feedback timeout: ${afterFeedbackTimeout}`);
  }
  if (!afterFeedbackTimeout.includes('Warning: Admissions throttled') || !afterFeedbackTimeout.includes('FIXTURE_TOKEN') || !afterFeedbackTimeout.includes('Note: no')) {
    throw new Error(`Warning or note lines missing after feedback timeout: ${afterFeedbackTimeout}`);
  }
  await page.screenshot({ path: path.join(evidence, 'drain-feedback-warnings-retained.png') });

  // The scoped dock still identifies pull drains and stopped admissions;
  // the chrome keeps the fleet visible when this workspace becomes idle.
  const pullIndicator = () => page.evaluate(() => ({
    globalHidden: document.getElementById('global-drain-state').hidden,
    global: document.getElementById('global-drain-state').textContent,
    globalLabel: document.getElementById('global-drain-state').getAttribute('aria-label'),
    state: document.getElementById('global-drain-state').dataset.drainState,
    tabLabel: document.getElementById('dock-tab-drain').getAttribute('aria-label'),
  }));
  await page.evaluate(() => globalThis.setDrainFixturePull({ runId: 'jrun-pull-fixture' }));
  let pulled = await pullIndicator();
  if (pulled.globalHidden || !pulled.global.includes('1 of 2 workspaces draining') || pulled.tabLabel !== 'Overview: Pull drain') throw new Error(`Pull drain indicator: ${JSON.stringify(pulled)}`);
  await page.evaluate(() => globalThis.setDrainFixturePull({ runId: 'jrun-pull-fixture', admissionsStopped: true }));
  pulled = await pullIndicator();
  if (pulled.globalHidden || !pulled.global.includes('1 of 2 workspaces draining') || pulled.tabLabel !== 'Overview: Pull drain · admissions stopped') throw new Error(`Stopped pull drain indicator: ${JSON.stringify(pulled)}`);
  await page.evaluate(() => globalThis.setDrainFixturePull({}));
  const cleared = await pullIndicator();
  if (cleared.globalHidden || !cleared.global.includes('1 of 2 workspaces draining') || cleared.tabLabel !== 'Overview: idle') throw new Error(`Indicator after pull drain ends: ${JSON.stringify(cleared)}`);

  // ORB-14566: changing concurrency alters the readiness URL but must keep
  // the panel scope stable while that refresh is pending, then restore focus.
  await setDock('start');
  await page.evaluate(() => {
    const input = document.getElementById('auto-drain-concurrency');
    input.focus();
    input.value = '6';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    globalThis.focusedDrainInput = input;
    globalThis.startPendingDrainReadinessRefresh();
  });
  await page.waitForFunction(() => globalThis.drainReadinessRequestPending());
  const pendingDrainRefresh = await page.evaluate(() => {
    const body = document.getElementById('auto-drain-body');
    const input = globalThis.focusedDrainInput;
    return {
      bodyBusy: body.getAttribute('aria-busy'),
      inputConnected: input.isConnected,
      inputFocused: document.activeElement === input,
      loading: [...body.querySelectorAll('[data-panel-status]')].some(note => note.textContent === 'Loading…'),
      liveCount: document.getElementById('auto-drain-phase-label').textContent,
      requestedConcurrency: globalThis.drainReadinessConcurrency(),
    };
  });
  if (pendingDrainRefresh.bodyBusy !== 'true' || !pendingDrainRefresh.inputConnected
    || !pendingDrainRefresh.inputFocused || pendingDrainRefresh.loading
    || pendingDrainRefresh.liveCount === '—' || pendingDrainRefresh.requestedConcurrency !== '6') {
    throw new Error(`Concurrency refresh reset the Drain card or lost focus while pending: ${JSON.stringify(pendingDrainRefresh)}`);
  }
  await page.evaluate(() => globalThis.releasePendingDrainReadinessRefresh());
  const completedDrainRefresh = await page.evaluate(() => ({
    inputFocused: document.activeElement?.id === 'auto-drain-concurrency',
    inputValue: document.getElementById('auto-drain-concurrency')?.value,
    liveCount: document.getElementById('auto-drain-phase-label').textContent,
  }));
  if (!completedDrainRefresh.inputFocused || completedDrainRefresh.inputValue !== '6'
    || completedDrainRefresh.liveCount === '—') {
    throw new Error(`Concurrency refresh did not restore focus and updated state: ${JSON.stringify(completedDrainRefresh)}`);
  }
  await setDock('drain');

  const timeLeft = async target => {
    await target.evaluate(() => globalThis.setDrainFixturePhase('draining'));
    return target.evaluate(() => `${document.getElementById('auto-drain-when').textContent} ${document.getElementById('auto-drain-run').textContent}`);
  };
  const firstBrowser = await timeLeft(page);
  const otherPage = await browser.newPage({ timezoneId: 'America/Los_Angeles' });
  await otherPage.addInitScript({ content: `window.__drainDeadline = ${JSON.stringify(sharedDrainDeadline)};` });
  await otherPage.goto(`http://127.0.0.1:${server.address().port}/`);
  await otherPage.addScriptTag({ type: 'module', url: '/test.mjs' });
  await otherPage.waitForFunction(() => globalThis.operationsTestsPassed);
  const secondBrowser = await timeLeft(otherPage);
  await otherPage.reload();
  await otherPage.addScriptTag({ type: 'module', url: '/test.mjs' });
  await otherPage.waitForFunction(() => globalThis.operationsTestsPassed);
  const afterDrainReload = await timeLeft(otherPage);
  for (const [name, value] of [['second browser', secondBrowser], ['reload', afterDrainReload]]) {
    if (!value.includes('left') || !value.includes('jrun-') || !/\d+h \d{2}m left/.test(value)) throw new Error(`Drain deadline missing after ${name}: ${value}`);
    if (value !== firstBrowser) throw new Error(`Drain deadline differs in ${name}: ${value} vs ${firstBrowser}`);
  }
  await otherPage.close();
  await page.evaluate(() => globalThis.setDrainFixturePhase('draining'));

  // ORB-15226: a plain click on a Drain run link follows it inside the page,
  // while a new-tab click is left to the browser, which opens the link's own
  // address in a new tab. That address names the workspace and time window.
  // macOS opens a new tab on Meta-click, not Control-click.
  await page.evaluate(async () => {
    (await import('/js/common.js')).setWindow('7d');
    await globalThis.setDrainFixturePhase('draining');
  });
  const drainRunLink = page.locator('#auto-drain-run a.operation-run-link');
  const drainRunUrl = new URL(await drainRunLink.getAttribute('href'), page.url());
  if (drainRunUrl.searchParams.get('workspace') !== 'one' || drainRunUrl.searchParams.get('window') !== '7d' || drainRunUrl.hash !== '#runs?run_id=jrun-20260923-0400-a1') throw new Error(`Drain run link lost its scope: ${drainRunUrl}`);
  const urlBeforeClicks = page.url();
  const newTabModifier = process.platform === 'darwin' ? 'Meta' : 'Control';
  const [linkedTab] = await Promise.all([page.context().waitForEvent('page'), drainRunLink.click({ modifiers: [newTabModifier] })]);
  try {
    await linkedTab.waitForURL(drainRunUrl.href);
  } finally {
    await linkedTab.close();
  }
  if (page.url() !== urlBeforeClicks) throw new Error(`A new-tab click on the Drain run link also moved this page: ${urlBeforeClicks} -> ${page.url()}`);
  await drainRunLink.click();
  await page.waitForFunction(() => location.hash.startsWith('#runs/jrun-20260923-0400-a1'));
  if (page.context().pages().length !== 1) throw new Error('A plain click on the Drain run link opened a new tab');
  await page.evaluate(async () => {
    const { persistScopeToUrl, setWindow } = await import('/js/common.js');
    setWindow('24h');
    persistScopeToUrl();
    (await import('/js/router.js')).setActiveTab('auto-drain');
    await globalThis.setDrainFixturePhase('draining');
  });
  await page.waitForFunction(() => location.hash.startsWith('#drain'));

  // ORB-14489: render the live throttle case with spare workspace capacity:
  // the summary blames the throttle, not running tasks.
  await page.setViewportSize({ width: 1024, height: 900 });
  await page.evaluate(() => globalThis.setDrainFixtureReadiness({ capacity: {
    active_leaf_runs: 6, max_active_leaf_runs: 12, free_slots: 0,
    occupancy: { runs: [{ task_ids: ['ORB-14488'], phase: 'post_implementation' }] },
    resource_throttle: { resources: [{ resource: 'cpu', percent: 164, high_percent: 90, resume_percent: 75, since: '2026-10-04T08:40:00Z' }] },
  } }));
  await drainCheck('1024-throttled', null);
  const drainEvidence = await page.evaluate(() => {
    const card = document.getElementById('auto-drain-panel');
    return {
      summary: card.querySelector('.drain-slots').textContent,
      workspace: card.querySelector('.drain-capacity-bar').getAttribute('aria-label'),
      admitted: card.querySelector('[data-stat="admitted"] dd')?.textContent,
      running: card.querySelector('[data-stat="running"] dd')?.textContent,
      throttle: Boolean(document.querySelector('#drain-banner:not([hidden]) .drain-throttle-note')),
      tab: document.getElementById('dock-tab-drain').textContent.trim(),
      stop: card.querySelector('.drain-stop-note:not([hidden])').textContent,
    };
  });
  if (!/throttle.*cpu/.test(drainEvidence.summary) || /finish|must clear/.test(drainEvidence.summary) || !drainEvidence.throttle) throw new Error(`Throttle blamed on running tasks: ${drainEvidence.summary}`);
  if (drainEvidence.workspace !== 'Workspace: 6 of 12 leaf slots in use' || drainEvidence.admitted !== '3' || drainEvidence.running !== '1') throw new Error(`Unscoped Drain counts: ${JSON.stringify(drainEvidence)}`);
  if (drainEvidence.tab !== 'Overview') throw new Error(`Duplicated Drain tab state: ${drainEvidence.tab}`);
  if (!/Stop starting new tasks/.test(drainEvidence.stop) || !/keep running/.test(drainEvidence.stop) || /settlements/.test(drainEvidence.stop)) throw new Error(`Stop help must explain the operator's action: ${drainEvidence.stop}`);
  fs.writeFileSync(path.join(evidence, 'drain-readiness-1024.json'), `${JSON.stringify(drainEvidence, null, 2)}\n`);
  // ORB-14705: a 36-task backlog (17 locks, 13 throttled, 3 saturated, 3 other)
  // leaves the cards fitting at desktop and phone widths, every task in a pool.
  await page.evaluate(() => {
    const task = (n, reason) => ({ task_id: `ORB-${9000 + n}`, status: 'backlog', eligible: false, reason, conflicts: reason === 'context_lock_conflict' ? [{ requested_file: 'file:a.rs', locking_task_id: 'ORB-30' }] : undefined });
    const reasons = [...Array(17).fill('context_lock_conflict'), ...Array(13).fill('resource_throttled'), ...Array(3).fill('capacity_saturated'), 'operator_validation_handoff', 'pilot_already_landed', 'surface_reserved'];
    globalThis.__drainWaitReadinessFixture = {
      capacity: { active_leaf_runs: 6, max_active_leaf_runs: 12, free_slots: 0, resource_throttle: { resources: [{ resource: 'cpu', percent: 164, high_percent: 90, resume_percent: 75, since: '2026-10-04T08:40:00Z' }] } },
      tasks: reasons.map((reason, i) => task(i, reason)),
    };
    return globalThis.setDrainFixtureReadiness(globalThis.__drainWaitReadinessFixture);
  });
  for (const [label, width, dock, height] of [['1440', 1440, 336, 1000], ['390', 390, null, 1000], ['375', 375, null, 812]]) {
    await page.setViewportSize({ width, height });
    await drainCheck(`backlog36-${label}`, dock);
    if (label !== '390') {
      await page.locator('#drain-waits-panel').scrollIntoViewIfNeeded();
      await page.waitForTimeout(350);
      await page.screenshot({ path: path.join(evidence, `drain-waits-${label}.png`) });
    }
    const pooled = await page.evaluate(async () => {
      const { drainReadinessFor } = await import('/js/drain-waits.js');
      return Array.from({ length: 36 }, (_, i) => drainReadinessFor(`ORB-${9000 + i}`)?.pool).filter(Boolean).length;
    });
    if (pooled !== 36) throw new Error(`Readiness at ${label}px does not place all 36 tasks: ${pooled}`);
  }
  const waitRows = await page.evaluate(() => {
    const body = document.getElementById('drain-waits-body');
    const rows = [...body.querySelectorAll('.drain-wait')];
    const links = [...body.querySelectorAll('.drain-wait-task-links a')].map(link => ({ id: link.textContent, href: link.getAttribute('href') }));
    const large = rows.find(row => row.querySelector('.drain-wait-count')?.textContent === '17');
    const short = rows.find(row => row.querySelector('.drain-wait-count')?.textContent === '3');
    return {
      links,
      groups: rows.map(row => ({
        count: Number(row.querySelector('.drain-wait-count')?.textContent),
        ids: [...row.querySelectorAll('.drain-wait-task-links a')].map(link => link.textContent).sort(),
      })),
      largePool: large?.dataset.pool,
      largeDisclosure: Boolean(large?.querySelector('details.drain-wait-task-disclosure')),
      largeOpen: large?.querySelector('details')?.open,
      largeLinkCount: large?.querySelectorAll('.drain-wait-task-links a').length,
      shortDisclosure: Boolean(short?.querySelector('details')),
      shortLinkCount: short?.querySelectorAll('.drain-wait-task-links a').length,
      poolBar: Boolean(body.querySelector('.drain-pool-bar[role="img"]')),
      legend: Boolean(body.querySelector('.drain-pool-legend')),
      holderLink: large?.querySelector('.drain-wait-reason a')?.textContent,
      detail: Boolean(rows.some(row => row.querySelector('.drain-wait-detail'))),
      truncationNote: Boolean(body.querySelector('.drain-waits-note')),
    };
  });
  const expectedWaitTaskIds = Array.from({ length: 36 }, (_, i) => `ORB-${9000 + i}`).sort();
  const expectedWaitGroups = [
    Array.from({ length: 17 }, (_, i) => `ORB-${9000 + i}`),
    Array.from({ length: 13 }, (_, i) => `ORB-${9017 + i}`),
    Array.from({ length: 3 }, (_, i) => `ORB-${9030 + i}`),
    ['ORB-9033'], ['ORB-9034'], ['ORB-9035'],
  ].sort((left, right) => left[0].localeCompare(right[0]));
  const actualWaitTaskIds = waitRows.links.map(link => link.id).sort();
  const actualWaitGroups = waitRows.groups.map(group => group.ids).sort((left, right) => left[0].localeCompare(right[0]));
  if (JSON.stringify(actualWaitTaskIds) !== JSON.stringify(expectedWaitTaskIds)
      || waitRows.links.some(link => !link.href?.includes(link.id))) throw new Error(`Drain reason rows must link every waiting task to its task: ${JSON.stringify(waitRows.links)}`);
  if (JSON.stringify(actualWaitGroups) !== JSON.stringify(expectedWaitGroups)) throw new Error(`Drain reason rows must list only their waiting tasks: ${JSON.stringify(waitRows.groups)}`);
  if (waitRows.largePool !== 'locks' || !waitRows.largeDisclosure || waitRows.largeOpen || waitRows.largeLinkCount !== 17
      || waitRows.shortDisclosure || waitRows.shortLinkCount !== 3) throw new Error(`Drain reason link groups do not use the expected disclosure boundary: ${JSON.stringify(waitRows)}`);
  if (!waitRows.poolBar || !waitRows.legend || waitRows.holderLink !== 'ORB-30' || !waitRows.detail || !waitRows.truncationNote) throw new Error(`Drain waits lost part of the existing card layout: ${JSON.stringify(waitRows)}`);
  await page.locator('#drain-waits-body .drain-wait[data-pool="locks"] details summary').click();
  if (!await page.locator('#drain-waits-body .drain-wait[data-pool="locks"] details').evaluate(details => details.open)) throw new Error('Drain wait disclosure did not open');
  await page.evaluate(() => globalThis.setDrainFixtureReadiness(globalThis.__drainWaitReadinessFixture));
  const expandedWaits = await page.locator('#drain-waits-body .drain-wait[data-pool="locks"] details').evaluate(details => ({
    open: details.open,
    visibleLinks: [...details.querySelectorAll('.drain-wait-task-links a')].filter(link => link.checkVisibility()).length,
  }));
  const waitsOverflow = await page.locator('#drain-waits-panel').evaluate(panel => panel.scrollWidth > panel.clientWidth + 1);
  if (!expandedWaits.open || expandedWaits.visibleLinks !== 17 || waitsOverflow) throw new Error(`Expanded task links must survive refresh and fit on a phone: ${JSON.stringify({ expandedWaits, waitsOverflow })}`);
  await page.setViewportSize({ width: 1024, height: 900 });
  await page.evaluate(() => globalThis.setDrainFixtureReadiness());
  await page.evaluate(async () => { const { setDockMode } = await import('/js/log-tail.js'); setDockMode('log'); });
  const logDrainState = await page.locator('#dock-tab-drain').getAttribute('aria-label');
  if (!logDrainState.includes('Draining')) throw new Error(`Drain tab loses its accessible live state while Log is selected: ${logDrainState}`);
  await page.click('.tab[data-tab="jobs"]');
  await page.click('#global-drain-state');
  if (!page.url().includes('#drain') || await page.locator('#side-dock').getAttribute('data-mode') !== 'drain') throw new Error('Global Drain indicator did not open the card');
  await page.emulateMedia({ reducedMotion: 'reduce' });
  for (const selector of ['#auto-drain-dot', '#global-drain-state .drain-dot']) {
    const animation = await page.locator(selector).evaluate(node => getComputedStyle(node).animationName);
    if (animation !== 'none') throw new Error(`Reduced-motion drain indicator ${selector} animates: ${animation}`);
  }
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await page.setViewportSize({ width: 900, height: 900 });
  await drainCheck('900', null);
  await page.setViewportSize({ width: 375, height: 812 });
  await drainCheck('375x812', null);
  // The approve-proposed opt-in and the live window's approvals line fit the
  // card with no overflow, and the card is captured on its own for review.
  const approveCard = async (label) => {
    await setDock('start');
    await page.evaluate(async () => {
      await globalThis.setDrainFixturePhase('idle');
      document.getElementById('auto-drain-approve').click();
    });
    const form = await page.evaluate(() => {
      const card = document.getElementById('auto-drain-start');
      const box = card.getBoundingClientRect();
      const label = card.querySelector('.drain-field-approve');
      return {
        selected: document.getElementById('auto-drain-approve').checked,
        overflowing: Array.from(card.querySelectorAll('*')).filter((node) => node.getClientRects().length > 0 && node.getBoundingClientRect().right > box.right + 1).map((node) => node.className || node.tagName),
        reachable: label.getBoundingClientRect().width > 0,
      };
    });
    if (!form.selected || !form.reachable || form.overflowing.length) throw new Error(`Approve-proposed control at ${label}: ${JSON.stringify(form)}`);
    await page.waitForTimeout(400);
    await page.locator('#auto-drain-start .drain-field-approve').screenshot({ path: path.join(evidence, `drain-approve-form-${label}.png`) });
    await setDock('drain');
    await page.evaluate(async () => {
      await globalThis.setDrainFixturePhase('draining');
      await globalThis.setDrainFixtureApprovals({ enabled: true, approved_total: 3, held_total: 2, held_by_reason: { missing_complexity: 1, warnings: 1 }, held: [{ task_id: 'ORB-8', reason: 'missing_complexity' }, { task_id: 'ORB-22', reason: 'warnings', findings: [{ kind: 'utility_warnings', text: 'Screenshots cannot be attached because the allowlist has no orbit.task.artifact.put' }] }] });
    });
    const live = await page.evaluate(() => {
      const card = document.getElementById('auto-drain-panel');
      const box = card.getBoundingClientRect();
      return {
        approved: card.querySelector('[data-stat="approved"] dd')?.firstChild?.textContent,
        held: card.querySelector('[data-stat="approved"] .drain-window-stat-detail')?.textContent,
        summary: card.querySelector('.drain-approval-summary')?.textContent || '',
        overflowing: Array.from(card.querySelectorAll('*')).filter((node) => node.getClientRects().length > 0 && node.getBoundingClientRect().right > box.right + 1).map((node) => node.className || node.tagName),
      };
    });
    if (live.approved !== '3' || live.held !== '2 held' || live.summary !== '1 missing complexity · 1 held for pilot warnings' || live.overflowing.length) throw new Error(`Live approvals at ${label}: ${JSON.stringify(live)}`);
    await page.waitForTimeout(400);
    await page.locator('#auto-drain-panel').screenshot({ path: path.join(evidence, `drain-approve-live-${label}.png`) });
    await page.evaluate(async () => {
      await globalThis.setDrainFixtureApprovals({ enabled: false });
      const approve = document.getElementById('auto-drain-approve');
      if (approve?.checked) approve.click();
      await globalThis.setDrainFixturePhase('draining');
    });
  };
  await approveCard('375x812');
  await page.setViewportSize({ width: 1440, height: 1000 });
  await approveCard('1440-dock336');
  await page.setViewportSize({ width: 375, height: 812 });

  // The full-page Log toolbar keeps every control reachable on desktop and
  // phone, while following and while paused with
  // the buffered action shown. Pausing and buffering go through the real
  // follow handler and SSE consumer, fed by a fixture stream.
  await page.evaluate(async () => {
    const fixtureFetch = globalThis.fetch;
    // The first snapshot fails (dashboard restart / 5xx). Later attempts
    // succeed only after the test releases the server, so the retry and the
    // disconnected state are observable before any EventSource exists.
    let logSnapshotAttempts = 0;
    let releaseSnapshot = false;
    globalThis.logSnapshotAttempts = () => logSnapshotAttempts;
    globalThis.releaseLogSnapshot = () => { releaseSnapshot = true; };
    globalThis.fetch = (url, options) => {
      const pathname = new URL(url, location.href).pathname;
      if (pathname !== '/api/log') return fixtureFetch(url, options);
      logSnapshotAttempts += 1;
      if (!releaseSnapshot) {
        return Promise.resolve({
          ok: false,
          status: 503,
          text: async () => JSON.stringify({ error: 'log snapshot unavailable' }),
        });
      }
      return Promise.resolve({
        ok: true,
        status: 200,
        json: async () => ({
          events: [{
            ts: '2026-09-27T12:00:00Z',
            level: 'info',
            code: 'OK',
            source: 'orbit.log',
            message_html: 'snapshot recovered',
          }],
          offset: 42,
          agent_offset: 12,
          cursor: '42@operation-generation:12@agent-generation',
        }),
      });
    };
    globalThis.EventSource = class FixtureStream {
      static CLOSED = 2;
      constructor(url) {
        globalThis.logFixtureStream = this;
        globalThis.logFixtureUrl = String(url);
        this.readyState = 1;
        queueMicrotask(() => {
          if (this.readyState === FixtureStream.CLOSED) return;
          if (typeof this.onopen === 'function') this.onopen();
        });
      }
      close() { this.readyState = FixtureStream.CLOSED; }
    };
    const { initLogTail, setDockMode } = await import('/js/log-tail.js');
    initLogTail();
    setDockMode('log');
  });
  await page.waitForFunction(() => {
    const bar = document.getElementById('log-statusbar');
    const box = bar ? bar.getBoundingClientRect() : { width: 0, height: 0 };
    const label = bar?.querySelector('.sb-label')?.textContent;
    return box.width > 0 && box.height > 0
      && bar.classList.contains('disconnected')
      && document.getElementById('side-dock')?.classList.contains('disconnected')
      && label === 'log stream unavailable, retrying'
      && globalThis.logSnapshotAttempts() >= 1
      && !globalThis.logFixtureStream;
  });
  // Hiding the tab must cancel the snapshot backoff. A background tab neither
  // retries nor opens a stream; showing it resumes, and only the recovered
  // snapshot may open the EventSource.
  const attemptsAtFailure = await page.evaluate(() => {
    let hidden = true;
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden });
    document.dispatchEvent(new Event('visibilitychange'));
    globalThis.__setLogHidden = (value) => { hidden = value; };
    return globalThis.logSnapshotAttempts();
  });
  await page.waitForTimeout(1300);
  const whileHidden = await page.evaluate(() => ({
    attempts: globalThis.logSnapshotAttempts(),
    stream: Boolean(globalThis.logFixtureStream),
  }));
  if (whileHidden.attempts !== attemptsAtFailure || whileHidden.stream) {
    throw new Error(`hidden tab retried the snapshot or opened a stream: ${JSON.stringify(whileHidden)} after ${attemptsAtFailure}`);
  }
  await page.evaluate(() => {
    globalThis.__setLogHidden(false);
    document.dispatchEvent(new Event('visibilitychange'));
    delete document.hidden;
    globalThis.releaseLogSnapshot();
  });
  await page.waitForFunction(() => {
    const bar = document.getElementById('log-statusbar');
    const dock = document.getElementById('side-dock');
    const stream = globalThis.logFixtureStream;
    return stream
      && stream.readyState === 1
      && typeof stream.onmessage === 'function'
      && bar
      && !bar.classList.contains('disconnected')
      && dock
      && !dock.classList.contains('disconnected')
      && bar.getAttribute('aria-label') === 'Latest log line';
  }, undefined, { timeout: 20000 });
  const recovered = await page.evaluate(() => ({
    attempts: globalThis.logSnapshotAttempts(),
    url: globalThis.logFixtureUrl,
    text: document.getElementById('logInner')?.textContent || '',
  }));
  if (recovered.attempts <= attemptsAtFailure) {
    throw new Error(`snapshot was not retried after the first failure: ${recovered.attempts} (at failure ${attemptsAtFailure})`);
  }
  if (!String(recovered.url).includes('from=42')) {
    throw new Error(`stream did not resume from the recovered snapshot offset: ${recovered.url}`);
  }
  if (!recovered.text.includes('snapshot recovered')) {
    throw new Error(`recovered snapshot was not rendered: ${recovered.text}`);
  }
  const snapshotCursor = new URL(recovered.url, 'http://fixture').searchParams.get('cursor');
  if (snapshotCursor !== '42@operation-generation:12@agent-generation') {
    throw new Error(`stream lost snapshot generations: ${recovered.url}`);
  }
  // A hidden-tab disconnect must retain the entire last SSE ID, including
  // independent file generations, even if the byte offsets moved backwards.
  const resumedCursor = await page.evaluate(() => {
    const old = globalThis.logFixtureStream;
    old.onmessage({ lastEventId: '17@rotated-operation:19@rotated-agent', data: JSON.stringify({
      ts: '2026-09-27T12:00:00Z', level: 'info', code: 'OK', source: 'orbit.log',
      message_html: 'rotation replay recovered',
    }) });
    let hidden = true;
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden });
    document.dispatchEvent(new Event('visibilitychange'));
    if (old.readyState !== EventSource.CLOSED) throw new Error('hidden tab did not close its stream');
    hidden = false;
    document.dispatchEvent(new Event('visibilitychange'));
    delete document.hidden;
    if (globalThis.logFixtureStream === old) throw new Error('visible tab did not reopen its stream');
    return globalThis.logFixtureUrl;
  });
  const resumedQuery = new URL(resumedCursor, 'http://fixture').searchParams;
  if (resumedQuery.get('cursor') !== '17@rotated-operation:19@rotated-agent'
      || resumedQuery.get('from') !== '17' || resumedQuery.get('agent_from') !== '19') {
    throw new Error(`hidden-tab reconnect lost the SSE cursor: ${resumedCursor}`);
  }
  const logToolbarCheck = async (label, viewport, dockWidth, paused) => {
    await page.setViewportSize(viewport);
    const ids = ['all', 'err', 'deny', 'warn'].map(filter => `.log-filters .filter-pill[data-filter="${filter}"]`)
      .concat(['#log-show-agent', '#log-follow-tail', '#log-wrap-lines'], paused ? ['#log-buffered-count'] : []);
    const layout = await page.evaluate((selectors) => {
      const bar = document.querySelector('#side-dock .log-filters');
      const barBox = bar.getBoundingClientRect();
      const unreachable = selectors.filter((selector) => {
        const box = document.querySelector(selector).getBoundingClientRect();
        return box.width === 0 || box.height === 0 || box.left < barBox.left - 1 || box.right > barBox.right + 1
          || box.top < barBox.top - 1 || box.bottom > barBox.bottom + 1;
      });
      return { dockWidth: Math.round(document.getElementById('side-dock').getBoundingClientRect().width), clipped: bar.scrollWidth > bar.clientWidth + 1, unreachable };
    }, ids);
    if (layout.dockWidth < viewport.width - 300) throw new Error(`Log does not fill the page at ${label}: ${layout.dockWidth}`);
    if (layout.clipped || layout.unreachable.length) throw new Error(`Log toolbar clips controls at ${label}: ${JSON.stringify(layout)}`);
    for (const selector of ids) {
      await page.focus(selector);
      const focused = await page.evaluate(sel => document.activeElement === document.querySelector(sel), selector);
      if (!focused) throw new Error(`Log toolbar control ${selector} not keyboard focusable at ${label}`);
      // The trial click fails if another element covers the control's centre.
      await page.click(selector, { trial: true, timeout: 2000 });
    }
    await page.screenshot({ path: path.join(evidence, `log-toolbar-${label}.png`), fullPage: true });
  };
  const logToolbarSizes = [
    ['1024', { width: 1024, height: 900 }, null],
    ['1440', { width: 1440, height: 1000 }, null],
    ['375', { width: 375, height: 812 }, null],
  ];
  for (const [label, viewport, width] of logToolbarSizes) await logToolbarCheck(`${label}-following`, viewport, width, false);
  await page.click('#log-follow-tail');
  await page.evaluate(() => {
    for (let id = 1; id <= 1123; id += 1) {
      globalThis.logFixtureStream.onmessage({ lastEventId: String(id), data: JSON.stringify({ ts: '2026-09-27T12:00:00Z', level: 'info', code: 'fixture', msg: `event ${id}` }) });
    }
  });
  const buffered = await page.locator('#log-buffered-count').textContent();
  if (!/buffered · \d+ discarded/.test(buffered)) throw new Error(`Paused SSE events did not show the buffered action: ${buffered}`);
  for (const [label, viewport, width] of logToolbarSizes) await logToolbarCheck(`${label}-paused`, viewport, width, true);
  await page.click('#log-buffered-count');
  const resumed = await page.evaluate(() => getComputedStyle(document.getElementById('log-buffered-count')).display);
  if (resumed !== 'none') throw new Error('Clicking the buffered action did not flush the paused buffer');

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
  // A phone row stacks: the title, then the ID line, then the crew and action.
  if (!taskLayout.areas.includes('"title title"') || !taskLayout.areas.includes('"crew quick"')) {
    throw new Error(`375px task row must stack the title over the crew and action, got ${JSON.stringify(taskLayout)}`);
  }
  await assertNoOverflow('375x812 / tasks');
  await page.screenshot({ path: path.join(evidence, 'tasks-375x812.png'), fullPage: true });

  // Exercise the real shell and hash router at phone width. The content is
  // fixture data; navigation, scrolling, keyboard focus and layout are native.
  await page.evaluate(() => {
    for (const [id, value] of Object.entries({
      'rail-count-diag-incidents': '3',
      'rail-count-ops-routines': '4/5',
      'rail-count-ops-auto-tasks': '8/16',
      'rail-count-jobs': '33 running',
    })) document.getElementById(id).textContent = value;
  });
  const mobileEvidence = { routes: [], navigation: [], counts: [] };
  for (const [route, panel] of [
    ['tasks', 'tasks-panel'],
    ['jobs/history', 'runs-panel'],
    ['runs/jrun-dashboard-fixture', 'run-detail-panel'],
    ['diagnostics/incidents', 'diagnostics-panel'],
  ]) {
    await page.evaluate(hash => { location.hash = hash; }, `#${route}`);
    await page.waitForFunction(id => document.getElementById(id).closest('.tab-pane').classList.contains('active'), panel);
    await page.locator(`#${panel}`).evaluate(node => Promise.all(node.closest('.tab-pane').getAnimations().map(animation => animation.finished)));
    const layout = await page.locator(`#${panel}`).evaluate(node => {
      const pane = node.closest('.tab-pane');
      pane.scrollTop = 0;
      const box = node.getBoundingClientRect();
      // A view's secondary tabs head the pane; its panel follows right under them.
      const views = [...pane.querySelectorAll(':scope > .view-tabs')].find(bar => !bar.hidden);
      const bar = views ? views.getBoundingClientRect() : null;
      return { top: bar ? bar.top : box.top, panelTop: box.top, barBottom: bar ? bar.bottom : null, width: box.width, scrollWidth: document.scrollingElement.scrollWidth };
    });
    if (layout.width <= 0 || layout.top < 0 || layout.top > 200) throw new Error(`First panel too low at #${route}: ${JSON.stringify(layout)}`);
    if (layout.barBottom !== null && layout.panelTop - layout.barBottom > 24) throw new Error(`Panel drifts below its view tabs at #${route}: ${JSON.stringify(layout)}`);
    await assertNoOverflow(`375x812 / #${route}`);
    mobileEvidence.routes.push({ route, ...layout });
    await page.screenshot({ path: path.join(evidence, `shell-${route.replaceAll('/', '-')}-375x812.png`), fullPage: true });
  }
  const destinations = await page.locator('.rail .tab').evaluateAll(nodes => nodes.map(node => node.dataset.tab));
  for (const destination of destinations) {
    await page.click(`.rail .tab[data-tab="${destination}"]`);
    const selectors = await page.locator('.rail .tab, .rail-subtabs:not(.dimmed) .subtab').evaluateAll(nodes => nodes.map(node =>
      node.classList.contains('tab') ? `.rail .tab[data-tab="${node.dataset.tab}"]` : `#${node.parentElement.id} .subtab[data-subtab="${node.dataset.subtab}"]`));
    // Tab through every visible entry from the workspace selector. Browser
    // focus must scroll the overflow row without hiding its focus ring.
    await page.focus('#workspace-select');
    for (const selector of selectors) {
      await page.keyboard.press('Tab');
      const focused = await page.locator(selector).evaluate(node => {
        const box = node.getBoundingClientRect();
        const row = document.getElementById('tabs').getBoundingClientRect();
        const style = getComputedStyle(node);
        return {
          active: document.activeElement === node && node.matches(':focus-visible'),
          ring: style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) >= 2,
          onscreen: box.width > 0 && box.height > 0 && box.left >= row.left - 1 && box.right <= row.right + 1,
          sameRow: box.top >= row.top && box.bottom <= row.bottom,
          left: box.left, right: box.right, rowLeft: row.left, rowRight: row.right,
        };
      });
      if (!focused.active || !focused.ring || !focused.onscreen || !focused.sameRow) throw new Error(`Unreachable navigation at ${destination} / ${selector}: ${JSON.stringify(focused)}`);
      await page.click(selector, { trial: true, timeout: 2000 });
    }
    const subtabs = selectors.filter(selector => selector.includes('.subtab'));
    for (const selector of subtabs) {
      await page.click(selector);
      await page.waitForFunction(sel => document.querySelector(sel).classList.contains('active'), selector);
      // Enter must activate the same route through the native button handler.
      await page.focus(selector);
      await page.keyboard.press('Enter');
      const subtab = await page.locator(selector).getAttribute('data-subtab');
      if (!new URL(page.url()).hash.startsWith(`#${destination}/${subtab}`)) throw new Error(`Subtab did not route at ${selector}: ${page.url()}`);
      await assertNoOverflow(`375x812 / ${destination}/${subtab}`);
      const gap = await page.locator(selector).evaluate(node => {
        const count = node.querySelector('.rail-count');
        if (!count?.textContent) return null;
        const label = document.createRange();
        label.selectNodeContents(node.firstChild);
        return count.getBoundingClientRect().left - label.getBoundingClientRect().right;
      });
      if (gap !== null && gap < 6) throw new Error(`Subtab count touches its label at ${selector}: ${gap}px`);
      if (gap !== null) mobileEvidence.counts.push({ selector, gap });
    }
    await assertNoOverflow(`375x812 / ${destination}`);
    mobileEvidence.navigation.push({ destination, controls: selectors.length, subtabs: subtabs.length });
  }
  // The widest readings the chips are sized for, with one resource held.
  const header = await page.evaluate(async () => {
    const { renderHostResources } = await import('/js/host-resources.js');
    renderHostResources({
      cpu: { percent: 1234, severity: 'critical' }, memory: { percent: 100, severity: 'critical' },
      disk: { path: '/workspace', percent: 100, severity: 'critical' }, sample_age_seconds: 1, max_age_seconds: 15,
      stale: false, throttle: true, pressures: [{ resource: 'memory' }], reason: 'memory high', thresholds: { enabled: true },
    });
    const workspace = document.getElementById('workspace-select').getBoundingClientRect();
    const brand = document.querySelector('.rail-brand').getBoundingClientRect();
    const drain = document.getElementById('global-drain-state').getBoundingClientRect();
    const refresh = document.getElementById('refresh-btn').getBoundingClientRect();
    const strip = document.getElementById('host-resource-chips');
    const row = strip.getBoundingClientRect();
    const chips = [...strip.querySelectorAll('.host-resource')].map(node => (
      { resource: node.dataset.resource, box: node.getBoundingClientRect().toJSON(), clipped: node.scrollWidth > node.clientWidth }));
    return {
      // The phone layout (ORB-15232) puts the brand, Drain and Refresh on the
      // top bar's first row and gives the host and workspace pickers their own
      // row below it.
      actionsInHeader: [brand, drain, refresh].every(box => box.width > 0 && Math.abs(box.top + box.height / 2 - brand.top - brand.height / 2) < 1),
      pickersBelowActions: workspace.width > 0 && workspace.top >= Math.max(brand.bottom, drain.bottom, refresh.bottom) && workspace.right <= innerWidth,
      chipsSingleRow: chips.length === 3 && chips.every(({ box }) => box.width > 0 && Math.abs(box.top - chips[0].box.top) < 1),
      // All three readings are in view at 375px without scrolling the row.
      chipsInView: chips.every(({ box, clipped }) => !clipped && box.left >= row.left - 0.5 && box.right <= Math.min(row.right, innerWidth) + 0.5),
      chips,
    };
  });
  if (!header.actionsInHeader || !header.pickersBelowActions || !header.chipsSingleRow || !header.chipsInView) throw new Error(`Phone header or host chips wrapped or clipped: ${JSON.stringify(header)}`);
  await page.screenshot({ path: path.join(evidence, 'topbar-375x812.png') });
  for (const selector of ['#refresh-btn', '#global-drain-state', ...['cpu', 'memory', 'disk'].map(resource => `#host-resource-chips [data-resource="${resource}"]`)]) {
    await page.locator(selector).focus();
    await page.locator(selector).scrollIntoViewIfNeeded();
    await page.locator(selector).click({ trial: true });
  }
  mobileEvidence.header = header;
  fs.writeFileSync(path.join(evidence, 'mobile-shell.json'), `${JSON.stringify(mobileEvidence, null, 2)}\n`);
  console.log(`PASS: phone shell panel positions ${JSON.stringify(mobileEvidence.routes)}; all ${destinations.length} destinations and active rail subtabs reachable; count gaps >=6px; no page overflow.`);

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
    let diag = 'incidents';
    let jobs = 'live';
    let operations = 'routines';
    let knowledge = 'frictions';
    let incidentsView = 'incidents';
    let analytics = 'metrics';
    let reliabilityView = 'pipeline';
    let agentsRole = 'orchestrators';
    initRouter({
      getTab: () => tab, setTab: (value) => { tab = value; },
      getJobsSubtab: () => jobs, setJobsSubtab: (value) => { jobs = value; },
      getDiagSubtab: () => diag, setDiagSubtab: (value) => { diag = value; },
      getOperationsSubtab: () => operations, setOperationsSubtab: (value) => { operations = value; },
      getKnowledgeSubtab: () => knowledge, setKnowledgeSubtab: (value) => { knowledge = value; },
      getRunId: () => null, setRunId: () => {}, getRunSubtab: () => 'steps', setRunSubtab: () => {},
      getRunDetail: () => null, setRunDetail: () => {}, getRunEvents: () => [], setRunEvents: () => {},
      getRunLogs: () => [], setRunLogs: () => {}, getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
      getLastRuns: () => [], refreshDashboard: () => {}, renderDiagnostics: () => {},
      fitLogPanelToViewport: () => {},
      getIncidentsView: () => incidentsView, setIncidentsView: (value) => { incidentsView = value; },
      getAnalyticsSubtab: () => analytics, setAnalyticsSubtab: (value) => { analytics = value; },
      getReliabilityView: () => reliabilityView, setReliabilityView: (value) => { reliabilityView = value; },
      getAgentsRole: () => agentsRole, setAgentsRole: (value) => { agentsRole = value; },
      applyAuditHashQuery: () => {}, syncAuditControls: () => {},
      buildAuditHash: () => '#diagnostics/incidents/events?status=non_success',
      applyCodebaseHashQuery: () => {}, buildCodebaseHash: () => '#analytics/codebase',
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
  // A browser in UTC, a host cron zone elsewhere: triggers read in the host
  // zone and agree with the UTC next fire beside them.
  const utcPage = await browser.newPage({ viewport: { width: 1440, height: 1000 }, timezoneId: 'UTC' });
  utcPage.on('pageerror', error => { pageErrors.push(String(error)); console.error(error); });
  await utcPage.goto(`http://127.0.0.1:${server.address().port}/#operations/auto-tasks`);
  await utcPage.addScriptTag({ type: 'module', url: '/cron-zone.mjs' });
  await utcPage.waitForFunction(() => globalThis.cronZoneTestsPassed, undefined, { timeout: 15000 });
  await utcPage.screenshot({ path: path.join(evidence, 'cron-zone-utc-browser.png') });
  // State- and delivery-triggered routines read by their own trigger, and the
  // next-hour strip draws every slot of a frequent cron routine.
  const triggerPage = await browser.newPage({ viewport: { width: 1440, height: 1000 }, timezoneId: 'UTC' });
  triggerPage.on('pageerror', error => { pageErrors.push(String(error)); console.error(error); });
  await triggerPage.goto(`http://127.0.0.1:${server.address().port}/#operations/routines`);
  await triggerPage.addScriptTag({ type: 'module', url: '/routine-triggers.mjs' });
  await triggerPage.waitForFunction(() => globalThis.routineTriggerTestsPassed, undefined, { timeout: 15000 });
  await triggerPage.screenshot({ path: path.join(evidence, 'routine-triggers-browser.png') });

  // Operation mutation success expiry retains readback failure diagnostics.
  const feedbackPage = await browser.newPage({ viewport: { width: 1440, height: 1000 }, timezoneId: 'America/Los_Angeles' });
  feedbackPage.on('pageerror', error => { pageErrors.push(String(error)); console.error(error); });
  await feedbackPage.clock.install();
  await feedbackPage.goto(`http://127.0.0.1:${server.address().port}/#operations/routines`);
  await feedbackPage.addScriptTag({ type: 'module', url: '/feedback-timer.mjs' });
  await feedbackPage.waitForFunction(() => globalThis.review, undefined, { timeout: 20000 });
  await feedbackPage.clock.pauseAt(await feedbackPage.evaluate(() => Date.now() + 1000));

  const feedbackText = () => feedbackPage.locator('#auto-task-operation-feedback').textContent();

  // 1. Successful mint with failed readback retains the diagnostic after 60s
  await feedbackPage.evaluate(() => review.mint(true));
  const beforeExpiry = await feedbackText();
  if (!beforeExpiry.includes('Refresh failed:') || !beforeExpiry.includes('The action succeeded') || !beforeExpiry.includes('Task created')) {
    throw new Error(`initial mint with failed readback missing feedback: ${beforeExpiry}`);
  }
  await feedbackPage.clock.runFor(60001);
  const afterExpiry = await feedbackText();
  if (!afterExpiry.includes('Refresh failed:') || !afterExpiry.includes('The action succeeded')) {
    throw new Error(`readback failure warning erased after expiry: ${afterExpiry}`);
  }
  if (afterExpiry.includes('Task created')) {
    throw new Error(`success confirmation did not expire independently: ${afterExpiry}`);
  }
  await feedbackPage.screenshot({ path: path.join(evidence, 'operations-readback-warning-retained.png') });

  // 2. Normal success clears completely after 60s
  await feedbackPage.evaluate(() => review.refresh());
  await feedbackPage.evaluate(() => review.mint(false));
  const plainSuccess = await feedbackText();
  if (!plainSuccess.includes('Task created')) throw new Error(`plain mint did not report success: ${plainSuccess}`);
  await feedbackPage.clock.runFor(59999);
  const justBefore = await feedbackText();
  if (!justBefore.includes('Task created')) throw new Error(`plain success cleared before deadline: ${justBefore}`);
  await feedbackPage.clock.runFor(2);
  const plainExpired = await feedbackText();
  if (plainExpired !== '') throw new Error(`plain success did not clear after 60s: ${plainExpired}`);

  // 3. Newer error feedback is protected from expiry
  await feedbackPage.evaluate(() => review.mint(false));
  await feedbackPage.clock.runFor(30000);
  await feedbackPage.evaluate(() => review.failMint());
  const failureText = await feedbackText();
  if (!failureText.includes('mutation refused')) throw new Error(`failure feedback missing: ${failureText}`);
  await feedbackPage.clock.runFor(60001);
  const retainedFailure = await feedbackText();
  if (failureText !== retainedFailure) throw new Error(`error feedback replaced or cleared: ${retainedFailure}`);

  // 4. Workspace change clears feedback
  await feedbackPage.evaluate(() => review.mint(true));
  await feedbackPage.clock.runFor(60001);
  if (!await feedbackText().then(t => t.includes('Refresh failed:'))) throw new Error('warning missing before switch');
  await feedbackPage.evaluate(() => review.switchWorkspace('two'));
  const afterSwitch = await feedbackText();
  if (afterSwitch !== '') throw new Error(`workspace switch did not clear feedback: ${afterSwitch}`);

  await feedbackPage.close();

  if (pageErrors.length) throw new Error(`page errors during the scenario: ${pageErrors.join(' | ')}`);
  console.log(`PASS: Chromium Operations fixture; 1440/672/390/375; phone shell, subtabs, Drain page at 1440/900/375, full-page Log toolbar following and paused, Running now task-grouped job id wrapping at 280px, reload, history, feedback timer expiry. Screenshots: ${evidence}`);
} finally {
  await browser?.close(); server.close();
}
