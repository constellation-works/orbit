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
const run = { run_id: 'jrun-events-fixture', job_id: 'fixture', state: 'cancelled' };
const events = Array.from({ length: 250 }, (_, index) => ({
  event_id: `event-${index}`,
  ts: new Date(Date.parse('2026-10-10T06:56:40Z') + index * 1000).toISOString(),
  body_kind: index === 249 ? 'run_finished' : index === 248 ? 'run_cancelled' : 'activity_started',
  event_type: 'run', agent_identity: 'fixture',
}));
const list = items => ({ items, total: items.length, limit: 100, truncated: false });
let releaseDetail;
let delayDetail = true;
let failDetail = false;
const detailReady = new Promise(resolve => { releaseDetail = resolve; });
const requests = [];
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
    case '/api/runs/jrun-events-fixture':
      if (delayDetail) await detailReady;
      if (failDetail) { res.writeHead(500); res.end(JSON.stringify({ error: 'fixture read failure' })); return; }
      payload = { run, steps: [] };
      break;
    case '/api/runs/jrun-events-fixture/events': {
      assert.equal(url.searchParams.get('tail'), 'true');
      const offset = Number(url.searchParams.get('offset'));
      const limit = Number(url.searchParams.get('limit'));
      requests.push({ offset, limit });
      const end = Math.max(0, events.length - offset);
      payload = { events: events.slice(Math.max(0, end - limit), end), total: events.length, offset };
      break;
    }
    case '/api/runs/jrun-events-fixture/logs': payload = []; break;
  }
  res.end(JSON.stringify(payload));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
const results = { requests, widths: [] };
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
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
  await page.locator('#run-steps-body .empty-state').waitFor({ state: 'attached' });
  assert.match(await page.locator('#run-steps-body').textContent(), /No steps recorded/);
  await page.evaluate(async () => (await import('/static/js/router.js')).setRunDetailSubtab('events'));
  const rows = page.locator('#run-events-body tbody tr');
  assert.equal(await rows.count(), 100);
  assert.match(await page.locator('.run-events-pagination').textContent(), /151–250 of 250/);
  assert.match(await rows.nth(98).textContent(), /run_cancelled/);
  assert.match(await rows.last().textContent(), /run_finished/);
  const times = await rows.locator('td:first-child').allTextContents();
  assert.match(times[0], /10\/9\/2026 23:59:10 PDT/);
  assert.match(times[50], /10\/10\/2026 00:00:00 PDT/);
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
  await page.getByRole('button', { name: 'Load earlier', exact: true }).click();
  await page.waitForFunction(() => document.querySelectorAll('#run-events-body tbody tr').length === 50);
  assert.match(await page.locator('.run-events-pagination').textContent(), /1–50 of 250/);
  assert.equal(await page.getByRole('button', { name: 'Load earlier', exact: true }).isDisabled(), true);
  await page.getByRole('button', { name: 'Newest events', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#run-events-body tbody tr')?.dataset.key === 'runev-event-150');
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
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
