// Execute the shipped Settings › Hosts module against a small DOM and fixture
// API [ORB-14451]: the local host first and labelled as the edited one, live
// reachability, version, protocol, skew and roles, an unreachable host's error,
// cached refreshes that keep live readings and show CLI additions, the load
// error banner, inline add, rename, remove and force-remove with keyboard
// focus handed back as user-interface §6 requires, and a read-only view for a
// session without the operator capability. When a Playwright module is
// installed, it also opens the shipped stylesheet and measures that the
// header, the local row and a remote row share column edges.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import vm from 'node:vm';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from './dashboard_static.mjs';

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
assert.ok(JSON.parse(parsed.stdout).includes('hosts'), 'Settings offers a Hosts sub-view');

const body = Object.assign(new Node('div'), { id: 'config-body' });
const controls = Object.assign(new Node('div'), { id: 'config-controls' });
const explainer = Object.assign(new Node('p'), { id: 'config-explainer' });
const count = Object.assign(new Node('span'), { id: 'config-count' });
const byId = { 'config-body': body, 'config-controls': controls, 'config-explainer': explainer, 'config-count': count };
const document = { activeElement: null, body: new Node('body'), createElement: tag => new Node(tag), getElementById: id => byId[id] || null };

// ---- fixture API: `orbit host list --json` rows ----
const LOCAL = { name: 'box-a', machine_id: 'hm_local', ssh: null, task_prefix: 'LB', local: true, legacy: false, reachable: true, error: null,
  binary_version: '1.4.0', protocol_fingerprint: 'abcdef0123456789', skew: false, skew_fields: [],
  workspaces: [{ id: 'ws_1', name: 'orbit', role: 'replica', owner_machine_id: 'hm_alpha', status: 'active' }] };
const remote = (name, id, prefix, extra = {}) => ({ name, machine_id: id, ssh: name, task_prefix: prefix, local: false, legacy: false,
  reachable: true, error: null, binary_version: '1.4.0', protocol_fingerprint: 'abcdef0123456789', skew: false, skew_fields: [],
  workspaces: [{ id: 'ws_1', name: 'orbit', role: 'owner', owner_machine_id: id, status: 'active' }], ...extra });
let entries = [
  remote('alpha', 'hm_alpha', 'AL'),
  remote('beta', 'hm_beta', 'BE', { reachable: false, error: { code: 'unreachable_destination', message: 'ssh: Could not resolve hostname beta' }, binary_version: null, protocol_fingerprint: null, workspaces: [] }),
  remote('gamma', 'hm_gamma', 'GA', { binary_version: '1.3.9', skew: true, skew_fields: ['binary_version'] }),
];
const cached = row => ({ ...row, reachable: null, error: null, binary_version: null, protocol_fingerprint: null, skew: false, skew_fields: [], workspaces: [] });
let loadError = null;
let hostEdit = { authorized: true, reason: null };
const requests = [];
const holds = [];
const response = (payload, status = 200) => ({ ok: status < 400, status, json: async () => payload, text: async () => JSON.stringify(payload) });
const refuse = (status, code, error, extra = {}) => response({ error, code, ...extra }, status);
const route = (url, options) => {
  const method = options.method || 'GET';
  const body = options.body ? JSON.parse(options.body) : null;
  if (url.pathname === '/api/hosts' && method === 'GET') {
    const probe = url.searchParams.get('probe') !== 'false';
    return response({ host_file: '/home/op/.orbit/hosts.toml', legacy: false, generation: 3, load_error: loadError, host_edit: hostEdit,
      hosts: [LOCAL, ...entries.map(row => probe ? row : cached(row))] });
  }
  if (url.pathname === '/api/hosts' && method === 'POST') {
    if (body.ssh === 'myself') return refuse(409, 'host_is_local', "'myself' answered with this machine's own machine_id hm_local");
    const added = remote(body.name || body.ssh, `hm_${body.ssh}`, 'EP');
    entries = [...entries, added];
    return response({ action: 'added', entry: { name: added.name, machine_id: added.machine_id, ssh: added.ssh, task_prefix: 'EP' }, previous_name: null, migrated: [], host: added, orphaned: null }, 201);
  }
  const id = decodeURIComponent(url.pathname.split('/').pop());
  const entry = entries.find(row => row.machine_id === id);
  if (method === 'PATCH') {
    const previous = entry.name;
    entry.name = body.name;
    return response({ action: 'renamed', entry: { name: entry.name, machine_id: id, ssh: entry.ssh, task_prefix: entry.task_prefix }, previous_name: previous, migrated: [], host: null, orphaned: null });
  }
  if (method === 'DELETE') {
    const dependents = { replica_checkouts: [{ workspace_id: 'ws_1', workspace_name: 'orbit', repo_root: '/srv/orbit' }], pull_drains: [{ workspace_id: 'ws_1', run_id: 'jrun-1', state: 'running' }] };
    if (url.searchParams.get('force') !== 'true') {
      return refuse(409, 'host_in_use', `${id} is still the owner route for: replica checkout orbit (ws_1) at /srv/orbit. Remove or re-home those first, or pass --force to remove the entry anyway`, { dependents });
    }
    entries = entries.filter(row => row.machine_id !== id);
    return response({ action: 'removed', entry: { name: entry.name, machine_id: id, ssh: entry.ssh, task_prefix: entry.task_prefix }, previous_name: null, migrated: [], host: null, orphaned: dependents });
  }
  return response({ error: `unexpected ${method} ${url.pathname}` }, 404);
};
const fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  requests.push({ path: url.pathname + url.search, method: options.method || 'GET', body: options.body ? JSON.parse(options.body) : null });
  if (holds.length) await holds.shift();
  return route(url, options);
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
const settle = async () => { for (let i = 0; i < 8; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
const rows = () => named(body, 'host-row');
const row = id => rows().find(node => node.dataset.key === id);
const button = (root, label) => named(root, 'config-action').find(node => node.textContent === label);
const editor = () => named(body, 'host-editor')[0];
const focused = cls => classesOf(document.activeElement || new Node('x')).includes(cls);
const type = (cls, text) => { const input = named(editor(), cls)[0]; input.value = text; input.dispatch('input'); };
const escape = () => editor().dispatch('keydown', { key: 'Escape' });
const submit = () => editor().dispatch('submit');
const lastRequest = () => requests.at(-1);

initConfig();
setConfigSubtab('hosts');
assert.equal(getConfigSubtab(), 'hosts', 'the Hosts sub-view is routable');
await fetchAndRenderConfig();

// ---- render: local host first, labelled as the host this dashboard edits ----
assert.deepEqual(requests.map(r => r.path), ['/api/hosts'], 'opening the view probes every host');
assert.equal(explainer.hidden, true, 'Hosts does not repeat the config explainer');
assert.deepEqual(rows().map(node => node.dataset.key), ['hm_local', 'hm_alpha', 'hm_beta', 'hm_gamma']);
assert.match(textOf(body, 'host-scope')[0], /edits the host file of box-a: \/home\/op\/\.orbit\/hosts\.toml/);
assert.match(row('hm_local').textContent, /local · edited here/);
assert.equal(named(row('hm_local'), 'host-rename').length, 0, 'the local host is renamed through machine.name, not here');
assert.match(textOf(row('hm_local'), 'host-workspace')[0], /orbit \(replica of hm_alpha\)/);
assert.equal(textOf(row('hm_alpha'), 'host-reach')[0], 'yes');
assert.equal(textOf(row('hm_alpha'), 'host-version')[0], '1.4.0');
assert.equal(textOf(row('hm_alpha'), 'host-protocol')[0], 'abcdef012345');
assert.equal(textOf(row('hm_alpha'), 'host-skew')[0], 'none');
assert.match(textOf(row('hm_alpha'), 'host-workspace')[0], /orbit \(owner\)/);
assert.match(textOf(row('hm_alpha'), 'host-facts')[0], /hm_alpha · ssh alpha · prefix AL/);
assert.equal(textOf(row('hm_beta'), 'host-reach')[0], 'no · unreachable_destination', 'an unreachable host stays listed');
assert.match(textOf(row('hm_beta'), 'host-error')[0], /unreachable_destination: ssh: Could not resolve hostname beta/);
assert.equal(textOf(row('hm_gamma'), 'host-skew')[0], 'skew: binary_version');
assert.equal(count.textContent, '3 remote hosts');

// ---- a background refresh reads cached fields and keeps live readings ----
entries = [...entries, remote('delta', 'hm_delta', 'DE')];
await fetchAndRenderConfig();
assert.equal(lastRequest().path, '/api/hosts?probe=false', 'the poll opens no SSH session');
assert.equal(textOf(row('hm_alpha'), 'host-reach')[0], 'yes', 'a cached row keeps its last live reading');
assert.equal(textOf(row('hm_beta'), 'host-reach')[0], 'no · unreachable_destination');
assert.equal(textOf(row('hm_delta'), 'host-reach')[0], 'not probed', 'a host added from the CLI appears on the next refresh');

// ---- a host file that fails to load is a banner over the last snapshot ----
loadError = { code: 'task_prefix_conflict', message: "invalid host file: prefix 'AL' is used twice" };
await fetchAndRenderConfig();
const banner = named(body, 'host-load-error')[0];
assert.ok(banner, 'the load error is shown');
assert.equal(banner.getAttribute('role'), 'alert');
assert.match(banner.textContent, /task_prefix_conflict.*prefix 'AL' is used twice.*last valid host file/);
assert.equal(rows().length, 5, 'the last valid snapshot stays listed');
loadError = null;
await fetchAndRenderConfig();
assert.equal(named(body, 'host-load-error').length, 0);

// ---- add: inline form, typed refusal, success, Escape ----
button(body, 'Add host').click();
assert.ok(focused('host-input-ssh'), 'the add form opens with focus in its first field');
type('host-input-ssh', 'myself');
submit();
await settle();
assert.deepEqual(lastRequest(), { path: '/api/hosts', method: 'POST', body: { ssh: 'myself' } });
assert.match(textOf(editor(), 'config-row-error')[0], /this machine's own machine_id/, 'a refusal is the server\'s own text');
assert.ok(focused('host-input-ssh'), 'a refused add keeps focus in the form');
assert.equal(named(editor(), 'host-input-ssh')[0].value, 'myself', 'the refused draft is kept');
type('host-input-ssh', 'epsilon');
type('host-input-name', 'build-box');
submit();
await settle();
assert.deepEqual(requests.at(-2), { path: '/api/hosts', method: 'POST', body: { ssh: 'epsilon', name: 'build-box' } });
assert.equal(lastRequest().path, '/api/hosts?probe=false');
assert.equal(editor(), undefined, 'a successful add closes the form');
assert.match(textOf(body, 'host-notice')[0], /Added build-box \(hm_epsilon, prefix EP\)/);
assert.equal(textOf(row('hm_epsilon'), 'host-reach')[0], 'yes', 'the added host keeps the live summary add returned');
assert.ok(focused('host-add-open'), 'focus returns to Add host');
button(body, 'Add host').click();
escape();
assert.equal(editor(), undefined, 'Escape closes the add form');
assert.ok(focused('host-add-open'));

// ---- rename: inline editor, Escape, save ----
named(row('hm_alpha'), 'host-rename')[0].click();
assert.ok(focused('host-input-name'));
assert.equal(named(editor(), 'host-input-name')[0].value, 'alpha');
escape();
assert.equal(editor(), undefined);
assert.ok(focused('host-rename') && row('hm_alpha').contains(document.activeElement), 'Escape returns focus to Rename');
named(row('hm_alpha'), 'host-rename')[0].click();
type('host-input-name', 'alpha-2');
let release;
holds.push(new Promise(resolve => { release = resolve; }));
submit();
await settle();
assert.match(button(editor(), 'Saving…')?.textContent || '', /Saving…/, 'the save is pending');
escape();
assert.ok(editor(), 'Escape does not close an editor while its request is in flight');
release();
await settle();
assert.deepEqual(requests.find(r => r.method === 'PATCH'), { path: '/api/hosts/hm_alpha', method: 'PATCH', body: { name: 'alpha-2' } });
assert.match(textOf(body, 'host-notice')[0], /Renamed alpha to alpha-2/);
assert.match(named(row('hm_alpha'), 'host-name-text')[0].textContent, /alpha-2/);
assert.ok(focused('host-rename') && row('hm_alpha').contains(document.activeElement), 'focus returns to Rename after a save');

// ---- remove: confirm, host_in_use lists dependents, explicit force ----
named(row('hm_beta'), 'host-remove')[0].click();
assert.ok(focused('host-confirm'), 'the inline confirmation takes focus');
assert.match(editor().textContent, /Remove beta \(hm_beta\) from the host file\?/);
button(editor(), 'Remove').click();
await settle();
assert.equal(lastRequest().path, '/api/hosts/hm_beta');
assert.equal(lastRequest().method, 'DELETE');
assert.ok(row('hm_beta'), 'a refused remove keeps the row');
assert.match(editor().textContent, /still the owner route/);
assert.deepEqual(textOf(editor(), 'mono').filter(line => /replica|pull drain/.test(line)), [
  'replica checkout orbit (ws_1) at /srv/orbit', 'running pull drain jrun-1 in ws_1',
], 'host_in_use lists its dependents');
assert.ok(focused('host-confirm') && document.activeElement.textContent === 'Force remove', 'the force confirmation takes focus');
button(editor(), 'Force remove').click();
await settle();
assert.equal(requests.at(-2).path, '/api/hosts/hm_beta?force=true');
assert.equal(row('hm_beta'), undefined, 'a forced remove drops the row');
assert.match(textOf(body, 'host-notice')[0], /Removed beta \(hm_beta\)\. These lost their owner route: replica checkout orbit/);
assert.ok(focused('host-add-open'), 'focus lands on Add host when the row is gone');

// ---- Reload probes again ----
button(controls, 'Reload').click();
await settle();
assert.equal(lastRequest().path, '/api/hosts', 'Reload probes every host');

// ---- without the operator capability the view is read-only ----
hostEdit = { authorized: false, reason: "operation 'host.edit' requires operator capability" };
await fetchAndRenderConfig();
assert.equal(named(body, 'host-add-open').length, 0, 'no Add host without the operator capability');
assert.equal(named(body, 'host-rename').length + named(body, 'host-remove').length, 0, 'no row edits without it');
assert.match(textOf(body, 'host-read-only')[0], /Read-only: operation 'host.edit' requires operator capability/);
assert.equal(rows().length, 5, 'the hosts stay listed');
console.log('settings hosts: rows, freshness, load banner, inline add/rename/remove/force with focus, and read-only passed');

// Column edges are a layout fact. The fake document above has no boxes, so
// this drives the shipped hosts module and stylesheet in Chromium.
await assertHostColumnsAlign();

function browserModule() {
  const fromEnv = process.env.PLAYWRIGHT_MODULE || process.env.ORBIT_PLAYWRIGHT_MODULE;
  if (fromEnv && fs.existsSync(fromEnv)) return fromEnv;
  const kit = path.join(os.homedir(), '.local/chromium-deps');
  const candidate = path.join(kit, 'kit/node_modules/playwright/index.mjs');
  if (!fs.existsSync(candidate)) return null;
  const lib = path.join(kit, 'root/usr/lib/x86_64-linux-gnu');
  if (fs.existsSync(lib) && !(process.env.LD_LIBRARY_PATH || '').split(':').includes(lib)) {
    process.env.LD_LIBRARY_PATH = [lib, process.env.LD_LIBRARY_PATH].filter(Boolean).join(':');
  }
  const fonts = path.join(kit, 'fonts.conf');
  if (!process.env.FONTCONFIG_FILE && fs.existsSync(fonts)) process.env.FONTCONFIG_FILE = fonts;
  if (!process.env.PLAYWRIGHT_BROWSERS_PATH && fs.existsSync(path.join(kit, 'browsers'))) {
    process.env.PLAYWRIGHT_BROWSERS_PATH = path.join(kit, 'browsers');
  }
  return candidate;
}

function hostPayload() {
  const local = {
    name: 'box-a', machine_id: 'hm_local', ssh: null, task_prefix: 'LB', local: true, legacy: false,
    reachable: true, error: null, binary_version: '1.4.0', protocol_fingerprint: 'abcdef0123456789',
    skew: false, skew_fields: [],
    workspaces: [{ id: 'ws_1', name: 'orbit', role: 'replica', owner_machine_id: 'hm_alpha', status: 'active' }],
  };
  const remote = {
    name: 'dk-server-2', machine_id: 'hm_remote', ssh: 'dk-server-2', task_prefix: 'DK', local: false, legacy: false,
    reachable: true, error: null, binary_version: '1.4.0', protocol_fingerprint: 'abcdef0123456789',
    skew: false, skew_fields: [],
    workspaces: [{ id: 'ws_2', name: 'orbit', role: 'owner', owner_machine_id: 'hm_remote', status: 'active' }],
  };
  const down = {
    ...remote, name: 'beta', machine_id: 'hm_beta', ssh: 'beta', task_prefix: 'BE', reachable: false,
    error: { code: 'unreachable_destination', message: 'ssh: Could not resolve hostname beta' },
    binary_version: null, protocol_fingerprint: null, skew_fields: [], workspaces: [],
  };
  return {
    host_file: '/home/op/.orbit/hosts.toml', legacy: false, generation: 1, load_error: null,
    host_edit: { authorized: true, reason: null },
    hosts: [local, remote, down],
  };
}

function columnBoxes() {
  const boxes = (grid) => [...grid.children].map((cell) => {
    const box = cell.getBoundingClientRect();
    return { left: box.left, width: box.width, right: box.right };
  });
  const rowGrid = (id) => document.querySelector(`.host-row[data-key="${id}"] .host-grid`);
  const header = document.querySelector('.host-head');
  const local = rowGrid('hm_local');
  const remote = rowGrid('hm_remote');
  const down = rowGrid('hm_beta');
  const error = document.querySelector('.host-row[data-key="hm_beta"] .host-error');
  const rename = document.querySelector('.host-row[data-key="hm_remote"] .host-rename');
  const actions = document.querySelector('.host-row[data-key="hm_remote"] .host-actions');
  return {
    header: boxes(header),
    local: boxes(local),
    remote: boxes(remote),
    down: boxes(down),
    localButtons: document.querySelectorAll('.host-row[data-key="hm_local"] .config-action').length,
    remoteButtons: document.querySelectorAll('.host-row[data-key="hm_remote"] .config-action').length,
    renameInsideActions: actions.getBoundingClientRect().right + 1 >= rename.getBoundingClientRect().right,
    errorBelowGrid: error.getBoundingClientRect().top >= down.getBoundingClientRect().bottom - 1,
    errorText: error.textContent,
  };
}

function narrowLayout() {
  const row = document.querySelector('.host-row[data-key="hm_beta"]');
  const grid = row.querySelector('.host-grid');
  const identity = row.querySelector('.host-identity');
  const reach = row.querySelector('.host-reach');
  const version = row.querySelector('.host-version');
  const actions = row.querySelector('.host-actions');
  const error = row.querySelector('.host-error');
  const list = document.querySelector('.host-list');
  const box = (node) => {
    const rect = node.getBoundingClientRect();
    return { top: rect.top, left: rect.left, right: rect.right, bottom: rect.bottom, width: rect.width, height: rect.height };
  };
  return {
    headerDisplay: getComputedStyle(document.querySelector('.host-head')).display,
    grid: box(grid),
    identity: box(identity),
    reach: box(reach),
    version: box(version),
    actions: box(actions),
    error: box(error),
    errorText: error.textContent,
    listOverflow: list.scrollWidth - list.clientWidth,
    remoteButtons: document.querySelectorAll('.host-row[data-key="hm_remote"] .host-rename, .host-row[data-key="hm_remote"] .host-remove').length,
    rows: document.querySelectorAll('.host-row').length,
  };
}

function sameTrack(name, actual, expected) {
  assert.equal(actual.length, expected.length, `${name} has a different column count`);
  for (let i = 0; i < expected.length; i++) {
    assert.ok(Math.abs(actual[i].left - expected[i].left) <= 0.5, `${name} column ${i} left ${actual[i].left} != ${expected[i].left}`);
    assert.ok(Math.abs(actual[i].width - expected[i].width) <= 0.5, `${name} column ${i} width ${actual[i].width} != ${expected[i].width}`);
  }
}

async function assertHostColumnsAlign() {
  const modulePath = browserModule();
  if (!modulePath) {
    if (process.env.ORBIT_REQUIRE_HOSTS_LAYOUT === '1') {
      throw new Error('hosts column alignment requires Playwright (PLAYWRIGHT_MODULE or the prepared-host kit)');
    }
    console.log('settings hosts: column alignment not measured (no Playwright module)');
    return;
  }
  const { chromium } = await import(pathToFileURL(path.resolve(modulePath)).href);
  const payload = hostPayload();
  const html = `<!doctype html><html><head><link rel="stylesheet" href="/static/dashboard.css"></head><body>
    <p id="config-explainer" hidden></p><div id="config-controls" hidden></div><span id="config-count"></span><div id="config-body"></div>
  </body></html>`;
  const server = http.createServer((req, res) => {
    const name = new URL(req.url, 'http://fixture').pathname;
    if (name === '/hosts-layout') {
      res.setHeader('content-type', 'text/html; charset=utf-8');
      res.end(html);
      return;
    }
    const served = dashboardFile(name);
    if (!served) { res.writeHead(404); res.end(); return; }
    res.setHeader('content-type', served.type);
    res.end(served.data);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  let browser;
  try {
    browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const pageErrors = [];
    page.on('pageerror', (error) => pageErrors.push(String(error)));
    await page.goto(`http://127.0.0.1:${port}/hosts-layout`);
    await page.evaluate(async (body) => {
      window.fetch = async (input) => {
        const url = new URL(input, location.origin);
        if (url.pathname === '/api/hosts') {
          return new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });
        }
        return new Response('not found', { status: 404 });
      };
      const mod = await import('/js/config.js');
      mod.initConfig();
      mod.setConfigSubtab('hosts');
      await mod.fetchAndRenderConfig();
    }, payload);
    await page.waitForSelector('.host-row[data-key="hm_remote"] .host-rename');
    assert.deepEqual(pageErrors, [], `hosts page errors: ${pageErrors.join('\n')}`);
    const wide = await page.evaluate(columnBoxes);
    sameTrack('local row', wide.local, wide.header);
    sameTrack('remote row', wide.remote, wide.header);
    sameTrack('error row', wide.down, wide.header);
    for (let i = 1; i < wide.header.length; i++) {
      assert.ok(wide.header[i].left > wide.header[i - 1].left + 40, `column ${i} collapsed into column ${i - 1}`);
    }
    assert.equal(wide.localButtons, 0, 'the local row has no actions');
    assert.equal(wide.remoteButtons, 2, 'the remote row has Rename and Remove');
    assert.equal(wide.renameInsideActions, true, 'Rename fits in the shared actions track');
    assert.equal(wide.errorBelowGrid, true, 'the host-error line sits under the row, not in a column');
    assert.match(wide.errorText, /unreachable_destination: ssh: Could not resolve hostname beta/);

    await page.setViewportSize({ width: 720, height: 900 });
    const narrow = await page.evaluate(narrowLayout);
    assert.equal(narrow.headerDisplay, 'none', 'the stacked layout hides the column header');
    assert.equal(narrow.rows, 3, 'stacked layout keeps every host row');
    assert.equal(narrow.remoteButtons, 2, 'Rename and Remove stay available when stacked');
    assert.ok(narrow.identity.width > narrow.grid.width * 0.8, 'identity spans the stacked row');
    assert.ok(narrow.grid.right - narrow.identity.right < 20, 'identity reaches the row edge');
    assert.ok(Math.abs(narrow.reach.top - narrow.version.top) <= 1, 'Reachable and Version share a stacked row');
    assert.ok(narrow.version.left >= narrow.reach.right - 1, 'Reachable and Version sit side by side');
    assert.ok(narrow.actions.top >= narrow.identity.bottom - 1, 'actions drop below the identity line');
    assert.ok(narrow.actions.width > narrow.grid.width * 0.8, 'actions span the stacked row');
    assert.ok(narrow.error.top >= narrow.grid.bottom - 1, 'the host-error line stays under the stacked cells');
    assert.ok(narrow.error.height > 0 && narrow.error.width > 0, 'the host-error line is visible');
    assert.match(narrow.errorText, /unreachable_destination/);
    assert.ok(narrow.listOverflow <= 1, `host list overflows by ${narrow.listOverflow}px at 720px`);
    console.log('settings hosts: column edges align across header, local, and remote rows; error line and 720px stack hold');
  } finally {
    if (browser) await browser.close();
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
}
