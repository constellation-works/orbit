// Cumulative primary-metric chart through the shipped experiments renderer.
// The Rust harness runs this with TZ=America/Los_Angeles, so a Local label
// that still shows the UTC clock cannot pass.
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
  await checkCumulativeTooltipBounds();
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
