// Usage: node dashboard_operations_phone_dock_browser.mjs /absolute/path/to/playwright/index.mjs .orbit/tmp/phone-dock-browser
import { pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const playwrightModule = process.argv[2];
if (!playwrightModule || !process.argv[3]) {
  throw new Error('Usage: node dashboard_operations_phone_dock_browser.mjs <playwright-module> <evidence-directory>');
}
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const { chromium } = await import(pathToFileURL(path.resolve(playwrightModule)).href);
let drainPhase = 'idle';
const server = http.createServer((req, res) => {
  const pathname = new URL(req.url, 'http://fixture').pathname;
  if (req.method !== 'GET') throw new Error(`Unexpected fixture mutation: ${req.method} ${pathname}`);
  const json = value => { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(value)); };
  if (pathname === '/api/workflows/auto/readiness') {
    json({ controls_authorized: true, capacity: {
      drain_phase: drainPhase, drain_run_id: drainPhase === 'draining' ? 'jrun-fixture' : null,
      active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4,
    }, tasks: drainPhase === 'draining' ? [{ id: 'ORB-1', eligible: true, title: 'Ready task' }] : [] });
    return;
  }
  if (pathname === '/api/fleet/drains') { json({ items: [] }); return; }
  if (pathname === '/api/tasks/ORB-1') { json({ id: 'ORB-1', context_files: ['file:src/shared.rs'] }); return; }
  if (pathname === '/api/log') {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ events: [{ level: 'warn', code: 'fixture.warning',
      ts: '2026-10-29T12:00:00Z', message_html: 'Fixture warning retained across dock switches.' }], offset: 1, agent_offset: 0 }));
    return;
  }
  if (pathname === '/api/log/stream') {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    res.write(': fixture stream connected\n\n');
    return;
  }
  const served = dashboardFile(pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (pathname === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

let browser;
const pageErrors = [];
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 375, height: 812 }, timezoneId: 'America/Los_Angeles' });
  page.on('pageerror', error => pageErrors.push(String(error)));
  await page.goto(`http://127.0.0.1:${server.address().port}/#tasks?status=backlog&q=phone`);
  const fixtureBootstrap = `
    const { initRouter, initTabs } = await import('/js/router.js');
    const { initLogTail, setDockMode, setDockRouteShown } = await import('/js/log-tail.js');
    const { applyTasksHashQuery, buildChips, buildTasksHash, syncTaskControls } = await import('/js/tasks.js');
    const statusOrder = ['in-progress', 'review', 'blocked', 'proposed', 'backlog', 'someday', 'done', 'rejected', 'archived'];
    const defaultActiveStatuses = statusOrder.filter(status => !['someday', 'done', 'rejected', 'archived'].includes(status));
    let activeStatuses = new Set(defaultActiveStatuses);
    let searchQuery = '';
    const taskContext = {
      getTasks: () => [], getTasksMeta: () => null,
      getActiveStatuses: () => activeStatuses, setActiveStatuses: value => { activeStatuses = value; },
      getSearchQuery: () => searchQuery, setSearchQuery: value => { searchQuery = value; },
      statusOrder, defaultActiveStatuses,
    };
    buildChips(taskContext);
    let tab = 'tasks';
    let jobs = 'live';
    let diagnostics = 'incidents';
    let incidents = 'incidents';
    let analytics = 'metrics';
    let reliability = 'pipeline';
    let agents = 'orchestrators';
    let knowledge = 'frictions';
    let operations = 'routines';
    let config = 'effective';
    let runId = null;
    let runSubtab = 'steps';
    initRouter({
      getTab: () => tab, setTab: value => { tab = value; },
      getJobsSubtab: () => jobs, setJobsSubtab: value => { jobs = value; },
      getDiagSubtab: () => diagnostics, setDiagSubtab: value => { diagnostics = value; },
      getIncidentsView: () => incidents, setIncidentsView: value => { incidents = value; },
      getAnalyticsSubtab: () => analytics, setAnalyticsSubtab: value => { analytics = value; },
      getReliabilityView: () => reliability, setReliabilityView: value => { reliability = value; },
      getAgentsRole: () => agents, setAgentsRole: value => { agents = value; },
      getKnowledgeSubtab: () => knowledge, setKnowledgeSubtab: value => { knowledge = value; },
      getOperationsSubtab: () => operations, setOperationsSubtab: value => { operations = value; },
      getConfigSubtab: () => config, setConfigSubtab: value => { config = value; },
      getRunId: () => runId, setRunId: value => { runId = value; },
      getRunSubtab: () => runSubtab, setRunSubtab: value => { runSubtab = value; },
      getRunDetail: () => null, setRunDetail: () => {}, getRunEvents: () => [], setRunEvents: () => {},
      getRunLogs: () => [], setRunLogs: () => {}, getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
      getLastRuns: () => [], refreshDashboard: () => {}, renderDiagnostics: () => {},
      fitLogPanelToViewport: () => {}, setDrainView: mode => setDockMode(mode), setDockRouteShown,
      applyTasksHashQuery: (query, taskPage) => applyTasksHashQuery(query, taskContext, taskPage),
      buildTasksHash: () => buildTasksHash(taskContext), syncTaskControls: () => syncTaskControls(taskContext),
    });
    // Bind the production Back control before any dock visibility assertions.
    initLogTail();
    initTabs();
    globalThis.phoneDockFixtureReady = true;
  `;
  await page.addScriptTag({ type: 'module', content: fixtureBootstrap });
  await page.waitForFunction(() => globalThis.phoneDockFixtureReady);

  const globalDrain = page.locator('#global-drain-state');
  await globalDrain.evaluate(node => {
    node.hidden = false;
    node.href = '#tasks?status=backlog&q=phone&dock=drain';
  });
  await globalDrain.click();
  await page.locator('#side-dock[data-mode="drain"]').waitFor({ state: 'visible' });
  await page.locator('#tasks-main > .col-tasks').waitFor({ state: 'hidden' });
  const drainRoute = await page.evaluate(() => ({
    hash: location.hash,
    dock: document.getElementById('side-dock').dataset.mode,
    listVisible: document.querySelector('#tasks-main > .col-tasks').getClientRects().length > 0,
  }));
  if (drainRoute.dock !== 'drain' || drainRoute.listVisible || !drainRoute.hash.includes('dock=drain')) {
    throw new Error(`Phone Drain route did not replace the task list: ${JSON.stringify(drainRoute)}`);
  }
  await page.screenshot({ path: path.join(evidence, 'tasks-dock-phone-drain-dark-375.png'), animations: 'disabled' });

  await page.evaluate(() => { location.hash = '#tasks?status=backlog&q=phone&dock=log'; });
  await page.locator('#side-dock[data-mode="log"] #log-panel').waitFor({ state: 'visible' });
  const logRoute = await page.evaluate(() => ({
    hash: location.hash,
    dock: document.getElementById('side-dock').dataset.mode,
    listVisible: document.querySelector('#tasks-main > .col-tasks').getClientRects().length > 0,
  }));
  if (logRoute.dock !== 'log' || logRoute.listVisible || !logRoute.hash.includes('dock=log')) {
    throw new Error(`Phone Log route did not replace the task list: ${JSON.stringify(logRoute)}`);
  }
  await page.screenshot({ path: path.join(evidence, 'tasks-dock-phone-log-dark-375.png'), animations: 'disabled' });

  await page.locator('#dock-back-tasks').focus();
  await page.keyboard.press('Enter');
  await page.locator('#tasks-main > .col-tasks').waitFor({ state: 'visible' });
  const returnedRoute = await page.evaluate(() => ({
    hash: location.hash,
    listVisible: document.querySelector('#tasks-main > .col-tasks').getClientRects().length > 0,
    dockVisible: document.getElementById('side-dock').getClientRects().length > 0,
    search: document.getElementById('task-search').value,
    backlogActive: document.querySelector('#task-filter [data-status="backlog"]').getAttribute('aria-pressed'),
  }));
  if (returnedRoute.hash !== '#tasks?status=backlog&q=phone'
    || !returnedRoute.listVisible || returnedRoute.dockVisible
    || returnedRoute.search !== 'phone' || returnedRoute.backlogActive !== 'true') {
    throw new Error(`Keyboard Back did not restore the filtered task list: ${JSON.stringify(returnedRoute)}`);
  }

  // ORB-19732: a desktop tab selection must stay visible after a resize to
  // phone width. chooseDockMode writes the Tasks hash with replaceState,
  // which bypasses the router, so the test selects each tab through the
  // shipped toggle at 1440px and resizes to 375px before any reload.
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.evaluate(() => { location.hash = '#tasks?status=backlog'; });
  await page.evaluate(async () => {
    const { setActiveTab } = await import('/js/router.js');
    setActiveTab(location.hash, { refresh: false });
  });
  await page.locator('#tasks-main > .col-tasks').waitFor({ state: 'visible' });
  const ordinaryDesktop = await page.evaluate(() => ({
    hash: location.hash,
    dockRoute: document.getElementById('tasks-main').classList.contains('dock-route'),
    listVisible: document.querySelector('#tasks-main > .col-tasks').getClientRects().length > 0,
    dockVisible: document.getElementById('side-dock').getClientRects().length > 0,
  }));
  if (ordinaryDesktop.hash.includes('dock=') || ordinaryDesktop.dockRoute
    || !ordinaryDesktop.listVisible || !ordinaryDesktop.dockVisible) {
    throw new Error(`Ordinary desktop Tasks list must show the list beside the dock with no dock route: ${JSON.stringify(ordinaryDesktop)}`);
  }
  const dockTabVisible = (mode) => page.evaluate((mode) => {
    const main = document.getElementById('tasks-main');
    const dock = document.getElementById('side-dock');
    const seg = document.querySelector('#dock-mode-toggle .dock-seg.on');
    return {
      hash: location.hash,
      dockRoute: main.classList.contains('dock-route'),
      listVisible: document.querySelector('#tasks-main > .col-tasks').getClientRects().length > 0,
      dockVisible: dock.getClientRects().length > 0,
      backVisible: document.getElementById('dock-back-tasks').getClientRects().length > 0,
      activeTab: seg ? seg.dataset.mode : null,
      card: dock.dataset.card,
    };
  }, mode);
  for (const mode of ['drain', 'locks', 'log']) {
    await page.locator(`#dock-mode-toggle .dock-seg[data-mode="${mode}"]`).click();
    const desktop = await dockTabVisible(mode);
    if (!desktop.hash.includes(`dock=${mode}`) || !desktop.dockRoute
      || !desktop.listVisible || !desktop.dockVisible || desktop.activeTab !== mode) {
      throw new Error(`Desktop ${mode} tab selection must keep the side-by-side dock with an explicit route: ${JSON.stringify(desktop)}`);
    }
    await page.setViewportSize({ width: 375, height: 812 });
    const phone = await dockTabVisible(mode);
    if (!phone.hash.includes(`dock=${mode}`) || !phone.dockRoute
      || phone.listVisible || !phone.dockVisible || !phone.backVisible || phone.activeTab !== mode) {
      throw new Error(`Desktop ${mode} selection resized to phone must show the dock and Back control: ${JSON.stringify(phone)}`);
    }
    await page.screenshot({ path: path.join(evidence, `tasks-dock-resize-${mode}-dark-375.png`), animations: 'disabled' });
    // Reloading the same URL routes through the router; it must show the
    // same dock route as the replaceState tab-selection path above.
    await page.evaluate(async () => {
      const { setActiveTab } = await import('/js/router.js');
      setActiveTab(location.hash, { refresh: false });
    });
    const rerouted = await dockTabVisible(mode);
    if (!rerouted.hash.includes(`dock=${mode}`) || !rerouted.dockRoute
      || rerouted.listVisible || !rerouted.dockVisible || rerouted.activeTab !== mode) {
      throw new Error(`Reloaded ${mode} route must match the resized tab selection: ${JSON.stringify(rerouted)}`);
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
  }
  // ORB-19783: a plain hash navigation to the list URL after a desktop dock
  // selection must show the list at phone width, as a reload of that URL does.
  // chooseDockMode rewrites the hash with replaceState, so the router's
  // last-routed address still names the list URL; the navigation must route
  // rather than be skipped as that address.
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.evaluate(() => { location.hash = '#jobs/live'; });
  await page.evaluate(() => { location.hash = '#tasks?status=backlog'; });
  await page.locator('#tasks-main > .col-tasks').waitFor({ state: 'visible' });
  await page.locator('#dock-mode-toggle .dock-seg[data-mode="log"]').click();
  await page.setViewportSize({ width: 375, height: 812 });
  const logSelected = await dockTabVisible('log');
  if (!logSelected.hash.includes('dock=log') || !logSelected.dockRoute || logSelected.listVisible || !logSelected.dockVisible) {
    throw new Error(`Desktop Log selection resized to phone must show the dock before the list navigation: ${JSON.stringify(logSelected)}`);
  }
  await page.evaluate(() => { location.hash = '#tasks?status=backlog'; });
  const listNavigated = await dockTabVisible('log');
  await page.screenshot({ path: path.join(evidence, 'tasks-list-hash-navigation-dark-375.png'), animations: 'disabled' });
  if (listNavigated.hash.includes('dock=') || listNavigated.dockRoute
    || !listNavigated.listVisible || listNavigated.dockVisible || listNavigated.backVisible) {
    throw new Error(`Hash navigation to the list URL after a desktop dock selection must show the list and hide the dock: ${JSON.stringify(listNavigated)}`);
  }
  await page.evaluate(async () => {
    const { setActiveTab } = await import('/js/router.js');
    setActiveTab(location.hash, { refresh: false });
  });
  const listReloaded = await dockTabVisible('log');
  if (JSON.stringify(listReloaded) !== JSON.stringify(listNavigated)) {
    throw new Error(`Hash navigation to the list URL must match a reload of it: ${JSON.stringify({ listNavigated, listReloaded })}`);
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  // Ordinary phone navigation still returns to the list, and the desktop
  // side-by-side dock remains usable afterwards.
  await page.setViewportSize({ width: 375, height: 812 });
  await page.evaluate(async () => {
    const { chooseDockMode } = await import('/js/log-tail.js');
    chooseDockMode('log');
  });
  await page.locator('#side-dock[data-mode="log"] #log-panel').waitFor({ state: 'visible' });
  await page.locator('#dock-back-tasks').click();
  await page.locator('#tasks-main > .col-tasks').waitFor({ state: 'visible' });
  const backAfterResize = await page.evaluate(() => ({
    hash: location.hash,
    dockRoute: document.getElementById('tasks-main').classList.contains('dock-route'),
    listVisible: document.querySelector('#tasks-main > .col-tasks').getClientRects().length > 0,
  }));
  if (backAfterResize.hash.includes('dock=') || backAfterResize.dockRoute || !backAfterResize.listVisible) {
    throw new Error(`Back after a resized dock route must return to the phone list: ${JSON.stringify(backAfterResize)}`);
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  const desktopAfterBack = await page.evaluate(() => ({
    listVisible: document.querySelector('#tasks-main > .col-tasks').getClientRects().length > 0,
    dockVisible: document.getElementById('side-dock').getClientRects().length > 0,
  }));
  if (!desktopAfterBack.listVisible || !desktopAfterBack.dockVisible) {
    throw new Error(`Desktop side-by-side dock must remain usable after phone navigation: ${JSON.stringify(desktopAfterBack)}`);
  }
  // Reproduce retained drain feedback changing the Locks first-card depth.
  // Exercise the shipped tabs and public renderers, never live drain actions.
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.evaluate(async () => {
    const { setWorkspace } = await import('/js/common.js');
    const { initOperations } = await import('/js/operations-shared.js');
    const { initLocks, paintLocks } = await import('/js/locks.js');
    const { renderTasks } = await import('/js/tasks.js');
    setWorkspace('fixture');
    initOperations({ getWorkspaces: () => [{ id: 'fixture', name: 'Fixture', status: 'active' }] });
    initLocks();
    const tasks = Array.from({ length: 24 }, (_, i) => ({
      id: `ORB-${i + 1}`, title: `Keep task ${i + 1} ready for delivery`, status: 'backlog',
      context_files: [`file:src/task-${i}.rs`, 'file:src/shared.rs'], tags: [],
    }));
    const context = {
      getTasks: () => tasks, getTasksMeta: () => null, getActiveStatuses: () => new Set(['backlog']),
      getSearchQuery: () => '', statusOrder: ['backlog'], defaultActiveStatuses: ['backlog'],
    };
    renderTasks(tasks, context);
    globalThis.dockLocksPayload = {
      total_locked: 25, total_tasks: 24,
      by_task: tasks.map(task => ({ ...task, status: 'in-progress' })), by_reservation: [],
    };
    paintLocks(globalThis.dockLocksPayload);
  });
  const settle = () => page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  });
  const boxes = () => page.evaluate(() => {
    const rect = selector => {
      const box = document.querySelector(selector).getBoundingClientRect();
      return { top: box.top, bottom: box.bottom, left: box.left, right: box.right };
    };
    const side = document.getElementById('side-dock');
    const mode = document.querySelector('#dock-mode-toggle .on').dataset.mode;
    const pane = document.querySelector(`#dock-pane-${mode === 'locks' ? 'drain' : mode}`);
    return {
      mode, tabs: rect('#dock-mode-toggle'), viewport: rect(`#${pane.id}`), list: rect('#tasks-body'),
      card: rect(mode === 'drain' ? '#auto-drain-panel' : mode === 'locks' ? '#locks-panel' : '#log-panel'),
      dock: rect('#side-dock'), lead: document.getElementById('tasks-dock').style.getPropertyValue('--tasks-dock-lead'),
      scroll: pane.scrollTop, listScroll: document.getElementById('tasks-body').scrollTop,
      feedbackShown: document.getElementById('auto-drain-operation-feedback').getClientRects().length > 0,
      overflow: document.documentElement.scrollWidth > innerWidth,
      selected: side.dataset.card,
    };
  });
  const measurements = [];
  const failures = [];
  const check = (condition, message) => { if (!condition) failures.push(message); };
  const select = async mode => {
    await page.locator(`#dock-tab-${mode}`).click();
    await settle();
    return boxes();
  };
  for (const height of [900, 420]) {
    await page.setViewportSize({ width: 1440, height });
    for (const phase of ['idle', 'draining']) {
      drainPhase = phase;
      await page.evaluate(async () => {
        const { fetchAndRenderAutoDrainPane } = await import('/js/operations-drain.js');
        await fetchAndRenderAutoDrainPane({ forecast: false });
      });
      for (const [kind, message] of [
        ['', ''], ['', 'Auto-drain Draining.'], ['pending', 'Starting a window…'],
        ['error', 'Window refused.\nWarning: no capacity available.\nRefresh failed: fixture unavailable.'],
        ['success', 'Window started.\nWarning: one task remains held.'],
      ]) {
        await page.evaluate(async ({ kind, message }) => {
          const { feedback } = await import('/js/operations-shared.js');
          feedback('auto-drain-operation-feedback', kind, message);
        }, { kind, message });
        const reference = await select('drain');
        for (const mode of ['locks', 'log', 'drain', 'locks', 'log', 'drain']) {
          const measured = await select(mode);
          measurements.push({ height, phase, kind, message, ...measured });
          for (const key of ['tabs', 'viewport', 'list', 'dock']) {
            check(Math.abs(measured[key].top - reference[key].top) <= 1,
              `${height}/${phase}/${kind}/${mode}: ${key} top moved ${reference[key].top} -> ${measured[key].top}`);
          }
          check(Math.abs(measured.dock.bottom - measured.list.bottom) <= 1, 'Dock/list bottom mismatch');
          check(Math.abs(measured.card.top + measured.scroll - measured.list.top) <= 1, 'First card/list alignment changed');
          check(!measured.overflow, 'Page overflow');
          if (mode === 'locks') check(!measured.feedbackShown, 'Drain feedback is visible above Locks held');
        }
        if (phase === 'draining' && kind === 'error') {
          const suffix = height === 900 ? '1440' : '1440-420';
          await select('locks');
          await page.screenshot({ path: path.join(evidence, `tasks-dock-locks-dark-${suffix}.png`), animations: 'disabled' });
          await select('log');
          await page.screenshot({ path: path.join(evidence, `tasks-dock-log-dark-${suffix}.png`), animations: 'disabled' });
          await select('drain');
          await page.locator('#dock-pane-drain').evaluate(node => { node.scrollTop = 0; });
          await page.screenshot({ path: path.join(evidence, `tasks-dock-drain-dark-${suffix}.png`), animations: 'disabled' });
          await page.locator('#auto-drain-operation-feedback').scrollIntoViewIfNeeded();
          check(await page.locator('#auto-drain-operation-feedback').textContent() === message, 'Drain feedback was lost');
          await page.screenshot({ path: path.join(evidence, `tasks-dock-drain-feedback-dark-${suffix}.png`), animations: 'disabled' });
        }
      }
    }
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await select('locks');
  await page.locator('#locks-view [data-view="task"]').click();
  check(await page.locator('#locks-count').textContent() === '25 files · 24 tasks', 'By-task counts changed');
  await page.locator('.lock-task-details summary').first().click();
  await page.evaluate(async () => {
    const { paintLocks } = await import('/js/locks.js');
    paintLocks(globalThis.dockLocksPayload);
  });
  check(await page.locator('.lock-task-details').first().getAttribute('open') !== null, 'Refresh closed lock expansion');
  await page.locator('#locks-view [data-view="task"]').focus();
  await page.keyboard.press('End');
  check(await page.locator('#locks-view [data-view="file"]').getAttribute('aria-selected') === 'true', 'Lock keyboard tabs failed');
  check(await page.locator('.lock-shared').count() === 1, 'Shared-lock warning missing');
  check(await page.locator('#dock-log-state').textContent() === '1 warning', 'Log warning badge missing');
  await page.locator('#locks-filter').fill('ORB-1');
  await page.waitForFunction(() => document.getElementById('locks-filter-note').textContent.startsWith('1 of 25'));
  await page.locator('#locks-filter').press('Escape');
  await page.waitForFunction(() => document.querySelectorAll('#locks-body .lock-group').length === 25);
  await page.locator('#dock-tab-locks').focus();
  await page.keyboard.press('End');
  check(await page.locator('#dock-tab-log').getAttribute('aria-selected') === 'true', 'Dock End key failed');
  await page.keyboard.press('Home');
  check(await page.locator('#dock-tab-drain').getAttribute('aria-selected') === 'true', 'Dock Home key failed');
  check(await page.evaluate(() => JSON.parse(localStorage.getItem('orbit.dashboard.logPanel')).dockMode) === 'drain', 'Dock preference not saved');

  // Scroll each bounded region and refresh while the dock is scrolled. The
  // chrome stays fixed; the list does not follow the dock's scroll position.
  await select('locks');
  await page.evaluate(() => {
    document.getElementById('dock-pane-drain').scrollTop = 200;
    document.getElementById('tasks-body').scrollTop = 150;
  });
  await settle();
  const scrolled = await boxes();
  await page.evaluate(async () => {
    const { paintLocks } = await import('/js/locks.js');
    const { fitLogPanelToViewport } = await import('/js/log-tail.js');
    paintLocks(globalThis.dockLocksPayload);
    fitLogPanelToViewport();
  });
  await settle();
  const refreshed = await boxes();
  check(scrolled.scroll > 0 && refreshed.scroll === scrolled.scroll, 'Refresh jumped dock scroll');
  check(scrolled.listScroll > 0 && refreshed.listScroll === scrolled.listScroll, 'Dock refresh moved list scroll');
  check(Math.abs(refreshed.tabs.top - scrolled.tabs.top) <= 1, 'Scrolled refresh moved tabs');
  measurements.push({ scenario: 'scrolled-refresh', scrolled, refreshed });
  for (const mode of ['log', 'drain', 'locks']) {
    const switched = await select(mode);
    check(Math.abs(switched.tabs.top - refreshed.tabs.top) <= 1, 'Scrolled tab switch moved tabs');
    check(switched.listScroll === refreshed.listScroll, 'Dock tab switch moved list scroll');
    measurements.push({ scenario: 'scrolled-switch', ...switched });
  }
  await page.evaluate(async () => {
    const { paintLocks } = await import('/js/locks.js');
    paintLocks({ total_locked: 0, total_tasks: 0, by_task: [], by_reservation: [] });
  });
  await settle();
  const emptyLocks = await boxes();
  check(Math.abs(emptyLocks.tabs.top - refreshed.tabs.top) <= 1, 'Empty lock results moved tabs');
  measurements.push({ scenario: 'empty-locks', ...emptyLocks });
  await page.evaluate(async () => {
    const { paintLocks } = await import('/js/locks.js');
    paintLocks(globalThis.dockLocksPayload);
  });
  for (const height of [812, 420]) {
    await page.setViewportSize({ width: 375, height });
    for (const mode of ['drain', 'locks', 'log']) {
      const measured = await select(mode);
      check(!measured.overflow, `Phone ${mode} overflow at ${height}`);
      check(measured.tabs.top >= 0 && measured.tabs.bottom < height, `Phone tabs clipped at ${height}`);
      check(await page.locator('#dock-back-tasks').isVisible(), 'Phone Back inaccessible');
      if (mode === 'locks') {
        await page.locator('#locks-filter').scrollIntoViewIfNeeded();
        const control = await page.locator('#locks-filter').boundingBox();
        check(control.y >= 0 && control.y + control.height <= height, 'Phone lock filter clipped');
      }
      if (mode === 'drain') {
        for (const selector of ['#auto-drain-quick-start .drain-start', '#auto-drain-operation-feedback']) {
          await page.locator(selector).scrollIntoViewIfNeeded();
          const control = await page.locator(selector).boundingBox();
          check(control.y >= measured.viewport.top && control.y + control.height <= measured.viewport.bottom + 1,
            `Phone drain control unreachable: ${selector}`);
        }
      }
      measurements.push({ width: 375, height, ...measured });
      await page.screenshot({ path: path.join(evidence, `tasks-dock-${mode}-dark-375-${height}.png`), animations: 'disabled' });
    }
  }
  // A fresh document must restore both the explicit URL and the saved tab.
  await select('locks');
  await page.reload();
  await page.addScriptTag({ type: 'module', content: fixtureBootstrap });
  await page.waitForFunction(() => globalThis.phoneDockFixtureReady);
  check(await page.locator('#dock-tab-locks').getAttribute('aria-selected') === 'true', 'Deep-linked Locks reload failed');
  await page.evaluate(() => { location.hash = '#tasks'; });
  await page.locator('#tasks-main > .col-tasks').waitFor({ state: 'visible' });
  await page.setViewportSize({ width: 1440, height: 900 });
  check(await page.locator('#dock-tab-locks').getAttribute('aria-selected') === 'true', 'Saved dock tab lost on plain Tasks route');
  // ORB-20461: keyboard focus must survive dock-warning repaints. Exercise the
  // shipped warning-list module on an isolated page: focus and expand a row,
  // repaint with identical entries (as every log filter pass does), repaint
  // with a changed list while the Log action holds focus, then remove the
  // focused warning and empty the list. Focus must follow the surviving row
  // or action, land on a nearby row when its warning is gone, and fall back
  // to the selected dock tab when nothing remains; expansion follows too.
  const focusPage = await browser.newPage({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
  try {
    focusPage.on('pageerror', (error) => pageErrors.push(String(error)));
    await focusPage.goto(`http://127.0.0.1:${server.address().port}/#tasks?dock=log`);
    // The browser answers focus and layout on the active tab; make this page
    // that tab before driving keyboard focus through the module.
    await focusPage.bringToFront();
    // Light the Tasks pane the way the tab switcher does, without booting the
    // dashboard: the regression needs the shipped warning-list module
    // rendered, not the router or the log stream (which would repaint
    // mid-assertion and race it).
    await focusPage.evaluate(() => {
      document.querySelector('.tab-pane[data-tab="tasks"]')?.classList.add('active');
    });
    const warningFocus = await focusPage.evaluate(async () => {
      const outcomes = [];
      const { setLogWarnings } = await import('/js/dock-warnings.js');
      const box = document.getElementById('dock-warnings');
      const selectedTab = () => document.querySelector('#dock-mode-toggle .dock-seg.on');
      const warning = (message) => ({
        ts: '2026-10-29T12:00:00Z', source: 'Log', message, reveal: () => {},
      });
      const activeLabel = () => {
        const active = document.activeElement;
        return `${active?.tagName}.${active?.className}`;
      };
      setLogWarnings([warning('Focus must survive repaint')]);
      const row = box.querySelector('.dock-warning');
      if (!row) {
        outcomes.push('warning row did not render');
        return outcomes;
      }
      row.focus();
      row.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      if (document.activeElement !== row || !row.classList.contains('expanded')) {
        outcomes.push(`row did not take focus and expand before the repaint: ${activeLabel()}`);
      }
      // Unchanged repaint, as log-tail.js issues on every filter pass.
      setLogWarnings([warning('Focus must survive repaint')]);
      const repainted = box.querySelector('.dock-warning');
      if (document.activeElement !== repainted) {
        outcomes.push(`unchanged repaint dropped row focus to ${activeLabel()}`);
      }
      if (!repainted?.classList.contains('expanded')) outcomes.push('unchanged repaint lost the expansion');
      // Changed repaint while the Log action holds focus.
      repainted.querySelector('button.dock-warning-source')?.focus();
      setLogWarnings([warning('Newer warning arrives'), warning('Focus must survive repaint')]);
      const targetAction = [...box.querySelectorAll('.dock-warning')]
        .find((node) => node.title === 'Focus must survive repaint')
        ?.querySelector('button.dock-warning-source');
      if (document.activeElement !== targetAction) {
        outcomes.push(`changed repaint dropped action focus to ${activeLabel()}`);
      }
      // Removing the focused warning lands on the row that took its place.
      targetAction?.focus();
      setLogWarnings([warning('Newer warning arrives')]);
      if (document.activeElement?.title !== 'Newer warning arrives') {
        outcomes.push(`removed warning left focus at ${activeLabel()} instead of a nearby row`);
      }
      // Emptying the list hides the box and falls back to the selected dock tab.
      document.activeElement?.focus();
      setLogWarnings([]);
      if (document.activeElement !== selectedTab() || !box.hidden) {
        outcomes.push(`emptied list left focus at ${activeLabel()} instead of the selected dock tab`);
      }
      // Leave one row visible for the evidence capture below.
      setLogWarnings([warning('Focus must survive repaint')]);
      return outcomes;
    });
    for (const outcome of warningFocus) failures.push(`[ORB-20461] ${outcome}`);
    const focusDebug = await focusPage.evaluate(() => {
      const box = document.getElementById('dock-warnings');
      const rect = box?.getBoundingClientRect();
      return {
        hidden: box?.hidden,
        rect: rect && { w: Math.round(rect.width), h: Math.round(rect.height) },
        rows: box?.querySelectorAll('.dock-warning').length,
        active: `${document.activeElement?.tagName}.${document.activeElement?.className}`,
      };
    });
    console.log(`ORB-20461 focus outcomes: ${JSON.stringify(warningFocus)} state: ${JSON.stringify(focusDebug)}`);
    await focusPage.locator('#dock-warnings').screenshot({ path: path.join(evidence, 'tasks-dock-warnings-focus-dark-1440.png'), animations: 'disabled' });
  } finally {
    await focusPage.close();
  }
  fs.writeFileSync(path.join(evidence, 'dock-alignment.json'), JSON.stringify({ measurements, failures, pageErrors }, null, 2));
  if (failures.length) throw new Error(`Dock regression (${failures.length} failures): ${failures.slice(0, 10).join(' | ')}`);
  if (pageErrors.length) throw new Error(`Browser page errors: ${pageErrors.join(' | ')}`);
  console.log(`PASS: stable dock tabs/viewport/list/card geometry at 1440x900 and 1440x420 with empty, state, pending, error and success feedback; lock counts, filter, expansion, warning badges, keyboard tabs, warning-row focus across log repaints with a removal fallback, scrolled refresh and persisted/deep-linked selection; 375px normal/short dock controls, resize routing and Back with retained status/search filters. Evidence: ${evidence}`);
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
