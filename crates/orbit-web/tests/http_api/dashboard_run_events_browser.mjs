// node dashboard_run_events_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/run-events-browser
// Runs the shipped app, router, renderers and CSS against a delayed HTTP fixture.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const run = {
  run_id: 'jrun-events-fixture', job_id: 'fixture', state: 'cancelled',
  started_at: '2026-10-10T06:56:40Z', finished_at: '2026-10-10T07:00:49Z',
};
const events = Array.from({ length: 250 }, (_, index) => ({
  event_id: `event-${index}`,
  ts: new Date(Date.parse('2026-10-10T06:56:40Z') + index * 1000).toISOString(),
  step_id: 'fixture', attempt: index, next_backoff_ms: 1000,
  body_kind: [40, 160].includes(index) ? 'step_retry' : index === 249 ? 'run_finished' : index === 248 ? 'run_cancelled' : 'activity_started',
  event_type: 'run', agent_identity: 'fixture',
}));
const list = items => ({ items, total: items.length, limit: 100, truncated: false });
let releaseDetail;
let delayDetail = true;
let failDetail = false;
let detailRequestCount = 0;
let notifySecondDetailRequest;
let detailReady;
function armDetailDelay() {
  delayDetail = true;
  detailReady = new Promise(resolve => { releaseDetail = resolve; });
}
armDetailDelay();
const requests = [];
const retryRequests = [];
let retryStatus = 200;
let retryEvents = events.filter(event => event.body_kind === 'step_retry');
let nextRetryDelay = null;
const pendingRetryReleases = [];
function holdNextRetry() {
  let notify;
  let release;
  const requested = new Promise(resolve => { notify = resolve; });
  const ready = new Promise(resolve => { release = resolve; });
  nextRetryDelay = { notify, ready };
  pendingRetryReleases.push(release);
  return { requested, release };
}
const steps = [{ step_index: 0, target_type: 'crew', target_id: 'fixture', state: 'success',
  exit_code: 0, duration_ms: 249000, started_at: run.started_at, finished_at: run.finished_at }];
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (!url.pathname.startsWith('/api/')) {
    const served = dashboardFile(url.pathname);
    if (!served) { res.writeHead(404); res.end(); return; }
    res.setHeader('content-type', served.type);
    res.end(served.data);
    return;
  }
  res.setHeader('content-type', 'application/json');
  let payload = list([]);
  switch (url.pathname) {
    case '/api/workspaces': payload = [{ id: 'ws_fixture', name: 'fixture', status: 'active', is_default: true }]; break;
    case '/api/crews': payload = { default_crew: null, crews: [] }; break;
    case '/api/diagnostics/friction': payload = []; break;
    case '/api/job-runs': payload = list([run]); break;
    case '/api/runs/jrun-other-fixture': payload = { run: { ...run, run_id: 'jrun-other-fixture' }, steps }; break;
    case '/api/runs/jrun-other-fixture/events':
      payload = url.searchParams.has('kind') ? [] : { events: [], total: 0, offset: 0 }; break;
    case '/api/runs/jrun-events-fixture':
      detailRequestCount += 1;
      notifySecondDetailRequest?.();
      if (delayDetail) await detailReady;
      if (failDetail) { res.writeHead(500); res.end(JSON.stringify({ error: 'fixture read failure' })); return; }
      payload = { run, steps };
      break;
    case '/api/runs/jrun-events-fixture/events': {
      if (url.searchParams.get('kind') === 'step_retry') {
        assert.equal(url.searchParams.has('tail'), false);
        assert.equal(Number(url.searchParams.get('offset')), 0);
        retryRequests.push({ workspace: url.searchParams.get('workspace'), limit: Number(url.searchParams.get('limit')) });
        const status = retryStatus;
        payload = url.searchParams.get('workspace') === 'ws_other'
          ? [] : retryEvents.slice(0, Number(url.searchParams.get('limit')));
        const delay = nextRetryDelay;
        nextRetryDelay = null;
        if (delay) { delay.notify(); await delay.ready; }
        if (status !== 200) {
          res.writeHead(status); res.end(JSON.stringify({ error: 'fixture bounded scan unavailable' })); return;
        }
        break;
      }
      assert.equal(url.searchParams.get('tail'), 'true');
      const offset = Number(url.searchParams.get('offset'));
      const limit = Number(url.searchParams.get('limit'));
      requests.push({ offset, limit });
      const end = Math.max(0, events.length - offset);
      payload = { events: events.slice(Math.max(0, end - limit), end), total: events.length, offset };
      break;
    }
    case '/api/runs/jrun-other-fixture/logs':
    case '/api/runs/jrun-events-fixture/logs': payload = []; break;
  }
  res.end(JSON.stringify(payload));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
async function checkpoint(promise, label) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Timed out: ${label}`)), 10000);
    })]);
  } finally { clearTimeout(timer); }
}
const results = { requests, retryRequests, widths: [], retryChecks: [] };
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
  const assertRetries = async (label, count = 2) => {
    await page.waitForFunction(count => document.querySelectorAll('.gantt-retry-marker').length === count, count);
    assert.equal(await page.locator('.gantt-retry-key').count(), count ? 1 : 0, `${label}: retry legend agrees with projection`);
    const markers = await page.locator('.gantt-retry-marker').evaluateAll(nodes => nodes.map(node => ({
      x: node.getAttribute('cx'), title: node.textContent,
    })));
    results.retryChecks.push({ label, count, markers });
    console.log(`Retry check: ${label} (${count})`);
    return markers;
  };
  const refresh = async () => {
    await page.getByRole('button', { name: 'Refresh', exact: true }).click();
    await page.waitForFunction(() => !document.querySelector('#meta-text')?.textContent.includes('fetching'));
  };
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture#diagnostics/runs`);
  await page.locator('#runs-body [data-key^="run-"]').first().waitFor();
  await page.evaluate(async () => (await import('/static/js/router.js')).navigateToRun('jrun-events-fixture'));
  await page.locator('#run-steps-body .skeleton-state').waitFor({ state: 'attached' });
  await page.waitForFunction(() => document.querySelectorAll('#run-events-body tbody tr').length === 100);
  assert.equal(await page.locator('#run-steps-body .empty-state').count(), 0, 'a settled logs/events response cannot turn an in-flight detail into empty steps');
  results.loading = await page.locator('#run-steps-body').textContent();
  await page.screenshot({ path: path.join(evidence, 'steps-loading-1440.png'), animations: 'disabled' });
  delayDetail = false;
  releaseDetail();
  const stepRow = page.locator('#run-steps-body .step-row');
  await stepRow.waitFor({ state: 'attached' });
  assert.match(await stepRow.textContent(), /crew:fixture/);
  await assertRetries('newest');
  await page.evaluate(async () => (await import('/static/js/router.js')).setRunDetailSubtab('events'));
  const rows = page.locator('#run-events-body tbody tr');
  assert.equal(await rows.count(), 100);
  assert.match(await page.locator('.run-events-pagination').textContent(), /151–250 of 250/);
  assert.match(await rows.nth(98).textContent(), /run_cancelled/);
  assert.match(await rows.last().textContent(), /run_finished/);
  const times = await rows.locator('td:first-child').allTextContents();
  assert.match(times[0], /2026-10-09 23:59:10 PDT/);
  assert.match(times[50], /2026-10-10 00:00:00 PDT/);
  assert.match(times[99], /^00:00:49 PDT$/);
  assert.equal(new Set(times).size, 100, 'each event second is distinguishable');
  results.times = { first: times[0], midnight: times[50], last: times[99] };
  for (const width of [1440, 600]) {
    await page.setViewportSize({ width, height: 900 });
    assert.equal(await page.evaluate(() => document.scrollingElement.scrollWidth > innerWidth), false, 'table overflow stays inside the panel');
    const controls = await page.locator('.run-events-pagination').boundingBox();
    assert.ok(controls.width <= width, 'paging controls fit the viewport');
    results.widths.push({ width, controls });
    await page.screenshot({ path: path.join(evidence, `events-${width}.png`), animations: 'disabled' });
  }
  await page.getByRole('button', { name: 'Load earlier', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#run-events-body tbody tr')?.dataset.key === 'runev-event-50');
  assert.match(await page.locator('.run-events-pagination').textContent(), /51–150 of 250/);
  const middleMarkers = await assertRetries('earlier');
  assert.deepEqual(middleMarkers.map(marker => marker.title), results.retryChecks[0].markers.map(marker => marker.title));
  const retryReadsBeforePage = retryRequests.length;
  await refresh();
  assert.match(await page.locator('.run-events-pagination').textContent(), /51–150 of 250/);
  assert.deepEqual(await assertRetries('refresh earlier'), middleMarkers);
  assert.ok(retryRequests.length > retryReadsBeforePage, 'Refresh updates the independent retry projection');
  await page.getByRole('button', { name: 'Load earlier', exact: true }).click();
  await page.waitForFunction(() => document.querySelectorAll('#run-events-body tbody tr').length === 50);
  assert.match(await page.locator('.run-events-pagination').textContent(), /1–50 of 250/);
  assert.deepEqual(await assertRetries('oldest'), middleMarkers);
  assert.equal(await page.getByRole('button', { name: 'Load earlier', exact: true }).isDisabled(), true);
  await page.getByRole('button', { name: 'Newest events', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#run-events-body tbody tr')?.dataset.key === 'runev-event-150');
  assert.deepEqual(await assertRetries('returned newest'), middleMarkers);
  const detailReadsBefore = detailRequestCount;
  armDetailDelay();
  const secondDetailRequest = new Promise(resolve => { notifySecondDetailRequest = resolve; });
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await checkpoint(secondDetailRequest, 'delayed detail request');
  assert.ok(detailRequestCount > detailReadsBefore);
  await assertRetries('refresh pending detail');
  assert.equal(await page.locator('#run-steps-body .skeleton-state').count(), 0, 'refresh keeps loaded steps visible');
  assert.equal(await page.locator('#run-steps-body .step-row').count(), 1, 'refresh preserves the loaded step row while detail is pending');
  delayDetail = false;
  releaseDetail();
  await page.waitForFunction(() => document.querySelector('#run-steps-body .step-row')?.textContent.includes('crew:fixture'));
  await page.waitForFunction(() => !document.querySelector('#meta-text')?.textContent.includes('fetching'));

  // Hold an old request until a run round trip has loaded a new empty projection.
  const runDelay = holdNextRetry();
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await checkpoint(runDelay.requested, 'held run retry read');
  await page.evaluate(async () => (await import('/static/js/router.js')).navigateToRun('jrun-other-fixture'));
  await assertRetries('run retirement', 0);
  await page.waitForFunction(() => document.querySelector('.gantt-bar') && !document.querySelector('.gantt-retry-status'));
  retryEvents = [];
  await page.evaluate(async () => (await import('/static/js/router.js')).navigateToRun('jrun-events-fixture'));
  await page.waitForFunction(() => document.querySelector('.gantt-bar') && !document.querySelector('.gantt-retry-status'));
  const runResponse = page.waitForResponse(response => response.url().includes('kind=step_retry'));
  runDelay.release();
  const staleRunResponse = await runResponse;
  assert.equal(staleRunResponse.status(), 200);
  assert.equal((await staleRunResponse.json()).length, 2, 'the stale response really carries the prior markers');
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await assertRetries('stale run response after round trip', 0);

  retryEvents = events.filter(event => event.body_kind === 'step_retry');
  await refresh();
  await assertRetries('run recovered');
  const workspaceDelay = holdNextRetry();
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await checkpoint(workspaceDelay.requested, 'held workspace retry read');
  await page.evaluate(async () => (await import('/static/js/common.js')).setWorkspace('ws_other'));
  await assertRetries('workspace retirement', 0);
  await page.evaluate(async () => (await import('/static/js/router.js')).navigateToRun('jrun-events-fixture'));
  await page.waitForFunction(() => document.querySelector('.gantt-bar') && !document.querySelector('.gantt-retry-status'));
  await assertRetries('other workspace loaded', 0);
  assert.ok(retryRequests.some(request => request.workspace === 'ws_other'), 'retry projection requests use the selected workspace');
  retryEvents = [];
  await page.evaluate(async () => (await import('/static/js/common.js')).setWorkspace('ws_fixture'));
  // Route navigation starts a new generation for the same run after the workspace round trip.
  await page.evaluate(async () => (await import('/static/js/router.js')).navigateToRun('jrun-events-fixture'));
  await page.waitForFunction(() => document.querySelector('.gantt-bar') && !document.querySelector('.gantt-retry-status'));
  const workspaceResponse = page.waitForResponse(response => response.url().includes('kind=step_retry'));
  workspaceDelay.release();
  const staleWorkspaceResponse = await workspaceResponse;
  assert.equal(staleWorkspaceResponse.status(), 200);
  assert.equal((await staleWorkspaceResponse.json()).length, 2, 'the stale workspace response really carries prior markers');
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await assertRetries('stale workspace response after round trip', 0);

  retryEvents = Array.from({ length: 101 }, (_, i) => ({ ...events[40], event_id: `retry-${i}`, attempt: i }));
  await refresh();
  await assertRetries('bounded projection', 100);
  assert.match(await page.locator('.gantt-retry-status').textContent(), /later retry markers are omitted/);
  retryStatus = 413;
  await refresh();
  await assertRetries('scan failure', 0);
  assert.match(await page.locator('.gantt-retry-status').textContent(), /Retry markers unavailable/);
  retryStatus = 200;
  retryEvents = events.filter(event => event.body_kind === 'step_retry');
  await refresh();
  await assertRetries('scan recovered');
  assert.equal(await page.locator('.gantt-retry-status').count(), 0);
  failDetail = true;
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#run-detail-meta').textContent.includes('Unable to load run'));
  await page.evaluate(async () => (await import('/static/js/router.js')).setRunDetailSubtab('steps'));
  assert.doesNotMatch(await page.locator('#run-steps-body').textContent(), /No steps recorded/);
  assert.deepEqual(errors, []);
  results.errors = errors;
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify(results, null, 2));
  console.log(JSON.stringify(results));
} finally {
  releaseDetail();
  for (const release of pendingRetryReleases) release();
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
