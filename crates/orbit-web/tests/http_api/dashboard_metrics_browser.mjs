// Usage: node dashboard_metrics_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/metrics-browser
// Health › Metrics (ORB-15746) through the shipped app, router, renderer and
// CSS: duration and completion trends, series toggles, statistic and table
// views, hover and keyboard tooltips, window changes, the empty window with
// its widen action, and a 390px phone.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const HOUR = 3600000;
const end = Date.parse('2026-10-11T12:20:00Z');
const minutes = m => m * 60000;
const stats = (n, avgMinutes) => ({ n, avg: minutes(avgMinutes), p50: minutes(avgMinutes * 0.9), p95: minutes(avgMinutes * 2) });
const WIDTHS = { '1h': 300, '24h': 3600, '7d': 21600 };

function trendWindow(key) {
  const width = WIDTHS[key] * 1000;
  const count = key === '1h' ? 13 : key === '24h' ? 25 : 29;
  const start = Math.floor(end / width) * width - (count - 1) * width;
  return {
    count, width,
    window: {
      key, bucket_seconds: width / 1000,
      since: new Date(start).toISOString(), until: new Date(end).toISOString(),
      previous_since: new Date(start - (end - start)).toISOString(), previous_until: new Date(start).toISOString(),
    },
    start: i => new Date(start + i * width).toISOString(),
  };
}

// Hard runs dip in bucket 5 to a single run; bucket 9 has none at all.
function durations(key, outcome = 'succeeded') {
  const { count, window, start } = trendWindow(key);
  if (key === '1h') {
    return {
      window, outcome, truncated: false,
      buckets: Array.from({ length: count }, (_, i) => ({ start: start(i), all: null, low: null, medium: null, hard: null, other_runs: 0 })),
      current: { all: null, low: null, medium: null, hard: null, xhard: null, unset: null },
      previous: { all: stats(3, 20), low: null, medium: null, hard: null, xhard: null, unset: null },
      actors: [], slowest: [], implement_one_by_complexity: [],
    };
  }
  const buckets = Array.from({ length: count }, (_, i) => {
    if (i === 9) return { start: start(i), all: null, low: null, medium: null, hard: null, other_runs: 0 };
    return {
      start: start(i),
      all: stats(9, 30 + i), low: stats(3, 20 + i), medium: stats(3, 30 + i),
      hard: i === 5 ? stats(1, 8) : stats(3, 45 + i), other_runs: i === 5 ? 2 : 0,
    };
  });
  if (outcome === 'all') {
    buckets[5] = { ...buckets[5], all: stats(19, 12), hard: stats(11, 3) };
  }
  return {
    window, outcome, truncated: false, buckets,
    current: { all: outcome === 'all' ? stats(210, 30) : stats(200, 43.9), low: stats(56, 43), medium: stats(70, 41.5), hard: stats(60, 47), xhard: stats(4, 70), unset: stats(10, 25) },
    previous: { all: stats(180, 30.1), low: stats(50, 24.3), medium: stats(60, 31), hard: stats(55, 35), xhard: null, unset: null },
    actors: [
      { actor: 'codex / gpt-6.1-sol', ...stats(126, 42.1), previous_avg: minutes(23.9), trend: buckets.map((_, i) => (i === 9 ? null : minutes(35 + i))) },
      { actor: 'claude / claude-opus-5-5', ...stats(60, 47.5), previous_avg: minutes(50), trend: buckets.map((_, i) => (i % 3 ? null : minutes(45))) },
      { actor: 'gemini / gemini-3.8-flash-high', ...stats(3, 25), previous_avg: null, trend: buckets.map(() => null) },
    ],
    slowest: [
      ...(outcome === 'all' ? [{ ts: start(5), actor: 'codex / gpt-6.1-sol', complexity: 'hard', duration_ms: minutes(300), task_id: 'ORB-15702' }] : []),
      { ts: start(3), actor: 'claude / claude-opus-5-5', complexity: 'hard', duration_ms: minutes(185), task_id: 'ORB-15700' },
      { ts: start(7), actor: 'codex / gpt-6.1-sol', complexity: 'medium', duration_ms: minutes(140), task_id: 'ORB-15701' },
    ],
    implement_one_by_complexity: ['low', 'medium', 'hard'].map(complexity => ({
      complexity, n: 120,
      actors: [
        { actor: 'codex / gpt-6.1-sol', ...stats(80, 30) },
        { actor: 'claude / claude-opus-5-5', ...stats(40, 50) },
      ],
    })),
  };
}

function completions(key) {
  const { count, window, start } = trendWindow(key);
  const counts = (low, medium, hard, other, rejected = 0, archived = 0) => ({
    done: { total: low + medium + hard + other, low, medium, hard, other }, rejected, archived,
  });
  return {
    window,
    buckets: Array.from({ length: count }, (_, i) => ({ start: start(i), ...(i === 9 ? counts(0, 0, 0, 0, 1, 0) : counts(i % 4, 5, 3, i === 2 ? 1 : 0, 0, i === 4 ? 2 : 0)) })),
    current: counts(56, 127, 92, 1, 1, 2),
    previous: counts(71, 77, 81, 0, 1, 32),
  };
}

const requests = [];
function payloadFor(url) {
  switch (url.pathname) {
    case '/api/workspaces': return [{ id: 'ws_fixture', name: 'fixture', status: 'active', is_default: true }];
    case '/api/crews': return { default_crew: null, crews: [] };
    case '/api/workflows/auto/readiness': return { capacity: {}, tasks: [] };
    case '/api/diagnostics/implement_one':
      requests.push(`implement_one?since=${url.searchParams.get('since')}`);
      requests.push(`outcome=${url.searchParams.get('outcome')}`);
      return durations(url.searchParams.get('since'), url.searchParams.get('outcome'));
    case '/api/diagnostics/completions':
      requests.push(`completions?since=${url.searchParams.get('since')}`);
      return completions(url.searchParams.get('since'));
    case '/api/tasks/completion-by-complexity': return {
      by_complexity: [
        { complexity: 'unset', total: 762, statuses: [{ status: 'done', count: 641 }, { status: 'rejected', count: 86 }, { status: 'archived', count: 35 }] },
        { complexity: 'low', total: 1239, statuses: [{ status: 'done', count: 1086 }, { status: 'rejected', count: 77 }, { status: 'archived', count: 60 }] },
      ],
    };
    default: return { items: [], total: 0, limit: 100, truncated: false };
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
const origin = `http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&window=24h`;
const assertions = [];
const pass = (name, detail = {}) => assertions.push({ name, ...detail });
let browser;
let page;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  page = await browser.newPage({ viewport: { width: 1440, height: 1600 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  // The fixture serves no log stream; the dashboard's reconnect noise is not ours.
  page.on('console', message => {
    if (message.type() === 'error' && !/EventSource|Failed to load resource/.test(message.text())) errors.push(message.text());
  });
  await page.goto(`${origin}#diagnostics/metrics`);
  const body = page.locator('#metrics-body');
  await body.locator('.mx-sc').first().waitFor();
  await page.evaluate(async () => { await document.fonts.ready; });

  // Duration trend: four series, All first, deltas against the previous window.
  const cards = await body.locator('.mx-sc').evaluateAll(nodes => nodes.map(node => ({
    label: node.querySelector('.mx-sc-label').textContent, value: node.querySelector('.mx-sc-value').textContent,
    delta: node.querySelector('.mx-sc-delta').textContent, pressed: node.getAttribute('aria-pressed'),
  })));
  assert.deepEqual(cards.map(card => card.value), ['43.9m', '43.0m', '41.5m', '47.0m']);
  assert.match(cards[0].delta, /▲ 46% slower/);
  assert.match(cards[0].delta, /vs prev 24h 30\.1m/);
  assert.ok(cards.every(card => card.pressed === 'true'));
  assert.equal(await page.locator('#diagnostics-main').evaluate(node => getComputedStyle(node).display), 'none', 'the list panel is swapped out');
  assert.match(await page.locator('#metrics-count').textContent(), /200 runs · 276 done · window 24h/);
  pass('duration cards', { cards });

  const paths = () => body.locator('.mx-lines path').evaluateAll(nodes => nodes.map(node => ({ cls: node.getAttribute('class'), d: node.getAttribute('d') })));
  let lines = await paths();
  assert.equal(lines.length, 4, 'All, easy, medium and hard lines');
  assert.ok(lines.at(-1).cls.includes('mx-s-all'), 'All draws over the complexity lines');
  assert.equal((lines.at(-1).d.match(/M/g) || []).length, 2, 'the bucket without runs breaks the All line');
  assert.equal(await body.locator('.mx-pt.hollow').count(), 1, 'only the one-run hard bucket is hollow');
  assert.equal(await body.locator('.mx-volume .mx-vol-col').count(), 25, 'one runs bar per hourly bucket');
  assert.match(await body.locator('.mx-prev-label').textContent(), /All · prev 24h avg 30\.1m/);
  await page.screenshot({ path: path.join(evidence, 'duration-1440.png'), fullPage: true, animations: 'disabled' });
  pass('duration chart', { lines: lines.length });

  // Hover the low-sample bucket: tooltip rows, amber count, other runs note.
  const hoverColumns = body.locator('.mx-hover-col');
  await hoverColumns.nth(5).hover();
  const tip = body.locator('.mx-tip');
  await tip.waitFor({ state: 'visible' });
  const tipText = await tip.textContent();
  assert.match(tipText, /All runs×9/);
  assert.match(tipText, /Hard×18\.0m/);
  assert.match(tipText, /xhard \/ unset: 2 runs \(in All only\)/);
  assert.match(tipText, /Amber counts: fewer than 3 runs/);
  assert.equal(await tip.locator('.mx-tip-n.low').count(), 1);
  await hoverColumns.nth(9).hover();
  assert.match(await tip.textContent(), /No implement_one runs finished in this bucket/);
  await page.mouse.move(5, 5);
  await tip.waitFor({ state: 'hidden' });
  await page.screenshot({ path: path.join(evidence, 'duration-tooltip-1440.png'), animations: 'disabled' });
  pass('duration tooltip');

  // Keyboard: the chart frame steps through buckets from the newest.
  await body.locator('.mx-chart-frame').focus();
  await page.keyboard.press('ArrowLeft');
  await tip.waitFor({ state: 'visible' });
  assert.match(await tip.locator('.mx-tip-title').textContent(), /in progress/, 'first step lands on the newest bucket');
  await page.keyboard.press('ArrowLeft');
  assert.doesNotMatch(await tip.locator('.mx-tip-title').textContent(), /in progress/);
  await page.keyboard.press('Escape');
  await tip.waitFor({ state: 'hidden' });
  pass('keyboard tooltip');

  // Toggle series; the last visible one cannot be hidden.
  await body.locator('.mx-sc').nth(1).click();
  lines = await paths();
  assert.equal(lines.length, 3);
  assert.equal(await body.locator('.mx-sc').nth(1).getAttribute('aria-pressed'), 'false');
  for (const index of [0, 2, 3]) await body.locator('.mx-sc').nth(index).click();
  assert.equal((await paths()).length, 1, 'hard alone');
  await body.locator('.mx-sc').nth(3).click();
  assert.equal(await body.locator('.mx-sc').nth(3).getAttribute('aria-pressed'), 'true', 'the last series stays shown');
  for (const index of [0, 1, 2]) await body.locator('.mx-sc').nth(index).click();
  assert.equal((await paths()).length, 4);
  pass('series toggles');

  // Statistic and table view.
  await body.getByRole('button', { name: 'p95' }).click();
  assert.equal(await body.locator('.mx-sc-value').first().textContent(), '1h 28m');
  await body.getByRole('button', { name: 'Table' }).click();
  const table = body.locator('.mx-trend-view table');
  assert.equal(await table.locator('tbody tr').count(), 25);
  assert.match(await table.locator('caption').textContent(), /p95 per bucket/);
  assert.match(await table.locator('tbody tr').first().textContent(), /in progress/, 'newest bucket first');
  assert.match(await table.locator('tbody tr').nth(15).textContent(), /—/, 'an empty bucket reads as a dash');
  await page.screenshot({ path: path.join(evidence, 'duration-table-1440.png'), fullPage: true, animations: 'disabled' });
  await body.getByRole('button', { name: 'Chart' }).click();
  await body.getByRole('button', { name: 'Avg' }).click();
  pass('statistic and table');

  // Tasks completed tab.
  await body.getByRole('tab', { name: 'Tasks completed' }).click();
  assert.equal(await body.getByRole('tab', { name: 'Tasks completed' }).getAttribute('aria-selected'), 'true');
  assert.equal(await body.getByRole('group', { name: 'Statistic' }).count(), 0, 'counts have no statistic');
  const doneCards = await body.locator('.mx-sc').evaluateAll(nodes => nodes.map(node => node.textContent));
  assert.match(doneCards[0], /Done \(total\)276▲ 21% morevs prev 24h 2291 rejected · 2 archived/);
  assert.match(doneCards[2], /Medium.*127.*46% of done/);
  assert.equal(await body.locator('.mx-bars .mx-bar-col').count(), 25);
  assert.equal((await paths()).length, 1, 'the total line');
  await hoverColumns.nth(4).hover();
  assert.match(await tip.textContent(), /rejected 0 · archived 2/);
  await hoverColumns.nth(9).hover();
  assert.match(await tip.textContent(), /No tasks reached done in this bucket/);
  await page.mouse.move(5, 5);
  await page.screenshot({ path: path.join(evidence, 'completed-1440.png'), fullPage: true, animations: 'disabled' });
  await page.keyboard.press('Tab');
  pass('completed trend', { doneCards });

  // Detail cards.
  assert.equal(await body.locator('.mx-actors tbody tr').count(), 3);
  assert.equal(await body.locator('.mx-actors tbody tr.mx-dim').count(), 1, 'a three-run actor is dimmed');
  assert.match(await body.locator('.mx-actors tbody tr').first().textContent(), /gpt-6\.1-sol.*126.*42\.1m.*▲ 76%/);
  assert.equal(await body.locator('.mx-slow-list li').count(), 2);
  assert.equal(await body.locator('.mx-slow-list a').first().getAttribute('href').then(href => new URL(href, origin).hash), '#tasks?open=ORB-15700');
  assert.equal(await body.locator('.mx-cx-tab[aria-pressed="true"]').textContent(), 'medium120');
  assert.equal(await body.locator('.mx-cx-tab:disabled').count(), 2, 'xhard and unset have no runs');
  assert.match(await body.locator('.mx-outcome').first().textContent(), /unset \(unlabeled\)84\.1% done · n=762/);
  pass('detail cards');

  // Window change through the panel's own selector.
  await body.getByRole('tab', { name: 'implement_one duration' }).click();
  await page.locator('#metrics-window-selector [data-window="7d"]').click();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('window 7d'));
  assert.ok(requests.includes('implement_one?since=7d') && requests.includes('completions?since=7d'));
  assert.match(await body.locator('.mx-sc-sub').first().textContent(), /vs prev 7d/);
  assert.equal(await body.locator('.mx-volume .mx-vol-col').count(), 29, '6-hour buckets');
  assert.match(await body.locator('.mx-chart .mx-notes').textContent(), /6-hour buckets/);
  pass('window change');

  // An empty window offers the next wider one.
  await page.locator('#metrics-window-selector [data-window="1h"]').click();
  await body.getByText('No implement_one runs finished in the last 1h').waitFor();
  await page.screenshot({ path: path.join(evidence, 'empty-1h-1440.png'), fullPage: true, animations: 'disabled' });
  await body.getByRole('button', { name: 'Widen to 24h' }).click();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('window 24h'));
  assert.equal(await page.locator('#metrics-window-selector [data-window="24h"]').getAttribute('aria-selected'), 'true');
  pass('empty window widens');

  // Outcome selection fetches the whole duration payload and survives navigation/reload.
  const outcomeButtons = body.getByRole('group', { name: 'Run outcome' });
  assert.equal(await outcomeButtons.getByRole('button', { name: 'Succeeded', exact: true }).getAttribute('aria-pressed'), 'true');
  assert.match(await body.locator('.mx-trend-view > .mx-notes').textContent(), /successful runs with a committed candidate/);
  const succeededHardBar = await body.locator('.mx-volume .mx-vol-col').nth(5).locator('.mx-s-hard').evaluate(node => node.style.height);
  await outcomeButtons.getByRole('button', { name: 'All', exact: true }).click();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('210 runs'));
  assert.ok(requests.includes('outcome=all'));
  assert.equal(new URLSearchParams(new URL(page.url()).hash.split('?')[1]).get('outcome'), 'all');
  assert.equal(await body.locator('.mx-sc-value').first().textContent(), '30.0m');
  assert.equal(await body.locator('.mx-slow-dur').first().textContent(), '5h 00m');
  const allHardBar = await body.locator('.mx-volume .mx-vol-col').nth(5).locator('.mx-s-hard').evaluate(node => node.style.height);
  assert.ok(parseFloat(allHardBar) > parseFloat(succeededHardBar), 'run bars use filtered bucket counts');
  assert.match(await body.locator('.mx-trend-view > .mx-notes').textContent(), /all implement_one invocations/);
  await body.locator('.mx-hover-col').nth(5).hover();
  assert.match(await body.locator('.mx-tip').textContent(), /All runs×19/);
  await page.mouse.move(5, 5);
  await body.getByRole('button', { name: 'Table', exact: true }).click();
  assert.match(await body.locator('.mx-trend-view tbody tr').nth(19).textContent(), /3\.0m \(11\)/);
  await page.locator('#metrics-window-selector [data-window="7d"]').click();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('210 runs') && document.getElementById('metrics-count').textContent.includes('window 7d'));
  assert.equal(await outcomeButtons.getByRole('button', { name: 'All', exact: true }).getAttribute('aria-pressed'), 'true');
  await page.locator('#metrics-window-selector [data-window="24h"]').click();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('210 runs') && document.getElementById('metrics-count').textContent.includes('window 24h'));
  await page.reload();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('210 runs'));
  assert.equal(await outcomeButtons.getByRole('button', { name: 'All', exact: true }).getAttribute('aria-pressed'), 'true');
  await outcomeButtons.getByRole('button', { name: 'Succeeded', exact: true }).click();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('200 runs'));
  await page.goBack();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('210 runs'));
  await page.goForward();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('200 runs'));
  assert.equal(await body.locator('.mx-slow-dur').first().textContent(), '3h 05m');
  pass('outcome toggle, filtered values, URL reload and history');

  // Phone width: no sideways page scroll, tables scroll inside their cards.
  await page.setViewportSize({ width: 390, height: 1400 });
  const phone = await page.evaluate(() => ({
    overflow: document.documentElement.scrollWidth > window.innerWidth,
    plot: document.querySelector('#metrics-body .mx-plot').getBoundingClientRect().width,
    tables: [...document.querySelectorAll('#metrics-body .mx-table-wrap')].map(wrap => getComputedStyle(wrap).overflowX),
  }));
  assert.equal(phone.overflow, false, `no page overflow at 390px: ${JSON.stringify(phone)}`);
  assert.ok(phone.plot > 260, `the plot keeps two thirds of the phone width: ${phone.plot}`);
  await page.screenshot({ path: path.join(evidence, 'metrics-390.png'), fullPage: true, animations: 'disabled' });
  pass('phone layout', phone);

  assert.deepEqual(errors, [], 'no browser errors');
  fs.writeFileSync(path.join(evidence, 'metrics-assertions.json'), JSON.stringify({ passed: true, assertions }, null, 2));
  console.log(`PASS: ${assertions.length} Health › Metrics scenarios.`);
} catch (error) {
  await page?.screenshot({ path: path.join(evidence, 'failure.png'), fullPage: true });
  throw error;
} finally {
  await browser?.close();
  server.close();
}
