// Cumulative primary-metric chart through the shipped experiments renderer.
// The Rust harness runs this with TZ=America/Los_Angeles, so a Local label
// that still shows the UTC clock cannot pass.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

assert.equal(Intl.DateTimeFormat().resolvedOptions().timeZone, 'America/Los_Angeles', 'the harness pins the zone');

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
    this.id = '';
    this.hidden = false;
    this.own = '';
    this.tabIndex = -1;
    this.style = { setProperty(name, value) { this[name] = value; } };
  }

  get classList() {
    const node = this;
    return {
      add: name => { if (!classesOf(node).includes(name)) node.className = [...classesOf(node), name].join(' '); },
      remove: name => { node.className = classesOf(node).filter(entry => entry !== name).join(' '); },
      contains: name => classesOf(node).includes(name),
      toggle: (name, on) => ((on ?? !classesOf(node).includes(name)) ? node.classList.add(name) : node.classList.remove(name)),
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
    if (child == null || child === false) return;
    if (typeof child === 'string') { this.own += child; return; }
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.children.push(child);
  }

  appendChild(child) { this.append(child); return child; }

  replaceChildren(...nodes) {
    this.children.forEach(child => { child.parentNode = null; });
    this.children = [];
    this.own = '';
    for (const node of nodes) this.append(node);
  }

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

const experimentsBody = Object.assign(new Node('div'), { id: 'experiments-body' });
const experimentsCount = Object.assign(new Node('span'), { id: 'experiments-count' });
const byId = { 'experiments-body': experimentsBody, 'experiments-count': experimentsCount };
const document = {
  activeElement: null,
  body: new Node('body'),
  createElement: tag => new Node(tag),
  createElementNS: (_ns, tag) => new Node(tag),
  createDocumentFragment: () => new Node('#fragment'),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => byId[id] || null,
  querySelectorAll(selector) { return this.body.querySelectorAll(selector); },
  addEventListener() {},
};
document.body.append(experimentsBody, experimentsCount);

const store = new Map();
const localStorage = {
  getItem(key) { return store.has(key) ? store.get(key) : null; },
  setItem(key, value) { store.set(String(key), String(value)); },
};

// The parent review's three buckets: 09:00, 09:05 and 09:10 UTC on
// 2026-10-26. The window closes at 09:12:30, inside the last 5-minute bucket,
// so that bucket's cumulative values stop at the response cutoff rather than
// at 09:15.
const starts = ['2026-10-26T09:00:00Z', '2026-10-26T09:05:00Z', '2026-10-26T09:10:00Z'];
const until = '2026-10-26T09:12:30Z';
const experiment = {
  id: 'exp-intraday',
  name: 'Intraday buckets',
  status: 'running',
  mode: 'shadow',
  segment: 0,
  hypothesis: 'A chart can tell 09:00 from 09:05.',
  primary_metric: 'pass_rate',
  arms: [
    { name: 'control', control: true, weight: 1 },
    { name: 'candidate', control: false, weight: 1 },
  ],
  events: [],
};
const points = (ns, values) => starts.map((start, index) => ({ start, n: ns[index], value: values[index] }));
const readout = {
  as_of: until,
  experiment: { primary_metric: 'pass_rate', min_sample: 20, started_at: starts[0], max_days: 14 },
  verdict: { state: 'collecting', label: 'collecting' },
  warnings: [],
  enrollment: { analysed: 2, forced_excluded: 0 },
  lagging: { open_tasks: 0, open_regression_windows: 0, regression_window_days: 14 },
  arms: [
    { name: 'control', control: true, weight: 1, n: 1 },
    { name: 'candidate', control: false, weight: 1, n: 2 },
  ],
  guardrails: [],
  metrics: [{
    metric: 'pass_rate',
    unit: 'rate',
    kind: 'rate',
    better: 'higher',
    measured: false,
    arms: [
      { arm: 'control', mean: 0, n: 1 },
      { arm: 'candidate', mean: 0.5, n: 2 },
    ],
    differences: [],
  }],
  cumulative: {
    metric: 'pass_rate',
    window: {
      key: 'all',
      since: starts[0],
      until,
      bucket_seconds: 300,
    },
    arms: [
      { arm: 'control', control: true, points: points([1, 1, 1], [0, 0, 0]) },
      { arm: 'candidate', control: false, points: points([1, 2, 2], [0, 0.5, 0.5]) },
    ],
  },
};

function jsonResponse(body) {
  return {
    ok: true,
    status: 200,
    headers: { get: () => null },
    json: async () => body,
    text: async () => JSON.stringify(body),
  };
}

function fetchImpl(url) {
  const path = String(url);
  if (path.includes('/api/diagnostics/experiments/exp-intraday/readout')) return Promise.resolve(jsonResponse(readout));
  if (path.includes('/api/diagnostics/experiments/exp-intraday')) return Promise.resolve(jsonResponse(experiment));
  return Promise.resolve(jsonResponse({}));
}

const context = vm.createContext({
  URLSearchParams,
  URL,
  AbortController,
  console,
  setTimeout,
  clearTimeout,
  fetch: fetchImpl,
  document,
  window: {
    location: { search: '?workspace=ws_fixture', hash: '#analytics/experiments/exp-intraday', href: 'http://127.0.0.1/?workspace=ws_fixture#analytics/experiments/exp-intraday' },
    localStorage,
    addEventListener() {},
    dispatchEvent() {},
  },
  Date,
  Intl,
  Math,
});

const load = name => new vm.SourceTextModule(
  fs.readFileSync(new URL(`../../assets/dashboard/js/${name}`, import.meta.url), 'utf8'),
  { context, identifier: name },
);
const common = load('common.js');
const sparkline = load('sparkline.js');
const experiments = load('experiments.js');
const router = new vm.SyntheticModule(['setActiveTab'], function exportRouter() {
  this.setExport('setActiveTab', () => {});
}, { context, identifier: 'router.js' });

await common.link(() => { throw new Error('unexpected common dependency'); });
await sparkline.link(() => { throw new Error('unexpected sparkline dependency'); });
await router.link(() => { throw new Error('unexpected router dependency'); });
await experiments.link(specifier => {
  if (specifier === './common.js') return common;
  if (specifier === './sparkline.js') return sparkline;
  if (specifier === './router.js') return router;
  throw new Error(`unexpected experiments dependency ${specifier}`);
});
await common.evaluate();
await sparkline.evaluate();
await router.evaluate();
await experiments.evaluate();

const { setExperimentId, fetchAndRenderExperiments } = experiments.namespace;
const { setClockZone, getClockZone } = common.namespace;

function chartLabels() {
  const ticks = experimentsBody.querySelectorAll('.ex-cum-xtick').map(node => node.textContent);
  const columns = experimentsBody.querySelectorAll('.ex-cum-col');
  const hovers = columns.map((column, index) => {
    column.dispatch('mouseenter');
    const title = experimentsBody.querySelector('.mx-tip-title')?.textContent;
    const candidate = experimentsBody.querySelectorAll('.mx-tip-row').find(row => row.textContent.includes('candidate'));
    assert.ok(title, `hover ${index} has a heading`);
    assert.ok(candidate, `hover ${index} names the candidate arm`);
    return {
      title,
      n: candidate.querySelector('.mx-tip-n').textContent,
      value: candidate.querySelector('.mx-tip-value').textContent,
    };
  });
  return { ticks, hovers };
}

setExperimentId('exp-intraday');
await fetchAndRenderExperiments();
assert.equal(getClockZone(), 'utc', 'the chart opens on the UTC dashboard clock');

const utc = chartLabels();
assert.deepEqual(utc.ticks, ['09:00', '09:05', '09:10'], `UTC ticks name each 5-minute bucket: ${utc.ticks.join(' | ')}`);
assert.deepEqual(utc.hovers.map(hover => hover.title), [
  '2026-10-26 09:00 UTC–09:05',
  '2026-10-26 09:05 UTC–09:10',
  '2026-10-26 09:10 UTC–09:12:30',
], `UTC hover names each bucket interval, and the last ends at the cutoff: ${utc.hovers.map(hover => hover.title).join(' | ')}`);
assert.deepEqual(utc.hovers.map(hover => `${hover.n} ${hover.value}`), ['n 1 0%', 'n 2 50%', 'n 2 50%']);
for (const hover of utc.hovers) {
  assert.equal(hover.title.startsWith('Through '), false, `hover does not call the bucket start its cutoff: ${hover.title}`);
}
assert.equal(utc.hovers[2].title.includes('09:15'), false, 'the open bucket does not run out to the next 5-minute boundary');

assert.equal(setClockZone('local'), true);
await fetchAndRenderExperiments();
const local = chartLabels();
assert.deepEqual(local.ticks, ['02:00', '02:05', '02:10'], `Local ticks follow the browser zone: ${local.ticks.join(' | ')}`);
assert.deepEqual(local.hovers.map(hover => hover.title), [
  '2026-10-26 02:00 PDT–02:05',
  '2026-10-26 02:05 PDT–02:10',
  '2026-10-26 02:10 PDT–02:12:30',
], `Local hover follows the browser zone and the same cutoff: ${local.hovers.map(hover => hover.title).join(' | ')}`);
assert.deepEqual(local.hovers.map(hover => `${hover.n} ${hover.value}`), ['n 1 0%', 'n 2 50%', 'n 2 50%']);
