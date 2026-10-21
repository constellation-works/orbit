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
const findAll = (node, match, found = []) => {
  if (typeof node !== 'object') return found;
  if (match(node)) found.push(node);
  for (const child of node.children) findAll(child, match, found);
  return found;
};
const fieldOf = (row, name) => findAll(row, node => node.attrs['data-field'] === name)[0];
// Visible text: field names for assistive tech are left out.
const shown = node => typeof node === 'string' ? node
  : (node.attrs.class || '').includes('visually-hidden') ? '' : node.textContent + node.children.map(shown).join('');
let hosts = [
  { name: 'owner', local: true }, { name: 'remote', ssh: 'remote' },
  { name: 'offline' }, { name: 'extra' },
];
let clock = 0, inFlight = 0, peak = 0;
const requests = [];
const calls = [];
const row = (name, phase, eligible) => ({ workspace_id: name, workspace_name: name, phase,
  slots_used: 1, slots_ceiling: 4, eligible_count: eligible, top_wait_reason: 'context_lock_conflict', holds: ['codex usage limit'] });
// A second window waiting behind the remote drain, started without a Claude
// credential while its window names a Claude crew.
const queuedWindow = { run_id: 'jrun-queued', completion: 'review', max_active_leaf_runs: 5, allowed_crews: ['opus'],
  for_seconds: 28800, env_pass_unset: ['CLAUDE_CODE_OAUTH_TOKEN', 'ANTHROPIC_API_KEY'], uncredentialed_crews: ['opus'] };
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
    return { items: [path.includes('remote') ? { ...row('remote-work', 'draining', 0), queued_drains: [queuedWindow] } : row('idle-ready', 'idle', 4)] };
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
  assert.equal(fleetDrainLabel(payload), '1 of 3 workspaces draining · partial');
  const card = renderFleetDrains(payload, (...scope) => calls.push(scope));
  const rows = findAll(card, node => node.tag === 'li');
  const ready = rows[0];
  assert.equal(shown(fieldOf(ready, 'eligible')), '4 eligible', 'idle eligible work is visible');
  assert.equal(shown(fieldOf(ready, 'top-wait')), 'top wait: context lock conflict');
  assert.equal(shown(fieldOf(ready, 'holds')), 'held: codex usage limit');
  assert.equal(fieldOf(ready, 'slots').children[0].textContent, 'Slots ', 'slots keep their field name for assistive tech');
  assert.equal(fieldOf(ready, 'queued'), undefined, 'a workspace with nothing queued adds no queued field');
  const queued = fieldOf(rows[1], 'queued');
  assert.equal(shown(queued), '1 queued window', 'a queued window behind a live drain is visible');
  assert.equal(queued.title, 'jrun-queued · 5 parallel · review · opus · 8h 00m window', 'its run, ceiling, completion, crews and window are named');
  assert.equal(shown(fieldOf(rows[1], 'queued-credential')), 'queued without Claude credential: opus', 'a queued window without a Claude credential is flagged');
  ready.events.click();
  rows[1].events.click();
  assert.deepEqual(calls, [['owner', 'idle-ready'], ['remote', 'remote-work']], 'rows carry host and workspace to the dashboard scope switch');
  assert.ok(findAll(ready, node => node.tag === 'button').length === 1, 'row navigation has a native keyboard control');
  assert.equal(rows[2].events.click, undefined, 'unavailable hosts cannot select a workspace');
  assert.equal(shown(fieldOf(rows[2], 'error')), 'Host unreachable', 'unavailable hosts keep their error');
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
