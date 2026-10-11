// Audit › Codebase: the subtab swap to a full-width view and its hash, the
// window / group-by segments and minimum-support stepper, the ranking tiles,
// the cross-ranking overlap list, each ranking's list and empty state, the
// detail panel, a poll that keeps the reader's place, and the denial badge
// that opens Events filtered to denials.
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
const layout = make('main', 'audit-main', body);
layout.className = 'tasks-layout audit-layout';
const subtabs = make('nav', 'audit-subtabs', layout);
for (const name of ['events', 'codebase']) {
  const button = make('button', null, subtabs);
  button.className = 'subtab';
  button.dataset.subtab = name;
}
for (const id of ['audit-title', 'audit-count', 'audit-events-controls', 'audit-body', 'audit-codebase-controls', 'audit-codebase-body', 'audit-summary-column', 'rail-count-audit-denials', 'audit-search']) {
  make('div', id, layout);
}
make('span', 'codebase-meta', byId['audit-codebase-controls']);
const segments = { 'codebase-window': ['7d', '30d', '90d', 'all'], 'codebase-rollup': ['file', 'dir', 'crate'] };
for (const [id, values] of Object.entries(segments)) {
  const group = make('div', id, byId['audit-codebase-controls']);
  for (const value of values) {
    const button = make('button', null, group);
    button.className = 'codebase-seg';
    button.dataset.value = value;
  }
}
make('button', 'codebase-min-down', byId['audit-codebase-controls']);
make('output', 'codebase-min-support', byId['audit-codebase-controls']);
make('button', 'codebase-min-up', byId['audit-codebase-controls']);
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
const auditModule = load('audit.js');
await auditModule.link(specifier => load(specifier.replace(/^\.\//, '')));
await auditModule.evaluate();
const audit = auditModule.namespace;
const codebase = modules.get('codebase.js').namespace;
const common = modules.get('common.js').namespace;
const settle = async () => { for (let i = 0; i < 6; i += 1) await new Promise(resolve => setTimeout(resolve, 0)); };
const shown = id => byId[id].style.display !== 'none';
const view = () => byId['audit-codebase-body'];
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

// Codebase takes the full width: the audit summary column is hidden and the
// layout drops to one column. Events brings both back. A retired Policy link
// lands on Events.
audit.setActiveAuditSubtabFromButton('codebase');
assert.ok(shown('audit-codebase-body') && shown('audit-codebase-controls'), 'Codebase shows its body and controls');
assert.ok(!shown('audit-body') && !shown('audit-events-controls'), 'Codebase hides the Events listing');
assert.ok(!shown('audit-summary-column') && layout.classList.contains('audit-full'), 'Codebase hides the audit summary and goes full width');
assert.deepEqual(subtabs.children.filter(node => node.classList.contains('active')).map(node => node.dataset.subtab), ['codebase']);
assert.equal(audit.buildAuditHash(), '#audit/codebase', 'the default selection writes a bare #audit/codebase');
audit.setAuditSubtab('policy');
assert.equal(audit.getActiveAuditSubtab(), 'events', 'the retired policy subtab falls back to Events');
assert.ok(shown('audit-body') && !shown('audit-codebase-body'));
assert.ok(shown('audit-summary-column') && !layout.classList.contains('audit-full'), 'Events keeps the audit summary beside it');

// An Events filter survives a visit to Codebase, and #audit/codebase?… restores
// the window, minimum support and rollup into the controls and the request.
audit.applyAuditHashQuery(new URLSearchParams('status=denied'), 'events');
audit.applyAuditHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'), 'codebase');
audit.setActiveAuditSubtabFromButton('codebase');
assert.equal(audit.buildAuditHash(), '#audit/codebase?window=30d&min_support=5&rollup=dir');
assert.deepEqual([pressed('codebase-window'), pressed('codebase-rollup'), byId['codebase-min-support'].value], [['30d'], ['dir'], '5']);
audit.setActiveAuditSubtabFromButton('events');
assert.match(audit.buildAuditHash(), /status=denied/, 'the Events filter is kept across the Codebase visit');
audit.setActiveAuditSubtabFromButton('codebase');

// The fixture's four path rankings share five paths in two or more of them:
// src/a/ in all four; src/c/ (footprint 80%), src/b/ (regressions 50%),
// src/y/ (blocked 20%) and src/z/ (footprint 10%) in two. src/z/ is also
// rejected at 95%, which must not lift it: Rejected does not order the list.
// Task ORB-9 regressed all five ranked regression paths.
const rate = (numerator, denominator) => ({ rate: numerator / denominator, numerator, denominator });
const regressed = (id, crew, by) => ({ task_id: id, title: `Task ${id}`, crew, regression_tasks: by });
const wide = regressed('ORB-9', 'opus', ['ORB-90']);
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
    { cause: 'unclassified', kind: 'unclassified', tasks: 189 },
    { cause: 'run_failure', kind: 'attributed', tasks: 129 },
  ] },
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
};
const sent = await serve(full);
assert.equal(sent.pathname, '/api/codebase/analytics');
assert.deepEqual(
  ['window', 'min_support', 'rollup', 'workspace'].map(name => sent.searchParams.get(name)),
  ['30d', '5', 'dir', 'ws-a'],
  'the request carries the selection and the workspace',
);
assert.match(byId['codebase-meta'].textContent, /^4,933 tasks scanned · computed .+ · recomputed every 5 min$/);

// Seven tiles, each a toggle button; the overlap list is open by default.
assert.deepEqual(tiles().map(node => node.dataset.tile),
  ['overlap', 'regressions', 'footprint_misses', 'blocked', 'rejected', 'change_coupling', 'lock_contention']);
assert.deepEqual(tiles().map(node => node.getAttribute('aria-pressed')), ['true', 'false', 'false', 'false', 'false', 'false', 'false']);
assert.equal(tiles().every(node => node.tagName === 'BUTTON' && node.getAttribute('type') === 'button'), true);
assert.equal(tile('overlap').querySelector('span.codebase-tile-big').textContent, '5 paths');
assert.equal(tile('regressions').querySelector('span.codebase-tile-big').textContent, '65%');
assert.equal(tile('regressions').querySelector('span.codebase-tile-sub').textContent, 'top: a/');
assert.match(tile('regressions').querySelector('span.codebase-tile-foot').textContent, /^5 ranked · 1,775 /);
assert.ok(tile('blocked').querySelector('span.codebase-tile-foot.warn'), 'Blocked warns about its unclassified blocks');
assert.equal(tile('lock_contention').querySelector('span.codebase-tile-big').textContent, 'None');
assert.ok(tile('lock_contention').querySelector('span.codebase-tile-foot.good'), 'nothing to rank is the good state');

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
assert.deepEqual(tiles().map(node => node.getAttribute('aria-pressed')), ['false', 'true', 'false', 'false', 'false', 'false', 'false']);
assert.equal(part('list').dataset.ranking, 'regressions');
assert.deepEqual(rowPaths(), ['src/a/', 'src/b/', 'src/c/', 'src/d/', 'src/e/']);
assert.equal(rowPaths()[rows().indexOf(selectedRow())], 'src/b/');
assert.deepEqual(rows()[0].querySelectorAll('span.codebase-chip').map(node => node.dataset.ranking), ['footprint_misses', 'blocked', 'rejected'],
  'chips name the other rankings the path is in');
assert.equal(rows()[0].querySelector('span.codebase-frac').textContent, '13 of 20');
assert.equal(button(part('list'), 'Show paths with 5+ tasks'), undefined, 'already at 5, so no lower-support button');

click(tile('footprint_misses'));
assert.equal(rowPaths()[rows().indexOf(selectedRow())], 'src/c/', 'a row the new list lacks falls back to its first');
assert.ok(rows()[0].querySelector('span.codebase-extra'), 'widenings beyond the tasks show as an extra');
assert.equal(rows()[1].querySelector('span.codebase-extra'), null);

// Blocked: the cause bar and legend, and a warning because unclassified is the
// largest share.
click(tile('blocked'));
const legend = part('list').querySelectorAll('li.codebase-cause').map(item => [item.dataset.cause, item.dataset.kind]);
assert.deepEqual(legend, [['run_failure', 'attributed'], ['unclassified', 'unclassified'], ['sandbox', 'infrastructure']]);
assert.deepEqual(part('list').querySelector('div.codebase-causebar').children.map(span => span.dataset.kind), ['attributed', 'unclassified', 'infrastructure']);
assert.ok(part('list').querySelector('div.codebase-hint.warn'), 'unclassified is the largest share');

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
assert.equal(audit.buildAuditHash(), '#audit/codebase?window=7d&min_support=1&rollup=crate');

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
};
await serve(empty);
assert.equal(part('list').dataset.ranking, 'lock_contention', 'the open tile survives a new payload');
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
assert.equal(audit.buildAuditHash(), '#audit/codebase?window=7d&min_support=5&rollup=dir');

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

// The denial badge beside Audit shows the canonical count and routes to
// Events filtered to denials for the window it counted.
const badge = byId['rail-count-audit-denials'];
audit.renderDenialCount({ denials: 4, denials_sql: 3, denials_v2: 1, window: '7d' });
assert.equal(badge.hidden, false);
assert.equal(badge.textContent, '4 denied');
assert.match(badge.title, /3 audit rows, 1 loop denials/);
assert.equal(badge.dataset.route, 'audit/events?since=7d&status=denied');
const [route, query] = badge.dataset.route.split('?');
audit.applyAuditHashQuery(new URLSearchParams(query), route.split('/')[1]);
audit.setActiveAuditSubtabFromButton(route.split('/')[1]);
assert.equal(audit.buildAuditHash(), '#audit?since=7d&status=denied', 'the badge lands on Events filtered to denials');
audit.renderDenialCount({ denials: 0, window: '7d' });
assert.equal(badge.hidden, true, 'no denials, no badge');
assert.equal(badge.dataset.route, undefined);

console.log('codebase view, subtab swap and denial badge behaviors passed');
