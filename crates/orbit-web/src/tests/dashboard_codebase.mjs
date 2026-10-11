// Audit › Codebase replaces Audit › Policy: the subtab swap and its hash, the
// window / minimum support / rollup controls, each section's render, the empty
// state, and the denial badge that opens Events filtered to denials.
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
const subtabs = make('nav', 'audit-subtabs', body);
for (const name of ['events', 'codebase']) {
  const button = make('button', null, subtabs);
  button.className = 'subtab';
  button.dataset.subtab = name;
}
for (const id of ['audit-title', 'audit-count', 'audit-events-controls', 'audit-body', 'audit-codebase-controls', 'audit-codebase-body', 'rail-count-audit-denials', 'audit-search']) {
  make('div', id, body);
}
for (const id of ['codebase-window', 'codebase-min-support', 'codebase-rollup']) make('select', id, byId['audit-codebase-controls']);
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
const pending = [];
const response = (payload, status = 200) => ({ ok: status >= 200 && status < 300, status, headers: { get: () => null }, json: async () => payload, text: async () => JSON.stringify(payload) });
const fetch = (path, options = {}) => new Promise(resolve => pending.push({ url: new URL(path, 'http://dashboard.test'), options, resolve }));
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, fetch, document, window, Node, Intl,
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
const section = key => byId['audit-codebase-body'].children.find(node => node.dataset.section === key);
const sectionText = key => section(key)?.textContent || '';

common.setMultiWorkspace(true);
common.setWorkspace('ws-a');
await settle();

// The subtabs are Events and Codebase; a retired Policy link lands on Events.
audit.setActiveAuditSubtabFromButton('codebase');
assert.ok(shown('audit-codebase-body') && shown('audit-codebase-controls'), 'Codebase shows its body and controls');
assert.ok(!shown('audit-body') && !shown('audit-events-controls'), 'Codebase hides the Events listing');
assert.equal(byId['audit-title'].textContent, 'Codebase analytics');
assert.deepEqual(subtabs.children.filter(button => button.classList.contains('active')).map(button => button.dataset.subtab), ['codebase']);
assert.equal(audit.buildAuditHash(), '#audit/codebase', 'the default selection writes a bare #audit/codebase');
audit.setAuditSubtab('policy');
assert.equal(audit.getActiveAuditSubtab(), 'events', 'the retired policy subtab falls back to Events');
assert.ok(shown('audit-body') && !shown('audit-codebase-body'));

// An Events filter survives a visit to Codebase, and #audit/codebase?… restores
// the window, minimum support and rollup into the controls and the request.
audit.applyAuditHashQuery(new URLSearchParams('status=denied'), 'events');
audit.applyAuditHashQuery(new URLSearchParams('window=30d&min_support=5&rollup=dir'), 'codebase');
audit.setActiveAuditSubtabFromButton('codebase');
assert.equal(audit.buildAuditHash(), '#audit/codebase?window=30d&min_support=5&rollup=dir');
assert.equal(byId['codebase-window'].value, '30d');
assert.equal(byId['codebase-min-support'].value, '5');
assert.equal(byId['codebase-rollup'].value, 'dir');
audit.setActiveAuditSubtabFromButton('events');
assert.match(audit.buildAuditHash(), /status=denied/, 'the Events filter is kept across the Codebase visit');
audit.setActiveAuditSubtabFromButton('codebase');

const full = {
  window: '30d', rollup: 'dir', min_support: 5, touched: 'declared_footprint', tasks_scanned: 42,
  computed_at: '2026-10-10T12:00:00.000Z', cache_ttl_seconds: 300,
  regressions: { below_support: 1, population: 9, rows: [{
    path: 'src/flaky/', rate: 0.5, numerator: 1, denominator: 2,
    regressed_tasks: [{ task_id: 'ORB-1', title: 'Flaky change', crew: 'opus', regression_tasks: ['ORB-2', 'ORB-3'] }],
    by_crew: [{ crew: 'opus', rate: 1, numerator: 1, denominator: 1 }, { crew: 'sonnet', rate: 0, numerator: 0, denominator: 1 }],
  }] },
  lock_contention: { below_support: 0, wait_time_recorded: false, rows: [
    { path: 'src/lock.rs', rate: 0.5, numerator: 1, denominator: 2, contending_tasks: 1, reservations: 1, denials: 20, wait_ms: null },
  ] },
  footprint_misses: { below_support: 0, population: 2, rows: [
    { path: 'src/widened.rs', rate: 0.5, numerator: 1, denominator: 2, widenings: 2 },
  ] },
  change_coupling: { below_support: 0, reservations: 4, excluded_large: 1, large_footprint_cap: 12, rows: [
    { paths: ['src/a.rs', 'src/b.rs'], rate: 0.75, numerator: 3, denominator: 4, reservations: [4, 3] },
  ] },
  blocked: { below_support: 0, causes: [
    { cause: 'auth', kind: 'infrastructure', tasks: 1 },
    { cause: 'unclassified', kind: 'unclassified', tasks: 1 },
    { cause: 'run_failure', kind: 'attributed', tasks: 1 },
  ], rows: [{ path: 'src/blocked.rs', rate: 1 / 3, numerator: 1, denominator: 3, unclassified: 1 }] },
  rejected: { below_support: 0, rejected_with_run: 1, excluded_without_run: 1, rows: [
    { path: 'src/rejected.rs', rate: 0.5, numerator: 1, denominator: 2 },
  ] },
};
const request = codebase.fetchAndRenderCodebase();
const sent = pending.at(-1).url;
assert.equal(sent.pathname, '/api/codebase/analytics');
assert.deepEqual(
  ['window', 'min_support', 'rollup', 'workspace'].map(name => sent.searchParams.get(name)),
  ['30d', '5', 'dir', 'ws-a'],
  'the request carries the selection and the workspace',
);
pending.at(-1).resolve(response(full));
await request;

const caption = byId['audit-codebase-body'].children.find(node => node.dataset.key === 'caption');
assert.match(caption.textContent, /Touched = declared footprint/, 'the view labels touched as the declared footprint');
assert.match(caption.textContent, /42 tasks scanned/);
for (const key of ['regressions', 'lock_contention', 'footprint_misses', 'change_coupling', 'blocked', 'rejected']) {
  assert.ok(section(key), `${key} renders`);
  assert.equal(section(key).querySelector('.codebase-empty'), null, `${key} has rows, not the empty state`);
}
// Every ranking shows its rate, numerator and denominator.
assert.match(sectionText('regressions'), /src\/flaky\/50%1 \/ 2/);
const regressed = section('regressions').querySelectorAll('a.codebase-task-link');
assert.deepEqual(regressed.map(link => link.textContent), ['ORB-1', 'ORB-2', 'ORB-3'], 'drill-down links the regressed task and its regressions');
assert.match(regressed[0].getAttribute('href'), /#tasks\?open=ORB-1$/);
assert.match(sectionText('regressions'), /By crew: opus 100% \(1\/1\) · sonnet 0% \(0\/1\)/);
assert.match(sectionText('regressions'), /1 below minimum support 5 omitted/);
const lockCells = section('lock_contention').querySelector('tbody tr').children.map(cell => cell.textContent);
assert.deepEqual(lockCells, ['src/lock.rs', '50%', '1 / 2', '1', '20'], 'contention, reservations and denials are separate columns; no wait column');
assert.match(sectionText('lock_contention'), /wait time is not recorded/);
assert.match(sectionText('footprint_misses'), /src\/widened\.rs50%1 \/ 22/);
assert.match(sectionText('change_coupling'), /src\/a\.rs ↔ src\/b\.rs75%3 \/ 44 · 3/);
assert.match(sectionText('change_coupling'), /1 reservations over 12 files excluded/);
const causes = section('blocked').querySelectorAll('li.codebase-cause');
assert.deepEqual(causes.map(item => [item.dataset.cause, item.classList.contains('codebase-cause-infrastructure')]),
  [['auth', true], ['unclassified', false], ['run_failure', false]], 'infrastructure causes are split out by cause');
assert.match(sectionText('blocked'), /src\/blocked\.rs33%1 \/ 31/);
assert.match(sectionText('rejected'), /src\/rejected\.rs50%1 \/ 2/);
assert.match(sectionText('rejected'), /1 rejected without a run \(at proposal\) excluded/);

// The controls rewrite the selection and ask for a new hash; an invalid
// minimum support keeps the last valid one.
let changes = 0;
codebase.wireCodebaseControls(() => { changes += 1; });
const fire = (id, value) => { byId[id].value = value; for (const listener of byId[id].listeners.change || []) listener(); };
fire('codebase-min-support', '3');
fire('codebase-rollup', 'crate');
fire('codebase-window', '7d');
assert.equal(JSON.stringify(codebase.getCodebaseQuery()), JSON.stringify({ window: '7d', min_support: 3, rollup: 'crate' }));
fire('codebase-min-support', '0');
assert.equal(codebase.getCodebaseQuery().min_support, 3, 'an invalid minimum support keeps the last valid one');
assert.equal(byId['codebase-min-support'].value, '3', 'and the input shows it again');
assert.equal(changes, 3, 'an invalid edit asks for no new hash');

// An empty workspace answers empty sections, and each says so.
const empty = {
  window: '7d', rollup: 'crate', min_support: 3, touched: 'declared_footprint', tasks_scanned: 0,
  computed_at: '2026-10-10T12:00:00.000Z', cache_ttl_seconds: 300,
  regressions: { rows: [], below_support: 0, population: 0 },
  lock_contention: { rows: [], below_support: 0, wait_time_recorded: false },
  footprint_misses: { rows: [], below_support: 0, population: 0 },
  change_coupling: { rows: [], below_support: 2, reservations: 0, excluded_large: 0, large_footprint_cap: 12 },
  blocked: { rows: [], below_support: 0, causes: [{ cause: 'unclassified', kind: 'unclassified', tasks: 0 }, { cause: 'run_failure', kind: 'attributed', tasks: 0 }] },
  rejected: { rows: [], below_support: 0, rejected_with_run: 0, excluded_without_run: 0 },
};
const emptyRequest = codebase.fetchAndRenderCodebase();
pending.at(-1).resolve(response(empty));
await emptyRequest;
for (const key of ['regressions', 'lock_contention', 'footprint_misses', 'change_coupling', 'blocked', 'rejected']) {
  assert.ok(section(key).querySelector('.codebase-empty'), `${key} shows its empty state`);
  assert.equal(section(key).querySelector('table'), null, `${key} draws no table when empty`);
}
assert.match(sectionText('regressions'), /No path has data in this window/);
assert.match(sectionText('change_coupling'), /No pair reaches minimum support 3; 2 below it/);

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
