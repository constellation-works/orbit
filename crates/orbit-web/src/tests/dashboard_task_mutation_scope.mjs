import assert from 'node:assert/strict';
const { setWorkspace } = await import('./js/common.js');
const { renderTasks, cacheCrewPayload } = await import('./js/tasks.js');
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
const flush = async () => { for (let i = 0; i < 5; i++) await tick(); };
const response = (payload, status = 200) => ({ ok: status === 200, status, json: async () => payload, text: async () => JSON.stringify(payload) });
const full = workspace => ({ id: 'ORB-1', title: `${workspace} task`, status: 'review', complexity: 'low', description: 'Original task body', artifacts: [], status_transitions: [{ status: 'backlog', required_field: null }] });
let tasks = [];
const writes = [];
let finishRead;
let finishWrite;
const context = {
  getTasks: () => tasks,
  replaceTask: task => { tasks = tasks.map(existing => existing.id === task.id ? task : existing); },
  getSearchQuery: () => '',
  getActiveStatuses: () => new Set(['review','backlog']),
  statusOrder: ['review','backlog'],
  fmtAbsTime: value => value,
  refreshDashboard: () => Promise.resolve(),
};
globalThis.fetch = async (path, options = {}) => {
  if (options.method === 'PATCH') {
    writes.push(new URL(path, 'http://dashboard.test'));
    return new Promise(resolve => { finishWrite = resolve; });
  }
  if (String(path).includes('/api/distributed/claims')) return response({ claims: [], capabilities: {} });
  return new Promise(resolve => { finishRead = resolve; });
};
const body = () => document.getElementById('tasks-body');
const statusSelect = () => body().children.find(node => node.dataset.key === 'task-ORB-1').querySelector('select.task-status-select');
const show = (workspace, task = full(workspace)) => { setWorkspace(workspace); tasks = [task]; renderTasks(tasks, context); };
const changeStatus = () => { const select = statusSelect(); select.value = 'backlog'; select.dispatch('change'); };

// Specific-workspace summary rows omit workspace_id. After fetching their
// transition requirements, a stale interaction must not write in the new scope.
show('A', { ...full('A'), projection: 'summary', status_transitions: [{ status: 'backlog' }] });
changeStatus();
assert.ok(finishRead, 'the summary transition reads its detailed requirement first');
show('B');
finishRead(response(full('A')));
await flush();
assert.equal(writes.length, 0, 'leaving A during a prerequisite read must not PATCH B');
assert.equal(tasks[0].title, 'B task');

// A completed request can still settle after a scope switch. Its response and
// success feedback must never replace or modify the current workspace's task.
show('A');
changeStatus();
assert.equal(writes.at(-1).searchParams.get('workspace'), 'A');
show('B');
finishWrite(response({ ...full('A'), status: 'backlog' }));
await flush();
assert.equal(tasks[0].title, 'B task', 'a late mutation response must not replace B with A');
assert.equal(tasks[0].status, 'review');
assert.ok(!body().textContent.includes('status saved'), 'a late mutation must not show A success feedback in B');

// Returning to the same workspace is a new visit, so an earlier write must
// not overwrite the task fetched during the return visit.
show('A');
changeStatus();
show('B');
show('A', { ...full('A'), title: 'A fresh visit' });
finishWrite(response({ ...full('A'), status: 'backlog' }));
await flush();
assert.equal(tasks[0].title, 'A fresh visit', 'A→B→A discards the first visit mutation response');

// Refusals from an earlier scope do not become errors on the new task either.
show('B');
show('A');
changeStatus();
show('B');
finishWrite(response({ error: 'A status refused' }, 500));
await flush();
assert.ok(!body().textContent.includes('A status refused'));

cacheCrewPayload({ default_crew: 'one', crews: [{ name: 'one' }, { name: 'two' }] });
show('A');
const crew = body().querySelector('select.task-crew-select');
crew.value = 'two';
crew.dispatch('change');
show('B');
finishWrite(response({ ...full('A'), crew: 'two' }));
await flush();
assert.equal(tasks[0].title, 'B task', 'a late crew edit cannot replace B');
assert.ok(!body().textContent.includes('crew saved'));

show('A');
body().children.find(node => node.dataset.key === 'task-ORB-1').dispatch('click');
const complexity = body().querySelector('select.task-complexity-select');
complexity.value = 'medium';
complexity.dispatch('change');
show('B');
finishWrite(response({ ...full('A'), complexity: 'medium' }));
await flush();
assert.equal(tasks[0].title, 'B task', 'a late complexity edit cannot replace B');
assert.equal(tasks[0].complexity, 'low');
assert.ok(!body().textContent.includes('complexity saved'));

show('A');
body().children.find(node => node.dataset.key === 'task-ORB-1').dispatch('click');
const descendants = node => [node, ...(node.children || []).flatMap(descendants)];
const descriptionBlock = descendants(body()).find(node => String(node.className).split(/\s+/).includes('field-block') && descendants(node.children[0]).some(child => child.className === 'field-title' && child.textContent === 'description'));
const edit = descendants(descriptionBlock).find(node => node.className === 'field-edit');
edit.dispatch('click');
const input = descendants(descriptionBlock).find(node => node.className === 'field-editor-input mono');
input.value = 'A rewritten body';
descendants(descriptionBlock).find(node => node.className === 'action save').dispatch('click');
show('B');
finishWrite(response({ ...full('A'), description: 'A rewritten body' }));
await flush();
assert.equal(tasks[0].title, 'B task', 'a late text-field save cannot replace B');
assert.equal(tasks[0].description, 'Original task body');
