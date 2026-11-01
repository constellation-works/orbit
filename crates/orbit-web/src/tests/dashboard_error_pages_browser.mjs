// Health › Incidents › Errors paging (ORB-20742).
//
// A fixture server feeds a populated 24h window (48 ordinary errors plus two
// recoverable agent diagnostics, and a larger page after Load more). Chromium
// checks that the first paint mounts a bounded page, that Previous and Next
// are keyboard buttons reaching the last ordinary error, and that the window,
// agent grouping, run links and fetch-more control stay usable.
//
// Usage:
//   node dashboard_error_pages_browser.mjs <playwright-module> <evidence-dir> [--before]
//
// `--before` records the unbounded baseline and does not require a pager.
import { pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
const before = process.argv.includes('--before');
const shots = path.join(evidence, 'screenshots');
fs.mkdirSync(shots, { recursive: true });

const PHONE = 4;
const DESKTOP = 8;
let feed = 'full';
const requests = [];

function stamp(minutesAgo) {
  return new Date(Date.now() - minutesAgo * 60 * 1000).toISOString();
}

function ordinaryRow(index, prefix, runPrefix) {
  return {
    ts: stamp(index + 1),
    source: 'process',
    job_run: `${runPrefix}${index}`,
    message: `${prefix} error ${index}: step failed while publishing the dashboard bundle to the workspace mirror`,
    step: 'publish',
    target: 'orbit-web',
    provider: 'codex',
    workspace_id: 'ws_orbit',
    recovered: false,
  };
}

function agentRows() {
  return [
    {
      ts: stamp(0),
      source: 'agent-stderr',
      target: 'codex_core::tools::router',
      message: 'apply_patch verification failed',
      job_run: 'jrun-agent-patch',
      step: 'apply',
      workspace_id: 'ws_orbit',
    },
    {
      ts: stamp(0),
      source: 'agent-stderr',
      target: 'codex_models_manager::manager',
      message: 'request timed out',
      job_run: 'jrun-agent-model',
      step: 'model',
      workspace_id: 'ws_orbit',
    },
  ];
}

function errorsPayload(url) {
  const sinceWindow = url.searchParams.get('since') || '24h';
  const limit = Number.parseInt(url.searchParams.get('limit') || '50', 10);
  const week = sinceWindow === '7d';
  const ordinaryCount = feed === 'short' ? 2 : week ? 10 : limit >= 100 ? 60 : 48;
  const prefix = week ? 'seven-day' : 'ordinary';
  const runPrefix = week ? 'jrun-7d-' : 'jrun-error-';
  const items = [
    ...Array.from({ length: ordinaryCount }, (_, index) => ordinaryRow(index, prefix, runPrefix)),
    ...agentRows(),
  ];
  const span = week ? 7 * 24 * 60 : 24 * 60;
  const since = stamp(span);
  // 24h coverage starts after the window, so Load more stays available.
  // 7d coverage matches the window, so that control hides.
  const coverage = week ? since : stamp(120);
  return { items, since, coverage_since: coverage };
}

function incidentsPayload(url) {
  const since = url.searchParams.get('since') || '24h';
  return {
    window: since,
    class: url.searchParams.get('class') || 'unexpected',
    incident_count: 0,
    shown_incident_count: 0,
    matching_incident_count: 0,
    incidents: [],
  };
}

const HOSTS = {
  hosts: [{
    name: 'local', machine_id: 'hm_local', local: true, reachable: true,
    legacy: false, ssh: null, workspaces: [],
  }],
};
const WORKSPACES = [{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true }];

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (!url.pathname.startsWith('/api/')) {
    const served = dashboardFile(url.pathname);
    if (!served) {
      res.writeHead(404);
      res.end();
      return;
    }
    res.setHeader('content-type', served.type);
    res.end(served.data);
    return;
  }
  requests.push(`${url.pathname}${url.search}`);
  if (url.pathname === '/api/events/stream' || url.pathname === '/api/log/stream') {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    res.write(': open\n\n');
    return;
  }
  const json = (body) => {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(body));
  };
  if (url.pathname === '/api/hosts' || url.pathname.startsWith('/api/hosts/')) return json(HOSTS);
  if (url.pathname === '/api/workspaces') return json(WORKSPACES);
  if (url.pathname === '/api/log') return json({ events: [], offset: 0, agent_offset: 0 });
  if (url.pathname === '/api/diagnostics/errors') return json(errorsPayload(url));
  if (url.pathname === '/api/audit/incidents') return json(incidentsPayload(url));
  json({});
});
const sockets = new Set();
server.on('connection', (socket) => {
  sockets.add(socket);
  socket.on('close', () => sockets.delete(socket));
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

const assert = (condition, message) => {
  if (!condition) throw new Error(message);
};
const until = async (label, predicate, timeout = 10000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    if (await predicate()) return;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
};

const pageErrors = [];
const browser = await chromium.launch({ headless: true });
const context = await browser.newContext({ colorScheme: 'dark', viewport: { width: 1440, height: 900 } });
const page = await context.newPage();
page.on('pageerror', (error) => pageErrors.push(String(error)));

const ERRORS_URL = `${origin}/?workspace=ws_orbit#diagnostics/incidents/errors`;

async function openErrors(width) {
  await page.setViewportSize({ width, height: 900 });
  await page.goto(ERRORS_URL, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('.diagnostics-errors-main tbody tr', { timeout: 20000 });
}

async function measure() {
  return page.evaluate(() => {
    const body = document.getElementById('diag-body');
    const pane = document.querySelector('.tab-pane[data-tab="diagnostics"]');
    const panel = document.getElementById('diagnostics-panel');
    const mainRows = [...document.querySelectorAll('.diagnostics-errors-main tbody tr')];
    const pager = document.querySelector('.diagnostics-errors-pager');
    const pagerBox = pager ? pager.getBoundingClientRect() : null;
    const paneBox = pane ? pane.getBoundingClientRect() : null;
    const runs = mainRows.map((row) => row.querySelector('a')?.textContent || '');
    return {
      bodyHeight: body ? body.scrollHeight : 0,
      paneScroll: pane ? pane.scrollHeight : 0,
      paneClient: pane ? pane.clientHeight : 0,
      rowCount: mainRows.length,
      runs,
      pager: !!pager,
      prevDisabled: document.querySelector('[data-page="previous"]')?.disabled ?? null,
      nextDisabled: document.querySelector('[data-page="next"]')?.disabled ?? null,
      buttonType: document.querySelector('.diagnostics-errors-page-button')?.getAttribute('type') || '',
      status: document.querySelector('.diagnostics-errors-page-status')?.textContent || '',
      count: document.getElementById('diag-count')?.textContent || '',
      agentOpen: document.querySelector('details.agent-diagnostics')?.open ?? null,
      agentRuns: [...document.querySelectorAll('details.agent-diagnostics tbody tr a')].map((node) => node.textContent),
      loadMore: !!document.querySelector('.diagnostics-load-more'),
      docOverflow: document.documentElement.scrollWidth - window.innerWidth,
      paneOverflow: pane ? pane.scrollWidth - pane.clientWidth : 0,
      panelOverflow: panel ? panel.scrollWidth - panel.clientWidth : 0,
      pagerInside: !!(pagerBox && paneBox && pagerBox.left >= paneBox.left - 1 && pagerBox.right <= paneBox.right + 1),
    };
  });
}

async function shot(name) {
  const pager = page.locator('.diagnostics-errors-pager');
  if (await pager.count()) {
    await pager.evaluate((node) => node.scrollIntoView({ block: 'start' }));
  } else {
    await page.locator('#diag-body').evaluate((node) => node.scrollIntoView({ block: 'start' }));
  }
  await page.waitForTimeout(100);
  await page.screenshot({ path: path.join(shots, name), type: 'jpeg', quality: 65, fullPage: false });
}

const pageSizeFor = (width) => (width <= 600 ? PHONE : DESKTOP);
const result = { before, views: [] };

function pageLabel(width) {
  return width <= 600 ? 'phone' : 'desktop';
}

async function assertBounded(width, { ordinary, fetched, windowLabel, firstRun }) {
  const size = pageSizeFor(width);
  const view = await measure();
  result.views.push({ width, ...view, runs: view.runs.slice(0, 3) });
  const where = `${pageLabel(width)} ${width}px`;
  assert(view.rowCount > 0 && view.rowCount <= size,
    `ORB-20742: ${where} mounted ${view.rowCount} ordinary errors; the first page must stay within ${size}`);
  assert(view.pager, `ORB-20742: ${where} has no way to reach errors past the first page`);
  assert(view.buttonType === 'button', `ORB-20742: ${where} page control is not a button`);
  assert(view.prevDisabled === true, `ORB-20742: ${where} Previous is enabled on the first page`);
  assert(view.nextDisabled === false, `ORB-20742: ${where} Next is disabled while later errors remain`);
  assert(view.runs[0] === firstRun, `ORB-20742: ${where} first error is ${view.runs[0]}, expected ${firstRun}`);
  assert(view.count.includes(`of ${ordinary}`), `ORB-20742: ${where} count does not name the available ordinary errors (${view.count})`);
  assert(view.count.includes(`${fetched} error events`), `ORB-20742: ${where} count does not name the fetched events (${view.count})`);
  assert(view.count.includes(`window ${windowLabel}`), `ORB-20742: ${where} count lost the selected window (${view.count})`);
  assert(view.status.includes(`of ${ordinary}`), `ORB-20742: ${where} page status lost the available count (${view.status})`);
  assert(view.agentOpen === false, `ORB-20742: ${where} agent diagnostics opened themselves`);
  assert(view.agentRuns.length === 2, `ORB-20742: ${where} agent diagnostics hold ${view.agentRuns.length} rows`);
  assert(!view.runs.includes('jrun-agent-patch') && !view.runs.includes('jrun-agent-model'),
    `ORB-20742: ${where} mixed agent diagnostics into the ordinary page`);
  const ratio = view.paneClient ? view.paneScroll / view.paneClient : Infinity;
  assert(ratio < 3.5, `ORB-20742: ${where} Errors pane is ${view.paneScroll}px in a ${view.paneClient}px pane (${ratio.toFixed(1)} heights)`);
  assert(view.docOverflow <= 1, `ORB-20742: ${where} page overflows by ${view.docOverflow}px`);
  assert(view.paneOverflow <= 1, `ORB-20742: ${where} pane overflows by ${view.paneOverflow}px`);
  assert(view.panelOverflow <= 1, `ORB-20742: ${where} panel overflows by ${view.panelOverflow}px`);
  assert(view.pagerInside, `ORB-20742: ${where} page control sits outside the Errors pane`);
  assert(view.loadMore === (windowLabel !== '7d'), `ORB-20742: ${where} Load more visibility does not match coverage`);
}

async function walkToEnd(width, lastRun) {
  const size = pageSizeFor(width);
  let steps = 0;
  for (;;) {
    const view = await measure();
    assert(view.rowCount > 0 && view.rowCount <= size,
      `ORB-20742: ${pageLabel(width)} page ${steps} mounted ${view.rowCount} rows, above ${size}`);
    if (view.nextDisabled) {
      assert(view.runs.at(-1) === lastRun, `ORB-20742: last page ends on ${view.runs.at(-1)}, expected ${lastRun}`);
      assert(view.prevDisabled === false, 'ORB-20742: Previous is disabled on the last page');
      const focused = await page.evaluate(() => document.activeElement?.dataset?.page || '');
      assert(focused === 'previous', `ORB-20742: focus left the page controls on the last page (${focused})`);
      return;
    }
    const next = page.locator('.diagnostics-errors-page-button[data-page="next"]');
    await next.focus();
    await page.keyboard.press(steps % 2 === 0 ? 'Enter' : 'Space');
    steps += 1;
    assert(steps < 40, 'ORB-20742: Next never reached the last ordinary error');
    await until('page advance', async () => (await measure()).runs[0] !== view.runs[0]);
    await page.waitForFunction(() => {
      const name = document.activeElement?.dataset?.page;
      return name === 'next' || name === 'previous';
    });
  }
}

try {
  await openErrors(1440);
  if (before) {
    result.views.push({ width: 1440, ...(await measure()) });
    await shot('health-errors-before-dark-1440.jpg');
    await page.setViewportSize({ width: 375, height: 900 });
    await page.waitForTimeout(200);
    result.views.push({ width: 375, ...(await measure()) });
    await shot('health-errors-before-dark-375.jpg');
  } else {
    await assertBounded(1440, { ordinary: 48, fetched: 50, windowLabel: '24h', firstRun: 'jrun-error-0' });
    await shot('health-errors-dark-1440.jpg');
    await walkToEnd(1440, 'jrun-error-47');
    await shot('health-errors-later-dark-1440.jpg');

    const beforePrevious = (await measure()).runs[0];
    await page.locator('.diagnostics-errors-page-button[data-page="previous"]').focus();
    await page.keyboard.press('Enter');
    await until('previous page', async () => (await measure()).runs[0] !== beforePrevious);

    await page.locator('details.agent-diagnostics > summary').click();
    await until('agent rows', async () => (await measure()).agentOpen === true);
    const opened = await measure();
    assert(opened.agentRuns.includes('jrun-agent-patch') && opened.agentRuns.includes('jrun-agent-model'),
      `ORB-20742: opened agent diagnostics are ${opened.agentRuns.join(', ')}`);

    await page.locator('.diagnostics-errors-main tbody tr a').first().click();
    await until('run link', () => page.evaluate(() => window.location.hash.includes('runs/jrun-error-40') || window.location.hash.includes('runs/')));
    const runHash = await page.evaluate(() => window.location.hash);
    assert(runHash.includes('runs/'), `ORB-20742: run link stayed on ${runHash}`);

    await openErrors(1440);
    const marked = requests.length;
    await page.locator('.diagnostics-load-more').click();
    await until('wider fetch', () => requests.slice(marked).some((entry) => entry.includes('/api/diagnostics/errors') && entry.includes('limit=100')));
    await until('wider count', async () => (await measure()).count.includes('of 60'));
    const wider = await measure();
    assert(wider.rowCount > 0 && wider.rowCount <= DESKTOP, `ORB-20742: after Load more the page mounted ${wider.rowCount} rows`);
    assert(wider.count.includes('62 error events'), `ORB-20742: Load more count is ${wider.count}`);

    await page.locator('#incidents-views [data-view="incidents"]').click();
    await page.waitForSelector('.incident-windows button[data-window="7d"]', { timeout: 10000 });
    const incidentMarked = requests.length;
    await page.locator('.incident-windows button[data-window="7d"]').click();
    await page.locator('#incidents-views [data-view="errors"]').click();
    await until('7d errors', () => requests.slice(incidentMarked).some((entry) => entry.includes('/api/diagnostics/errors') && entry.includes('since=7d')));
    await until('7d count', async () => (await measure()).count.includes('window 7d'));
    await assertBounded(1440, { ordinary: 10, fetched: 12, windowLabel: '7d', firstRun: 'jrun-7d-0' });

    await openErrors(375);
    await assertBounded(375, { ordinary: 48, fetched: 50, windowLabel: '24h', firstRun: 'jrun-error-0' });
    await shot('health-errors-dark-375.jpg');
    await walkToEnd(375, 'jrun-error-47');
    await shot('health-errors-later-dark-375.jpg');

    feed = 'short';
    await openErrors(1440);
    const short = await measure();
    result.views.push({ width: 1440, short: true, rowCount: short.rowCount, count: short.count, pager: short.pager });
    assert(short.pager === false, 'ORB-20742: a two-error list rendered a pager');
    assert(short.rowCount === 2, `ORB-20742: a two-error list mounted ${short.rowCount} ordinary rows`);
    assert(short.count.startsWith('4 error events · window 24h'), `ORB-20742: unpaged count changed (${short.count})`);
    assert(!short.count.includes('Showing'), `ORB-20742: unpaged count uses a page range (${short.count})`);
    assert(short.agentRuns.length === 2, 'ORB-20742: the short list dropped agent diagnostics');
    assert(pageErrors.length === 0, `ORB-20742: page errors: ${pageErrors.join(' | ')}`);
  }
  fs.writeFileSync(path.join(evidence, 'error-pages-result.json'), JSON.stringify(result, null, 2));
} catch (error) {
  result.error = String(error);
  result.pageErrors = pageErrors;
  fs.writeFileSync(path.join(evidence, 'error-pages-result.json'), JSON.stringify(result, null, 2));
  throw error;
} finally {
  await browser.close();
  server.close();
  for (const socket of sockets) socket.destroy();
}
