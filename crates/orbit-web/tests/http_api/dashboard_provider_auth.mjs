// Renders the Drain card's provider readings and authentication failures
// through the dashboard module the page loads.
import assert from 'node:assert/strict';

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
    this.id = '';
    this.title = '';
    this.own = '';
    this.style = {};
    this.disabled = false;
    this.hidden = false;
    this.href = '';
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

  append(...children) {
    for (const child of children) {
      if (child == null) continue;
      if (typeof child === 'string') { this.own += child; continue; }
      if (child.parentNode) child.parentNode.removeChild(child);
      child.parentNode = this;
      this.children.push(child);
    }
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

const body = new Node('body');
const ids = ['auto-drain-body', 'auto-drain-phase-label', 'auto-drain-start', 'auto-drain-start-body', 'auto-drain-start-sub', 'auto-drain-panel', 'auto-drain-phase', 'auto-drain-dot', 'auto-drain-operation-feedback'];
for (const id of ids) {
  const node = new Node(id === 'auto-drain-body' || id.endsWith('-body') ? 'div' : 'span');
  node.id = id;
  body.append(node);
}

const document = {
  hidden: false,
  activeElement: null,
  body,
  title: 'orbit',
  createElement: tag => new Node(tag),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => {
    const walk = node => {
      if (node.id === id) return node;
      for (const child of node.children || []) {
        const found = walk(child);
        if (found) return found;
      }
      return null;
    };
    return walk(body);
  },
  addEventListener() {},
  querySelector(selector) { return body.querySelector(selector); },
  querySelectorAll(selector) { return body.querySelectorAll(selector); },
};

const location = { search: '', hash: '', href: 'http://fixture.local/dashboard' };
globalThis.document = document;
globalThis.window = {
  location,
  document,
  confirm: () => false,
  addEventListener() {},
  dispatchEvent() {},
};
globalThis.history = { state: null, replaceState() {}, pushState() {} };
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

let readiness = null;
globalThis.fetch = async () => ({
  ok: true,
  headers: { get: () => null },
  json: async () => readiness,
  text: async () => JSON.stringify(readiness),
});

const { setWorkspace } = await import('../../assets/dashboard/js/common.js');
const { initOperations, fetchAndRenderAutoDrainPane } = await import('../../assets/dashboard/js/operations.js');

initOperations({
  getOperationsSubtab: () => 'routines',
  getWorkspaces: () => [{ id: 'ws-fixture', name: 'fixture', status: 'active' }],
});
setWorkspace('ws-fixture');

const until = '2026-10-17T18:00:00Z';
const readings = [
  {
    provider: 'codex', scope: null, window: 'seven_day', used_percent: 92, threshold: 90,
    gated: true, until, crews: ['sol'], observed_at: '2026-10-14T12:00:00Z', stale: true, age_seconds: 3 * 86400,
  },
  {
    provider: 'claude', scope: null, window: 'five_hour', used_percent: 40, threshold: 90,
    gated: false, until, crews: ['opus'], observed_at: new Date().toISOString(), stale: false, age_seconds: 90,
  },
];
const auth = [{
  host: 'dk-server-2',
  provider: 'claude',
  runs: [
    { run_id: 'jrun-auth-early', at: '2026-10-17T01:38:00Z' },
    { run_id: 'jrun-auth-late', at: '2026-10-17T01:39:00Z' },
  ],
  fix_command: 'Run `claude auth login`, or renew `claude setup-token`.',
}];

function payload(failures) {
  return {
    controls_authorized: true,
    replica: false,
    auto_concurrency: { mode: 'shadow', source: 'configured' },
    snapshot: { read_only: true, limitations: 'fixture' },
    capacity: {
      active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4,
      drain_phase: 'idle', admitted_workers: 0, running_admitted_workers: 0,
    },
    tasks: [],
    provider_limits: readings,
    provider_auth_failures: failures,
  };
}

const drain = () => document.getElementById('auto-drain-body');

readiness = payload(auth);
await fetchAndRenderAutoDrainPane();
const note = drain().querySelector('.drain-provider-limit-note');
assert(note, 'the gated note is still its own element');
assert(note.textContent.startsWith('Provider limits: codex seven_day 92%'), `gated note: ${note.textContent}`);
assert(!note.textContent.includes('claude'), `a reading below its threshold stays out of the gated note: ${note.textContent}`);
assert(!note.textContent.includes('stale') && !note.textContent.includes('today'), `the gated note does not carry age: ${note.textContent}`);

const rendered = [...drain().querySelectorAll('.drain-provider-reading')];
assert.equal(rendered.length, 2, `every reading is listed: ${rendered.map(node => node.textContent)}`);
const stale = rendered.find(node => node.textContent.includes('codex'));
assert(stale.classList.contains('stale'), 'a stale reading carries the stale class');
assert(stale.textContent.includes('92%') && stale.textContent.includes('3d old') && stale.textContent.includes('stale'), `stale age: ${stale.textContent}`);
assert(stale.title.includes('2026-10-14'), `the row names when it was observed: ${stale.title}`);
const fresh = rendered.find(node => node.textContent.includes('claude'));
assert(fresh.textContent.includes('today') && !fresh.textContent.includes('stale') && !fresh.classList.contains('stale'), `a fresh reading renders as today: ${fresh.textContent}`);

const item = drain().querySelector('.drain-provider-auth-item');
assert(item, 'an authentication failure is listed');
assert(item.textContent.includes('claude') && item.textContent.includes('dk-server-2'), `the item names provider and host: ${item.textContent}`);
const links = [...item.querySelectorAll('a')].map(link => link.textContent);
assert.deepEqual(links, ['jrun-auth-early', 'jrun-auth-late'], `the failing runs are linked: ${links}`);
const linkScopes = [...item.querySelectorAll('a')].map(link => link.href);
assert(linkScopes.every(href => href.includes('workspace=ws-fixture')), `run links keep the selected workspace scope: ${linkScopes}`);
const fix = item.querySelector('.drain-provider-auth-fix');
assert(fix && fix.textContent.includes('claude auth login'), `the fix command is shown: ${item.textContent}`);

readiness = payload([]);
await fetchAndRenderAutoDrainPane();
assert.equal(drain().querySelector('.drain-provider-auth-item'), null, 'a cleared payload removes the auth item');
assert([...drain().querySelectorAll('.drain-provider-reading')].some(node => node.textContent.includes('today')), 'fresh readings still render as today after the failure clears');
