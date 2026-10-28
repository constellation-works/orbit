import assert from 'node:assert/strict';
import { agents, common, answer, takeAll, response, settle, part, click, selectButton, byId, notComputed, attribution } from './dashboard_agents.mjs';
import { implementerCard, implementerRadarCard, implementerOutlierCard } from './agents_implementers_fixture.mjs';

common.setWorkspace('ws-a');
common.setWindow('30d');
agents.setAgentsRole('implementers');
const loading = agents.fetchAndRenderAgents();
answer(takeAll(), url => {
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
assert.equal(part('panel'), undefined, 'implementers show no role panel');
assert.equal(body.querySelectorAll('.agents-role-panel').length, 0, 'no empty role-panel container is left behind');
assert.deepEqual(body.children.map(node => node.dataset.key).filter(Boolean), ['strip', 'ranking', 'detail', 'how']);
for (const selector of ['.agents-cost-plot', '.agents-cost-point', '.agents-cost-frontier', '.agents-cost-axis', '.agents-cost-legend', '.agents-tier-table']) {
  assert.equal(body.querySelectorAll(selector).length, 0, `the removed panel leaves no ${selector}`);
}
assert.ok(!body.textContent.includes('Score against cost') && !body.textContent.includes('First reviews by tier'), 'the removed panel leaves no chart, frontier or tier copy');
assert.match(part('detail').textContent, /Cost per landed task/, 'cost stays in Shown, not scored');
assert.deepEqual(takeAll(), [], 'the dashboard does not fetch agent detail');
assert.match(part('detail').textContent, /model-b/);
const shown = part('detail').querySelectorAll('.agents-term-context');
assert.equal(shown.length, implementerCard().terms.length, 'every scored term shows its reference count');
assert.equal(shown[0].textContent, '22 actual · 19.1 expected');
const rejects = part('detail').querySelectorAll('.agents-breakdown li').find(node => node.dataset.term === 'review_rejects');
assert.equal(rejects.querySelector('.agents-term-value').textContent, '−2.7', 'the bar uses excess rejects, rather than total rejects');
assert.ok(!part('detail').textContent.includes('[object Object]'), 'structured tier data never leaks into the detail column');
click(selectButton('costly'));
assert.deepEqual(takeAll(), [], 'selecting another row does not request agent detail');
await settle();
assert.equal(part('detail').querySelector('h3').textContent, 'costly');

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
assert.ok(radarRadius('escaped_regressions') > 52, 'a rank-2 row better than the escaped-regression median plots outside the median ring');
for (const term of ['first_pass_accepts', 'review_rejects', 'reviewer_repairs']) {
  assert.ok(hasClass(radarVertex(term), 'is-gap'), `${term} with no reviewed deliveries is a gap`);
  assert.match(radarVertex(term).getAttribute('aria-label'), /no reviewed deliveries/, `${term} gap names its reason`);
  assert.match(radarTableRow(term).textContent, /no reviewed deliveries/, `${term} accessible row names the gap reason`);
}
assert.match(radarVertex('weighted_landed').getAttribute('aria-label'), /weight \+1; contribution vs fleet median \+20/, 'the landed vertex discloses weight and signed contribution vs the median');
assert.match(radarVertex('escaped_regressions').getAttribute('aria-label'), /weight −2; contribution vs fleet median \+10/, 'the escaped vertex discloses weight and signed contribution vs the median');
assert.match(radarTableRow('weighted_landed').textContent, /\+20/, 'the accessible table carries the landed contribution vs the median');
assert.match(radarTableRow('escaped_regressions').textContent, /\+10/, 'the accessible table carries the escaped contribution vs the median');

// Adding one extreme outlier crew must not move the median ring or squeeze
// a normal crew toward the rim.
await selectCrew('peer-a');
const peerLandedBefore = radarRadius('weighted_landed');
const peerEscapedBefore = radarRadius('escaped_regressions');
const outlierCard = implementerOutlierCard();
await loadCard(outlierCard);
await selectCrew('peer-a');
for (const radius of fleetRadii()) assert.ok(Math.abs(radius - 52) < 0.01, `the outlier does not move the fleet median ring, got ${radius}`);
assert.ok(radarRadius('weighted_landed') < 78 && Math.abs(radarRadius('weighted_landed') - peerLandedBefore) < 26, 'the outlier does not move a normal crew toward the rim on landed');
assert.ok(radarRadius('escaped_regressions') < 78 && Math.abs(radarRadius('escaped_regressions') - peerEscapedBefore) < 26, 'the outlier does not squeeze a normal crew on escaped regressions');
assert.ok(!hasClass(radarVertex('first_pass_accepts'), 'is-gap'), 'a reviewed crew keeps its first-pass vertex');
console.log('dashboard implementers ok');
