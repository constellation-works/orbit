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
  prepend(child) { this.insertBefore(child, this.children[0] || null); }
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
  replaceWith(...nodes) {
    if (!this.parentNode) return;
    const parent = this.parentNode;
    const index = parent.children.indexOf(this);
    parent.removeChild(this);
    const before = parent.children[index] || null;
    for (const node of nodes) parent.insertBefore(node, before);
  }
  setAttribute(name, value) { this.attrs[name] = String(value); }
  removeAttribute(name) { delete this.attrs[name]; }
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
  // `[data-draft]`-style presence tests, so an open detail is not mistaken for a draft.
  for (const [, name] of simple.matchAll(/\[([\w-]+)\]/g)) {
    const key = name.startsWith('data-') ? name.slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase()) : null;
    if (!(key ? node.dataset[key] !== undefined : node.getAttribute(name) != null)) return false;
  }
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

const ids = ['tasks-main', 'tasks-body', 'task-detail-pane', 'task-detail-body', 'task-pane-resizer', 'tasks-count', 'rail-count-tasks', 'tasks-previous', 'tasks-next', 'tasks-page-status', 'host-select'];
const byId = Object.fromEntries(ids.map(id => [id, Object.assign(new Node('div'), { id })]));
// The selected task's detail renders in the pane beside the list.
const paneBody = byId['task-detail-body'];
const document = {
  activeElement: null,
  body: new Node('body'),
  createElement: tag => new Node(tag),
  createDocumentFragment: () => new Node('#fragment'),
  createTextNode: text => { const node = new Node('#text'); node.own = String(text); return node; },
  getElementById: id => byId[id] || null,
  querySelectorAll: selector => document.body.querySelectorAll(selector),
  querySelector: selector => document.body.querySelector(selector),
};
// Document queries must reach rendered details when runtime data or scope changes.
for (const node of Object.values(byId)) document.body.appendChild(node);
const window = { location: { search: '', hash: '' }, confirm: () => true };
class Event { constructor(type) { this.type = type; } }
// Unanswered stub fetches keep their timeout behavior without keeping Node
// alive for 30 seconds after the fixture has finished all its assertions.
const setFixtureTimeout = (callback, delay) => setTimeout(callback, delay).unref();
const context = vm.createContext({
  URLSearchParams, URL, AbortController, console, setTimeout: setFixtureTimeout, clearTimeout, document, window, Node, Event,
  requestAnimationFrame: () => 0,
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
const { renderTasks, applyTasksHashQuery, buildTasksHash, setPinnedExternalTask, clearPinnedExternalTask } = tasksModule.namespace;
const common = modules.get('common.js').namespace;

// A matching set of 65 tasks read in pages of 50, as /api/tasks serves them.
const task = (index, extra = {}) => ({ id: `ORB-${index}`, title: `Task ${index}`, status: 'in-progress', priority: 'medium', ...extra });
// A row's title sits inside its title line, and this stub does not bubble, so
// a click on the title reaches the row's disclosure handler from the row.
const clickTitle = title => {
  let row = title.parentNode;
  while (row && !('rovingRow' in row.dataset)) row = row.parentNode;
  row.dispatch('click', { target: title });
};
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

// Crew-only limits appear in both the list and detail; config-only changes
// refresh them even when task tags and crew assignment have not changed.
common.setWorkspace('ws_orbit');
const constrained = task(67, { crew: 'fixture-crew', tags: [], os_requirement: {
  any_of: ['macos'], state: 'restricted', describe: 'macOS hosts (crew fixture-crew)',
  source: { tags: [], crew: 'fixture-crew', executor: 'fixture-executor' },
} });
paint({ total: 1, limit: 50, offset: 0, next_cursor: null }, [constrained]);
let osRow = byId['tasks-body'].querySelectorAll('.row').find(row => row.dataset.key === 'task-ORB-67');
assert.equal(osRow.querySelector('.os-badge').title, `Runs on ${constrained.os_requirement.describe}`);
const osTitle = osRow.querySelector('button.title');
clickTitle(osTitle);
// The detail names the OS in a word; the hint beside it carries the full reason.
assert.equal(paneBody.querySelector('.task-os-readout').textContent, 'macOS');
assert.equal(paneBody.querySelector('.task-os-readout').parentNode.querySelector('.info-hint-text').textContent, constrained.os_requirement.describe);
constrained.os_requirement = { any_of: [], state: 'unresolved', describe: 'unresolved (crew fixture-crew)' };
paint({ total: 1, limit: 50, offset: 0, next_cursor: null }, [constrained]);
osRow = byId['tasks-body'].querySelectorAll('.row').find(row => row.dataset.key === 'task-ORB-67');
assert.equal(osRow.querySelector('.os-badge').textContent, 'unresolved');
assert.equal(paneBody.querySelector('.task-os-readout').textContent, 'unresolved');
assert.equal(paneBody.querySelector('.task-os-readout').parentNode.querySelector('.info-hint-text').textContent, constrained.os_requirement.describe);

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
clickTitle(historyTitle);
const historyLines = paneBody.querySelectorAll('.history-line').map(line => line.textContent);
assert.ok(historyLines.some(line => line.includes(': status proposed → backlog')), historyLines.join('\n'));
assert.ok(historyLines.some(line => line.includes(': started backlog → in-progress')), historyLines.join('\n'));
assert.ok(historyLines.some(line => line.includes(': status_changed')), historyLines.join('\n'));

// Drive comment presentation through the shipped task detail renderer and raw control.
const openTask = fixture => {
  paint({ total: 1, limit: 50, offset: 0, next_cursor: null }, [fixture]);
  const title = byId['tasks-body'].querySelector('.row button.title');
  clickTitle(title);
  return paneBody.querySelector('.comment-card');
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
  { by: 'task-pilot', operation_id: 'invalid', message: JSON.stringify({ assessment }) },
  { by: 'task-pilot', message: JSON.stringify({ assessment }) },
].entries()) {
  const fallback = openTask({ ...pilotTask, id: `ORB-${80 + index}`, comments: [comment] });
  assert.equal(fallback.querySelector('.comment-body').textContent, comment.message, 'unrecognized comment retains the Markdown/plain-text fallback');
  assert.equal(fallback.querySelector('.comment-raw').textContent, comment.message);
}
// A receipt written with the typed operation id carries only the audit body.
const typed = openTask({ ...pilotTask, id: 'ORB-89', comments: [{ by: 'task-pilot', operation_id: 'e'.repeat(64), message: JSON.stringify({ assessment }) }] });
assert.ok(typed.querySelector('.comment-body').textContent.includes(assessment.assessment_rationale), 'typed receipt renders its assessment');
assert.ok(!typed.querySelector('.comment-body').textContent.includes('{"'), 'typed receipt presents fields rather than serialized JSON');
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

// ORB-15213: a backlog row says why the drain is not starting it, as a
// readiness dot beside its ID with the wait in its label and tooltip, and the
// Backlog card counts each pool in a legend that filters the card, from the
// same readiness snapshot the Drain card renders.
const drainWaits = modules.get('drain-waits.js').namespace;
const backlogContext = { ...taskContext, getActiveStatuses: () => new Set(['backlog']), statusOrder: ['backlog'] };
const backlog = [31, 32, 33, 34, 35].map(index => task(index, { status: 'backlog' }));
const paintBacklog = () => {
  served = { total: backlog.length, limit: 50, offset: 0, next_cursor: null };
  currentTasks = backlog;
  renderTasks(backlog, backlogContext);
};
const backlogBody = byId['tasks-body'];
const taskRows = () => backlogBody.querySelectorAll('.row').filter(row => String(row.dataset.key || '').startsWith('task-'));
const rowFor = id => taskRows().find(row => row.dataset.key === `task-${id}`);
const dotOf = id => rowFor(id).querySelector('.ready-dot');
const legendButtons = () => backlogBody.querySelectorAll('.ready-filter');
const legendCounts = () => Object.fromEntries(legendButtons().map(button => [button.dataset.pool, button.querySelector('.ready-filter-count').textContent]));

// ORB-15344: equal-count selector replacements repaint the run tooltip on the
// lock read, and an ordinary list refresh reaches the same new paths. Repeating
// the exact snapshot leaves both the row and runtime publication count alone.
const taskRuntime = modules.get('task-runtime.js').namespace;
const lockTask = task(36, { job_run_id: 'run-36', job_run_machine: { machine_name: 'dk-server-2' } });
const lockPage = { total: 1, limit: 50, offset: 0, next_cursor: null };
const lockPayload = path => ({ by_task: [{ id: 'ORB-36', context_files: [path] }], total_locked: 1 });
taskRuntime.setTaskLocks(lockPayload('file:old.rs'));
paint(lockPage, [lockTask]);
assert.ok(rowFor('ORB-36').querySelector('.run-cell').title.includes('file:old.rs'));
const lockPublications = [];
const stopLockListening = taskRuntime.onTaskRuntimeChange(() => {
  lockPublications.push('changed');
  renderTasks(currentTasks, taskContext);
});
taskRuntime.setTaskLocks(lockPayload('file:new.rs'));
assert.equal(lockPublications.length, 1, 'a same-count replacement publishes the changed selector');
assert.ok(rowFor('ORB-36').querySelector('.run-cell').title.includes('file:new.rs'), 'the lock read repaints the run tooltip');
paint(lockPage, [lockTask]);
const refreshedLockRow = rowFor('ORB-36');
assert.ok(refreshedLockRow.querySelector('.run-cell').title.includes('file:new.rs'), 'a task-list refresh keeps the replacement path');
taskRuntime.setTaskLocks(lockPayload('file:new.rs'));
assert.equal(lockPublications.length, 1, 'an identical lock snapshot does not publish');
renderTasks(currentTasks, taskContext);
assert.equal(rowFor('ORB-36'), refreshedLockRow, 'an identical snapshot preserves the existing row');
stopLockListening();

// Empty context uses the same refreshable list as populated selectors: when a
// background task/lock refresh adds a contended selector, the separate open
// description editor keeps its node, text, and focus.
const contextDraftTask = task(37, { description: 'Saved description', context_files: [] });
const contextDraftPage = { total: 1, limit: 50, offset: 0, next_cursor: null };
const contextDraftContext = { ...taskContext, getTasks: () => [contextDraftTask] };
currentTasks = [contextDraftTask];
served = contextDraftPage;
renderTasks(currentTasks, contextDraftContext);
const contextDraftTitle = rowFor('ORB-37').querySelector('button.title');
clickTitle(contextDraftTitle);
let contextDraftDetail = paneBody.querySelector('#detail-ORB-37');
assert.ok(
  contextDraftDetail.querySelector('.file-list .field-empty').textContent.includes('no context files'),
  'the empty context state is visible before refresh',
);
const descriptionEdit = contextDraftDetail
  .querySelectorAll('.field-edit')
  .find(button => button.getAttribute('aria-label') === 'Edit description for ORB-37');
descriptionEdit.dispatch('click');
const descriptionDraft = contextDraftDetail.querySelector('.field-editor-input');
descriptionDraft.value = 'Unsaved description';
descriptionDraft.focus();
contextDraftTask.context_files = ['file:src/shared.rs'];
taskRuntime.setTaskLocks({
  by_task: [{ id: 'ORB-38', context_files: ['file:src/shared.rs'] }],
  total_locked: 1,
  context_contention: {
    'ORB-37': {
      status: 'in-progress',
      entries: [{ selector: 'file:src/shared.rs', known: true, contenders: [{ id: 'ORB-38', status: 'in-progress', selector: 'file:src/shared.rs' }] }],
    },
  },
});
renderTasks(currentTasks, contextDraftContext);
contextDraftDetail = paneBody.querySelector('#detail-ORB-37');
const refreshedSelector = contextDraftDetail.querySelector('.context-selector');
assert.ok(
  refreshedSelector && refreshedSelector.textContent.includes('file:src/') && refreshedSelector.textContent.includes('shared.rs'),
  'empty-to-populated refresh renders the selector',
);
const activeContention = contextDraftDetail.querySelector('.context-contended-active');
assert.ok(!activeContention.textContent.includes('ORB-38'), 'contention adds no blocker metadata');
assert.ok(activeContention.querySelector('a.context-selector').href.includes('open=ORB-37'), 'the context links to its owning task');
assert.ok(activeContention.textContent.includes('file:src/') && activeContention.textContent.includes('shared.rs'), 'the selector text remains intact');
assert.equal(contextDraftDetail.querySelector('.field-editor-input'), descriptionDraft, 'refresh preserves the separate editor control');
assert.equal(descriptionDraft.value, 'Unsaved description', 'refresh preserves the editor text');
assert.equal(document.activeElement, descriptionDraft, 'refresh preserves editor focus');

const cancelDescription = contextDraftDetail.querySelector('.field-editor .cancel');
cancelDescription.dispatch('click');
const contextEdit = contextDraftDetail.querySelectorAll('.field-edit').find(button => button.getAttribute('aria-label') === 'Edit context files for ORB-37');
contextEdit.dispatch('click');
const selectorDraft = contextDraftDetail.querySelector('.field-editor-input');
selectorDraft.value = 'file:unsaved.rs';
selectorDraft.focus();
contextDraftTask.context_files = ['file:server.rs'];
renderTasks(currentTasks, contextDraftContext);
contextDraftDetail = paneBody.querySelector('#detail-ORB-37');
assert.equal(
  contextDraftDetail.querySelector('.field-editor-input'),
  selectorDraft,
  'refresh preserves the context editor control',
);
assert.equal(selectorDraft.value, 'file:unsaved.rs', 'refresh preserves the context editor text');
assert.equal(document.activeElement, selectorDraft, 'refresh preserves context editor focus');

const repaints = [];
const stopListening = drainWaits.onDrainReadinessChange(() => { repaints.push('changed'); paintBacklog(); });

paintBacklog();
assert.equal(backlogBody.querySelector('.group-readiness'), null, 'without a snapshot the card claims no readiness');
assert.ok(taskRows().every(row => row.querySelector('.ready-dot').classList.contains('none')), 'no snapshot, no readiness dots');

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
assert.equal(dotOf('ORB-31').dataset.pool, 'eligible');
assert.equal(dotOf('ORB-31').getAttribute('aria-label'), 'Eligible now', 'an eligible task says so');
assert.ok(rowFor('ORB-31').querySelector('.task-quick').classList.contains('ready'), 'Ship reads ready when nothing holds the task');
assert.equal(dotOf('ORB-32').dataset.pool, 'locks');
assert.equal(dotOf('ORB-32').getAttribute('aria-label'), 'Waiting on locks: waits on ORB-77', 'a lock wait names the holder');
assert.equal(dotOf('ORB-32').title, 'Waits on ORB-77', 'the tooltip names the holder without raw lock metadata');
assert.ok(!rowFor('ORB-32').querySelector('.task-quick').classList.contains('ready'), 'Ship stays quiet while the task waits');
assert.equal(dotOf('ORB-33').dataset.pool, 'other');
assert.equal(dotOf('ORB-33').getAttribute('aria-label'), 'Waiting, other: needs macos host', 'a host wait names the OS');
assert.ok(dotOf('ORB-33').title.includes('waits for a macos host (os:macos)'), 'the host detail is in the tooltip');
assert.equal(dotOf('ORB-34').dataset.pool, 'capacity');
assert.equal(dotOf('ORB-34').getAttribute('aria-label'), 'Waiting on capacity: throttled', 'a throttle wait says so');
assert.ok(dotOf('ORB-35').classList.contains('none'), 'a task the snapshot does not mention shows no readiness');
assert.deepEqual(legendCounts(), { eligible: '1', locks: '1', capacity: '1', other: '1' }, 'the legend counts each pool once');
assert.equal(backlogBody.querySelector('.ready-unknown').textContent, '1 not in the drain snapshot');

// A legend entry narrows the card to its pool and a second press restores it.
legendButtons().find(button => button.dataset.pool === 'locks').dispatch('click');
assert.deepEqual(taskRows().map(row => row.dataset.key), ['task-ORB-32'], 'the locks entry shows only lock waits');
assert.equal(legendButtons().find(button => button.dataset.pool === 'locks').getAttribute('aria-pressed'), 'true');
legendButtons().find(button => button.dataset.pool === 'locks').dispatch('click');
assert.equal(taskRows().length, 5, 'pressing it again shows every backlog task');

// The same snapshot again changes nothing, so the 30 s poll does not repaint.
drainWaits.setDrainReadiness(JSON.parse(JSON.stringify(snapshot)));
assert.equal(repaints.length, 1, 'an unchanged snapshot does not repaint');

// A snapshot belongs to one workspace: leaving it drops every wait.
common.setWorkspace('ws_other');
assert.equal(repaints.length, 2, 'leaving the workspace repaints');
assert.ok(taskRows().every(row => row.querySelector('.ready-dot').classList.contains('none')), 'another workspace wears none of the previous waits');
assert.equal(backlogBody.querySelector('.group-readiness'), null, 'and the card stops counting');
stopListening();
common.setWorkspace('ws_orbit');

// A requeued task resuming its published delivery is admitted past another
// task's lock: its dot is eligible, names the pull request it completes, and
// Ship reads ready rather than a lock wait.
drainWaits.setDrainReadiness({
  tasks: [{ task_id: 'ORB-32', status: 'backlog', eligible: true, reason: 'ready', published_delivery: { source_run_id: 'run-32', pull_request: '17' } }],
});
paintBacklog();
assert.equal(dotOf('ORB-32').dataset.pool, 'eligible', 'a resumed published delivery is not a lock wait');
assert.ok(dotOf('ORB-32').getAttribute('aria-label').includes('PR #17'), 'the dot names the pull request it resumes');
assert.ok(dotOf('ORB-32').title.includes('PR #17'), 'and so does its tooltip');
assert.ok(rowFor('ORB-32').querySelector('.task-quick').classList.contains('ready'), 'Ship reads ready');
drainWaits.setDrainReadiness({ tasks: [] });

// ORB-15371: the Readiness card of a proposed or backlog task leads with its
// drain pool and the row dot's badge text, then what holds it, the server's
// detail and the raw reason, above the preparation gaps. The detail reads its
// own task's entry, so it answers with no Drain card snapshot at all, and
// "Ready" is claimed only for an eligible task with nothing missing.
const readyGaps = { ready: true, preparing: false, gaps: [] };
const advisoryGap = { ready: true, preparing: false, gaps: [{ code: 'missing_plan', severity: 'advisory', message: 'No plan yet', fix: 'Write a plan' }] };
const redBase = 'required validation `make ci-test-affected` fails on `origin/agent-main` at 1a2b3c4d; the hold stands until the owner\'s clock checks a new base tip and the command passes there';
const readinessEntries = {
  'ORB-41': { task_id: 'ORB-41', status: 'backlog', eligible: false, reason: 'context_lock_conflict', conflicts: [{ requested_file: 'file:docs/CONFIG.md', locking_task_id: 'ORB-77' }] },
  'ORB-42': { task_id: 'ORB-42', status: 'backlog', eligible: false, reason: 'capacity_saturated', active_run_ids: ['jrun-1'] },
  'ORB-43': { task_id: 'ORB-43', status: 'backlog', eligible: false, reason: 'baseline_red_hold', detail: redBase },
  'ORB-44': { task_id: 'ORB-44', status: 'backlog', eligible: true, reason: 'ready' },
  'ORB-45': { task_id: 'ORB-45', status: 'backlog', eligible: false, reason: 'unmet_dependency', dependencies: [{ task_id: 'ORB-50', status: 'in-progress' }, { task_id: 'ORB-51', status: 'proposed' }] },
  'ORB-46': { task_id: 'ORB-46', status: 'backlog', eligible: true, reason: 'ready' },
  'ORB-47': { task_id: 'ORB-47', status: 'backlog', eligible: false, reason: 'brand_new_wait', detail: 'something the dashboard has never heard of' },
  'ORB-48': { task_id: 'ORB-48', status: 'proposed', eligible: false, reason: 'not_backlog' },
  'ORB-60': { task_id: 'ORB-60', status: 'backlog', eligible: false, reason: 'crew_not_allowed', crew: 'opus', allowed_crews: ['sol'] },
};
const readinessReads = [];
const heldReads = new Map();
const holdRead = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};
const settleReads = () => new Promise(setImmediate);
const readinessResponse = (body, ok = true) => ({ ok, status: ok ? 200 : 500, text: async () => JSON.stringify(body), json: async () => body, headers: { get: () => null } });
context.fetch = path => {
  const url = new URL(path, 'http://dashboard.test');
  assert.equal(url.pathname, '/api/workflows/auto/readiness', `the detail reads only drain readiness: ${path}`);
  const id = url.searchParams.get('task');
  readinessReads.push({ id, workspace: url.searchParams.get('workspace') });
  if (heldReads.has(id)) return heldReads.get(id).promise;
  if (id === 'ORB-49') return Promise.resolve(readinessResponse({ error: 'readiness unavailable' }, false));
  return Promise.resolve(readinessResponse({ tasks: [readinessEntries[id]] }));
};
const cardTasks = [
  task(41, { status: 'backlog', readiness: readyGaps }),
  task(42, { status: 'backlog', readiness: readyGaps }),
  task(43, { status: 'backlog', readiness: readyGaps }),
  task(44, { status: 'backlog', readiness: readyGaps }),
  task(45, { status: 'backlog', readiness: readyGaps }),
  task(46, { status: 'backlog', readiness: advisoryGap }),
  task(47, { status: 'backlog', readiness: readyGaps }),
  task(48, { status: 'proposed', readiness: readyGaps }),
  task(60, { status: 'backlog', readiness: readyGaps }),
  task(49, { status: 'backlog', readiness: readyGaps }),
];
const cardContext = { ...taskContext, getActiveStatuses: () => new Set(['proposed', 'backlog']), statusOrder: ['proposed', 'backlog'] };
const paintCards = () => {
  served = { total: cardTasks.length, limit: 50, offset: 0, next_cursor: null };
  currentTasks = cardTasks;
  renderTasks(cardTasks, cardContext);
};
// One task's detail at a time: reading a card selects its row first.
const showCard = id => {
  if (paneBody.querySelector(`#detail-${id}`)) return;
  const title = rowFor(id).querySelector('button.title');
  clickTitle(title);
};
const readinessCard = id => {
  showCard(id);
  return paneBody.querySelector(`#detail-${id} .readiness-list`);
};
const cardHead = id => readinessCard(id).querySelector('.readiness-drain-head').children.slice(0, 2).map(node => node.textContent);
heldReads.set('ORB-41', holdRead());
paintCards();
for (const item of cardTasks) {
  const title = rowFor(item.id).querySelector('button.title');
  clickTitle(title);
}
assert.ok(readinessCard('ORB-41').textContent.includes('Checking whether the drain can take this task'), 'a read in flight says so');
assert.ok(!readinessCard('ORB-41').textContent.includes('Ready'), 'a read in flight claims no readiness');
heldReads.get('ORB-41').resolve(readinessResponse({ tasks: [readinessEntries['ORB-41']] }));
heldReads.delete('ORB-41');
await settleReads();
assert.deepEqual(
  readinessReads.map(read => read.id).sort(),
  cardTasks.map(item => item.id).sort(),
  'each open proposed or backlog task asks for its own readiness',
);
assert.ok(readinessReads.every(read => read.workspace === 'ws_orbit'), 'in the selected workspace');

// Waiting on a lock links its holder without exposing raw lock metadata.
assert.deepEqual(cardHead('ORB-41'), ['', 'Waiting on locks']);
const holderLink = readinessCard('ORB-41').querySelector('a.relation-target');
assert.equal(holderLink.textContent, 'ORB-77');
assert.ok(holderLink.href.includes('open=ORB-77'), `the holder is a link to its task: ${holderLink.href}`);
assert.ok(!readinessCard('ORB-41').textContent.includes('file:docs/CONFIG.md'), 'raw lock paths do not crowd readiness');
assert.ok(!readinessCard('ORB-41').textContent.includes('Reason:'), 'readiness does not repeat raw reason codes');
assert.ok(!readinessCard('ORB-41').textContent.includes('Ready: nothing is missing.'), 'a waiting task is never Ready');
assert.ok(readinessCard('ORB-41').textContent.includes('No preparation gaps.'));

// Waiting on capacity.
assert.deepEqual(cardHead('ORB-42'), ['', 'Waiting on capacity']);
assert.ok(readinessCard('ORB-42').textContent.includes('no free slot'));

// A red base: "base red", with the command and base sha from the hold.
assert.deepEqual(cardHead('ORB-43'), ['', 'Waiting, other']);
assert.ok(readinessCard('ORB-43').textContent.includes('make ci-test-affected') && readinessCard('ORB-43').textContent.includes('1a2b3c4d'), 'the hold names its command and red base');
assert.ok(readinessCard('ORB-43').textContent.includes('base red'));

// Eligible with nothing missing is the only Ready.
assert.deepEqual(cardHead('ORB-44'), ['', 'Eligible now']);
assert.ok(readinessCard('ORB-44').textContent.includes('Ready: nothing is missing.'), 'an eligible task with no gaps is Ready');

// Unmet dependencies: each linked, with its status.
assert.deepEqual(cardHead('ORB-45'), ['', 'Waiting, other']);
const dependencies = readinessCard('ORB-45').querySelectorAll('.readiness-dependency').map(node => node.textContent);
assert.deepEqual(dependencies, ['ORB-50in-progress', 'ORB-51proposed'], 'each dependency is listed with its status');
assert.deepEqual(readinessCard('ORB-45').querySelectorAll('.readiness-dependency a.relation-target').map(link => link.textContent), ['ORB-50', 'ORB-51']);

// Eligible, but a preparation gap stays listed in the same card and Ready is not claimed.
assert.deepEqual(cardHead('ORB-46'), ['', 'Eligible now']);
assert.ok(readinessCard('ORB-46').querySelector('.readiness-gap').textContent.includes('No plan yet'), 'the gap stays in the card');
assert.ok(!readinessCard('ORB-46').textContent.includes('Ready: nothing is missing.'));

// An unknown reason keeps its readable cause and detail.
assert.deepEqual(cardHead('ORB-47'), ['', 'Waiting, other']);
assert.ok(readinessCard('ORB-47').textContent.includes('something the dashboard has never heard of'));
assert.ok(readinessCard('ORB-47').textContent.includes('brand new wait'));

// A proposed task waits for approval.
assert.deepEqual(cardHead('ORB-48'), ['', 'Waiting, other']);
assert.ok(readinessCard('ORB-48').textContent.includes('awaits approval'), 'a proposed task still names approval as its cause');

// ORB-16358: a task the live drain's `--allow-crew` set leaves out waits,
// naming the crew it runs as and the crews the drain allows.
assert.deepEqual(cardHead('ORB-60'), ['', 'Waiting, other']);
assert.ok(readinessCard('ORB-60').textContent.includes('crew opus not allowed'), readinessCard('ORB-60').textContent);
assert.ok(readinessCard('ORB-60').textContent.includes('it allows only sol'), readinessCard('ORB-60').textContent);
assert.ok(!readinessCard('ORB-60').textContent.includes('Ready: nothing is missing.'), 'a crew-excluded task is never Ready');

// No answer and no snapshot: the card says readiness is unknown, never Ready.
assert.ok(readinessCard('ORB-49').textContent.includes('Drain readiness unknown: readiness unavailable.'), readinessCard('ORB-49').textContent);
assert.ok(!readinessCard('ORB-49').textContent.includes('Ready'), 'unknown readiness is not Ready');

// The row dot and the card word one entry alike. The Drain card's snapshot
// also stands in for a task whose own read failed.
drainWaits.setDrainReadiness({ tasks: [
  ...Object.values(readinessEntries).filter(entry => entry.status === 'backlog'),
  { task_id: 'ORB-49', status: 'backlog', eligible: false, reason: 'capacity_saturated' },
] });
paintCards();
for (const id of ['ORB-41', 'ORB-42', 'ORB-43', 'ORB-44', 'ORB-45', 'ORB-60']) {
  const [, pool] = cardHead(id);
  const dotLabel = dotOf(id).getAttribute('aria-label');
  assert.ok(dotLabel === pool || dotLabel.startsWith(`${pool}: `), `${id}: dot "${dotLabel}" matches card pool "${pool}"`);
}
assert.deepEqual(cardHead('ORB-49'), ['', 'Waiting on capacity'], 'the snapshot entry stands in');
assert.equal(dotOf('ORB-60').getAttribute('aria-label'), 'Waiting, other: crew opus not allowed', 'the dot names the excluded crew');
assert.ok(dotOf('ORB-60').title.includes('it allows only sol'), `the tooltip names the allowed crews: ${dotOf('ORB-60').title}`);
assert.equal(dotOf('ORB-44').dataset.pool, 'eligible', 'a task the drain allows stays eligible');

// The Drain card names the live drain's crews and the tasks they leave out.
const crewNote = drainWaits.drainCrewNote({ capacity: { allowed_crews: ['sol'] }, tasks: Object.values(readinessEntries) });
assert.ok(crewNote.textContent.includes('Allowed crews: sol.'), crewNote.textContent);
assert.ok(crewNote.textContent.includes('1 backlog task runs as another crew and waits: ORB-60 (opus).'), crewNote.textContent);
assert.ok(!crewNote.textContent.includes('ORB-44'), 'an eligible task is not listed as excluded');
assert.equal(drainWaits.drainCrewNote({ capacity: { allowed_crews: null }, tasks: Object.values(readinessEntries) }), null, 'no restriction, no note');
// Clicking the selected row again closes the pane.
const selectedTitle = backlogBody.querySelector('.row.expanded button.title');
clickTitle(selectedTitle);
assert.equal(byId['task-detail-pane'].hidden, true, 'the selected row toggles the pane closed');
assert.equal(backlogBody.querySelector('.row.expanded'), null, 'no row reads as selected once the pane closes');

// A live child run belongs to the task's workspace, even when the page is in
// the aggregate view or the task is pinned from another workspace. Its link
// keeps the selected host and time window as well.
const liveRunEntries = new Map([
  ['ORB-998', { task_id: 'ORB-998', status: 'backlog', eligible: false, reason: 'claimed_by_live_child', run_ids: ['jrun-owner'] }],
  ['ORB-999', { task_id: 'ORB-999', status: 'backlog', eligible: false, reason: 'claimed_by_live_child', run_ids: ['jrun-owner'] }],
]);
const liveRunReads = [];
context.fetch = path => {
  const url = new URL(path, 'http://dashboard.test');
  if (url.pathname.endsWith('/workflows/auto/readiness')) liveRunReads.push(url);
  const id = url.searchParams.get('task');
  return Promise.resolve(readinessResponse({ tasks: [liveRunEntries.get(id)] }));
};
common.setRegisteredHosts(hostRows('owner', 'hm_local'));
common.setWindow('7d');
for (const fixture of [
  { id: 'ORB-998', pageWorkspace: null, label: 'aggregate task' },
  { id: 'ORB-999', pageWorkspace: 'ws_selected', label: 'cross-workspace pinned task' },
]) {
  common.setWorkspace(fixture.pageWorkspace);
  const liveTask = task(Number(fixture.id.slice(4)), { status: 'backlog', workspace_id: 'ws_owner', readiness: readyGaps });
  served = { total: 1, limit: 50, offset: 0, next_cursor: null };
  currentTasks = [liveTask];
  renderTasks(currentTasks, cardContext);
  const title = rowFor(fixture.id).querySelector('button.title');
  clickTitle(title);
  await settleReads();

  const read = liveRunReads.findLast(url => url.searchParams.get('task') === fixture.id);
  assert.ok(read, `${fixture.label} reads its readiness: ${liveRunReads.map(url => url.href).join(', ')}`);
  assert.equal(read.searchParams.get('workspace'), 'ws_owner', `${fixture.label} readiness request uses its owner`);
  const link = readinessCard(fixture.id).querySelector('.readiness-drain a');
  assert.ok(link, `${fixture.label} links to its live run`);
  const href = new URL(link.href, 'http://dashboard.test');
  assert.equal(href.searchParams.get('host'), 'box-a', `${fixture.label} run link keeps its host`);
  assert.equal(href.searchParams.get('workspace'), 'ws_owner', `${fixture.label} run link uses the task owner`);
  assert.equal(href.searchParams.get('window'), '7d', `${fixture.label} run link keeps its time window`);
  assert.equal(href.hash, '#runs?run_id=jrun-owner', `${fixture.label} run link opens its live run`);
}
common.setWorkspace('ws_orbit');
common.setWindow('24h');

drainWaits.setDrainReadiness({ tasks: [] });

// ORB-15380: pinned readiness invalidation follows its rendered answer even
// outside the active filter, and a draft keeps its controls while the answer
// is refreshed in place.
const pinnedVisibleTask = task(152);
const pinnedContext = {
  ...taskContext,
  getActiveStatuses: () => new Set(['in-progress']),
  statusOrder: ['in-progress'],
};
const paintPinned = () => {
  served = { total: 1, limit: 50, offset: 0, next_cursor: null };
  currentTasks = [pinnedVisibleTask];
  renderTasks(currentTasks, pinnedContext);
};
const pinnedWrap = () => paneBody.querySelector('.pinned-task-wrap');
const pinnedReadiness = () => pinnedWrap().querySelector('.readiness-list');
const pinnedCardHead = () => pinnedReadiness().querySelector('.readiness-drain-head').children.slice(0, 2).map(node => node.textContent);
const pinnedReads = [];
context.fetch = path => {
  const url = new URL(path, 'http://dashboard.test');
  assert.equal(url.pathname, '/api/workflows/auto/readiness', `the pinned detail reads drain readiness: ${path}`);
  const id = url.searchParams.get('task');
  pinnedReads.push(id);
  return heldReads.get(id).promise;
};

const pinnedSuccessTask = task(150, { status: 'backlog', workspace_id: 'ws_orbit', readiness: readyGaps });
const pinnedSuccessEntry = { task_id: 'ORB-150', status: 'backlog', eligible: true, reason: 'ready' };
heldReads.set('ORB-150', holdRead());
const pinnedSuccessBefore = JSON.stringify(pinnedSuccessTask);
setPinnedExternalTask(pinnedSuccessTask, pinnedContext);
paintPinned();
const pendingSuccessWrap = pinnedWrap();
assert.ok(pinnedReadiness().textContent.includes('Checking whether the drain can take this task'), 'a pinned detail shows pending readiness');
heldReads.get('ORB-150').resolve(readinessResponse({ tasks: [pinnedSuccessEntry] }));
heldReads.delete('ORB-150');
await settleReads();
assert.notEqual(pinnedWrap(), pendingSuccessWrap, 'a settled pinned answer invalidates its wrapper');
assert.deepEqual(pinnedCardHead(), ['', 'Eligible now'], 'a pinned proposed or backlog task shows its completed answer');
assert.ok(pinnedReadiness().textContent.includes('Ready: nothing is missing.'));
assert.equal(JSON.stringify(pinnedSuccessTask), pinnedSuccessBefore, 'the readiness repaint needs no task field change');
assert.deepEqual(pinnedReads, ['ORB-150'], 'settling readiness does not issue another per-task read');
clearPinnedExternalTask(pinnedContext);

const pinnedFailureTask = task(151, { status: 'backlog', workspace_id: 'ws_orbit', readiness: readyGaps });
heldReads.set('ORB-151', holdRead());
setPinnedExternalTask(pinnedFailureTask, pinnedContext);
paintPinned();
const pendingFailureWrap = pinnedWrap();
assert.ok(pinnedReadiness().textContent.includes('Checking whether the drain can take this task'));
heldReads.get('ORB-151').resolve(readinessResponse({ error: 'readiness offline' }, false));
heldReads.delete('ORB-151');
await settleReads();
assert.notEqual(pinnedWrap(), pendingFailureWrap, 'a failed pinned answer invalidates its wrapper');
assert.ok(pinnedReadiness().textContent.includes('Drain readiness unknown: readiness offline.'), pinnedReadiness().textContent);
const commentButton = pinnedWrap().querySelector('.action.comment');
commentButton.dispatch('click');
const draftWrap = pinnedWrap();
const draftTextarea = draftWrap.querySelector('.comment-form textarea');
assert.ok(draftTextarea, 'the pinned detail opens its comment draft control');
draftTextarea.value = 'keep this unsent draft';
const readinessRepaints = [];
const stopPinnedReadiness = drainWaits.onDrainReadinessChange(() => {
  readinessRepaints.push('changed');
  paintPinned();
});
const pinnedLock = { task_id: 'ORB-151', status: 'backlog', eligible: false, reason: 'context_lock_conflict', conflicts: [{ requested_file: 'file:src/locked.rs', locking_task_id: 'ORB-88' }] };
drainWaits.setDrainReadiness({ tasks: [pinnedLock] });
assert.equal(readinessRepaints.length, 1, 'a fallback snapshot repaints a pinned detail');
assert.equal(pinnedWrap(), draftWrap, 'fallback repaint preserves the pinned wrapper holding a draft');
assert.equal(pinnedWrap().querySelector('.comment-form textarea'), draftTextarea, 'fallback repaint preserves the live draft control');
assert.equal(draftTextarea.value, 'keep this unsent draft', 'fallback repaint preserves the unsent text');
assert.deepEqual(pinnedCardHead(), ['', 'Waiting on locks'], 'the failed read now shows the known snapshot state');
const pinnedCapacity = { task_id: 'ORB-151', status: 'backlog', eligible: false, reason: 'capacity_saturated', active_run_ids: ['jrun-7'] };
drainWaits.setDrainReadiness({ tasks: [pinnedCapacity] });
assert.equal(readinessRepaints.length, 2, 'a changed fallback answer repaints again');
assert.equal(pinnedWrap(), draftWrap, 'a second fallback repaint still preserves the draft wrapper');
assert.equal(pinnedWrap().querySelector('.comment-form textarea'), draftTextarea, 'a second fallback repaint keeps the same control');
assert.equal(draftTextarea.value, 'keep this unsent draft');
assert.deepEqual(pinnedCardHead(), ['', 'Waiting on capacity'], 'the card shows the current fallback snapshot');
stopPinnedReadiness();
drainWaits.setDrainReadiness({ tasks: [] });
clearPinnedExternalTask(pinnedContext);

// Ordinary expanded drafts refresh only the Readiness card when their own
// readiness read settles or a failed read falls back to a changed snapshot.
const paintOrdinary = ordinaryTask => {
  served = { total: 1, limit: 50, offset: 0, next_cursor: null };
  currentTasks = [ordinaryTask];
  renderTasks(currentTasks, cardContext);
};
const openOrdinaryDraft = ordinaryTask => {
  paintOrdinary(ordinaryTask);
  const title = rowFor(ordinaryTask.id).querySelector('button.title');
  clickTitle(title);
  const detail = paneBody.querySelector(`#detail-${ordinaryTask.id}`);
  const readiness = readinessCard(ordinaryTask.id);
  assert.ok(readiness.textContent.includes('Checking whether the drain can take this task'), 'an ordinary detail shows its held readiness read');
  detail.querySelector('.action.comment').dispatch('click');
  const textarea = detail.querySelector('.comment-form textarea');
  assert.ok(textarea, 'an ordinary detail opens its comment draft control');
  textarea.value = 'keep this ordinary draft';
  textarea.focus();
  return { detail, readiness, textarea };
};

const ordinarySuccessTask = task(154, { status: 'backlog', readiness: readyGaps });
const ordinarySuccessEntry = { task_id: 'ORB-154', status: 'backlog', eligible: true, reason: 'ready' };
heldReads.set('ORB-154', holdRead());
const ordinarySuccessDraft = openOrdinaryDraft(ordinarySuccessTask);
heldReads.get('ORB-154').resolve(readinessResponse({ tasks: [ordinarySuccessEntry] }));
heldReads.delete('ORB-154');
await settleReads();
assert.notEqual(readinessCard('ORB-154'), ordinarySuccessDraft.readiness, 'a settled ordinary answer replaces its readiness list');
assert.equal(readinessCard('ORB-154').textContent.includes('Ready: nothing is missing.'), true, 'an ordinary draft shows the successful answer');
assert.equal(paneBody.querySelector('#detail-ORB-154'), ordinarySuccessDraft.detail, 'success repaint preserves the ordinary detail node');
assert.equal(ordinarySuccessDraft.detail.querySelector('.comment-form textarea'), ordinarySuccessDraft.textarea, 'success repaint preserves the draft textarea');
assert.equal(ordinarySuccessDraft.textarea.value, 'keep this ordinary draft', 'success repaint preserves unsent text');
assert.equal(document.activeElement, ordinarySuccessDraft.textarea, 'success repaint preserves textarea focus');
assert.equal(pinnedReads.filter(id => id === 'ORB-154').length, 1, 'success repaint does not issue another readiness request');

const ordinaryFailureTask = task(155, { status: 'backlog', readiness: readyGaps });
heldReads.set('ORB-155', holdRead());
const ordinaryFailureDraft = openOrdinaryDraft(ordinaryFailureTask);
heldReads.get('ORB-155').resolve(readinessResponse({ error: 'readiness offline' }, false));
heldReads.delete('ORB-155');
await settleReads();
assert.notEqual(readinessCard('ORB-155'), ordinaryFailureDraft.readiness, 'a failed ordinary answer replaces its readiness list');
assert.ok(readinessCard('ORB-155').textContent.includes('Drain readiness unknown: readiness offline.'));
assert.equal(paneBody.querySelector('#detail-ORB-155'), ordinaryFailureDraft.detail, 'error repaint preserves the ordinary detail node');
assert.equal(ordinaryFailureDraft.detail.querySelector('.comment-form textarea'), ordinaryFailureDraft.textarea, 'error repaint preserves the draft textarea');
assert.equal(ordinaryFailureDraft.textarea.value, 'keep this ordinary draft', 'error repaint preserves unsent text');
assert.equal(document.activeElement, ordinaryFailureDraft.textarea, 'error repaint preserves textarea focus');

let ordinaryReadinessRepaints = 0;
const stopOrdinaryReadiness = drainWaits.onDrainReadinessChange(() => {
  ordinaryReadinessRepaints += 1;
  paintOrdinary(ordinaryFailureTask);
});
const ordinaryLock = { task_id: 'ORB-155', status: 'backlog', eligible: false, reason: 'context_lock_conflict', conflicts: [{ requested_file: 'file:src/locked.rs', locking_task_id: 'ORB-88' }] };
drainWaits.setDrainReadiness({ tasks: [ordinaryLock] });
assert.equal(ordinaryReadinessRepaints, 1, 'a fallback snapshot repaints an ordinary draft');
assert.equal(paneBody.querySelector('#detail-ORB-155'), ordinaryFailureDraft.detail, 'fallback repaint preserves the ordinary detail node');
assert.equal(ordinaryFailureDraft.detail.querySelector('.comment-form textarea'), ordinaryFailureDraft.textarea, 'fallback repaint preserves the draft textarea');
assert.equal(ordinaryFailureDraft.textarea.value, 'keep this ordinary draft', 'fallback repaint preserves unsent text');
assert.equal(document.activeElement, ordinaryFailureDraft.textarea, 'fallback repaint preserves textarea focus');
assert.deepEqual(cardHead('ORB-155'), ['', 'Waiting on locks'], 'an ordinary draft shows the fallback lock snapshot');
// A run blocker can change while the task holders stay identical. Both the
// snapshot notification and the draft's fallback card must follow that change.
for (const runId of ['jrun-blocker-first', 'jrun-blocker-second']) {
  drainWaits.setDrainReadiness({ tasks: [{ ...ordinaryLock, run_ids: [runId] }] });
  assert.deepEqual(readinessCard('ORB-155').querySelectorAll('.readiness-drain a').map(link => link.textContent), ['ORB-88', runId], 'refresh links every task and the current run blocker');
  assert.equal(ordinaryFailureDraft.textarea.value, 'keep this ordinary draft');
}
const ordinaryCapacity = { task_id: 'ORB-155', status: 'backlog', eligible: false, reason: 'capacity_saturated', active_run_ids: ['jrun-8'] };
drainWaits.setDrainReadiness({ tasks: [ordinaryCapacity] });
assert.equal(ordinaryReadinessRepaints, 4, 'changed run blockers and capacity each repaint the fallback');
assert.deepEqual(cardHead('ORB-155'), ['', 'Waiting on capacity'], 'the ordinary draft shows the current fallback snapshot');
assert.equal(ordinaryFailureDraft.detail.querySelector('.comment-form textarea'), ordinaryFailureDraft.textarea, 'a second fallback repaint retains the same textarea');
assert.equal(ordinaryFailureDraft.textarea.value, 'keep this ordinary draft');
assert.equal(document.activeElement, ordinaryFailureDraft.textarea);
assert.equal(pinnedReads.filter(id => id === 'ORB-155').length, 1, 'error and fallback repaints do not issue another readiness request');
stopOrdinaryReadiness();
drainWaits.setDrainReadiness({ tasks: [] });

context.fetch = () => new Promise(() => {});

// Review approval runs the shipped row handler and request client. Hold the
// response to observe pending state without timers or an optimistic status edit.
const approvalStatuses = ['proposed', 'review', 'backlog', 'in-progress', 'blocked', 'someday', 'done', 'rejected', 'archived'];
let refreshes = 0;
let refreshedStatus = 'done';
const approvalContext = {
  ...taskContext,
  getActiveStatuses: () => new Set(approvalStatuses),
  statusOrder: approvalStatuses,
  refreshDashboard: async () => {
    refreshes += 1;
    currentTasks = currentTasks.map(item => ({ ...item, status: refreshedStatus }));
    renderTasks(currentTasks, approvalContext);
  },
};
const paintApprovals = items => {
  served = { total: items.length, limit: 50, offset: 0, next_cursor: null };
  currentTasks = items;
  renderTasks(items, approvalContext);
};
const approveButton = id => rowFor(id).querySelector('.task-quick.approve');
const response = (body, ok = true) => ({ ok, status: ok ? 200 : 409, text: async () => JSON.stringify(body), json: async () => body });
const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};
const settleActions = () => new Promise(setImmediate);
const requests = [];
let claims = [];
let nextApproval = null;
context.fetch = (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  if (options.method === 'POST') {
    requests.push({ path: url.pathname, workspace: url.searchParams.get('workspace'), body: JSON.parse(options.body) });
    assert.ok(nextApproval, 'the fixture provides the approval response');
    return nextApproval.promise;
  }
  assert.equal(url.pathname, '/api/distributed/claims', 'review admission reads claim state once on click');
  return Promise.resolve(response({ claims }));
};
const prRef = [{ system: 'github-pr', id: '123', url: 'https://github.com/example/orbit/pull/123' }];
paintApprovals([
  task(81, { status: 'review' }),
  task(82, { status: 'review', external_refs: prRef }),
  task(83, { status: 'proposed' }),
  ...approvalStatuses.filter(status => !['review', 'proposed'].includes(status)).map((status, index) => task(90 + index, { status })),
]);
assert.equal(approveButton('ORB-81').textContent, 'Approve', 'review without a PR has an approval');
assert.ok(approveButton('ORB-81').title.includes('done'), 'review approval explains the destination');
assert.equal(approveButton('ORB-82'), null, 'review with a PR omits the row approval');
const prLink = rowFor('ORB-82').querySelector('.task-quick-link');
assert.equal(prLink.href, prRef[0].url, 'review keeps the PR link');
assert.equal(prLink.textContent, 'PR #123');
assert.equal(rowFor('ORB-82').querySelector('.task-quick-cell').children.length, 1, 'the linked review action cell contains only the PR link');
assert.ok(!rowFor('ORB-82').classList.contains('no-quick'), 'the PR link keeps the review action column');
assert.equal(approveButton('ORB-83').textContent, 'Approve', 'proposed keeps its approval');
assert.ok(approveButton('ORB-83').title.includes('backlog'));
for (const item of currentTasks.filter(item => !['review', 'proposed'].includes(item.status))) {
  assert.equal(approveButton(item.id), null, `${item.status} gains no approval`);
}
assert.equal(rowFor('ORB-90').querySelector('.task-quick.ship').textContent, 'Ship', 'backlog keeps Ship');
paintApprovals([task(81, { status: 'review' })]);
assert.ok(approveButton('ORB-81'), 'a review-only list retains its action column');
nextApproval = deferred();
approveButton('ORB-81').dispatch('click');
assert.equal(approveButton('ORB-81').textContent, 'Approving…');
assert.equal(approveButton('ORB-81').disabled, true, 'pending approval is disabled');
await settleActions();
assert.deepEqual(requests, [{ path: '/api/tasks/ORB-81/approve', workspace: 'ws_orbit', body: {} }]);
assert.equal(currentTasks[0].status, 'review', 'pending approval keeps the task in review');
assert.equal(refreshes, 0, 'the list waits for API confirmation');
approveButton('ORB-81').dispatch('click');
await settleActions();
assert.equal(requests.length, 1, 'a pending action cannot submit twice');
nextApproval.resolve(response({}));
await settleActions();
assert.equal(refreshes, 1, 'accepted approval refreshes the list');
assert.equal(currentTasks[0].status, 'done');
assert.ok(byId['tasks-body'].querySelectorAll('.group-label').some(label => label.textContent === 'Done'), 'the refreshed task appears under Done');
assert.equal(approveButton('ORB-81'), null, 'Done offers no approval');
assert.ok(byId['tasks-body'].querySelector('.task-action-notice').textContent.includes('done'), 'the notice reports the review destination');

paintApprovals([task(81, { status: 'review' })]);
nextApproval = deferred();
approveButton('ORB-81').dispatch('click');
await settleActions();
nextApproval.resolve(response({ error: 'Review is not ready to approve' }, false));
await settleActions();
assert.equal(rowFor('ORB-81').querySelector('.task-quick-error').textContent, 'approve failed: Review is not ready to approve', 'a review without a PR shows the server refusal inline');
assert.equal(currentTasks[0].status, 'review', 'refusal leaves the task in review');
assert.equal(refreshes, 1, 'refusal does not refresh or move the task');
assert.equal(approveButton('ORB-81').disabled, false, 'a refused approval can be retried');

// Both approval statuses use the same existing mutation refusal text.
const assertDisabledApprovals = reason => {
  paintApprovals([task(84, { status: 'review' }), task(85, { status: 'proposed' })]);
  for (const id of ['ORB-84', 'ORB-85']) {
    assert.equal(approveButton(id).disabled, true);
    assert.ok(approveButton(id).title.includes(reason), 'disabled approval names the shared refusal');
    approveButton(id).dispatch('click');
  }
};
const priorRequests = requests.length;
common.setWorkspace(null);
assertDisabledApprovals('select a specific workspace');
paintApprovals([
  task(84, { status: 'review', workspace_id: 'ws_orbit' }),
  task(85, { status: 'proposed', workspace_id: 'ws_orbit' }),
]);
assert.equal(approveButton('ORB-84').disabled, true, 'qualified review rows also require a selected workspace');
assert.ok(approveButton('ORB-84').title.includes('select a specific workspace'));
approveButton('ORB-84').dispatch('click');
assert.equal(approveButton('ORB-85').disabled, false, 'qualified proposed approvals keep their existing behavior');
await settleActions();
assert.equal(requests.length, priorRequests, 'unqualified aggregate approvals send no write');
common.setWorkspace('ws_orbit');
common.setHost('dk-server-2');
common.setHostWrites({ authorized: false, reason: 'Operator authorization is required' });
byId['tasks-body'].replaceChildren(); // The dashboard clears panels on a host switch.
assertDisabledApprovals('Read-only on dk-server-2: Operator authorization is required');
await settleActions();
assert.equal(requests.length, priorRequests, 'read-only approvals send no write');
common.setHost(null);
common.setWorkspace('ws_orbit');

// A handed-off review uses the same exact-candidate approval as the detail.
refreshedStatus = 'review';
claims = [{ task_id: 'ORB-86', phase: 'handed_off', handoff: {
  handoff_id: 'handoff-86', authority: { state: 'not_authorized' },
  candidate: { candidate: { commit: 'candidate-86' }, base: { commit: 'base-86' } },
} }];
paintApprovals([task(86, { status: 'review' })]);
nextApproval = deferred();
approveButton('ORB-86').dispatch('click');
await settleActions();
const handoffRequest = requests.at(-1);
assert.equal(handoffRequest.path, '/api/distributed/handoffs/handoff-86/approve');
assert.equal(handoffRequest.workspace, 'ws_orbit');
assert.equal(handoffRequest.body.expected_candidate_commit, 'candidate-86');
assert.equal(handoffRequest.body.expected_base_commit, 'base-86');
assert.ok(handoffRequest.body.request_id, 'handoff approval has a replay identity');
nextApproval.resolve(response({}));
await settleActions();
assert.equal(currentTasks[0].status, 'review', 'handoff approval waits for the owner landing job');
assert.ok(byId['tasks-body'].querySelector('.task-action-notice').textContent.includes('owner landing job'), 'handoff notice does not claim it completed');
claims[0].handoff.authority = { state: 'authorized', summary: 'Landing is already authorized' };
paintApprovals([task(86, { status: 'review' })]);
const beforeClaimRefusal = requests.length;
approveButton('ORB-86').dispatch('click');
await settleActions();
assert.equal(requests.length, beforeClaimRefusal, 'claim refusal sends no status write');
assert.ok(rowFor('ORB-86').querySelector('.task-quick-error').textContent.includes('Landing is already authorized'), 'claim refusal is inline on the row');
claims = [];

// Admission must not approve in a different workspace after an asynchronous read.
const admission = deferred();
context.fetch = () => admission.promise;
paintApprovals([task(87, { status: 'review' })]);
approveButton('ORB-87').dispatch('click');
common.setWorkspace('ws_other');
common.setWorkspace('ws_orbit');
admission.resolve(response({ claims: [] }));
await settleActions();
assert.equal(requests.length, beforeClaimRefusal, 'a stale workspace visit starts no approval');
context.fetch = () => new Promise(() => {});

// A task link names only the task: it opens under the operator's own filter
// instead of replacing it, and the open task is what a copied URL carries.
let statuses = new Set(['review']);
let search = 'zzz';
const hashContext = {
  getActiveStatuses: () => statuses,
  setActiveStatuses: value => { statuses = value; },
  getSearchQuery: () => search,
  setSearchQuery: value => { search = value; },
  statusOrder: ['in-progress', 'review', 'done'],
  defaultActiveStatuses: ['in-progress', 'review'],
};
applyTasksHashQuery(new URLSearchParams('open=ORB-9'), hashContext);
assert.deepEqual([...statuses], ['review'], 'a task link keeps the operator\'s status chips');
assert.equal(search, 'zzz', 'a task link keeps the operator\'s search');
assert.equal(buildTasksHash(hashContext), '#tasks?status=review&q=zzz&open=ORB-9', 'the normalized hash carries the filter and the open task');
applyTasksHashQuery(new URLSearchParams('status=done&open=ORB-9'), hashContext);
assert.deepEqual([...statuses], ['done'], 'a hash that names a filter still sets it');
assert.equal(search, '', 'a hash that names a status resets the omitted search');
assert.equal(buildTasksHash(hashContext), '#tasks?status=done&open=ORB-9');
applyTasksHashQuery(new URLSearchParams('status=done&open=%3Cscript%3E'), hashContext);
assert.equal(buildTasksHash(hashContext), '#tasks?status=done', 'a malformed task id is ignored');
applyTasksHashQuery(new URLSearchParams('status=done'), hashContext);
assert.equal(buildTasksHash(hashContext), '#tasks?status=done', 'a hash without open names no task');
// `#tasks/<id>` is that task's full page; it names only the task, so the
// filter behind it is kept for the way back.
applyTasksHashQuery(new URLSearchParams(), hashContext, 'ORB-9');
assert.equal(buildTasksHash(hashContext), '#tasks/ORB-9', 'a task page route is its own hash');
assert.deepEqual([...statuses], ['done'], 'a task page keeps the operator\'s status chips');
applyTasksHashQuery(new URLSearchParams('status=done&open=ORB-9'), hashContext);
assert.equal(buildTasksHash(hashContext), '#tasks?status=done&open=ORB-9', 'leaving the page returns to the list at the same task');
applyTasksHashQuery(new URLSearchParams('status=done'), hashContext, '<script>');
assert.equal(buildTasksHash(hashContext), '#tasks?status=done', 'a malformed page id falls back to the list');

// The next render selects a named task the list holds, and the pane shows it.
const paneHost = byId['task-detail-pane'];
applyTasksHashQuery(new URLSearchParams('open=ORB-7'), taskContext);
paint({ total: 3, limit: 50, offset: 0, next_cursor: null }, [task(6), task(7), task(8, { history: [{ event: 'updated', by: 'operator', at: '2026-10-18T10:00:00Z' }] })]);
const selectedRows = () => byId['tasks-body'].querySelectorAll('.row.expanded').map(row => row.dataset.key);
assert.deepEqual(selectedRows(), ['task-ORB-7'], 'only the task the link names is selected');
assert.equal(paneHost.hidden, false, 'a selection opens the pane');
assert.deepEqual(paneBody.children.map(node => node.dataset.key), ['pane-head-ORB-7', 'detail-ORB-7'], 'the pane holds the selected task alone');

// Moving the selection rebuilds no list row: the list keeps its nodes, so it
// neither reflows nor loses its scroll, and only the pane changes.
const listNodes = () => byId['tasks-body'].children.slice();
const before = listNodes();
window.location.hash = '#tasks?status=in-progress&open=ORB-7';
window.history = { replaceState: (_state, _title, url) => { window.location.hash = url; } };
const titleOf = id => rowFor(id).querySelector('button.title');
clickTitle(titleOf('ORB-8'));
assert.deepEqual(selectedRows(), ['task-ORB-8'], 'clicking another row moves the selection');
assert.ok(listNodes().every((node, index) => node === before[index]) && listNodes().length === before.length, 'selection keeps every list node in place');
assert.deepEqual(paneBody.children.map(node => node.dataset.key), ['pane-head-ORB-8', 'detail-ORB-8']);
assert.equal(titleOf('ORB-8').getAttribute('aria-expanded'), 'true');
assert.equal(titleOf('ORB-7').getAttribute('aria-expanded'), 'false', 'the previous row reads as unselected');
assert.equal(window.location.hash, '#tasks?status=in-progress&open=ORB-8', 'the URL names the selected task, so a copied link reopens it');
paint({ total: 3, limit: 50, offset: 0, next_cursor: null }, currentTasks.map(item => ({ ...item })));
assert.ok(listNodes().every((node, index) => node === before[index]), 'a refresh with unchanged rows keeps the list nodes too');
assert.deepEqual(selectedRows(), ['task-ORB-8'], 'a refresh keeps the selection');

// Tabs: Overview first; the chosen tab carries over to the next selection.
const tab = name => paneBody.querySelector(`.detail-tabs`).children.find(button => button.dataset.tab === name);
const shownPanel = () => paneBody.querySelectorAll('.detail-tabpanel').filter(panel => !panel.hidden).map(panel => panel.dataset.tabPanel);
assert.deepEqual(paneBody.querySelector('.detail-tabs').children.map(button => button.dataset.tab), ['overview', 'history', 'runs', 'artifacts', 'comments']);
assert.deepEqual(shownPanel(), ['overview']);
tab('history').dispatch('click');
assert.deepEqual(shownPanel(), ['history'], 'a tab shows its own panel only');
assert.equal(tab('history').getAttribute('aria-selected'), 'true');
assert.ok(paneBody.querySelector('.history-line').textContent.includes('operator: updated'), 'History lists the task history');
const runReads = [];
context.fetch = path => {
  const url = new URL(path, 'http://dashboard.test');
  runReads.push(url);
  return Promise.resolve(readinessResponse({ items: [{ run_id: 'jrun-tab-1', job_id: 'ship', state: 'failed', started_at: '2026-10-18T09:00:00Z', duration_ms: 1200, error_message: 'gate failed' }], total: 3 }));
};
tab('runs').dispatch('click');
await settleReads();
assert.equal(runReads.length, 1, 'the Runs tab reads the runs once');
assert.equal(runReads[0].pathname, '/api/job-runs');
assert.equal(runReads[0].searchParams.get('task_id'), 'ORB-8', 'the read is filtered to the selected task');
const runLink = paneBody.querySelector('.task-run a.task-run-id');
assert.equal(runLink.textContent, 'jrun-tab-1');
assert.ok(runLink.href.includes('#runs?run_id=jrun-tab-1'), 'each run links to its run detail');
assert.ok(paneBody.querySelector('.task-run-error').textContent.includes('gate failed'));
assert.ok(paneBody.querySelector('.task-runs-more').textContent.includes('1 of 3'), 'a partial list says so');
clickTitle(titleOf('ORB-6'));
assert.deepEqual(shownPanel(), ['runs'], 'the chosen tab stays chosen for the next task');
await settleReads();
assert.equal(runReads.at(-1).searchParams.get('task_id'), 'ORB-6', 'and reads that task\'s runs');
tab('overview').dispatch('click');
context.fetch = () => new Promise(() => {});

// The full page: `#tasks/<id>` shows the pane across the width, with Back in
// place of the close and full-page controls; Back returns to the list with
// the same task selected.
applyTasksHashQuery(new URLSearchParams(), taskContext, 'ORB-7');
paint({ total: 3, limit: 50, offset: 0, next_cursor: null }, currentTasks);
assert.ok(byId['tasks-main'].classList.contains('task-page-mode'), 'the page route switches the layout to the full page');
assert.deepEqual(paneBody.children.map(node => node.dataset.key), ['pane-head-ORB-7', 'detail-ORB-7']);
const pageHead = paneBody.querySelector('.task-pane-head');
assert.equal(pageHead.querySelector('.task-pane-full'), null, 'a full page offers no full-page control');
const backToList = pageHead.querySelector('.task-pane-back');
assert.equal(backToList.textContent, 'Back to list');
backToList.dispatch('click');
assert.ok(!byId['tasks-main'].classList.contains('task-page-mode'), 'Back leaves the full page');
assert.deepEqual(selectedRows(), ['task-ORB-7'], 'the list comes back with the same task selected');
assert.ok(window.location.hash.startsWith('#tasks?') && window.location.hash.endsWith('open=ORB-7'), `a page reached by link goes back to the list naming the task: ${window.location.hash}`);

// Closing the pane from its own control clears the selection and the URL.
paneBody.querySelector('.task-pane-close').dispatch('click');
assert.equal(paneHost.hidden, true);
assert.deepEqual(selectedRows(), []);
assert.ok(!window.location.hash.includes('open='), 'a closed pane names no task');
delete window.history;
