// Usage: node dashboard_agents_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/agents-browser
// Analytics › Agents through the shipped app, router, renderer and CSS, fed
// contract-shaped `/api/agents` payloads: `#analytics/orchestrators` opening
// `#analytics/agents/orchestrators`, the role tabs and their counts, row selection
// driving scorecard detail, no detail API reads, the disclosure surviving a refresh, no
// Implementers role panel or the empty grid row and gap that track would
// reserve (16px at 1440, 12px at 375), the Reviewers provisional
// banner, `accepts often` tag, unjudged row and verdict-outcome bars, the Pilots
// tier-calibration matrices, the empty state, and the 375px layout (ranking
// scrolling in its own box, detail stacked below, 44px touch targets).
// Screenshots of each role tab (and the Implementers ranking) at 1440 and 375 go to the evidence directory.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';
import { implementerCard, implementerMatrix } from './agents_implementers_fixture.mjs';
import { pilotCard } from './agents_pilots_fixture.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const UNITS = { orchestrators: 'owned tasks', implementers: 'implemented tasks', reviewers: 'settled reviews', pilots: 'assessed tasks' };
const TERMS = {
  orchestrators: [
    ['landed', 'Landed', 1, 'credit', 'Landed tasks, one each whatever their complexity, so per 100 owned tasks the percent that landed.'],
    ['spec_defects', 'Spec defects', -1, 'problem', 'Description or criteria edited after the first run started (once per task), agent blocks naming the criteria, final-recovery escalations citing them, regressions with escape spec_ambiguous, and confirmed task-pilot holds on validation-tool, ADR-conflict, surface or utility findings. Pilot holds count at most once per task and add no charge when their confirming event already counts as a spec edit, criteria block or criteria escalation; prerequisite-only holds and unconfirmed warnings add none.'],
    ['spec_rework', 'Spec-caused rework', -1, 'problem', 'Repeat runs, [BLOCKED] or unmerged PRs, and operator requeues whose cause is spec.'],
  ],
  implementers: [
    ['weighted_landed', 'Landed', 1, 'credit', 'Landed tasks whose final implementation run is the row\'s.'],
    ['first_pass_accepts', 'First-pass accepts', 1, 'credit', 'Reviewed tasks whose first review verdict was accept.'],
    ['review_rejects', 'Review rejects', -1, 'problem', 'reject verdicts on the row\'s candidates.'],
    ['reviewer_repairs', 'Reviewer repairs', -0.5, 'problem', 'accept_with_fixes verdicts on the row\'s candidates.'],
    ['crew_rework', 'Crew-caused rework', -1, 'problem', 'Rework with cause crew.'],
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
function reviewerRow(crew, model, units, counts, provisional, [gate, jobs, accept, peers, often, minutes], outcomes, unjudged = false) {
  const per100 = Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, Math.round((count * 10000) / units) / 100]));
  const raw = unjudged ? null : Math.round(terms('reviewers').reduce((sum, term) => sum + term.weight * per100[term.key], 0) * 100) / 100;
  return {
    crew, model, units, below_support: units < 10, unjudged, rank: null, counts, per_100: per100, raw, score: raw, runs_without_model: 0, provisional,
    stats: [
      { key: 'gate_reviews', label: 'Review gate settlements', value: gate },
      { key: 'review_jobs', label: 'Review job runs that filed findings', value: jobs },
      { key: 'accept_rate', label: 'Accept rate (%)', value: accept },
      { key: 'peer_accept_rate', label: "Peers' accept rate on the same tiers (%)", value: peers },
      { key: 'accepts_often', label: 'Accepts often', value: often },
      ...OUTCOMES.map((key, index) => ({ key, label: ['Accepted and held up', 'Caught a real problem', 'False alarm', 'Missed a regression', 'Unresolved (not judged yet, incomplete, failed fixes or findings dropped unverified)'][index], value: outcomes[index] })),
      { key: 'median_review_minutes', label: 'Median review minutes', value: minutes },
    ],
  };
}
const REVIEWER_ROWS = [
  reviewerRow('codex-review', 'gpt-5.5-codex', 64, { findings_held: 118, fixes_held: 9, rejected_findings: 7, false_rejects: 3, failed_fixes: 1, missed_regressions: 1, incomplete_reviews: 2 }, 41, [60, 4, 55, 62, false, 11.5], [30, 22, 3, 1, 8]),
  reviewerRow('claude-review', 'claude-opus-5-5', 48, { findings_held: 46, fixes_held: 12, rejected_findings: 4, false_rejects: 1, failed_fixes: 2, missed_regressions: 3, incomplete_reviews: 1 }, 30, [48, 0, 81, 58, true, 8], [34, 8, 1, 3, 2]),
  reviewerRow('gemini-review', 'gemini-3-pro', 6, { findings_held: 6, fixes_held: 0, rejected_findings: 2, false_rejects: 1, failed_fixes: 0, missed_regressions: 0, incomplete_reviews: 0 }, 6, [6, 0, 33, 67, false, 14], [2, 1, 1, 0, 2]),
  // A review-job-only reviewer whose filed findings are all still open:
  // unjudged, so no score or rank, listed last, out of the fleet average.
  reviewerRow('sol-review', 'gpt-6.1-sol', 73, { findings_held: 0, fixes_held: 0, rejected_findings: 0, false_rejects: 0, failed_fixes: 0, missed_regressions: 0, incomplete_reviews: 0 }, 73, [0, 73, null, null, false, null], [0, 0, 0, 0, 73], true),
];
const JUDGED_REVIEWERS = REVIEWER_ROWS.filter(row => !row.unjudged);
const REVIEWER_FLEET = Math.round((JUDGED_REVIEWERS.reduce((sum, row) => sum + row.units * row.raw, 0) / JUDGED_REVIEWERS.reduce((sum, row) => sum + row.units, 0)) * 100) / 100;
for (const row of JUDGED_REVIEWERS) row.score = Math.round(Math.min(row.raw, (row.units * row.raw + 30 * REVIEWER_FLEET) / (row.units + 30)) * 100) / 100;
JUDGED_REVIEWERS.filter(row => !row.below_support).sort((a, b) => b.score - a.score).forEach((row, index) => { row.rank = index + 1; });
let computedAt = '2026-10-19T08:00:00.000Z';
const requests = [];
let matrixUnavailable = false;
const scorecardWindows = [];

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
  if (url.pathname === '/api/agents/implementers/by-type') return { ...implementerMatrix(), requested_window: url.searchParams.get('window') };
  const agents = url.pathname.match(/^\/api\/agents\/(\w+)\/(scorecard|detail)$/);
  if (agents) {
    requests.push(`${agents[1]}/${agents[2]}${agents[2] === 'detail' ? `:${url.searchParams.get('crew')}` : ''}`);
    if (agents[2] === 'scorecard') scorecardWindows.push({ role: agents[1], window: url.searchParams.get('window') });
    return agents[2] === 'scorecard' ? scorecard(agents[1], url) : {};
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
  if (url.pathname === '/api/agents/implementers/by-type' && matrixUnavailable) { res.writeHead(503); res.end('{}'); return; }
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

  // Space between the disclosure's border box and the body's padding edge.
  // Desktop keeps at least one gap for the detail column's trailing stretch
  // row; a taller detail column may extend beyond the disclosure. A
  // panel track with no panel adds a second gap: the 16px desktop gap, or
  // the 12px gap used at the phone breakpoint.
  const disclosureTail = () => page.evaluate(() => {
    const node = document.querySelector('#agents-body');
    const how = node.querySelector('.agents-how').getBoundingClientRect();
    const box = node.getBoundingClientRect();
    const style = getComputedStyle(node);
    const paddingBottom = parseFloat(style.paddingBottom);
    const rowGap = parseFloat(style.rowGap);
    const areas = style.gridTemplateAreas;
    const rowHeights = style.gridTemplateRows.split(/\s+/).filter(Boolean).map(value => parseFloat(value));
    const spaceAfterDisclosure = box.bottom - paddingBottom - how.bottom;
    const phone = window.innerWidth <= 900;
    const detail = node.querySelector('.agents-detail')?.getBoundingClientRect();
    const expectedSpace = phone ? 0 : Math.max(rowGap, (detail?.bottom ?? how.bottom) - how.bottom);
    return {
      width: window.innerWidth,
      areas,
      rowGap,
      paddingBottom,
      spaceAfterDisclosure,
      expectedSpace,
      excess: spaceAfterDisclosure - expectedSpace,
      zeroRows: rowHeights.filter(height => height < 1).length,
      rows: rowHeights.length,
      panelAreas: (areas.match(/panel/g) || []).length,
      phone,
    };
  });

  const assertNoEmptyPanelTrack = tail => {
    assert.equal(tail.panelAreas, 0, `implementers reserve no panel area: ${JSON.stringify(tail)}`);
    assert.equal(tail.rows, 4, `the layout has only strip, ranking, detail/disclosure and the desktop stretch track: ${JSON.stringify(tail)}`);
    assert.ok(Math.abs(tail.excess) <= 1,
      `an empty panel track adds the row gap after the disclosure (16px at desktop, 12px at 375px); measured excess ${tail.excess}px: ${JSON.stringify(tail)}`);
  };

  // A rendered panel occupies the row under the disclosure, one gap below it.
  // A banner, when that role has one, sits one gap under the strip.
  const assertPanelOccupiesItsRow = async label => {
    const placed = await page.evaluate(() => {
      const node = document.querySelector('#agents-body');
      const style = getComputedStyle(node);
      const how = node.querySelector('.agents-how').getBoundingClientRect();
      const panel = node.querySelector('.agents-role-panel');
      const panelBox = panel ? panel.getBoundingClientRect() : null;
      const banner = node.querySelector('.agents-banner');
      const strip = node.querySelector('.agents-strip').getBoundingClientRect();
      const ranking = node.querySelector('.agents-ranking').getBoundingClientRect();
      const bannerBox = banner ? banner.getBoundingClientRect() : null;
      return {
        areas: style.gridTemplateAreas,
        rowGap: parseFloat(style.rowGap),
        panelHeight: panelBox ? panelBox.height : 0,
        panelGap: panelBox ? panelBox.top - how.bottom : null,
        bannerGap: bannerBox ? bannerBox.top - strip.bottom : null,
        rankingGap: bannerBox ? ranking.top - bannerBox.bottom : null,
        width: window.innerWidth,
      };
    });
    assert.ok(placed.areas.includes('panel'), `${label} keeps a panel area: ${JSON.stringify(placed)}`);
    assert.ok(placed.panelHeight > 20, `${label} panel has content height: ${JSON.stringify(placed)}`);
    assert.ok(Math.abs(placed.panelGap - placed.rowGap) <= 1, `${label} panel sits one gap under the disclosure: ${JSON.stringify(placed)}`);
    if (placed.bannerGap != null) {
      assert.ok(Math.abs(placed.bannerGap - placed.rowGap) <= 1, `${label} banner sits one gap under the strip: ${JSON.stringify(placed)}`);
      assert.ok(Math.abs(placed.rankingGap - placed.rowGap) <= 1, `${label} ranking sits one gap under the banner: ${JSON.stringify(placed)}`);
    }
    return placed;
  };

  // The retired Analytics › Orchestrators link opens Analytics › Agents › Orchestrators.
  await page.goto(`${origin}#analytics/orchestrators?window=30d`);
  await body.locator('.agents-row').first().waitFor();
  // The panel opens on Last 500; the four choices are the only tabs.
  const agentWindows = await page.locator('#agents-window-selector [data-window]').evaluateAll(nodes => nodes.map(node => node.dataset.window));
  assert.deepEqual(agentWindows, ['last500', 'last1000', '7d', '30d']);
  assert.deepEqual(await page.locator('#agents-window-selector [data-window]').allTextContents(), ['Last 500', 'Last 1000', '7d', '30d']);
  assert.equal(await page.locator('#agents-window-selector .scoreboard-window-seg.on').getAttribute('data-window'), 'last500', 'Agents defaults to the newest 500 delivery attempts');
  await shot('agents-window-last500-dark-1440');
  await page.locator('#agents-window-selector [data-window="last1000"]').click();
  await page.waitForFunction(() => /in last1000/.test(document.getElementById('agents-meta').textContent));
  assert.equal(await page.locator('#agents-window-selector .scoreboard-window-seg.on').getAttribute('data-window'), 'last1000');
  assert.equal(scorecardWindows.filter(entry => entry.role === 'orchestrators').at(-1).window, 'last1000', 'Last 1000 loads its scorecard data');
  await page.locator('#agents-window-selector [data-window="30d"]').click();
  await page.waitForFunction(() => /in 30d/.test(document.getElementById('agents-meta').textContent));
  await page.evaluate(async () => { await document.fonts.ready; });
  assert.equal(new URL(page.url()).hash, '#analytics/agents/orchestrators?window=30d');
  assert.equal(await page.locator('.tab-pane.active').getAttribute('data-tab'), 'analytics');
  assert.equal(await page.locator('.rail .tab.active').getAttribute('data-tab'), 'analytics', 'Analytics is the active rail destination');
  assert.equal(await page.locator('.rail [data-tab="agents"]').count(), 0, 'the rail has no standalone Agents destination');
  assert.equal(await page.locator('#analytics-subtabs [data-subtab="agents"]').count(), 1, 'Analytics lists Agents');
  assert.equal(await page.locator('#analytics-subtabs [data-subtab="agents"]').evaluate(node => node.classList.contains('active')), true, 'Agents is the active Analytics view');
  pass('legacy route opens Agents');

  // Role tabs and their counts; the strip; the default selection.
  await page.waitForFunction(() => document.getElementById('agents-count-reviewers').textContent === '2 ranked');
  await page.waitForFunction(() => document.getElementById('agents-count-pilots').textContent === '2 ranked');
  const counts = await page.locator('#agents-roles .agents-role-count').allTextContents();
  assert.deepEqual(counts, ['3 ranked', '5 ranked', '2 ranked', '2 ranked'], 'each role tab labels its ranked-row count');
  assert.equal(await page.locator('#agents-count').textContent(), '4 rows · 3 ranked', 'the panel labels all rows and the ranked subset');
  assert.match(await body.locator('.agents-strip').textContent(), /131 problems in 30d/);
  assert.equal(await body.locator('.agents-strip-seg.is-current.agents-owner-orchestrators').count(), 1);
  const stripContext = body.locator('.agents-strip [role="note"]');
  assert.equal(await stripContext.count(), 1, 'ownership counts disclose unscored context');
  assert.equal(await stripContext.isVisible(), true, 'the explanation is visible beside the counts');
  assert.equal(await body.locator('.agents-strip').getAttribute('aria-describedby'), await stripContext.getAttribute('id'), 'assistive technology associates the counts with their explanation');
  assert.equal(await body.locator('.agents-row-select[aria-pressed="true"] .agents-crew').textContent(), 'claude-orchestrator');
  assert.equal(await body.locator('.agents-penalty').count(), 0, 'the detail column has no penalty section');
  assert.match(await body.locator('tr.agents-row-below').textContent(), /below 10/);
  pass('ranking, strip and default detail', { counts });
  await shot('agents-orchestrators-dark-1440');

  // Selecting a row moves the detail column to it.
  await body.locator('.agents-row', { hasText: 'codex-orchestrator' }).locator('.agents-row-select').click();
  await page.waitForFunction(() => document.querySelector('#agents-detail h3')?.textContent === 'codex-orchestrator');
  assert.equal(await body.locator('.agents-detail-rank').textContent(), '#2');
  assert.equal(await body.locator('.agents-penalty').count(), 0);
  assert.match(await body.locator('.agents-stats').textContent(), /Active hours41\.3/);
  assert.equal(await body.locator('.agents-row.is-selected .agents-crew').textContent(), 'codex-orchestrator');
  await body.locator('#agents-detail').screenshot({ path: path.join(evidence, 'agents-orchestrators-detail-dark-1440.png'), animations: 'disabled' });
  pass('selection updates the detail column');

  // The disclosure stays open across a refresh of the scorecard.
  await body.locator('.agents-how > summary').click();
  assert.equal(await body.locator('.agents-how').evaluate(node => node.open), true);
  assert.match(await body.locator('.agents-how').textContent(), /score = min\(raw, \(owned tasks × raw \+ 30 × fleet average\)/);
  computedAt = '2026-10-19T08:05:00.000Z';
  await page.locator('#refresh-btn').click();
  await page.waitForTimeout(100);
  assert.equal(await body.locator('.agents-penalty').count(), 0);
  assert.equal(await body.locator('.agents-how').evaluate(node => node.open), true, 'the disclosure keeps its state across a poll');
  assert.equal(await body.locator('.agents-row-select[aria-pressed="true"] .agents-crew').textContent(), 'codex-orchestrator', 'the selection survives a poll');
  await shot('agents-orchestrators-how-dark-1440');
  pass('disclosure survives a refresh', { panel: await assertPanelOccupiesItsRow('orchestrators') });

  // Reviewers: the provisional banner under the strip, the ranking with the
  // `accepts often` tag and the unjudged row last, the detail and each
  // reviewer's verdict outcomes.
  await page.locator('#agents-roles [data-view="reviewers"]').click();
  await body.locator('.agents-banner').waitFor();
  assert.equal(new URL(page.url()).hash, '#analytics/agents/reviewers?window=30d');
  assert.match(await body.locator('.agents-banner').textContent(), /Provisional\. 150 of 191 reviews in this window are younger than the 30-day miss lookback/);
  assert.deepEqual(await body.locator('.agents-row .agents-crew').allTextContents(), ['codex-review', 'claude-review', 'gemini-review', 'sol-review'], 'the unjudged row is listed after every other row');
  assert.deepEqual(await body.locator('.agents-row .agents-tag').allTextContents(), ['accepts often', 'unjudged']);
  const unjudged = body.locator('.agents-row', { hasText: 'sol-review' });
  assert.equal(await unjudged.evaluate(node => node.classList.contains('agents-row-unjudged') && node.classList.contains('agents-row-below')), true, 'the unjudged row is greyed');
  assert.equal(await unjudged.locator('.agents-rank').textContent(), '—', 'an unjudged row has no rank');
  assert.equal(await unjudged.locator('.agents-score-value').textContent(), 'unjudged', 'an unjudged row shows no numeric score');
  assert.equal(await unjudged.locator('.agents-bar').count(), 0, 'nor a score bar');
  assert.match(await page.locator('#agents-meta').textContent(), /unjudged rows are listed last, without a score/);
  assert.equal(await body.locator('.agents-row', { hasText: 'claude-review' }).locator('.agents-tag').count(), 1, 'the tag sits on the row accepting well above its peers');
  const order = await page.evaluate(() => {
    const top = selector => document.querySelector(selector).getBoundingClientRect();
    return { strip: top('.agents-strip').bottom, banner: top('.agents-banner').top, bannerBottom: top('.agents-banner').bottom, ranking: top('.agents-ranking').top, width: top('.agents-banner').width, body: top('#agents-body').width };
  });
  assert.ok(order.strip <= order.banner && order.bannerBottom <= order.ranking, `the banner sits between the strip and the ranking: ${JSON.stringify(order)}`);
  assert.ok(order.width > order.body * 0.9, 'the banner spans the body');
  await body.locator('.agents-row', { hasText: 'claude-review' }).locator('.agents-row-select').click();
  await page.waitForFunction(() => document.querySelector('#agents-detail h3')?.textContent === 'claude-review');
  assert.equal(await body.locator('.agents-penalty').count(), 0);
  assert.match(await body.locator('.agents-stats').textContent(), /Accepts oftenyes/);
  assert.match(await body.locator('.agents-stats').textContent(), /Provisional30/);
  const outcomeRows = body.locator('.agents-role-panel-reviewers .agents-cause-row');
  assert.equal(await outcomeRows.count(), 4);
  assert.equal(await outcomeRows.nth(3).evaluate(node => node.classList.contains('agents-row-below')), true, 'the unjudged row is greyed in the outcomes');
  assert.deepEqual(await outcomeRows.nth(1).locator('.agents-cause-seg').evaluateAll(nodes => nodes.map(node => node.dataset.outcome)),
    ['outcome_held_up', 'outcome_caught', 'outcome_false_alarm', 'outcome_missed', 'outcome_unresolved']);
  assert.equal(await outcomeRows.nth(0).locator('.agents-cause-total').textContent(), '64 reviews');
  pass('reviewers banner, tag, detail and outcomes', { panel: await assertPanelOccupiesItsRow('reviewers') });
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-reviewers-dark-1440');
  await body.locator('.agents-role-panel-reviewers').scrollIntoViewIfNeeded();
  await shot('agents-reviewers-outcomes-dark-1440');
  // The unjudged row's detail and the disclosure that explains it.
  await unjudged.locator('.agents-row-select').click();
  await page.waitForFunction(() => document.querySelector('#agents-detail h3')?.textContent === 'sol-review');
  assert.equal(await body.locator('.agents-detail-rank').textContent(), 'unjudged');
  assert.equal(await body.locator('.agents-detail-score').textContent(), '—no score until a unit is judged');
  assert.match(await body.locator('.agents-detail-raw').textContent(), /Not judged yet: no unit of this row has an outcome/);
  assert.match(await body.locator('.agents-radar-note').textContent(), /no unit of this row is judged yet/);
  if (!(await body.locator('.agents-how').evaluate(node => node.open))) await body.locator('.agents-how > summary').click();
  const how = await body.locator('.agents-how').textContent();
  assert.match(how, /Without a verdict its task's outcome judges it: a task that landed held/);
  assert.match(how, /is unjudged: it has no score or rank, is tagged unjudged, is listed after every other row, and stays out of the fleet average/);
  assert.match(how, /once its task lands, at the highest severity rated on its own regression links, else low/);
  pass('reviewers unjudged row');
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-reviewers-unjudged-dark-1440');
  await body.locator('.agents-how').scrollIntoViewIfNeeded();
  await body.locator('.agents-how').screenshot({ path: path.join(evidence, 'agents-reviewers-how-dark-1440.png'), animations: 'disabled' });

  // Implementers uses the shared ranking/detail and shows no role panel.
  await page.locator('#agents-roles [data-view="implementers"]').click();
  await body.locator('.agents-row').first().waitFor();
  assert.equal(await body.locator('.agents-penalty').count(), 0);
  assert.equal(await page.locator('#agents-count-implementers').textContent(), '5 ranked', 'the tab labels the ranked subset without hover');
  assert.equal(await page.locator('#agents-count').textContent(), '6 rows · 5 ranked', 'the panel labels all rows and the ranked subset');
  assert.equal(await body.locator('.agents-row').count(), 6);
  assert.equal(await body.locator('.agents-role-panel').count(), 0, 'no role panel and no empty container');
  assert.equal(await body.locator('.agents-cost-plot, .agents-cost-point, .agents-cost-frontier, .agents-cost-axis, .agents-cost-legend, .agents-tier-table').count(), 0);
  const implementersText = await body.textContent();
  assert.ok(!implementersText.includes('Score against cost') && !implementersText.includes('First reviews by tier'), 'no chart, frontier or tier copy');
  assert.equal(await body.evaluate(node => node.lastElementChild.classList.contains('agents-how')), true, 'nothing reserves space past the disclosure');
  assert.match(await body.locator('.agents-stats').textContent(), /Cost per landed task/, 'cost stays in Shown, not scored');
  for (const label of ['Escaped regressions', 'Regression residual', 'Diff economy']) assert.ok((await body.locator('.agents-stats').textContent()).includes(label));
  assert.equal(await body.locator('.agents-term-context').count(), implementerCard().terms.length);
  assert.equal(await body.locator('[data-term="weighted_landed"] .agents-term-context').textContent(), '22 actual · 19.1 expected');
  await body.locator('#agents-detail').screenshot({ path: path.join(evidence, 'agents-implementers-detail-dark-1440.png'), animations: 'disabled' });
  assert.equal(await body.locator('.agents-cost-note').count(), 0, 'complete cost coverage needs no unavailable-cost explanation');
  await body.locator('.agents-row', { hasText: 'unpriced' }).locator('button').click();
  await page.waitForFunction(() => document.querySelector('#agents-detail h3')?.textContent === 'unpriced');
  assert.match(await body.locator('.agents-cost-note').textContent(), /Cost per landed task is unavailable because 2 counted attempts have no run and 4 counted attempts have no priced cost/);
  assert.match(await body.locator('.agents-cost-note').textContent(), /Cost coverage: 18 measured of 24 counted attempts/);
  assert.match(await body.locator('.agents-stats').textContent(), /Cost coverage \(counted attempts\)18 measured · 4 unpriced · 2 missing run of 24 counted attempts/);
  await body.locator('#agents-detail').screenshot({ path: path.join(evidence, 'agents-implementers-cost-unavailable-dark-1440.png'), animations: 'disabled' });
  await body.locator('.agents-row', { hasText: 'sol' }).first().locator('button').click();
  await page.waitForFunction(() => document.querySelector('#agents-detail h3')?.textContent === 'sol');
  for (const tier of ['low', 'medium', 'hard', 'xhard']) {
    await page.locator('#agents-tier').selectOption(tier);
    const ranked = await body.locator('.agents-row .agents-model').allTextContents();
    assert.equal(ranked[0], ({ low: 'model-d', medium: 'model-c', hard: 'model-e', xhard: 'model-d' })[tier], 'tier ranking uses the tier crew mean rather than all-tier score');
    assert.equal(ranked.includes('model-e'), tier === 'hard', 'a row without work in a tier is hidden');
    assert.match(await body.locator('#agents-detail').textContent(), /All tiers score breakdown/);
  }
  await shot('agents-implementers-tier-dark-1440');
  await page.setViewportSize({ width: 375, height: 812 });
  await page.locator('#agents-tier').selectOption('low');
  await page.locator('#agents-tier').scrollIntoViewIfNeeded();
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await shot('agents-implementers-tier-dark-375');
  await page.locator('#agents-tier').selectOption('');
  assert.equal(await body.locator('.agents-row .agents-model').first().textContent(), 'model-b', 'All tiers restores the original tier-mix ranking');
  await page.setViewportSize({ width: 1440, height: 1100 });
  const implementersTails = [await disclosureTail()];
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-implementers-dark-1440');
  await body.locator('.agents-how').evaluate(node => node.scrollIntoView({ block: 'end' }));
  await shot('agents-implementers-disclosure-dark-1440');
  await page.evaluate(() => window.scrollTo(0, 0));
  const desktopRanking = await rankingVisibility();
  assert.deepEqual(desktopRanking.headersInside.filter(([, visible]) => !visible), [], `rank, crew · model and score are visible without scrolling at 1440px: ${JSON.stringify(desktopRanking)}`);
  assert.equal(desktopRanking.scoreInsideEveryRow, true, 'every row shows its score at 1440px');
  assert.equal(desktopRanking.pageOverflow, false, 'the reduced set of evidence columns fits or scrolls within the ranking at 1440px');
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
  await page.locator('#agents-roles').evaluate(node => node.scrollIntoView({ block: 'start' }));
  const phoneOverview = await page.evaluate(() => {
    const insideViewport = node => {
      const rect = node.getBoundingClientRect();
      return rect.top >= 0 && rect.bottom <= window.innerHeight;
    };
    return {
      roleTabsVisible: insideViewport(document.querySelector('#agents-roles')),
      panelHeaderVisible: insideViewport(document.querySelector('#agents-panel > header')),
    };
  });
  assert.deepEqual(phoneOverview, { roleTabsVisible: true, panelHeaderVisible: true }, 'the 375px overview shows the role counts and panel count together');
  await shot('agents-implementers-dark-375');
  await body.locator('#agents-detail').scrollIntoViewIfNeeded();
  await shot('agents-implementers-detail-dark-375');
  await body.locator('.agents-row', { hasText: 'unpriced' }).locator('button').click();
  await page.waitForFunction(() => document.querySelector('#agents-detail h3')?.textContent === 'unpriced');
  assert.match(await body.locator('.agents-cost-note').textContent(), /Cost coverage: 18 measured of 24 counted attempts/);
  await body.locator('#agents-detail').scrollIntoViewIfNeeded();
  await shot('agents-implementers-cost-unavailable-dark-375');
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.equal(await body.locator('.agents-term-context').count(), implementerCard().terms.length);
  implementersTails.push(await disclosureTail());
  await body.locator('.agents-how').evaluate(node => node.scrollIntoView({ block: 'end' }));
  await shot('agents-implementers-disclosure-dark-375');
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.setViewportSize({ width: 1440, height: 1100 });
  fs.writeFileSync(path.join(evidence, 'implementers-track.json'), JSON.stringify(implementersTails, null, 2));
  for (const tail of implementersTails) assertNoEmptyPanelTrack(tail);
  pass('implementers ranking and detail without a role panel at desktop and phone widths', { tails: implementersTails });

  // An unavailable optional matrix leaves the all-tier ranking usable.
  matrixUnavailable = true;
  await page.reload();
  await body.locator('.agents-row').first().waitFor();
  assert.equal(await page.locator('#agents-tier').isDisabled(), true);
  assert.match(await body.locator('.agents-tier-control').textContent(), /Tier scores unavailable/);
  assert.equal(await body.locator('.agents-row').count(), 6);
  await shot('agents-implementers-unavailable-dark-1440');
  await page.setViewportSize({ width: 375, height: 812 });
  await page.locator('#agents-tier').scrollIntoViewIfNeeded();
  await shot('agents-implementers-unavailable-dark-375');
  matrixUnavailable = false;
  await page.reload();
  await body.locator('.agents-row').first().waitFor();
  assert.equal(await page.locator('#agents-tier').isEnabled(), true);
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.locator('#agents-window-selector [data-window="30d"]').click();
  await page.waitForFunction(() => /in 30d/.test(document.getElementById('agents-meta').textContent));
  pass('implementer tier matrix failure keeps all tiers usable and recovers');

  // Pilots: the shared ranking and detail, then one tier matrix per row with
  // its diagonal and its too-easy and too-hard counts, at both widths.
  await page.locator('#agents-roles [data-view="pilots"]').click();
  await body.locator('.agents-tier-card').first().waitFor();
  assert.equal(await body.locator('.agents-penalty').count(), 0);
  assert.equal(await page.locator('#agents-count-pilots').textContent(), '2 ranked');
  assert.equal(await page.locator('#agents-count').textContent(), '3 rows · 2 ranked');
  assert.equal(new URL(page.url()).hash, '#analytics/agents/pilots?window=30d');
  assert.equal(await body.locator('.agents-row').count(), 3);
  assert.equal(await body.locator('#agents-detail h3').textContent(), 'luna');
  assert.doesNotMatch(await body.locator('.agents-stats').textContent(), /object Object/, 'the matrix is not a detail stat');
  assert.equal(await body.locator('.agents-tier-card').count(), 3);
  assert.equal(await body.locator('.agents-tier-matrix').count(), 2, 'a row with nothing placed has no matrix');
  assert.deepEqual(await body.locator('.agents-tier-card').first().locator('.agents-tier-cell.is-on').allTextContents(), ['3', '4', '2']);
  assert.deepEqual(await body.locator('.agents-tier-card').first().locator('.agents-tier-counts dd').allTextContents(), ['9', '2', '2', '1']);
  const pilotRow = crew => body.locator('.agents-row').filter({ has: page.locator('.agents-crew', { hasText: new RegExp(`^${crew}$`) }) });
  assert.equal(await pilotRow('luna').locator('[data-term="tier_calibration"]').textContent(), '69.2');
  assert.equal(await pilotRow('haiku').locator('[data-term="tier_calibration"]').textContent(), '50');
  assert.match(await pilotRow('haiku').locator('[data-term="tier_calibration"]').getAttribute('title'), /per 100 placed tasks.*provisional/);
  assert.equal(await body.locator('#agents-detail li[data-term="tier_calibration"] .agents-term-context').textContent(), '9 of 13 placed tasks');
  const pilotsDesktop = await assertPanelOccupiesItsRow('pilots');
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
  const pilotsPhonePanel = await assertPanelOccupiesItsRow('pilots at 375px');
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-pilots-dark-375');
  await body.locator('.agents-role-panel h3').evaluate(node => node.scrollIntoView({ block: 'start' }));
  await page.evaluate(() => window.scrollBy(0, -80));
  await shot('agents-pilots-panel-dark-375');
  await page.setViewportSize({ width: 1440, height: 1100 });
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: width === 375 ? 812 : 1100 });
    for (const [crew, support, name] of [['haiku', '4 of 8', 'low-support'], ['crew unknown', '0 of 0', 'unmeasured']]) {
      await pilotRow(crew).locator('.agents-row-select').click();
      const calibration = body.locator('#agents-detail li[data-term="tier_calibration"]');
      assert.equal(await calibration.locator('.agents-term-context').textContent(), `${support} placed tasks · provisional, not scored`);
      assert.equal(await calibration.locator('.agents-term-value').textContent(), '+0');
      await calibration.evaluate(node => node.scrollIntoView({ block: 'center' }));
      await shot(`agents-pilots-${name}-dark-${width}`);
    }
  }
  await page.setViewportSize({ width: 1440, height: 1100 });
  pass('pilots ranking, detail and tier matrices at desktop and phone widths', { pilotsPhone, pilotsDesktop, pilotsPhonePanel });

  // Unsupported shared windows keep Last 500 selected and load its rows.
  await page.locator('#agents-roles [data-view="orchestrators"]').click();
  await body.locator('.agents-row').first().waitFor();
  for (const sharedWindow of ['1h', '24h', 'all']) {
    await page.evaluate(async selected => {
      const common = await import('/js/common.js');
      const agents = await import('/js/agents.js');
      common.setWindow(selected);
      common.persistScopeToUrl();
      common.syncWindowSelectors();
      await agents.fetchAndRenderAgents();
    }, sharedWindow);
    assert.equal(await page.locator('#agents-window-selector .scoreboard-window-seg.on').getAttribute('data-window'), 'last500', `${sharedWindow} shows the Last 500 fallback`);
    assert.equal(scorecardWindows.filter(entry => entry.role === 'orchestrators').at(-1).window, 'last500', `${sharedWindow} loads Last 500 scorecard data`);
    assert.ok(await body.locator('.agents-row').count() > 0, `${sharedWindow} shows ranked data`);
  }
  await page.setViewportSize({ width: 375, height: 812 });
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-window-last500-dark-375');
  await page.evaluate(async () => {
    const common = await import('/js/common.js');
    const agents = await import('/js/agents.js');
    common.setWindow('30d');
    common.persistScopeToUrl();
    common.syncWindowSelectors();
    await agents.fetchAndRenderAgents();
  });
  await page.setViewportSize({ width: 1440, height: 1100 });
  pass('unsupported shared windows fall back to Last 500 and load data');

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
  pass('phone layout', { phone, panel: await assertPanelOccupiesItsRow('orchestrators at 375px') });
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-orchestrators-dark-375');
  await page.locator('#agents-detail .agents-detail-head').evaluate(node => window.scrollTo(0, node.getBoundingClientRect().top + window.scrollY - 140));
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
  pass('reviewers phone layout', { reviewersPhone, panel: await assertPanelOccupiesItsRow('reviewers at 375px') });
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot('agents-reviewers-dark-375');
  await page.evaluate(() => document.querySelector('.agents-banner').scrollIntoView({ block: 'center' }));
  await shot('agents-reviewers-ranking-dark-375');
  await body.locator('.agents-role-panel-reviewers').scrollIntoViewIfNeeded();
  await shot('agents-reviewers-outcomes-dark-375');
  await page.evaluate(() => document.querySelector('.agents-row-unjudged').scrollIntoView({ block: 'center' }));
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth), false, 'the unjudged row fits 375px');
  await shot('agents-reviewers-unjudged-dark-375');
  assert.deepEqual(errors, []);
  assert.equal(requests.some(entry => entry.includes('/detail')), false, 'the dashboard never requests agent detail');
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ ok: true, results }, null, 2));
  console.log(JSON.stringify({ ok: true, checks: results.map(result => result.name) }));
} finally {
  if (browser) await browser.close();
  server.close();
}
