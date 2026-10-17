// Execute shipped modules against the DOM parsed from the shipped dashboard.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { spawnSync } from 'node:child_process';

class Node {
  constructor(tag) {
    this.tagName = tag;
    this.children = [];
    this.className = '';
    this.text = '';
    this.attributes = new Map();
    this.dataset = {};
  }
  set textContent(value) { this.text = String(value); this.children = []; }
  get textContent() { return this.text + this.children.map(child => child.textContent ?? child).join(' '); }
  append(child) { this.children.push(child); }
  replaceChildren(...children) { this.text = ''; this.children = children; }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  set tabIndex(value) { this.setAttribute('tabindex', value); }
  get tabIndex() { return Number(this.getAttribute('tabindex') ?? -1); }
}
// Use a real HTML parser rather than assertions over source text. The small DOM
// implements only the element/text operations used by the shipped renderer.
const parsed = spawnSync('python3', ['-c', `
import json, sys
from html.parser import HTMLParser
class Parser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.root = {"tag": "document", "attrs": {}, "children": []}
        self.stack = [self.root]
    def handle_starttag(self, tag, attrs):
        node = {"tag": tag, "attrs": dict(attrs), "children": []}
        self.stack[-1]["children"].append(node)
        if tag not in {"area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"}:
            self.stack.append(node)
    def handle_endtag(self, tag):
        for index in range(len(self.stack) - 1, 0, -1):
            if self.stack[index]["tag"] == tag:
                self.stack = self.stack[:index]
                break
    def handle_data(self, data):
        self.stack[-1]["children"].append(data)
p = Parser()
p.feed(sys.stdin.read())
print(json.dumps(p.root))
`], { input: fs.readFileSync(new URL('../../assets/dashboard/index.html', import.meta.url), 'utf8'), encoding: 'utf8' });
assert.equal(parsed.status, 0, parsed.stderr);
const elements = [];
function buildDOM(value) {
  if (typeof value === 'string') return value;
  const node = new Node(value.tag);
  node.id = value.attrs.id;
  node.className = value.attrs.class || '';
  for (const [name, attr] of Object.entries(value.attrs)) {
    if (name.startsWith('data-')) node.dataset[name.slice(5)] = attr;
  }
  elements.push(node);
  node.children = value.children.map(buildDOM);
  return node;
}
buildDOM(JSON.parse(parsed.stdout));
const byId = id => elements.find(node => node.id === id);
const host = byId('host-resource-chips');
const descendants = node => node.children.filter(child => child instanceof Node).flatMap(child => [child, ...descendants(child)]);
const chips = () => host.children.filter(child => child instanceof Node);
const topbar = elements.find(node => node.tagName === 'header' && node.className === 'topbar');
assert.ok(descendants(topbar).includes(host), 'live readings belong to the topbar');
assert.equal(byId('host-resources'), undefined, 'there is no standalone host panel');
assert.deepEqual(chips().map(chip => chip.dataset.resource), ['cpu', 'memory', 'disk'], 'three host chips exist before the first fetch');
const providerHost = byId('provider-usage-chips');
const providerChips = () => providerHost.children.filter(child => child instanceof Node);
assert.ok(descendants(byId('host-resource-group')).includes(providerHost), 'provider usage sits in the selected host\'s group, so it names the same host');
assert.equal(providerChips().length, 1, 'one placeholder chip exists before the first fetch');

let now = 100000;
let nextResponse;
let resolvePending;
const urls = [];
const timers = new Map();
const document = { hidden: false, createElement: tag => new Node(tag), getElementById: byId };
const context = vm.createContext({
  URLSearchParams, window: { location: { search: '?workspace=all', hash: '' } }, document, console,
  Date: class extends Date { static now() { return now; } },
  setInterval: (callback, delay) => timers.set(delay, callback),
  setTimeout, clearTimeout, AbortController,
  fetch: async url => { urls.push(url); return nextResponse; },
});
const common = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/common.js', import.meta.url), 'utf8'), { context });
const resource = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/host-resources.js', import.meta.url), 'utf8'), { context });
await common.link(() => { throw new Error('unexpected common dependency'); });
await resource.link(name => { assert.equal(name, './common.js'); return common; });
await resource.evaluate();
const { renderHostResources, renderProviderUsage, fetchAndRenderHostResources, initHostResources, resetHostResources } = resource.namespace;
const reading = (percent, severity = 'ok', unknown_reason = null) => ({ percent, severity, unknown_reason });
const payload = {
  cpu: reading(193.7, 'critical'), memory: reading(82, 'elevated'), disk: { path: '/worst', ...reading(88, 'critical') },
  // A large legacy per-path payload must never produce additional chips.
  disks: Array.from({ length: 22 }, (_, index) => ({ path: `/workspace/${index}`, ...reading(20) })),
  severity: 'critical', sample_age_seconds: 2, max_age_seconds: 15, throttle: true,
  pressures: [{ resource: 'cpu' }, { resource: 'disk /held' }],
  reason: '<unsafe> cpu and disk high', thresholds: { enabled: true }, stale: false,
};
const text = chip => Object.fromEntries(['k', 'v'].map(name => [name, descendants(chip).find(node => node.className === name).textContent.replace(/\s+/g, ' ').trim()]));
const chip = name => chips().find(node => node.dataset.resource === name);
const readings = () => chips().map(text);
renderHostResources(payload);
assert.deepEqual(chips().map(node => node.dataset.resource), ['cpu', 'memory', 'disk'], 'cpu, memory and disk each keep their own chip');
assert.deepEqual(readings(), [{ k: 'load', v: '1.9× cores' }, { k: 'mem', v: '82%' }, { k: 'disk', v: '88%' }], 'load reads relative to cores; memory and disk read as percentages');
assert.doesNotMatch(chip('cpu').textContent, /cpu|193/, 'the cpu chip never shows load as a bare cpu percentage');
assert.deepEqual(chips().map(node => node.className), [
  'host-resource critical throttled', 'host-resource elevated', 'host-resource critical throttled',
], 'each chip carries its own severity, and only the held resources are throttled');
for (const node of chips()) {
  assert.equal(node.tabIndex, -1, 'static host readings do not add an unhelpful tab stop');
  assert.equal(node.getAttribute('role'), 'group');
  assert.equal(node.getAttribute('aria-label'), node.title, 'assistive technology reads the same detail as the title');
  assert.match(node.title, /sampled 2s ago/);
  assert.match(node.title, /Throttle verdict: held/);
  assert.match(node.title, /<unsafe> cpu and disk high/);
  assert.equal(descendants(node).some(child => child.tagName === 'unsafe'), false, 'throttle reasons never become markup');
}
assert.match(chip('cpu').title, /1-minute load average divided by online cores/, 'the load title explains the measure');
assert.match(chip('cpu').title, /193\.7% of cores \(critical\)/);
assert.match(chip('memory').title, /^memory 82\.0% \(elevated\)/);
assert.match(chip('disk').title, /^disk \/worst 88\.0% \(critical\)/, 'the disk title names the worst path');
const heldWords = node => descendants(node).filter(child => child.className === 'host-resource-held').map(child => child.textContent);
assert.deepEqual(heldWords(chip('cpu')), ['throttled'], 'the throttle verdict is accessible text on the held chip');
assert.deepEqual(heldWords(chip('memory')), [], 'a resource that is not held carries no throttled text');
assert.match(chip('cpu').title, /Throttle verdict: held on this resource/);
assert.doesNotMatch(chip('memory').title, /on this resource/);

// The verdict flipping changes each chip's state, never its visible text, so
// the bar's width and height cannot move with it.
renderHostResources({ ...payload, throttle: false, pressures: [] });
assert.deepEqual(chips().map(node => node.className), ['host-resource critical', 'host-resource elevated', 'host-resource critical']);
assert.deepEqual(readings(), [{ k: 'load', v: '1.9× cores' }, { k: 'mem', v: '82%' }, { k: 'disk', v: '88%' }]);
assert.ok(chips().every(node => heldWords(node).length === 0));
assert.match(chip('disk').title, /Throttle verdict: open/);

renderHostResources({ ...payload, pressures: [{ resource: 'memory' }] });
assert.deepEqual(chips().map(node => node.className), ['host-resource critical', 'host-resource elevated throttled', 'host-resource critical'], 'the held resource is the one marked throttled');

renderHostResources({ ...payload, disk: null });
assert.deepEqual(readings()[2], { k: 'disk', v: '-' }, 'an unknown disk reads as a dash');
assert.equal(chip('disk').className, 'host-resource unknown throttled', 'a held resource stays marked while its reading is unknown');
assert.match(chip('disk').title, /^disk unavailable/);
assert.deepEqual(readings().slice(0, 2), [{ k: 'load', v: '1.9× cores' }, { k: 'mem', v: '82%' }], 'an unknown disk does not hide the known readings');
renderHostResources({ ...payload, memory: { percent: null, severity: 'unknown', unknown_reason: 'meminfo unreadable' } });
assert.deepEqual(readings()[1], { k: 'mem', v: '-' });
assert.match(chip('memory').title, /^memory meminfo unreadable/, 'the unknown reason reaches the title');
renderHostResources({ ...payload, stale: true });
assert.deepEqual(readings(), [{ k: 'load', v: '-' }, { k: 'mem', v: '-' }, { k: 'disk', v: '-' }]);
assert.ok(chips().every(node => node.className === 'host-resource unknown'), 'a stale sample makes every chip unknown and none held');
assert.ok(chips().every(node => /stale · .*Throttle verdict: unknown/.test(node.title)));

nextResponse = { ok: true, json: async () => payload };
await fetchAndRenderHostResources();
assert.equal(urls[0], '/api/host/resources', 'workspace selection must not augment a host request');
initHostResources();
// Staleness advances even while the next poll is pending.
nextResponse = new Promise(resolve => { resolvePending = resolve; });
const pendingPoll = timers.get(5000)();
now += 14000;
timers.get(1000)();
assert.deepEqual(readings(), [{ k: 'load', v: '-' }, { k: 'mem', v: '-' }, { k: 'disk', v: '-' }]);
assert.match(chip('disk').title, /sampled 16s ago/);
assert.match(chip('disk').title, /Throttle verdict: unknown.*Resource sample expired/);
const recovered = { ...payload, throttle: false, severity: 'ok', cpu: reading(20), memory: reading(42), disk: { path: '/known', ...reading(50) }, pressures: [], reason: 'recovered' };
resolvePending({ ok: true, json: async () => recovered });
await pendingPoll;
assert.deepEqual(readings(), [{ k: 'load', v: '0.2× cores' }, { k: 'mem', v: '42%' }, { k: 'disk', v: '50%' }]);
assert.ok(chips().every(node => node.className === 'host-resource ok'));
assert.match(chip('disk').title, /Throttle verdict: open.*recovered/);
renderHostResources({ ...recovered, thresholds: { enabled: false } });
assert.match(chip('cpu').title, /Throttle verdict: disabled/);
nextResponse = { ok: false, status: 503 };
await assert.rejects(fetchAndRenderHostResources());
assert.equal(chips().length, 3);
assert.deepEqual(readings(), [{ k: 'load', v: '-' }, { k: 'mem', v: '-' }, { k: 'disk', v: '-' }]);
assert.match(chip('disk').title, /age unknown.*Throttle verdict: unknown.*Resource API unavailable/);

// Provider usage rides the same payload: one chip per provider and window,
// worst first, behind a summary chip that narrow bars show instead.
const observedToday = new Date().toISOString();
const usage = extra => ({
  provider: 'claude', scope: null, window: 'five_hour', used_percent: 5, exhausted: false, threshold: 90, gating: true,
  gated: false, partial: false, stale: false, age_seconds: 30, observed_at: observedToday, until: '2026-10-18T03:00:00Z',
  crews: ['opus'], ...extra,
});
const staleOff = usage({
  provider: 'codex', window: 'primary', used_percent: 100, exhausted: true, threshold: 'off', stale: true,
  age_seconds: 3 * 86400, observed_at: '2026-10-14T12:00:00Z', crews: [],
});
const unreported = usage({ scope: 'opus', window: 'seven_day', used_percent: null });
const limits = [usage({}), usage({ window: 'seven_day', used_percent: 26 }), staleOff];
const kv = node => Object.fromEntries(['k', 'v'].map(name => [name, descendants(node).find(child => child.className === name)?.textContent.trim()]));
const providerItems = () => providerChips().filter(node => node.className.includes('provider-usage-item'));
const providerSummary = () => providerChips().find(node => node.className.includes('provider-usage-summary'));
const base = 'host-resource provider-usage provider-usage-item';
renderProviderUsage({ provider_limits: limits });
assert.deepEqual(providerItems().map(kv), [
  { k: 'codex', v: '100%' }, { k: 'claude 5h', v: '5%' }, { k: 'claude 7d', v: '26%' },
], 'one chip per provider and window, worst first');
assert.deepEqual(providerItems().map(node => node.className), [
  `${base} critical stale gating-off`, `${base} ok`, `${base} ok`,
], 'stale and gating-off readings are marked in their chip class');
const [codexChip, fiveHour] = providerItems();
assert.deepEqual(descendants(codexChip).filter(node => node.className === 'provider-usage-flag').map(node => node.textContent), ['old'], 'a stale reading says so in visible text');
assert.deepEqual(descendants(fiveHour).filter(node => node.className === 'provider-usage-flag'), [], 'a fresh reading carries no flag');
for (const node of providerItems()) {
  assert.equal(node.getAttribute('role'), 'group');
  assert.equal(node.getAttribute('aria-label'), node.title, 'assistive technology reads the same detail as the title');
}
assert.match(codexChip.title, /codex primary 100%/);
assert.match(codexChip.title, /observed 2026-10-14T12:00:00Z \(3d old · stale\)/, 'the title carries the observed time and age');
assert.match(codexChip.title, /gating off/, 'the title says gating is off');
assert.match(fiveHour.title, /claude five_hour 5% · observed .* \(today\) · limit 90%/);
renderProviderUsage({ provider_limits: [unreported, usage({})] });
assert.deepEqual(providerItems().map(kv), [{ k: 'claude 5h', v: '5%' }, { k: 'claude opus 7d', v: '-' }], 'a missing usage reads as a dash and sorts after known readings');
assert.equal(providerItems()[1].className, `${base} unknown`);
assert.match(providerItems()[1].title, /claude \[opus\] seven_day usage not reported/);
renderProviderUsage({ provider_limits: limits });
assert.deepEqual(kv(providerSummary()), { k: 'usage', v: '100%' }, 'the summary chip shows the worst reading');
assert.equal(providerSummary().className, 'host-resource provider-usage provider-usage-summary critical stale gating-off');
assert.equal(providerSummary().title.split('\n').length, 3, 'the summary title lists every reading');
assert.equal(providerSummary().getAttribute('aria-label'), providerSummary().title);

// Near, at and over the limit; a gated reading names its crews.
renderProviderUsage({ provider_limits: [
  usage({ used_percent: 85 }),
  usage({ provider: 'codex', window: 'primary', used_percent: 92, gated: true, crews: ['sol'] }),
] });
assert.deepEqual(providerItems().map(node => node.className), [`${base} critical`, `${base} elevated`]);
assert.match(providerItems()[0].title, /gated at 92% ≥ 90% until 2026-10-18 03:00Z: sol skipped/);
assert.deepEqual(providerItems().map(kv), [{ k: 'codex', v: '92%' }, { k: 'claude 5h', v: '85%' }]);

// Beyond three windows the rest fold into one chip, so the bar cannot grow.
renderProviderUsage({ provider_limits: [...limits, unreported, usage({ window: 'weekly_extra', used_percent: 60 })] });
assert.equal(providerItems().length, 4);
assert.deepEqual(kv(providerItems().at(-1)), { k: '+2 more', v: undefined });
assert.equal(providerItems().at(-1).className, `${base} provider-usage-more unknown`);
assert.equal(providerItems().at(-1).title.split('\n').length, 5, 'the folded chip lists every reading');

// No readings keep one placeholder, and say why.
const placeholder = payload => { renderProviderUsage(payload); const chips = providerChips(); assert.equal(chips.length, 1); assert.equal(chips[0].className, 'host-resource provider-usage unknown'); return chips[0].title; };
assert.match(placeholder({}), /not reported by this host/, 'an older host sends no field');
assert.match(placeholder({ provider_limits: null }), /not reported by this host/);
assert.match(placeholder({ provider_limits: [] }), /No provider usage readings/);
assert.match(placeholder(null), /unavailable/);

// The readings follow the host poll: a fetch fills them, the 1s staleness tick
// leaves them alone, a failed poll or a host switch clears them.
nextResponse = { ok: true, json: async () => ({ ...recovered, provider_limits: limits }) };
await fetchAndRenderHostResources();
assert.equal(providerItems().length, 3, 'the poll renders the selected host\'s readings');
const shown = providerChips()[0];
timers.get(1000)();
assert.equal(providerChips()[0], shown, 'the staleness tick does not rebuild the provider chips');
resetHostResources();
assert.match(providerChips()[0].title, /unavailable/, 'switching host drops the previous host\'s readings');
nextResponse = { ok: true, json: async () => ({ ...recovered, provider_limits: limits }) };
await fetchAndRenderHostResources();
nextResponse = { ok: false, status: 503 };
await assert.rejects(fetchAndRenderHostResources());
assert.match(providerChips()[0].title, /unavailable/, 'a failed poll does not leave stale readings');
// The other host's readings replace the previous host's, and a hidden tab
// requests nothing: the readings have no poll of their own.
const otherHost = [usage({ provider: 'codex', window: 'primary', used_percent: 61 })];
nextResponse = { ok: true, json: async () => ({ ...recovered, provider_limits: otherHost }) };
await fetchAndRenderHostResources();
assert.deepEqual(providerItems().map(kv), [{ k: 'codex', v: '61%' }]);
document.hidden = true;
const requests = urls.length;
await timers.get(5000)();
assert.equal(urls.length, requests, 'a hidden tab does not poll host resources or provider usage');
document.hidden = false;

console.log('three topbar host chips (load, mem, disk): per-chip severity and held state, many paths, fixed text across throttle, unknown/stale and polling, and provider usage chips (stale, gating-off, missing usage, fold, host reset) passed');
