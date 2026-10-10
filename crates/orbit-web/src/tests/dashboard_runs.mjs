// Behavior tests for Runs tab: Load more pagination, live elapsed duration,
// Actions header, and Cancel button styling.
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
    this.title = '';
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
  dispatch(type, extra = {}) {
    const event = { type, target: this, stopPropagation() {}, preventDefault() {}, ...extra };
    for (const fn of this.listeners[type] || []) fn(event);
  }
  click() { if (!this.disabled) this.dispatch('click'); }
  focus() { document.activeElement = this; }
  contains(node) { for (let current = node; current; current = current.parentNode) if (current === this) return true; return false; }
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
  for (const [, name] of simple.matchAll(/\[data-([\w-]+)\]/g)) if (node.dataset[name] === undefined) return false;
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

const runsBody = Object.assign(new Node('div'), { id: 'runs-body' });
const diagCount = Object.assign(new Node('span'), { id: 'diag-count' });
const byId = { 'runs-body': runsBody, 'diag-count': diagCount };

const document = {
  activeElement: null,
  body: new Node('body'),
  createElement: tag => new Node(tag),
  createDocumentFragment: () => new Node('#fragment'),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => byId[id] || null,
};

const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, fetch: () => Promise.resolve(), document,
  window: { location: { search: '', hash: '', href: 'http://localhost/#diagnostics/runs' } },
  history: { replaceState() {} },
  Date,
  Math,
});

const modules = new Map();
const load = name => {
  if (!modules.has(name)) {
    modules.set(name, new vm.SourceTextModule(fs.readFileSync(new URL(`../../assets/dashboard/js/${name}`, import.meta.url), 'utf8'), { context, identifier: name }));
  }
  return modules.get(name);
};

const runsModule = load('runs.js');
await runsModule.link(specifier => load(specifier.replace(/^\.\//, '')));
await runsModule.evaluate();
const { initRuns, renderRuns } = runsModule.namespace;

// Mock runsContext
let loadMoreCalled = 0;
let runsLoading = false;
let currentMeta = { limit: 25, total: 100, truncated: true };
let currentRunsList = [];

const { fmtDuration } = load('common.js').namespace;

initRuns({
  navigateToRun: () => {},
  fetchAndRenderRuns: () => Promise.resolve(),
  fetchAndRenderRunDetail: () => Promise.resolve(),
  fetchAndRenderRunEvents: () => Promise.resolve(),
  getActiveRunId: () => null,
  getLastRuns: () => currentRunsList,
  getRunsMeta: () => currentMeta,
  getRunsLoading: () => runsLoading,
  markRunsLoading: () => { runsLoading = true; },
  getRunSourcesUnavailable: () => [],
  fmtTimestamp: () => '10:00:00',
  fmtDuration,
  loadMoreRuns: () => {
    loadMoreCalled += 1;
    return Promise.resolve();
  },
});

// Test 1: Scope note has shortened one-line text and details in title
renderRuns([]);
const scopeNote = runsBody.querySelector('.runs-scope-note');
assert.ok(scopeNote, 'scope note exists');
assert.equal(scopeNote.textContent, 'Every job run, newest first, with no time window.', 'scope note text is shortened to one line');
assert.ok(scopeNote.title && scopeNote.title.includes('selected window only'), 'extended details are in scope note title attribute');

// Test 2: Load more control when truncated
const sampleRuns = [
  {
    run_id: 'jrun-running-1',
    job_id: 'test-job',
    state: 'running',
    started_at: new Date(Date.now() - 125000).toISOString(),
  },
  {
    run_id: 'jrun-success-2',
    job_id: 'test-job',
    state: 'success',
    duration_ms: 45000,
    started_at: new Date(Date.now() - 200000).toISOString(),
    finished_at: new Date(Date.now() - 155000).toISOString(),
  },
];
currentRunsList = sampleRuns;
currentMeta = { limit: 25, total: 100, truncated: true };
renderRuns(sampleRuns);

const limitNote = runsBody.querySelector('.runs-limit-note');
assert.ok(limitNote, 'limit note renders when truncated');
assert.match(diagCount.textContent, /\blimit 25\b/);
assert.doesNotMatch(diagCount.textContent, /server limit/, 'a requested page size below the cap is not a server limit');
for (const total of [100, undefined]) {
  currentMeta = { limit: 75, total, truncated: true };
  renderRuns(sampleRuns);
  assert.match(diagCount.textContent, /\blimit 75\b/, 'the count retains the requested size after two pagination increments');
  assert.doesNotMatch(diagCount.textContent, /server limit/, 'the requested size is identified even without a server total');
}
currentMeta = { limit: 25, total: 100, truncated: true };
renderRuns(sampleRuns);
const loadMoreBtn = runsBody.querySelector('.runs-limit-note').querySelector('.runs-load-more');
assert.ok(loadMoreBtn, 'load more button exists in limit note');
assert.equal(loadMoreBtn.textContent, 'Load more', 'load more button text is "Load more"');
assert.equal(loadMoreBtn.disabled, false, 'load more button is not disabled initially');

// Click load more button
loadMoreBtn.click();
assert.equal(loadMoreCalled, 1, 'clicking load more triggers loadMoreRuns callback');

// When loading, button shows loading state
runsLoading = true;
renderRuns(sampleRuns);
const loadingLimitNote = runsBody.querySelector('.runs-limit-note');
const loadingBtn = loadingLimitNote.querySelector('.runs-load-more');
assert.ok(loadingBtn.disabled, 'load more button is disabled while loading');
assert.equal(loadingBtn.textContent, 'Loading…', 'load more button displays Loading… state');
runsLoading = false;

// When not truncated, limit note is omitted
currentMeta = { limit: 25, total: 2, truncated: false };
renderRuns(sampleRuns);
assert.equal(runsBody.querySelector('.runs-limit-note'), null, 'limit note omitted when truncated is false');

// Test 3: Running rows display live elapsed duration with refresh symbol
renderRuns(sampleRuns);
const rows = runsBody.querySelectorAll('.runs-row').filter(r => !classesOf(r).includes('runs-header'));
assert.equal(rows.length, 2, 'two run rows rendered');

const runningRow = rows.find(r => r.dataset.key.endsWith('jrun-running-1'));
assert.ok(runningRow, 'running row found');
const runningDurationCell = runningRow.querySelector('.duration');
assert.ok(runningDurationCell, 'duration cell found');
assert.ok(runningDurationCell.textContent.includes('↻'), 'running row duration includes ↻ refresh indicator');
assert.ok(runningDurationCell.textContent.includes('2m'), 'running row duration derives elapsed time from started_at');
assert.notEqual(runningDurationCell.textContent.trim(), '-', 'running row duration is not "-"');

const successRow = rows.find(r => r.dataset.key.endsWith('jrun-success-2'));
assert.ok(successRow, 'success row found');
const successDurationCell = successRow.querySelector('.duration');
assert.ok(!successDurationCell.textContent.includes('↻'), 'completed row duration does not include ↻');
assert.equal(successDurationCell.textContent.trim(), '45.0s', 'completed row duration displays formatted duration_ms');

// Test 4: Actions column header and Cancel button styling
const header = runsBody.querySelector('.runs-header');
assert.ok(header, 'runs table header exists');
const actionsHeader = header.querySelector('.run-actions-header');
assert.ok(actionsHeader, 'actions column header exists');
assert.equal(actionsHeader.textContent, 'Actions', 'actions column header is titled "Actions"');

const cancelBtn = runningRow.querySelector('.run-cancel');
assert.ok(cancelBtn, 'cancel button exists for running run');
assert.equal(cancelBtn.textContent, 'Cancel', 'cancel button has text "Cancel"');
assert.ok(cancelBtn.classList.contains('run-cancel'), 'cancel button has run-cancel class');

// Test 5: at the server's request cap Load more is withheld, since it would refetch the same rows
let limitCapped = true;
initRuns({
  navigateToRun: () => {},
  fetchAndRenderRuns: () => Promise.resolve(),
  getLastRuns: () => currentRunsList,
  getRunsMeta: () => currentMeta,
  getRunsLoading: () => false,
  getRunsLimitCapped: () => limitCapped,
  markRunsLoading: () => {},
  getRunSourcesUnavailable: () => [],
  fmtTimestamp: () => '10:00:00',
  fmtDuration,
  loadMoreRuns: () => Promise.resolve(),
});
currentMeta = { limit: 200, total: 5000, truncated: true };
renderRuns(sampleRuns);
const cappedNote = runsBody.querySelector('.runs-limit-note');
assert.ok(cappedNote, 'limit note still explains the truncation at the server cap');
assert.match(diagCount.textContent, /server limit 200\b/, 'a confirmed server cap is named in the count');
currentMeta = { limit: 200, truncated: true };
renderRuns(sampleRuns);
assert.match(diagCount.textContent, /server limit 200\b/, 'a confirmed cap is also named when the total is unavailable');
currentMeta = { limit: 200, total: 5000, truncated: true };
renderRuns(sampleRuns);
assert.equal(cappedNote.querySelector('.runs-load-more'), null, 'no Load more button once the server cap is reached');
limitCapped = false;
renderRuns(sampleRuns);
assert.ok(runsBody.querySelector('.runs-load-more'), 'Load more returns while the server can still return more');

console.log('All dashboard runs behavior assertions passed.');
