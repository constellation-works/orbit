// Analytics › Codebase: its hash and view routes, the window / group-by
// segments and minimum-support select, the Hotspots table (flagged paths,
// the top of each signal, Show all, the path search, hidden columns, the
// regression filters and footnotes), the file view with its related files,
// Coupling & locks, the regression-risk marks view and a poll that keeps the
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
    // Text after an element child keeps its place, as a text node.
    if (typeof child === 'string' && !this.children.length) { this.own += child; return; }
    if (typeof child === 'string') { const text = new Node('#text'); text.own = child; child = text; }
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
  const tag = simple.match(/^[a-z][a-z0-9]*/i)?.[0];
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
make('p', 'codebase-purpose', byId['codebase-controls']);
make('span', 'codebase-meta', byId['codebase-controls']);
const views = make('nav', 'codebase-views', byId['codebase-controls']);
for (const value of ['hotspots', 'coupling']) make('a', null, views).dataset.view = value;
const toolbar = make('div', 'codebase-toolbar', byId['codebase-controls']);
const segments = { 'codebase-window': ['7d', '30d', '90d', 'all'], 'codebase-rollup': ['file', 'dir', 'crate'] };
for (const [id, values] of Object.entries(segments)) {
  const group = make('div', id, toolbar);
  for (const value of values) {
    const button = make('button', null, group);
    button.className = 'codebase-seg';
    button.dataset.value = value;
  }
}
make('select', 'codebase-min-support', toolbar);
make('input', 'codebase-path-search', make('label', 'codebase-search', toolbar));
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
const view = () => byId['codebase-body'];
// The panel's live status note sits before the rendered sections.
const parts = () => view().children.filter(node => !node.dataset.panelStatus);
const part = key => parts().find(node => node.dataset.key === key);
const click = node => { assert.ok(node, 'the control exists'); for (const listener of node.listeners.click || []) listener(); };
const change = (node, value) => { assert.ok(node, 'the control exists'); if (value !== undefined) node.value = value; for (const listener of node.listeners.change || []) listener(); };
const pressed = id => byId[id].children.filter(button => button.getAttribute('aria-pressed') === 'true').map(button => button.dataset.value);
const button = (scope, label) => scope.querySelectorAll('button').find(node => node.textContent === label);
const hotRows = () => part('hotspots').querySelectorAll('tr.codebase-hot-row');
const hotPaths = () => hotRows().map(row => row.dataset.path);
const sections = () => part('hotspots').querySelectorAll('td.codebase-hot-section').map(cell => cell.textContent);
const columns = () => part('hotspots').querySelectorAll('th.codebase-hot-signal').map(cell => cell.dataset.signal);
const cells = path => hotRows().find(row => row.dataset.path === path).querySelectorAll('td.codebase-cell-td')
  .map(cell => [cell.dataset.signal, cell.querySelector('span.codebase-cell').textContent]);
const notes = () => part('notes').querySelectorAll('p.codebase-note').map(node => node.textContent);
const tiles = () => Object.fromEntries(part('file-tiles').querySelectorAll('div.codebase-file-tile')
  .map(tile => [tile.dataset.signal, [tile.querySelector('span.codebase-tile-big').textContent, tile.querySelector('span.codebase-tile-sub').textContent]]));
const tasks = () => part('file-tasks').querySelectorAll('li.codebase-task');
const relatedBox = () => part('file-related');
let hashes = 0;
codebase.wireCodebaseControls(() => { hashes += 1; });
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
// The segment after `codebase` names the view; a file view needs its path.
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase', 'the default selection writes a bare #analytics/codebase');
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'));
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase?window=30d&min_support=5&rollup=dir');
assert.deepEqual([pressed('codebase-window'), pressed('codebase-rollup'), byId['codebase-min-support'].value], [['30d'], ['dir'], '5']);
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir&path=src/a/'), 'file');
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase/file?window=30d&min_support=5&rollup=dir&path=src%2Fa%2F');
assert.equal(byId['codebase-toolbar'].hidden, true, 'the file view has no toolbar');
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'), 'coupling');
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase/coupling?window=30d&min_support=5&rollup=dir');
assert.deepEqual([byId['codebase-toolbar'].hidden, byId['codebase-search'].hidden], [false, true], 'Coupling keeps the toolbar without the path search');
assert.deepEqual(views.children.map(link => link.getAttribute('aria-current')), ['false', 'page']);
assert.match(views.children[0].getAttribute('href'), /^\?.*workspace=ws-a.*#analytics\/codebase\?window=30d&min_support=5&rollup=dir$/, 'view tabs keep the scope and the selection');
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'), 'file');
assert.equal(codebase.getCodebaseRoute().view, 'hotspots', 'a file view without a path falls back to Hotspots');
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'), 'bogus');
assert.equal(codebase.getCodebaseRoute().view, 'hotspots', 'an unknown view falls back to Hotspots');

// The fixture's path signals share five paths in two or more of them, which
// the server lists in `overlap`: src/a/ in all four; src/c/, src/b/, src/y/
// and src/z/ in two. Regressions ranks four more paths, footprint misses one.
// Task ORB-9 regressed all five of the first ranked regression paths.
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
    { path: 'src/f/', ...rate(3, 20), regressed_tasks: [], by_crew: [] },
    { path: 'src/g/', ...rate(2, 20), regressed_tasks: [], by_crew: [] },
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
  ], unclassified_events: [{ event: 'claim_failed', tasks: 120 }], not_failures: 3 },
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
  overlap: { ranked_paths: 10, rows: [
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
assert.match(byId['codebase-meta'].textContent, /^4,933 tasks scanned · computed .+$/);

// Hotspots: filters, the table and its footnotes, nothing else. One column
// per signal with data; paths flagged by two or more signals first, in the
// server's order, then the top three of each signal that no other flags.
assert.deepEqual(parts().map(node => node.dataset.key), ['filters', 'hotspots', 'notes']);
assert.deepEqual(columns(), ['regressions', 'footprint_misses', 'blocked', 'rejected']);
assert.deepEqual(sections(), ['Flagged by two or more signals · 5', 'One signal · top 3 of each']);
assert.deepEqual(hotPaths(), ['src/a/', 'src/c/', 'src/b/', 'src/y/', 'src/z/', 'src/d/', 'src/e/', 'src/f/', 'src/x/']);
assert.deepEqual(cells('src/a/'), [['regressions', '65%13/20'], ['footprint_misses', '30%6/20'], ['blocked', '25%5/20'], ['rejected', '5.0%1/20']]);
assert.deepEqual(cells('src/d/'), [['regressions', '30%6/20'], ['footprint_misses', '—'], ['blocked', '—'], ['rejected', '—']], 'a signal that does not rank the path reads as a dash');
assert.match(hotRows()[0].querySelector('a.codebase-path-link').getAttribute('href'), /#analytics\/codebase\/file\?window=30d&min_support=5&rollup=dir&path=src%2Fa%2F$/, 'a path opens its deep-linked file view');
assert.equal(part('hotspots').querySelector('div.codebase-hot-foot').querySelector('span.codebase-extra').textContent, '9 of 10 ranked paths shown');
click(button(part('hotspots'), 'Show all 10'));
assert.deepEqual(hotPaths().slice(5), ['src/d/', 'src/e/', 'src/f/', 'src/g/', 'src/x/'], 'Show all lists every ranked path');
assert.equal(sections()[1], 'One signal · 5');
click(button(part('hotspots'), 'Show top 3 of each'));
assert.equal(hotPaths().length, 9);

// Removed by design: no ranking tiles, cross-tabs, crew or complexity rates
// and no standalone related-files search on the Codebase page.
const everything = () => view().querySelectorAll('div, section, table, ul, ol, button');
assert.equal(everything().filter(node => /codebase-(tile|crosstab|slice|crew-row|related-search)\b/.test(node.className) && !/codebase-file-tile/.test(node.className)).length, 0);
assert.doesNotMatch(view().textContent, /by crew|by complexity|Nature × escape|Escape × layer|What changes alongside/i);

// Footnotes: blocks that never count against a path, the marks and where
// defect trends are. The defects link claims no crew or complexity rates.
assert.deepEqual(notes().slice(0, 1), ['Blocks from infrastructure (7) and manual holds (40) never count against paths. Neither do 189 blocks with no recorded cause.']);
assert.match(notes().at(-1), /^Defect trends are on Metrics › Defects\.$/);
assert.match(part('notes').querySelectorAll('a').at(-1).getAttribute('href'), /#analytics\/metrics$/);
assert.doesNotMatch(part('notes').textContent, /crew|complexity/i);
assert.match(notes().at(-2), /nothing to compare/, 'before any mark there is nothing to compare');

// A poll that answers the same payload keeps every node.
const before = parts();
await serve(full);
assert.deepEqual(parts().map((node, index) => node === before[index]), before.map(() => true), 'the same payload keeps every node');

// A signal with no data in the window is hidden and the footnote says why.
const quiet = JSON.parse(JSON.stringify(full));
quiet.rejected = { below_support: 0, rejected_with_run: 0, excluded_without_run: 3, rows: [] };
quiet.overlap.rows = quiet.overlap.rows.map(row => ({ ...row, rankings: row.rankings.filter(key => key !== 'rejected') })).filter(row => row.rankings.length >= 2);
await serve(quiet);
assert.deepEqual(columns(), ['regressions', 'footprint_misses', 'blocked']);
assert.equal(notes()[0], 'No task was rejected after a run in this window, so that column is hidden.');
assert.deepEqual(hotPaths().slice(0, 2), ['src/a/', 'src/c/'], 'only paths two shown signals flag stay flagged');
assert.ok(hotPaths().includes('src/b/') && hotPaths().includes('src/y/'), 'a path one shown signal ranks moves to One signal');
await serve(full);

// The path search narrows the table to every ranked match and offers known
// paths no signal ranks.
const search = byId['codebase-path-search'];
const type = value => { search.value = value; for (const listener of search.listeners.input) listener(); };
await serve({ ...full, known_paths: ['src/b/', 'src/bz.rs', 'docs/b.md'] });
type('src/b');
assert.deepEqual(hotPaths(), ['src/b/']);
assert.equal(part('hotspots').querySelector('div.codebase-hot-foot').querySelector('span.codebase-extra').textContent, '1 of 10 ranked paths match');
assert.deepEqual(part('hotspots').querySelectorAll('ul.codebase-other-paths a').map(link => link.dataset.path), ['src/bz.rs'], 'an unranked known path can be opened');
type('nothing-here');
assert.match(part('hotspots').textContent, /No ranked path matches “nothing-here”\./);
type('');
await serve(full);

// Regression filters: one select per dimension with each value's link count
// (absent and unclassified apart, zero counts hidden). Choosing one sends it,
// writes it to the hash and filters the Regressed column; Clear resets all.
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
  by_crew: { rows: [{ crew: 'opus', ...rate(3, 10), support: 10 }], below_support: 2 },
  by_complexity: { rows: [], below_support: 3 },
});
sliced.regressions.rows[0].regressed_tasks[1] = {
  ...regressed('ORB-1', 'sol', ['ORB-2', 'ORB-3']),
  regressions: [link('ORB-2', 'broke_existing', 'no_test', 'dashboard', 'delivery_review'), link('ORB-3', 'absent', 'absent', 'store_migration', 'operator', 'footprint')],
};
await serve(sliced);
const filter = dimension => part('filters').querySelectorAll('select.codebase-select').find(node => node.dataset.dimension === dimension);
const options = dimension => filter(dimension).children.map(option => [option.value, option.textContent]);
assert.deepEqual(options('nature'), [['', 'Any'], ['broke_existing', 'broke existing · 3'], ['unclassified', 'unclassified · 1'], ['absent', 'absent · 2']],
  'a value no link takes is hidden; absent and unclassified are separate options');
assert.deepEqual(options('layer').map(([, text]) => text), ['Any', 'dashboard · 4', 'store/migration · 2']);
assert.deepEqual(options('detected_by').map(([value]) => value), ['', 'delivery_review', 'operator']);
assert.doesNotMatch(view().textContent, /opus.*30%/, 'crew rates in the payload are not rendered');
hashes = 0;
change(filter('nature'), 'broke_existing');
assert.equal(codebase.getCodebaseQuery().nature, 'broke_existing');
assert.equal(hashes, 1, 'a filter asks for a new hash');
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase?window=30d&min_support=5&rollup=dir&nature=broke_existing');
const narrowed = JSON.parse(JSON.stringify(sliced));
narrowed.regression_filter = { ...noFilter, nature: 'broke_existing' };
narrowed.regressions.rows = [{ path: 'src/a/', ...rate(1, 20), by_crew: [], regressed_tasks: [{ ...regressed('ORB-1', 'sol', ['ORB-2']), regressions: [link('ORB-2', 'broke_existing', 'no_test', 'dashboard', 'delivery_review')] }] }];
Object.assign(narrowed.regressions, { matching_links: 3, regressed: 2 });
const filteredRequest = await serve(narrowed);
assert.equal(filteredRequest.searchParams.get('nature'), 'broke_existing', 'the filter goes to the server');
assert.equal(filteredRequest.searchParams.get('escape'), null, 'an unset filter is not sent');
assert.equal(filter('nature').value, 'broke_existing');
assert.equal(part('filters').querySelector('span.codebase-filter-count').textContent, '3 of 6 regression links on 2 regressed tasks');
assert.deepEqual(cells('src/a/')[0], ['regressions', '5.0%1/20'], 'the Regressed column is the filtered one');
change(filter('layer'), 'store_migration');
click(button(part('filters'), 'Clear filters'));
assert.deepEqual(['nature', 'escape', 'layer', 'detected_by'].map(name => codebase.getCodebaseQuery()[name]), [null, null, null, null]);
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir&escape=no_test&layer=bogus'));
assert.deepEqual([codebase.getCodebaseQuery().escape, codebase.getCodebaseQuery().layer], ['no_test', null], 'a hash restores known filters only');
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'));

// A filter that leaves no regressed path keeps the column, so the filter row
// still says what it left; a window without links has nothing to filter.
const nothing = JSON.parse(JSON.stringify(sliced));
nothing.regression_filter = { ...noFilter, layer: 'store_migration' };
nothing.regressions.rows = [];
Object.assign(nothing.regressions, { matching_links: 0, regressed: 0 });
await serve(nothing);
assert.deepEqual(columns()[0], 'regressions');
assert.ok(button(part('filters'), 'Clear filters'));
const noLinks = JSON.parse(JSON.stringify(full));
noLinks.regression_filter = noFilter;
Object.assign(noLinks.regressions, { links: 0, matching_links: 0, regressed: 0, breakdowns: {} });
await serve(noLinks);
assert.equal(part('filters').querySelectorAll('select').length, 0);
assert.match(part('filters').textContent, /no regression link targets a task done in this window/);

// Regression-risk marks: one footnote line against the 30 days before, and
// a link to the per-path comparison, which is its own view.
const marked = JSON.parse(JSON.stringify(sliced));
marked.regressions.risk_marks = {
  enabled_at: '2026-10-01T00:00:00Z', before_since: '2026-09-01T00:00:00Z', window_days: 30, marked_paths: 2,
  with_marks: rate(1, 4), before: rate(6, 10),
  rows: [{ path: 'src/a/x.rs', with_marks: rate(1, 3), before: rate(5, 8) }, { path: 'src/b.rs', with_marks: rate(0, 1), before: rate(0, 0) }],
};
await serve(marked);
assert.equal(notes().at(-2), 'Regression-risk marks: 25% (1 of 4) of marked tasks regressed, against 60% (6 of 10) in the 30 days before marking. Per-path comparison');
hashes = 0;
click(part('notes').querySelectorAll('a').find(node => node.textContent === 'Per-path comparison'));
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase/marks?window=30d&min_support=5&rollup=dir');
assert.equal(hashes, 1, 'opening a view asks for its hash');
const riskTable = () => part('marks').querySelector('table.codebase-risk-table');
assert.deepEqual(riskTable().querySelectorAll('tbody tr').map(row => [row.dataset.path, ...row.querySelectorAll('td').map(cell => cell.querySelector('span.codebase-frac')?.textContent ?? cell.textContent)]),
  [['', '1/4', '6/10'], ['src/a/x.rs', '1/3', '5/8'], ['src/b.rs', '0/1', 'no tasks']], 'each rate shows its counts; an empty sample says so');
assert.deepEqual(riskTable().querySelectorAll('thead th').map(cell => cell.textContent), ['', 'With marks', '30 days before']);
click(view().querySelector('a.codebase-back'));
assert.equal(codebase.getCodebaseRoute().view, 'hotspots', 'the back link returns to Hotspots');
await serve(sliced);

// The file view: the path with Copy path, a tile per signal with its rate,
// counts and rank, and the regressed tasks with what regressed them. A
// directory key has no file partners to rank.
const requestsBeforeFile = pending.length;
click(hotRows()[0].querySelector('a.codebase-path-link'));
assert.deepEqual({ ...codebase.getCodebaseRoute() }, { view: 'file', path: 'src/a/' });
assert.deepEqual(parts().map(node => node.dataset.key), ['file-head', 'file-tiles', 'file-tasks', 'file-related']);
assert.equal(part('file-head').querySelector('h3').textContent, 'a/');
assert.deepEqual(tiles(), {
  regressions: ['65%', '13 of 20 done tasks · #1 of 7'],
  footprint_misses: ['30%', '6 of 20 declared · #3 of 4'],
  blocked: ['25%', '5 of 20 declared · #1 of 2'],
  rejected: ['5.0%', '1 of 20 with a run · #4 of 4'],
});
assert.deepEqual(tasks().map(item => item.dataset.task), ['ORB-9', 'ORB-1', 'ORB-4'], 'three regressed tasks before All');
assert.deepEqual(tasks().map(item => item.dataset.wide ?? null), ['5', null, null], 'the task that regressed five ranked paths is marked');
assert.deepEqual(tasks()[1].querySelectorAll('a.codebase-task-link').map(node => node.textContent), ['ORB-1', 'ORB-2', 'ORB-3']);
assert.match(tasks()[1].querySelector('a.codebase-task-link').getAttribute('href'), /#tasks\?open=ORB-1$/);
assert.deepEqual(tasks()[1].querySelectorAll('div.codebase-task-by').map(line => [line.dataset.task, line.querySelectorAll('span.codebase-by-dim').map(tag => tag.dataset.value)]),
  [['ORB-2', ['broke_existing', 'dashboard', 'delivery_review']], ['ORB-3', ['absent', 'store_migration', 'operator']]], 'each regression names its nature, layer and detector');
assert.match(tasks()[1].querySelectorAll('span.codebase-by-dim')[4].title, /declared footprint/, 'a layer read from the footprint says so');
assert.equal(tasks()[2].querySelector('span.codebase-crew').textContent, 'unassigned');
const all = button(part('file-tasks'), 'All 4');
assert.equal(all.getAttribute('aria-expanded'), 'false');
click(all);
assert.equal(tasks().length, 4, 'All lists every regressed task');
click(button(part('file-head'), 'Copy path'));
await settle();
assert.deepEqual(copied, ['src/a/']);
assert.ok(button(part('file-head'), 'Copied'), 'the copy action confirms');
assert.match(relatedBox().textContent, /Group by file to see what lands alongside one/);
assert.equal(pending.length, requestsBeforeFile, 'a directory key asks for no related files');
assert.doesNotMatch(view().textContent, /Regressions by crew/);

// A path no signal ranks under reads "not in the top 25"; a new path starts
// with its tasks collapsed.
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir&path=src/b/'), 'file');
assert.deepEqual(tiles().footprint_misses, ['—', 'not in the top 25']);
assert.equal(tasks().length, 1, 'a new path starts collapsed');
const fileNodes = parts();
await serve(sliced);
assert.ok(parts().every((node, index) => node === fileNodes[index]), 'a poll keeps the file view');

// At file grouping the file view reads the related files at the selected
// window and minimum, and lists the partners with their evidence.
const fileRollup = { ...full, rollup: 'file', regressions: { ...full.regressions, rows: [{ path: 'src/a.rs', ...rate(3, 6), regressed_tasks: [], by_crew: [] }] }, overlap: { rows: [], ranked_paths: 1 } };
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=10&rollup=file&path=src/a.rs'), 'file');
await serve(fileRollup);
let relatedRequest = pending.at(-1);
assert.equal(relatedRequest.url.pathname, '/api/codebase/related');
assert.deepEqual(['path', 'window', 'min_support', 'top', 'workspace'].map(name => relatedRequest.url.searchParams.get(name)), ['src/a.rs', '30d', '10', '10', 'ws-a']);
assert.match(relatedBox().textContent, /Loading related files/);
relatedRequest.resolve(response({ path: 'src/a.rs', path_support: 12, min_support: 10, rows: [
  { path: 'docs/design.md', jaccard: 0.5, conditional_rate: 1, support: { both: 10, path: 12, partner: 18, either: 20 }, footprint_miss_count: 1, tasks: [{ task_id: 'ORB-9', title: '<img src=x onerror=bad()>', commit: 'abc' }], footprint_miss_tasks: [] },
  { path: 'src/b.rs', jaccard: 0.25, conditional_rate: 0.5, support: { both: 10, path: 12, partner: 38, either: 40 }, footprint_miss_count: 2, tasks: [], footprint_miss_tasks: [] },
] }));
await settle();
assert.deepEqual(relatedBox().querySelectorAll('li.codebase-related-row').map(row => [row.dataset.path, row.querySelector('span.codebase-frac').textContent]), [['docs/design.md', '10/20'], ['src/b.rs', '10/40']]);
assert.equal(relatedBox().querySelector('a.codebase-task-link').textContent, '<img src=x onerror=bad()>', 'task titles stay text');
assert.match(relatedBox().querySelector('a.codebase-task-link').getAttribute('href'), /open=ORB-9$/);
assert.match(relatedBox().querySelector('a.codebase-path-link').getAttribute('href'), /#analytics\/codebase\/file\?.*path=docs%2Fdesign\.md$/, 'a partner opens its own file view');

// An unchanged poll leaves the related box and its open evidence in place,
// without another request; a changed payload refetches.
const evidence = relatedBox().querySelector('details');
evidence.open = true;
const relatedNode = relatedBox();
const relatedRequests = () => pending.filter(request => request.url.pathname === '/api/codebase/related').length;
const requestsBeforePoll = relatedRequests();
await serve(fileRollup);
assert.equal(relatedRequests(), requestsBeforePoll, 'an unchanged analytics poll does not refetch related files');
assert.equal(relatedBox(), relatedNode, 'an unchanged analytics poll keeps the related box');
assert.equal(evidence.open, true);
await serve({ ...fileRollup, computed_at: '2026-10-10T12:05:00.000Z' });
assert.equal(relatedRequests(), requestsBeforePoll + 1, 'a refreshed payload refreshes the related files');

// Below the minimum, the box gives the real reason with the support numbers
// and offers a wider window and a lower minimum.
pending.at(-1).resolve(response({ path: 'src/a.rs', path_support: 3, min_support: 10, unavailable_landed_tasks: 2, large_footprint_cap: 20, rows: [], empty_reason: { kind: 'path_below_minimum', path_support: 3, min_support: 10 } }));
await settle();
assert.equal(relatedBox().querySelector('p.codebase-empty').textContent, 'Only 3 landed tasks changed this file in the last 30 days; ranking partners needs 10.');
assert.match(relatedBox().textContent, /2 done tasks without a recorded landing, finished by hand or before Orbit recorded landings/);
assert.ok(button(relatedBox(), 'Lower minimum to 3'));
click(button(relatedBox(), 'Use all time'));
assert.equal(codebase.getCodebaseQuery().window, 'all');
const widerRequest = await serve({ ...fileRollup, window: 'all' });
assert.equal(widerRequest.pathname, '/api/codebase/related');
assert.equal(widerRequest.searchParams.get('window'), 'all', 'the wider window refetches the related files');
pending.at(-1).resolve(response({ path: 'src/a.rs', path_support: 0, rows: [], empty_reason: { kind: 'no_landed_change' } }));
await settle();
assert.match(relatedBox().querySelector('p.codebase-empty').textContent, /^No landed task with a recorded landing changed this file in all recorded history\.$/);
assert.equal(relatedBox().querySelectorAll('button').length, 0, 'nothing wider or lower can help a file with no landed change');

// A failed related request is retried on the next poll even when its payload
// has not changed; a late result cannot cross workspace visits.
await serve({ ...fileRollup, window: 'all', computed_at: '2026-10-10T12:06:00.000Z' });
pending.at(-1).resolve(response({ error: 'temporary failure' }, 503));
await settle();
assert.match(relatedBox().textContent, /Could not load related files: temporary failure/);
const retry = await serve({ ...fileRollup, window: 'all', computed_at: '2026-10-10T12:06:00.000Z' });
assert.equal(retry.pathname, '/api/codebase/related', 'a failed related request retries on the next poll');
relatedRequest = pending.at(-1);
common.setWorkspace('ws-b');
relatedRequest.resolve(response({ path: 'src/a.rs', path_support: 4, rows: [] }));
await settle();
common.setWorkspace('ws-a');
await settle();
codebase.applyCodebaseHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'), 'coupling');

// Coupling & locks: reserved-together pairs with the release-bump files
// folded into one row by default, and lock waits.
const coupled = JSON.parse(JSON.stringify(full));
coupled.change_coupling.rows = [
  { paths: ['Cargo.lock', 'Cargo.toml'], ...rate(20, 20), reservations: [20, 20] },
  ...coupled.change_coupling.rows,
  { paths: ['CHANGELOG.md', 'Cargo.toml'], ...rate(8, 19), reservations: [8, 20] },
];
await serve(coupled);
assert.deepEqual(parts().map(node => node.dataset.key), ['pairs', 'locks']);
const pairRows = () => part('pairs').querySelectorAll('li.codebase-pair-row').map(row => row.dataset.pair);
const releaseRow = () => part('pairs').querySelector('li.codebase-release-row');
assert.deepEqual(pairRows(), ['src/web/\nsrc/web/tests/', 'src/a/\nsrc/b/']);
assert.equal(releaseRow().querySelector('span.codebase-release-title').textContent, 'Release bump set · 3 files');
assert.equal(releaseRow().querySelector('span.codebase-release-range').textContent, '42%–100%');
assert.ok(part('pairs').querySelectorAll('li.codebase-pair-row')[0].querySelector('span.codebase-tag'), 'a directory and its tests is tagged as expected');
const groupRelease = part('pairs').querySelector('input.codebase-check');
assert.equal(groupRelease.checked, true, 'release files are grouped by default');
groupRelease.checked = false;
change(groupRelease);
assert.equal(releaseRow(), null);
assert.equal(pairRows().length, 4, 'ungrouped, every pair is listed');
assert.ok(part('locks').querySelector('div.codebase-empty-state.good'), 'no refused lock is the good state');
assert.match(part('locks').textContent, /2,557 file reservations/);

// Lock waits: total, median and denied of requesters per row, sorted by
// total wait by default and switchable to the denial rate. A payload without
// waits shows rates and no sort.
const lockRow = (path, numerator, denominator, extra) => ({ path, ...rate(numerator, denominator), contending_tasks: numerator, reservations: 9, denials: 40, ...extra });
const lockPaths = () => part('locks').querySelectorAll('li.codebase-lock-row').map(row => row.dataset.path);
const lockLines = () => part('locks').querySelectorAll('li.codebase-lock-row').map(row => [row.querySelector('span.codebase-lock-total').textContent, row.querySelector('div.codebase-lock-line').textContent]);
const legacyLock = JSON.parse(JSON.stringify(full));
legacyLock.lock_contention = { below_support: 0, reservations: 30, denials: 80, wait_time_recorded: false, rows: [lockRow('src/hot.rs', 3, 4, {}), lockRow('src/slow.rs', 1, 5, {})] };
await serve(legacyLock);
assert.deepEqual(lockPaths(), ['src/hot.rs', 'src/slow.rs']);
assert.deepEqual(lockLines()[0], ['75%', '3 of 4 requesters denied']);
assert.equal(part('locks').querySelector('div.codebase-segs'), null, 'nothing to sort by without waits');
assert.match(part('locks').textContent, /Lock wait time is not recorded, so waits cannot be shown\./);
const waited = JSON.parse(JSON.stringify(full));
const hot = lockRow('src/hot.rs', 3, 4, { wait: { episodes: 3, waiting: 0, median_ms: 120000, max_ms: 600000, total_ms: 840000 } });
const slow = lockRow('src/slow.rs', 1, 5, { wait: { episodes: 1, waiting: 1, median_ms: 9000000, max_ms: 9000000, total_ms: 9000000 } });
const runOnly = lockRow('src/run.rs', 1, 6, { wait: null });
waited.lock_contention = { below_support: 0, reservations: 30, denials: 80, wait_time_recorded: true, rows: [hot, slow, runOnly], rows_by_wait: [slow, hot] };
await serve(waited);
const sort = label => button(part('locks'), label);
assert.deepEqual(lockPaths(), ['src/slow.rs', 'src/hot.rs'], 'longest total wait first by default');
assert.deepEqual(lockLines(), [['2 h 30 min', 'median wait 2 h 30 min · 1 of 5 requesters denied1 still waiting'], ['14 min', 'median wait 2 min · 3 of 4 requesters denied']]);
assert.deepEqual([sort('Total wait').getAttribute('aria-pressed'), sort('Denial rate').getAttribute('aria-pressed')], ['true', 'false']);
click(sort('Denial rate'));
assert.deepEqual(lockPaths(), ['src/hot.rs', 'src/slow.rs', 'src/run.rs'], 'the denial rate restores the rate order');
assert.deepEqual(lockLines()[2], ['—', '1 of 6 requesters denied'], 'a row whose denials named no task has no wait');
await serve(waited);
assert.deepEqual(lockPaths(), ['src/hot.rs', 'src/slow.rs', 'src/run.rs'], 'a poll keeps the chosen sort');
click(sort('Total wait'));
assert.match(part('locks').textContent, /first denial to its admission or its leaving the backlog/);

// The window and group-by segments and the select rewrite the selection and
// ask for a new hash; an unchanged one asks for nothing.
hashes = 0;
const segment = (id, value) => byId[id].children.find(node => node.dataset.value === value);
click(segment('codebase-rollup', 'crate'));
click(segment('codebase-window', '7d'));
click(segment('codebase-window', '7d'));
assert.equal(hashes, 2, 'an unchanged segment asks for no new hash');
assert.deepEqual([pressed('codebase-window'), pressed('codebase-rollup')], [['7d'], ['crate']]);
change(byId['codebase-min-support'], '30');
assert.equal(codebase.getCodebaseQuery().min_support, 30);
assert.deepEqual(byId['codebase-min-support'].children.map(option => option.value), ['1', '2', '3', '5', '10', '15', '20', '30', '50', '100']);
codebase.applyCodebaseHashQuery(new URLSearchParams('window=7d&min_support=7&rollup=crate'), 'coupling');
assert.ok(byId['codebase-min-support'].children.some(option => option.value === '7'), 'a hashed minimum off the ladder is offered');
assert.equal(byId['codebase-min-support'].value, '7');
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase/coupling?window=7d&min_support=7&rollup=crate');

// An empty coupling ranking offers directory grouping and a lower minimum;
// both rewrite the selection. Nothing ranked anywhere is its own state.
const empty = {
  window: '7d', rollup: 'crate', min_support: 15, touched: 'declared_footprint', tasks_scanned: 0,
  computed_at: '2026-10-10T12:00:00.000Z', cache_ttl_seconds: 300, row_limit: 25,
  regressions: { rows: [], below_support: 0, population: 0 },
  lock_contention: { rows: [], below_support: 0, reservations: 0, denials: 0, wait_time_recorded: false },
  footprint_misses: { rows: [], below_support: 0, population: 0 },
  change_coupling: { rows: [], below_support: 13051, reservations: 0, excluded_large: 250, large_footprint_cap: 12 },
  blocked: { rows: [], below_support: 0, causes: [] },
  rejected: { rows: [], below_support: 0, rejected_with_run: 0, excluded_without_run: 0 },
  overlap: { rows: [], ranked_paths: 0 },
};
codebase.applyCodebaseHashQuery(new URLSearchParams('window=7d&min_support=15&rollup=crate'), 'coupling');
await serve(empty);
assert.ok(part('pairs').querySelector('div.codebase-empty-state'));
hashes = 0;
click(button(part('pairs'), 'Group by directory'));
assert.equal(codebase.getCodebaseQuery().rollup, 'dir');
click(button(part('pairs'), 'Lower to 5'));
assert.equal(codebase.getCodebaseQuery().min_support, 5);
assert.equal(hashes, 2, 'each action asks for a new hash');
assert.equal(codebase.buildCodebaseHash(), '#analytics/codebase/coupling?window=7d&min_support=5&rollup=dir');
codebase.applyCodebaseHashQuery(new URLSearchParams('window=7d&min_support=15&rollup=crate'));
await serve(empty);
assert.match(part('hotspots').querySelector('p.codebase-empty-title').textContent, /^No path reaches minimum support 15 under any signal in this window\.$/);
assert.ok(button(part('hotspots'), 'Show paths with 5+ tasks'), 'a minimum above 5 offers to lower it');
assert.match(part('notes').textContent, /At crate grouping nearly every crate clears the minimum/);

// The aggregate view shows the placeholder instead of the codebase view.
common.setWorkspace('');
await settle();
const requests = pending.length;
await codebase.fetchAndRenderCodebase();
assert.equal(pending.length, requests, 'the aggregate view requests nothing');
assert.deepEqual(parts().map(node => node.dataset.key), ['aggregate-placeholder']);
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
