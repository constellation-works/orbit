// The reported aggregate blind spot: idle ready work and another host's live
// drain must be visible together, and selecting either must carry its scope.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

class Node {
  constructor(tag, attrs = {}, children = []) {
    this.tag = tag; this.attrs = attrs; this.children = children.filter(Boolean);
    this.textContent = attrs.text || ''; this.events = {};
    this.classList = { add: name => { this.attrs.class = `${this.attrs.class || ''} ${name}`; } };
  }
  append(...nodes) { this.children.push(...nodes); }
  setAttribute(name, value) { this.attrs[name] = value; }
  addEventListener(type, fn) { this.events[type] = fn; }
}
let hosts = [
  { name: 'owner', local: true }, { name: 'remote', ssh: 'remote' },
  { name: 'offline' }, { name: 'extra' },
];
let clock = 0, inFlight = 0, peak = 0;
const requests = [];
const calls = [];
const row = (name, phase, eligible) => ({ workspace_id: name, workspace_name: name, phase,
  slots_used: 1, slots_ceiling: 4, eligible_count: eligible, top_wait_reason: 'context_lock_conflict', holds: ['codex usage limit'] });
const common = new vm.SyntheticModule(['el', 'fetchJson', 'getRegisteredHosts', 'getHost'], function () {
  this.setExport('el', (tag, attrs, children) => new Node(tag, attrs, children));
  this.setExport('getRegisteredHosts', () => hosts);
  this.setExport('getHost', () => null);
  this.setExport('fetchJson', async path => {
    requests.push(path);
    inFlight++; peak = Math.max(peak, inFlight);
    await new Promise(resolve => setImmediate(resolve));
    inFlight--;
    if (path.includes('offline')) throw new Error('Host unreachable');
    return { items: [path.includes('remote') ? row('remote-work', 'draining', 0) : row('idle-ready', 'idle', 4)] };
  });
}, { context: undefined });
// Use the same realm so payload comparisons do not depend on VM prototypes.
const module = new vm.SourceTextModule(fs.readFileSync(new URL('../../assets/dashboard/js/fleet-drains.js', import.meta.url), 'utf8'));
const actualNow = Date.now;
Date.now = () => clock;
try {
  await module.link(() => common);
  await module.evaluate();
  const { fetchFleetDrains, peekFleetDrains, renderFleetDrains, fleetDrainLabel, FLEET_CACHE_MS } = module.namespace;
  const [payload, concurrent] = await Promise.all([fetchFleetDrains(), fetchFleetDrains()]);
  assert.strictEqual(payload, concurrent, 'concurrent refreshes share one fleet read');
  assert.equal(requests.length, 4);
  assert.equal(peak, 3, 'fan-out stays bounded to three host requests');
  assert.equal(payload.items.length, 4, 'unreachable hosts keep a row');
  assert.equal(payload.items[2].error, 'Host unreachable');
  assert.equal(fleetDrainLabel(payload), 'Draining · 1 of 3 · partial');
  const table = renderFleetDrains(payload, (...scope) => calls.push(scope));
  const body = table.children.find(child => child.tag === 'tbody');
  const ready = body.children[0];
  assert.equal(ready.children[4].textContent, '4', 'idle eligible work is visible');
  assert.equal(ready.children[5].textContent, 'context lock conflict');
  assert.equal(ready.children[6].textContent, 'codex usage limit');
  ready.events.click();
  body.children[1].events.click();
  assert.deepEqual(calls, [['owner', 'idle-ready'], ['remote', 'remote-work']], 'rows carry host and workspace to the dashboard scope switch');
  assert.equal(ready.children[1].children[0].tag, 'button', 'row navigation has a native keyboard control');
  assert.equal(body.children[2].events.click, undefined, 'unavailable hosts cannot select a workspace');
  await fetchFleetDrains();
  assert.equal(requests.length, 4, 'fresh snapshots are reused');
  clock = 30_000;
  await fetchFleetDrains();
  assert.equal(requests.length, 4, 'a snapshot still serves the next 30s panel poll');
  const stale = peekFleetDrains();
  clock = FLEET_CACHE_MS;
  assert.equal(peekFleetDrains().fresh, false, 'a snapshot older than the fleet cadence is not fresh');
  assert.equal(stale.payload, peekFleetDrains().payload, 'a panel poll can paint the stale snapshot');
  assert.equal(requests.length, 4, 'reading the cached fleet does not fetch');
  await fetchFleetDrains();
  assert.equal(requests.length, 8, 'a snapshot older than the fleet cadence is refreshed');
  hosts = [{ name: 'changed' }];
  const [updated, shared] = await Promise.all([fetchFleetDrains(), fetchFleetDrains()]);
  assert.strictEqual(updated, shared);
  assert.equal(updated.items[0].host, 'changed', 'a changed host list never reuses the previous fleet');
  assert.equal(requests.length, 9);
  console.log('fleet drain rendering, scope, partial hosts, cache and bounded fan-out passed');
} finally { Date.now = actualNow; }
