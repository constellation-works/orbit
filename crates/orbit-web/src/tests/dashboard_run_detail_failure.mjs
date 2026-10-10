// Run-detail failure box: run and task ids in the message are links, the
// deepest descendant is named, and a cancelled run says who cancelled it.
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
    this.title = '';
    this.own = '';
    this.id = '';
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
  get lastElementChild() { return [...this.children].reverse().find(child => child.tagName !== '#TEXT') || null; }
  append(child) {
    if (child == null || child === false) return;
    if (typeof child === 'string' || typeof child === 'number') {
      const text = new Node('#text');
      text.own = String(child);
      child = text;
    }
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
    if (typeof child === 'string') {
      const text = new Node('#text');
      text.own = child;
      child = text;
    }
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    const index = ref ? this.children.indexOf(ref) : -1;
    if (index < 0) this.children.push(child);
    else this.children.splice(index, 0, child);
    return child;
  }
  setAttribute(name, value) { this.attrs[name] = String(value); }
  getAttribute(name) { return Object.prototype.hasOwnProperty.call(this.attrs, name) ? this.attrs[name] : null; }
  set href(value) { this.setAttribute('href', value); }
  get href() { return this.getAttribute('href'); }
  addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); }
  dispatch(type, extra = {}) {
    const event = { type, target: this, stopPropagation() {}, preventDefault() {}, ...extra };
    for (const fn of this.listeners[type] || []) fn(event);
  }
  click() { if (!this.disabled) this.dispatch('click'); }
  focus() { document.activeElement = this; }
  contains(node) { for (let current = node; current; current = current.parentNode) if (current === this) return true; return false; }
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
  if (node.tagName === '#TEXT') return false;
  const tag = simple.match(/^[a-z]+/i)?.[0];
  if (tag && node.tagName !== tag.toUpperCase()) return false;
  for (const [, name] of simple.matchAll(/\.([^\s.#[]+)/g)) if (!classesOf(node).includes(name)) return false;
  for (const [, name] of simple.matchAll(/\[data-([\w-]+)\]/g)) if (node.dataset[name] === undefined) return false;
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

const meta = Object.assign(new Node('div'), { id: 'run-detail-meta' });
const title = Object.assign(new Node('h1'), { id: 'run-detail-title' });
const count = Object.assign(new Node('span'), { id: 'run-detail-count' });
const byId = { 'run-detail-meta': meta, 'run-detail-title': title, 'run-detail-count': count };
const document = {
  activeElement: null,
  body: new Node('body'),
  createElement: tag => new Node(tag),
  createDocumentFragment: () => new Node('#fragment'),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => byId[id] || null,
};
const location = {
  search: '?workspace=ws-orbit',
  hash: '#runs?run_id=jrun-20261010-0552-c1',
  href: 'http://127.0.0.1/?workspace=ws-orbit#runs?run_id=jrun-20261010-0552-c1',
};
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout,
  fetch: () => Promise.resolve(), document,
  window: { location },
  history: { replaceState() {} },
  Date, Math,
});
const modules = new Map();
const load = name => {
  if (!modules.has(name)) {
    modules.set(name, new vm.SourceTextModule(
      fs.readFileSync(new URL(`../../assets/dashboard/js/${name}`, import.meta.url), 'utf8'),
      { context, identifier: name },
    ));
  }
  return modules.get(name);
};
const detailModule = load('run-detail.js');
await detailModule.link(specifier => load(specifier.replace(/^\.\//, '')));
await detailModule.evaluate();
const { initRunDetail, setActiveRunDetail, renderRunDetailMeta } = detailModule.namespace;

const opened = [];
initRunDetail({
  navigateToRun: runId => { opened.push(runId); },
  fmtAbsTime: value => `FMT:${value}`,
  fmtDuration: () => '-',
  runIsCancellable: () => false,
});

const message = 'gate runs did not succeed: results[0] run jrun-20261010-0552-c2 status failed: result run jrun-20261010-0552-c3 status cancelled for ORB-15159; see ADR-12 and ORB-abc';
setActiveRunDetail({
  run: {
    run_id: 'jrun-20261010-0552-c1',
    state: 'failed',
    workspace_id: 'ws-orbit',
    error_message: message,
    failure_root: {
      run_id: 'jrun-20261010-0552-c3',
      state: 'cancelled',
      step: 'landing_review',
      message: 'stopped for ORB-15178',
    },
  },
  steps: [{ step_index: 4, target_id: 'require_gate_success', state: 'failed', error_message: message }],
});
renderRunDetailMeta();

const failure = meta.querySelector('.run-failure');
assert.ok(failure, 'failed run renders a failure box');
assert.equal(failure.getAttribute('aria-label'), 'Why this run failed');
const pre = failure.querySelector('.run-failure-message');
assert.equal(pre.textContent, message);
const links = failure.querySelectorAll('.failure-id-link');
const described = links.map(node => ({
  tag: node.tagName,
  text: node.textContent,
  type: node.getAttribute('type'),
  href: node.getAttribute('href'),
}));
assert.deepEqual(described.map(link => link.text), [
  'jrun-20261010-0552-c2',
  'jrun-20261010-0552-c3',
  'ORB-15159',
  'jrun-20261010-0552-c3',
  'ORB-15178',
]);
for (const runLink of described.filter(link => link.text.startsWith('jrun-'))) {
  assert.equal(runLink.tag, 'BUTTON');
  assert.equal(runLink.type, 'button');
}
for (const taskLink of described.filter(link => link.text.startsWith('ORB-'))) {
  assert.equal(taskLink.tag, 'A');
  const url = new URL(taskLink.href, location.href);
  const [route, query] = url.hash.slice(1).split('?');
  const params = new URLSearchParams(query);
  assert.equal(url.searchParams.get('workspace'), 'ws-orbit');
  assert.equal(route, 'tasks');
  assert.equal(params.get('open'), taskLink.text);
  assert.equal(params.has('status') || params.has('q'), false, 'a task link names the task, never a filter');
}
assert.equal(links.some(node => node.textContent === 'ADR-12' || node.textContent === 'ORB-abc'), false);
assert.match(pre.textContent, /ADR-12/);
assert.match(pre.textContent, /ORB-abc/);
const root = failure.querySelector('.run-failure-root');
assert.equal(root.textContent, 'Root cause: jrun-20261010-0552-c3 cancelled at landing_review - stopped for ORB-15178');
links.find(node => node.textContent === 'jrun-20261010-0552-c2').click();
links.find(node => node.textContent === 'ORB-15178').click();
assert.deepEqual(opened, ['jrun-20261010-0552-c2']);

setActiveRunDetail({
  run: {
    run_id: 'jrun-20261010-0552-c3',
    state: 'cancelled',
    workspace_id: 'ws-orbit',
    cancellation: {
      actor: 'dashboard',
      source: 'web',
      reason: 'operator stopped the drain after jrun-20261010-0552-c4',
      at: '2026-10-10T05:52:00Z',
    },
  },
  steps: [],
});
renderRunDetailMeta();
const failureBoxes = meta.querySelectorAll('.run-failure');
assert.equal(failureBoxes.length, 1);
assert.ok(failureBoxes[0].classList.contains('run-cancelled'));
const cancelled = failureBoxes[0];
assert.equal(cancelled.getAttribute('aria-label'), 'Why this run was cancelled');
assert.match(cancelled.querySelector('.run-failure-head').textContent, /^Cancelled by dashboard at FMT:2026-10-10T05:52:00Z$/);
const when = cancelled.querySelector('.run-cancelled-when');
assert.ok(when.textContent.trim(), 'cancelled box names a time');
const reason = cancelled.querySelector('.run-cancelled-reason');
assert.equal(reason.textContent, 'operator stopped the drain after jrun-20261010-0552-c4');
const reasonRun = reason.querySelector('.failure-id-link');
assert.equal(reasonRun.tagName, 'BUTTON');
assert.equal(reasonRun.textContent, 'jrun-20261010-0552-c4');
reasonRun.click();
assert.deepEqual(opened, ['jrun-20261010-0552-c2', 'jrun-20261010-0552-c4']);

setActiveRunDetail({
  run: { run_id: 'jrun-bare-cancel', state: 'cancelled' },
  steps: [],
});
renderRunDetailMeta();
const unnamed = meta.querySelector('.run-cancelled');
assert.equal(unnamed.querySelector('.run-failure-head').textContent, 'Cancelled');
assert.equal(unnamed.querySelector('.run-cancelled-when'), null);
assert.equal(unnamed.querySelector('.run-cancelled-reason').textContent, 'no reason recorded');
assert.equal(unnamed.querySelector('.run-failure-root'), null);
