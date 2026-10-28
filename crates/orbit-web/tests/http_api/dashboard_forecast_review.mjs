// Analytics › Forecast must show review-mode uncertainty on the public
// fetch/render path: a kept mode, a dropped mode, unknown current and
// historical evidence, and an older response that omits those fields. An
// explicit Live Refresh must also survive the dashboard's stored-read polls.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

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

const ids = ['forecast-body', 'forecast-count'];
const byId = Object.fromEntries(ids.map(id => [id, Object.assign(new Node('div'), { id })]));
const document = {
  activeElement: null,
  hidden: false,
  addEventListener() {},
  body: new Node('body'),
  createElement: tag => new Node(tag),
  createDocumentFragment: () => new Node('#fragment'),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => byId[id] || null,
};
const window = { location: { search: '?window=24h', hash: '' }, confirm: () => true };
const pending = [];
const response = (payload, status = 200) => ({
  ok: status >= 200 && status < 300,
  status,
  headers: { get: () => null },
  json: async () => payload,
  text: async () => JSON.stringify(payload),
});
const fetch = (path, options = {}) => new Promise((resolve, reject) => pending.push({
  url: new URL(path, 'http://dashboard.test'), options, resolve, reject,
}));
let clock = 0;
let timerId = 0;
const timers = new Map();
const schedule = (fn, delay) => {
  const id = ++timerId;
  timers.set(id, { fn, at: clock + delay });
  return id;
};
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, fetch, document, window, Node,
  setTimeout: schedule, clearTimeout: id => timers.delete(id),
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
const forecastModule = load('forecast.js');
await forecastModule.link(specifier => load(specifier.replace(/^\.\//, '')));
await forecastModule.evaluate();
const pollingModule = load('polling.js');
await pollingModule.link(specifier => load(specifier.replace(/^\.\//, '')));
await pollingModule.evaluate();
const forecast = forecastModule.namespace;
const common = modules.get('common.js').namespace;
const body = () => document.getElementById('forecast-body');
const text = () => body().textContent;
const settle = async () => { for (let i = 0; i < 6; i += 1) await new Promise(resolve => setTimeout(resolve, 0)); };

function task(fields) {
  return {
    id: 'ORB-1',
    title: 'Forecast a change',
    status: 'backlog',
    predicted_start_hours: { p50: 1, p90: 3 },
    predicted_start_rank: { p50: 1, p90: 2 },
    completion_probability: 0.9,
    ...fields,
  };
}

function payload({ review = true, coverage = {}, basis = { cell: 1 }, tasks = [task({ review_mode: 'before_landing', review_mode_fallback: false, basis: 'cell' })] } = {}) {
  const counts = {
    tasks: tasks.length,
    unassessed: 0,
    thin_cell_fallbacks: 0,
    uncovered_model: 0,
    uncosted: 0,
    no_history: 0,
    dependency_unforecastable: 0,
    unverifiable_dependencies: 0,
    review_lock_overlaps: 0,
    blocked: 0,
    outcome_fallbacks: 0,
    outcome_no_history: 0,
    return_unsupported: 0,
    return_tail_unsupported: 0,
    phase_age_unknown: 0,
    ...coverage,
  };
  if (review) {
    counts.review_mode_fallbacks ??= 0;
    counts.review_mode_unknown ??= 0;
    counts.history_review_mode_unknown ??= 0;
  }
  return {
    model_version: review ? 5 : 3,
    backlog: { backlog: tasks.length, in_progress: 0, blocked: 0, proposed: 0, review: 1 },
    inputs: {
      lookback_days: 30,
      concurrency: 5,
      concurrency_source: 'default',
      history_deliveries: 12,
      iterations: 1000,
      min_cell_samples: 5,
      arrivals: false,
      admission_factors: review ? { review_modes: 1 } : { admission: 1 },
    },
    arrivals: {
      window_days: 7,
      per_day: { total: 1, auto_task: 0, sweep: 0, filing: 1 },
      done_per_day: 1,
      coverage: { deliveries: 12, missing_tokens: 0, missing_usd: 0 },
    },
    forecast: {
      tasks: tasks.length,
      time_to_drain_hours: { p50: 6.5, p90: 20 },
      usd: { p50: 12.5, p90: 40 },
      tokens: { p50: 120000, p90: 400000 },
      effective_parallelism: { p50: 1.4, p90: 2.1 },
      critical_path_hours: { p50: 4, p90: 9 },
      horizon_hours: 2160,
      drain_probability: 0.9,
      completion_probability: 0.8,
      not_expected_to_drain: 0.2,
      by_complexity: [{
        complexity: 'medium',
        tasks: tasks.length,
        basis,
        work_hours: { p50: 8, p90: 14 },
        usd: { p50: 10, p90: 22 },
        tokens: { p50: 80000, p90: 160000 },
      }],
      task_predictions: tasks,
    },
    coverage: counts,
    calibration: { forecasts: [] },
    stored: {},
  };
}

function coverageRow(label) {
  const row = [...body().querySelectorAll('.fc-cov')].find(entry => entry.querySelector('dt')?.own?.trim() === label);
  if (!row) return null;
  const value = row.querySelector('dd');
  return { text: value?.textContent ?? '', warn: Boolean(value?.classList.contains('fc-warn')) };
}

function headers() {
  return [...body().querySelectorAll('thead th')].map(cell => cell.textContent);
}

async function render(bodyPayload) {
  const before = pending.length;
  const done = forecast.fetchAndRenderForecast();
  assert.equal(pending.length, before + 1, 'the public fetch path asks for the forecast');
  pending.at(-1).resolve(response(bodyPayload));
  await done;
  await settle();
}

common.setMultiWorkspace(true);
common.setWorkspace('ws-review');
await settle();

const keptOrder = 'Below 5 deliveries it keeps the review mode, first widening to that complexity at any crew, then to every delivery of that mode. Only after that does it drop the review mode: the same complexity and crew pool once that pool also has 5 deliveries, then that complexity, then every delivery.';

await render(payload({
  coverage: { thin_cell_fallbacks: 1 },
  basis: { cell: 1, review_complexity: 1 },
  tasks: [
    task({ id: 'ORB-1', title: 'Matched cell', review_mode: 'before_landing', review_mode_fallback: false, basis: 'cell' }),
    task({ id: 'ORB-2', title: 'Wider crew', review_mode: 'before_landing', review_mode_fallback: false, basis: 'review_complexity' }),
  ],
}));
const supported = text();
assert.ok(supported.includes('same complexity, crew and review mode 1'), `supported sampling names the matched cell: ${supported}`);
assert.ok(supported.includes('review mode kept, crew dropped 1'), `supported sampling names the mode-preserving fallback: ${supported}`);
assert.ok(supported.includes('Before landing · mode kept'), `a mode-preserving task says the mode was kept: ${supported}`);
assert.ok(!supported.includes('· mode dropped'), `a kept mode is not described as dropped: ${supported}`);
assert.ok(!supported.includes('mode_pooled_cell') && !supported.includes('review_complexity'), `wire basis names stay off the supported view: ${supported}`);
assert.ok(supported.includes(keptOrder), `the sampling explanation states the shipped order: ${supported}`);
assert.deepEqual(coverageRow('Review mode dropped'), { text: '0', warn: false });
assert.deepEqual(coverageRow('Unknown review assumption'), { text: '0', warn: false });
assert.deepEqual(coverageRow('History with unknown review'), { text: '0', warn: false });
assert.ok(headers().includes('Review'), `the schedule names the review assumption: ${headers().join(', ')}`);

await render(payload({
  coverage: { thin_cell_fallbacks: 1, review_mode_fallbacks: 1 },
  basis: { mode_pooled_cell: 1, review_workspace: 1 },
  tasks: [
    task({ id: 'ORB-3', title: 'Dropped mode', review_mode: 'before_pr', review_mode_fallback: true, basis: 'mode_pooled_cell' }),
    task({ id: 'ORB-4', title: 'Kept across work', review_mode: 'before_landing', review_mode_fallback: false, basis: 'review_workspace' }),
  ],
}));
const fallback = text();
assert.notEqual(fallback, supported, 'dropping review mode changes the rendered forecast');
assert.ok(fallback.includes('review mode dropped, same complexity and crew 1'), `a mode-pooled cell has a readable label: ${fallback}`);
assert.ok(fallback.includes('review mode kept, complexity dropped 1'), `a mode-preserving workspace pool stays distinct: ${fallback}`);
assert.ok(fallback.includes('Before PR · mode dropped'), `the task that dropped its mode says so: ${fallback}`);
assert.ok(fallback.includes('Before landing · mode kept'), `the task that kept its mode says so: ${fallback}`);
assert.ok(!fallback.includes('mode_pooled_cell') && !fallback.includes('review_workspace'), `wire basis names stay off the fallback view: ${fallback}`);
assert.deepEqual(coverageRow('Review mode dropped'), { text: '1', warn: true });
assert.deepEqual(coverageRow('Unknown review assumption'), { text: '0', warn: false });

await render(payload({
  coverage: {
    thin_cell_fallbacks: 1,
    review_mode_fallbacks: 1,
    review_mode_unknown: 1,
    history_review_mode_unknown: 7,
  },
  basis: { mode_pooled_cell: 1 },
  tasks: [
    task({
      id: 'ORB-5',
      title: 'Unresolved review',
      review_mode: 'unknown',
      review_mode_fallback: true,
      basis: 'mode_pooled_cell',
      unforecastable: 'review',
    }),
  ],
}));
const unknown = text();
assert.notEqual(unknown, fallback, 'unknown review evidence changes the rendered forecast');
assert.ok(unknown.includes('Unknown assumption'), `an unresolved task assumption is visible: ${unknown}`);
assert.ok(unknown.includes('waits on work in review'), `an unforecastable review dependency still renders beside the assumption: ${unknown}`);
assert.deepEqual(coverageRow('Unknown review assumption'), { text: '1', warn: true });
assert.deepEqual(coverageRow('History with unknown review'), { text: '7', warn: true });
assert.deepEqual(coverageRow('Review mode dropped'), { text: '1', warn: true });
assert.ok(!unknown.includes('mode_pooled_cell'), `an unknown assumption does not reveal the wire basis: ${unknown}`);

await render(payload({
  review: false,
  coverage: { thin_cell_fallbacks: 1 },
  basis: { cell: 1, complexity: 1 },
  tasks: [task({ id: 'ORB-6', title: 'Older forecast', basis: 'cell' })],
}));
const older = text();
assert.notEqual(older, supported, 'an older response does not render as a review-mode forecast');
assert.ok(older.includes('cell 1 · complexity 1'), `older pools keep their previous labels: ${older}`);
assert.ok(older.includes('falling back to its complexity, then to all deliveries, when there are too few.'), `older responses keep the previous sampling sentence: ${older}`);
assert.ok(!older.includes('Review mode dropped'), `older responses do not invent a dropped-mode count: ${older}`);
assert.ok(!older.includes('Unknown review assumption') && !older.includes('Unknown assumption'), `older responses do not invent unknown review evidence: ${older}`);
assert.ok(!older.includes('keeps the review mode'), `older responses do not describe the review-mode pool order: ${older}`);
assert.ok(!older.includes('mode_pooled_cell'), `older responses do not show a review-mode wire key: ${older}`);
assert.ok(!headers().includes('Review'), `older schedules omit the review column: ${headers().join(', ')}`);
assert.deepEqual(coverageRow('Thin-cell fallbacks'), { text: '1', warn: true });

console.log('forecast review-mode rendering passed');

async function advanceClock(ms) {
  const until = clock + ms;
  for (;;) {
    const next = [...timers].sort((a, b) => a[1].at - b[1].at)[0];
    if (!next || next[1].at > until) break;
    const [id, timer] = next;
    timers.delete(id);
    clock = timer.at;
    timer.fn();
    await settle();
  }
  clock = until;
}

function clickForecastControl(label) {
  const button = [...body().querySelectorAll('button')].find(node => node.textContent === label);
  assert.ok(button, `Forecast has the ${label} control`);
  assert.ok(!button.disabled, `${label} is enabled`);
  for (const listener of button.listeners.click || []) listener();
  return button;
}

for (const overlap of [false, true]) {
  await test(overlap ? 'a poll cannot supersede a pending Refresh' : 'Live Refresh survives two dashboard poll ticks', async () => {
    common.setWorkspace(`ws-refresh-${overlap}`);
    const stored = { ...payload(), served_from: 'stored', generated_at: '2026-10-28T20:00:00Z' };
    const live = { ...payload(), served_from: 'live', generated_at: '2026-10-28T20:10:00Z' };
    live.forecast.time_to_drain_hours = { p50: 2, p90: 4 };
    let ticks = 0;
    pollingModule.namespace.startDashboardPolling(() => {
      ticks += 1;
      return common.runPanelPoll(true, () => {
        const before = pending.length;
        const done = forecast.fetchAndRenderForecast();
        for (const request of pending.slice(before)) {
          assert.equal(request.url.searchParams.get('refresh'), null, 'polls use the stored read');
          request.resolve(response(stored));
        }
        return done.then(() => true);
      });
    });
    try {
      await settle();
      assert.match(body().querySelector('.fc-source').textContent, /^Stored /);
      if (overlap) await advanceClock(29_000);
      const beforeRefresh = pending.length;
      const button = clickForecastControl('Refresh');
      assert.equal(pending.length, beforeRefresh + 1);
      const refresh = pending.at(-1);
      assert.equal(refresh.url.searchParams.get('refresh'), '1', 'Refresh computes a live forecast');
      assert.equal(button.disabled, true);
      assert.match(text(), /6\.5h/, 'previous figures remain while Refresh computes');
      if (overlap) {
        await advanceClock(1_000);
        assert.equal(ticks, 2, 'the poll starts while Refresh is still pending');
      }
      refresh.resolve(response(live));
      await settle();
      assert.equal(body().querySelector('.fc-source').textContent, 'Live', 'the Refresh result renders');
      assert.equal(button.disabled, false);
      const liveFigures = body().querySelector('.fc-kpi').textContent;
      assert.match(liveFigures, /2\.0h/);
      assert.match(liveFigures, /4\.0h/);
      for (let tick = 1; tick <= 2; tick += 1) {
        await advanceClock(30_000);
        assert.equal(ticks, (overlap ? 2 : 1) + tick, 'the next poll tick executes');
        assert.equal(body().querySelector('.fc-source').textContent, 'Live', 'polls preserve the Live source');
        assert.equal(body().querySelector('.fc-kpi').textContent, liveFigures, 'polls preserve Live figures');
      }
      if (!overlap) {
        const failedRefresh = forecast.fetchAndRenderForecast({ refresh: true });
        pending.at(-1).resolve(response({ error: 'forecast store busy' }, 500));
        await assert.rejects(failedRefresh, /forecast store busy/);
        assert.match(text(), /Refresh failed; showing stale data/);
        assert.equal(body().querySelector('.fc-kpi').textContent, liveFigures);
        await render(stored);
        assert.match(body().querySelector('.fc-source').textContent, /^Stored /, 'a failed Refresh releases polling');
      }
    } finally {
      timers.clear();
    }
  });
}
