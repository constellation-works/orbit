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

const location = { search: '', hash: '#diagnostics/runs', href: 'http://localhost/#diagnostics/runs' };
const history = {
  replaceState(_data, _title, url) {
    if (url == null) return;
    const resolved = new URL(String(url), location.href);
    location.href = resolved.href;
    location.search = resolved.search;
    location.hash = resolved.hash;
  },
};
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, fetch: () => Promise.resolve(), document,
  window: { location },
  history,
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
assert.match(scopeNote.textContent, /Every job run, newest first, in the 24h window/);
assert.doesNotMatch(scopeNote.textContent, /no time window/);
assert.match(scopeNote.title, /selected window/);

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

// Failed rows say where they stopped; the job cell's tooltip names the job.
const longMessage = 'deterministic action `file_ci_failure_tasks` failed: ci_failure_sweep retryable stage collection_or_investigation after a very long tail of detail';
const pipelineRun = (id, job, state, extra = {}) => ({
  run_id: id, job_id: job, state, task_ids: ['ORB-1'], tasks: [{ id: 'ORB-1', title: 'T' }],
  finished_at: new Date(Date.now() - 1000).toISOString(), ...extra,
});
const failedRun = pipelineRun('jrun-failed-1', 'ci_failure_sweep_pipeline', 'failed', {
  task_ids: null, tasks: null,
  steps: [
    { target_id: 'collect', state: 'success' },
    { target_id: 'file_ci_failure_tasks', state: 'failed', error_code: 'action_failed', error_message: longMessage },
    { target_id: 'cleanup', state: 'skipped', error_message: 'when: false' },
  ],
});
currentMeta = { limit: 25, total: 4, truncated: false };
currentRunsList = [failedRun, { ...failedRun, run_id: 'jrun-timeout-1', state: 'timeout', steps: [{ target_id: 'wait', state: 'timeout' }] },
  { ...failedRun, run_id: 'jrun-bare-1', state: 'interrupted', steps: [] }, sampleRuns[1]];
renderRuns(currentRunsList);
const failedRow = key => runsBody.querySelectorAll('.runs-row').find(r => r.dataset.key.endsWith(key));
const failedAt = failedRow(':jrun-failed-1').querySelector('.run-failed-at');
assert.equal(failedAt.querySelector('.run-failed-step').textContent, 'file_ci_failure_tasks', 'failed row names the step that errored, not the skipped one after it');
const excerpt = failedAt.querySelector('.run-failed-excerpt').textContent;
assert.ok(excerpt.startsWith('action_failed: deterministic action') && excerpt.endsWith('…') && excerpt.length <= 80, `excerpt is clipped to 80 chars: ${excerpt}`);
assert.match(failedAt.title, /Message: deterministic action .*very long tail of detail/, 'tooltip carries the full error text');
assert.equal(failedRow(':jrun-timeout-1').querySelector('.run-failed-step').textContent, 'wait', 'timeout rows show their step');
assert.equal(failedRow(':jrun-bare-1').querySelector('.run-failed-at').textContent, 'no error recorded', 'interrupted run with nothing recorded says so');
assert.equal(failedRow(':jrun-success-2').querySelector('.run-failed-at').children.length, 0, 'successful rows carry no failure detail');
assert.ok(runsBody.querySelector('.runs-header .run-failed-at-header'), 'header names the Failed at column');
assert.equal(failedRow(':jrun-failed-1').querySelector('.id').title, 'ci_failure_sweep_pipeline', 'job button tooltip is the full job name');

// A task's auto/gate/pr pipeline runs fold into one expandable group.
const triplet = [
  pipelineRun('jrun-pr', 'task_pr_pipeline', 'failed', { steps: [{ target_id: 'open_pr', state: 'failed', error_code: 'gh_failed', error_message: 'no remote' }] }),
  pipelineRun('jrun-gate', 'task_gate_pipeline', 'success'),
  pipelineRun('jrun-auto', 'task_auto_pipeline', 'success'),
];
const lone = pipelineRun('jrun-lone', 'task_auto_pipeline', 'success', { task_ids: ['ORB-2'], tasks: [{ id: 'ORB-2', title: 'U' }] });
currentRunsList = [...triplet, lone];
renderRuns(currentRunsList);
const runRowKeys = () => runsBody.querySelectorAll('.runs-row').filter(r => !classesOf(r).includes('runs-header')).map(r => r.dataset.key);
assert.deepEqual(runRowKeys().length, 2, 'three runs of one task collapse into one group row beside the ungrouped run');
const groupRow = runsBody.querySelector('.runs-group');
assert.ok(groupRow, 'group row renders');
const toggle = groupRow.querySelector('.run-group-toggle');
assert.match(toggle.textContent, /3 pipeline runs/);
assert.equal(toggle.getAttribute('aria-expanded'), 'false', 'groups start folded');
assert.equal(groupRow.querySelector('[data-state]').dataset.state, 'failed', 'a group shows its worst state');
assert.equal(groupRow.querySelector('.run-failed-step').textContent, 'pr › open_pr', 'a folded group still shows where it failed');
assert.ok(groupRow.querySelector('.run-task-id').textContent === 'ORB-1');
groupRow.click();
const opened = runsBody.querySelector('.runs-group');
assert.equal(opened.querySelector('.run-group-toggle').getAttribute('aria-expanded'), 'true', 'clicking the group opens it');
assert.equal(runRowKeys().length, 5, 'opened group lists its three runs under the header');
assert.equal(runsBody.querySelectorAll('.runs-group-child').length, 3, 'member runs are marked as group children');
runsBody.querySelector('.runs-group').click();
assert.equal(runRowKeys().length, 2, 'clicking again folds the group');

// Filters update the address and the scope note before the next fetch returns.
const filterButton = (label) => [...runsBody.querySelectorAll('.runs-filter-button')].find((button) => button.textContent === label);
const windowButton = (value) => [...runsBody.querySelectorAll('.runs-filter-button')].find((button) => button.dataset.window === value);
const searchParams = () => new URL(location.href).searchParams;
currentMeta = { limit: 25, total: 2, truncated: false, state: 'all', runsQuery: 'all\n\n\n24h' };
runsLoading = false;
renderRuns(sampleRuns);
filterButton('Failed').click();
assert.ok(classesOf(filterButton('Failed')).includes('active'), 'Failed is selected as soon as it is clicked');
assert.ok(!classesOf(filterButton('All')).includes('active'), 'All is no longer selected while the failed list loads');
assert.equal(searchParams().get('run_state'), 'failed');
assert.match(runsBody.querySelector('.runs-scope-note').textContent, /Failed, timed-out, and interrupted/);
assert.match(runsBody.querySelector('.runs-scope-note').textContent, /24h/);
assert.doesNotMatch(runsBody.querySelector('.runs-scope-note').textContent, /no time window/);
windowButton('7d').click();
assert.equal(searchParams().get('window'), '7d');
assert.equal(searchParams().get('run_state'), 'failed');
assert.ok(classesOf(windowButton('7d')).includes('active'));
assert.match(runsBody.querySelector('.runs-scope-note').textContent, /7d/);
windowButton('all').click();
assert.equal(searchParams().get('window'), 'all');
assert.match(runsBody.querySelector('.runs-scope-note').textContent, /no time window/);
const query = runsBody.querySelector('.runs-query');
query.value = 'ORB-15159';
query.dispatch('input');
assert.equal(searchParams().get('task_id'), 'ORB-15159');
assert.equal(searchParams().get('job_id'), null);
assert.match(runsBody.querySelector('.runs-scope-note').textContent, /ORB-15159/);
query.value = 'ci_failure_sweep_pipeline';
query.dispatch('input');
assert.equal(searchParams().get('job_id'), 'ci_failure_sweep_pipeline');
assert.equal(searchParams().get('task_id'), null);
assert.match(runsBody.querySelector('.runs-scope-note').textContent, /ci_failure_sweep_pipeline/);
query.value = 'orb-15159';
query.dispatch('input');
assert.equal(searchParams().get('job_id'), 'orb-15159');
assert.equal(searchParams().get('task_id'), null, 'a lowercase id is a job id');

console.log('All dashboard runs behavior assertions passed.');
