// Observe › Agents › Pilots through contract-shaped payloads: the ranking,
// the detail column (structured stats left to the panel, the biggest
// penalty's tasks), and the tier-calibration panel with one assessed-by-
// observed matrix per row, its diagonal and its too-easy and too-hard counts.
// The warning terms are rated per 100 settled assessments, provisional ones
// read as such and add nothing, and an unmeasured missed-scope-block source
// gets a banner instead of passing for no misses.
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
assert.equal(byId['agents-count-pilots'].textContent, '2 ranked', 'the tab names its ranked-crew count');
assert.equal(byId['agents-count'].textContent, '3 rows · 2 ranked', 'the panel names all rows and the ranked subset');
assert.equal(part('ranking').querySelectorAll('tr.agents-row').length, 3);
assert.equal(part('detail').querySelector('h3').textContent, 'luna');

assert.deepEqual(part('detail').querySelectorAll('span.agents-term-weight').map(node => node.textContent), ['× +1', '× +0.25', '× +3', '× −1', '× −3', '× −1', '× −1'], 'a fractional weight reads as configured');

const banner = part('banner');
assert.ok(banner, 'an unmeasured missed-scope-block source has a banner');
assert.equal(banner.querySelector('p').dataset.source, 'missed_scope_blocks');
assert.match(banner.textContent, /Missed scope blocks unmeasured\. Unavailable: no blocked-task triage record/);

const headTitle = key => part('ranking').querySelectorAll('thead th').find(th => th.textContent === key).title;
assert.match(headTitle('Confirmed warnings'), /^Confirmed warnings per 100 settled assessments \(weight \+3\)/, 'a warning term names its own unit');
assert.match(headTitle('F1 lift'), /per 100 assessed tasks/);
const rowNamed = crew => part('ranking').querySelectorAll('tr.agents-row').find(row => row.dataset.row.startsWith(`${crew}\0`));
const cell = (crew, key) => rowNamed(crew).querySelectorAll('td').find(td => td.dataset.term === key);
assert.equal(cell('luna', 'confirmed_warnings').textContent, '40', 'eight of 20 settled assessments');
assert.ok(!cell('luna', 'confirmed_warnings').classList.contains('agents-term-provisional'));
assert.ok(cell('haiku', 'confirmed_warnings').classList.contains('agents-term-provisional'), 'six settled assessments are provisional');
assert.match(cell('haiku', 'missed_scope_blocks').title, /provisional, too few settled assessments to score/);
const breakdown = () => Object.fromEntries(part('detail').querySelectorAll('ul.agents-breakdown li').map(item => [item.dataset.term, item]));
assert.equal(breakdown().confirmed_warnings.querySelector('span.agents-term-value').textContent, '+120');
assert.equal(breakdown().dismissed_warnings.querySelector('small.agents-term-context').textContent, '4 of 20 settled assessments');
const how = part('how').textContent;
assert.match(how, /Confirmed warnings, Dismissed warnings, Missed scope blocks are rated per 100 settled assessments instead/);
assert.match(how, /An approval after a field edit that names no document/);

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
assert.equal(stats['Warning precision (%)'], '66.7');
assert.equal(stats['Holds per 100 assessments'], '40');
assert.equal(stats['Settled assessments'], '20');
assert.equal(stats['Excluded or pending assessments'], '5');
assert.equal(stats['Missed scope blocks'], '0');
assert.equal(detail.querySelector('section.agents-penalty').dataset.term, 'lock_wait_hours');
assert.deepEqual(detail.querySelectorAll('li.agents-task').map(item => item.dataset.taskId), ['ORB-41'], 'the wait is listed under its holder');

click(selectButton('haiku'));
const requests = takeAll();
assert.equal(requests[0].url.pathname, '/api/agents/pilots/detail');
assert.equal(requests[0].url.searchParams.get('model'), 'claude-haiku-5-5');
requests[0].resolve(response(pilotDetail('haiku', 'claude-haiku-5-5')));
await settle();
assert.equal(part('detail').querySelector('h3').textContent, 'haiku');
assert.equal(part('detail').querySelector('section.agents-penalty').dataset.term, 'costly_misses', 'a provisional missed scope block is no penalty');
assert.equal(breakdown().missed_scope_blocks.querySelector('span.agents-term-value').textContent, '+0');
assert.match(breakdown().missed_scope_blocks.querySelector('small.agents-term-context').textContent, /1 of 6 settled assessments · provisional, not scored/);
console.log('dashboard pilots ok');
