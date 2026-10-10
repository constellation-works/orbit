// ORB-14701: the Tasks rail count reports the matching total the panel header
// reports, not the rows on the page in front of the operator. Runs the shipped
// tasks.js renderer against a DOM stub, so what is asserted is what it paints.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { task as pilotTask, assessment, message as pilotMessage } from '../../tests/http_api/pilot_comment_fixture.mjs';

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
  dispatchEvent(event) { for (const fn of this.listeners[event.type] || []) fn(event); return true; }
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

const ids = ['tasks-body', 'tasks-count', 'rail-count-tasks', 'tasks-previous', 'tasks-next', 'tasks-page-status', 'host-select'];
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
class Event { constructor(type) { this.type = type; } }
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout, clearTimeout, document, window, Node, Event,
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

// Drive comment presentation through the shipped task detail renderer and raw control.
const openTask = fixture => {
  paint({ total: 1, limit: 50, offset: 0, next_cursor: null }, [fixture]);
  const title = byId['tasks-body'].querySelector('.row button.title');
  title.parentNode.dispatch('click', { target: title });
  return byId['tasks-body'].querySelector('.comment-card');
};
const card = openTask(pilotTask);
const body = card.querySelector('.comment-body');
const labels = body.querySelectorAll('dt').map(node => node.textContent);
const values = body.querySelectorAll('dd').map(node => node.textContent);
const fields = Object.fromEntries(labels.map((label, index) => [label, values[index]]));
assert.equal(fields.Disposition, assessment.disposition);
assert.equal(fields.Confidence, assessment.confidence);
assert.equal(fields['Recommended crew'], `implementer → ${assessment.recommended_crew}`);
assert.equal(fields['Recommended complexity'], `low → ${assessment.recommended_complexity}`);
assert.equal(fields.Rationale, assessment.assessment_rationale);
assert.equal(fields['Duplicate of'], `${assessment.duplicate_of.task_id} · ${assessment.duplicate_of.evidence}`);
assert.equal(fields['Already landed'], assessment.already_landed.evidence);
for (const value of [...assessment.evidence_gaps, ...assessment.reassessment_triggers, ...assessment.blocked_by]) {
  assert.ok(body.querySelectorAll('li').some(node => node.textContent === value), value);
}
assert.ok(!body.textContent.includes('{"'), 'default view presents fields rather than serialized JSON');
assert.ok(!card.classList.contains('collapsed'), 'assessment fields are visible without expanding the original JSON');
const raw = card.querySelector('.comment-raw');
assert.equal(raw.textContent, pilotMessage);
assert.equal(raw.style.display, 'none');
const rawToggle = card.querySelectorAll('button').find(button => button.textContent === 'raw');
rawToggle.dispatch('click');
assert.equal(body.style.display, 'none');
assert.equal(raw.style.display, '');
assert.equal(raw.textContent, pilotMessage, 'raw control shows the original receipt bytes');
rawToggle.dispatch('click');
assert.equal(body.style.display, '');

for (const [index, comment] of [
  { by: 'task-pilot', message: `operation_id=${'b'.repeat(64)}\n{"assessment":broken` },
  { by: 'human:fixture', message: pilotMessage },
  { by: 'task-pilot', message: `operation_id=invalid\n${JSON.stringify({ assessment })}` },
  { by: 'task-pilot', message: `operation_id=${'c'.repeat(64)}\n{"assessment":{}}` },
].entries()) {
  const fallback = openTask({ ...pilotTask, id: `ORB-${80 + index}`, comments: [comment] });
  assert.equal(fallback.querySelector('.comment-body').textContent, comment.message, 'unrecognized comment retains the Markdown/plain-text fallback');
  assert.equal(fallback.querySelector('.comment-raw').textContent, comment.message);
}
const plain = openTask({ ...pilotTask, id: 'ORB-90', comments: [{ ...pilotTask.comments[0], message: `operation_id=${'d'.repeat(64)}\n${JSON.stringify({ assessment: { ...assessment, assessment_rationale: '<img src=x onerror=alert(1)>' } })}` }] });
assert.ok(plain.querySelector('.comment-body').textContent.includes('<img src=x onerror=alert(1)>'));
assert.equal(plain.querySelectorAll('img').length, 0, 'assessment fields are text, never executable markup');

// No pagination metadata: the rail falls back to the rows it was given.
common.setWorkspace('ws_orbit');
served = null;
renderTasks(matching.slice(0, 3).map(index => task(index)), taskContext);
assert.equal(rail.textContent, '3', 'without metadata the rail counts the rows it has');

// ORB-15216: a replica workspace with no local tasks names its owner and
// switches the host picker to it. Any other empty workspace keeps the plain text.
const picker = byId['host-select'];
const emptyPage = { total: 0, limit: 50, offset: 0, next_cursor: null };
const emptyText = () => byId['tasks-body'].querySelector('.empty-state .text').textContent;
const hostRows = (role, ownerMachineId, ownerRegistered = true) => [
  { name: 'box-a', machine_id: 'hm_local', local: true, workspaces: [{ id: 'ws_orbit', name: 'orbit', role, owner_machine_id: ownerMachineId, status: 'active' }] },
  ...(ownerRegistered ? [{ name: 'dk-server-2', machine_id: 'hm_owner', local: false, reachable: true }] : []),
];
const pickerChanges = [];
picker.addEventListener('change', () => pickerChanges.push(picker.value));

common.setRegisteredHosts(hostRows('replica', 'hm_owner'));
paint(emptyPage, []);
assert.equal(emptyText(), 'This checkout is a pull replica of dk-server-2; its tasks live on the owner.');
const showOnOwner = byId['tasks-body'].querySelector('.empty-state button');
assert.equal(showOnOwner.textContent, 'Show on dk-server-2', 'a registered owner gets a switch control');
showOnOwner.dispatch('click');
assert.equal(picker.value, 'dk-server-2', 'the control switches the host picker to the owner');
assert.deepEqual(pickerChanges, ['dk-server-2'], 'the picker change is what the host switcher acts on');

common.setRegisteredHosts(hostRows('replica', 'hm_gone', false));
paint(emptyPage, []);
assert.equal(emptyText(), 'This checkout is a pull replica of hm_gone; its tasks live on the owner.', 'an unregistered owner is named by machine id');
assert.equal(byId['tasks-body'].querySelector('.empty-state button'), null, 'no picker entry exists to switch to');

common.setRegisteredHosts(hostRows('owner', 'hm_local'));
paint(emptyPage, []);
assert.equal(emptyText(), 'No tasks available.', 'a non-replica workspace keeps the plain empty state');
assert.equal(byId['tasks-body'].querySelector('.empty-state button'), null);

// ORB-15213: a backlog row says why the drain is not starting it, and the group
// hint counts what is eligible versus waiting, from the same readiness snapshot
// the Drain card renders. Rows without a wait stay as they were.
const drainWaits = modules.get('drain-waits.js').namespace;
const backlogContext = { ...taskContext, getActiveStatuses: () => new Set(['backlog']), statusOrder: ['backlog'] };
const backlog = [31, 32, 33, 34, 35].map(index => task(index, { status: 'backlog' }));
const paintBacklog = () => {
  served = { total: backlog.length, limit: 50, offset: 0, next_cursor: null };
  currentTasks = backlog;
  renderTasks(backlog, backlogContext);
};
const backlogBody = byId['tasks-body'];
const rowFor = id => backlogBody.querySelectorAll('.row').find(row => row.dataset.key === `task-${id}`);
const waitBadgeOf = id => rowFor(id).querySelector('.drain-wait-badge');
const groupHintText = () => backlogBody.querySelector('.group-header .group-hint').textContent;
const repaints = [];
const stopListening = drainWaits.onDrainReadinessChange(() => { repaints.push('changed'); paintBacklog(); });

paintBacklog();
assert.ok(!groupHintText().includes('eligible'), `without a snapshot the hint claims no eligibility: ${groupHintText()}`);
assert.equal(backlogBody.querySelectorAll('.drain-wait-badge').length, 0, 'no snapshot, no wait badges');

const snapshot = {
  tasks: [
    { task_id: 'ORB-31', status: 'backlog', eligible: true, reason: 'ready' },
    { task_id: 'ORB-32', status: 'backlog', eligible: false, reason: 'context_lock_conflict', conflicts: [{ requested_file: 'file:docs/CONFIG.md', locking_task_id: 'ORB-77' }] },
    { task_id: 'ORB-33', status: 'backlog', eligible: false, reason: 'host_os_mismatch', detail: 'waits for a macos host (os:macos)' },
    { task_id: 'ORB-34', status: 'backlog', eligible: false, reason: 'resource_throttled', detail: 'cpu 91% over 80%' },
    { task_id: 'ORB-99', status: 'backlog', eligible: false, reason: 'context_lock_conflict' },
  ],
};
drainWaits.setDrainReadiness(snapshot);
assert.equal(repaints.length, 1, 'a new snapshot repaints the list once');
assert.equal(waitBadgeOf('ORB-31'), null, 'an eligible task shows no wait');
assert.equal(waitBadgeOf('ORB-32').textContent, 'waits on ORB-77 · lock', 'a lock wait names the holder');
assert.ok(waitBadgeOf('ORB-32').title.includes('Lock: file:docs/CONFIG.md') && waitBadgeOf('ORB-32').title.includes('context_lock_conflict'), `the lock detail is in the tooltip: ${waitBadgeOf('ORB-32').title}`);
assert.equal(waitBadgeOf('ORB-33').textContent, 'needs macos host', 'a host wait names the OS');
assert.ok(waitBadgeOf('ORB-33').title.includes('waits for a macos host (os:macos)'), 'the host detail is in the tooltip');
assert.equal(waitBadgeOf('ORB-34').textContent, 'throttled', 'a throttle wait says so');
assert.equal(waitBadgeOf('ORB-35'), null, 'a task the snapshot does not mention shows no wait');
assert.equal(groupHintText(), '5 approved · 1 eligible now, 1 waiting on locks, 1 waiting on capacity, 1 waiting, other, 1 not in the drain snapshot');

// The same snapshot again changes nothing, so the 30 s poll does not repaint.
drainWaits.setDrainReadiness(JSON.parse(JSON.stringify(snapshot)));
assert.equal(repaints.length, 1, 'an unchanged snapshot does not repaint');

// A snapshot belongs to one workspace: leaving it drops every wait.
common.setWorkspace('ws_other');
assert.equal(repaints.length, 2, 'leaving the workspace repaints');
assert.equal(backlogBody.querySelectorAll('.drain-wait-badge').length, 0, 'another workspace wears none of the previous waits');
assert.ok(!groupHintText().includes('eligible'), 'and the hint stops counting');
stopListening();
common.setWorkspace('ws_orbit');
