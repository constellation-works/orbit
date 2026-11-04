import assert from 'node:assert/strict';
import { agents, common, answer, takeAll, response, settle, part, click, selectButton, byId, notComputed, attribution } from './dashboard_agents.mjs';
import { implementerCard, implementerRadarCard, implementerOutlierCard, implementerMatrix } from './agents_implementers_fixture.mjs';

common.setWorkspace('ws-a');
common.setWindow('30d');
agents.setAgentsRole('implementers');
const loading = agents.fetchAndRenderAgents();
// Size curves: one default series (one bucket sparse, one under the minimum
// sample) and one series below the default threshold.
const curveBucket = (bucket, tasks, regressed, low, high) => ({
  bucket, lines_from: 2 ** bucket - 1, lines_to: 2 ** (bucket + 1) - 2, tasks, regressed,
  rate: regressed / tasks, low, high, drawn: tasks >= 10, sparse: regressed < 5,
});
const sizeCurves = url => ({
  window: '30d', requested_window: url.searchParams.get('window'), role: 'implementers', complexity: url.searchParams.get('complexity'),
  complexities: [{ tier: 'medium', tasks: 44 }], follow_up_days: 7, min_bucket_tasks: 10, min_series_tasks: 30, sparse_outcomes: 5,
  excluded: { immature: 3, no_size: 1 },
  series: [
    { crew: 'opus@medium', models: ['model-b'], tasks: 38, regressed: 9, default_selected: true,
      buckets: [curveBucket(5, 12, 1, 0.0149, 0.3536), curveBucket(6, 20, 6, 0.1455, 0.519), curveBucket(7, 6, 2, 0.0968, 0.7)] },
    { crew: 'opus@high', models: ['model-b'], tasks: 6, regressed: 1, default_selected: false, buckets: [curveBucket(8, 6, 1, 0.0301, 0.5635)] },
  ],
});
const executions = new Set();
answer(takeAll(), url => {
  if (url.pathname.includes('/implementers/')) executions.add(url.searchParams.get('execution'));
  if (url.pathname.endsWith('/size-curves')) return response(sizeCurves(url));
  if (url.pathname.endsWith('/by-type')) return response(implementerMatrix());
  if (url.pathname.endsWith('/attribution')) return response(attribution);
  const role = url.pathname.split('/')[3];
  return response(role === 'implementers' ? implementerCard() : notComputed(role));
});
await loading;
await settle();
assert.equal(byId['agents-count-implementers'].textContent, '5 ranked');
assert.equal(byId['agents-count'].textContent, '6 rows · 5 ranked');
assert.equal(part('ranking').querySelectorAll('tr.agents-row').length, 6);
assert.equal(part('detail').querySelector('h3').textContent, 'sol');
const body = byId['agents-body'];
assert.deepEqual(body.children.map(node => node.dataset.key).filter(Boolean), ['strip', 'ranking', 'detail', 'how', 'panel', 'execution']);
assert.deepEqual([...executions], ['managed'], 'implementer scorecard, matrix and curves requests name the managed default');
assert.equal(byId['agents-execution-control'].hidden, false, 'the execution control shows for implementers');
assert.match(part('execution').textContent, /No interactive work/, 'a window without interactive work says so');
const curves = part('panel');
// SVG classes are attributes in this DOM; series groups carry `data-crew`.
const svgClass = node => node.getAttribute('class') || '';
assert.deepEqual(curves.querySelectorAll('g').map(node => node.dataset.crew).filter(Boolean), ['opus@medium'], 'only default-selected crews are drawn');
const markers = curves.querySelectorAll('circle');
assert.equal(markers.length, 2, 'a bucket under the minimum sample is not drawn');
assert.equal(markers.filter(node => svgClass(node).includes('is-sparse')).length, 1, 'a bucket with fewer than five regressed tasks is hollow');
for (const selector of ['.agents-cost-plot', '.agents-cost-point', '.agents-cost-frontier', '.agents-cost-axis', '.agents-cost-legend', '.agents-tier-table']) {
  assert.equal(body.querySelectorAll(selector).length, 0, `the removed panel leaves no ${selector}`);
}
assert.ok(!body.textContent.includes('Score against cost') && !body.textContent.includes('First reviews by tier'), 'the removed panel leaves no chart, frontier or tier copy');
assert.match(part('detail').textContent, /Cost per landed task/, 'cost stays in Shown, not scored');
assert.deepEqual(takeAll(), [], 'the dashboard does not fetch agent detail');
assert.match(part('detail').textContent, /model-b/);
const shown = part('detail').querySelectorAll('.agents-term-context');
assert.equal(shown.length, implementerCard().terms.length + 3, 'every scored term shows its reference count and review terms name their denominator');
assert.equal(shown[0].textContent, '22 actual · 19.1 expected');
const measuredZero = part('ranking').querySelectorAll('tr.agents-row').find(node => node.textContent.includes('small-sample')).querySelectorAll('td').find(node => node.dataset.term === 'reviewer_repairs');
assert.equal(measuredZero.textContent, '0', 'observed zero repairs remains a measured zero');
const rejects = part('detail').querySelectorAll('.agents-breakdown li').find(node => node.dataset.term === 'review_rejects');
assert.equal(rejects.querySelector('.agents-term-value').textContent, '−2.7', 'the bar uses excess rejects, rather than total rejects');
assert.ok(!part('detail').textContent.includes('[object Object]'), 'structured tier data never leaks into the detail column');
click(selectButton('costly'));
assert.deepEqual(takeAll(), [], 'selecting another row does not request agent detail');
await settle();
assert.equal(part('detail').querySelector('h3').textContent, 'costly');
assert.equal(part('detail').querySelector('.agents-cost-note'), null, 'complete cost coverage needs no unavailable-cost explanation');
assert.match(part('detail').textContent, /Cost coverage \(counted attempts\)24 measured · 0 unpriced · 0 missing run of 24 counted attempts/);
click(selectButton('unpriced'));
await settle();
assert.equal(part('detail').querySelector('h3').textContent, 'unpriced');
assert.match(part('detail').querySelector('.agents-cost-note').textContent, /Cost per landed task is unavailable because 2 counted attempts have no run and 4 counted attempts have no priced cost/);
assert.match(part('detail').querySelector('.agents-cost-note').textContent, /Cost coverage: 18 measured of 24 counted attempts/);
assert.match(part('detail').querySelector('.agents-cost-note').textContent, /not averaged over priced attempts alone/);
assert.match(part('detail').textContent, /Cost coverage denominator \(counted attempts\)24/);
click(selectButton('small-sample'));
await settle();
assert.equal(part('detail').querySelector('.agents-cost-note'), null, 'a valid zero cost is displayed, not explained as unavailable');
assert.match(part('detail').textContent, /Cost per landed task \(USD\)0/);

// Hold real dashboard reads without timing races. Retired reads must never
// replace the latest curves, even when the complexity returns to the same URL.
const holdRefreshCurves = () => {
  const loading = agents.fetchAndRenderAgents();
  const requests = takeAll();
  const curve = requests.find(request => request.url.pathname.endsWith('/size-curves'));
  assert.ok(curve, 'refresh reads size curves beside the scorecard');
  answer(requests.filter(request => request !== curve), url => {
    if (url.pathname.endsWith('/by-type')) return response(implementerMatrix());
    if (url.pathname.endsWith('/attribution')) return response(attribution);
    return response(url.pathname.includes('/implementers/') ? implementerCard() : notComputed(url.pathname.split('/')[3]));
  });
  return { curve, loading };
};
const holdComplexityCurves = tier => {
  const select = part('panel').querySelector('select');
  select.value = tier;
  for (const listener of select.listeners.change) listener();
  const requests = takeAll();
  assert.equal(requests.length, 1, 'the complexity filter reads only size curves');
  assert.equal(requests[0].url.searchParams.get('complexity') || '', tier);
  return { curve: requests[0], loading: Promise.resolve() };
};
const assertLatestCurves = label => {
  const cohort = part('panel').querySelector('.agents-curves-cohort');
  assert.ok(cohort, `${label}: the curves remain available`);
  assert.match(cohort.textContent, /56 landed tasks/, label);
  const bucket = part('panel').querySelector('.agents-curves-table tbody tr');
  assert.equal(bucket.querySelectorAll('td')[1].textContent, '50', 'the bucket table retains the latest 50-task bucket');
  const markers = part('panel').querySelectorAll('circle');
  assert.equal(markers.length, 1, 'the chart retains only the latest bucket');
  assert.match(markers[0].textContent, /10 of 50 regressed/, 'the chart retains the latest outcomes');
};
for (const source of ['refresh', 'complexity']) {
  for (const olderFails of [false, true]) {
    const older = source === 'refresh' ? holdRefreshCurves() : holdComplexityCurves('medium');
    const middle = source === 'complexity' ? holdComplexityCurves('hard') : null;
    const newer = source === 'refresh' ? holdRefreshCurves() : holdComplexityCurves('medium');
    assert.equal(older.curve.url.href, newer.curve.url.href, 'the retired and latest reads have identical URLs');
    const latest = sizeCurves(newer.curve.url);
    latest.series[0].tasks = 50;
    latest.series[0].regressed = 10;
    latest.complexities[0].tasks = 56;
    latest.series[0].buckets = [curveBucket(5, 50, 10, 0.1, 0.3)];
    newer.curve.resolve(response(latest));
    await newer.loading;
    await settle();
    assertLatestCurves('the last-started read is visible before the older read settles');
    older.curve.resolve(olderFails ? response({ error: 'retired read failed' }, 500) : response(sizeCurves(older.curve.url)));
    if (middle) middle.curve.resolve(response(sizeCurves(middle.curve.url)));
    await older.loading;
    await settle();
    // Scorecard requestPanel already discards the older render. Force local
    // row and picker redraws too, which consume the panel-global curve cache.
    const label = `${source}: older ${olderFails ? 'failure' : 'success'} cannot replace the newer curves`;
    click(selectButton(olderFails ? 'costly' : 'sol'));
    await settle();
    assertLatestCurves(`${label} after a row redraw`);
    const picker = part('panel').querySelector('input');
    picker.checked = true;
    for (const listener of picker.listeners.change) listener();
    await settle();
    assertLatestCurves(`${label} after a picker redraw`);
  }
}
// Restore all complexities for the existing profile checks below.
const resetCurves = holdComplexityCurves('');
resetCurves.curve.resolve(response(sizeCurves(resetCurves.curve.url)));
await settle();

// --- Agent profile radar: percentile scale, reviewed gaps, contributions ---
const radarVertex = (term) => part('detail').querySelectorAll('g').find(node => node.dataset.term === term);
const hasClass = (node, name) => String(node.getAttribute('class') || node.className || '').split(/\s+/).includes(name);
const radarRadius = (term) => {
  const circle = radarVertex(term).children[0];
  return Math.hypot(Number(circle.getAttribute('cx')) - 150, Number(circle.getAttribute('cy')) - 130);
};
const radarTableRow = (term) => part('detail').querySelectorAll('tr').find(node => node.dataset.term === term);
const fleetRadii = () => {
  const polygon = part('detail').querySelectorAll('polygon').find(node => hasClass(node, 'agents-radar-fleet'));
  assert.ok(polygon, 'the fleet median polygon is drawn');
  return polygon.getAttribute('points').split(' ').map(pair => {
    const [x, y] = pair.split(',').map(Number);
    return Math.hypot(x - 150, y - 130);
  });
};
const loadCard = async (card) => {
  const load = agents.fetchAndRenderAgents();
  answer(takeAll(), url => {
    if (url.pathname.endsWith('/by-type')) return response(implementerMatrix(card));
    if (url.pathname.endsWith('/attribution')) return response(attribution);
    const role = url.pathname.split('/')[3];
    return response(role === 'implementers' ? card : notComputed(role));
  });
  await load;
  await settle();
  assert.deepEqual(takeAll(), [], 'loading a scorecard does not request agent detail');
};
const selectCrew = async (crew) => {
  click(selectButton(crew));
  assert.deepEqual(takeAll(), [], 'selecting a crew does not request agent detail');
  await settle();
};

// Live-shaped fleet: rank-2 sol, above the median on landed and escaped
// regressions, with no reviewed deliveries.
const radarCard = implementerRadarCard();
await loadCard(radarCard);
await selectCrew('sol');
assert.equal(part('detail').querySelector('h3').textContent, 'sol');
for (const radius of fleetRadii()) assert.ok(Math.abs(radius - 52) < 0.01, `the fleet median sits on the middle ring, got ${radius}`);
assert.ok(radarRadius('weighted_landed') > 52, 'a rank-2 row above the landed median plots outside the median ring');
for (const term of ['first_pass_accepts', 'review_rejects', 'reviewer_repairs']) {
  const rankingRow = part('ranking').querySelectorAll('tr.agents-row').find(node => node.textContent.includes('sol'));
  const cell = rankingRow.querySelectorAll('td').find(node => node.dataset.term === term);
  assert.equal(cell.textContent, 'no reviewed deliveries', `${term} table cell distinguishes missing reviews from zero`);
  const breakdown = part('detail').querySelectorAll('.agents-breakdown li').find(node => node.dataset.term === term);
  assert.equal(breakdown.querySelector('.agents-term-value').textContent, '+0', `${term} without reviews contributes nothing`);
  assert.ok(hasClass(radarVertex(term), 'is-gap'), `${term} with no reviewed deliveries is a gap`);
  assert.match(radarVertex(term).getAttribute('aria-label'), /no reviewed deliveries/, `${term} gap names its reason`);
  assert.match(radarTableRow(term).textContent, /no reviewed deliveries/, `${term} accessible row names the gap reason`);
}
assert.match(radarVertex('weighted_landed').getAttribute('aria-label'), /weight \+1; contribution vs fleet median \+20/, 'the landed vertex discloses weight and signed contribution vs the median');
assert.match(radarTableRow('weighted_landed').textContent, /\+20/, 'the accessible table carries the landed contribution vs the median');

// Adding one extreme outlier crew must not move the median ring or squeeze
// a normal crew toward the rim.
await selectCrew('peer-a');
const firstPassBreakdown = part('detail').querySelectorAll('.agents-breakdown li').find(node => node.dataset.term === 'first_pass_accepts');
assert.equal(firstPassBreakdown.querySelector('.agents-term-value').textContent, '+5.4', 'review contribution uses expected counts per reviewed delivery');
const peerLandedBefore = radarRadius('weighted_landed');
const outlierCard = implementerOutlierCard();
await loadCard(outlierCard);
await selectCrew('peer-a');
for (const radius of fleetRadii()) assert.ok(Math.abs(radius - 52) < 0.01, `the outlier does not move the fleet median ring, got ${radius}`);
assert.ok(radarRadius('weighted_landed') < 78 && Math.abs(radarRadius('weighted_landed') - peerLandedBefore) < 26, 'the outlier does not move a normal crew toward the rim on landed');
assert.ok(!hasClass(radarVertex('first_pass_accepts'), 'is-gap'), 'a reviewed crew keeps its first-pass vertex');
console.log('dashboard implementers ok');
