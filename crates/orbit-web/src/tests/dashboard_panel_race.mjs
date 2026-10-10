// Superseded audit, policy, and scoreboard responses must not paint.
// Holds the real module fetches, then releases an older request after a newer
// one — a later search and a workspace A→B — and checks aggregate view clears
// every scoreboard surface the previous workspace filled.
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
    this.listeners = {};
    this.className = '';
    this.own = '';
    this.style = { setProperty(name, value) { this[name] = value; } };
  }
  get classList() {
    const node = this;
    return {
      add: name => { if (!classesOf(node).includes(name)) node.className = [...classesOf(node), name].join(' '); },
      remove: name => { node.className = classesOf(node).filter(entry => entry !== name).join(' '); },
      contains: name => classesOf(node).includes(name),
      toggle: (name, on) => (on ?? !classesOf(node).includes(name)) ? node.classList.add(name) : node.classList.remove(name),
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
  addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); }
  contains(node) { for (let current = node; current; current = current.parentNode) if (current === this) return true; return false; }
  focus() { document.activeElement = this; }
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

const ids = [
  'audit-body', 'audit-policy-body', 'audit-count', 'audit-search',
  'plugins-body', 'plugins-count',
  'scoreboard-body', 'scoreboard-narrative', 'scoreboard-agent-strip', 'scoreboard-meta',
  'scoreboard-insights', 'scoreboard-orchestration', 'scoreboard-count',
  'scoreboard-insights-count', 'scoreboard-orchestration-count',
];
const byId = Object.fromEntries(ids.map(id => [id, Object.assign(new Node('div'), { id })]));
const document = {
  activeElement: null,
  body: new Node('body'),
  createElement: tag => new Node(tag),
  createDocumentFragment: () => new Node('#fragment'),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => byId[id] || null,
};
const window = { location: { search: '?window=24h', hash: '' }, confirm: () => true };
const pending = [];
const response = (payload, status = 200) => ({ ok: status >= 200 && status < 300, status, json: async () => payload, text: async () => JSON.stringify(payload) });
const fetch = (path, options = {}) => new Promise(resolve => pending.push({ url: new URL(path, 'http://dashboard.test'), options, resolve }));
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, fetch, document, window, Node,
});
const modules = new Map();
const dir = new URL('../../assets/dashboard/js/', import.meta.url);
const load = name => {
  if (!modules.has(name)) {
    const url = new URL(name, dir);
    modules.set(name, new vm.SourceTextModule(fs.readFileSync(url, 'utf8'), { context, identifier: url.href }));
  }
  return modules.get(name);
};
const scoreboardModule = load('scoreboard.js');
await scoreboardModule.link(specifier => load(specifier.replace(/^\.\//, '')));
await scoreboardModule.evaluate();
const audit = modules.get('audit.js').namespace;
const scoreboard = scoreboardModule.namespace;
const common = modules.get('common.js').namespace;
const pluginsModule = load('plugins.js');
await pluginsModule.link(specifier => load(specifier.replace(/^\.\//, '')));
await pluginsModule.evaluate();
const plugins = pluginsModule.namespace;
const ctx = {};
const text = id => document.getElementById(id).textContent;
const settle = async () => { for (let i = 0; i < 6; i += 1) await new Promise(resolve => setTimeout(resolve, 0)); };
const placeheld = id => [...document.getElementById(id).children].some(child => child.dataset.key === 'aggregate-placeholder');

function event(marker) {
  return [{ id: marker, command: 'tool', subcommand: 'run-mcp', target_id: marker, status: 'success', timestamp: '2026-10-04T00:00:00Z', role: 'grok', tool_name: 'orbit.task.show', duration_ms: 1, exit_code: 0 }];
}
function policy(marker) {
  return { total: 1, by_profile: [{ name: marker, count: 2 }], by_target: [], by_run: [], by_execution: [], by_agent: [], recent_denials: [], top_causes: [] };
}
function summary(marker) {
  return {
    window: '24h',
    agents: { claude: { tasks_created: 12, tasks_planned: 1, tasks_completed: 1, tool_calls: 4 } },
    orchestration: { since: marker, until: 'cutoff', as_of: 'now', buckets: [], normalized_tokens: { normalized_token_total: 10, invocation_count: 1, linked_task_count: 1 } },
  };
}

async function overlap(start, olderPayload, newerPayload) {
  const before = pending.length;
  const first = start();
  const second = start();
  assert.equal(pending.length, before + 2, 'both requests stay in flight');
  const older = pending[before];
  const newer = pending[before + 1];
  newer.resolve(response(newerPayload));
  await settle();
  older.resolve(response(olderPayload));
  await settle();
  await Promise.all([first, second]);
  pending.splice(before, 2);
}

function shows(id, marker, absent) {
  const rendered = text(id);
  assert.ok(rendered.includes(marker), `${id} should show ${marker}: ${rendered}`);
  assert.ok(!rendered.includes(absent), `${id} kept superseded ${absent}: ${rendered}`);
}

common.setMultiWorkspace(true);
common.setWorkspace('ws-a');
await settle();

await overlap(() => audit.fetchAndRenderAudit(ctx), event('audit-old-refresh'), event('audit-new-refresh'));
shows('audit-body', 'audit-new-refresh', 'audit-old-refresh');
const auditRow = document.getElementById('audit-body').querySelector('tr');
assert.ok(auditRow, 'audit row is present for the expansion re-render');
for (const listener of auditRow.listeners.click || []) listener();
await settle();
shows('audit-body', 'audit-new-refresh', 'audit-old-refresh');

audit.applyAuditHashQuery(new URLSearchParams('q=a'));
const broad = audit.fetchAndRenderAudit(ctx);
audit.applyAuditHashQuery(new URLSearchParams('q=abc'));
const typed = audit.fetchAndRenderAudit(ctx);
assert.ok(pending.at(-2).url.searchParams.get('q') === 'a', 'the older search is the broad query');
assert.equal(pending.at(-1).url.searchParams.get('q'), 'abc', 'the newer search is the typed query');
pending.at(-1).resolve(response(event('typed-query')));
await settle();
pending.at(-2).resolve(response(event('broad-query')));
await settle();
await Promise.all([broad, typed]);
shows('audit-body', 'typed-query', 'broad-query');

const lateAudit = audit.fetchAndRenderAudit(ctx);
assert.equal(pending.at(-1).url.searchParams.get('workspace'), 'ws-a');
common.setWorkspace('ws-b');
const currentAudit = audit.fetchAndRenderAudit(ctx);
assert.equal(pending.at(-1).url.searchParams.get('workspace'), 'ws-b');
pending.at(-1).resolve(response(event('audit-workspace-b')));
await settle();
pending.at(-2).resolve(response(event('audit-workspace-a')));
await settle();
await Promise.all([lateAudit, currentAudit]);
shows('audit-body', 'audit-workspace-b', 'audit-workspace-a');

common.setWorkspace('ws-a');
await overlap(() => audit.fetchAndRenderPolicy(ctx), policy('policy-old-refresh'), policy('policy-new-refresh'));
shows('audit-policy-body', 'policy-new-refresh', 'policy-old-refresh');
const policyHeader = document.getElementById('audit-policy-body').querySelector('th');
for (const listener of policyHeader.listeners.click || []) listener();
await settle();
shows('audit-policy-body', 'policy-new-refresh', 'policy-old-refresh');

const latePolicy = audit.fetchAndRenderPolicy(ctx);
common.setWorkspace('ws-b');
const currentPolicy = audit.fetchAndRenderPolicy(ctx);
pending.at(-1).resolve(response(policy('policy-workspace-b')));
await settle();
pending.at(-2).resolve(response(policy('policy-workspace-a')));
await settle();
await Promise.all([latePolicy, currentPolicy]);
shows('audit-policy-body', 'policy-workspace-b', 'policy-workspace-a');

common.setWorkspace('ws-a');
await overlap(() => scoreboard.fetchAndRenderScoreboard(), summary('score-old-refresh'), summary('score-new-refresh'));
shows('scoreboard-orchestration', 'score-new-refresh', 'score-old-refresh');
assert.ok(text('scoreboard-narrative').includes('claude'), 'scoreboard narrative painted the newer summary');
assert.ok(text('scoreboard-agent-strip').includes('claude'), 'scoreboard agent strip painted the newer summary');

const lateScore = scoreboard.fetchAndRenderScoreboard();
assert.equal(pending.at(-1).url.pathname, '/api/scoreboard');
assert.equal(pending.at(-1).url.searchParams.get('workspace'), 'ws-a');
common.setWorkspace('ws-b');
assert.ok(!text('scoreboard-orchestration').includes('score-new-refresh'), 'workspace change clears scoreboard chrome before the next response');
const currentScore = scoreboard.fetchAndRenderScoreboard();
assert.equal(pending.at(-1).url.searchParams.get('workspace'), 'ws-b');
pending.at(-1).resolve(response(summary('score-workspace-b')));
await settle();
pending.at(-2).resolve(response(summary('score-workspace-a')));
await settle();
await Promise.all([lateScore, currentScore]);
shows('scoreboard-orchestration', 'score-workspace-b', 'score-workspace-a');
assert.ok(text('scoreboard-body').includes('claude'), 'scoreboard matrix shows the newer workspace');
assert.ok(!text('scoreboard-insights').includes('score-workspace-a'), 'insights do not keep the previous workspace');

common.setWorkspace(null);
scoreboard.placeholdScoreboardAggregate();
for (const id of ['scoreboard-body', 'scoreboard-narrative', 'scoreboard-agent-strip', 'scoreboard-insights', 'scoreboard-orchestration']) {
  assert.ok(placeheld(id), `${id} is placeheld in aggregate view`);
  assert.ok(!text(id).includes('score-workspace-b'), `${id} still shows the previous workspace in aggregate view: ${text(id)}`);
  assert.ok(!text(id).includes('claude'), `${id} still shows the previous agent strip in aggregate view: ${text(id)}`);
}
for (const id of ['scoreboard-meta', 'scoreboard-count', 'scoreboard-insights-count', 'scoreboard-orchestration-count']) {
  assert.equal(text(id), '—', `${id} keeps a previous-workspace count in aggregate view`);
}

common.setWorkspace('ws-a');
const crossing = scoreboard.fetchAndRenderScoreboard();
assert.equal(pending.at(-1).url.searchParams.get('workspace'), 'ws-a');
common.setWorkspace(null);
scoreboard.placeholdScoreboardAggregate();
pending.at(-1).resolve(response(summary('score-after-aggregate')));
await settle();
await crossing;
for (const id of ['scoreboard-body', 'scoreboard-narrative', 'scoreboard-agent-strip', 'scoreboard-insights', 'scoreboard-orchestration']) {
  assert.ok(placeheld(id), `${id} stays placeheld after a late response`);
  assert.ok(!text(id).includes('score-after-aggregate'), `${id} painted a late workspace response in aggregate view`);
}
await audit.fetchAndRenderAudit(ctx);
await audit.fetchAndRenderPolicy(ctx);
assert.ok(placeheld('audit-body') && !text('audit-body').includes('audit-workspace-b'), 'aggregate audit drops the previous workspace');
assert.ok(placeheld('audit-policy-body') && !text('audit-policy-body').includes('policy-workspace-b'), 'aggregate policy drops the previous workspace');

common.setWorkspace('ws-a');
const initialPlugins = plugins.fetchAndRenderPlugins();
assert.equal(pending.at(-1).url.pathname, '/api/plugins');
assert.equal(pending.at(-1).options.method, undefined, 'plugin listing uses GET');
pending.at(-1).resolve(response([{
  name: 'example', version: '1', status: 'disabled', host_enabled: false, workspace_toggle: false,
  capabilities: { enable: { authorized: true } }, panels: [],
}]));
await settle();
await initialPlugins;

const pluginCard = () => document.getElementById('plugins-body').querySelector('.plugin-card');
const pluginToggle = () => pluginCard().querySelector('.plugin-toggle');
const staleRefreshAction = pluginToggle().listeners.click[0]();
const enableRequest = pending.at(-1);
assert.equal(enableRequest.url.pathname, '/api/plugins/example/enable');
assert.equal(enableRequest.options.method, 'POST');
enableRequest.resolve(response({ plugin: {} }));
await settle();
const failedListing = pending.at(-1);
assert.equal(failedListing.url.pathname, '/api/plugins');
failedListing.resolve(response({ error: 'plugin listing unavailable' }, 500));
await staleRefreshAction;
assert.equal(pluginCard().querySelector('.plugin-change-error'), null, 'a failed listing refresh is not shown as a failed plugin change');
assert.ok(document.getElementById('plugins-body').querySelector('.panel-placeholder.action-error')?.textContent.includes('Refresh failed; showing stale data'), 'a failed listing refresh is reported by the panel stale-data note');
assert.ok(pluginCard().textContent.includes('Host: disabled'), 'a failed listing refresh retains the last plugin state');

const rejectedAction = pluginToggle().listeners.click[0]();
const refusedEnable = pending.at(-1);
assert.equal(refusedEnable.url.pathname, '/api/plugins/example/enable');
refusedEnable.resolve(response({ error: 'plugin write refused' }, 403));
await rejectedAction;
assert.equal(pluginCard().querySelector('.plugin-change-error')?.textContent, 'plugin write refused', 'a rejected mutation remains visible on its plugin card');

// Certification warnings use host SemVer precedence and disappear on refresh.
for (const [certified, host, behind] of [
  ['0.24.0', '0.28.0', true],
  ['0.9.0', '0.10.0', true],
  ['0.28.0', '0.28.0', false],
  ['0.29.0', '0.28.0', false],
  ['0.28.0-rc.1', '0.28.0', true],
  ['0.28.0', '0.28.0-rc.1', false],
  ['0.28.0-rc.2', '0.28.0-rc.10', true],
  ['0.28.0-alpha', '0.28.0-beta', true],
  ['0.28.0-1', '0.28.0-alpha', true],
  ['0.28.0-alpha', '0.28.0-alpha.1', true],
  ['0.28.0+build.1', '0.28.0+build.2', false],
  ['0.28.0-01', '0.28.0', false],
  ['unknown', '0.28.0', false],
  ['0.24.0', undefined, false],
  [undefined, '0.28.0', false],
]) {
  const refresh = plugins.fetchAndRenderPlugins();
  pending.at(-1).resolve(response([{
    name: 'example', status: 'active', version: '1',
    certified_orbit_version: certified, host_orbit_version: host,
  }]));
  await refresh;
  const chip = pluginCard().querySelector('.plugin-certification');
  assert.equal(Boolean(chip?.classList.contains('plugin-chip-warn')), behind,
    `certification ${certified} against host ${host} warns only for an older known version`);
  assert.equal(Boolean(chip), Boolean(certified), 'uncertified plugins have no certification chip');
}

console.log('audit, policy, scoreboard, and plugin panel behaviors passed');
