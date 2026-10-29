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
const server = http.createServer((req, res) => {
  const pathname = new URL(req.url, 'http://fixture').pathname;
  if (pathname === '/api/log') {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ events: [], offset: 0, agent_offset: 0 }));
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
  await page.addScriptTag({ type: 'module', content: `
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
  `});
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
  if (pageErrors.length) throw new Error(`Browser page errors: ${pageErrors.join(' | ')}`);
  console.log(`PASS: 375x812 phone dock Drain, Log, keyboard Back and retained status/search filters; desktop tab selection resized to phone for drain/locks/log with matching reload route; hash navigation to the list URL after a desktop Log selection shows the list. Screenshots: ${evidence}`);
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
