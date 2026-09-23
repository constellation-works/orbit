// Runs against shipped modules in both the Node DOM harness and Chromium.
const { setWorkspace } = await import('./common.js');
const { initOperations, fetchAndRenderOperations: fetchAndRenderOperationsPane, fetchAndRenderAutoDrainPane } = await import('./operations.js');
// The Operations tab and the Tasks dock's Drain card refresh separately in the
// app; the harness drives both so every panel's behaviour is asserted together.
const fetchAndRenderOperations = async () => {
  const results = await Promise.allSettled([fetchAndRenderOperationsPane(), fetchAndRenderAutoDrainPane()]);
  const failed = results.find(result => result.status === 'rejected');
  if (failed) throw failed.reason;
};
const get = id => document.getElementById(id);
const descendants = node => [node, ...Array.from(node.children || []).flatMap(descendants)];
const button = (id, label) => descendants(get(id)).find(node => node.textContent === label && node.type === 'button');
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
const allowed = { authorized: true, reason: null };
const denied = { authorized: false, reason: 'Test session cannot perform this action. Ask the server operator.' };
const capabilities = { routine_toggle: allowed, job_run: allowed, clock_service: allowed, clock_cadence: allowed, auto_task_toggle: allowed, auto_task_mint: allowed };
const enabled = { one: true, two: true };
let clock = { enabled: true, configured_cadence_seconds: 60, provider: 'fixture', health: 'healthy', loaded: true, running: true, schedulable: true, last_tick_at: '2026-09-07T21:00:00Z', next_tick_at: '2026-09-07T21:01:00Z' };
let responseError = null;
let readbackError = false;
let releasePost = null;
let delayPost = false;
let delayGet = false;
let releaseGet = null;
let nextTask = 1;
let drainRunId = null;
let submittedJob = null;
const requests = [];
const confirmations = [];
const readinessTasks = [
  { task_id: 'ORB-1', status: 'backlog', eligible: true, reason: 'ready' },
  { task_id: 'ORB-2', status: 'backlog', eligible: false, reason: 'unmet_dependency', dependencies: [{ task_id: 'ORB-20', status: 'in-progress' }] },
  { task_id: 'ORB-3', status: 'backlog', eligible: false, reason: 'conflict_deferred', blocking_task_ids: ['ORB-30'], conflicts: [{ requested_file: 'file:crates/shared/src/lib.rs', locking_task_id: 'ORB-30' }] },
  { task_id: 'ORB-4', status: 'backlog', eligible: false, reason: 'claimed_by_live_child', run_ids: ['jrun-claimed-child'] },
  { task_id: 'ORB-5', status: 'backlog', eligible: false, reason: 'capacity_saturated', active_run_ids: ['jrun-active-leaf'] },
  { task_id: 'ORB-6', status: 'backlog', eligible: false, reason: 'crew_not_allowed', crew: 'luna', allowed_crews: ['sol', 'terra'] },
  { task_id: 'ORB-7', status: 'backlog', eligible: false, reason: 'outside_grant_scope', grant_id: 'opg-fixture' },
  { task_id: 'ORB-8', status: 'backlog', eligible: false, reason: 'future_server_reason' },
  { task_id: 'ORB-9', status: 'backlog', eligible: false, reason: 'unmet_dependency' },
];
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
    if (url.pathname === '/api/workflows/auto') return response({ workflow: 'auto', run_id: 'jrun-20260923-0400-a1', state: 'submitted', completion: body.complete ? 'done' : 'review', submitted_at: new Date().toISOString() });
    if (url.pathname === '/api/workflows/auto/stop') return response({ workflow: 'auto', outcome: 'stopped', coordinators: [{ run_id: drainRunId, outcome: 'stopped', remaining_children: ['jrun-child'] }] });
    if (url.pathname === '/api/jobs/fixture/run') {
      submittedJob = { run_id: 'jrun-dashboard-fixture', job_id: 'fixture', state: 'pending', created_at: new Date().toISOString() };
      return response({ job_id: 'fixture', run_id: submittedJob.run_id, state: 'submitted', submitted_at: submittedJob.created_at });
    }
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
    machine_name: 'fixture-host', controls_authorized: capabilities.routine_toggle.authorized, capabilities: { ...capabilities }, session_explanation: 'Session access: restart the dashboard server with explicit operator authority.',
    routines: [
      ...['one', 'two'].map(source => ({ name: `Routine ${source}`, source, target: 'job:fixture', enabled: enabled[source], cron: '30 14 * * *', description: 'Sweep landed deliveries.', last_fire: { state: 'succeeded', run_id: 'jrun-fixture-done', started_at: '2026-09-07T20:30:05Z', finished_at: '2026-09-07T20:33:10Z', duration_ms: 185000 }, next_evaluation: { state: enabled[source] ? 'scheduled' : 'disabled', at: '2026-09-07T21:30:00Z', hypothetical: !enabled[source] } })),
      { name: 'Parked one', source: 'one', target: 'job:parked_pipeline', enabled: false, cron: '*/20 * * * *', description: 'Kept in the repo, never fires.', next_evaluation: { state: 'disabled', at: '2026-09-07T21:40:00Z', hypothetical: true } },
    ],
    clock: { ...clock },
  });
  if (url.pathname === '/api/job-runs') return response({
    items: [
      ...(submittedJob ? [submittedJob] : []),
      { run_id: 'jrun-fixture-running', job_id: 'fixture', state: 'running', run_role: 'top-level', resolved_crew: 'system', created_at: '2026-09-07T20:59:00Z', started_at: '2026-09-07T20:59:10Z', finished_at: null, duration_ms: null },
      { run_id: 'jrun-fixture-done', job_id: 'fixture', state: 'succeeded', run_role: 'top-level', resolved_crew: 'system', created_at: '2026-09-07T20:30:00Z', started_at: '2026-09-07T20:30:05Z', finished_at: '2026-09-07T20:33:10Z', duration_ms: 185000 },
      { run_id: 'jrun-orphan', job_id: 'task_pr_pipeline', state: 'failed', run_role: 'child', resolved_crew: 'opus', created_at: '2026-09-07T19:00:00Z', started_at: '2026-09-07T19:00:01Z', finished_at: '2026-09-07T19:05:00Z', duration_ms: 299000 },
    ],
    total: submittedJob ? 4 : 3, limit: 100, truncated: false,
  });
  if (url.pathname === '/api/workflows/auto/readiness') return response({
    controls_authorized: true,
    snapshot: { read_only: true, limitations: 'Fixture snapshot only; eligibility can change immediately and does not guarantee a task will start.' },
    capacity: {
      active_leaf_runs: 4, max_active_leaf_runs: 4, free_slots: 0,
      candidate_pool_size: 8, candidate_pool_truncated: true,
      occupancy: { phases: { implementing: 2, lock_waiting: 1, post_implementation: 1, unknown: 0 } },
      deferred_conflicts: [{ task_id: 'ORB-3', blocking_task_ids: ['ORB-30'] }],
      drain_run_id: drainRunId, admissions_stopped: false,
    },
    tasks: readinessTasks,
  });
  return response({});
};
setWorkspace('one');
initOperations({ getWorkspaces: () => ['one', 'two'].map(id => ({ id, name: id, status: 'active' })), formatAbsoluteTime: value => value });
await fetchAndRenderOperations();
// The Drain card keeps only what an operator acts on: duration, concurrency,
// completion, Start/Stop, the slot line, two counts and the blocked-by list.
const drainBody = get('auto-drain-body');
const drainText = () => get('auto-drain-body').textContent;
const drainButton = label => button('auto-drain-body', label);
const durations = descendants(drainBody).filter(node => node.type === 'button' && String(node.className || '').includes('drain-duration'));
assert(durations.map(node => node.textContent).join(' ') === '15m 30m 1h 2h 4h 8h', `duration segments: ${durations.map(node => node.textContent)}`);
assert(durations.every(node => node.type === 'button' && ['true', 'false'].includes(node.getAttribute('aria-pressed'))), 'duration segments are pressed-state buttons');
assert(durations.find(node => node.getAttribute('aria-pressed') === 'true')?.textContent === '1h', 'one hour is the default window');
assert(drainText().includes('Eligible now1') && drainText().includes('Blocked by running2'), `counts use strict server eligibility and lock reasons: ${drainText()}`);
assert(drainText().includes('4/4 slots busy · admits up to 0 now'), 'slot line reads busy/limit and what a window admits now');
assert(drainText().includes('ORB-3 waits on ORB-30') && drainText().includes('lock · …/src/lib.rs'), 'a lock-blocked task names its holder and the shortened lock');
assert(drainText().includes('ORB-4 waits on jrun-claimed-child'), 'a live-child claim names the claiming run');
for (const gone of ['Task readiness', 'Waiting on deps', 'free slot', 'Snapshot only']) {
  assert(!drainText().includes(gone), `the card no longer renders ${JSON.stringify(gone)}`);
}
assert(!descendants(drainBody).some(node => /auto-drain-(task|slot)/.test(String(node.className || ''))), 'no readiness rows or slot tiles');
const blockedLinks = descendants(drainBody).filter(node => String(node.href || '').includes('#tasks?'));
assert(blockedLinks.some(link => String(link.href).includes('workspace=one') && String(link.href).includes('q=ORB-30')), 'blocked-by links stay workspace-qualified');
assert(get('auto-drain-live').textContent === 'idle', 'no live window reads idle');
assert(drainButton('Stop').disabled && String(drainButton('Stop').title).includes('No auto-delivery window is live'), 'Stop is disabled without a live window and says why');

// More than three blocked tasks collapse to "+N more".
readinessTasks.push(...[10, 11, 12].map(n => ({ task_id: `ORB-${n}`, status: 'backlog', eligible: false, reason: 'context_lock_conflict', conflicts: [{ requested_file: 'file:a.rs', locking_task_id: 'ORB-30' }] })));
await fetchAndRenderOperations();
assert(drainText().includes('Blocked by running5') && drainText().includes('+2 more'), 'blocked list is capped at three lines');
readinessTasks.splice(-3);

// Duration, stepper and completion drive the Start label and the submitted body.
drainButton('2h').click();
assert(drainButton('Start 2h window'), 'the Start label carries the chosen duration');
drainButton('+').click();
assert(descendants(drainBody).find(node => node.id === 'auto-drain-concurrency').value === '5', 'the stepper steps up from the runtime default');
drainButton('−').click(); drainButton('−').click(); drainButton('−').click(); drainButton('−').click(); drainButton('−').click();
assert(descendants(drainBody).find(node => node.id === 'auto-drain-concurrency').value === '1', 'the stepper stops at the input minimum of 1');
drainButton('+').click();
assert(drainText().includes('leave in review'), 'unchecked completion reads leave in review');
const completion = descendants(drainBody).find(node => node.type === 'checkbox');
completion.checked = true; completion.dispatchEvent(new Event('change'));
assert(drainText().includes('mark done · skip review'), 'checked completion states that it skips review');
drainButton('Start 2h window').click(); await tick(); await tick(); await tick();
const started = requests.find(r => r.path === '/api/workflows/auto');
assert(started && started.workspace === 'one' && started.body.for_duration === '2h' && started.body.concurrency === 2 && started.body.complete === true, `start posts the chosen window: ${JSON.stringify(started)}`);
assert(confirmations.at(-1).includes('Duration: 2h · Concurrency: 2') && confirmations.at(-1).includes('WARNING'), 'start confirms the window and warns about completion');
assert(get('auto-drain-operation-feedback').textContent.includes('Run jrun-20260923-0400-a1 submitted (completion: done).'), 'start result lands in the card status line');
const freshCompletion = descendants(get('auto-drain-body')).find(node => node.type === 'checkbox');
freshCompletion.checked = false; freshCompletion.dispatchEvent(new Event('change'));
assert(drainText().includes('leave in review'), 'completion unchecks back to review');

// A live window: header link in short form, time left for a window this
// browser started, and Stop enabled.
drainRunId = 'jrun-20260923-0400-a1';
await fetchAndRenderOperations();
const liveLink = descendants(get('auto-drain-live')).find(node => String(node.href || '').includes('#runs/'));
assert(liveLink?.textContent === 'jrun-…0400-a1' && String(liveLink.title).includes('jrun-20260923-0400-a1'), 'header links the live run by its short id');
if (typeof window.localStorage?.setItem === 'function') assert(/· (1h 59m|2h 00m) left/.test(get('auto-drain-live').textContent), `header shows time left: ${get('auto-drain-live').textContent}`);
drainButton('Stop').click(); await tick(); await tick(); await tick();
assert(requests.some(r => r.path === '/api/workflows/auto/stop' && r.workspace === 'one'), 'stop posts to the stop endpoint');
assert(confirmations.at(-1).includes('This is not cancellation.') && confirmations.at(-1).includes('jrun-20260923-0400-a1'), 'stop confirms and names the window');
assert(get('auto-drain-operation-feedback').textContent.includes('Admissions stopped') && get('auto-drain-operation-feedback').textContent.includes('1 admitted worker still running.'), 'stop result lands in the card status line');
drainRunId = null;

// No concrete workspace: the card is read-only and fetches nothing.
setWorkspace(null);
const readinessRequests = requests.filter(r => r.path === '/api/workflows/auto/readiness').length;
await fetchAndRenderOperations();
assert(drainText().includes('All-workspace mode is read-only') && get('auto-drain-live').textContent === 'read-only', 'aggregate mode is read-only');
assert(requests.filter(r => r.path === '/api/workflows/auto/readiness').length === readinessRequests, 'aggregate mode does not fetch readiness');
setWorkspace('one');
await fetchAndRenderOperations();
// Routines are grouped by whether they will fire, the toggle is a switch that
// still reads Enable/Disable, and the row names the job it runs.
const routineGroups = descendants(get('routines-body')).filter(node => String(node.className || '').includes('operation-group-title')).map(node => node.textContent);
assert(routineGroups.some(text => text.startsWith('Active1')) && routineGroups.some(text => text.startsWith('Paused1')), `routines grouped by state: ${routineGroups}`);
const routineSwitch = descendants(get('routines-body')).find(node => String(node.className || '').includes('operation-switch'));
assert(routineSwitch?.getAttribute('role') === 'switch' && routineSwitch.getAttribute('aria-checked') === 'true' && routineSwitch.textContent === 'Disable', 'routine toggle is a switch named by its action');
assert(descendants(get('routines-body')).some(node => node.href === '#operations/jobs?job=fixture'), 'routine row links the job it runs');
assert(descendants(get('routines-body')).some(node => String(node.className || '').includes('operation-timeline')), 'routines pane projects the next hour');
assert(!get('routines-body').textContent.includes('Routine two'), 'routines stay scoped to the selected workspace');
assert(descendants(get('clock-body')).some(node => String(node.className || '').includes('operation-clock-bar')), 'clock renders as a bar');
// Jobs are projected from routine targets plus recent runs. Authorized Run
// submits in the selected workspace and refreshes the run cells.
const jobsText = get('jobs-body').textContent;
assert(jobsText.includes('Running now1'), `running strip counts in-flight runs: ${jobsText.slice(0, 120)}`);
const jobCards = descendants(get('jobs-body')).filter(node => String(node.className || '').includes('job-card'));
assert(jobCards.length === 3 && ['fixture', 'task_pr_pipeline', 'parked_pipeline'].every(id => jobCards.some(card => card.dataset.job === id)), `catalogue unions routine targets (paused included) and run job ids: ${jobCards.map(card => card.dataset.job)}`);
const fixtureCard = jobCards.find(card => card.dataset.job === 'fixture');
const jobRunButton = id => {
  const card = descendants(get('jobs-body')).find(node => node.dataset?.job === id);
  return card && descendants(card).find(node => node.type === 'button' && node.textContent === 'Run ▸');
};
assert(fixtureCard.textContent.includes('Routine one') && fixtureCard.textContent.includes('1 running') && fixtureCard.textContent.includes('jrun-fixture-running'), 'job row shows its routine, active count and latest run');
const runButton = descendants(fixtureCard).find(node => node.textContent === 'Run ▸' && node.type === 'button');
assert(runButton && !runButton.disabled, 'authorized job run is enabled');
assert(!jobRunButton('parked_pipeline').disabled, 'a paused routine does not disable manual Run');
assert(fixtureCard.textContent.includes('orbit run job fixture --workspace one'), 'job details carry the CLI command');
assert(jobRunButton('task_pr_pipeline')?.disabled, 'delivery job needs task input');
assert(descendants(jobCards.find(card => card.dataset.job === 'task_pr_pipeline')).some(node => String(node.title).includes('Use Ship or Drain')), 'disabled delivery button explains its reason');
assert(requests.some(r => r.path === '/api/job-runs' && r.workspace === 'one'), 'jobs read the workspace-scoped recent runs');
delayPost = true;
runButton.click(); runButton.click(); await tick();
assert(requests.filter(r => r.path === '/api/jobs/fixture/run').length === 1, 'double click submits one job run');
assert(requests.find(r => r.path === '/api/jobs/fixture/run').workspace === 'one', 'job submission keeps workspace scope');
assert(get('job-operation-feedback').textContent.includes('Submitting fixture'), 'pending job feedback is visible');
assert(descendants(get('jobs-body')).some(node => node.textContent === 'Submitting…' && node.disabled), 'pending job button is disabled');
releasePost(); await tick(); await tick(); await tick();
delayPost = false;
assert(get('job-operation-feedback').textContent.includes('jrun-dashboard-fixture submitted'), 'submission receipt is visible');
assert(get('jobs-body').textContent.includes('jrun-dashboard-fixture'), 'new run appears after refresh');
responseError = 'Fixture submission refused';
jobRunButton('fixture').click(); await tick(); await tick();
assert(get('job-operation-feedback').textContent.includes('Fixture submission refused'), 'server error is visible');
assert(!jobRunButton('fixture').disabled, 'failed submission can be retried');
responseError = null;
capabilities.job_run = denied;
await fetchAndRenderOperations();
assert(jobRunButton('fixture').disabled && String(jobRunButton('fixture').title).includes('Test session'), 'read-only session disables job Run with reason');
capabilities.job_run = allowed;
await fetchAndRenderOperations();
assert(!button('auto-tasks-body', 'Disable').disabled, 'authorized toggle available');
assert(!button('auto-tasks-body', 'Mint now').disabled, 'authorized mint available');
assert(!button('clock-body', 'Pause clock').disabled, 'authorized clock available');
assert(!get('routines-body').textContent.includes('auto_task_scheduler'), 'scheduler is absent from routine fixture');

// A launchd agent that is still loaded but can no longer run must not read as
// HEALTHY on this card; the server reports it as missed [DANI-10386].
const healthyClock = clock;
clock = { ...clock, health: 'missed', schedulable: false, effective_cadence_seconds: null, next_tick_at: null, health_issue: 'launchd agent com.orbit.sweep is loaded but its program cannot run (/opt/homebrew/bin/orbit: program does not exist); recovery: `orbit clock enable` rewrites the unit to this binary and reloads it' };
await fetchAndRenderOperations();
const clockBadge = descendants(get('clock-body')).find(node => String(node.className || '').includes('operation-state'));
assert(clockBadge.textContent === 'missed', 'a stalled clock is not badged healthy');
assert(get('clock-body').textContent.includes('its program cannot run'), 'the stalled clock names its health issue');
assert(get('clock-body').textContent.includes('orbit clock enable'), 'the stalled clock card carries the recovery hint');
assert(get('clock-body').textContent.includes('Not scheduled'), 'a stalled clock is not reported as armed');
clock = healthyClock;
await fetchAndRenderOperations();
assert(descendants(get('clock-body')).find(node => String(node.className || '').includes('operation-state')).textContent === 'healthy', 'a healthy clock still renders healthy');

// An unobserved clock (systemd user bus down) must not read as paused or
// healthy, and must not offer enable/disable as if enabled were false.
clock = {
  health: 'unknown',
  enabled: null,
  provider: null,
  configured_cadence_seconds: null,
  effective_cadence_seconds: null,
  loaded: null,
  running: null,
  schedulable: null,
  last_tick_at: null,
  next_tick_at: null,
  health_issue: 'systemd clock manager is unavailable; Failed to connect to bus: No medium found',
  error: 'systemd clock manager is unavailable; Failed to connect to bus: No medium found',
};
await fetchAndRenderOperations();
const unknownBadge = descendants(get('clock-body')).find(node => String(node.className || '').includes('operation-state'));
assert(unknownBadge.textContent === 'unknown', 'an unobserved clock is badged unknown');
assert(get('clock-body').textContent.includes('service unknown'), 'an unobserved clock is not labeled paused');
assert(!get('clock-body').textContent.includes('service paused'), 'an unobserved clock does not invent paused authority');
assert(get('clock-body').textContent.includes('Failed to connect to bus'), 'an unobserved clock names the diagnostic');
assert(button('clock-body', 'Clock unavailable')?.disabled, 'unknown clock disables service controls');
assert(!button('clock-body', 'Pause clock') && !button('clock-body', 'Enable clock'), 'unknown clock does not offer pause or enable');
assert(button('clock-body', 'Apply cadence').disabled, 'unknown clock disables cadence');
clock = healthyClock;
await fetchAndRenderOperations();
assert(descendants(get('clock-body')).find(node => String(node.className || '').includes('operation-state')).textContent === 'healthy', 'restoring a healthy clock still renders healthy');

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
assert(requests.filter(r => r.path === '/api/routines/clock').every(r => ['enable', 'disable', 'set_cadence'].includes(r.body.action)), 'clock controls use the canonical action set');

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

// Definitions are grouped by trigger with a stats strip; the toggle is a switch.
const autoGroups = descendants(get('auto-tasks-body')).filter(node => String(node.className || '').includes('operation-group-title')).map(node => node.textContent);
assert(autoGroups.some(text => text.startsWith('On a schedule2')), `auto-tasks grouped by trigger: ${autoGroups}`);
const autoStats = descendants(get('auto-tasks-body')).filter(node => String(node.className || '').includes('operation-stat ')).map(node => node.textContent);
assert(autoStats.some(text => text.startsWith('Definitions2')) && autoStats.some(text => text.startsWith('Enabled2')) && autoStats.some(text => text.startsWith('Open duplicates1')), `auto-task stats: ${autoStats}`);
const autoSwitch = descendants(autoCard).find(node => String(node.className || '').includes('operation-switch'));
assert(autoSwitch?.getAttribute('role') === 'switch' && autoSwitch.getAttribute('aria-checked') === 'true', 'auto-task toggle is a switch');
assert(autoHead.textContent.includes('still open · scheduler will skip'), 'open duplicate is called out on the row');

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
