// Execute the shipped Settings modules against a small DOM and fixture API:
// the System sub-view's render, provenance, workspace-override marker, edit
// round-trip and refused write.
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
assert.deepEqual(subtabs.slice(-3), ['crews', 'keys', 'system'], 'System sits beside Crews and Keys');

const body = Object.assign(new Node('div'), { id: 'config-body' });
const controls = Object.assign(new Node('div'), { id: 'config-controls' });
const count = Object.assign(new Node('span'), { id: 'config-count' });
const byId = { 'config-body': body, 'config-controls': controls, 'config-count': count };
const document = { activeElement: null, body: new Node('body'), createElement: tag => new Node(tag), getElementById: id => byId[id] || null };

// ---- fixture API ----
const KEY = 'workflow.resource_throttle.';
const defaults = { enabled: true, cpu_high_percent: 90, cpu_resume_percent: 85, memory_high_percent: 90, memory_resume_percent: 85, disk_high_percent: 90, disk_resume_percent: 85 };
const globalSet = { disk_resume_percent: 80 };
const workspaceSet = { memory_high_percent: 75 };
const GLOBAL_PATH = '/home/test/.orbit/config.toml';
const value = name => globalSet[name] ?? defaults[name];
const row = (name, layerValue, set, layer) => ({
  key: KEY + name, label: name, value: layerValue, value_type: name === 'enabled' ? 'bool' : 'integer',
  state: set ? 'set' : 'default', source: { layer, path: set ? GLOBAL_PATH : null }, shadowed_by: [], description: `${name} description`,
});
const globalFile = () => ({
  scope: 'global', layers: { global: { path: GLOBAL_PATH, exists: true }, workspace: { path: '/ws/.orbit/config.toml', exists: true } },
  sections: [{ token: 'delivery', title: 'Delivery', keys: [
    { key: 'workflow.base_branch', label: 'base_branch', value: 'main', value_type: 'string', state: 'default', source: { layer: 'built-in' }, shadowed_by: [] },
    ...Object.keys(defaults).map(name => row(name, value(name), name in globalSet, name in globalSet ? 'global' : 'built-in')),
  ] }],
});
const effective = () => ({
  scope: 'effective', layers: { global: { path: GLOBAL_PATH }, workspace: { path: '/ws/.orbit/config.toml' } },
  sections: [{ token: 'delivery', keys: Object.keys(defaults).map(name => name in workspaceSet
    ? { ...row(name, workspaceSet[name], true, 'workspace'), source: { layer: 'workspace', path: '/ws/.orbit/config.toml' } }
    : row(name, value(name), name in globalSet, name in globalSet ? 'global' : 'built-in')) }],
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
const response = (payload, status = 200) => ({ ok: status < 400, status, json: async () => payload, text: async () => JSON.stringify(payload) });
const fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  requests.push({ path: url.pathname + url.search, method: options.method || 'GET', body: options.body ? JSON.parse(options.body) : null });
  if (url.pathname === '/api/config/file') return response(globalFile());
  if (url.pathname === '/api/config/effective') return response(effective());
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
assert.match(textOf(body, 'config-sys-verdict-text')[0], /Holding new admissions: cpu 97% ≥ 90% since 2026-10-04 08:41Z; disk \/data 91% ≥ 90% since 2026-10-04 08:45Z/);
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
console.log('settings system tab: render, provenance, workspace override, edit round-trip, refused write and live readings passed');
