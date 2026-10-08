// Execute the shipped Settings modules against a small DOM and fixture API:
// review health and crew table rendering, sub-view chrome, plus the System
// view's provenance, override marker, edit round-trip, and refused write.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { spawnSync } from 'node:child_process';

const classesOf = node => String(node.className || '').split(/\s+/).filter(Boolean);
class Node {
  constructor(tag) {
    this.tagName = tag.toUpperCase();
    this.children = [];
    this.parentNode = null;
    this.dataset = {};
    this.style = {};
    this.attrs = {};
    this.listeners = {};
    this.className = '';
    this.own = '';
    this.tabIndex = -1;
  }
  get classList() {
    const node = this;
    return {
      add: name => { if (!classesOf(node).includes(name)) node.className = [...classesOf(node), name].join(' '); },
      remove: name => { node.className = classesOf(node).filter(c => c !== name).join(' '); },
      contains: name => classesOf(node).includes(name),
      toggle: (name, on) => (on ?? !classesOf(node).includes(name)) ? node.classList.add(name) : node.classList.remove(name),
    };
  }
  set textContent(value) { this.children.forEach(child => { child.parentNode = null; }); this.children = []; this.own = String(value); }
  get textContent() { return this.own + this.children.map(child => child.textContent).join(''); }
  append(child) {
    if (typeof child === 'string') { this.own += child; return; }
    child.parentNode = this;
    this.children.push(child);
  }
  appendChild(child) { this.append(child); return child; }
  replaceChildren(...children) { this.children.forEach(child => { child.parentNode = null; }); this.children = []; this.own = ''; children.forEach(child => this.append(child)); }
  insertBefore(child, ref) {
    child.parentNode = this;
    const index = ref ? this.children.indexOf(ref) : -1;
    if (index < 0) this.children.push(child); else this.children.splice(index, 0, child);
    return child;
  }
  setAttribute(name, value) { this.attrs[name] = String(value); }
  getAttribute(name) { return this.attrs[name] ?? null; }
  addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); }
  dispatch(type, extra = {}) {
    const event = { type, target: this, stopPropagation() {}, preventDefault() {}, ...extra };
    for (const fn of this.listeners[type] || []) fn(event);
  }
  click() { if (!this.disabled) this.dispatch('click'); }
  focus() { document.activeElement = this; }
  contains(node) { for (let n = node; n; n = n.parentNode) if (n === this) return true; return false; }
  closest(selector) { for (let n = this; n; n = n.parentNode) if (matches(n, selector)) return n; return null; }
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
  for (const [, name] of simple.matchAll(/\.([\w-]+)/g)) if (!classesOf(node).includes(name)) return false;
  for (const [, name] of simple.matchAll(/\[data-([\w-]+)\]/g)) if (node.dataset[name] === undefined) return false;
  const id = simple.match(/#([\w-]+)/)?.[1];
  return !id || node.id === id;
}
function matchesChain(path, chain) {
  let at = path.length - 1;
  if (!matches(path[at], chain[chain.length - 1])) return false;
  for (let i = chain.length - 2; i >= 0; i--) {
    at--;
    while (at >= 0 && !matches(path[at], chain[i])) at--;
    if (at < 0) return false;
  }
  return true;
}
const all = root => root.children.flatMap(child => [child, ...all(child)]);
const named = (root, cls) => all(root).filter(node => classesOf(node).includes(cls));
const textOf = (root, cls) => named(root, cls).map(node => node.textContent);

// The shipped markup is the source of truth for which sub-views exist.
const parsed = spawnSync('python3', ['-c', `
import json, sys
from html.parser import HTMLParser
class P(HTMLParser):
    def __init__(self):
        super().__init__(); self.inside = False; self.subtabs = []
    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if a.get("id") == "config-subtabs": self.inside = True
        elif self.inside and a.get("data-subtab"): self.subtabs.append(a["data-subtab"])
    def handle_endtag(self, tag):
        if tag == "nav": self.inside = False
p = P(); p.feed(sys.stdin.read()); print(json.dumps(p.subtabs))
`], { input: fs.readFileSync(new URL('../../assets/dashboard/index.html', import.meta.url), 'utf8'), encoding: 'utf8' });
assert.equal(parsed.status, 0, parsed.stderr);
const subtabs = JSON.parse(parsed.stdout);
assert.deepEqual(subtabs.slice(subtabs.indexOf('crews'), subtabs.indexOf('crews') + 3), ['crews', 'keys', 'system'], 'System sits beside Crews and Keys');

const body = Object.assign(new Node('div'), { id: 'config-body' });
const controls = Object.assign(new Node('div'), { id: 'config-controls' });
const explainer = Object.assign(new Node('p'), { id: 'config-explainer' });
const count = Object.assign(new Node('span'), { id: 'config-count' });
const byId = {
  'config-body': body,
  'config-controls': controls,
  'config-explainer': explainer,
  'config-count': count,
};
const document = { activeElement: null, body: new Node('body'), createElement: tag => new Node(tag), getElementById: id => byId[id] || null };

// ---- fixture API ----
const KEY = 'workflow.resource_throttle.';
const defaults = { enabled: true, cpu_high_percent: 90, cpu_resume_percent: 85, memory_high_percent: 90, memory_resume_percent: 85, disk_high_percent: 90, disk_resume_percent: 85 };
const globalSet = { disk_resume_percent: 80 };
const workspaceSet = { memory_high_percent: 75 };
const GLOBAL_PATH = '/home/test/.orbit/config.toml';
const configSet = { authorized: true, reason: null };
const value = name => globalSet[name] ?? defaults[name];
const row = (name, layerValue, set, layer) => ({
  key: KEY + name, label: name, value: layerValue, value_type: name === 'enabled' ? 'bool' : 'integer',
  state: set ? 'set' : 'default', source: { layer, path: set ? GLOBAL_PATH : null }, shadowed_by: [], description: `${name} description`,
});
const globalFile = () => ({
  scope: 'global', config_set: configSet, layers: { global: { path: GLOBAL_PATH, exists: true }, workspace: { path: '/ws/.orbit/config.toml', exists: true } },
  sections: [{ token: 'delivery', title: 'Delivery', keys: [
    { key: 'workflow.base_branch', label: 'base_branch', value: 'main', value_type: 'string', state: 'default', source: { layer: 'built-in' }, shadowed_by: [] },
    ...Object.keys(defaults).map(name => row(name, value(name), name in globalSet, name in globalSet ? 'global' : 'built-in')),
  ] }],
});
const effective = () => ({
  scope: 'effective', config_set: configSet, layers: { global: { path: GLOBAL_PATH }, workspace: { path: '/ws/.orbit/config.toml' } },
  sections: [
    { token: 'machine', title: 'Machine (machine.*)', blurb: 'machine identity', key_prefix: 'machine', kind: 'keys', counts: { set: 3, default: 0, unset: 0, total: 3 }, keys: [
      { key: 'machine.id', label: 'id', value: 'hm_fixture', value_type: 'string', state: 'set', settable: false, source: { layer: 'global', path: GLOBAL_PATH }, shadowed_by: [], description: 'Generated machine identity' },
      { key: 'machine.task_prefix', label: 'task_prefix', value: 'HF', value_type: 'string', state: 'set', settable: false, source: { layer: 'global', path: GLOBAL_PATH }, shadowed_by: [], description: 'Task ID namespace' },
      { key: 'machine.name', label: 'name', value: 'http-fixture', value_type: 'string', state: 'set', settable: true, source: { layer: 'global', path: GLOBAL_PATH }, shadowed_by: [], description: 'Display name' },
    ] },
    { token: 'delivery', title: 'Delivery (workflow.*)', blurb: 'delivery settings', key_prefix: 'workflow', kind: 'keys', counts: { set: 1, unset: 0, total: 1 }, keys: [
      ...Object.keys(defaults).map(name => name in workspaceSet
        ? { ...row(name, workspaceSet[name], true, 'workspace'), source: { layer: 'workspace', path: '/ws/.orbit/config.toml' } }
        : row(name, value(name), name in globalSet, name in globalSet ? 'global' : 'built-in')),
      { ...row('low_complexity_crews', ['astra:2'], true, 'workspace'), key: 'workflow.low_complexity_crews', label: 'low_complexity_crews' },
    ] },
    { token: 'crews', title: 'Crews', blurb: 'named crews', key_prefix: 'crews', kind: 'crews', counts: { set: 0, unset: 0, total: 0 }, keys: [] },
  ],
  crews: [{ name: 'astra', provider: 'codex', model: 'gpt-6-astra', effort: 'high', tags: [], description: 'review and implementation crew', source: 'global', enabled: true, referenced_by: ['workflow.default_crew'] }],
  review: {
    healthy: false,
    before_pr: { enabled: false, line: 'off (built-in)', problems: [] },
    after_landing: {
      enabled: true,
      line: "on (auto-task delivery-code-review); unhealthy: consumer state is 'definition_changed'",
      health: {
        healthy: false,
        problems: ["consumer state is 'definition_changed' (not adopted automatically: active_execution)"],
        line: "unhealthy: consumer state is 'definition_changed' (not adopted automatically: active_execution)",
      },
    },
  },
  paths: [],
});
let hostPayload = {
  cpu: { percent: 97.2, severity: 'critical' }, memory: { percent: 40, severity: 'ok' },
  disk: { percent: 91.4, severity: 'critical', path: '/data' },
  sample_age_seconds: 1, max_age_seconds: 15, throttle: true, stale: false, reason: 'cpu high',
  thresholds: { enabled: true },
  pressures: [
    { resource: 'cpu', percent: 97.2, high_percent: 90, resume_percent: 85, since: '2026-10-04T08:41:12Z' },
    { resource: 'disk /data', percent: 91.4, high_percent: 90, resume_percent: 85, since: '2026-10-04T08:45:00Z' },
  ],
};
const requests = [];
let fileFailure = null;
const response = (payload, status = 200) => ({ ok: status < 400, status, json: async () => payload, text: async () => JSON.stringify(payload) });
const fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  requests.push({ path: url.pathname + url.search, method: options.method || 'GET', body: options.body ? JSON.parse(options.body) : null });
  if (url.pathname === '/api/config/file') return fileFailure ? response(fileFailure.body, fileFailure.status) : response(globalFile());
  if (url.pathname === '/api/config/effective') return response(effective());
  if (url.pathname === '/api/config/keys') return response({ keys: [
    { key: 'machine.id', value_type: 'string', section: 'machine', description: 'Generated machine identity', settable: false, options: [] },
    { key: 'machine.task_prefix', value_type: 'string', section: 'machine', description: 'Task ID namespace', settable: false, options: [] },
    { key: 'machine.name', value_type: 'string', section: 'machine', description: 'Display name', settable: true, options: [] },
  ] });
  if (url.pathname === '/api/host/resources') return hostPayload ? response(hostPayload) : response({ error: 'down' }, 503);
  if (options.method === 'PUT' && url.pathname.startsWith('/api/config/keys/')) {
    const name = decodeURIComponent(url.pathname.slice('/api/config/keys/'.length)).slice(KEY.length);
    const next = { ...defaults, ...globalSet, [name]: JSON.parse(options.body).value };
    for (const resource of ['cpu', 'memory', 'disk']) {
      if (next[`${resource}_resume_percent`] >= next[`${resource}_high_percent`]) {
        return response({ error: `workflow.resource_throttle.${resource}_resume_percent must be less than ${resource}_high_percent` }, 400);
      }
    }
    globalSet[name] = next[name];
    return response({ ok: true });
  }
  return response({ error: `unexpected ${path}` }, 404);
};

const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, fetch, document,
  window: { location: { search: '', hash: '' } },
});
const modules = new Map();
const load = name => {
  if (!modules.has(name)) {
    modules.set(name, new vm.SourceTextModule(fs.readFileSync(new URL(`../../assets/dashboard/js/${name}`, import.meta.url), 'utf8'), { context, identifier: name }));
  }
  return modules.get(name);
};
const config = load('config.js');
await config.link(specifier => load(specifier.replace(/^\.\//, '')));
await config.evaluate();
const { initConfig, setConfigSubtab, getConfigSubtab, fetchAndRenderConfig } = config.namespace;
const host = load('host-resources.js').namespace;
const settle = async () => { for (let i = 0; i < 6; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
const resource = name => named(body, 'config-sys-resource').find(node => node.dataset.resource === name);
const cell = key => named(body, 'config-sys-cell').find(node => node.dataset.key === KEY + key);
const edit = key => { named(cell(key), 'config-pencil')[0].click(); };

initConfig();
setConfigSubtab('system');
assert.equal(getConfigSubtab(), 'system', 'the System sub-view is routable');
await fetchAndRenderConfig();

// ---- render ----
assert.deepEqual(requests.map(r => r.path).sort(), ['/api/config/effective', '/api/config/file?scope=global', '/api/host/resources']);
assert.equal(requests.find(r => r.path === '/api/host/resources').method, 'GET');
assert.deepEqual(named(body, 'config-sys-resource').map(node => node.dataset.resource), ['cpu', 'memory', 'disk']);
assert.match(textOf(body, 'config-note').join(' '), /Edits write the global file \(\/home\/test\/\.orbit\/config\.toml\)/);
assert.match(textOf(body, 'config-warning').join(' '), /workspace file overrides/);
assert.equal(textOf(body, 'config-verdict')[0], 'held');
assert.match(textOf(body, 'config-sys-verdict-text')[0], /Holding new admissions: cpu 97% ≥ 90% since 2026-10-04 01:41 PDT; disk \/data 91% ≥ 90% since 2026-10-04 01:45 PDT/);
const cpu = resource('cpu');
assert.equal(textOf(cpu, 'config-sys-reading')[0].startsWith('97.2%'), true);
assert.match(textOf(cpu, 'config-sys-reading')[0], /critical.*held/);
assert.deepEqual(textOf(cell('cpu_high_percent'), 'config-value'), ['90']);
assert.deepEqual(textOf(cell('cpu_high_percent'), 'config-source'), ['default']);
assert.deepEqual(textOf(cell('cpu_resume_percent'), 'config-value'), ['85']);
assert.deepEqual(textOf(cell('disk_resume_percent'), 'config-value'), ['80']);
assert.deepEqual(textOf(cell('disk_resume_percent'), 'config-source'), ['global'], 'a global-file value shows the global source');
assert.deepEqual(textOf(cell('enabled'), 'config-value'), ['true']);
assert.match(textOf(resource('memory'), 'config-sys-reading')[0], /40\.0%ok/);
assert.doesNotMatch(textOf(resource('memory'), 'config-sys-reading')[0], /held/);
// A workspace-file override is marked, and the global value stays what is edited.
assert.deepEqual(textOf(cell('memory_high_percent'), 'config-source'), ['default', 'workspace 75']);
assert.ok(classesOf(cell('memory_high_percent')).includes('overridden'));
assert.match(named(cell('memory_high_percent'), 'config-source')[1].title, /that value wins for this workspace's runtimes/);
assert.equal(classesOf(cell('cpu_high_percent')).includes('overridden'), false);
assert.equal(named(body, 'config-filter').length, 0, 'there is no key filter on the throttle panel');

// ---- immutable identity rows stay read-only and section headings show a prefix once ----
setConfigSubtab('effective');
await fetchAndRenderConfig();
for (const key of ['machine.id', 'machine.task_prefix']) {
  const row = named(body, 'config-row').find(node => node.dataset.key === key);
  assert.ok(row, `${key} renders in Effective`);
  assert.equal(named(row, 'config-pencil').length, 0, `${key} has no edit button`);
  const main = named(row, 'config-row-main')[0];
  assert.equal(classesOf(main).includes('clickable'), false, `${key} is not click-to-edit`);
  main.dispatch('click');
  assert.equal(named(body, 'config-editor').length, 0, `${key} click opens no editor`);
}
const sectionTitles = textOf(body, 'config-section-title');
for (const [title, prefix] of [['Machine (machine.*)', 'machine.*'], ['Delivery (workflow.*)', 'workflow.*']]) {
  const heading = sectionTitles.find(value => value === title);
  assert.ok(heading, `${title} section heading renders`);
  assert.equal(heading.split(prefix).length - 1, 1, `${title} shows its key prefix once`);
}
assert.equal(named(body, 'config-section-prefix').length, 0, 'section headers do not add a second prefix');

setConfigSubtab('keys');
await fetchAndRenderConfig();
const listedKeys = named(body, 'config-key-row').map(node => node.textContent);
assert.equal(listedKeys.length, 1, 'Keys lists only writable registry keys');
assert.match(listedKeys[0], /machine\.name/);
assert.doesNotMatch(listedKeys.join(' '), /machine\.(id|task_prefix)/);

setConfigSubtab('system');
await fetchAndRenderConfig();

// ---- edit round-trip ----
edit('cpu_high_percent');
assert.match(textOf(body, 'config-note').join(' '), /Writes global: \/home\/test\/\.orbit\/config\.toml/);
let input = named(body, 'config-editor')[0].querySelector('input');
assert.equal(input.value, '90');
input.value = '95';
input.dispatch('input');
const press = label => named(body, 'config-action').find(node => node.textContent === label).click();
press('Save');
await settle();
const put = requests.find(r => r.method === 'PUT');
assert.equal(put.path, `/api/config/keys/${encodeURIComponent(KEY + 'cpu_high_percent')}`);
assert.deepEqual(put.body, { value: 95, scope: 'global' }, 'System edits write the global file');
assert.equal(named(body, 'config-editor').length, 0, 'a saved edit closes its editor');
assert.deepEqual(textOf(cell('cpu_high_percent'), 'config-value'), ['95'], 'the new value is read back from the server');
assert.deepEqual(textOf(cell('cpu_high_percent'), 'config-source'), ['global']);

// ---- resume >= high is refused with the inline error ----
requests.length = 0;
edit('cpu_resume_percent');
input = named(body, 'config-editor')[0].querySelector('input');
input.value = '96';
input.dispatch('input');
press('Save');
await settle();
assert.equal(requests.filter(r => r.method === 'PUT').length, 1);
assert.match(textOf(body, 'config-row-error').join(' '), /cpu_resume_percent must be less than cpu_high_percent/);
assert.equal(named(body, 'config-editor').length, 1, 'a refused write keeps the editor open');
assert.equal(named(body, 'config-editor')[0].querySelector('input').value, '96', 'the refused value stays in the draft');
assert.deepEqual(textOf(cell('cpu_resume_percent'), 'config-value'), ['85'], 'nothing is persisted');
press('Cancel');
assert.equal(named(body, 'config-editor').length, 0);

// ---- the enabled toggle ----
edit('enabled');
input = named(body, 'config-editor')[0].querySelector('input');
assert.equal(input.checked, true);
input.checked = false;
input.dispatch('change');
press('Save');
await settle();
assert.deepEqual(requests.filter(r => r.method === 'PUT').at(-1).body, { value: false, scope: 'global' });
assert.deepEqual(textOf(cell('enabled'), 'config-value'), ['false']);

// ---- live readings follow the host poll, but never under an open editor ----
const respond = payload => { hostPayload = payload; };
respond({ ...hostPayload, throttle: false, pressures: [], cpu: { percent: 20, severity: 'ok' }, reason: 'recovered' });
await host.fetchAndRenderHostResources();
assert.equal(textOf(body, 'config-verdict')[0], 'open');
assert.match(textOf(body, 'config-sys-reading')[0], /^20\.0%ok/);
edit('disk_high_percent');
respond({ ...hostPayload, cpu: { percent: 55, severity: 'ok' } });
await host.fetchAndRenderHostResources();
assert.match(textOf(body, 'config-sys-reading')[0], /^20\.0%/, 'an open editor is not rebuilt under the operator');
press('Cancel');
respond(null);
await assert.rejects(host.fetchAndRenderHostResources(), 'a failed poll is reported to its caller');
assert.equal(textOf(body, 'config-verdict')[0], 'unknown');
assert.match(textOf(body, 'config-sys-verdict-text')[0], /Verdict unknown/);

// ---- a caller without the operator capability sees keys read-only ----
setConfigSubtab('global-file');
await fetchAndRenderConfig();
let keyRows = named(body, 'config-row');
assert.ok(keyRows.length > 0, 'the global file renders key rows');
assert.ok(named(body, 'config-pencil').length > 0, 'an authorized payload renders key edit controls');
named(body, 'config-pencil')[0].click();
assert.equal(named(body, 'config-editor').length, 1, 'an authorized key row opens an editor');
configSet.authorized = false;
configSet.reason = 'config.set requires operator';
await fetchAndRenderConfig();
keyRows = named(body, 'config-row');
assert.ok(keyRows.length > 0, 'an unauthorized payload still renders its key rows');
assert.equal(named(body, 'config-pencil').length, 0, 'an unauthorized payload renders no key edit control');
assert.equal(named(body, 'config-editor').length, 0, 'a refresh that withdraws authority closes the key editor');
for (const row of keyRows) {
  const main = named(row, 'config-row-main')[0];
  assert.ok(main, 'a key row keeps its read-only cells');
  assert.equal(classesOf(main).includes('clickable'), false, 'an unauthorized key row is not an edit affordance');
  main.dispatch('click');
}
assert.equal(named(body, 'config-editor').length, 0, 'activating an unauthorized key row does not open an editor');
setConfigSubtab('system');
await fetchAndRenderConfig();
assert.equal(named(body, 'config-pencil').length, 0, 'system key cells stay read-only without operator authority');
assert.equal(named(body, 'config-editor').length, 0, 'the system view does not open an editor without operator authority');

// ---- effective review status, crew headers and sub-view chrome ----
setConfigSubtab('effective');
await fetchAndRenderConfig();
assert.equal(explainer.hidden, false, 'the shared explainer appears on Effective');
const reviewCard = named(body, 'config-review')[0];
assert.ok(reviewCard, 'Effective renders the review health card');
assert.equal(reviewCard.children[0].getAttribute('role'), 'alert', 'the unhealthy enabled switch leads with an alert');
assert.ok(classesOf(reviewCard.children[0]).includes('alert'), 'the unhealthy status has the alert treatment');
assert.match(reviewCard.children[0].textContent, /After-landing review: on · unhealthy/);
assert.match(reviewCard.children[0].textContent, /definition_changed/);
assert.match(reviewCard.children[0].textContent, /orbit doctor/);
const diagnosticDisclosure = named(reviewCard, 'config-review-details')[0];
assert.ok(diagnosticDisclosure, 'raw diagnostics have a disclosure');
assert.equal(diagnosticDisclosure.getAttribute('open'), null, 'raw diagnostics start collapsed');
assert.match(textOf(diagnosticDisclosure, 'config-review-diagnostic')[0], /not adopted automatically/);

setConfigSubtab('crews');
await fetchAndRenderConfig();
assert.equal(explainer.hidden, true, 'Crews does not repeat the generic explainer');
assert.equal(controls.hidden, true, 'Crews hides the empty controls band');
const crewHead = named(body, 'config-crew-head')[0];
assert.ok(crewHead, 'Crews has a header row');
assert.deepEqual(crewHead.children.map(cell => cell.textContent).slice(0, 7), [
  'Name', 'Provider', 'Model', 'Effort', 'Tags / fallbacks', 'Layer', 'Used by',
]);
const crewCells = named(body, 'config-crew-cells').find(node => !classesOf(node).includes('config-crew-head'));
assert.ok(crewCells, 'the crew row is present');
assert.doesNotMatch(crewCells.textContent, /\[\]/, 'empty crew arrays use the em dash placeholder');
assert.equal(crewCells.children[4].textContent, '—', 'an empty crew array uses an em dash');
assert.match(crewCells.textContent, /workflow\.default_crew/);
assert.match(crewCells.textContent, /Low complexity pool/);
assert.equal(named(body, 'config-referenced').length, 0, 'informational crew usage is not warning-colored');

setConfigSubtab('keys');
await fetchAndRenderConfig();
assert.equal(explainer.hidden, true, 'Keys does not repeat the generic explainer');
assert.equal(controls.hidden, false, 'Keys keeps its populated controls');
assert.ok(named(controls, 'config-filter').length > 0, 'Keys has a filter instead of an empty controls band');

// ---- file validation errors keep the path, key, and a corrective remedy ----
setConfigSubtab('workspace-file');
fileFailure = { status: 400, body: { error: "config file '/ws/.orbit/config.toml': workflow.low_complexity_crews: crew 'missing' is not defined in [crews.*]" } };
await assert.rejects(fetchAndRenderConfig());
const validationMessage = body.textContent;
assert.ok(validationMessage.includes('/ws/.orbit/config.toml'), 'a cold error shows the file path');
assert.ok(validationMessage.includes('workflow.low_complexity_crews'), 'a validation error names the failing key');
assert.ok(validationMessage.includes('missing'), 'a validation error names the dangling crew');
assert.doesNotMatch(validationMessage, /Use Refresh to retry/i, 'validation requires correcting the file');
assert.match(validationMessage, /correct.*configuration/i, 'the remedy asks for a configuration correction');
fileFailure = null;
await fetchAndRenderConfig();
fileFailure = { status: 503, body: { error: 'temporarily unavailable' } };
await assert.rejects(fetchAndRenderConfig());
assert.match(body.textContent, /Use Refresh to retry/i, 'a transient HTTP failure still offers a retry');
fileFailure = null;
console.log('settings views: review health alert, collapsed diagnostics, crew headers and pool usage, sub-view chrome, and system behavior passed');
