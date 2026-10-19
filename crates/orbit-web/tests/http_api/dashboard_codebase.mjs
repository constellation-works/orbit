// Analytics › Codebase: its hash, the window / group-by segments and
// minimum-support stepper, the ranking tiles, the cross-ranking overlap list,
// each ranking's list and empty state, the detail panel, the task-pilot
// ranking with its misses and over-declarations, a poll that keeps the
// reader's place; and the Health denial badge that opens Events filtered to
// denials, with Events' default failure + denied filter.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const classesOf = node => String(node.className || '').split(/\s+/).filter(Boolean);
class Node {
  constructor(tag) {
    this.tagName = String(tag).toUpperCase();
    this.children = [];
    this.parentNode = null;
    this.dataset = {};
    this.attrs = {};
    this.listeners = {};
    this.className = '';
    this.own = '';
    this.style = { setProperty(name, value) { this[name] = value; } };
  }
  get classList() {
    const node = this;
    return {
      add: name => { if (!classesOf(node).includes(name)) node.className = [...classesOf(node), name].join(' '); },
      remove: name => { node.className = classesOf(node).filter(entry => entry !== name).join(' '); },
      contains: name => classesOf(node).includes(name),
      toggle: (name, on) => (on ?? !classesOf(node).includes(name)) ? node.classList.add(name) : node.classList.remove(name),
    };
  }
  set textContent(value) {
    this.children.forEach(child => { child.parentNode = null; });
    this.children = [];
    this.own = String(value);
  }
  get textContent() { return this.own + this.children.map(child => child.textContent ?? '').join(''); }
  get lastElementChild() { return this.children.at(-1) || null; }
  append(child) {
    if (child == null) return;
    if (typeof child === 'string') { this.own += child; return; }
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.children.push(child);
  }
  appendChild(child) { this.append(child); return child; }
  removeChild(child) {
    const index = this.children.indexOf(child);
    if (index >= 0) this.children.splice(index, 1);
    if (child) child.parentNode = null;
    return child;
  }
  insertBefore(child, ref) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    const index = ref ? this.children.indexOf(ref) : -1;
    if (index < 0) this.children.push(child);
    else this.children.splice(index, 0, child);
    return child;
  }
  setAttribute(name, value) { this.attrs[name] = String(value); }
  getAttribute(name) { return Object.prototype.hasOwnProperty.call(this.attrs, name) ? this.attrs[name] : null; }
  addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); }
  contains(node) { for (let current = node; current; current = current.parentNode) if (current === this) return true; return false; }
  focus() { document.activeElement = this; }
  querySelectorAll(selector) {
    const chains = selector.split(',').map(part => part.trim().split(/\s+/));
    const out = [];
    const walk = (node, ancestors) => {
      for (const child of node.children) {
        const path = [...ancestors, child];
        if (chains.some(chain => matchesChain(path, chain))) out.push(child);
        walk(child, path);
      }
    };
    walk(this, []);
    return out;
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
}
function matches(node, simple) {
  const tag = simple.match(/^[a-z]+/i)?.[0];
  if (tag && node.tagName !== tag.toUpperCase()) return false;
  for (const [, name] of simple.matchAll(/\.([^\s.#[]+)/g)) if (!classesOf(node).includes(name)) return false;
  const id = simple.match(/#([\w-]+)/)?.[1];
  return !id || node.id === id;
}
function matchesChain(path, chain) {
  let at = path.length - 1;
  if (!matches(path[at], chain[chain.length - 1])) return false;
  for (let index = chain.length - 2; index >= 0; index -= 1) {
    at -= 1;
    while (at >= 0 && !matches(path[at], chain[index])) at -= 1;
    if (at < 0) return false;
  }
  return true;
}

const byId = {};
const make = (tag, id, parent) => {
  const node = new Node(tag);
  if (id) { node.id = id; byId[id] = node; }
  if (parent) parent.appendChild(node);
  return node;
};
const body = new Node('body');
const layout = make('main', 'analytics-codebase-main', body);
for (const id of ['audit-count', 'audit-body', 'codebase-controls', 'codebase-body', 'rail-count-denials', 'audit-search', 'audit-filter']) {
  make('div', id, layout);
}
make('span', 'codebase-meta', byId['codebase-controls']);
const segments = { 'codebase-window': ['7d', '30d', '90d', 'all'], 'codebase-rollup': ['file', 'dir', 'crate'] };
for (const [id, values] of Object.entries(segments)) {
  const group = make('div', id, byId['codebase-controls']);
  for (const value of values) {
    const button = make('button', null, group);
    button.className = 'codebase-seg';
    button.dataset.value = value;
  }
}
make('button', 'codebase-min-down', byId['codebase-controls']);
make('output', 'codebase-min-support', byId['codebase-controls']);
make('button', 'codebase-min-up', byId['codebase-controls']);
for (const [tag, id] of [['form', 'codebase-related-form'], ['input', 'codebase-path-search'], ['div', 'codebase-path-options'], ['div', 'codebase-related-results']]) make(tag, id, layout);
const document = {
  activeElement: null,
  body,
  createElement: tag => new Node(tag),
  createDocumentFragment: () => new Node('#fragment'),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => byId[id] || null,
  querySelectorAll: selector => body.querySelectorAll(selector),
  querySelector: selector => body.querySelector(selector),
};
const window = { location: { search: '?window=24h', hash: '' }, confirm: () => true };
const copied = [];
const navigator = { clipboard: { writeText: async text => { copied.push(text); } } };
const pending = [];
const response = (payload, status = 200) => ({ ok: status >= 200 && status < 300, status, headers: { get: () => null }, json: async () => payload, text: async () => JSON.stringify(payload) });
const fetch = (path, options = {}) => new Promise(resolve => pending.push({ url: new URL(path, 'http://dashboard.test'), options, resolve }));
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, fetch, document, window, navigator, Node, Intl,
});
const modules = new Map();
const dir = new URL('../../assets/dashboard/js/', import.meta.url);
const load = name => {
  if (!modules.has(name)) {
    const url = new URL(name, dir);
    modules.set(name, new vm.SourceTextModule(fs.readFileSync(url, 'utf8'), { context, identifier: url.href }));
  }
  return modules.get(name);
};
for (const name of ['codebase.js', 'audit.js']) {
  await load(name).link(specifier => load(specifier.replace(/^\.\//, '')));
  await load(name).evaluate();
}
const audit = modules.get('audit.js').namespace;
const codebase = modules.get('codebase.js').namespace;
const common = modules.get('common.js').namespace;
const settle = async () => { for (let i = 0; i < 6; i += 1) await new Promise(resolve => setTimeout(resolve, 0)); };
const shown = id => byId[id].style.display !== 'none';
const view = () => byId['codebase-body'];
const part = key => view().children.find(node => node.dataset.key === key);
const click = node => { assert.ok(node, 'the control exists'); for (const listener of node.listeners.click || []) listener(); };
const tiles = () => part('tiles').querySelectorAll('button.codebase-tile');
const tile = key => tiles().find(button => button.dataset.tile === key);
const rows = () => part('list').querySelectorAll('button.codebase-row');
const rowPaths = () => rows().map(row => row.querySelector('span.codebase-path').textContent);
const selectedRow = () => rows().find(row => row.getAttribute('aria-current') === 'true');
const pressed = id => byId[id].children.filter(button => button.getAttribute('aria-pressed') === 'true').map(button => button.dataset.value);
const button = (scope, label) => scope.querySelectorAll('button').find(node => node.textContent === label);
const standings = () => Object.fromEntries(part('detail').querySelectorAll('li.codebase-standing')
  .map(item => [item.dataset.ranking, item.querySelector('span.codebase-standing-rank').textContent]));
async function serve(payload) {
  const request = codebase.fetchAndRenderCodebase();
  pending.at(-1).resolve(response(payload));
  await request;
  return pending.at(-1).url;
}

common.setMultiWorkspace(true);
common.setWorkspace('ws-a');
await settle();

// The default selection writes a bare #analytics/codebase, and a hash restores
// the window, minimum support and rollup into the controls and the request.
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase', 'the default selection writes a bare #analytics/codebase');
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'));
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase?window=30d&min_support=5&rollup=dir');
assert.deepEqual([pressed('codebase-window'), pressed('codebase-rollup'), byId['codebase-min-support'].value], [['30d'], ['dir'], '5']);

// The fixture's four path rankings share five paths in two or more of them,
// which the server lists in `overlap`: src/a/ in all four; src/c/ (footprint
// 80%), src/b/ (regressions 50%), src/y/ (blocked 20%) and src/z/ (footprint
// 10%) in two. src/z/ is also rejected at 95%, which does not lift it:
// Rejected does not order the list. Task ORB-9 regressed all five ranked
// regression paths.
const rate = (numerator, denominator) => ({ rate: numerator / denominator, numerator, denominator });
const regressed = (id, crew, by) => ({ task_id: id, title: `Task ${id}`, crew, regression_tasks: by });
const wide = regressed('ORB-9', 'opus', ['ORB-90']);
// Two pilot crews: luna lifts F1 over its filers and missed files in four
// tasks; haiku does worse than its filers and missed nothing.
const missed = (id, paths) => ({ task_id: id, title: `Task ${id}`, paths });
const luna = {
  crew: 'luna', model: 'luna-model', support: 5, recall: rate(6, 10), precision: rate(4, 5), f1: 0.6857,
  author: { recall: rate(2, 10), precision: rate(2, 2), f1: 0.3333 }, lift: { recall: 0.4, precision: -0.2, f1: 0.3524 },
  widenings: 3, widenings_per_task: 0.6, missed_files: 4, unused_selectors: 1,
  misses: [missed('ORB-21', ['src/a.rs', 'src/b.rs']), missed('ORB-22', ['src/c.rs']), missed('ORB-23', ['src/d.rs']), missed('ORB-24', ['src/e.rs'])],
  over_declarations: [{ task_id: 'ORB-31', title: '<b>web</b>', selectors: ['file:docs/unused.md'] }],
};
const haiku = {
  crew: 'haiku', model: null, support: 6, recall: rate(6, 6), precision: rate(6, 12), f1: 0.6667,
  author: { recall: rate(5, 6), precision: rate(5, 6), f1: 0.8333 }, lift: { recall: 0.1667, precision: -0.3333, f1: -0.1667 },
  widenings: 0, widenings_per_task: 0, missed_files: 0, unused_selectors: 6, misses: [], over_declarations: [],
};
const full = {
  window: '30d', rollup: 'dir', min_support: 5, touched: 'declared_footprint', tasks_scanned: 4933,
  computed_at: '2026-10-10T12:00:00.000Z', cache_ttl_seconds: 300, row_limit: 25,
  regressions: { below_support: 1775, population: 900, rows: [
    { path: 'src/a/', ...rate(13, 20), regressed_tasks: [wide, regressed('ORB-1', 'sol', ['ORB-2', 'ORB-3']), regressed('ORB-4', null, ['ORB-5']), regressed('ORB-6', 'opus', ['ORB-7'])],
      by_crew: [{ crew: 'opus', ...rate(12, 16) }, { crew: 'sol', ...rate(1, 4) }] },
    { path: 'src/b/', ...rate(9, 18), regressed_tasks: [wide], by_crew: [] },
    { path: 'src/c/', ...rate(8, 20), regressed_tasks: [wide], by_crew: [] },
    { path: 'src/d/', ...rate(6, 20), regressed_tasks: [wide], by_crew: [] },
    { path: 'src/e/', ...rate(4, 20), regressed_tasks: [wide], by_crew: [] },
  ] },
  footprint_misses: { below_support: 1614, population: 900, rows: [
    { path: 'src/c/', ...rate(16, 20), widenings: 20 },
    { path: 'src/x/', ...rate(12, 20), widenings: 12 },
    { path: 'src/a/', ...rate(6, 20), widenings: 6 },
    { path: 'src/z/', ...rate(2, 20), widenings: 2 },
  ] },
  blocked: { below_support: 1527, rows: [
    { path: 'src/a/', ...rate(5, 20), unclassified: 2 },
    { path: 'src/y/', ...rate(4, 20), unclassified: 0 },
  ], causes: [
    { cause: 'sandbox', kind: 'infrastructure', tasks: 7 },
    { cause: 'manual', kind: 'manual', actor: 'operator', tasks: 40 },
    { cause: 'base_conflict', kind: 'base_conflict', tasks: 12 },
    { cause: 'unclassified', kind: 'unclassified', tasks: 189 },
    { cause: 'run_failure', kind: 'attributed', tasks: 129 },
  ], unclassified_events: [
    { event: 'claim_failed', tasks: 120 },
    { event: 'claim_release_budget_exhausted', tasks: 50 },
    { event: 'review_repair_after_merge', tasks: 15 },
    { event: 'dependency_blocked', tasks: 4 },
  ], not_failures: 3 },
  rejected: { below_support: 105, rejected_with_run: 8, excluded_without_run: 88, rows: [
    { path: 'src/z/', ...rate(19, 20) },
    { path: 'src/b/', ...rate(2, 18) },
    { path: 'src/y/', ...rate(1, 15) },
    { path: 'src/a/', ...rate(1, 20) },
  ] },
  change_coupling: { below_support: 5633, reservations: 2300, excluded_large: 250, large_footprint_cap: 12, rows: [
    { paths: ['src/web/', 'src/web/tests/'], ...rate(22, 35), reservations: [33, 24] },
    { paths: ['src/a/', 'src/b/'], ...rate(16, 40), reservations: [30, 26] },
  ] },
  lock_contention: { below_support: 0, reservations: 2557, denials: 0, wait_time_recorded: false, rows: [] },
  task_pilot: { rows: [luna, haiku], below_support: 1, assessed: 14, unattributed: 2, without_assessment: 3,
    excluded_large: 1, excluded_no_diff: 4, large_footprint_cap: 12, drilldown_limit: 10 },
  overlap: { ranked_paths: 8, rows: [
    { path: 'src/a/', rankings: ['regressions', 'footprint_misses', 'blocked', 'rejected'], top_rate: 0.65 },
    { path: 'src/c/', rankings: ['regressions', 'footprint_misses'], top_rate: 0.8 },
    { path: 'src/b/', rankings: ['regressions', 'rejected'], top_rate: 0.5 },
    { path: 'src/y/', rankings: ['blocked', 'rejected'], top_rate: 0.2 },
    { path: 'src/z/', rankings: ['footprint_misses', 'rejected'], top_rate: 0.1 },
  ] },
};
const sent = await serve(full);
assert.equal(sent.pathname, '/api/codebase/analytics');
assert.deepEqual(
  ['window', 'min_support', 'rollup', 'workspace'].map(name => sent.searchParams.get(name)),
  ['30d', '5', 'dir', 'ws-a'],
  'the request carries the selection and the workspace',
);
assert.match(byId['codebase-meta'].textContent, /^4,933 tasks scanned · computed .+ · recomputed every 5 min$/);

// Eight tiles, each a toggle button; the overlap list is open by default.
assert.deepEqual(tiles().map(node => node.dataset.tile),
  ['overlap', 'regressions', 'footprint_misses', 'blocked', 'rejected', 'change_coupling', 'lock_contention', 'task_pilot']);
assert.deepEqual(tiles().map(node => node.getAttribute('aria-pressed')), ['true', 'false', 'false', 'false', 'false', 'false', 'false', 'false']);
assert.equal(tiles().every(node => node.tagName === 'BUTTON' && node.getAttribute('type') === 'button'), true);
assert.equal(tile('overlap').querySelector('span.codebase-tile-big').textContent, '5 paths');
assert.equal(tile('regressions').querySelector('span.codebase-tile-big').textContent, '65%');
assert.equal(tile('regressions').querySelector('span.codebase-tile-sub').textContent, 'top: a/');
assert.match(tile('regressions').querySelector('span.codebase-tile-foot').textContent, /^5 ranked · 1,775 /);
assert.ok(tile('blocked').querySelector('span.codebase-tile-foot.warn'), 'Blocked warns about its unclassified blocks');
assert.equal(tile('lock_contention').querySelector('span.codebase-tile-big').textContent, 'None');
assert.ok(tile('lock_contention').querySelector('span.codebase-tile-foot.good'), 'nothing to rank is the good state');
assert.deepEqual(['big', 'sub', 'foot'].map(name => tile('task_pilot').querySelector(`span.codebase-tile-${name}`).textContent),
  ['F1 69%', 'top: luna · luna-model', '2 ranked · 1 below support']);

// The overlap list: exactly the paths in two or more path rankings, by count,
// then by the highest rate outside Rejected; one chip per ranking.
assert.equal(part('list').dataset.ranking, 'overlap');
assert.deepEqual(rowPaths(), ['src/a/', 'src/c/', 'src/b/', 'src/y/', 'src/z/']);
assert.deepEqual(rows().map(row => row.querySelector('span.codebase-rank').textContent), ['4×', '2×', '2×', '2×', '2×']);
assert.deepEqual(rows()[0].querySelectorAll('span.codebase-chip').map(node => [node.dataset.ranking, node.querySelector('span.codebase-chip-frac').textContent]),
  [['regressions', '13/20'], ['footprint_misses', '6/20'], ['blocked', '5/20'], ['rejected', '1/20']]);
assert.equal(part('list').querySelector('div.codebase-hint'), null, 'no crate-level hint at directory rollup');

// The detail panel defaults to the first row: its standing in every ranking,
// its regressed tasks with links and the wide-task marker, and crew rates.
assert.equal(selectedRow(), rows()[0], 'the first row is selected by default');
assert.deepEqual(standings(), { regressions: '#1 of 5', footprint_misses: '#3 of 4', blocked: '#1 of 2', rejected: '#4 of 4' });
const tasks = () => part('detail').querySelectorAll('li.codebase-task');
assert.deepEqual(tasks().map(item => item.dataset.task), ['ORB-9', 'ORB-1', 'ORB-4'], 'three regressed tasks before All');
assert.deepEqual(tasks().map(item => item.dataset.wide ?? null), ['5', null, null], 'the task that regressed five ranked paths is marked');
assert.deepEqual(tasks()[1].querySelectorAll('a.codebase-task-link').map(link => link.textContent), ['ORB-1', 'ORB-2', 'ORB-3']);
assert.match(tasks()[1].querySelector('a.codebase-task-link').getAttribute('href'), /#tasks\?open=ORB-1$/);
assert.equal(tasks()[2].querySelector('span.codebase-crew').textContent, 'unassigned');
const all = button(part('detail'), 'All 4 →');
assert.equal(all.getAttribute('aria-expanded'), 'false');
click(all);
assert.equal(tasks().length, 4, 'All lists every regressed task');
assert.deepEqual(part('detail').querySelectorAll('li.codebase-crew-row').map(item => [item.dataset.crew, item.querySelector('span.codebase-frac').textContent]),
  [['opus', '12/16'], ['sol', '1/4']]);
click(button(part('detail'), 'Copy path'));
await settle();
assert.deepEqual(copied, ['src/a/']);
assert.ok(button(part('detail'), 'Copied'), 'the copy action confirms');

// Another row: rankings it is absent from read "not in the top 25".
click(rows()[2]);
assert.equal(selectedRow(), rows()[2]);
assert.deepEqual(standings(), { regressions: '#2 of 5', footprint_misses: 'not in the top 25', blocked: 'not in the top 25', rejected: '#2 of 4' });
assert.equal(tasks().length, 1, 'a new row starts collapsed');

// A poll that answers the same payload keeps every node, so the tile, the row
// and the reader's place stay; the selection survives.
const before = view().children.slice();
await serve(full);
assert.deepEqual(view().children.map((node, index) => node === before[index]), before.map(() => true), 'the same payload keeps every node');
assert.equal(rowPaths()[rows().indexOf(selectedRow())], 'src/b/');

// Selecting a tile swaps the list and keeps the row when the new list has it.
click(tile('regressions'));
assert.deepEqual(tiles().map(node => node.getAttribute('aria-pressed')), ['false', 'true', 'false', 'false', 'false', 'false', 'false', 'false']);
assert.equal(part('list').dataset.ranking, 'regressions');
assert.deepEqual(rowPaths(), ['src/a/', 'src/b/', 'src/c/', 'src/d/', 'src/e/']);
assert.equal(rowPaths()[rows().indexOf(selectedRow())], 'src/b/');
assert.deepEqual(rows()[0].querySelectorAll('span.codebase-chip').map(node => node.dataset.ranking), ['footprint_misses', 'blocked', 'rejected'],
  'chips name the other rankings the path is in');
assert.equal(rows()[0].querySelector('span.codebase-frac').textContent, '13 of 20');
assert.equal(button(part('list'), 'Show paths with 5+ tasks'), undefined, 'already at 5, so no lower-support button');

// A changed scan can change another ranking while the open section stays
// identical. Its cross-ranking chips must update without losing selection.
const changedRankings = JSON.parse(JSON.stringify(full));
changedRankings.footprint_misses.rows = changedRankings.footprint_misses.rows.filter(row => row.path !== 'src/a/');
changedRankings.blocked.rows[0] = { ...changedRankings.blocked.rows[0], ...rate(10, 20) };
changedRankings.overlap.rows[0] = { path: 'src/a/', rankings: ['regressions', 'blocked', 'rejected'], top_rate: 0.65 };
await serve(changedRankings);
assert.deepEqual(rows()[0].querySelectorAll('span.codebase-chip').map(node => [node.dataset.ranking, node.querySelector('span.codebase-chip-rate').textContent]),
  [['blocked', '50%'], ['rejected', '5.0%']], 'a poll removes absent rankings and updates rates in the open list’s chips');
assert.equal(rowPaths()[rows().indexOf(selectedRow())], 'src/b/', 'a cross-ranking update keeps the selected path');
const refreshedNodes = view().children.slice();
await serve(changedRankings);
assert.ok(view().children.every((node, index) => node === refreshedNodes[index]), 'an unchanged poll after the update keeps every node');
await serve(full);

click(tile('footprint_misses'));
assert.equal(rowPaths()[rows().indexOf(selectedRow())], 'src/c/', 'a row the new list lacks falls back to its first');
assert.ok(rows()[0].querySelector('span.codebase-extra'), 'widenings beyond the tasks show as an extra');
assert.equal(rows()[1].querySelector('span.codebase-extra'), null);

// Blocked: the cause bar and legend, base conflicts and manual blocks by
// actor in their own buckets, and a warning because unclassified is the
// largest share, naming its count and top events.
click(tile('blocked'));
const legend = part('list').querySelectorAll('li.codebase-cause').map(item => [item.dataset.cause, item.dataset.kind]);
assert.deepEqual(legend, [['run_failure', 'attributed'], ['base_conflict', 'base_conflict'], ['unclassified', 'unclassified'], ['sandbox', 'infrastructure'], ['manual', 'manual']]);
assert.deepEqual(part('list').querySelector('div.codebase-causebar').children.map(span => span.dataset.kind),
  ['attributed', 'base_conflict', 'unclassified', 'infrastructure', 'manual']);
const manualItem = part('list').querySelectorAll('li.codebase-cause').find(item => item.dataset.kind === 'manual');
assert.equal(manualItem.dataset.actor, 'operator');
assert.match(manualItem.textContent, /^Manual \(operator\) 40set by hand$/, 'a manual block names its actor kind');
assert.match(part('list').querySelectorAll('li.codebase-cause')[1].textContent, /^Base conflict 12counted on its conflicting paths$/);
const unknownHint = part('list').querySelector('div.codebase-hint.warn');
assert.ok(unknownHint, 'unclassified is the largest share');
assert.match(unknownHint.textContent,
  /^189 of 377 blocked tasks \(50(\.\d)?%\) carry no recorded cause, so they count against no path\. Most entered blocked through claim_failed \(120\), claim_release_budget_exhausted \(50\), review_repair_after_merge \(15\)\. The rates below only reflect the 129 that a run failure explains\.$/);
assert.match(part('list').querySelector('p.codebase-note.codebase-not-failure').textContent, /^3 tasks were held for review evidence or cancelled by an operator; those blocks are not failures and are left out\.$/);

// Once unclassified is no longer the largest share, the hint still names the
// remainder, without the warning.
const fewUnknown = JSON.parse(JSON.stringify(full));
fewUnknown.blocked.causes = fewUnknown.blocked.causes.map(cause => cause.cause === 'unclassified' ? { ...cause, tasks: 6 } : cause);
fewUnknown.blocked.unclassified_events = [{ event: 'claim_failed', tasks: 6 }];
fewUnknown.blocked.not_failures = 0;
await serve(fewUnknown);
assert.equal(part('list').querySelector('div.codebase-hint.warn'), null, 'a minor remainder does not warn');
assert.match(part('list').querySelector('div.codebase-hint').textContent,
  /^6 of 194 blocked tasks \(3(\.\d)?%\) carry no recorded cause, so they count against no path\. Most entered blocked through claim_failed \(6\)\.$/);
assert.equal(part('list').querySelector('p.codebase-note.codebase-not-failure'), null);
await serve(full);

// Rejected: the counts behind the ranking.
click(tile('rejected'));
assert.match(part('list').querySelector('div.codebase-hint').textContent, /^8 tasks were rejected after a run in this window; 88 rejected at proposal are left out\. The most on any one path is 19\.$/);

// Change coupling: a directory and its own tests/ is an expected pair that a
// toggle hides; a pair opens the pair view.
click(tile('change_coupling'));
assert.equal(rows().length, 2);
assert.ok(rows()[0].querySelector('span.codebase-tag'), 'a directory and its tests is tagged as expected');
assert.equal(rows()[1].querySelector('span.codebase-tag'), null);
assert.ok(part('detail').querySelector('span.codebase-pair-second'), 'a pair opens the pair view');
const hide = button(part('list'), 'Hide expected pairs');
assert.equal(hide.getAttribute('aria-pressed'), 'false');
click(hide);
assert.equal(rows().length, 1, 'hiding expected pairs removes them');
assert.equal(button(part('list'), 'Hide expected pairs').getAttribute('aria-pressed'), 'true');
assert.match(part('detail').querySelector('p.codebase-pair-line').textContent, /^Reserved together 16 of 40 /);
assert.equal(part('detail').dataset.ranking, 'change_coupling');

// Lock contention with nothing refused is a positive state, not a table.
click(tile('lock_contention'));
assert.ok(part('list').querySelector('div.codebase-empty-state.good'), 'no refused lock is the good state');
assert.equal(rows().length, 0);
assert.match(part('list').textContent, /2,557 file reservations/);

// Lock contention from a server that records no waits renders as it always
// did: rate rows with their reservation and denial counts, no sort, and the
// note that waits are not recorded.
const lockRow = (path, numerator, denominator, extra) => ({ path, ...rate(numerator, denominator), contending_tasks: numerator, reservations: 9, denials: 40, ...extra });
const legacyLock = JSON.parse(JSON.stringify(full));
legacyLock.lock_contention = { below_support: 0, reservations: 30, denials: 80, wait_time_recorded: false, rows: [
  lockRow('src/hot.rs', 3, 4, { wait_ms: null }), lockRow('src/slow.rs', 1, 5, { wait_ms: null }),
] };
await serve(legacyLock);
assert.deepEqual(rowPaths(), ['src/hot.rs', 'src/slow.rs']);
assert.match(rows()[0].textContent, /9 reservations · 40 denials/);
assert.equal(part('list').querySelectorAll('span.codebase-wait').length, 0, 'a payload without waits shows none');
assert.equal(button(part('list'), 'Sort by total wait'), undefined, 'nothing to sort by without waits');
assert.match(part('list').querySelector('div.codebase-list-foot').textContent, /^Lock wait time is not recorded, so waits cannot be shown\.$/);

// With waits, each row gives its median and longest wait and total wait
// minutes, and a row still waiting says so. A row whose denials named no task
// has no wait. Sorting by total wait reads the rows the server ranked by it.
const waited = JSON.parse(JSON.stringify(full));
const hot = lockRow('src/hot.rs', 3, 4, { wait: { episodes: 3, waiting: 0, median_ms: 120000, max_ms: 600000, total_ms: 840000 } });
const slow = lockRow('src/slow.rs', 1, 5, { wait: { episodes: 1, waiting: 1, median_ms: 9000000, max_ms: 9000000, total_ms: 9000000 } });
const runOnly = lockRow('src/run.rs', 1, 6, { wait: null });
waited.lock_contention = { below_support: 0, reservations: 30, denials: 80, wait_time_recorded: true, rows: [hot, slow, runOnly], rows_by_wait: [slow, hot] };
await serve(waited);
const waitText = () => rows().map(row => row.querySelector('span.codebase-wait')?.textContent ?? null);
const ranks = () => rows().map(row => row.querySelector('span.codebase-rank').textContent);
assert.deepEqual(rowPaths(), ['src/hot.rs', 'src/slow.rs', 'src/run.rs'], 'rows keep the rate order by default');
assert.deepEqual(waitText(), ['wait median 2 min · max 10 min · 14 min total', 'wait median 2 h 30 min · max 2 h 30 min · 150 min total', null]);
assert.match(rows()[1].textContent, /1 still waiting/);
assert.doesNotMatch(rows()[0].textContent, /still waiting/);
assert.doesNotMatch(part('list').textContent, /not recorded/, 'recorded waits drop the note');
assert.match(part('list').querySelector('div.codebase-list-foot').textContent, /first denial to its admission or its leaving the backlog/);
const sortByWait = () => button(part('list'), 'Sort by total wait');
assert.equal(sortByWait().getAttribute('aria-pressed'), 'false');
click(sortByWait());
assert.deepEqual(rowPaths(), ['src/slow.rs', 'src/hot.rs'], 'longest total wait first');
assert.deepEqual(ranks(), ['1', '2']);
assert.equal(sortByWait().getAttribute('aria-pressed'), 'true');
await serve(waited);
assert.deepEqual(rowPaths(), ['src/slow.rs', 'src/hot.rs'], 'a poll keeps the chosen sort');
click(sortByWait());
assert.deepEqual(rowPaths(), ['src/hot.rs', 'src/slow.rs', 'src/run.rs'], 'the toggle restores the rate order');

// Task pilot: crews ranked by F1 as the server ordered them, each with its
// recall and precision, its lift over the filers' selectors and widenings,
// and a footer naming what the ranking leaves out.
await serve(full);
click(tile('task_pilot'));
assert.equal(part('list').dataset.ranking, 'task_pilot');
const crews = () => rows().map(row => [row.querySelector('span.codebase-pilot-crew').textContent, row.querySelector('span.codebase-pilot-model').textContent]);
assert.deepEqual(crews(), [['luna', 'luna-model'], ['haiku', 'model not recorded']]);
assert.deepEqual(rows().map(row => row.querySelector('span.codebase-rate').textContent), ['69%', '67%']);
assert.deepEqual(rows().map(row => row.querySelector('span.codebase-frac').textContent), ['5 tasks', '6 tasks']);
assert.match(rows()[0].textContent, /recall 60% · precision 80%/);
assert.equal(rows()[0].querySelector('span.codebase-tag.lift').textContent, '+35 pts vs author');
assert.equal(rows()[1].querySelector('span.codebase-tag.drop').textContent, '−17 pts vs author', 'a crew worse than its filers shows a drop');
assert.match(rows()[0].textContent, /0\.6 widenings per task/);
assert.equal(part('list').querySelector('div.codebase-list-foot').textContent,
  '14 assessed tasks landed in this window. 1 crew below minimum support 5 is hidden. 2 assessments could not be matched to a pilot run. 3 landed tasks had no pilot assessment. 1 bulk and 4 no-diff tasks are left out.');

// Its detail: the scores beside the filers', then the tasks with the worst
// misses and the largest over-declarations, each linked to its task.
assert.equal(selectedRow(), rows()[0]);
assert.equal(part('detail').dataset.ranking, 'task_pilot');
assert.equal(part('detail').querySelector('p.codebase-eyebrow').textContent, 'Task pilot #1');
assert.deepEqual(part('detail').querySelectorAll('li.codebase-standing').map(item => [item.dataset.metric, item.querySelector('span.codebase-rate').textContent, item.querySelector('span.codebase-standing-rank').textContent]),
  [['f1', '69%', 'author 33%'], ['recall', '60%', 'author 20%'], ['precision', '80%', 'author 100%']]);
const pilotBlock = label => part('detail').querySelectorAll('div.codebase-detail-block')
  .find(block => block.querySelector('p.codebase-eyebrow')?.textContent === label);
const listed = label => pilotBlock(label).querySelectorAll('li.codebase-task');
assert.deepEqual(listed('Worst misses').map(item => item.dataset.task), ['ORB-21', 'ORB-22', 'ORB-23'], 'three tasks before All');
assert.deepEqual(listed('Worst misses')[0].querySelectorAll('span.codebase-path').map(node => node.textContent), ['src/a.rs', 'src/b.rs']);
assert.match(listed('Worst misses')[0].querySelector('a.codebase-task-link').getAttribute('href'), /#tasks\?open=ORB-21$/);
const allMisses = button(pilotBlock('Worst misses'), 'All 4 →');
assert.equal(allMisses.getAttribute('aria-expanded'), 'false');
click(allMisses);
assert.equal(listed('Worst misses').length, 4, 'All lists every task with a miss');
assert.equal(button(pilotBlock('Largest over-declarations'), 'All 4 →'), undefined, 'one task needs no All');
const over = listed('Largest over-declarations');
assert.deepEqual(over.map(item => item.dataset.task), ['ORB-31']);
assert.equal(over[0].querySelector('span.codebase-task-title').textContent, '<b>web</b>', 'task titles stay text');
assert.deepEqual(over[0].querySelectorAll('code.codebase-selector').map(node => node.textContent), ['file:docs/unused.md']);
assert.match(over[0].querySelector('a.codebase-task-link').getAttribute('href'), /#tasks\?open=ORB-31$/);
const pilotNodes = view().children.slice();
await serve(full);
assert.ok(view().children.every((node, index) => node === pilotNodes[index]), 'a poll keeps the pilot list and its open detail');
click(rows()[1]);
assert.equal(part('detail').querySelector('p.codebase-eyebrow').textContent, 'Task pilot #2');
assert.equal(listed('Worst misses').length, 0);
assert.match(pilotBlock('Worst misses').textContent, /Every landed file was covered by a pilot selector\./);

// Regressions: one chip row per dimension with its link counts (absent and
// unclassified apart, zero counts hidden) and the definitions; the nature x
// escape and escape x layer cross-tabs shaded against their largest count;
// crew and complexity rates above minimum support; and each regressed task's
// links with their four dimensions in the detail.
const link = (task_id, nature, escape, layer, detected_by, layer_source = 'landed') => ({ task_id, nature, escape, layer, layer_source, detected_by });
const breakdown = counts => Object.entries(counts).map(([value, links]) => ({ value, links }));
const noFilter = { nature: null, escape: null, layer: null, detected_by: null };
const sliced = JSON.parse(JSON.stringify(full));
sliced.regression_filter = noFilter;
Object.assign(sliced.regressions, {
  links: 6, matching_links: 6, regressed: 4,
  breakdowns: {
    nature: breakdown({ broke_existing: 3, defect_in_new: 0, unclassified: 1, absent: 2 }),
    escape: breakdown({ no_test: 4, review_missed: 0, absent: 2 }),
    layer: breakdown({ ci_tests: 0, dashboard: 4, store_migration: 2 }),
    detected_by: breakdown({ delivery_review: 5, operator: 1 }),
  },
  cross_tabs: {
    nature_escape: { row: 'nature', column: 'escape', rows: ['broke_existing', 'unclassified', 'absent'], columns: ['no_test', 'absent'], counts: [[3, 0], [1, 0], [0, 2]] },
    escape_layer: { row: 'escape', column: 'layer', rows: ['no_test', 'absent'], columns: ['dashboard', 'store_migration'], counts: [[4, 0], [0, 2]] },
  },
  by_crew: { rows: [{ crew: 'opus', ...rate(3, 10), support: 10 }, { crew: 'sol', ...rate(1, 8), support: 8 }], below_support: 2 },
  by_complexity: { rows: [], below_support: 3 },
});
sliced.regressions.rows[0].regressed_tasks[1] = {
  ...regressed('ORB-1', 'sol', ['ORB-2', 'ORB-3']),
  regressions: [link('ORB-2', 'broke_existing', 'no_test', 'dashboard', 'delivery_review'), link('ORB-3', 'absent', 'absent', 'store_migration', 'operator', 'footprint')],
};
await serve(sliced);
click(tile('regressions'));
const facets = dimension => part('list').querySelectorAll('button.codebase-facet').filter(node => node.dataset.dimension === dimension);
assert.deepEqual(facets('nature').map(node => [node.dataset.value, node.textContent, node.getAttribute('aria-pressed')]),
  [['broke_existing', 'broke existing 3', 'false'], ['unclassified', 'unclassified 1', 'false'], ['absent', 'absent 2', 'false']],
  'a value no link takes is hidden; absent and unclassified are separate chips');
assert.deepEqual(facets('layer').map(node => node.textContent), ['dashboard 4', 'store/migration 2']);
assert.deepEqual(facets('detected_by').map(node => node.dataset.value), ['delivery_review', 'operator']);
assert.deepEqual(part('list').querySelector('details.codebase-definitions').querySelectorAll('dt').map(node => node.textContent),
  ['Nature', 'Escape', 'Layer', 'Detected by', 'Counts'], 'the tile documents every dimension');
const crossTab = name => part('list').querySelectorAll('table.codebase-crosstab').find(table => table.dataset.tab === name);
const levels = name => crossTab(name).querySelectorAll('td').map(cell => [cell.dataset.row, cell.dataset.column, cell.textContent, cell.dataset.level]);
assert.deepEqual(levels('nature_escape'), [
  ['broke_existing', 'no_test', '3', '4'], ['broke_existing', 'absent', '·', '0'],
  ['unclassified', 'no_test', '1', '2'], ['unclassified', 'absent', '·', '0'],
  ['absent', 'no_test', '·', '0'], ['absent', 'absent', '2', '3'],
]);
assert.deepEqual(crossTab('escape_layer').querySelectorAll('thead th').map(cell => cell.textContent), ['', 'dashboard', 'store/migration']);
const slice = title => part('list').querySelectorAll('div.codebase-slice').find(block => block.querySelector('p.codebase-eyebrow').textContent === title);
assert.deepEqual(slice('Regression rate by crew').querySelectorAll('li.codebase-crew-row').map(item => [item.dataset.crew, item.querySelector('span.codebase-rate').textContent, item.querySelector('span.codebase-frac').textContent]),
  [['opus', '30%', '3/10'], ['sol', '13%', '1/8']]);
assert.match(slice('Regression rate by crew').textContent, /2 crews below minimum support 5 are hidden\./);
assert.equal(slice('Regression rate by complexity').querySelector('p.codebase-note').textContent,
  'No complexity has 5 or more done tasks in this window; 3 complexities fall below it.', 'an empty slice says why');
assert.equal(selectedRow(), rows()[0]);
const linkDims = () => tasks()[1].querySelectorAll('span.codebase-link-dims');
assert.deepEqual(linkDims().map(line => [line.dataset.task, line.querySelectorAll('span.codebase-tag').map(tag => tag.dataset.value)]),
  [['ORB-2', ['broke_existing', 'no_test', 'dashboard', 'delivery_review']], ['ORB-3', ['absent', 'absent', 'store_migration', 'operator']]]);
assert.match(linkDims()[1].querySelectorAll('span.codebase-tag')[2].title, /declared footprint/, 'a layer read from the footprint says so');
assert.equal(part('detail').querySelector('p.codebase-filter-note'), null, 'no filter, no filter note');

// A chip filters: the request and the hash carry it, the tile names it, the
// served ranking and its detail are the filtered ones, and the chip reads
// pressed. Choosing it again clears it.
click(facets('nature')[0]);
assert.equal(codebase.getCodebaseQuery().nature, 'broke_existing');
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase?window=30d&min_support=5&rollup=dir&nature=broke_existing');
const narrowed = JSON.parse(JSON.stringify(sliced));
narrowed.regression_filter = { ...noFilter, nature: 'broke_existing' };
narrowed.regressions.rows = [{ path: 'src/a/', ...rate(1, 20), by_crew: [], regressed_tasks: [{ ...regressed('ORB-1', 'sol', ['ORB-2']), regressions: [link('ORB-2', 'broke_existing', 'no_test', 'dashboard', 'delivery_review')] }] }];
Object.assign(narrowed.regressions, { matching_links: 3, regressed: 2 });
const filteredRequest = await serve(narrowed);
assert.equal(filteredRequest.searchParams.get('nature'), 'broke_existing', 'the filter goes to the server');
assert.equal(filteredRequest.searchParams.get('escape'), null, 'an unset filter is not sent');
assert.deepEqual(rowPaths(), ['src/a/']);
assert.equal(facets('nature')[0].getAttribute('aria-pressed'), 'true');
assert.equal(tile('regressions').querySelector('span.codebase-tile-foot.filter').textContent, 'filtered: broke existing');
assert.match(part('list').querySelector('div.codebase-facets').textContent, /3 of 6 regression links on 2 regressed tasks/);
assert.deepEqual(tasks().map(item => item.dataset.task), ['ORB-1']);
assert.deepEqual(tasks()[0].querySelectorAll('a.codebase-task-link').map(node => node.textContent), ['ORB-1', 'ORB-2'], 'the detail lists only matching regressions');
assert.match(part('detail').querySelector('p.codebase-filter-note').textContent, /^Filtered to nature broke existing: /);
click(facets('nature')[0]);
assert.equal(codebase.getCodebaseQuery().nature, null, 'choosing the chosen chip clears it');

// Filters that leave no path: an empty state with Clear filters, which
// resets every dimension. A hash with an unknown value keeps no filter.
click(facets('layer')[1]);
click(facets('detected_by')[1]);
const nothing = JSON.parse(JSON.stringify(sliced));
nothing.regression_filter = { ...noFilter, layer: 'store_migration', detected_by: 'operator' };
nothing.regressions.rows = [];
Object.assign(nothing.regressions, { matching_links: 0, regressed: 0, cross_tabs: { nature_escape: { row: 'nature', column: 'escape', rows: [], columns: [], counts: [] }, escape_layer: { row: 'escape', column: 'layer', rows: [], columns: [], counts: [] } } });
const both = await serve(nothing);
assert.deepEqual(['layer', 'detected_by'].map(name => both.searchParams.get(name)), ['store_migration', 'operator']);
assert.equal(rows().length, 0);
assert.equal(part('list').querySelector('p.codebase-empty-title').textContent, 'No path has a regression matching these filters.');
assert.deepEqual(part('list').querySelectorAll('div.codebase-crosstab-block').map(block => block.querySelector('p.codebase-note')?.textContent),
  ['No regression matches these filters.', 'No regression matches these filters.']);
assert.equal(part('detail').querySelector('p.codebase-note').textContent, 'Nothing is ranked here, so there is no row to show.');
click(button(part('list').querySelector('div.codebase-empty-state'), 'Clear filters'));
assert.deepEqual(['nature', 'escape', 'layer', 'detected_by'].map(name => codebase.getCodebaseQuery()[name]), [null, null, null, null]);
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase?window=30d&min_support=5&rollup=dir');
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir&escape=no_test&layer=bogus'));
assert.deepEqual([codebase.getCodebaseQuery().escape, codebase.getCodebaseQuery().layer], ['no_test', null], 'a hash restores known filters only');
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'));

// A window without any regression link says there is nothing to break down.
const noLinks = JSON.parse(JSON.stringify(full));
noLinks.regression_filter = noFilter;
Object.assign(noLinks.regressions, { links: 0, matching_links: 0, regressed: 0, breakdowns: {}, cross_tabs: {}, by_crew: { rows: [], below_support: 0 }, by_complexity: { rows: [], below_support: 0 } });
await serve(noLinks);
assert.equal(part('list').querySelectorAll('button.codebase-facet').length, 0);
assert.match(part('list').querySelector('div.codebase-facets').textContent, /^No regression_from link targets a task done in this window/);
assert.equal(part('list').querySelector('table.codebase-crosstab'), null, 'no cross-tab without links');
await serve(full);
click(tile('task_pilot'));

// The window and group-by segments and the stepper rewrite the selection and
// ask for a new hash; an unchanged one asks for nothing.
let changes = 0;
codebase.wireCodebaseControls(() => { changes += 1; });
const segment = (id, value) => byId[id].children.find(node => node.dataset.value === value);
click(segment('codebase-rollup', 'crate'));
click(segment('codebase-window', '7d'));
click(segment('codebase-window', '7d'));
assert.equal(changes, 2, 'an unchanged segment asks for no new hash');
assert.deepEqual([pressed('codebase-window'), pressed('codebase-rollup')], [['7d'], ['crate']]);
click(byId['codebase-min-up']);
assert.equal(codebase.getCodebaseQuery().min_support, 10, 'the stepper walks to the next step');
click(byId['codebase-min-down']);
click(byId['codebase-min-down']);
click(byId['codebase-min-down']);
click(byId['codebase-min-down']);
assert.equal(codebase.getCodebaseQuery().min_support, 1);
assert.equal(byId['codebase-min-down'].disabled, true, 'the stepper stops at 1');
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase?window=7d&min_support=1&rollup=crate');

// An empty coupling ranking offers directory grouping and a lower minimum;
// both rewrite the selection. Crate grouping warns that overlaps say little.
const empty = {
  window: '90d', rollup: 'crate', min_support: 15, touched: 'declared_footprint', tasks_scanned: 0,
  computed_at: '2026-10-10T12:00:00.000Z', cache_ttl_seconds: 300, row_limit: 25,
  regressions: { rows: [], below_support: 0, population: 0 },
  lock_contention: { rows: [], below_support: 0, reservations: 0, denials: 0, wait_time_recorded: false },
  footprint_misses: { rows: [], below_support: 0, population: 0 },
  change_coupling: { rows: [], below_support: 13051, reservations: 0, excluded_large: 250, large_footprint_cap: 12 },
  blocked: { rows: [], below_support: 0, causes: [{ cause: 'unclassified', kind: 'unclassified', tasks: 0 }, { cause: 'run_failure', kind: 'attributed', tasks: 0 }] },
  rejected: { rows: [], below_support: 0, rejected_with_run: 0, excluded_without_run: 0 },
  overlap: { rows: [], ranked_paths: 0 },
  task_pilot: { rows: [], below_support: 2, assessed: 7, unattributed: 0, without_assessment: 0, excluded_large: 0, excluded_no_diff: 0, large_footprint_cap: 12, drilldown_limit: 10 },
};
await serve(empty);
assert.equal(part('list').dataset.ranking, 'task_pilot', 'the open tile survives a new payload');
assert.equal(tile('task_pilot').querySelector('span.codebase-tile-big').textContent, 'None');
assert.equal(part('list').querySelector('p.codebase-empty-title').textContent,
  'No pilot crew has 15 or more assessed tasks that landed in this window; 2 crews fall below it.');
assert.equal(rows().length, 0);
assert.equal(part('detail').querySelector('p.codebase-note').textContent, 'Nothing is ranked here, so there is no row to show.');
assert.ok(button(part('list'), 'Lower to 5'), 'crews below support offer a lower minimum');
click(tile('lock_contention'));
click(tile('overlap'));
assert.ok(part('list').querySelector('div.codebase-hint'), 'crate grouping warns that the overlap list says little');
assert.ok(part('list').querySelector('p.codebase-empty'));
click(tile('regressions'));
assert.ok(button(part('list'), 'Show paths with 5+ tasks'), 'a minimum above 5 offers to lower it');
assert.ok(part('list').querySelector('p.codebase-empty'));
click(tile('change_coupling'));
assert.ok(part('list').querySelector('div.codebase-empty-state'));
changes = 0;
click(button(part('list'), 'Group by directory'));
assert.equal(codebase.getCodebaseQuery().rollup, 'dir');
click(button(part('list'), 'Lower to 5'));
assert.equal(codebase.getCodebaseQuery().min_support, 5);
assert.equal(changes, 2, 'each action asks for a new hash');
assert.deepEqual([pressed('codebase-rollup'), byId['codebase-min-support'].value], [['dir'], '5']);
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase?window=7d&min_support=5&rollup=dir');

// Prefix suggestions include landed-only Markdown and submit the same file query.
const pathPayload = { ...full, known_paths: ['docs/design.md', 'src/a.rs', 'src/b.rs'] };
codebase.renderCodebase(pathPayload);
const search = byId['codebase-path-search'];
search.value = 'src/';
for (const listener of search.listeners.input) listener();
assert.deepEqual(byId['codebase-path-options'].children.map(node => node.textContent), ['src/a.rs', 'src/b.rs']);
click(byId['codebase-path-options'].children[0]);
assert.equal(byId['codebase-path-options'].children.length, 0);
let relatedRequest = pending.at(-1);
assert.equal(relatedRequest.url.pathname, '/api/codebase/related');
assert.equal(relatedRequest.url.searchParams.get('path'), 'src/a.rs');
assert.equal(relatedRequest.url.searchParams.get('workspace'), 'ws-a');
relatedRequest.resolve(response({ path: 'src/a.rs', path_support: 2, rows: [
  { path: 'docs/design.md', jaccard: 0.5, conditional_rate: 1, support: { both: 2, path: 2, partner: 4, either: 4 }, footprint_miss_count: 1, tasks: [{ task_id: 'ORB-9', title: '<img src=x onerror=bad()>', commit: 'abc' }], footprint_miss_tasks: [] },
  { path: 'src/b.rs', jaccard: 0.25, conditional_rate: 0.5, support: { both: 1, path: 2, partner: 3, either: 4 }, reservation_coupling: { ...rate(1, 2), tasks: [{ task_id: 'ORB-8', title: 'Reserved together fixture' }] }, footprint_miss_count: 2, tasks: [], footprint_miss_tasks: [] },
] }));
await settle();
const related = byId['codebase-related-results'];
assert.deepEqual(related.querySelectorAll('span.codebase-path').map(node => node.textContent), ['docs/design.md', 'src/b.rs']);
assert.equal(related.querySelector('a.codebase-task-link').textContent, '<img src=x onerror=bad()>', 'task titles stay text');
assert.match(related.querySelector('a.codebase-task-link').getAttribute('href'), /open=ORB-9$/);
assert.match(related.querySelectorAll('a.codebase-task-link')[1].getAttribute('href'), /open=ORB-8$/, 'reservation evidence links to its task');
click(button(related, 'Footprint misses'));
assert.deepEqual(related.querySelectorAll('span.codebase-path').map(node => node.textContent), ['src/b.rs', 'docs/design.md']);

// An unchanged analytics poll leaves the related panel and its open evidence
// nodes in place, without issuing another related-files request.
const evidence = related.querySelector('details');
assert.ok(evidence, 'related task evidence is displayed in a details element');
evidence.open = true;
const relatedNodes = related.children.slice();
const relatedRequestCount = () => pending.filter(request => request.url.pathname === '/api/codebase/related').length;
const requestsBeforePoll = relatedRequestCount();
await serve(pathPayload);
assert.equal(relatedRequestCount(), requestsBeforePoll, 'an unchanged analytics poll does not refetch related files');
assert.deepEqual(related.children, relatedNodes, 'an unchanged analytics poll keeps the related panel DOM');
assert.equal(evidence.open, true, 'an unchanged analytics poll keeps evidence open');

// A changed window or minimum support still fetches and renders its result.
click(segment('codebase-window', '30d'));
const windowRequest = await serve(pathPayload);
assert.equal(windowRequest.pathname, '/api/codebase/related');
assert.equal(windowRequest.searchParams.get('window'), '30d');
pending.at(-1).resolve(response({ path: 'src/a.rs', path_support: 7, rows: [] }));
await settle();
assert.match(related.querySelector('p.codebase-note').textContent, /7 landed tasks changed this file/);

click(byId['codebase-min-up']);
const supportRequest = await serve(pathPayload);
assert.equal(supportRequest.pathname, '/api/codebase/related');
assert.equal(supportRequest.searchParams.get('min_support'), '10');
pending.at(-1).resolve(response({ path: 'src/a.rs', path_support: 8, rows: [] }));
await settle();
assert.match(related.querySelector('p.codebase-note').textContent, /8 landed tasks changed this file/);

// A refreshed analytics payload also refreshes the related result.
const refreshedPayload = { ...pathPayload, computed_at: '2026-10-10T12:05:00.000Z' };
const refreshedRequest = await serve(refreshedPayload);
assert.equal(refreshedRequest.pathname, '/api/codebase/related');
pending.at(-1).resolve(response({ path: 'src/a.rs', path_support: 9, rows: [] }));
await settle();
assert.match(related.querySelector('p.codebase-note').textContent, /9 landed tasks changed this file/);

// An explicit path change fetches, and a failed related request is retried on
// the next analytics poll even when its payload has not changed.
search.value = 'broken.rs';
for (const listener of search.listeners.input) listener();
for (const listener of byId['codebase-related-form'].listeners.submit) listener({ preventDefault() {} });
let pathRequest = pending.at(-1);
assert.equal(pathRequest.url.pathname, '/api/codebase/related');
assert.equal(pathRequest.url.searchParams.get('path'), 'broken.rs');
pathRequest.resolve(response({ error: 'temporary failure' }, 503));
await settle();
assert.match(related.textContent, /Could not load related files: temporary failure/);
const retryRequest = await serve(refreshedPayload);
assert.equal(retryRequest.pathname, '/api/codebase/related');
assert.equal(retryRequest.searchParams.get('path'), 'broken.rs');
pending.at(-1).resolve(response({ path: 'broken.rs', rows: [] }));
await settle();
assert.ok(related.querySelector('p.codebase-empty'), 'a failed related request retries on the next poll');

search.value = 'missing.rs';
for (const listener of search.listeners.input) listener();
assert.equal(byId['codebase-path-options'].children.length, 0, 'an unknown prefix has no suggestions');
for (const listener of byId['codebase-related-form'].listeners.submit) listener({ preventDefault() {} });
const missingPathRequest = pending.at(-1);
assert.equal(missingPathRequest.url.searchParams.get('path'), 'missing.rs');
missingPathRequest.resolve(response({ path: 'missing.rs', rows: [] }));
await settle();
assert.ok(related.querySelector('p.codebase-empty'), 'empty search explains minimum support');
// A late result cannot cross workspace visits.
for (const listener of byId['codebase-related-form'].listeners.submit) listener({ preventDefault() {} });
relatedRequest = pending.at(-1);
common.setWorkspace('ws-b');
relatedRequest.resolve(response({ path: 'missing.rs', rows: [] }));
await settle();
assert.equal(related.textContent, '');
assert.equal(search.value, '');
common.setWorkspace('ws-a');

// The aggregate view shows the placeholder instead of the codebase view.
common.setWorkspace('');
await settle();
const requests = pending.length;
await codebase.fetchAndRenderCodebase();
assert.equal(pending.length, requests, 'the aggregate view requests nothing');
assert.deepEqual(view().children.map(node => node.dataset.key), ['aggregate-placeholder']);
assert.equal(byId['codebase-meta'].textContent, '');
common.setWorkspace('ws-a');
await settle();

// Events opens on failures and denials; `status=all` is the explicit
// unfiltered choice, and an unknown status falls back to the default.
audit.buildAuditChips({});
const statusChips = () => byId['audit-filter'].children.filter(chip => chip.dataset.status);
const activeChip = () => statusChips().filter(chip => chip.classList.contains('active')).map(chip => chip.dataset.status);
audit.applyAuditHashQuery(new URLSearchParams(''));
audit.syncAuditControls();
assert.equal(audit.buildAuditHash(), '#diagnostics/incidents/events?since=24h&status=non_success', 'Events opens on failure + denied');
assert.deepEqual(activeChip(), ['non_success'], 'the failure + denied chip is preselected');
audit.applyAuditHashQuery(new URLSearchParams('status=all'));
audit.syncAuditControls();
assert.equal(audit.buildAuditHash(), '#diagnostics/incidents/events?since=24h&status=all');
assert.deepEqual(activeChip(), ['all'], 'all statuses stay available');
audit.applyAuditHashQuery(new URLSearchParams('status=bogus'));
assert.match(audit.buildAuditHash(), /status=non_success/, 'an unknown status falls back to the default');
click(statusChips().find(chip => chip.dataset.status === 'all'));
assert.equal(window.location.hash, '#diagnostics/incidents/events?since=24h&status=all', 'the all chip writes status=all');

// The denial badge on Health shows the canonical count and routes to Events
// filtered to denials for the window it counted.
const badge = byId['rail-count-denials'];
audit.renderDenialCount({ denials: 4, denials_sql: 3, denials_v2: 1, window: '7d' });
assert.equal(badge.hidden, false);
assert.equal(badge.textContent, '4 denied');
assert.match(badge.title, /3 audit rows, 1 loop denials/);
assert.equal(badge.dataset.route, 'diagnostics/incidents/events?since=7d&status=denied');
audit.applyAuditHashQuery(new URLSearchParams(badge.dataset.route.split('?')[1]));
assert.equal(audit.buildAuditHash(), '#diagnostics/incidents/events?since=7d&status=denied', 'the badge lands on Events filtered to denials');
audit.renderDenialCount({ denials: 0, window: '7d' });
assert.equal(badge.hidden, true, 'no denials, no badge');
assert.equal(badge.dataset.route, undefined);

console.log('codebase view, Events default filter and denial badge behaviors passed');
