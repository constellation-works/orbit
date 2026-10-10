// Usage: node dashboard_durations_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/durations-browser
// Exercise duration output through the shipped app, router, renderers and CSS.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const now = Date.parse('2026-10-08T08:00:00Z');
const ago = ms => new Date(now - ms).toISOString();
const actors = Array.from({ length: 55 }, (_, index) => ({
  actor: `${['codex/gpt-6.1-sol', 'claude/claude-sonnet-4.6', 'gemini/gemini-2.5-pro'][index % 3]}-${index + 1}`,
  n: 100 + index, avg: 1272000 + index * 1000,
  p50: 303000 + index * 1000, p95: 4920000 + index * 60000,
}));
const bands = ['low', 'medium', 'hard', 'expert', 'unset'].map((complexity, index) => ({
  complexity, n: 1100, actors: actors.slice(index * 11, (index + 1) * 11),
}));
const runningRun = { run_id: 'jrun-duration-fixture', job_id: 'fixture', state: 'running', started_at: ago(125000), duration_ms: null };
const steps = [
  { state: 'running', started_at: ago(61000), duration_ms: null },
  { state: 'running', started_at: ago(30000), duration_ms: 0 },
  { state: 'success', started_at: ago(360000), duration_ms: 303000 },
  { state: 'pending', duration_ms: null },
].map((step, step_index) => ({ step_index, target_type: 'activity', target_id: `fixture-${step_index}`, ...step }));
let automationDuration = 13191607;
const list = items => ({ items, total: items.length, limit: 100, truncated: false });
function payloadFor(url) {
  switch (url.pathname) {
    case '/api/workspaces': return [{ id: 'ws_fixture', name: 'fixture', status: 'active', is_default: true }];
    case '/api/crews': return { default_crew: null, crews: [] };
    case '/api/workflows/auto/readiness': return { capacity: {}, tasks: [] };
    case '/api/diagnostics/implement_one': return { implement_one_by_complexity: bands };
    case '/api/diagnostics/metrics': return [];
    case '/api/tasks/completion-by-complexity': return { by_complexity: [] };
    case '/api/job-runs': return list([runningRun]);
    case '/api/runs/jrun-duration-fixture': return { run: runningRun, steps };
    case '/api/runs/jrun-duration-fixture/events':
    case '/api/runs/jrun-duration-fixture/logs': return [];
    case '/api/routines': return {
      routines: [{ name: 'Duration fixture', source: 'fixture', target: 'job:fixture', enabled: true, cron: '0 * * * *',
        last_fire: { state: 'success', started_at: ago(automationDuration), finished_at: ago(0), duration_ms: automationDuration } }],
      clock: {},
    };
    default: return list([]);
  }
}
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (url.pathname.startsWith('/api/')) {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify(payloadFor(url)));
    return;
  }
  const served = dashboardFile(url.pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  res.setHeader('content-type', served.type);
  res.end(served.data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}/?workspace=ws_fixture`;
const measurements = { metrics: [], automation: [], runDetail: [] };
let browser;
let page;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto(`${origin}#diagnostics/metrics`);
  await page.waitForFunction(() => document.querySelectorAll('#diag-implement-one-body tbody tr').length === 55);
  await page.evaluate(async () => { await document.fonts.ready; });
  for (const width of [1440, 1024]) {
    await page.setViewportSize({ width, height: 1100 });
    const cells = await page.locator('#diag-implement-one-body tbody td.num').evaluateAll(nodes => nodes.map(node => {
      const range = document.createRange();
      range.selectNodeContents(node);
      return { text: node.textContent, rects: range.getClientRects().length, whiteSpace: getComputedStyle(node).whiteSpace };
    }));
    assert.equal(cells.length, 220, '55 actor rows include counts and 165 durations');
    for (const cell of cells) assert.equal(cell.rects, 1, `numeric/duration text stays on one line at ${width}: ${JSON.stringify(cell)}`);
    assert.equal(await page.evaluate(() => document.scrollingElement.scrollWidth > innerWidth), false, 'table scrolling stays inside the card');
    measurements.metrics.push({ width, durationCells: 165, cells });
    await page.screenshot({ path: path.join(evidence, `metrics-${width}.png`), fullPage: true, animations: 'disabled' });
    await page.locator('#diag-implement-one-body .card-body').evaluateAll(nodes => {
      for (const node of nodes) node.scrollLeft = node.scrollWidth;
    });
    await page.screenshot({ path: path.join(evidence, `metrics-durations-${width}.png`), fullPage: true, animations: 'disabled' });
    await page.locator('#diag-implement-one-body .card-body').evaluateAll(nodes => {
      for (const node of nodes) node.scrollLeft = 0;
    });
  }

  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.evaluate(async () => { (await import('/static/js/router.js')).setActiveTab('operations/routines'); });
  await page.locator('.routine-card').waitFor();
  for (const ms of [13191607, 12345, 303000, 183660000]) {
    automationDuration = ms;
    const expected = await page.evaluate(async ms => {
      const { fmtDuration } = await import('/static/js/common.js');
      await (await import('/static/js/operations.js')).fetchAndRenderOperations('routines');
      return fmtDuration(ms);
    }, ms);
    const text = await page.locator('.routine-card .operation-cell-sub .mono').allTextContents();
    assert.ok(text.includes(expected), `Automation uses the shared format for ${ms}: ${text}`);
    measurements.automation.push({ ms, expected, text });
  }

  await page.evaluate(async now => {
    Date.now = () => now;
    (await import('/static/js/router.js')).navigateToRun('jrun-duration-fixture');
  }, now);
  await page.locator('.step-row').first().waitFor();
  const durationValues = async () => page.evaluate(() => ({
    run: [...document.querySelectorAll('.run-meta-grid > div')].find(cell => cell.querySelector('.label')?.textContent === 'duration').querySelector('.value').textContent,
    steps: [...document.querySelectorAll('.step-row .duration')].map(node => node.textContent),
  }));
  const initial = await durationValues();
  assert.equal(initial.run, '2m 5s ↻', 'running run shows live elapsed duration');
  assert.deepEqual(initial.steps, ['1m 1s ↻', '30.0s ↻', '5m 3s', '-'], 'running steps show elapsed time; completed and pending steps keep stored duration');
  measurements.runDetail.push(initial);
  await page.screenshot({ path: path.join(evidence, 'run-running-1440.png'), fullPage: true, animations: 'disabled' });
  await page.evaluate(async now => {
    Date.now = () => now;
    const detail = await import('/static/js/run-detail.js');
    detail.renderRunDetailMeta();
    detail.renderRunSteps();
  }, now + 65000);
  const refreshed = await durationValues();
  assert.equal(refreshed.run, '3m 10s ↻');
  assert.deepEqual(refreshed.steps, ['2m 6s ↻', '1m 35s ↻', '5m 3s', '-'], 'keyed step rows refresh elapsed values');
  measurements.runDetail.push(refreshed);
  await page.evaluate(async () => {
    const detail = await import('/static/js/run-detail.js');
    const current = detail.getActiveRunDetail();
    current.run.state = 'success';
    current.run.duration_ms = 13191607;
    for (const step of current.steps.filter(step => step.state === 'running')) {
      step.state = 'success';
      step.duration_ms = 13191607;
    }
    detail.renderRunDetailMeta();
    detail.renderRunSteps();
  });
  const completed = await durationValues();
  assert.equal(completed.run, '3h 39m');
  assert.deepEqual(completed.steps, ['3h 39m', '3h 39m', '5m 3s', '-'], 'finishing removes live markers and shows recorded durations');
  measurements.runDetail.push(completed);
  assert.deepEqual(errors, [], 'the duration fixture reports no browser errors');
  fs.writeFileSync(path.join(evidence, 'measurements.json'), JSON.stringify({ passed: true, ...measurements }, null, 2));
  console.log('PASS: 165 duration cells and 55 count cells at 1440/1024; shared Automation formatting; live run and step durations, refresh and completion.');
} catch (error) {
  await page?.screenshot({ path: path.join(evidence, 'failure.png'), fullPage: true });
  throw error;
} finally {
  await browser?.close();
  server.close();
}
