import assert from 'node:assert/strict';
import { agents, common, answer, takeAll, response, settle, part, click, selectButton, byId, notComputed, attribution } from './dashboard_agents.mjs';
import { implementerCard, implementerDetail } from './agents_implementers_fixture.mjs';

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
assert.equal(byId['agents-count-implementers'].textContent, '5');
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
for (const request of takeAll()) request.resolve(response(implementerDetail('sol', 'model-b')));
await settle();
assert.match(part('detail').textContent, /model-b/);
const shown = part('detail').querySelectorAll('.agents-term-context');
assert.equal(shown.length, implementerCard().terms.length, 'every scored term shows its reference count');
assert.equal(shown[0].textContent, '22 actual · 19.1 expected');
const rejects = part('detail').querySelectorAll('.agents-breakdown li').find(node => node.dataset.term === 'review_rejects');
assert.equal(rejects.querySelector('.agents-term-value').textContent, '−2.7', 'the bar uses excess rejects, rather than total rejects');
assert.ok(!part('detail').textContent.includes('[object Object]'), 'structured tier data never leaks into the detail column');
click(selectButton('costly'));
const requests = takeAll();
assert.equal(requests[0].url.searchParams.get('model'), 'model-d');
requests[0].resolve(response(implementerDetail('costly', 'model-d')));
await settle();
assert.equal(part('detail').querySelector('h3').textContent, 'costly');
console.log('dashboard implementers ok');
