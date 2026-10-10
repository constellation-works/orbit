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

let now = 100000;
let nextResponse;
let resolvePending;
const urls = [];
const timers = new Map();
const document = { hidden: false, createElement: tag => new Node(tag), getElementById: byId };
const context = vm.createContext({
  URLSearchParams, window: { location: { search: '?workspace=all', hash: '' } }, document, console,
  Date: { now: () => now },
  setInterval: (callback, delay) => timers.set(delay, callback),
  setTimeout, clearTimeout, AbortController,
  fetch: async url => { urls.push(url); return nextResponse; },
});
const common = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/common.js', import.meta.url), 'utf8'), { context });
const resource = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/host-resources.js', import.meta.url), 'utf8'), { context });
await common.link(() => { throw new Error('unexpected common dependency'); });
await resource.link(name => { assert.equal(name, './common.js'); return common; });
await resource.evaluate();
const { renderHostResources, fetchAndRenderHostResources, initHostResources } = resource.namespace;
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
console.log('three topbar host chips (load, mem, disk): per-chip severity and held state, many paths, fixed text across throttle, unknown/stale and polling passed');
