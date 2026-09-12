// Runs against shipped modules in both the Node DOM harness and Chromium.
const { setWorkspace } = await import('./common.js');
const { initOperations, fetchAndRenderOperations } = await import('./operations.js');
const get = id => document.getElementById(id);
const descendants = node => [node, ...Array.from(node.children || []).flatMap(descendants)];
const button = (id, label) => descendants(get(id)).find(node => node.textContent === label && node.type === 'button');
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
const allowed = { authorized: true, reason: null };
const denied = { authorized: false, reason: 'Test session cannot perform this action. Ask the server operator.' };
const capabilities = { routine_toggle: allowed, clock_service: allowed, clock_cadence: allowed, auto_task_toggle: allowed, auto_task_mint: allowed };
const enabled = { one: true, two: true };
let responseError = null;
let readbackError = false;
let releasePost = null;
let delayPost = false;
let delayGet = false;
let releaseGet = null;
let nextTask = 1;
const requests = [];
const confirmations = [];
window.confirm = message => { confirmations.push(message); return true; };
const response = (payload, status = 200) => ({ ok: status === 200, status, json: async () => payload, text: async () => JSON.stringify(payload) });
globalThis.fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  const workspace = url.searchParams.get('workspace');
  const body = options.body ? JSON.parse(options.body) : null;
  requests.push({ path: url.pathname, workspace, body });
  if (options.method === 'POST') {
    if (delayPost) await new Promise(resolve => { releasePost = resolve; });
    if (responseError) return response({ error: responseError }, 500);
    if (url.pathname.endsWith('/toggle')) enabled[workspace] = body.enabled;
    return response(url.pathname.endsWith('/mint')
      ? { message: `Minted TEST-${nextTask}`, task_id: `TEST-${nextTask++}` }
      : { message: 'Saved', clock: { enabled: true } });
  }
  if (url.pathname === '/api/auto-tasks') {
    if (readbackError) throw new Error('Fixture readback unavailable');
    const payload = { workspace, controls_authorized: capabilities.auto_task_toggle.authorized && capabilities.auto_task_mint.authorized, capabilities: { ...capabilities }, unconditional_mint_warning: "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.", definitions: [{ name: `Chore ${workspace}`, enabled: enabled[workspace], template: { title: 'Fixture chore' }, template_summary: 'Fixture chore', schedule_summary: 'every 15 minutes', description: 'Remediate CI failures for the selected workspace.', may_create_open_duplicate: true, open_duplicate: true, last_minted_task_id: 'ORB-00099', last_minted_task_status: 'backlog', last_evaluation: { kind: 'fired', last_task_id: 'ORB-00001', last_fired_at: '2026-09-07T20:00:00Z' }, next_evaluation: { state: 'scheduled', at: '2026-09-07T22:00:00Z' }, automation: { reason: 'covered', state: { consumer: `auto-task/${workspace}`, baseline: { commit: 'abc1234', tree: 'def5678' }, observed: { commit: 'abc1234', tree: 'def5678' }, covered: { commit: 'abc1234', tree: 'def5678' }, pending: [], pending_commits: [], waived: [], excluded: [], unresolved: {} } } }, { name: `Someday ${workspace}`, enabled: true, template: { title: 'Parked chore' }, template_summary: 'Parked chore', schedule_summary: 'every 60 minutes', description: 'Auto-task whose only instance is parked in someday.', dedupe: 'skip_if_open', may_create_open_duplicate: false, open_duplicate: false, last_minted_task_id: 'ORB-00100', last_minted_task_status: 'someday', last_evaluation: { kind: 'fired', last_task_id: 'ORB-00100', last_fired_at: '2026-09-07T20:00:00Z' }, next_evaluation: { state: 'scheduled', at: '2026-09-07T22:00:00Z' } }] };
    if (delayGet) await new Promise(resolve => { releaseGet = resolve; });
    return response(payload);
  }
  if (url.pathname === '/api/routines') return response({
    host_id: 'fixture-host', controls_authorized: capabilities.routine_toggle.authorized, capabilities: { ...capabilities }, session_explanation: 'Session access: restart the dashboard server with explicit operator authority.',
    routines: ['one', 'two'].map(source => ({ name: `Routine ${source}`, source, target: 'job:fixture', enabled: enabled[source], cron: '30 14 * * *', description: 'Sweep landed deliveries.', next_evaluation: { state: enabled[source] ? 'scheduled' : 'disabled', at: '2026-09-07T21:30:00Z', hypothetical: !enabled[source] } })),
    clock: { enabled: true, configured_cadence_seconds: 60, provider: 'fixture', health: 'healthy', loaded: true, running: true, schedulable: true, last_tick_at: '2026-09-07T21:00:00Z', next_tick_at: '2026-09-07T21:01:00Z' },
  });
  if (url.pathname === '/api/workflows/auto/readiness') return response({
    controls_authorized: true,
    capacity: { active_leaf_runs: 1, max_active_leaf_runs: 4, free_slots: 3 },
    tasks: [{ id: 'ORB-1', eligible: true }, { id: 'ORB-2', eligible: false }],
  });
  if (url.pathname === '/api/operation/explain') return response({
    controls_authorized: true,
    policy: { preset: { value: 'balanced', source: 'config' } },
    authority: { grant_id: null, admission: 'none', rights: [], task_ids: [] },
    delivery: { effective_completion: 'review' },
    limiting_reasons: [],
  });
  return response({});
};
setWorkspace('one');
initOperations({ getWorkspaces: () => ['one', 'two'].map(id => ({ id, name: id, status: 'active' })), formatAbsoluteTime: value => value });
await fetchAndRenderOperations();
assert(!button('auto-tasks-body', 'Disable').disabled, 'authorized toggle available');
assert(!button('auto-tasks-body', 'Mint now').disabled, 'authorized mint available');
assert(!button('clock-body', 'Pause clock').disabled, 'authorized clock available');

// The old aggregate authorization bit must not suppress an independently allowed action.
capabilities.auto_task_toggle = denied;
capabilities.clock_service = denied;
await fetchAndRenderOperations();
assert(button('auto-tasks-body', 'Disable').disabled, 'toggle denied independently');
assert(!button('auto-tasks-body', 'Mint now').disabled, 'partial capability preserves mint');
assert(button('clock-body', 'Pause clock').disabled, 'clock service denied independently');
const explanation = descendants(get('auto-tasks-body')).find(node => node.getAttribute?.('aria-label')?.includes(denied.reason));
assert(explanation?.tabIndex === 0, 'disabled action reason is keyboard accessible');
assert(!get('auto-tasks-body').textContent.includes('Ask the server operator'), 'no repeated per-card session warning');
capabilities.auto_task_mint = denied;
await fetchAndRenderOperations();
assert(button('auto-tasks-body', 'Mint now').disabled, 'read-only mint denied');
capabilities.auto_task_toggle = capabilities.auto_task_mint = capabilities.clock_service = allowed;
await fetchAndRenderOperations();

// Toggle both ways with server readback; a second event on the old button is ignored.
delayPost = true;
let action = button('auto-tasks-body', 'Disable');
action.click(); action.click();
await tick();
assert(requests.filter(r => r.path === '/api/auto-tasks/toggle').length === 1, 'toggle double click submits once');
assert(button('auto-tasks-body', 'Pending…').disabled, 'toggle pending is disabled');
releasePost(); await tick(); await tick();
assert(button('auto-tasks-body', 'Enable'), 'toggle readback changes rendered state');
delayPost = false;
button('auto-tasks-body', 'Enable').click(); await tick(); await tick();
assert(button('auto-tasks-body', 'Disable'), 'toggle can be re-enabled');

// Routine uses the same guarded path, including readback in both directions.
button('routines-body', 'Disable').click(); await tick(); await tick();
assert(button('routines-body', 'Enable'), 'routine disable reads back');
button('routines-body', 'Enable').click(); await tick(); await tick();
assert(button('routines-body', 'Disable'), 'routine enable reads back');

// Host controls post canonical action names and serialize service/cadence changes.
delayPost = true;
action = button('clock-body', 'Pause clock'); action.click(); action.click(); await tick();
assert(requests.filter(r => r.path === '/api/routines/clock').length === 1, 'clock double click submits once');
assert(requests.find(r => r.path === '/api/routines/clock').body.action === 'disable', 'clock service canonical action');
releasePost(); await tick(); await tick();
delayPost = false;
const cadence = descendants(get('clock-body')).find(node => node.title === 'Clock cadence');
cadence.value = '300'; cadence.dispatchEvent(new Event('change'));
button('clock-body', 'Apply cadence').click(); await tick(); await tick();
assert(requests.some(r => r.body?.action === 'set_cadence' && r.body.cadence_seconds === 300), 'cadence canonical request');

// Mint acknowledgement, duplicate warning, no dispatch, feedback and task link.
delayPost = true;
action = button('auto-tasks-body', 'Mint now');
action.click(); action.click(); await tick();
assert(requests.filter(r => r.path === '/api/auto-tasks/mint').length === 1, 'mint double click submits once');
assert(get('auto-task-operation-feedback').textContent.includes('Minting'), 'mint pending feedback');
assert(confirmations.at(-1).includes('An open instance already exists'), 'duplicate is clearly acknowledged');
releasePost(); await tick(); await tick();
const link = descendants(get('auto-task-operation-feedback')).find(node => String(node.href || '').includes('q=TEST-1'));
assert(link && String(link.href).includes('workspace=one'), 'mint links to task in originating workspace');
assert(requests.find(r => r.path.endsWith('/mint')).body.acknowledge_unconditional === true, 'mint acknowledges unconditional semantics');
assert(!requests.some(r => r.path.includes('/ship')), 'mint never dispatches');

// Error persists through a refresh and releases the pending guard.
delayPost = false; responseError = 'Fixture disk failure';
button('auto-tasks-body', 'Mint now').click(); await tick(); await tick();
await fetchAndRenderOperations();
assert(get('auto-task-operation-feedback').textContent.includes('Fixture disk failure'), 'mint failure stays inline');
assert(!button('auto-tasks-body', 'Mint now').disabled, 'failure permits intentional retry');
button('auto-tasks-body', 'Disable').click(); await tick(); await tick();
assert(get('auto-task-operation-feedback').textContent.includes('Fixture disk failure'), 'toggle failure stays inline');
button('routines-body', 'Disable').click(); await tick(); await tick();
assert(get('routine-operation-feedback').textContent.includes('Fixture disk failure'), 'routine error visible');
responseError = null;

// A successful mint must not be reported as failed when only its GET readback fails.
readbackError = true;
button('auto-tasks-body', 'Mint now').click(); await tick(); await tick();
assert(get('auto-task-operation-feedback').textContent.includes('The action succeeded'), 'readback failure preserves mutation success');
assert(descendants(get('auto-task-operation-feedback')).some(node => String(node.href || '').includes('#tasks?')), 'readback failure preserves created-task link');
readbackError = false; await fetchAndRenderOperations();

// Old mutation replies and old DOM controls cannot act on a new selection.
delayPost = true;
action = button('auto-tasks-body', 'Mint now');
action.click(); await tick();
const posted = requests.filter(r => r.body).length;
setWorkspace('two');
action.click();
assert(requests.filter(r => r.body).length === posted, 'old DOM action cannot mutate new workspace');
await fetchAndRenderOperations();
releasePost(); await tick(); await tick();
assert(get('auto-tasks-body').textContent.includes('Chore two'), 'old mutation cannot replace new workspace');
assert(!get('auto-task-operation-feedback').textContent.includes('Minted'), 'old success cannot be attributed to new workspace');
assert(!button('routines-body', 'Disable').disabled, 'routine toggle follows operator authority alone');
assert(!button('auto-tasks-body', 'Mint now').disabled, 'workspace switch does not lock another workspace');

// A→B→A does not revive a response from the prior visit.
setWorkspace('one'); await fetchAndRenderOperations();
action = button('auto-tasks-body', 'Mint now'); action.click(); await tick();
setWorkspace('two'); setWorkspace('one'); await fetchAndRenderOperations();
releasePost(); await tick(); await tick();
assert(!get('auto-task-operation-feedback').textContent.includes('Minted'), 'A→B→A drops old feedback');
assert(button('auto-tasks-body', 'Mint now') && !button('auto-tasks-body', 'Mint now').disabled, 'return visit releases pending guard when old request completes');
delayPost = false;
await fetchAndRenderOperations();

// A delayed GET is discarded after selection changes.
delayGet = true;
const staleLoad = fetchAndRenderOperations(); await tick();
setWorkspace('two'); delayGet = false; await fetchAndRenderOperations();
releaseGet(); await staleLoad;
assert(get('auto-tasks-body').textContent.includes('Chore two'), 'stale GET cannot overwrite selection');
setWorkspace(null); await fetchAndRenderOperations();
assert(get('auto-tasks-body').textContent.includes('Select a workspace'), 'all-workspace selection remains read-only');

// Leave a populated fixture for desktop/narrow rendered inspection.
setWorkspace('one'); await fetchAndRenderOperations();
const autoCard = descendants(get('auto-tasks-body')).find(node => String(node.className || '').includes('auto-task-card'));
const autoKids = Array.from(autoCard?.children || []);
const autoHead = autoKids.find(node => node.className === 'operation-row-head');
const autoDetails = autoKids.find(node => node.className === 'operation-details');
assert(autoHead, 'auto-task collapsed row is present');
assert(autoDetails, 'auto-task details are present');
assert(autoHead.textContent.includes('Chore one'), 'collapsed row shows the name');
assert(autoHead.textContent.includes('Disable') || autoHead.textContent.includes('Enable'), 'collapsed row keeps the primary action');
assert(!autoHead.textContent.includes('Manual mint ignores'), 'mint warning is not repeated on the collapsed row');
assert(autoDetails.textContent.includes('Manual mint ignores'), 'mint explanation stays in details');
assert(autoDetails.textContent.includes('Open duplicate'), 'duplicate explanation stays in details');
assert(autoDetails.textContent.includes('Last scheduler evaluation'), 'scheduler cursor stays in details');
assert(autoDetails.textContent.includes('Delivery coverage') || autoDetails.textContent.includes('covered'), 'delivery coverage stays in details');
autoDetails.open = true;
if (typeof Event === 'function') autoDetails.dispatchEvent(new Event('toggle'));
else autoDetails.listeners?.toggle?.();
await fetchAndRenderOperations();
const restored = descendants(get('auto-tasks-body')).find(node => node.className === 'operation-details');
assert(restored?.open, 'details stay open across rerender');

// A skip_if_open auto-task whose only instance is parked in someday reports no open duplicate and mint confirmation does not claim an open instance exists [ORB-12158].
const autoCards = descendants(get('auto-tasks-body')).filter(node => String(node.className || '').includes('auto-task-card'));
const somedayCard = autoCards.find(node => node.textContent?.includes('Someday one'));
assert(somedayCard, 'someday auto-task card is present');
const somedayDuplicateField = descendants(somedayCard).find(node => node.className === 'operation-field' && node.children[0]?.textContent === 'Open duplicate');
assert(somedayDuplicateField, 'someday auto-task card exposes an open-duplicate field');
assert(somedayDuplicateField.children[1]?.textContent === 'No', 'someday auto-task card reports no open duplicate');
assert(!somedayCard.textContent.includes('Yes — mint will create another'), 'someday card does not report duplicate warning');
const somedayMint = descendants(somedayCard).find(node => node.textContent === 'Mint now' && node.type === 'button');
somedayMint.click(); await tick(); await tick();
const somedayConfirm = confirmations.at(-1);
assert(somedayConfirm.includes('No open instance is currently tagged for this definition.'), 'someday mint confirmation reports no open instance');
assert(!somedayConfirm.includes('An open instance already exists'), 'someday mint confirmation does not claim open instance exists');

globalThis.operationsTestsPassed = true;
