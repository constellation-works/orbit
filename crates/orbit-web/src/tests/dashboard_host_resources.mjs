// Execute the shipped modules against a small DOM fixture; no source-text assertions.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

class Node {
  constructor(tag) { this.tagName = tag; this.children = []; this.className = ''; this.text = ''; }
  set textContent(value) { this.text = String(value); this.children = []; }
  get textContent() { return this.text + this.children.map(child => child.textContent ?? child).join(' '); }
  append(child) { this.children.push(child); }
  replaceChildren(...children) { this.text = ''; this.children = children; }
}
const host = new Node('section');
let now = 100000;
let nextResponse;
const urls = [];
const timers = new Map();
const document = { hidden: false, createElement: tag => new Node(tag), getElementById: id => id === 'host-resources' ? host : null };
const context = vm.createContext({
  URLSearchParams, window: { location: { search: '?workspace=all', hash: '' } }, document, console,
  Date: { now: () => now },
  setInterval: (callback, delay) => timers.set(delay, callback),
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
  cpu: reading(95, 'critical'), memory: reading(42), disks: [{ path: '/workspace', ...reading(88, 'critical') }, { path: '/global', ...reading(null, 'unknown', 'unavailable') }],
  severity: 'critical', sample_age_seconds: 2, max_age_seconds: 15, throttle: true, reason: '<unsafe> cpu high', thresholds: { enabled: true }, stale: false,
};
renderHostResources(payload);
const tiles = host.children[1].children;
assert.equal(tiles.length, 4);
assert.equal(tiles[0].children[1].textContent, '95.0%');
assert.equal(tiles[0].className, 'host-resource-tile critical');
assert.equal(tiles[3].children[1].textContent, 'Unknown');
assert.equal(tiles[3].className, 'host-resource-tile unknown');
assert.equal(host.children[2].textContent, 'Throttle verdict: held · <unsafe> cpu high');
assert.equal(host.children[2].children.length, 0, 'reason is a text node, never HTML');

nextResponse = { ok: true, json: async () => payload };
await fetchAndRenderHostResources();
assert.equal(urls[0], '/api/host/resources', 'aggregate selection must not augment a host request');
initHostResources();
now += 14000;
timers.get(1000)();
assert.equal(host.children[1].children[0].children[1].textContent, 'Unknown', 'aging readings must stop displaying a healthy percentage');
assert.equal(host.children[2].textContent, 'Throttle verdict: unknown · Resource sample expired; awaiting a fresh verdict');
nextResponse = { ok: true, json: async () => ({ ...payload, throttle: false, severity: 'ok', cpu: reading(20), disks: [], reason: 'recovered' }) };
await timers.get(5000)();
assert.equal(host.children[1].children[0].children[1].textContent, '20.0%');
assert.equal(host.children[2].textContent, 'Throttle verdict: open · recovered');
nextResponse = { ok: false, status: 503 };
await assert.rejects(fetchAndRenderHostResources());
assert.equal(host.children[1].children[0].children[1].textContent, 'Unknown');
assert.equal(host.children[2].textContent, 'Throttle verdict unknown · resource API unavailable');
console.log('host resource dashboard behavior passed');
