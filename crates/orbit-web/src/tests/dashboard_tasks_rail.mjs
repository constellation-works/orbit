// ORB-14701: the Tasks rail count reports the matching total the panel header
// reports, not the rows on the page in front of the operator. Runs the shipped
// tasks.js renderer against a DOM stub, so what is asserted is what it paints.
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
  replaceChildren(...children) {
    this.children.forEach(child => { child.parentNode = null; });
    this.children = [];
    this.own = '';
    children.forEach(child => this.append(child));
  }
  append(child) {
    if (child == null) return;
    if (typeof child === 'string') { this.own += child; return; }
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.children.push(child);
  }
  appendChild(child) { this.append(child); return child; }
  dispatch(type, extra = {}) {
    const event = { type, target: this, stopPropagation() {}, preventDefault() {}, ...extra };
    for (const fn of this.listeners[type] || []) fn(event);
  }
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

const ids = ['tasks-body', 'tasks-count', 'rail-count-tasks', 'tasks-previous', 'tasks-next', 'tasks-page-status'];
const byId = Object.fromEntries(ids.map(id => [id, Object.assign(new Node('div'), { id })]));
const document = {
  activeElement: null,
  body: new Node('body'),
  createElement: tag => new Node(tag),
  createDocumentFragment: () => new Node('#fragment'),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => byId[id] || null,
};
const window = { location: { search: '', hash: '' }, confirm: () => true };
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, document, window, Node,
  fetch: () => new Promise(() => {}),
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
const linker = async specifier => {
  const module = load(specifier.replace(/^\.\//, ''));
  if (module.status === 'unlinked') await module.link(linker);
  return module;
};
const tasksModule = load('tasks.js');
await tasksModule.link(linker);
await tasksModule.evaluate();
const { renderTasks } = tasksModule.namespace;
const common = modules.get('common.js').namespace;

// A matching set of 65 tasks read in pages of 50, as /api/tasks serves them.
const task = (index, extra = {}) => ({ id: `ORB-${index}`, title: `Task ${index}`, status: 'in-progress', priority: 'medium', ...extra });
const matching = Array.from({ length: 65 }, (_, index) => index + 1);
const page = (offset, size, total, extra) => ({
  total,
  limit: 50,
  offset,
  next_cursor: offset + size < total ? `cursor-${offset + size}` : null,
  items: matching.slice(offset, offset + size).map(index => task(index, extra)),
});

let served = null;
let currentTasks = [];
const taskContext = {
  getActiveStatuses: () => new Set(['in-progress']),
  statusOrder: ['in-progress'],
  getSearchQuery: () => '',
  getTaskPagination: () => ({ canPrevious: Boolean(served && served.offset > 0), canNext: Boolean(served && served.next_cursor), loading: false }),
  getTasksMeta: () => served,
  getTasks: () => currentTasks,
};
const rail = byId['rail-count-tasks'];
const header = byId['tasks-count'];
const paint = (payload, items = payload.items) => {
  served = payload;
  currentTasks = items;
  renderTasks(items, taskContext);
};

// Single workspace: the rail and the header agree on the matching total,
// on page 1 and on page 2.
common.setMultiWorkspace(true);
common.setWorkspace('ws_orbit');
paint(page(0, 50, 65));
assert.equal(rail.textContent, '65', 'rail counts the matching total, not the 50 rows on page 1');
assert.ok(header.textContent.endsWith(' of 65'), `header reports the matching total: ${header.textContent}`);
assert.ok(rail.title.length > 0, 'rail count names what it counts');

paint(page(50, 15, 65));
assert.equal(rail.textContent, '65', 'rail keeps the matching total on page 2');
assert.ok(header.textContent.endsWith(' of 65'), `header still reports the matching total: ${header.textContent}`);

// Aggregate ("All workspaces") view: same agreement across workspaces.
common.setWorkspace(null);
assert.ok(common.isAggregateView(), 'the aggregate view is active');
const aggregate = (offset, size) => page(offset, size, 101, { workspace_name: 'ws_orbit' });
paint(aggregate(0, 50));
assert.equal(rail.textContent, '101', 'aggregate rail counts the matching total across workspaces');
assert.ok(header.textContent.endsWith(' of 101'), `aggregate header reports the same total: ${header.textContent}`);
paint(aggregate(50, 50));
assert.equal(rail.textContent, '101', 'aggregate rail keeps the total on the next page');

// The expanded task detail paints transitions from the same history projection
// the CLI reads, while older entries without statuses retain their event label.
common.setWorkspace('ws_orbit');
const historyTask = task(66, {
  history: [
    { event: 'status_changed', from_status: 'proposed', to_status: 'backlog', by: 'human:fixture', at: '2026-10-08T08:00:00Z' },
    { event: 'started', from_status: 'backlog', to_status: 'in-progress', by: 'system', at: '2026-10-08T08:01:00Z' },
    { event: 'status_changed', by: 'legacy', at: '2026-10-08T08:02:00Z' },
  ],
});
paint({ total: 1, limit: 50, offset: 0, next_cursor: null }, [historyTask]);
const historyTitle = byId['tasks-body'].querySelector('.row button.title');
assert.ok(historyTitle, 'task title disclosure exists');
historyTitle.parentNode.dispatch('click', { target: historyTitle });
const historyLines = byId['tasks-body'].querySelectorAll('.history-line').map(line => line.textContent);
assert.ok(historyLines.some(line => line.includes(': status proposed → backlog')), historyLines.join('\n'));
assert.ok(historyLines.some(line => line.includes(': started backlog → in-progress')), historyLines.join('\n'));
assert.ok(historyLines.some(line => line.includes(': status_changed')), historyLines.join('\n'));

// No pagination metadata: the rail falls back to the rows it was given.
common.setWorkspace('ws_orbit');
served = null;
renderTasks(matching.slice(0, 3).map(index => task(index)), taskContext);
assert.equal(rail.textContent, '3', 'without metadata the rail counts the rows it has');
