// Execute shipped modules against the DOM parsed from the shipped dashboard.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { spawnSync } from 'node:child_process';

class Node {
  constructor(tag) { this.tagName = tag; this.children = []; this.className = ''; this.text = ''; }
  set textContent(value) { this.text = String(value); this.children = []; }
  get textContent() { return this.text + this.children.map(child => child.textContent ?? child).join(' '); }
  append(child) { this.children.push(child); }
  replaceChildren(...children) { this.text = ''; this.children = children; }
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
assert.ok(descendants(byId('health-strip')).includes(host));
assert.equal(byId('host-resources'), undefined, 'there is no standalone host panel');
const stripChildren = byId('health-strip').children.filter(child => child instanceof Node);
assert.ok(stripChildren.indexOf(host) > stripChildren.indexOf(byId('kpi-window')), 'live resources follow the window label');
assert.equal(chips().length, 3, 'exactly three host chips exist before the first fetch');
assert.deepEqual(chips().map(chip => descendants(chip).find(node => node.className === 'k').textContent.trim()), ['cpu', 'memory', 'disk']);

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
  cpu: reading(95, 'critical'), memory: reading(82, 'elevated'), disk: { path: '/worst', ...reading(88, 'critical') },
  // A large legacy per-path payload must never produce additional chips.
  disks: Array.from({ length: 22 }, (_, index) => ({ path: `/workspace/${index}`, ...reading(20) })),
  severity: 'critical', sample_age_seconds: 2, max_age_seconds: 15, throttle: true,
  pressures: [{ resource: 'cpu' }, { resource: 'disk /held' }],
  reason: '<unsafe> cpu and disk high', thresholds: { enabled: true }, stale: false,
};
renderHostResources(payload);
assert.equal(chips().length, 3);
assert.equal(chips()[0].children[0].textContent, '95.0%');
assert.equal(chips()[2].children[0].textContent, '88.0%', 'disk uses the worst known aggregate');
assert.equal(chips()[0].className, 'kpi host-resource critical throttled');
assert.equal(chips()[1].className, 'kpi host-resource elevated');
assert.equal(chips()[2].className, 'kpi host-resource critical throttled');
assert.equal(chips()[2].children[2].textContent, 'throttled');
assert.match(chips()[2].title, /\/worst/);
for (const chip of chips()) {
  assert.match(chip.title, /sampled 2s ago/);
  assert.match(chip.title, /Throttle verdict: held/);
  assert.match(chip.title, /<unsafe> cpu and disk high/);
  assert.equal(chip.tabIndex, 0, 'keyboard users can inspect chip tooltips');
  assert.equal(chip.children.some(child => child.tagName === 'unsafe'), false, 'throttle reasons never become markup');
}
renderHostResources({ ...payload, disk: null });
assert.equal(chips()[2].children[0].textContent, '-');
assert.equal(chips()[2].className, 'kpi host-resource unknown throttled');
assert.match(chips()[2].title, /unavailable/);
renderHostResources({ ...payload, stale: true });
assert.ok(chips().every(chip => chip.children[0].textContent === '-'));
assert.ok(chips().every(chip => chip.className === 'kpi host-resource unknown'));
assert.match(chips()[2].title, /Throttle verdict: unknown/);

nextResponse = { ok: true, json: async () => payload };
await fetchAndRenderHostResources();
assert.equal(urls[0], '/api/host/resources', 'workspace selection must not augment a host request');
initHostResources();
// Staleness advances even while the next poll is pending.
nextResponse = new Promise(resolve => { resolvePending = resolve; });
const pendingPoll = timers.get(5000)();
now += 14000;
timers.get(1000)();
assert.ok(chips().every(chip => chip.children[0].textContent === '-'));
assert.match(chips()[2].title, /sampled 16s ago/);
assert.match(chips()[2].title, /Throttle verdict: unknown.*Resource sample expired/);
const recovered = { ...payload, throttle: false, severity: 'ok', cpu: reading(20), memory: reading(42), disk: { path: '/known', ...reading(50) }, pressures: [], reason: 'recovered' };
resolvePending({ ok: true, json: async () => recovered });
await pendingPoll;
assert.equal(chips()[0].children[0].textContent, '20.0%');
assert.ok(chips().every(chip => chip.className === 'kpi host-resource ok'));
assert.match(chips()[2].title, /Throttle verdict: open.*recovered/);
renderHostResources({ ...recovered, thresholds: { enabled: false } });
assert.match(chips()[0].title, /Throttle verdict: disabled/);
nextResponse = { ok: false, status: 503 };
await assert.rejects(fetchAndRenderHostResources());
assert.equal(chips().length, 3);
assert.ok(chips().every(chip => chip.children[0].textContent === '-'));
assert.match(chips()[2].title, /age unknown.*Throttle verdict: unknown.*Resource API unavailable/);
console.log('three topbar host chips: aggregate, many paths, severity, held/open, unknown/stale and polling passed');
