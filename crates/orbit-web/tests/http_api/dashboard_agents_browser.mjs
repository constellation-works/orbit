// Usage: node dashboard_agents_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/agents-browser
// Observe › Agents through the shipped app, router, renderer and CSS, fed
// contract-shaped `/api/agents` payloads: `#analytics/orchestrators` opening
// `#agents/orchestrators`, the role tabs and their counts, row selection
// driving the detail column, the disclosure surviving a refresh, no
// Implementers role panel, the Reviewers provisional
// banner, `accepts often` tag and verdict-outcome bars, the Pilots
// tier-calibration matrices, the empty state, and the 375px layout (ranking
// scrolling in its own box, detail stacked below, 44px touch targets).
// Screenshots of each role tab (and the Implementers ranking) at 1440 and 375 go to the evidence directory.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';
import { implementerCard, implementerDetail } from './agents_implementers_fixture.mjs';
import { pilotCard, pilotDetail } from './agents_pilots_fixture.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const UNITS = { orchestrators: 'owned tasks', implementers: 'implemented tasks', reviewers: 'settled reviews', pilots: 'assessed tasks' };
const TERMS = {
  orchestrators: [
    ['landed', 'Landed', 1, 'credit', 'Landed tasks, one each whatever their complexity, so per 100 owned tasks the percent that landed.'],
    ['spec_defects', 'Spec defects', -1, 'problem', 'Description or criteria edited after the first run started, agent blocks naming the criteria, final-recovery escalations citing them, and regressions with escape spec_ambiguous.'],
    ['spec_rework', 'Spec-caused rework', -1, 'problem', 'Repeat runs, [BLOCKED] or unmerged PRs, and operator requeues whose cause is spec.'],
  ],
  implementers: [
    ['weighted_landed', 'Weighted landed', 1, 'credit', 'Landed tasks whose final implementation run is the row\'s.'],
    ['first_pass_accepts', 'First-pass accepts', 1, 'credit', 'Reviewed tasks whose first review verdict was accept.'],
    ['review_rejects', 'Review rejects', -1, 'problem', 'reject verdicts on the row\'s candidates.'],
    ['reviewer_repairs', 'Reviewer repairs', -0.5, 'problem', 'accept_with_fixes verdicts on the row\'s candidates.'],
    ['crew_rework', 'Crew-caused rework', -1, 'problem', 'Rework with cause crew.'],
    ['escaped_regressions', 'Escaped regressions', -2, 'problem', 'Regressions with escape no_test.'],
  ],
  reviewers: [
    ['findings_held', 'Findings that held', 1, 'credit', 'Findings repaired by the reviewer and validated, open findings on a reject whose cited path the next run changed, and CONFIRMED review-job findings; each weighted by severity: critical 8, high 4, medium 2, low 1.'],
    ['fixes_held', 'Fixes that held', 1, 'credit', 'accept_with_fixes verdicts whose fixes passed owner revalidation and whose task has no review_missed regression.'],
    ['rejected_findings', 'Rejected findings', -1, 'problem', 'Findings closed by an authorized decision, and review-job findings finding verification REJECTED.'],
    ['false_rejects', 'False rejects', -2, 'problem', 'reject verdicts whose next run changed none of the cited paths and whose next review accepted.'],
    ['failed_fixes', 'Failed fixes', -1, 'problem', 'Reviewer fixes that failed owner revalidation and were escalated.'],
    ['missed_regressions', 'Missed regressions', -4, 'problem', 'Regressions with escape review_missed against tasks the row accepted, whenever filed.'],
    ['incomplete_reviews', 'Incomplete reviews', -0.5, 'problem', 'incomplete verdicts not caused by infrastructure.'],
  ],
};
const terms = role => TERMS[role].map(([key, label, weight, group, definition]) => ({ key, label, weight, group, definition }));

const stats = (hours, rework) => [
  { key: 'unclassified', label: 'Unclassified rework and regressions (not scored)', value: rework[3] },
  { key: 'dropped_initiative', label: 'Dropped initiative', value: 1 },
  { key: 'unknown_origin', label: 'Unknown origin', value: 2 },
  { key: 'active_hours', label: 'Active hours', value: hours },
  { key: 'rework_spec', label: 'Rework: spec (counted)', value: rework[0] },
  { key: 'rework_crew', label: 'Rework: crew (counted against implementers)', value: rework[1] },
  { key: 'rework_infrastructure', label: 'Rework: infrastructure (not scored)', value: rework[2] },
  { key: 'rework_no_cause', label: 'Rework: no cause (not scored)', value: rework[3] },
];
function orchestratorRow(crew, rank, units, counts, raw, score, extra) {
  const per100 = Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, Math.round((count * 10000) / units) / 100]));
  return { crew, model: 'model unknown', units, below_support: rank == null, rank, counts, per_100: per100, raw, score, runs_without_model: 0, provisional: null, stats: stats(...extra) };
}
const ORCHESTRATOR_ROWS = [
  orchestratorRow('claude-orchestrator', 1, 186, { landed: 180, spec_defects: 14, spec_rework: 6 }, 86.02, 85.05, [6.5, [6, 19, 4, 3]]),
  orchestratorRow('codex-orchestrator', 2, 74, { landed: 70, spec_defects: 11, spec_rework: 5 }, 72.97, 72.97, [41.25, [5, 12, 2, 1]]),
  orchestratorRow('operator', 3, 22, { landed: 19, spec_defects: 4, spec_rework: 3 }, 54.55, 54.55, [12, [3, 2, 0, 4]]),
  orchestratorRow('gemini-orchestrator', null, 4, { landed: 3, spec_defects: 2, spec_rework: 1 }, 0, 0, [3.5, [1, 0, 0, 0]]),
];
let orchestratorRows = ORCHESTRATOR_ROWS;

// Reviewer rows scored as the server does: raw = Σ weight × rate per 100,
// pulled down toward the fleet average by K = 30 when above it.
const OUTCOMES = ['outcome_held_up', 'outcome_caught', 'outcome_false_alarm', 'outcome_missed', 'outcome_unresolved'];
function reviewerRow(crew, model, units, counts, provisional, [gate, jobs, accept, peers, often, minutes], outcomes) {
  const per100 = Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, Math.round((count * 10000) / units) / 100]));
  const raw = Math.round(terms('reviewers').reduce((sum, term) => sum + term.weight * per100[term.key], 0) * 100) / 100;
  return {
    crew, model, units, below_support: units < 10, rank: null, counts, per_100: per100, raw, score: raw, runs_without_model: 0, provisional,
    stats: [
      { key: 'gate_reviews', label: 'Review gate settlements', value: gate },
      { key: 'review_jobs', label: 'Review job runs that filed findings', value: jobs },
      { key: 'accept_rate', label: 'Accept rate (%)', value: accept },
      { key: 'peer_accept_rate', label: "Peers' accept rate on the same tiers (%)", value: peers },
      { key: 'accepts_often', label: 'Accepts often', value: often },
      ...OUTCOMES.map((key, index) => ({ key, label: ['Accepted and held up', 'Caught a real problem', 'False alarm', 'Missed a regression', 'Unresolved (not judged yet, incomplete or failed fixes)'][index], value: outcomes[index] })),
      { key: 'median_review_minutes', label: 'Median review minutes', value: minutes },
    ],
  };
}
const REVIEWER_ROWS = [
  reviewerRow('codex-review', 'gpt-5.5-codex', 64, { findings_held: 118, fixes_held: 9, rejected_findings: 7, false_rejects: 3, failed_fixes: 1, missed_regressions: 1, incomplete_reviews: 2 }, 41, [60, 4, 55, 62, false, 11.5], [30, 22, 3, 1, 8]),
  reviewerRow('claude-review', 'claude-opus-5-5', 48, { findings_held: 46, fixes_held: 12, rejected_findings: 4, false_rejects: 1, failed_fixes: 2, missed_regressions: 3, incomplete_reviews: 1 }, 30, [48, 0, 81, 58, true, 8], [34, 8, 1, 3, 2]),
  reviewerRow('gemini-review', 'gemini-3-pro', 6, { findings_held: 6, fixes_held: 0, rejected_findings: 2, false_rejects: 1, failed_fixes: 0, missed_regressions: 0, incomplete_reviews: 0 }, 6, [6, 0, 33, 67, false, 14], [2, 1, 1, 0, 2]),
];
const REVIEWER_FLEET = Math.round((REVIEWER_ROWS.reduce((sum, row) => sum + row.units * row.raw, 0) / REVIEWER_ROWS.reduce((sum, row) => sum + row.units, 0)) * 100) / 100;
for (const row of REVIEWER_ROWS) row.score = Math.round(Math.min(row.raw, (row.units * row.raw + 30 * REVIEWER_FLEET) / (row.units + 30)) * 100) / 100;
REVIEWER_ROWS.filter(row => !row.below_support).sort((a, b) => b.score - a.score).forEach((row, index) => { row.rank = index + 1; });
let computedAt = '2026-10-19T08:00:00.000Z';
const requests = [];

function header(role, url) {
  const requested = url.searchParams.get('window') || '30d';
  return {
    role, window: requested === 'all' ? '90d' : requested, requested_window: requested,
    since: '2026-09-19T08:00:00Z', min_support: 10, computed_at: computedAt, cache_ttl_seconds: 300, tasks_scanned: 6450,
    status: 'ok', unit: UNITS[role], terms: TERMS[role] ? terms(role) : [],
  };
}
function scorecard(role, url) {
  const head = header(role, url);
  if (role === 'implementers') return { ...implementerCard(), ...head };
  if (role === 'reviewers') {
    const rows = url.searchParams.get('window') === '1h' ? [] : REVIEWER_ROWS;
    return { ...head, fleet_average: rows.length ? REVIEWER_FLEET : null, provisional: rows.reduce((sum, row) => sum + row.provisional, 0), rows };
  }
  if (role === 'pilots') return { ...pilotCard(), computed_at: computedAt };
  if (role !== 'orchestrators') return { ...head, fleet_average: null, rows: [] };
  const rows = url.searchParams.get('window') === '1h' ? [] : orchestratorRows;
  return { ...head, fleet_average: rows.length ? 79.02 : null, rows };
}
const task = (id, title, count, detail) => ({ task_id: id, title, status: 'done', count, occurrences: [{ at: '2026-10-12T14:03:00Z', detail, run_id: null }] });
const REVIEWER_TASKS = {
  'codex-review': { rejected_findings: [
    task('ORB-16410', 'Lease renewal under clock skew', 1, 'disposed: accepted risk, tracked separately'),
    task('ORB-16455', 'Finding: drain receipts double-count retries', 1, 'finding verification REJECTED'),
  ] },
  'claude-review': { missed_regressions: [
    task('ORB-16288', 'Host forward retries on 502', 2, 'regression: escape review_missed'),
    task('ORB-16301', 'Review settlement replay', 1, 'regression: escape review_missed'),
  ] },
};
function detail(role, url) {
  if (role === 'implementers') return implementerDetail(url.searchParams.get('crew'), url.searchParams.get('model'));
  if (role === 'pilots') return { ...pilotDetail(url.searchParams.get('crew'), url.searchParams.get('model')), computed_at: computedAt };
  const crew = url.searchParams.get('crew');
  if (role === 'reviewers') {
    const row = REVIEWER_ROWS.find(entry => entry.crew === crew);
    return {
      ...header(role, url), fleet_average: REVIEWER_FLEET, provisional: 77, row,
      terms: terms(role).map(term => ({
        key: term.key, label: term.label, weight: term.weight, group: term.group,
        count: row.counts[term.key], per_100: row.per_100[term.key], contribution: term.weight * row.per_100[term.key],
        tasks: (REVIEWER_TASKS[crew] || {})[term.key] || [],
      })),
    };
  }
  const row = orchestratorRows.find(entry => entry.crew === crew);
  const tasks = {
    'claude-orchestrator': [
      task('ORB-16012', 'Ship mode for owner-local delivery', 3, 'spec_edit: task fields updated: acceptance_criteria'),
      task('ORB-16120', 'Dock layout at phone width', 2, 'criteria_block: acceptance-criterion-unmet'),
      task('ORB-16204', 'Drain receipt lookup by claim', 1, 'regression: escape spec_ambiguous'),
    ],
    'codex-orchestrator': [
      task('ORB-15877', 'Host forward retries on 502', 4, 'spec_edit: task fields updated: description'),
      task('ORB-15940', 'Review settlement replay', 1, 'final_recovery_escalated: criteria'),
    ],
    operator: [task('ORB-15001', 'Config keys reference page', 2, 'spec_edit: task fields updated: description')],
    'gemini-orchestrator': [task('ORB-16330', 'Pilot receipt backfill', 2, 'criteria_block: scope-ambiguous')],
  }[crew] || [];
  return {
    ...header(role, url), fleet_average: 192.13, row,
    terms: terms(role).map(term => ({
      key: term.key, label: term.label, weight: term.weight, group: term.group,
      count: row.counts[term.key], per_100: row.per_100[term.key], contribution: term.weight * row.per_100[term.key],
      tasks: term.key === 'spec_defects' ? tasks : [],
    })),
  };
}
function attribution(url) {
  const requested = url.searchParams.get('window') || '30d';
  return {
    window: requested === 'all' ? '90d' : requested, requested_window: requested, since: '2026-09-19T08:00:00Z',
    computed_at: computedAt, cache_ttl_seconds: 300, total: 131,
    owners: { orchestrators: 34, implementers: 61, reviewers: 19, pilots: 0, not_scored: { count: 17, reasons: { infrastructure: 9, no_cause: 5, unattributed: 3 } } },
    charged: { orchestrators: [], implementers: [], reviewers: [], pilots: [] },
  };
}

function payloadFor(url) {
  const agents = url.pathname.match(/^\/api\/agents\/(\w+)\/(scorecard|detail)$/);
  if (agents) {
    requests.push(`${agents[1]}/${agents[2]}${agents[2] === 'detail' ? `:${url.searchParams.get('crew')}` : ''}`);
    return agents[2] === 'scorecard' ? scorecard(agents[1], url) : detail(agents[1], url);
  }
  switch (url.pathname) {
    case '/api/agents/attribution': return attribution(url);
    case '/api/workspaces': return [{ id: 'ws_fixture', name: 'orbit', status: 'active', is_default: true }];
    case '/api/crews': return { default_crew: null, crews: [] };
    case '/api/workflows/auto/readiness': return { capacity: { drain_phase: 'idle' }, tasks: [] };
    case '/api/audit/summary': return { events: 1, failed_runs: 0, denials: 0, active_long_runs: 0, window: '30d' };
    case '/api/log': return { events: [], offset: 0 };
    default: return { items: [], total: 0, limit: 100, truncated: false };
  }
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (url.pathname === '/api/log/stream') { res.writeHead(204); res.end(); return; }
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
const origin = `http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&window=30d`;
const results = [];
const pass = (name, detailInfo = {}) => results.push({ name, ...detailInfo });
const errors = [];
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  page.on('pageerror', error => errors.push(String(error)));
  page.on('console', message => {
    if (message.type() === 'error' && !/EventSource|Failed to load resource/.test(message.text())) errors.push(message.text());
  });
  const body = page.locator('#agents-body');
  const shot = name => page.screenshot({ path: path.join(evidence, `${name}.png`), animations: 'disabled' });

  // Where the leading cells and the score sit against the ranking's scroll box.
  const rankingVisibility = () => page.evaluate(() => {
    const wrap = document.querySelector('.agents-table-wrap');
    const box = wrap.getBoundingClientRect();
    const inside = node => { const rect = node.getBoundingClientRect(); return rect.left >= box.left - 0.5 && rect.right <= box.right + 0.5; };
    const heads = [...document.querySelectorAll('.agents-table thead th')];
    const head = text => heads.find(th => th.textContent.trim() === text);
    const rows = [...document.querySelectorAll('.agents-row')];
    return {
      headersInside: ['Rank', 'Crew · model', 'Score'].map(text => [text, inside(head(text))]),
      scoreInsideEveryRow: rows.every(row => inside(row.querySelector('.agents-score-col .agents-score-value'))),
      rowCount: rows.length,
      scrolls: wrap.scrollWidth > wrap.clientWidth + 1,
      cue: getComputedStyle(wrap).backgroundImage.includes('radial-gradient'),
      pageOverflow: document.documentElement.scrollWidth > window.innerWidth,
    };
  });

  // The retired Analytics › Orchestrators link opens Agents › Orchestrators.
  await page.goto(`${origin}#analytics/orchestrators?window=30d`);
  await body.locator('.agents-row').first().waitFor();
  await page.evaluate(async () => { await document.fonts.ready; });
  assert.equal(new URL(page.url()).hash, '#agents/orchestrators?window=30d');
  assert.equal(await page.locator('.tab-pane.active').getAttribute('data-tab'), 'agents');
  assert.equal(await page.locator('.rail .tab.active').getAttribute('data-tab'), 'agents', 'Agents is the active rail destination');
  assert.equal(await page.locator('#analytics-subtabs [data-subtab="orchestrators"]').count(), 0, 'Analytics lists no Orchestrators');
  assert.match(await page.locator('.rail-group', { has: page.locator('[data-tab="agents"]') }).locator('.rail-group-label').textContent(), /Observe/);
  pass('legacy route opens Agents');

  // Role tabs and their counts; the strip; the default selection.
  await page.waitForFunction(() => document.getElementById('agents-count-reviewers').textContent === '2');
  await page.waitForFunction(() => document.getElementById('agents-count-pilots').textContent === '2');
  const counts = await page.locator('#agents-roles .agents-role-count').allTextContents();
  assert.deepEqual(counts, ['3', '5', '2', '2'], 'three ranked orchestrators, five implementers, two reviewers and two pilots');
  assert.match(await body.locator('.agents-strip').textContent(), /131 problems in 30d/);
  assert.equal(await body.locator('.agents-strip-seg.is-current.agents-owner-orchestrators').count(), 1);
  assert.equal(await body.locator('.agents-row-select[aria-pressed="true"] .agents-crew').textContent(), 'claude-orchestrator');
  await body.locator('.agents-penalty .agents-task').first().waitFor();
  assert.deepEqual(await body.locator('.agents-penalty .agents-task-id').allTextContents(), ['ORB-16012', 'ORB-16120', 'ORB-16204']);
  assert.match(await body.locator('tr.agents-row-below').textContent(), /below 10/);
  pass('ranking, strip and default detail', { counts });
  await shot('agents-orchestrators-dark-1440');

  // Selecting a row moves the detail column to it.
  await body.locator('.agents-row', { hasText: 'codex-orchestrator' }).locator('.agents-row-select').click();
  await page.waitForFunction(() => document.querySelector('#agents-detail h3')?.textContent === 'codex-orchestrator');
  await body.locator('.agents-penalty .agents-task-id', { hasText: 'ORB-15877' }).waitFor();
  assert.equal(await body.locator('.agents-detail-rank').textContent(), '#2');
  assert.equal(await body.locator('.agents-penalty').getAttribute('data-term'), 'spec_defects');
  assert.match(await body.locator('.agents-stats').textContent(), /Active hours41\.3/);
  assert.equal(await body.locator('.agents-row.is-selected .agents-crew').textContent(), 'codex-orchestrator');
  // A penalty task opens the task.
  const taskHref = await body.locator('.agents-penalty .agents-task-id').first().getAttribute('href');
  assert.match(taskHref, /#tasks\?open=ORB-15877/);
  pass('selection updates the detail column');

  // The disclosure stays open across a refresh, even when a newer scan
  // rebuilds the ranking and re-reads the detail.
  await body.locator('.agents-how > summary').click();
  assert.equal(await body.locator('.agents-how').evaluate(node => node.open), true);
  assert.match(await body.locator('.agents-how').textContent(), /score = min\(raw, \(owned tasks × raw \+ 30 × fleet average\)/);
  const detailReads = requests.filter(entry => entry === 'orchestrators/detail:codex-orchestrator').length;
  computedAt = '2026-10-19T08:05:00.000Z';
  await page.locator('#refresh-btn').click();
  for (let tries = 0; tries < 50 && requests.filter(entry => entry === 'orchestrators/detail:codex-orchestrator').length <= detailReads; tries += 1) {
    await page.waitForTimeout(100);
  }
  await page.waitForTimeout(100);
  assert.ok(requests.filter(entry => entry === 'orchestrators/detail:codex-orchestrator').length > detailReads, 'a newer scan re-reads the selected detail');
  assert.equal(await body.locator('.agents-how').evaluate(node => node.open), true, 'the disclosure keeps its state across a poll');
  assert.equal(await body.locator('.agents-row-select[aria-pressed="true"] .agents-crew').textContent(), 'codex-orchestrator', 'the selection survives a poll');
  await shot('agents-orchestrators-how-dark-1440');
  pass('disclosure survives a refresh');

  // Reviewers: the provisional banner under the strip, the ranking with the
  // `accepts often` tag, the detail and each reviewer's verdict outcomes.
  await page.locator('#agents-roles [data-view="reviewers"]').click();
  await body.locator('.agents-banner').waitFor();
  assert.equal(new URL(page.url()).hash, '#agents/reviewers?window=30d');
  assert.match(await body.locator('.agents-banner').textContent(), /Provisional\. 77 of 118 reviews in this window are younger than the 30-day miss lookback/);
  assert.deepEqual(await body.locator('.agents-row .agents-crew').allTextContents(), ['codex-review', 'claude-review', 'gemini-review']);
  assert.deepEqual(await body.locator('.agents-row .agents-tag').allTextContents(), ['accepts often']);
  assert.equal(await body.locator('.agents-row', { hasText: 'claude-review' }).locator('.agents-tag').count(), 1, 'the tag sits on the row accepting well above its peers');
  const order = await page.evaluate(() => {
    const top = selector => document.querySelector(selector).getBoundingClientRect();
    return { strip: top('.agents-strip').bottom, banner: top('.agents-banner').top, bannerBottom: top('.agents-banner').bottom, ranking: top('.agents-ranking').top, width: top('.agents-banner').width, body: top('#agents-body').width };
  });
  assert.ok(order.strip <= order.banner && order.bannerBottom <= order.ranking, `the banner sits between the strip and the ranking: ${JSON.stringify(order)}`);
  assert.ok(order.width > order.body * 0.9, 'the banner spans the body');
  await body.locator('.agents-row', { hasText: 'claude-review' }).locator('.agents-row-select').click();
  await page.waitForFunction(() => document.querySelector('#agents-detail h3')?.textContent === 'claude-review');
  await body.locator('.agents-penalty .agents-task-id', { hasText: 'ORB-16288' }).waitFor();
  assert.equal(await body.locator('.agents-penalty').getAttribute('data-term'), 'missed_regressions');
  assert.match(await body.locator('.agents-stats').textContent(), /Accepts oftenyes/);
  assert.match(await body.locator('.agents-stats').textContent(), /Provisional30/);
  const outcomeRows = body.locator('.agents-role-panel-reviewers .agents-cause-row');
  assert.equal(await outcomeRows.count(), 3);
  assert.deepEqual(await outcomeRows.nth(1).locator('.agents-cause-seg').evaluateAll(nodes => nodes.map(node => node.dataset.outcome)),
    ['outcome_held_up', 'outcome_caught', 'outcome_false_alarm', 'outcome_missed', 'outcome_unresolved']);
  assert.equal(await outcomeRows.nth(0).locator('.agents-cause-total').textContent(), '64 reviews');
  pass('reviewers banner, tag, detail and outcomes');
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-reviewers-dark-1440');
  await body.locator('.agents-role-panel-reviewers').scrollIntoViewIfNeeded();
  await shot('agents-reviewers-outcomes-dark-1440');

  // Implementers uses the shared ranking/detail and shows no role panel.
  await page.locator('#agents-roles [data-view="implementers"]').click();
  await body.locator('.agents-row').first().waitFor();
  await body.locator('.agents-penalty .agents-task').first().waitFor();
  assert.equal(await body.locator('.agents-row').count(), 6);
  assert.equal(await body.locator('.agents-role-panel').count(), 0, 'no role panel and no empty container');
  assert.equal(await body.locator('.agents-cost-plot, .agents-cost-point, .agents-cost-frontier, .agents-cost-axis, .agents-cost-legend, .agents-tier-table').count(), 0);
  const implementersText = await body.textContent();
  assert.ok(!implementersText.includes('Score against cost') && !implementersText.includes('First reviews by tier'), 'no chart, frontier or tier copy');
  assert.equal(await body.evaluate(node => node.lastElementChild.classList.contains('agents-how')), true, 'nothing reserves space past the disclosure');
  assert.match(await body.locator('.agents-stats').textContent(), /Cost per weighted landed task/, 'cost stays in Shown, not scored');
  await shot('agents-implementers-dark-1440');
  const desktopRanking = await rankingVisibility();
  assert.deepEqual(desktopRanking.headersInside.filter(([, visible]) => !visible), [], `rank, crew · model and score are visible without scrolling at 1440px: ${JSON.stringify(desktopRanking)}`);
  assert.equal(desktopRanking.scoreInsideEveryRow, true, 'every row shows its score at 1440px');
  assert.equal(desktopRanking.scrolls, true, 'the evidence columns still scroll at 1440px');
  assert.equal(desktopRanking.cue, true, 'a scrolling ranking shows edge fades');
  await body.locator('.agents-ranking').screenshot({ path: path.join(evidence, 'agents-implementers-table-dark-1440.png'), animations: 'disabled' });
  await body.locator('.agents-row', { hasText: 'costly' }).locator('button').click();
  await page.waitForFunction(() => document.querySelector('#agents-detail h3')?.textContent === 'costly');
  await page.setViewportSize({ width: 375, height: 812 });
  await page.waitForTimeout(100);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.equal(await body.locator('.agents-role-panel').count(), 0, 'no role panel at 375px either');
  const phoneRanking = await rankingVisibility();
  assert.deepEqual(phoneRanking.headersInside.filter(([, visible]) => !visible), [], `rank, crew · model and score are visible at 375px: ${JSON.stringify(phoneRanking)}`);
  assert.equal(phoneRanking.scoreInsideEveryRow, true, 'every row shows its score at 375px');
  assert.equal(phoneRanking.pageOverflow, false);
  assert.equal(phoneRanking.cue, true, 'a scrolling ranking shows edge fades at 375px');
  await body.locator('.agents-ranking').screenshot({ path: path.join(evidence, 'agents-implementers-table-dark-375.png'), animations: 'disabled' });
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-implementers-dark-375');
  await page.setViewportSize({ width: 1440, height: 1100 });
  pass('implementers ranking and detail without a role panel at desktop and phone widths');

  // Pilots: the shared ranking and detail, then one tier matrix per row with
  // its diagonal and its too-easy and too-hard counts, at both widths.
  await page.locator('#agents-roles [data-view="pilots"]').click();
  await body.locator('.agents-tier-card').first().waitFor();
  await body.locator('.agents-penalty .agents-task').first().waitFor();
  assert.equal(new URL(page.url()).hash, '#agents/pilots?window=30d');
  assert.equal(await body.locator('.agents-row').count(), 3);
  assert.equal(await body.locator('#agents-detail h3').textContent(), 'luna');
  assert.equal(await body.locator('.agents-penalty').getAttribute('data-term'), 'lock_wait_hours');
  assert.doesNotMatch(await body.locator('.agents-stats').textContent(), /object Object/, 'the matrix is not a detail stat');
  assert.equal(await body.locator('.agents-tier-card').count(), 3);
  assert.equal(await body.locator('.agents-tier-matrix').count(), 2, 'a row with nothing placed has no matrix');
  assert.deepEqual(await body.locator('.agents-tier-card').first().locator('.agents-tier-cell.is-on').allTextContents(), ['3', '4', '2']);
  assert.deepEqual(await body.locator('.agents-tier-card').first().locator('.agents-tier-counts dd').allTextContents(), ['9', '2', '2', '1']);
  await shot('agents-pilots-dark-1440');
  await body.locator('.agents-role-panel h3').evaluate(node => node.scrollIntoView({ block: 'start' }));
  await page.evaluate(() => window.scrollBy(0, -80));
  await shot('agents-pilots-panel-dark-1440');
  await page.setViewportSize({ width: 375, height: 812 });
  await page.waitForTimeout(100);
  const pilotsPhone = await page.evaluate(() => ({
    pageOverflow: document.documentElement.scrollWidth > window.innerWidth,
    cardsInside: [...document.querySelectorAll('.agents-tier-card')].every(card => {
      const box = card.getBoundingClientRect();
      return box.left >= 0 && box.right <= window.innerWidth;
    }),
  }));
  assert.deepEqual(pilotsPhone, { pageOverflow: false, cardsInside: true }, 'pilots fit 375px');
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-pilots-dark-375');
  await body.locator('.agents-role-panel h3').evaluate(node => node.scrollIntoView({ block: 'start' }));
  await page.evaluate(() => window.scrollBy(0, -80));
  await shot('agents-pilots-panel-dark-375');
  await page.setViewportSize({ width: 1440, height: 1100 });
  pass('pilots ranking, detail and tier matrices at desktop and phone widths', pilotsPhone);

  // An empty window says so.
  await page.locator('#agents-roles [data-view="orchestrators"]').click();
  await page.locator('#agents-window-selector [data-window="1h"]').click();
  await body.locator('.agents-state-empty').waitFor();
  assert.match(await body.locator('.agents-state').textContent(), /No owned tasks in the 1h window/);
  assert.equal(new URL(page.url()).hash, '#agents/orchestrators?window=1h');
  await shot('agents-orchestrators-empty-dark-1440');
  await page.locator('#agents-window-selector [data-window="all"]').click();
  await body.locator('.agents-row').first().waitFor();
  assert.match(await page.locator('#agents-meta').textContent(), /in 90d \(all\)/, 'all is served and labelled as 90d');
  await page.locator('#agents-window-selector [data-window="30d"]').click();
  await body.locator('.agents-row').first().waitFor();
  pass('empty window and all as 90d');

  // Phone width: the page never scrolls sideways, the ranking scrolls inside
  // its own box, the detail stacks below it, and touch targets are 44px.
  await page.setViewportSize({ width: 375, height: 812 });
  await page.waitForTimeout(150);
  const phone = await page.evaluate(() => {
    const wrap = document.querySelector('.agents-table-wrap');
    const ranking = document.querySelector('.agents-ranking').getBoundingClientRect();
    const detail = document.querySelector('.agents-detail').getBoundingClientRect();
    const heights = selector => [...document.querySelectorAll(selector)].filter(node => node.getClientRects().length).map(node => node.getBoundingClientRect().height);
    return {
      pageOverflow: document.documentElement.scrollWidth > window.innerWidth,
      wrapOverflowX: getComputedStyle(wrap).overflowX,
      wrapScrolls: wrap.scrollWidth > wrap.clientWidth,
      detailBelow: detail.top >= ranking.bottom,
      detailWidth: detail.width,
      rowButtons: heights('.agents-row-select'),
      roleTabs: heights('#agents-roles .subtab'),
      legend: heights('a.agents-strip-entry'),
      summary: heights('.agents-how > summary'),
    };
  });
  assert.equal(phone.pageOverflow, false, `no page overflow at 375px: ${JSON.stringify(phone)}`);
  assert.equal(phone.wrapOverflowX, 'auto');
  assert.equal(phone.wrapScrolls, true, 'the ranking scrolls in its own box');
  assert.equal(phone.detailBelow, true, 'the detail column stacks under the ranking');
  for (const [name, list] of Object.entries({ rowButtons: phone.rowButtons, roleTabs: phone.roleTabs, legend: phone.legend, summary: phone.summary })) {
    assert.ok(list.length && list.every(height => height >= 44), `${name} are at least 44px: ${list}`);
  }
  pass('phone layout', phone);
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-orchestrators-dark-375');
  await page.locator('#agents-detail').scrollIntoViewIfNeeded();
  await shot('agents-orchestrators-detail-dark-375');
  await page.locator('#agents-roles [data-view="reviewers"]').click();
  await body.locator('.agents-banner').waitFor();
  await page.waitForTimeout(150);
  const reviewersPhone = await page.evaluate(() => ({
    pageOverflow: document.documentElement.scrollWidth > window.innerWidth,
    bannerFits: document.querySelector('.agents-banner').getBoundingClientRect().right <= window.innerWidth,
    outcomeBars: [...document.querySelectorAll('.agents-role-panel-reviewers .agents-cause-bar')].map(node => node.getBoundingClientRect().width),
  }));
  assert.equal(reviewersPhone.pageOverflow, false, `reviewers fit 375px: ${JSON.stringify(reviewersPhone)}`);
  assert.equal(reviewersPhone.bannerFits, true);
  assert.ok(reviewersPhone.outcomeBars.every(width => width > 200), `outcome bars take the row's width on a phone: ${reviewersPhone.outcomeBars}`);
  pass('reviewers phone layout', reviewersPhone);
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-reviewers-dark-375');
  await page.evaluate(() => document.querySelector('.agents-banner').scrollIntoView({ block: 'center' }));
  await shot('agents-reviewers-ranking-dark-375');
  await body.locator('.agents-role-panel-reviewers').scrollIntoViewIfNeeded();
  await shot('agents-reviewers-outcomes-dark-375');
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ ok: true, results }, null, 2));
  console.log(JSON.stringify({ ok: true, checks: results.map(result => result.name) }));
} finally {
  if (browser) await browser.close();
  server.close();
}
