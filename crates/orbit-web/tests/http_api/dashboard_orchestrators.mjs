// Analytics › Orchestrators: the scorecard's definition tiles (components
// first, the composite and its weights last), the per-100 / per-8h toggle,
// greyed below-support rows that sort last, and the drill-down from a count
// to its tasks, cached per orchestrator, with its failure state.
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
const layout = make('main', 'analytics-orchestrators-main', body);
for (const id of ['orchestrators-count', 'orchestrators-meta', 'orchestrators-body']) make('div', id, layout);
const windows = make('div', 'orchestrators-window-selector', layout);
for (const value of ['1h', '24h', '7d', '30d', 'all']) {
  const button = make('button', null, windows);
  button.className = 'scoreboard-window-seg';
  button.dataset.window = value;
}
const rateToggle = make('div', 'orchestrators-rate', layout);
for (const value of ['per_100_tasks', 'per_8_hours']) {
  const button = make('button', null, rateToggle);
  button.className = 'scoreboard-window-seg';
  button.dataset.value = value;
}
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
const window = { location: { search: '?window=30d', hash: '', href: 'http://dashboard.test/?window=30d' } };
const history = { replaceState() {}, pushState() {} };
const pending = [];
const response = (payload, status = 200) => ({ ok: status >= 200 && status < 300, status, headers: { get: () => null }, json: async () => payload, text: async () => JSON.stringify(payload) });
const fetch = (path, options = {}) => new Promise(resolve => pending.push({ url: new URL(path, 'http://dashboard.test'), options, resolve }));
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, fetch, document, window, history, Node, Intl,
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
await load('orchestrators.js').link(specifier => load(specifier.replace(/^\.\//, '')));
await load('orchestrators.js').evaluate();
const orchestrators = modules.get('orchestrators.js').namespace;
const common = modules.get('common.js').namespace;
const settle = async () => { for (let i = 0; i < 6; i += 1) await new Promise(resolve => setTimeout(resolve, 0)); };
const click = node => { assert.ok(node, 'the control exists'); for (const listener of node.listeners.click || []) listener(); };
const view = () => byId['orchestrators-body'];
const part = key => view().children.find(node => node.dataset.key === key);
const rows = () => part('table').querySelectorAll('tr.orch-row');
const rowNames = () => rows().map(row => row.dataset.orchestrator);
const row = name => rows().find(node => node.dataset.orchestrator === name);
const cell = (name, key) => row(name).children.find(node => node.dataset.component === key);
const cellButton = (name, key) => cell(name, key).querySelector('button.orch-cell');
const rateText = (name, key) => cell(name, key).querySelector('span.orch-rate').textContent;
const headers = () => part('table').querySelectorAll('thead tr')[1].children.map(th => th.textContent);
const sortButton = key => part('table').querySelectorAll('button.orch-sort').find(node => node.dataset.sort === key);
const rateButton = value => rateToggle.children.find(button => button.dataset.value === value);
const detailTasks = () => part('detail').querySelectorAll('li.orch-task').map(item => item.dataset.taskId);

const definitions = {
  groups: [
    { key: 'throughput', label: 'Throughput' },
    { key: 'proactivity', label: 'Proactivity' },
    { key: 'spec_defects', label: 'Spec defects' },
    { key: 'rework', label: 'Rework' },
  ],
  components: [
    { key: 'owned', group: 'throughput', label: 'Owned', definition: 'Tasks it owns.' },
    { key: 'weighted_landed', group: 'throughput', label: 'Weighted landed', definition: 'Landed by complexity.' },
    { key: 'landed_initiative', group: 'proactivity', label: 'Landed initiative', definition: 'Initiative that landed.' },
    { key: 'spec_edits', group: 'spec_defects', label: 'Edited after run start', definition: 'Edits after the first run.' },
    { key: 'spec_defects', group: 'spec_defects', label: 'Spec defects', definition: 'All spec defects.' },
    { key: 'review_rejects', group: 'rework', label: 'Review rejects', definition: 'Review gate rejects.' },
    { key: 'rework', group: 'rework', label: 'Rework', definition: 'All rework.' },
  ],
  causes: [{ key: 'spec', definition: 'The spec.' }, { key: 'crew', definition: 'The crew.' }],
  complexity_weights: { low: 1, medium: 2, hard: 4, xhard: 8, unassessed: 1 },
  composite: {
    definition: 'Weighted landed + landed initiative − spec defects − rework.',
    weights: [
      { key: 'weighted_landed', weight: 1 },
      { key: 'landed_initiative', weight: 1 },
      { key: 'spec_defects', weight: -1 },
      { key: 'rework', weight: -1 },
    ],
  },
  normalization: { per_100_tasks: 'Count × 100 ÷ owned.', per_8_hours: 'Count × 8 ÷ active hours.' },
};
const scorecardRow = (name, below, counts, per100, per8h, composite) => ({
  orchestrator: name, below_support: below,
  support: { owned_tasks: counts.owned, tasks_with_runs: counts.owned, active_hours: per8h ? 2.5 : 0, active_8h_windows: 0.31 },
  counts, per_100_tasks: per100, per_8_hours: per8h,
  landed_share: 1, origins: { operator_request: 1, orchestrator_initiative: 1, automation: 0, unknown: 1 },
  rework_by_cause: { spec: 2, crew: 1, infrastructure: 0, other: 0 },
  composite,
});
const scorecard = {
  window: '30d', since: '2026-09-18T00:00:00Z', min_support: 10, computed_at: '2026-10-18T00:00:00.000Z',
  tasks_scanned: 40, attributed_tasks: 31, unattributed_tasks: 9, cache_ttl_seconds: 300, definitions,
  rows: [
    scorecardRow('opus', false,
      { owned: 30, weighted_landed: 40, landed_initiative: 5, spec_edits: 2, spec_defects: 4, review_rejects: 1, rework: 3 },
      { owned: 100, weighted_landed: 133.33, landed_initiative: 16.67, spec_edits: 6.67, spec_defects: 13.33, review_rejects: 3.33, rework: 10 },
      { owned: 96, weighted_landed: 128, landed_initiative: 16, spec_edits: 6.4, spec_defects: 12.8, review_rejects: 3.2, rework: 9.6 },
      { value: 38, per_100_tasks: 126.67, per_8_hours: 121.6 }),
    scorecardRow('sonnet', true,
      { owned: 1, weighted_landed: 8, landed_initiative: 1, spec_edits: 1, spec_defects: 1, review_rejects: 0, rework: 0 },
      { owned: 100, weighted_landed: 800, landed_initiative: 100, spec_edits: 100, spec_defects: 100, review_rejects: 0, rework: 0 },
      { owned: null, weighted_landed: null, landed_initiative: null, spec_edits: null, spec_defects: null, review_rejects: null, rework: null },
      { value: 8, per_100_tasks: 800, per_8_hours: null }),
  ],
};
const detailFor = (name, components) => ({
  window: '30d', min_support: 10, orchestrator: name, definitions, row: scorecard.rows[0],
  components: definitions.components.map(component => ({ key: component.key, group: component.group, label: component.label, count: (components[component.key] || []).length, tasks: components[component.key] || [] })),
});
const task = (id, occurrences) => ({ task_id: id, title: `Task ${id}`, status: 'done', origin: 'orchestrator_initiative', complexity: 'hard', created_at: '2026-10-10T00:00:00Z', count: 1, occurrences });

common.setMultiWorkspace(true);
common.setWorkspace('ws-a');
common.setWindow('30d');
await settle();
orchestrators.wireOrchestratorControls();
assert.equal(rateButton('per_100_tasks').getAttribute('aria-pressed'), 'true', 'per 100 owned tasks is the default rate');

// The scorecard request carries the shared window and the minimum support.
const loading = orchestrators.fetchAndRenderOrchestrators();
const request = pending.at(-1);
assert.equal(request.url.pathname, '/api/orchestrators/scorecard');
assert.equal(request.url.searchParams.get('window'), '30d');
assert.equal(request.url.searchParams.get('min_support'), '10');
assert.equal(request.url.searchParams.get('workspace'), 'ws-a');
request.resolve(response(scorecard));
await loading;
assert.equal(byId['orchestrators-count'].textContent, '2', 'one row per orchestrator');
assert.match(byId['orchestrators-meta'].textContent, /31 attributed tasks created in 30d · 9 tasks with no orchestrator/);

// Definitions sit in the tiles: one per component group, then the composite
// with its weights.
const tiles = part('tiles').children;
assert.deepEqual(tiles.map(tile => tile.dataset.group), ['throughput', 'proactivity', 'spec_defects', 'rework', 'composite']);
assert.match(tiles[2].textContent, /Edited after run startEdits after the first run\./, 'a component tile defines each of its counts');
assert.deepEqual(tiles[4].querySelectorAll('ul.orch-weights li').map(item => item.textContent),
  ['+1 × Weighted landed', '+1 × Landed initiative', '−1 × Spec defects', '−1 × Rework']);
assert.equal(tiles[4].querySelector('p.orch-norm').textContent, 'Count × 100 ÷ owned.');

// Components come first, in the definitions' order, and the composite last.
assert.deepEqual(headers(), ['Orchestrator', 'Support', 'Owned', 'Weighted landed', 'Landed initiative', 'Edited after run start', 'Spec defects', 'Review rejects', 'Rework', 'Composite /100']);
assert.deepEqual(rowNames(), ['opus', 'sonnet']);
assert.ok(row('sonnet').classList.contains('orch-row-below'), 'a row below the minimum support is greyed');
assert.match(row('sonnet').textContent, /below support/);
assert.ok(!row('opus').classList.contains('orch-row-below'));
assert.equal(rateText('opus', 'spec_edits'), '6.7');
assert.match(cell('opus', 'rework').textContent, /spec 2 · crew 1/, 'rework shows its spec and crew split');
assert.equal(cell('opus', 'rework').title, 'Rework by cause: spec 2, crew 1, infrastructure 0, other 0');
assert.equal(cellButton('opus', 'review_rejects') != null, true, 'a non-zero count opens its tasks');
assert.equal(cellButton('sonnet', 'review_rejects'), null, 'a zero count has nothing to open');

// The toggle re-renders every count per 8 active hours; a row with no active
// time has no such rate.
click(rateButton('per_8_hours'));
assert.equal(rateButton('per_8_hours').getAttribute('aria-pressed'), 'true');
assert.equal(rateButton('per_100_tasks').getAttribute('aria-pressed'), 'false');
assert.equal(rateText('opus', 'spec_edits'), '6.4');
assert.equal(rateText('sonnet', 'spec_edits'), '—');
assert.equal(headers().at(-1), 'Composite /8h');
assert.equal(part('tiles').children[4].querySelector('p.orch-norm').textContent, 'Count × 8 ÷ active hours.');
click(rateButton('per_100_tasks'));
assert.equal(rateText('opus', 'spec_edits'), '6.7');

// Sorting by the composite keeps a below-support row last even when its rate
// leads; a second click reverses, a third restores the server's order.
click(sortButton('composite'));
assert.deepEqual(rowNames(), ['opus', 'sonnet']);
assert.equal(sortButton('composite').parentNode.getAttribute('aria-sort'), 'descending');
click(sortButton('composite'));
assert.equal(sortButton('composite').parentNode.getAttribute('aria-sort'), 'ascending');
assert.deepEqual(rowNames(), ['opus', 'sonnet']);
click(sortButton('composite'));
assert.equal(sortButton('composite').parentNode.getAttribute('aria-sort'), 'none');

// A count opens its tasks from the detail route.
const fetched = pending.length;
click(cellButton('opus', 'spec_edits'));
assert.equal(pending.length, fetched + 1, 'opening a count requests its orchestrator detail');
const detailRequest = pending.at(-1);
assert.equal(detailRequest.url.pathname, '/api/orchestrators/detail');
assert.equal(detailRequest.url.searchParams.get('orchestrator'), 'opus');
assert.equal(detailRequest.url.searchParams.get('window'), '30d');
assert.match(part('detail').textContent, /Loading the tasks/);
assert.equal(cellButton('opus', 'spec_edits').getAttribute('aria-pressed'), 'true');
detailRequest.resolve(response(detailFor('opus', {
  spec_edits: [task('ORB-1', [{ at: '2026-10-11T09:00:00Z', detail: 'acceptance_criteria edited by opus', cause: null }]), task('ORB-2', [])],
  review_rejects: [task('ORB-3', [{ at: '2026-10-12T09:00:00Z', detail: 'review attempt rv-1 rejected', cause: 'crew' }])],
})));
await settle();
assert.match(part('detail').querySelector('header.orch-detail-head').children[0].textContent, /opus › Edited after run start/);
assert.deepEqual(detailTasks(), ['ORB-1', 'ORB-2']);
assert.match(part('detail').textContent, /acceptance_criteria edited by opus/);
assert.equal(part('detail').querySelector('a.orch-task-id').href.includes('open=ORB-1'), true, 'a task links to the task pane');

// Another count of the same orchestrator reuses its detail; its occurrences
// carry the attributed cause.
click(cellButton('opus', 'review_rejects'));
assert.equal(pending.length, fetched + 1, 'the cached detail answers without a request');
assert.deepEqual(detailTasks(), ['ORB-3']);
assert.equal(part('detail').querySelector('span.orch-cause').textContent, 'crew');
click(cellButton('opus', 'review_rejects'));
assert.equal(part('detail'), undefined, 'opening the open count closes it');

// A refused detail reports itself in place.
click(cellButton('sonnet', 'spec_edits'));
pending.at(-1).resolve(response({ error: "no task created in the 30d window names orchestrator 'sonnet'" }, 400));
await settle();
assert.match(part('detail').querySelector('p.orch-detail-error').textContent, /Unable to load the tasks: no task created in the 30d window/);
click(part('detail').querySelector('button.orch-close'));
assert.equal(part('detail'), undefined);

// The aggregate view has no per-workspace scorecard.
common.setWorkspace('');
orchestrators.placeholdOrchestratorsAggregate();
assert.equal(view().textContent, common.AGGREGATE_PANEL_PLACEHOLDER);
assert.equal(byId['orchestrators-count'].textContent, '—');
console.log('dashboard orchestrators ok');
