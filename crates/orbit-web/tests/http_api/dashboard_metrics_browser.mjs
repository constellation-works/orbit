// Usage: node dashboard_metrics_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/metrics-browser
// Analytics › Metrics (ORB-15746) through the shipped app, router, renderer and
// CSS: duration, completion and defect trends, series toggles, statistic and table
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

const ORIGINS = ['code-review', 'full-code-review', 'delivery-code-review', 'qa-sweep', 'qa-full-sweep', 'security-review',
  'ci-failure-sweep', 'scanning-sweep', 'review-reconciliation', 'friction-curation', 'other'];

// Bucket 3 holds a fix-for-a-fix filed by a full code review; bucket 9 is empty.
function defects(key) {
  const { count, window, start } = trendWindow(key);
  const counts = (fixForFix, regression, unattributed, origin) => ({
    total: fixForFix + regression + unattributed,
    lineage: { fix_for_fix: fixForFix, regression, unattributed },
    origin: Object.fromEntries(ORIGINS.map(name => [name, origin[name] || 0])),
  });
  return {
    window,
    lineages: ['fix_for_fix', 'regression', 'unattributed'],
    origins: ORIGINS,
    buckets: Array.from({ length: count }, (_, i) => ({
      start: start(i),
      ...(i === 9 ? counts(0, 0, 0, {}) : i === 3 ? counts(1, 1, 1, { 'full-code-review': 2, 'friction-curation': 1 }) : counts(0, 1, 1, { 'qa-sweep': 1, other: 1 })),
    })),
    current: counts(4, 10, 6, { 'code-review': 5, 'full-code-review': 6, 'qa-sweep': 4, 'scanning-sweep': 2, other: 3 }),
    previous: counts(2, 8, 6, { 'code-review': 6, 'qa-sweep': 6, other: 4 }),
  };
}

function tokens(key, url) {
  const { count, window, start } = trendWindow(key);
  const workspace = url.searchParams.get('workspace') || 'ws_fixture';
  const scope = url.searchParams.get('scope') || 'all';
  const selectedTags = (url.searchParams.get('tags') || '').split(',').filter(Boolean);
  const mean = workspace === 'ws_b' ? (scope === 'implement' ? 222 : 1000) : 111;
  const cell = () => ({ n: 5, mean, median: mean, thin: false, excluded_uncovered: 0, excluded_no_invocations: 0 });
  return {
    window, outcome: url.searchParams.get('outcome') || 'done', scope,
    tags: selectedTags, model: null, min_tasks: 3,
    bucket_starts: Array.from({ length: count }, (_, i) => start(i)),
    series: [{ complexity: 'low', cohort: 'all', points: Array.from({ length: count }, () => cell()), total: cell() }],
    total: cell(), excluded_other_model: 0, available_models: [],
    available_tags: Array.from({ length: 9 }, (_, i) => ({ tag: `tag${i}`, tasks: 5 })),
  };
}

const requests = [];
const tokenTagRequests = [];
// An older host has no defect trend; set to serve its 404.
let defectsMissing = false;
// Controlled token replies: while `holdTokensPredicate` matches a request,
// its response is parked in `heldTokens` until `releaseHeldTokens()`.
let heldTokens = [];
let holdTokensPredicate = null;
let heldTokensAsError = false;
function releaseHeldTokens() {
  for (const held of heldTokens.splice(0)) {
    if (held.error) {
      held.res.writeHead(404, { 'content-type': 'application/json' });
      held.res.end(JSON.stringify({ error: 'not found' }));
    } else {
      held.res.setHeader('content-type', 'application/json');
      held.res.end(JSON.stringify(held.payload));
    }
  }
}
async function waitForHeldTokens(count = 1) {
  for (let i = 0; i < 200 && heldTokens.length < count; i++) await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(heldTokens.length, count, 'held token request arrived');
}
function payloadFor(url) {
  switch (url.pathname) {
    case '/api/workspaces': return [
      { id: 'ws_fixture', name: 'Fixture A', status: 'active', is_default: true },
      { id: 'ws_b', name: 'Fixture B', status: 'active', is_default: false },
    ];
    case '/api/diagnostics/task_tokens':
      requests.push(`task_tokens?since=${url.searchParams.get('since')}&scope=${url.searchParams.get('scope')}&workspace=${url.searchParams.get('workspace')}`);
      tokenTagRequests.push((url.searchParams.get('tags') || '').split(',').filter(Boolean));
      return tokens(url.searchParams.get('since'), url);
    case '/api/crews': return { default_crew: null, crews: [] };
    case '/api/workflows/auto/readiness': return { capacity: {}, tasks: [] };
    case '/api/diagnostics/implement_one':
      requests.push(`implement_one?since=${url.searchParams.get('since')}`);
      requests.push(`outcome=${url.searchParams.get('outcome')}`);
      return durations(url.searchParams.get('since'), url.searchParams.get('outcome'));
    case '/api/diagnostics/completions':
      requests.push(`completions?since=${url.searchParams.get('since')}`);
      return completions(url.searchParams.get('since'));
    case '/api/diagnostics/defects':
      requests.push(`defects?since=${url.searchParams.get('since')}`);
      return defects(url.searchParams.get('since'));
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
  if (defectsMissing && url.pathname === '/api/diagnostics/defects') {
    res.writeHead(404, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error: 'not found' }));
    return;
  }
  if (url.pathname.startsWith('/api/')) {
    if (url.pathname === '/api/diagnostics/task_tokens' && holdTokensPredicate && holdTokensPredicate(url)) {
      heldTokens.push({ res, payload: payloadFor(url), error: heldTokensAsError });
      return;
    }
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
  await page.goto(`${origin}#analytics/metrics`);
  const body = page.locator('#metrics-body');
  await body.locator('.mx-sc').first().waitFor();
  await page.evaluate(async () => { await document.fonts.ready; });

  // Duration trend: four series, All first, deltas against the previous window.
  const cards = await body.locator('.mx-sc').evaluateAll(nodes => nodes.map(node => ({
    label: node.querySelector('.mx-sc-label').textContent, value: node.querySelector('.mx-sc-value').textContent,
    delta: node.querySelector('.mx-sc-delta').textContent, pressed: node.getAttribute('aria-pressed'),
  })));
  assert.deepEqual(cards.map(card => card.value), ['43.9m', '43.0m', '41.5m', '47.0m']);
  assert.deepEqual(cards.map(card => card.label), ['All runs', 'Low', 'Medium', 'Hard']);
  assert.match(cards[0].delta, /▲ 46% slower/);
  assert.match(cards[0].delta, /vs prev 24h 30\.1m/);
  assert.ok(cards.every(card => card.pressed === 'true'));
  assert.equal(await page.locator('.tab-pane.active').getAttribute('data-tab'), 'analytics', 'Metrics opens under Analytics');
  assert.match(await page.locator('#metrics-count').textContent(), /200 runs · 276 done · window 24h/);
  pass('duration cards', { cards });

  const paths = () => body.locator('.mx-lines path').evaluateAll(nodes => nodes.map(node => ({ cls: node.getAttribute('class'), d: node.getAttribute('d') })));
  let lines = await paths();
  assert.equal(lines.length, 4, 'All, low, medium and hard lines');
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
  assert.deepEqual(await table.locator('thead th').allTextContents(), ['Bucket', 'Runs', 'All', 'Low', 'Medium', 'Hard']);
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
  assert.match(doneCards[1], /^Low.*56.*20% of done/);
  assert.match(doneCards[2], /^Medium.*127.*46% of done/);
  assert.match(doneCards[3], /^Hard/);
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

  // Defects tab: lineage and origin groupings over the same buckets.
  await body.getByRole('tab', { name: 'Defects' }).click();
  assert.equal(await body.getByRole('tab', { name: 'Defects' }).getAttribute('aria-selected'), 'true');
  const groups = body.getByRole('group', { name: 'Group by' });
  assert.equal(await groups.getByRole('button', { name: 'Lineage' }).getAttribute('aria-pressed'), 'true');
  assert.equal(await body.locator('.mx-sc').count(), 4, 'total plus three lineages');
  assert.equal(await body.locator('.mx-sc-value').first().textContent(), '20');
  assert.equal(await body.locator('.mx-sc .bad').count(), 3, 'a rise in defects reads as worse; unattributed is flat');
  assert.equal(await body.locator('.mx-bars .mx-bar-col').count(), 25);
  assert.equal(await body.locator('.mx-bars .mx-bar-col').nth(3).locator('.mx-s-fix_for_fix').evaluate(node => parseFloat(node.style.height) > 0), true);
  assert.equal(await body.locator('.mx-bars .mx-bar-col').nth(4).locator('.mx-s-fix_for_fix').evaluate(node => parseFloat(node.style.height)), 0);
  await body.locator('.mx-sc.mx-s-unattributed').click();
  assert.equal(await body.locator('.mx-sc.mx-s-unattributed').getAttribute('aria-pressed'), 'false');
  assert.equal(await body.locator('.mx-bars .mx-bar-col').nth(4).locator('.mx-s-unattributed').evaluate(node => parseFloat(node.style.height)), 0, 'a hidden series leaves the stack');
  await page.screenshot({ path: path.join(evidence, 'defects-lineage-1440.png'), fullPage: true, animations: 'disabled' });

  await groups.getByRole('button', { name: 'Origin' }).click();
  assert.equal(await body.locator('.mx-sc').count(), 9, 'total plus seven named finders and Other');
  // Scanning sweeps, friction curation and untagged fold into Other.
  assert.equal(await body.locator('.mx-sc.mx-s-other .mx-sc-value').textContent(), '5');
  assert.equal(await body.locator('.mx-bars .mx-bar-col').nth(3).locator('.mx-s-full-code-review').evaluate(node => parseFloat(node.style.height) > 0), true);
  await hoverColumns.nth(3).hover();
  assert.match(await tip.textContent(), /fix-for-a-fix 1/);
  await page.mouse.move(5, 5);
  await page.screenshot({ path: path.join(evidence, 'defects-origin-1440.png'), fullPage: true, animations: 'disabled' });
  await body.getByRole('button', { name: 'Table', exact: true }).click();
  assert.equal(await body.locator('.mx-trend-view thead th').count(), 13, 'bucket, total and every origin the API sends');
  await body.getByRole('button', { name: 'Chart', exact: true }).click();
  await groups.getByRole('button', { name: 'Lineage' }).click();
  assert.equal(await body.locator('.mx-sc.mx-s-unattributed').getAttribute('aria-pressed'), 'false', 'each grouping keeps its own hidden series');
  await body.locator('.mx-sc.mx-s-unattributed').click();
  pass('defects trend');

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
  assert.ok(requests.includes('implement_one?since=7d') && requests.includes('completions?since=7d') && requests.includes('defects?since=7d'));
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

  await body.getByRole('tab', { name: 'Defects' }).click();
  await body.getByRole('group', { name: 'Group by' }).getByRole('button', { name: 'Origin' }).click();
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth), false, 'nine origin cards fit a phone');
  await page.screenshot({ path: path.join(evidence, 'defects-origin-390.png'), fullPage: true, animations: 'disabled' });
  await page.setViewportSize({ width: 1440, height: 1000 });

  // A host without the defect trend keeps the other trends.
  defectsMissing = true;
  await page.locator('#metrics-window-selector [data-window="7d"]').click();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('window 7d'));
  assert.equal(await body.locator('.mx-empty').count(), 1, 'the Defects tab explains itself');
  assert.equal(await body.getByRole('button', { name: /^Widen to/ }).count(), 0, 'a wider window would not help');
  await body.getByRole('tab', { name: 'Tasks completed' }).click();
  assert.equal(await body.locator('.mx-bars .mx-bar-col').count(), 29, 'completions still render');
  pass('older host without defects');

  // Tokens per task: a held reply from a retired workspace visit cannot
  // overwrite the current visit, including an A→B→A round trip and an
  // unavailable/error reply.
  defectsMissing = false;
  await page.locator('#metrics-window-selector [data-window="7d"]').click();
  await page.waitForFunction(() => document.getElementById('metrics-count').textContent.includes('window 7d'));
  await body.getByRole('tab', { name: 'Tokens per task' }).click();
  const tokenValue = () => body.locator('.mx-sc-value').first().textContent();
  await page.waitForFunction(() => document.querySelector('#metrics-body .mx-sc-value')?.textContent === '111');
  assert.equal(await tokenValue(), '111', 'workspace A tokens at 7d');
  const tagPicker = () => body.getByRole('combobox', { name: 'Compare a tag' });
  const tagChips = () => body.locator('.mx-tags .mx-chip');
  await page.screenshot({ path: path.join(evidence, 'tokens-tags-before-1440.jpg'), type: 'jpeg', quality: 82 });
  await page.setViewportSize({ width: 375, height: 900 });
  await page.screenshot({ path: path.join(evidence, 'tokens-tags-before-375.jpg'), type: 'jpeg', quality: 82 });
  await page.setViewportSize({ width: 1440, height: 1600 });
  for (let i = 0; i < 8; i++) {
    await tagPicker().selectOption(`tag${i}`);
    await body.getByRole('button', { name: `Remove tag tag${i}` }).waitFor();
    if (i < 7) await tagPicker().locator(`option[value="tag${i + 1}"]`).waitFor({ state: 'attached' });
  }
  await page.waitForFunction(() => document.querySelector('#metrics-body .mx-sc-value')?.textContent === '111');
  assert.equal(await tagChips().count(), 8, 'eight compared tag chips remain visible');
  assert.equal(await tagPicker().locator('option').count(), 1, 'the picker offers no ninth tag');
  assert.equal(await tagPicker().locator('option:checked').textContent(), 'Maximum of 8 tags selected');
  await page.screenshot({ path: path.join(evidence, 'tokens-tags-cap-1440.jpg'), type: 'jpeg', quality: 82 });
  await page.setViewportSize({ width: 375, height: 900 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth), false, 'tag chips fit the 375px layout');
  await page.screenshot({ path: path.join(evidence, 'tokens-tags-cap-375.jpg'), type: 'jpeg', quality: 82 });
  await page.setViewportSize({ width: 1440, height: 1600 });
  await body.getByRole('button', { name: 'Remove tag tag0' }).click();
  await body.getByRole('button', { name: 'Remove tag tag0' }).waitFor({ state: 'detached' });
  assert.equal(await tagChips().count(), 7, 'removing a chip makes room');
  await tagPicker().locator('option[value="tag8"]').waitFor({ state: 'attached' });
  assert.ok(await tagPicker().locator('option').count() > 1, 'the picker offers tags again after removal');
  await tagPicker().selectOption('tag8');
  await body.getByRole('button', { name: 'Remove tag tag8' }).waitFor();
  await page.waitForFunction(() => document.querySelector('#metrics-body .mx-sc-value')?.textContent === '111');
  assert.equal(await tagChips().count(), 8, 'a newly available tag can be selected after removal');
  assert.equal(await body.getByText('Tokens per task unavailable').count(), 0, 'the chart remains available after remove-then-add');
  assert.ok(tokenTagRequests.some(tags => tags.length === 8), 'the browser sends requests with eight selected tags');
  assert.ok(tokenTagRequests.every(tags => tags.length <= 8), 'no token request carries more than eight selected tags');
  pass('tokens tag picker cap and recovery', { requests: tokenTagRequests.length, maxTags: Math.max(...tokenTagRequests.map(tags => tags.length)) });

  const invocations = body.getByRole('group', { name: 'Invocations' });
  holdTokensPredicate = url => url.searchParams.get('workspace') === 'ws_fixture' && url.searchParams.get('scope') === 'implement';
  await invocations.getByRole('button', { name: 'Implement only' }).click();
  await waitForHeldTokens();
  await page.locator('#workspace-select').selectOption('ws_b');
  await page.waitForFunction(() => document.querySelector('#metrics-body .mx-sc-value')?.textContent === '222');
  await page.screenshot({ path: path.join(evidence, 'tokens-1440.png'), fullPage: true, animations: 'disabled' });
  await page.setViewportSize({ width: 375, height: 1200 });
  await page.screenshot({ path: path.join(evidence, 'tokens-375.png'), fullPage: true, animations: 'disabled' });
  await page.setViewportSize({ width: 1440, height: 1600 });
  releaseHeldTokens();
  await page.waitForTimeout(200);
  assert.equal(await tokenValue(), '222', 'held A reply does not overwrite B');
  holdTokensPredicate = null;
  pass('tokens workspace fence');

  heldTokensAsError = true;
  holdTokensPredicate = url => url.searchParams.get('workspace') === 'ws_b' && url.searchParams.get('scope') === 'all';
  await invocations.getByRole('button', { name: 'All invocations' }).click();
  await waitForHeldTokens();
  await page.locator('#workspace-select').selectOption('ws_fixture');
  await page.waitForFunction(() => document.querySelector('#metrics-body .mx-sc-value')?.textContent === '111');
  releaseHeldTokens();
  await page.waitForTimeout(200);
  assert.equal(await tokenValue(), '111', 'held B error reply does not overwrite A');
  assert.equal(await body.getByText('Tokens per task unavailable').count(), 0);
  holdTokensPredicate = null;
  heldTokensAsError = false;
  await page.locator('#workspace-select').selectOption('ws_b');
  await page.waitForFunction(() => document.querySelector('#metrics-body .mx-sc-value')?.textContent === '1k');
  pass('tokens round trip and error fence');

  // A full-panel refresh started under All invocations cannot replace a
  // newer Implement-only result.
  holdTokensPredicate = url => url.searchParams.get('workspace') === 'ws_b' && url.searchParams.get('scope') === 'all';
  await page.locator('#refresh-btn').click();
  await waitForHeldTokens();
  await invocations.getByRole('button', { name: 'Implement only' }).click();
  await page.waitForFunction(() => document.querySelector('#metrics-body .mx-sc-value')?.textContent === '222');
  releaseHeldTokens();
  await page.waitForTimeout(200);
  assert.equal(await tokenValue(), '222', 'held full refresh does not overwrite Implement only');
  assert.equal(await invocations.getByRole('button', { name: 'Implement only' }).getAttribute('aria-pressed'), 'true');
  holdTokensPredicate = null;
  pass('tokens full-refresh fence');

  assert.deepEqual(errors, [], 'no browser errors');
  fs.writeFileSync(path.join(evidence, 'metrics-assertions.json'), JSON.stringify({ passed: true, assertions }, null, 2));
  console.log(`PASS: ${assertions.length} Analytics › Metrics scenarios.`);
} catch (error) {
  await page?.screenshot({ path: path.join(evidence, 'failure.png'), fullPage: true });
  throw error;
} finally {
  await browser?.close();
  server.close();
}
