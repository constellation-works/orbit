// Cumulative primary-metric chart through the shipped experiments renderer.
// The Rust harness runs this with TZ=America/Los_Angeles, so a Local label
// that still shows the UTC clock cannot pass. With a Playwright module and an
// evidence directory it checks tooltip bounds in Chromium, or with
// `--authoring` the operator's create, edit and start flow.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

if (!process.argv[2]) {
  assert.equal(Intl.DateTimeFormat().resolvedOptions().timeZone, 'America/Los_Angeles', 'the harness pins the zone');
}

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
  // Geometry is exercised in Chromium mode, not in this clock-only DOM shim.
  getBoundingClientRect() { return { left: 0, right: 300, width: 300 }; }
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
  documentElement: { clientWidth: 1440 },
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
  conclude_authorized: true,
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

// With module/evidence arguments, exercise real browser geometry using the
// same buckets as the clock regression below. No browser is needed for that
// default VM mode, which remains part of the ordinary HTTP test target.
if (process.argv[2]) {
  if (process.argv.includes('--authoring')) await checkAuthoringFlow();
  else await checkCumulativeTooltipBounds();
  process.exit(0);
}

function jsonResponse(body) {
  return {
    ok: true,
    status: 200,
    headers: { get: () => null },
    json: async () => body,
    text: async () => JSON.stringify(body),
  };
}

let finishConclude;
function fetchImpl(url, options = {}) {
  if (options.method === 'POST') return new Promise(resolve => { finishConclude = resolve; });
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
const experimentForm = load('experiment-form.js');
const experiments = load('experiments.js');
const router = new vm.SyntheticModule(['setActiveTab'], function exportRouter() {
  this.setExport('setActiveTab', () => {});
}, { context, identifier: 'router.js' });

await common.link(() => { throw new Error('unexpected common dependency'); });
await sparkline.link(() => { throw new Error('unexpected sparkline dependency'); });
await router.link(() => { throw new Error('unexpected router dependency'); });
await experimentForm.link(specifier => {
  if (specifier === './common.js') return common;
  throw new Error(`unexpected experiment-form dependency ${specifier}`);
});
await experiments.link(specifier => {
  if (specifier === './common.js') return common;
  if (specifier === './sparkline.js') return sparkline;
  if (specifier === './router.js') return router;
  if (specifier === './experiment-form.js') return experimentForm;
  throw new Error(`unexpected experiments dependency ${specifier}`);
});
await common.evaluate();
await sparkline.evaluate();
await router.evaluate();
await experimentForm.evaluate();
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

const winner = experimentsBody.querySelector('select.ex-conclude-control');
const reason = experimentsBody.querySelector('textarea.ex-conclude-control');
winner.value = 'candidate'; winner.dispatch('change');
reason.value = 'Keep the candidate'; reason.dispatch('input');
readout.as_of = '2026-10-26T09:13:00Z';
await fetchAndRenderExperiments();
assert.equal(experimentsBody.querySelector('select.ex-conclude-control').value, 'candidate');
assert.equal(experimentsBody.querySelector('textarea.ex-conclude-control').value, 'Keep the candidate');
experimentsBody.querySelector('button.ex-conclude-submit').dispatch('click');
readout.as_of = '2026-10-26T09:14:00Z';
await fetchAndRenderExperiments();
assert.equal(experimentsBody.querySelector('button.ex-conclude-submit').disabled, true);
assert.match(experimentsBody.querySelector('.ex-conclude-status').textContent, /Concluding/);
setExperimentId(null);
await fetchAndRenderExperiments();
finishConclude(jsonResponse({}));
await new Promise(resolve => setImmediate(resolve));
assert.equal(experiments.namespace.getExperimentId(), null, 'a delayed submission cannot restore its old selection');
setExperimentId('exp-intraday');
await fetchAndRenderExperiments();
assert.equal(experimentsBody.querySelector('textarea.ex-conclude-control').value, '', 'changing experiment retires the draft');

const retiredWinner = experimentsBody.querySelector('select.ex-conclude-control');
const retiredReason = experimentsBody.querySelector('textarea.ex-conclude-control');
retiredWinner.value = 'candidate'; retiredWinner.dispatch('change');
retiredReason.value = 'Keep the candidate'; retiredReason.dispatch('input');
common.namespace.setWorkspace('ws_other');
await fetchAndRenderExperiments();
assert.equal(experimentsBody.querySelector('textarea.ex-conclude-control').value, '', 'a workspace change retires the conclusion draft');
assert.equal(experimentsBody.querySelector('select.ex-conclude-control').value, '', 'a workspace change retires the winning arm');
common.namespace.setWorkspace('ws_fixture');
await fetchAndRenderExperiments();
const failedWinner = experimentsBody.querySelector('select.ex-conclude-control');
const failedReason = experimentsBody.querySelector('textarea.ex-conclude-control');
failedWinner.value = 'candidate'; failedWinner.dispatch('change');
failedReason.value = 'Keep the candidate'; failedReason.dispatch('input');
experimentsBody.querySelector('button.ex-conclude-submit').dispatch('click');
setExperimentId(null);
await fetchAndRenderExperiments();
finishConclude({
  ok: false,
  status: 409,
  headers: { get: () => null },
  text: async () => JSON.stringify({ error: 'conclude refused' }),
});
await new Promise(resolve => setImmediate(resolve));
setExperimentId('exp-intraday');
await fetchAndRenderExperiments();
assert.equal(experimentsBody.querySelector('textarea.ex-conclude-control').value, '', 'a delayed failure cannot restore a retired draft');
assert.equal(experimentsBody.querySelector('.ex-conclude-status').textContent, '', 'a delayed failure cannot mark another selection');
assert.equal(experimentsBody.querySelector('button.ex-conclude-submit').disabled, false, 'a delayed failure cannot leave another selection pending');

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

// Usage: node dashboard_experiments_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/experiments-browser [--before]
async function checkCumulativeTooltipBounds() {
  const { default: http } = await import('node:http');
  const { default: path } = await import('node:path');
  const { pathToFileURL } = await import('node:url');
  const { dashboardFile, dashboardCsp } = await import('../../src/tests/dashboard_static.mjs');
  const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
  const evidence = path.resolve(process.argv[3]);
  fs.mkdirSync(evidence, { recursive: true });
  const suffix = process.argv.includes('--before') ? '-before' : '';
  const initializer = `
    import * as experiments from '/static/js/experiments.js';
    document.querySelector('.tab-pane[data-tab=analytics]').classList.add('active');
    document.querySelectorAll('.tab-pane[data-tab=analytics] > main').forEach(node => {
      node.style.display = node.id === 'analytics-experiments-main' ? 'grid' : 'none';
    });
    experiments.setExperimentId('exp-intraday');
    await experiments.fetchAndRenderExperiments();
    window.fixtureReady = true;
  `;
  let currentExperiment = experiment;
  let currentReadout = readout;
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://fixture');
    if (url.pathname.startsWith('/api/diagnostics/experiments/')) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(url.pathname.endsWith('/readout') ? currentReadout : currentExperiment));
      return;
    }
    const served = url.pathname === '/fixture.mjs'
      ? { data: initializer, type: 'text/javascript' } : dashboardFile(url.pathname);
    if (!served) { res.writeHead(404); res.end(); return; }
    const data = url.pathname === '/' ? served.data.toString().replace(
      /<script[^>]*src="[^"]*app.js"[^>]*><\/script>/,
      '<script type="module" src="/fixture.mjs"></script>',
    ) : served.data;
    res.writeHead(200, { 'content-type': served.type, 'content-security-policy': dashboardCsp() });
    res.end(data);
  });
  const results = [];
  const failures = [];
  const errors = [];
  let browser;
  try {
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
    page.on('pageerror', error => errors.push(String(error)));
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: 1000 });
      for (const scenario of ['short', 'long', 'wider']) {
        currentExperiment = structuredClone(experiment);
        currentReadout = structuredClone(readout);
        if (scenario !== 'short') {
          const names = ['controlWithAnUnbrokenArmNameThatIsLong', 'candidateWithAnUnbrokenArmNameThatIsLong'];
          currentExperiment.arms.forEach((arm, index) => { arm.name = names[index]; });
          currentReadout.arms.forEach((arm, index) => { arm.name = names[index]; });
          currentReadout.metrics[0].arms.forEach((arm, index) => { arm.arm = names[index]; });
          currentReadout.cumulative.arms.forEach((arm, index) => { arm.arm = names[index]; });
        }
        await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&case=${width}-${scenario}#analytics/experiments/exp-intraday`);
        await page.waitForFunction(() => window.fixtureReady);
        assert.equal(await page.locator('.ex-cum-col').count(), 3);
        if (scenario === 'short') {
          const winner = page.locator('select.ex-conclude-control');
          const reason = page.locator('textarea.ex-conclude-control');
          await winner.selectOption('candidate');
          await reason.fill('Keep the candidate');
          await reason.focus();
          currentReadout.as_of = '2026-10-26T09:13:00Z';
          await page.evaluate(async () => (await import('/static/js/experiments.js')).fetchAndRenderExperiments());
          assert.equal(await winner.inputValue(), 'candidate');
          assert.equal(await reason.inputValue(), 'Keep the candidate');
          assert.equal(await reason.evaluate(node => document.activeElement === node), true, 'poll restores conclusion focus');
          await reason.scrollIntoViewIfNeeded();
          await page.screenshot({ path: path.join(evidence, `conclusion-dark-${width}.png`) });
        }

        if (scenario === 'wider') {
          // Placement must follow measured size rather than the shared CSS's
          // current 244px desktop / 200px phone defaults.
          await page.locator('.ex-cum-tip').evaluate(node => { node.style.width = '280px'; });
        }
        await page.locator('.ex-cum-frame').scrollIntoViewIfNeeded();
        const overflow = () => page.evaluate(() => {
          const pane = document.querySelector('.tab-pane[data-tab=analytics]');
          return { document: document.scrollingElement.scrollWidth, viewport: document.documentElement.clientWidth,
            pane: pane.scrollWidth, paneClient: pane.clientWidth };
        });
        const before = await overflow();
        for (const bucket of [0, 1, 2]) {
          await page.locator('.ex-cum-col').nth(bucket).hover();
          const geometry = await page.locator('.ex-cum-tip').evaluate(node => {
            const rect = element => {
              const { left, right, top, bottom, width, height } = element.getBoundingClientRect();
              return { left, right, top, bottom, width, height };
            };
            const textRects = element => {
              const range = document.createRange();
              range.selectNodeContents(element);
              return [...range.getClientRects()].map(({ left, right, top, bottom }) => ({ left, right, top, bottom }));
            };
            return { tip: rect(node), frame: rect(node.parentElement),
              viewport: { left: 0, right: document.documentElement.clientWidth, top: 0, bottom: innerHeight },
              rows: [...node.querySelectorAll('.mx-tip-row')].map(row => ({
                name: row.querySelector('.mx-tip-name').textContent,
                nameRects: textRects(row.querySelector('.mx-tip-name')),
                value: row.querySelector('.mx-tip-value').textContent,
                valueRect: rect(row.querySelector('.mx-tip-value')),
              })) };
          });
          const after = await overflow();
          const label = `${width}px ${scenario} bucket ${bucket}`;
          assert.deepEqual(geometry.rows.map(row => row.name), currentReadout.cumulative.arms.map(arm => arm.arm), `${label}: requested arm labels render`);
          const contains = (outer, inner) => inner.left >= outer.left - 1 && inner.right <= outer.right + 1
            && inner.top >= outer.top - 1 && inner.bottom <= outer.bottom + 1;
          const check = (condition, message) => { if (!condition) failures.push(`${label}: ${message}`); };
          check(contains(geometry.viewport, geometry.tip), 'complete tooltip is inside the viewport');
          check(contains(geometry.frame, geometry.tip), 'complete tooltip is inside the chart frame');
          for (const row of geometry.rows) {
            check(contains(geometry.tip, row.valueRect), `${row.name}: ${row.value} stays inside the tooltip`);
            check(contains(geometry.viewport, row.valueRect), `${row.name}: value stays inside the viewport`);
            check(row.nameRects.every(rect => contains(geometry.tip, rect)), `${row.name}: full label wraps inside the tooltip`);
          }
          check(after.document <= after.viewport && after.document <= before.document, 'no document overflow is introduced');
          check(after.pane <= after.paneClient && after.pane <= before.pane, 'no pane overflow hides values');
          results.push({ width, scenario, bucket, ...geometry, before, after });
          if (bucket === 1) {
            await page.screenshot({ path: path.join(evidence, `cumulative-${scenario}-dark-${width}${suffix}.png`) });
          }
        }
        await page.mouse.move(0, 0);
        assert.equal(await page.locator('.ex-cum-tip').isVisible(), false, 'leaving the chart hides its tooltip');
      }
    }
    assert.deepEqual(errors, [], 'no browser errors');
    assert.deepEqual(failures, [], 'cumulative tooltips and metric values remain readable');
  } finally {
    fs.writeFileSync(path.join(evidence, 'bounds.json'), JSON.stringify({ results, failures, errors }, null, 2));
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}

// Usage: node dashboard_experiments_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/experiments-browser --authoring
// The fixture server stands in for the experiment routes and answers with
// core's own refusal text for the two refusals the flow provokes; the routes
// themselves are covered against the real server in experiments.rs.
async function checkAuthoringFlow() {
  const { default: http } = await import('node:http');
  const { default: path } = await import('node:path');
  const { pathToFileURL } = await import('node:url');
  const { dashboardFile, dashboardCsp } = await import('../../src/tests/dashboard_static.mjs');
  const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
  const evidence = path.resolve(process.argv[3]);
  fs.mkdirSync(evidence, { recursive: true });
  const authoring = {
    create: { authorized: true, reason: null },
    update: { authorized: true, reason: null },
    start: { authorized: true, reason: null },
    enabled: true,
    max_running: 3,
    config_keys: ['workflow.explore.max_calls_per_run'],
    metrics: ['cost_usd', 'first_pass_acceptance', 'lead_time'],
    complexities: ['low', 'medium', 'hard', 'xhard'],
    crews: [{ name: 'opus', enabled: true }, { name: 'sonnet', enabled: true }],
  };
  const initializer = `
    import * as experiments from '/static/js/experiments.js';
    document.querySelector('.tab-pane[data-tab=analytics]').classList.add('active');
    document.querySelectorAll('.tab-pane[data-tab=analytics] > main').forEach(node => {
      node.style.display = node.id === 'analytics-experiments-main' ? 'grid' : 'none';
    });
    window.experiments = experiments;
    experiments.setExperimentId(null);
    await experiments.fetchAndRenderExperiments();
    window.fixtureReady = true;
  `;
  let store = [];
  const requests = [];
  const view = experiment => ({
    ...experiment,
    enrollment: experiment.arms.map(arm => ({ arm: arm.name, enrolled: 0, segment_enrolled: 0 })),
    forced: 0,
    excluded: 0,
  });
  const apply = (experiment, body) => {
    for (const name of ['name', 'hypothesis', 'population', 'primary_metric', 'guardrails', 'min_sample', 'max_days', 'mode']) {
      if (body[name] !== undefined) experiment[name] = body[name];
    }
    if (body.arms) experiment.arms = body.arms.map(arm => ({ ...arm, overlay_digest: 'sha256:fixture', retired_segment: null }));
  };
  const refusal = (res, status, error) => {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error }));
  };
  const readBody = req => new Promise(resolve => {
    let text = '';
    req.on('data', chunk => { text += chunk; });
    req.on('end', () => resolve(text ? JSON.parse(text) : {}));
  });
  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, 'http://fixture');
    if (url.pathname.startsWith('/api/diagnostics/experiments')) {
      const body = req.method === 'GET' ? null : await readBody(req);
      requests.push({ method: req.method, path: url.pathname, body });
      const [, id, action] = url.pathname.replace('/api/diagnostics/experiments', '').split('/');
      const found = store.find(item => item.id === id);
      const json = value => { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); };
      if (!id && req.method === 'GET') return json({ experiments: store.map(view), authoring });
      for (const [metric, threshold] of Object.entries(body?.guardrails || {})) {
        if (!/^[+-]\d+(\.\d+)?(pt|%)?$/.test(threshold)) {
          return refusal(res, 400, `guardrail threshold \`${threshold}\` for \`${metric}\` must be a signed number with an optional \`pt\` or \`%\` unit, such as \`-5pt\` or \`+0\``);
        }
      }
      if (!id && req.method === 'POST') {
        const experiment = { id: `exp-${store.length + 1}`, status: 'draft', segment: 0, mode: 'shadow', hypothesis: '', population: {}, guardrails: {},
          primary_metric: null, min_sample: null, max_days: null, arms: [], events: [], created_at: new Date().toISOString() };
        apply(experiment, body);
        store.push(experiment);
        return json(experiment);
      }
      if (!found) return refusal(res, 400, `no experiment \`${id}\` in this workspace`);
      if (!action && req.method === 'PATCH') {
        if (found.status !== 'draft') {
          return refusal(res, 400, `experiment \`${found.name}\` is ${found.status}: its arms, overlays, population, metrics and sample rules are editable only in draft. Reweight or retire arms with reweight, or clone it into a new draft`);
        }
        apply(found, body);
        return json(found);
      }
      if (action === 'start' && req.method === 'POST') {
        if (!found.primary_metric) return refusal(res, 400, `experiment \`${found.name}\` cannot start: it has no primary metric`);
        found.status = 'running';
        found.segment = 1;
        return json(found);
      }
      if (action === 'readout') {
        const fixture = structuredClone(readout);
        fixture.arms = found.arms.map(arm => ({ name: arm.name, control: arm.control, weight: arm.weight, n: 0 }));
        fixture.cumulative = { metric: 'pass_rate', window: fixture.cumulative.window, arms: [] };
        fixture.metrics = [];
        return json(fixture);
      }
      if (!action) return json({ ...found, conclude_authorized: true, authoring });
      return refusal(res, 404, 'unexpected route');
    }
    const served = url.pathname === '/fixture.mjs'
      ? { data: initializer, type: 'text/javascript' } : dashboardFile(url.pathname);
    if (!served) { res.writeHead(404); res.end(); return; }
    const data = url.pathname === '/' ? served.data.toString().replace(
      /<script[^>]*src="[^"]*app.js"[^>]*><\/script>/,
      '<script type="module" src="/fixture.mjs"></script>',
    ) : served.data;
    res.writeHead(200, { 'content-type': served.type, 'content-security-policy': dashboardCsp() });
    res.end(data);
  });
  const errors = [];
  const steps = [];
  let browser;
  try {
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
    page.on('pageerror', error => errors.push(String(error)));
    for (const width of [1440, 375]) {
      store = [];
      requests.length = 0;
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&case=authoring-${width}#analytics/experiments`);
      await page.waitForFunction(() => window.fixtureReady);
      const form = page.locator('#experiments-form form.exf');
      const labelled = (scope, label) => scope.locator('label.exf-field', { hasText: new RegExp(`^${label}`) });
      const shot = name => page.screenshot({ path: path.join(evidence, `${name}-dark-${width}.png`) });

      // Create: the form covers the definition; crew pickers offer only the
      // workspace's configured crews.
      await page.locator('#experiments-new').click();
      assert.equal(await form.isVisible(), true, 'New experiment opens the form');
      assert.equal(await page.locator('#experiments-new').getAttribute('aria-expanded'), 'true');
      await labelled(form, 'Name').locator('input').fill('trial');
      await labelled(form, 'Hypothesis').locator('textarea').fill('A smaller crew pool keeps first-pass acceptance.');
      await form.locator('.exf-population label.exf-check', { hasText: 'medium' }).locator('input').check();
      await labelled(form, 'Tags to include').locator('input').fill('ui, docs');
      const treatment = form.locator('.exf-arm').nth(1);
      await treatment.locator('.exf-surface-crew > summary').click();
      await treatment.locator('.exf-add-crew').click();
      const crewOptions = await treatment.locator('.exf-surface-crew select').evaluate(node => [...node.options].map(option => option.value));
      assert.deepEqual(crewOptions, ['', 'opus', 'sonnet'], 'the crew picker lists the configured crews');
      await treatment.locator('.exf-surface-crew select').selectOption('sonnet');
      await treatment.locator('.exf-surface-skills > summary').click();
      assert.match(await treatment.locator('.exf-surface-skills > summary').textContent(), /Claude on Unix only/);
      await form.locator('.exf-add-guardrail').click();
      await form.locator('.exf-guardrails select').selectOption('cost_usd');
      await form.locator('.exf-guardrails input').fill('5pt');
      await page.evaluate(() => document.getElementById('experiments-form').scrollIntoView({ block: 'start' }));
      await shot('experiment-form');
      await treatment.evaluate(node => node.scrollIntoView({ block: 'start' }));
      await shot('experiment-form-arms');

      // Core's refusal lands under the field it names.
      await form.locator('.exf-save').click();
      const guardrailError = form.locator('.exf-guardrails .exf-error');
      await guardrailError.waitFor();
      assert.match(await guardrailError.textContent(), /guardrail threshold `5pt` for `cost_usd`/);
      await guardrailError.scrollIntoViewIfNeeded();
      await shot('field-error');
      await form.locator('.exf-guardrails input').fill('-5pt');
      await form.locator('.exf-save').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const created = requests.find(request => request.method === 'POST' && request.path === '/api/diagnostics/experiments' && request.body.guardrails.cost_usd === '-5pt');
      assert.ok(created, 'the draft is created');
      assert.deepEqual(created.body.population, { complexity: ['medium'], tags_include: ['ui', 'docs'] });
      assert.deepEqual(created.body.arms.map(arm => [arm.name, arm.control, arm.weight]), [['control', true, 1], ['treatment', false, 1]]);
      assert.deepEqual(created.body.arms[1].overlay, { crew: { pool: [{ crew: 'sonnet', weight: 1 }] } });
      assert.equal(created.body.mode, 'shadow', 'mode defaults to shadow');

      // A draft card offers Edit and Start; a refused start says why.
      const card = page.locator('.ex-item', { hasText: 'trial' });
      await card.locator('.exf-start').click();
      await card.locator('.exf-confirm-start').click();
      await card.locator('.exf-error').waitFor();
      assert.match(await card.locator('.exf-error').textContent(), /it has no primary metric/);
      await card.locator('.exf-confirm .action.cancel').click();

      // Edit: the form opens on the stored definition and patches it.
      await card.locator('.exf-edit').click();
      assert.equal(await labelled(form, 'Name').locator('input').inputValue(), 'trial');
      assert.equal(await form.locator('.exf-arm').nth(1).locator('.exf-surface-crew select').inputValue(), 'sonnet');
      await labelled(form, 'Primary metric').locator('select').selectOption('first_pass_acceptance');
      await labelled(form, 'Minimum sample per arm').locator('input').fill('20');
      await labelled(form, 'Reason').locator('textarea').fill('Measure acceptance');
      await form.locator('.exf-save').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const patch = requests.find(request => request.method === 'PATCH');
      assert.equal(patch.body.primary_metric, 'first_pass_acceptance');
      assert.equal(patch.body.min_sample, 20);
      assert.equal(patch.body.reason, 'Measure acceptance');
      assert.equal(patch.body.name, undefined, 'an unchanged name is not sent as a rename');

      // Start: the confirmation names arms, weights and population.
      await card.locator('.exf-start').click();
      const confirmation = await card.locator('.exf-confirm').textContent();
      for (const fragment of ['control (control) weight 1', 'treatment weight 1', 'complexity medium', 'tags include ui, docs', 'Shadow']) {
        assert.ok(confirmation.includes(fragment), `confirmation names ${fragment}: ${confirmation}`);
      }
      await card.locator('.exf-confirm textarea').fill('Ready to measure');
      await card.locator('.exf-confirm').scrollIntoViewIfNeeded();
      await shot('start-confirm');
      await card.locator('.exf-confirm-start').click();
      await card.locator('.ex-status-running').waitFor();
      assert.deepEqual(await card.locator('.ex-arm-progress .ex-arm-name').allTextContents(), ['controlcontrol', 'treatment']);
      assert.equal(await card.locator('.exf-edit').count(), 0, 'a running experiment offers no Edit');
      assert.deepEqual(requests.filter(request => request.path.endsWith('/start')).at(-1).body, { reason: 'Ready to measure' });

      // Save and start: one confirmation, two writes.
      await page.locator('#experiments-new').click();
      await labelled(form, 'Name').locator('input').fill('trial-b');
      await labelled(form, 'Primary metric').locator('select').selectOption('cost_usd');
      await labelled(form, 'Mode').locator('select').selectOption('live');
      await form.locator('.exf-save-start').click();
      const summary = await form.locator('.exf-confirm').textContent();
      assert.match(summary, /Start trial-b\?/);
      assert.match(summary, /every task/);
      assert.match(summary, /Live: .*changes how those dispatches run/);
      await form.locator('.exf-confirm-start').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const second = page.locator('.ex-item', { hasText: 'trial-b' });
      await second.locator('.ex-status-running').waitFor();
      await page.locator('.ex-list').scrollIntoViewIfNeeded();
      await shot('running');

      // A started experiment's detail says why there is no Edit.
      await page.evaluate(async () => {
        window.experiments.setExperimentId('exp-1');
        await window.experiments.fetchAndRenderExperiments();
      });
      await page.locator('.exf-locked').waitFor();
      assert.match(await page.locator('.exf-locked').textContent(), /running, so its definition can no longer be edited/);
      steps.push({ width, requests: requests.map(({ method, path: route }) => `${method} ${route}`) });
    }
    assert.deepEqual(errors, [], 'no browser errors');
  } finally {
    fs.writeFileSync(path.join(evidence, 'authoring.json'), JSON.stringify({ steps, errors }, null, 2));
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}
