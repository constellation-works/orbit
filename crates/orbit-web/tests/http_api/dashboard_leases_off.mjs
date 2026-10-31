// Execute the shipped leases module with a fake DOM: a workspace with path
// leases off reads as a neutral state in both panels (no error styling, no
// retry advice), a poll tick stops re-requesting the lease routes, an explicit
// Refresh or a workspace change rechecks them, and a genuine failure keeps its
// error styling and retry hint.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const classesOf = node => String(node.className || '').split(/\s+/).filter(Boolean);

class Node {
  constructor(tag) {
    this.tagName = String(tag).toUpperCase();
    this.children = [];
    this.parentNode = null;
    this.dataset = {};
    this.attrs = {};
    this.className = '';
    this.own = '';
    this.id = '';
  }
  get classList() {
    const node = this;
    return {
      add: name => { if (!classesOf(node).includes(name)) node.className = [...classesOf(node), name].join(' '); },
      remove: name => { node.className = classesOf(node).filter(entry => entry !== name).join(' '); },
      contains: name => classesOf(node).includes(name),
    };
  }
  set textContent(value) {
    this.children.forEach(child => { child.parentNode = null; });
    this.children = [];
    this.own = String(value);
  }
  get textContent() { return this.own + this.children.map(child => child.textContent ?? '').join(''); }
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
  addEventListener() {}
  // Only `.class` selectors: that is all the panels this test renders query.
  querySelectorAll(selector) {
    const name = selector.replace(/^\./, '');
    const out = [];
    const walk = node => {
      for (const child of node.children) {
        if (classesOf(child).includes(name)) out.push(child);
        walk(child);
      }
    };
    walk(this);
    return out;
  }
}

const bodies = Object.fromEntries(['leases-body', 'leases-count', 'lease-landings-body', 'lease-landings-count']
  .map(id => [id, Object.assign(new Node('div'), { id })]));
const document = {
  activeElement: null,
  body: new Node('body'),
  createElement: tag => new Node(tag),
  getElementById: id => bodies[id] || null,
};
const window = { location: { search: '?workspace=ws_orbit', hash: '' }, addEventListener: () => {} };

// Per-route replies, as [status, body]. The disabled reply is the server's own
// 400 for every lease route while `lease.enabled` is false.
const DISABLED = [400, { error: 'path leases are disabled in this workspace; set `lease.enabled = true` to accept lease operations.' }];
const routes = { '/api/leases': DISABLED, '/api/leases/notices': DISABLED };
const requests = [];
const respond = (status, body) => ({
  ok: status < 400, status, headers: { get: () => null },
  json: async () => body, text: async () => JSON.stringify(body),
});
const fetch = async path => {
  const url = new URL(path, 'http://dashboard.test');
  requests.push(url.pathname);
  const [status, body] = routes[url.pathname] || [404, { error: `unexpected ${url.pathname}` }];
  return respond(status, body);
};

const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, fetch, document, window,
});
const dir = new URL('../../assets/dashboard/js/', import.meta.url);
const load = name => new vm.SourceTextModule(fs.readFileSync(new URL(name, dir), 'utf8'), { context, identifier: name });
const common = load('common.js');
const leases = load('leases.js');
await common.link(() => { throw new Error('unexpected common dependency'); });
await leases.link(specifier => { assert.equal(specifier, './common.js'); return common; });
await leases.evaluate();
const { fetchAndRenderLeases, fetchAndRenderLeaseLandings } = leases.namespace;
const { runPanelPoll, setWorkspace } = common.namespace;

const leaseReads = () => requests.filter(path => path.startsWith('/api/leases')).length;
const poll = () => runPanelPoll(true, () => Promise.all([fetchAndRenderLeases(), fetchAndRenderLeaseLandings()]));
const refresh = () => Promise.all([fetchAndRenderLeases(), fetchAndRenderLeaseLandings()]);
const leasesBody = bodies['leases-body'];
const landingsBody = bodies['lease-landings-body'];

function assertOff() {
  assert.ok(leasesBody.textContent.includes('Path leases are off in this workspace'), 'the Leases panel says the feature is off');
  assert.ok(leasesBody.textContent.includes('lease.enabled = true'), 'the Leases panel names the setting that enables it');
  assert.ok(landingsBody.textContent.includes('Off while path leases are disabled.'), 'the landings panel says it is off');
  assert.equal(bodies['leases-count'].textContent, '—');
  assert.equal(bodies['lease-landings-count'].textContent, '—');
  for (const body of [leasesBody, landingsBody]) {
    assert.equal(body.querySelectorAll('.action-error').length, 0, 'an off workspace is not styled as an error');
    assert.doesNotMatch(body.textContent, /Unable to load|Use Refresh/, 'an off workspace gives no retry advice');
  }
}

// Disabled on first load: each route is asked once and both panels read as off.
await refresh();
assertOff();
assert.equal(leaseReads(), 2, 'the first load asks each lease route once');

// A poll tick does not repeat the disabled reads.
await poll();
assertOff();
assert.equal(leaseReads(), 2, 'a poll tick does not re-request the disabled lease routes');

// An explicit Refresh rechecks once and the state holds.
await refresh();
assertOff();
assert.equal(leaseReads(), 4, 'an explicit Refresh rechecks each lease route once');

// A workspace change is a new visit: the next poll checks it again.
setWorkspace('ws_other');
await poll();
assertOff();
assert.equal(leaseReads(), 6, 'a workspace change lets the next poll recheck the routes');

// Enabled: the lists render as before, and the poll reads them again.
routes['/api/leases'] = [200, { leases: [] }];
routes['/api/leases/notices'] = [200, { notices: [], cursor: 0 }];
await refresh();
assert.ok(leasesBody.textContent.includes('No active or queued leases in this workspace'), 'an enabled workspace shows the lease list');
assert.ok(!leasesBody.textContent.includes('off in this workspace'), 'an enabled workspace is not shown as off');
assert.ok(landingsBody.textContent.includes('No landed commit has changed a leased path yet.'), 'an enabled workspace shows the landings list');
const beforeEnabledPoll = leaseReads();
await poll();
assert.equal(leaseReads(), beforeEnabledPoll + 2, 'once enabled, a poll tick reads the lease routes again');

// The panel has loaded data by now, so a genuine failure reads as stale data
// (common.js panelMessage); the retry hint and error styling still apply.
// A 400 that does not name the setting is a genuine failure, not the off state.
routes['/api/leases'] = [400, { error: 'query and body principal must match' }];
await assert.rejects(fetchAndRenderLeases(), { message: 'query and body principal must match' });
assert.ok(leasesBody.textContent.includes('query and body principal must match. Use Refresh to retry.'), 'a 400 that does not name the setting keeps the retry hint');
assert.equal(leasesBody.querySelectorAll('.action-error').length, 1, 'a genuine failure keeps its error styling');

// A genuine server failure keeps its error and retry hint, and polling still reads it.
routes['/api/leases'] = [500, { error: 'boom' }];
await assert.rejects(fetchAndRenderLeases(), { message: 'boom' });
assert.ok(leasesBody.textContent.includes('Refresh failed; showing stale data: boom. Use Refresh to retry.'), 'a genuine failure keeps the retry hint');
assert.equal(leasesBody.querySelectorAll('.action-error').length, 1, 'a genuine failure keeps its error styling');
const beforeFailurePoll = leaseReads();
await runPanelPoll(true, () => fetchAndRenderLeases().catch(() => {}));
assert.equal(leaseReads(), beforeFailurePoll + 1, 'a genuine failure is not treated as the off state by the poll');

console.log('dashboard leases off-state behavior passed');
