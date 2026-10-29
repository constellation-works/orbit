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
  if (pageErrors.length) throw new Error(`Browser page errors: ${pageErrors.join(' | ')}`);
  console.log(`PASS: 375x812 phone dock Drain, Log, keyboard Back and retained status/search filters. Screenshots: ${evidence}`);
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
