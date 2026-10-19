// Observe › Agents › Pilots through contract-shaped payloads: the ranking,
// the detail column (structured stats left to the panel, the biggest
// penalty's tasks), and the tier-calibration panel with one assessed-by-
// observed matrix per row, its diagonal and its too-easy and too-hard counts.
import assert from 'node:assert/strict';
import { agents, common, answer, takeAll, response, settle, part, click, selectButton, byId, notComputed, attribution } from './dashboard_agents.mjs';
import { pilotCard, pilotDetail } from './agents_pilots_fixture.mjs';

common.setWorkspace('ws-a');
common.setWindow('30d');
agents.setAgentsRole('pilots');
const loading = agents.fetchAndRenderAgents();
answer(takeAll(), url => {
  if (url.pathname.endsWith('/attribution')) return response(attribution);
  const role = url.pathname.split('/')[3];
  return response(role === 'pilots' ? pilotCard() : notComputed(role));
});
await loading;
await settle();
assert.equal(byId['agents-count-pilots'].textContent, '2', 'two ranked crews; the below-support row is not counted');
assert.equal(part('ranking').querySelectorAll('tr.agents-row').length, 3);
assert.equal(part('detail').querySelector('h3').textContent, 'luna');

assert.deepEqual(part('detail').querySelectorAll('span.agents-term-weight').map(node => node.textContent), ['× +1', '× +0.25', '× −1', '× −1'], 'a fractional weight reads as configured');

const panel = part('panel');
const cards = panel.querySelectorAll('article.agents-tier-card');
assert.deepEqual(cards.map(card => card.dataset.row), ['luna\0luna-model', 'haiku\0claude-haiku-5-5', 'crew unknown\0model unknown']);
const cells = card => card.querySelectorAll('td.agents-tier-cell');
const grid = card => card.querySelectorAll('tbody tr').map(line => line.querySelectorAll('td').map(cell => Number(cell.textContent)));
assert.deepEqual(grid(cards[0]), [[3, 1, 0], [1, 4, 1], [0, 1, 2]], 'rows are assessed tiers, columns observed tiers');
assert.deepEqual(cards[0].querySelectorAll('thead th').slice(1).map(cell => cell.textContent), ['Low', 'Medium', 'Hard']);
assert.deepEqual(cells(cards[0]).filter(cell => cell.classList.contains('is-on')).map(cell => cell.dataset.assessed), ['low', 'medium', 'hard'], 'the diagonal is on tier');
assert.deepEqual(cells(cards[0]).filter(cell => cell.classList.contains('is-easy')).map(cell => [cell.dataset.assessed, cell.dataset.observed]),
  [['low', 'medium'], ['low', 'hard'], ['medium', 'hard']], 'above the diagonal the pilot assessed too easy');
const counts = card => Object.fromEntries(card.querySelectorAll('dd').map(dd => [dd.dataset.count, dd.textContent]));
assert.deepEqual(counts(cards[0]), { on: '9', easy: '2', hard: '2', unplaced: '1' });
assert.deepEqual(counts(cards[1]), { on: '4', easy: '3', hard: '3', unplaced: '2' });
assert.equal(cards[2].querySelector('table'), null, 'a row with nothing placed has no matrix');
assert.ok(cards[2].classList.contains('agents-row-below'), 'a below-support row is greyed');
assert.match(cards[2].querySelector('p.agents-muted').textContent, /No assessed task/);

for (const request of takeAll()) request.resolve(response(pilotDetail('luna', 'luna-model')));
await settle();
const detail = part('detail');
assert.ok(!detail.textContent.includes('[object Object]'), 'the tier matrix is left to the panel');
const stats = Object.fromEntries(detail.querySelectorAll('dt').map((dt, at) => [dt.textContent, detail.querySelectorAll('dd')[at].textContent]));
assert.equal(stats['Recall (%)'], '88.9', 'rounded as every agents number is');
assert.equal(stats['Cost per assessment (USD)'], '—', 'an unpriced cost is no number');
assert.equal(detail.querySelector('section.agents-penalty').dataset.term, 'lock_wait_hours');
assert.deepEqual(detail.querySelectorAll('li.agents-task').map(item => item.dataset.taskId), ['ORB-41'], 'the wait is listed under its holder');

click(selectButton('haiku'));
const requests = takeAll();
assert.equal(requests[0].url.pathname, '/api/agents/pilots/detail');
assert.equal(requests[0].url.searchParams.get('model'), 'claude-haiku-5-5');
requests[0].resolve(response(pilotDetail('haiku', 'claude-haiku-5-5')));
await settle();
assert.equal(part('detail').querySelector('h3').textContent, 'haiku');
assert.equal(part('detail').querySelector('section.agents-penalty').dataset.term, 'costly_misses');
console.log('dashboard pilots ok');
