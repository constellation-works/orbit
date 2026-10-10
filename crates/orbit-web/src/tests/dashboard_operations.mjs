// Runs against shipped modules in Chromium via dashboard_operations_browser.mjs,
// the required dashboard-operations-browser scenario in the QA sweep inventory.
const { setWorkspace, statusPill } = await import('./js/common.js');
const { initOperations, fetchAndRenderOperations: fetchAndRenderOperationsPane, fetchAndRenderAutoDrainPane } = await import('./js/operations.js');
// The Operations tab and the Tasks dock's Drain card refresh separately in the
// app; the harness drives both so every panel's behaviour is asserted together.
const fetchAndRenderOperations = async () => {
  const results = await Promise.allSettled([...(["routines", "auto-tasks", "jobs"].map(subtab => fetchAndRenderOperationsPane(subtab))), fetchAndRenderAutoDrainPane()]);
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
let operationsSubtab = 'routines';
const capabilities = { routine_toggle: allowed, job_run: allowed, clock_service: allowed, clock_cadence: allowed, auto_task_toggle: allowed, auto_task_mint: allowed };
const enabled = { one: true, two: true };
let clock = { enabled: true, configured_cadence_seconds: 60, provider: 'fixture', health: 'healthy', loaded: true, running: true, schedulable: true, last_tick_at: '2026-09-07T21:00:00Z', next_tick_at: '2026-09-07T21:01:00Z' };
let responseError = null;
let readbackError = false;
let releasePost = null;
let delayPost = false;
let delayGet = false;
let releaseGet = null;
let delayJobRunsWorkspace = null;
let releaseJobRuns = null;
let failJobRunsWorkspace = null;
let workspaceTwoRunId = null;
let nextTask = 1;
let drainRunId = null;
let drainPhase = 'idle';
let controlsAuthorized = true;
let replicaWorkspace = false;
let approvalsFixture = { enabled: false };
let stopOutcome = 'stopped';
let stopSettlements;
let drainAdmissionsStopped = false;
let pullDrainRunId = null;
let pullDrainStopped = false;
let failReadiness = false;
let delayReadiness = false;
let readinessPending = false;
let releaseReadiness = null;
let drainReadinessRefresh = null;
let nullCapacity = false;
let resourceThrottle = null;
let drainCapacityOverride = {};
let drainTasksOverride = null;
const drainDeadline = window.__drainDeadline || new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString();
let submittedJob = null;
const memberDiagnostic = {
  reason: 'withheld',
  state: { consumer: 'routine/one', counts: { pending: 0, pending_commits: 0, waived: 0, excluded: 0, unresolved: 0 }, unresolved: {}, members: {
    counts: { pending: 999, fresh: 251, ready: 125, withheld: 1000, failed: 200 },
    withheld: Object.fromEntries(Array.from({ length: 20 }, (_, i) => [`member-${i}`, `Waiting for dependency ${i}`])),
    active: { member: { key: 'member-active' }, attempt: 1, max_attempts: 2, deadline: '2026-10-07T09:00:00Z', action_id: 'jrun-member' },
  } },
};
let fullStateError = false;
const requests = [];
const confirmations = [];
const readinessTasks = [
  { task_id: 'ORB-1', status: 'backlog', eligible: true, reason: 'ready' },
  { task_id: 'ORB-2', status: 'backlog', eligible: false, reason: 'unmet_dependency', dependencies: [{ task_id: 'ORB-20', status: 'in-progress' }] },
  { task_id: 'ORB-14334', status: 'backlog', eligible: false, reason: 'conflict_deferred', blocking_task_ids: ['ORB-14488'], conflicts: [{ requested_file: 'file:crates/shared/src/a-long-lock-selector-for-the-drain-layout-fixture.rs', locking_task_id: 'ORB-14488' }] },
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
  requests.push({ method: options.method || 'GET', path: url.pathname, workspace, body, concurrency: url.searchParams.get('concurrency') });
  if (options.method === 'POST') {
    if (delayPost) await new Promise(resolve => { releasePost = resolve; });
    if (responseError) return response({ error: responseError }, 500);
    if (url.pathname.endsWith('/toggle')) enabled[workspace] = body.enabled;
    if (url.pathname === '/api/workflows/auto') return response({ workflow: 'auto', run_id: 'jrun-20260923-0400-a1', state: 'submitted', completion: body.complete ? 'done' : 'review', approve_proposed: body.approve_proposed === true, submitted_at: new Date().toISOString() });
    if (url.pathname === '/api/workflows/auto/stop') return response({ workflow: 'auto', outcome: stopOutcome, coordinators: drainRunId ? [{ run_id: drainRunId, outcome: 'stopped', remaining_children: ['jrun-child'] }] : pullDrainRunId ? [{ run_id: pullDrainRunId, outcome: 'stopped', remaining_children: [] }] : [], pull_settlements: stopSettlements });
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
    const payload = { workspace, cron_zone: { name: 'America/Los_Angeles', offset_seconds: -25200 }, controls_authorized: capabilities.auto_task_toggle.authorized && capabilities.auto_task_mint.authorized, capabilities: { ...capabilities }, unconditional_mint_warning: "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.", definitions: [{ name: `Chore ${workspace}`, enabled: enabled[workspace], template: { title: 'Fixture chore' }, template_summary: 'Fixture chore', schedule_summary: 'every 15 minutes', description: 'Remediate CI failures for the selected workspace.', may_create_open_duplicate: true, open_duplicate: true, last_minted_task_id: 'ORB-00099', last_minted_task_status: 'in_progress', last_evaluation: { kind: 'fired', last_task_id: 'ORB-00001', last_fired_at: '2026-09-07T20:00:00Z' }, next_evaluation: { state: 'scheduled', at: '2026-09-07T22:00:00Z' }, automation: { reason: 'covered', state: { consumer: `auto-task/${workspace}`, baseline: { commit: 'abc1234', tree: 'def5678' }, observed: { commit: 'abc1234', tree: 'def5678' }, covered: { commit: 'abc1234', tree: 'def5678' }, counts: { pending: 0, pending_commits: 0, waived: 0, excluded: 0, unresolved: 0 }, excluded: [], unresolved: {} } } }, { name: `Someday ${workspace}`, enabled: true, template: { title: 'Parked chore' }, template_summary: 'Parked chore', schedule_summary: 'every 60 minutes', description: 'Auto-task whose only instance is parked in someday.', dedupe: 'skip_if_open', may_create_open_duplicate: false, open_duplicate: false, last_minted_task_id: 'ORB-00100', last_minted_task_status: 'someday', last_evaluation: { kind: 'fired', last_task_id: 'ORB-00100', last_fired_at: '2026-09-07T20:00:00Z' }, next_evaluation: { state: 'scheduled', at: '2026-09-07T22:00:00Z' } }] };
    // A plugin-off definition is hidden unless asked for; listed, it is
    // enabled with an earlier slot, so leaking into a summary would show.
    payload.inactive_plugin_count = 1;
    if (url.searchParams.get('include_inactive_plugins') === 'true') {
      payload.definitions.push({ name: 'graph-reindex', enabled: true, plugin_inactive: true, skipped_reason: "seeded by plugin:graph@1.0.0, which is switched off in this workspace; run `orbit plugin enable graph --scope workspace` to fire it again", template: { title: 'Reindex' }, schedule_summary: 'every 5 minutes', next_evaluation: { state: 'scheduled', at: '2026-09-07T20:05:00Z' } });
    }
    if (delayGet) await new Promise(resolve => { releaseGet = resolve; });
    return response(payload);
  }
  if (url.pathname.startsWith('/api/automation/') && url.pathname.endsWith('/state')) {
    return fullStateError ? response({ error: 'full state unavailable' }, 500)
      : response({ state: { consumer: 'routine/one', members: { pending: { 'member-full': { fingerprint: 'full-input' } } } } });
  }
  if (url.pathname === '/api/routines') return response({
    machine_name: 'fixture-host', cron_zone: { name: 'America/Los_Angeles', offset_seconds: -25200 }, controls_authorized: capabilities.routine_toggle.authorized, capabilities: { ...capabilities }, session_explanation: 'Session access: restart the dashboard server with explicit operator authority.',
    routines: [
      ...['one', 'two'].map(source => ({ automation: memberDiagnostic, name: `Routine ${source}`, source, target: 'job:fixture', enabled: enabled[source], cron: '30 14 * * *', description: 'Sweep landed deliveries.', last_fire: { state: 'succeeded', run_id: 'jrun-fixture-done', started_at: '2026-09-07T20:30:05Z', finished_at: '2026-09-07T20:33:10Z', duration_ms: 185000 }, next_evaluation: { state: enabled[source] ? 'scheduled' : 'disabled', at: '2026-09-07T21:30:00Z', hypothetical: !enabled[source] } })),
      { name: 'Parked one', source: 'one', target: 'job:parked_pipeline', enabled: false, cron: '*/20 * * * *', description: 'Kept in the repo, never fires.', next_evaluation: { state: 'disabled', at: '2026-09-07T21:40:00Z', hypothetical: true } },
    ],
    clock: { ...clock },
    inactive_plugin_counts: { one: 1 },
    retired: url.searchParams.get('include_inactive_plugins') === 'true'
      ? [{ name: 'graph-refresh', source: 'one', target: 'job:graph_refresh_pipeline', plugin_inactive: true, reason: "seeded by plugin:graph@1.0.0, which is switched off in workspace 'one'; run `orbit plugin enable graph --scope workspace` there to fire it again" }]
      : [],
  });
  if (url.pathname === '/api/job-runs') {
    if (workspace === delayJobRunsWorkspace) await new Promise(resolve => { releaseJobRuns = resolve; });
    if (workspace === failJobRunsWorkspace) return response({ error: 'Fixture job runs unavailable' }, 500);
    const items = workspace === 'two' && workspaceTwoRunId
      ? [{ run_id: workspaceTwoRunId, job_id: 'fixture', state: 'running', run_role: 'top-level', resolved_crew: 'system', created_at: '2026-09-07T21:00:00Z', started_at: '2026-09-07T21:00:01Z', finished_at: null, duration_ms: null }]
      : [
        ...(submittedJob ? [submittedJob] : []),
        { run_id: 'jrun-fixture-running', job_id: 'fixture', state: 'running', run_role: 'top-level', resolved_crew: 'system', created_at: '2026-09-07T20:59:00Z', started_at: '2026-09-07T20:59:10Z', finished_at: null, duration_ms: null },
        { run_id: 'jrun-fixture-done', job_id: 'fixture', state: 'succeeded', run_role: 'top-level', resolved_crew: 'system', created_at: '2026-09-07T20:30:00Z', started_at: '2026-09-07T20:30:05Z', finished_at: '2026-09-07T20:33:10Z', duration_ms: 185000 },
        { run_id: 'jrun-orphan', job_id: 'task_pr_pipeline', state: 'failed', run_role: 'child', resolved_crew: 'opus', created_at: '2026-09-07T19:00:00Z', started_at: '2026-09-07T19:00:01Z', finished_at: '2026-09-07T19:05:00Z', duration_ms: 299000 },
      ];
    return response({ items, total: items.length, limit: 100, truncated: false });
  }
  if (url.pathname === '/api/workflows/auto/readiness' && delayReadiness) {
    readinessPending = true;
    await new Promise(resolve => { releaseReadiness = resolve; });
    readinessPending = false;
  }
  if (url.pathname === '/api/workflows/auto/readiness' && failReadiness) return response({ error: 'readiness unavailable' }, 500);
  if (url.pathname === '/api/workflows/auto/readiness') return response({
    controls_authorized: controlsAuthorized,
    replica: replicaWorkspace,
    approvals: approvalsFixture,
    snapshot: { read_only: true, limitations: 'Fixture snapshot only; eligibility can change immediately and does not guarantee a task will start.' },
    capacity: {
      active_leaf_runs: 4, max_active_leaf_runs: 4, free_slots: 0,
      candidate_pool_size: 8, candidate_pool_truncated: true,
      occupancy: { phases: { implementing: 2, lock_waiting: 1, post_implementation: 1, unknown: 0 } },
      deferred_conflicts: [{ task_id: 'ORB-3', blocking_task_ids: ['ORB-30'] }],
      drain_run_id: drainRunId, admissions_stopped: drainAdmissionsStopped,
      pull_drain_run_id: pullDrainRunId, pull_drain_admissions_stopped: pullDrainStopped,
      drain_phase: drainPhase,
      drain_status_run_id: drainPhase === 'idle' ? null : 'jrun-20260923-0400-a1',
      ends_at: drainDeadline, running_admitted_workers: drainPhase === 'idle' ? 0 : 1,
      admitted_workers: drainPhase === 'idle' ? 0 : 3,
      ...(nullCapacity ? { active_leaf_runs: null, max_active_leaf_runs: null, free_slots: null, occupancy: null } : {}),
      resource_throttle: resourceThrottle,
      ...drainCapacityOverride,
    },
    tasks: drainTasksOverride || readinessTasks,
  });
  return response({});
};
setWorkspace('one');
initOperations({ getOperationsSubtab: () => operationsSubtab, getWorkspaces: () => ['one', 'two'].map(id => ({ id, name: id, status: 'active' })) });
await fetchAndRenderOperations();
const mintedStatus = get('auto-tasks-body').querySelector('.pill');
const taskStatus = statusPill('in-progress');
get('auto-tasks-body').appendChild(taskStatus);
assert(mintedStatus.dataset.status === taskStatus.dataset.status && mintedStatus.textContent === taskStatus.textContent, 'last-minted status uses the Tasks token');
assert(getComputedStyle(mintedStatus, '::before').backgroundColor === getComputedStyle(taskStatus, '::before').backgroundColor, 'last-minted status uses the Tasks dot colour');
assert(getComputedStyle(mintedStatus).getPropertyValue('--dot').trim() === getComputedStyle(mintedStatus).getPropertyValue('--status-in-progress').trim(), 'in-progress status uses its theme colour');
taskStatus.remove();

// The Drain card keeps only what an operator acts on: the capacity line and
// what a window would admit, two counts, the blocked-by list, then duration,
// concurrency, completion and Start/Stop.
const drainBody = get('auto-drain-body');
const drainText = () => get('auto-drain-body').textContent;
const drainButton = label => button('auto-drain-body', label);
const durations = descendants(drainBody).filter(node => node.type === 'button' && String(node.className || '').includes('drain-duration'));
assert(durations.map(node => node.textContent).join(' ') === '15m 30m 1h 2h 4h 8h', `duration segments: ${durations.map(node => node.textContent)}`);
assert(durations.every(node => node.type === 'button' && ['true', 'false'].includes(node.getAttribute('aria-pressed'))), 'duration segments are pressed-state buttons');
assert(durations.find(node => node.getAttribute('aria-pressed') === 'true')?.textContent === '1h', 'one hour is the default window');
const poolCount = tone => drainBody.querySelector(`.drain-stat.${tone} .drain-stat-value`).textContent;
assert(poolCount('eligible') === '1' && poolCount('locks') === '2' && poolCount('capacity') === '1' && poolCount('other') === '5', `counts use strict server eligibility and reason groups: ${drainText()}`);
assert(drainBody.querySelector('.drain-capacity-count').textContent.includes('Workspace: 4 of 4') && drainText().includes('0 free slots'), `capacity labels workspace slot occupancy: ${drainText()}`);
assert(drainBody.querySelector('.drain-slots').textContent.includes('1 occupied slot'), 'a saturated workspace says how many slots must clear');
assert(drainText().includes('ORB-14334 waits on ORB-14488') && drainText().includes('lock · …/src/a-long-lock-selector'), 'a lock-blocked task names its holder and the shortened lock');
assert(drainText().includes('ORB-4 waits on jrun-claimed-child'), 'a live-child claim names the claiming run');
for (const gone of ['Task readiness', 'Waiting on deps', 'slots busy', 'Snapshot only']) {
  assert(!drainText().includes(gone), `the card no longer renders ${JSON.stringify(gone)}`);
}
assert(!descendants(drainBody).some(node => /auto-drain-(task|slot)/.test(String(node.className || ''))), 'no readiness rows or slot tiles');
const blockedLinks = descendants(drainBody).filter(node => String(node.href || '').includes('#tasks?'));
assert(blockedLinks.some(link => String(link.href).includes('workspace=one') && String(link.href).includes('q=ORB-14488')), 'blocked-by links stay workspace-qualified');
assert(get('auto-drain-live').textContent === 'idle', 'no live window reads idle');
// With no live window Stop becomes "Send pending results": the settle-only pass
// needs no drain, so it stays usable and says what it does in visible text.
assert(!drainButton('Stop') && drainButton('Send pending results') && !drainButton('Send pending results').disabled, 'Stop relabels to Send pending results and stays enabled without a live window');
assert(drainBody.querySelector('.drain-stop-note:not([hidden])')?.textContent.trim(), 'the idle control has visible guidance');

// More than three blocked tasks collapse to "+N more".
readinessTasks.push(...[10, 11, 12].map(n => ({ task_id: `ORB-${n}`, status: 'backlog', eligible: false, reason: 'context_lock_conflict', conflicts: [{ requested_file: 'file:a.rs', locking_task_id: 'ORB-30' }] })));
await fetchAndRenderOperations();
assert(poolCount('locks') === '5' && drainText().includes('+2 more'), 'blocked list is capped at three lines');
readinessTasks.splice(-3);

// A task whose `os:` tags this host cannot run names the host it waits for,
// apart from the lock-blocked list, so the drain does not read as idle.
readinessTasks.push({ task_id: 'ORB-40', status: 'backlog', eligible: false, reason: 'host_os_mismatch', detail: 'waits for a macos host (os:macos)' });
await fetchAndRenderOperations();
assert(drainText().includes('ORB-40 waits for a macos host (os:macos)'), `an OS wait is named on the card: ${drainText()}`);
assert(poolCount('locks') === '2' && poolCount('other') === '6', 'an OS wait is not counted as waiting on a running task');
readinessTasks.splice(-1);

// Duration, stepper and completion drive the Start label and the submitted body.
drainButton('2h').click();
assert(drainButton('Start 2h window'), 'the Start label carries the chosen duration');
drainButton('+').click();
assert(descendants(drainBody).find(node => node.id === 'auto-drain-concurrency').value === '5', 'the stepper steps up from the runtime default');
drainButton('−').click(); drainButton('−').click(); drainButton('−').click(); drainButton('−').click(); drainButton('−').click();
assert(descendants(drainBody).find(node => node.id === 'auto-drain-concurrency').value === '1', 'the stepper stops at the input minimum of 1');
drainButton('+').click();
const completionOption = value => descendants(get('auto-drain-body')).find(node => node.type === 'radio' && node.value === value);
assert(drainText().includes('Stop at review') && drainText().includes('Mark done'), 'completion names both outcomes');
assert(completionOption('review').checked && !completionOption('done').checked, 'completion defaults to stopping at review');
const markDone = completionOption('done');
markDone.checked = true; markDone.dispatchEvent(new Event('change'));
assert(completionOption('done').checked && !completionOption('review').checked, 'choosing Mark done selects it and releases review');
drainButton('Start 2h window').click(); await tick(); await tick(); await tick();
const started = requests.find(r => r.path === '/api/workflows/auto');
assert(started && started.workspace === 'one' && started.body.for_duration === '2h' && started.body.concurrency === 2 && started.body.complete === true && started.body.approve_proposed === false, `start posts the chosen window: ${JSON.stringify(started)}`);
assert(confirmations.at(-1).includes('Duration: 2h · Concurrency: 2') && confirmations.at(-1).includes('WARNING'), 'start confirms the window and warns about completion');
assert(get('auto-drain-operation-feedback').textContent.includes('Run jrun-20260923-0400-a1 submitted (completion: done).'), 'start result lands in the card status line');
const backToReview = completionOption('review');
backToReview.checked = true; backToReview.dispatchEvent(new Event('change'));
assert(completionOption('review').checked, 'completion returns to review');

// Approving proposed tasks is its own explicit opt-in, off by default, with the
// qualification rule in the tooltip and a line in the confirm dialog.
const approveOption = value => descendants(get('auto-drain-body')).find(node => node.type === 'radio' && node.name === 'auto-drain-approve' && node.value === value);
const approveLabel = value => approveOption(value).parentNode;
assert(drainText().includes('Proposed tasks') && drainText().includes('Leave for me') && drainText().includes('Approve qualifying'), 'proposed-task handling names both choices');
assert(approveOption('leave').checked && !approveOption('approve').checked, 'approving proposed tasks defaults to off');
assert(String(approveLabel('approve').title).includes('context files') && String(approveLabel('approve').title).includes('task-pilot') && String(approveLabel('approve').title).includes('no-diff-expected') && String(approveLabel('approve').title).includes('no-auto-approve'), `the tooltip states the qualification rule: ${approveLabel('approve').title}`);
const requestsBeforeApprove = requests.filter(r => r.path === '/api/workflows/auto').length;
drainButton('Start 2h window').click(); await tick(); await tick(); await tick();
const defaultStart = requests.filter(r => r.path === '/api/workflows/auto').at(-1);
assert(requests.filter(r => r.path === '/api/workflows/auto').length === requestsBeforeApprove + 1 && defaultStart.body.approve_proposed === false, `an ordinary start posts approve_proposed false: ${JSON.stringify(defaultStart)}`);
assert(!confirmations.at(-1).includes('approve qualifying proposed tasks'), 'the confirm dialog stays quiet when approving is off');
approveOption('approve').checked = true; approveOption('approve').dispatchEvent(new Event('change'));
assert(approveOption('approve').checked && !approveOption('leave').checked, 'choosing Approve qualifying selects it');
drainButton('Start 2h window').click(); await tick(); await tick(); await tick();
const approveStart = requests.filter(r => r.path === '/api/workflows/auto').at(-1);
assert(approveStart.body.approve_proposed === true && approveStart.body.complete === false, `the opt-in posts approve_proposed true: ${JSON.stringify(approveStart)}`);
assert(confirmations.at(-1).includes('approve qualifying proposed tasks, including ones filed while it runs'), `the confirm dialog names it: ${confirmations.at(-1)}`);
assert(get('auto-drain-operation-feedback').textContent.includes('Approving qualifying proposed tasks.'), 'the start result says the window approves');
approveOption('leave').checked = true; approveOption('leave').dispatchEvent(new Event('change'));
assert(approveOption('leave').checked, 'approving returns to off');

// Unauthorized sessions and pull replicas get the control disabled with the
// reason as visible text; Start itself still works without the opt-in.
controlsAuthorized = false;
await fetchAndRenderOperations();
assert(approveOption('approve').disabled && !approveOption('leave').disabled, 'an unauthorized session cannot choose to approve');
assert(drainText().includes('Approving proposed tasks requires an authorized operator session'), 'the unauthorized reason is visible text');
assert(!drainButton('Start 2h window').disabled, 'an unauthorized session can still start a default window');
controlsAuthorized = true;
replicaWorkspace = true;
await fetchAndRenderOperations();
assert(approveOption('approve').disabled && drainText().includes('A pull replica cannot approve proposed tasks'), 'a pull replica is not offered approving');
replicaWorkspace = false;
await fetchAndRenderOperations();
assert(!approveOption('approve').disabled, 'the opt-in returns on an owner');

// Concurrency the server would refuse (zero, negative, fractional) never reaches
// it: the readiness read a poll makes would answer 400 and blank the card, and
// Start would answer 422. Start stays off and the reason is visible until the
// field is fixed.
const concurrencyInput = () => descendants(get('auto-drain-body')).find(node => node.id === 'auto-drain-concurrency');
const concurrencyProblem = () => descendants(get('auto-drain-body')).find(node => node.id === 'auto-drain-concurrency-problem');
const typeConcurrency = value => { concurrencyInput().value = value; concurrencyInput().dispatchEvent(new Event('input')); };
const lastReadiness = () => requests.filter(r => r.path === '/api/workflows/auto/readiness').at(-1);
assert(concurrencyProblem().hidden === true, 'a valid concurrency shows no problem');
for (const bad of ['0', '-1', '2.5']) {
  typeConcurrency(bad);
  assert(drainButton('Start 2h window').disabled, `Start is off for concurrency ${bad}`);
  assert(concurrencyInput().getAttribute('aria-invalid') === 'true', `the field is marked invalid for ${bad}`);
  assert(concurrencyProblem().hidden === false && concurrencyProblem().textContent.includes('whole number'), `the reason is visible for ${bad}: ${concurrencyProblem().textContent}`);
  assert(concurrencyProblem().getAttribute('role') === 'alert', 'the reason is announced');
  const before = requests.filter(r => r.path === '/api/workflows/auto').length;
  drainButton('Start 2h window').click(); await tick();
  assert(requests.filter(r => r.path === '/api/workflows/auto').length === before, `Start posts nothing for ${bad}`);
  await fetchAndRenderOperations();
  assert(lastReadiness().concurrency === null, `the readiness poll does not send concurrency ${bad}`);
  assert(concurrencyInput().value === bad, 'a poll does not discard what the operator typed');
  assert(drainButton('Start 2h window').disabled, `Start stays off across a poll for ${bad}`);
}
typeConcurrency('3');
assert(!drainButton('Start 2h window').disabled && concurrencyProblem().hidden === true, 'a valid value turns Start back on and clears the reason');
await fetchAndRenderOperations();
assert(lastReadiness().concurrency === '3', 'a valid concurrency is sent with the readiness read');
typeConcurrency('');
assert(!drainButton('Start 2h window').disabled, 'blank means the runtime default and is valid');
typeConcurrency('2');

// A payload without capacity figures reads as unknown, not as zero: `Number(null)`
// is 0, which printed "0 free slots" and a placeholder of "null".
nullCapacity = true;
await fetchAndRenderOperations();
assert(/capacity unknown/i.test(drainText()) && !drainText().includes('0 free slots') && !drainText().includes('NaN'), `missing figures read as unknown: ${drainText()}`);
assert(concurrencyInput().placeholder === 'auto', `no limit means no numeric placeholder: ${concurrencyInput().placeholder}`);
nullCapacity = false;
await fetchAndRenderOperations();

// A resource throttle holds every admission, naming both high-water and
// resume thresholds for resources above high and inside the hysteresis band;
// it disappears once pressure clears.
assert(!drainText().includes('Admissions throttled'), 'no throttle note without a throttle');
resourceThrottle = {
  resources: [
    { resource: 'memory', percent: 93.4, high_percent: 90, resume_percent: 80, since: '2026-10-04T08:41:00Z' },
    { resource: 'cpu', percent: 89.0, high_percent: 90, resume_percent: 75, since: '2026-10-04T08:40:00Z' },
  ],
};
await fetchAndRenderOperations();
assert(
  drainText().includes('Admissions throttled: memory 93% (throttled at ≥ 90% since 2026-10-04 01:41 PDT') &&
  drainText().includes('; resumes below 80%)') &&
  drainText().includes('load 0.9× cores (throttled at ≥ 0.9× cores since 2026-10-04 01:40 PDT') &&
  drainText().includes('; resumes below 0.75× cores)') &&
  drainText().includes('Running tasks are not touched.'),
  `the throttle names both thresholds for held resources above high and in hysteresis band: ${drainText()}`
);
assert(!drainText().includes('89% ≥'), 'hysteresis band reading must not render a false comparison against high threshold');
assert(descendants(drainBody).some(node => node.getAttribute?.('role') === 'status' && node.textContent.includes('Admissions throttled')), 'the throttle note is announced as status');
resourceThrottle = null;
await fetchAndRenderOperations();
assert(!drainText().includes('Admissions throttled'), 'the note clears with the throttle');

// ORB-14705: the pool figures partition the readiness tasks, so they add up to
// the backlog and name each group; "blocked" stays a task status.
const backlogTask = (n, reason) => ({ task_id: `ORB-${9000 + n}`, status: 'backlog', eligible: false, reason, conflicts: reason === 'context_lock_conflict' ? [{ requested_file: 'file:a.rs', locking_task_id: 'ORB-30' }] : undefined });
drainTasksOverride = [
  ...Array.from({ length: 17 }, (_, i) => backlogTask(i, 'context_lock_conflict')),
  ...Array.from({ length: 13 }, (_, i) => backlogTask(17 + i, 'resource_throttled')),
  ...Array.from({ length: 3 }, (_, i) => backlogTask(30 + i, 'capacity_saturated')),
  backlogTask(33, 'operator_validation_handoff'),
  backlogTask(34, 'pilot_already_landed'),
  backlogTask(35, 'surface_reserved'),
];
await fetchAndRenderAutoDrainPane();
const poolGroups = Array.from(drainBody.querySelectorAll('.drain-stat')).map(node => ({
  label: node.querySelector('.drain-stat-label').textContent,
  title: node.querySelector('.drain-stat-label').title,
  value: Number(node.querySelector('.drain-stat-value').textContent),
}));
assert(poolGroups.map(group => `${group.label}=${group.value}`).join('; ') === 'Pool: eligible=0; Pool: waiting on locks=17; Pool: waiting on capacity=16; Pool: waiting, other=3', `pool groups: ${JSON.stringify(poolGroups)}`);
assert(poolGroups.reduce((sum, group) => sum + group.value, 0) === 36, 'the pool figures sum to the 36 readiness tasks');
assert(poolGroups[2].title.includes('resource_throttled 13') && poolGroups[2].title.includes('capacity_saturated 3'), `the capacity group lists its reasons: ${poolGroups[2].title}`);
assert(['operator_validation_handoff 1', 'pilot_already_landed 1', 'surface_reserved 1'].every(part => poolGroups[3].title.includes(part)), `the other group lists its reasons: ${poolGroups[3].title}`);
assert(!poolGroups.some(group => /blocked/i.test(`${group.label} ${group.title}`)), `no pool label calls a readiness group blocked: ${JSON.stringify(poolGroups)}`);
drainTasksOverride = null;

// The throttle note and the top bar's load chip state one CPU reading in one
// unit: 164% of cores is "1.6× cores" in both, and 75% is a bare 0.75× threshold.
{
  const { renderHostResources } = await import('./js/host-resources.js');
  const reading = percent => ({ percent, severity: 'critical' });
  renderHostResources({ cpu: reading(164), memory: reading(40), disk: { path: '/', ...reading(50) }, throttle: true, pressures: [{ resource: 'cpu' }], reason: 'cpu high', thresholds: { enabled: true }, stale: false, sample_age_seconds: 1 });
  const chip = get('host-resource-chips').querySelector('[data-resource="cpu"] .v');
  resourceThrottle = { resources: [{ resource: 'cpu', percent: 164, high_percent: 90, resume_percent: 75, since: '2026-10-04T08:40:00Z' }] };
  await fetchAndRenderAutoDrainPane();
  const note = drainBody.querySelector('.drain-throttle-note').textContent;
  assert(chip.textContent === '1.6× cores' && note.includes(`load ${chip.textContent} (throttled at ≥ 0.9× cores`) && note.includes('resumes below 0.75× cores'), `chip ${chip.textContent} vs note ${note}`);
  assert(!/cpu \d|164/.test(note), `the note never states cpu as a bare percentage: ${note}`);
  resourceThrottle = null;
  await fetchAndRenderAutoDrainPane();
}

// ORB-14489: zero free slots alone does not imply that finishing a task can
// unblock admission. Exercise the readiness-to-card boundary for each reason.
for (const fixture of [
  { capacity: { active_leaf_runs: 6, max_active_leaf_runs: 12, free_slots: 0, resource_throttle: { resources: [{ resource: 'cpu', percent: 164, high_percent: 90, resume_percent: 75, since: '2026-10-04T08:40:00Z' }] } }, summary: /host resource throttle.*cpu/, cannotClearSlot: true },
  { capacity: { active_leaf_runs: 12, max_active_leaf_runs: 12, free_slots: 0 }, summary: /workspace leaf limit.*1 occupied slot/ },
  { capacity: { active_leaf_runs: 14, max_active_leaf_runs: 12, free_slots: 0, leaf_occupancy_by_pipeline: { task_gate_pipeline: 10, task_pr_pipeline: 4 } }, summary: /workspace leaf limit.*3 occupied slots/, pipelines: true },
  { capacity: { active_leaf_runs: 6, max_active_leaf_runs: 6, free_slots: 0, leaf_occupancy_by_pipeline: { task_gate_pipeline: 4 } }, summary: /workspace leaf limit.*1 occupied slot/, otherSlots: true },
  { capacity: { active_leaf_runs: null, max_active_leaf_runs: 12, free_slots: 0, occupancy: { active_leaf_runs: 12 } }, summary: /workspace leaf limit.*1 occupied slot/ },
  { capacity: { active_leaf_runs: 6, max_active_leaf_runs: 12, free_slots: 0, admissions_stopped: true }, summary: /window has stopped/, cannotClearSlot: true },
  { capacity: { active_leaf_runs: 6, max_active_leaf_runs: 12, free_slots: 0, host_shutdown: { kind: 'reboot' } }, summary: /host shutdown/, cannotClearSlot: true },
  { capacity: { active_leaf_runs: 6, max_active_leaf_runs: 12, free_slots: 0 }, summary: /No admissions.*snapshot/, cannotClearSlot: true },
  { capacity: { active_leaf_runs: 6, max_active_leaf_runs: 12, free_slots: 6 }, tasks: [readinessTasks[2]], summary: /1 pool task.*locks or live claims/, cannotClearSlot: true },
  { capacity: { active_leaf_runs: 6, max_active_leaf_runs: 12, free_slots: 2 }, tasks: [readinessTasks[0]], summary: /admits up to 1 task/ },
]) {
  drainCapacityOverride = fixture.capacity;
  drainTasksOverride = fixture.tasks || null;
  await fetchAndRenderAutoDrainPane();
  const summary = drainBody.querySelector('.drain-slots').textContent;
  assert(fixture.summary.test(summary), `readiness constraint is explained: ${JSON.stringify(fixture.capacity)} => ${summary}`);
  if (fixture.cannotClearSlot) assert(!/finish|must clear/.test(summary), `ORB-14489: task completion must not be presented as clearing another constraint: ${summary}`);
  if (fixture.pipelines) {
    const pipelines = drainBody.querySelector('.drain-pipeline-occupancy').textContent;
    assert(pipelines.includes('task_gate_pipeline: 10 slots') && pipelines.includes('task_pr_pipeline: 4 slots'), `occupied slots retain the per-pipeline explanation: ${pipelines}`);
  }
  if (fixture.otherSlots) assert(drainBody.querySelector('.drain-pipeline-occupancy').textContent.includes('other: 2 slots'), 'legacy wrapper occupancy reconciles with the workspace total');
}
drainCapacityOverride = {};
drainTasksOverride = null;
await fetchAndRenderOperations();

// The window Start opens changes the card's state, and that change must not be
// announced over Start's own result (the run and its completion mode).
drainRunId = 'jrun-20260923-0400-a1';
drainPhase = 'draining';
drainButton('Start 2h window').click(); await tick(); await tick(); await tick();
assert(get('auto-drain-live').textContent.includes('Draining'), 'the card moved to the live window');
assert(get('auto-drain-operation-feedback').textContent.includes('Run jrun-20260923-0400-a1 submitted') && get('auto-drain-operation-feedback').className.includes('success'), `the start result survives the state change: ${get('auto-drain-operation-feedback').textContent}`);
// Later changes are still announced.
drainPhase = 'winding_down';
await fetchAndRenderOperations();
assert(get('auto-drain-operation-feedback').textContent.includes('Auto-drain Winding down'), `a later state change is announced: ${get('auto-drain-operation-feedback').textContent}`);
drainRunId = null;
drainPhase = 'idle';
await fetchAndRenderOperations();
assert(get('auto-drain-operation-feedback').textContent === 'Auto-drain idle.', `settling to idle is announced: ${get('auto-drain-operation-feedback').textContent}`);

// Start went through but the read-back failed: the window exists, so the result
// stays; it is not rewritten into a start failure.
failReadiness = true;
drainButton('Start 2h window').click(); await tick(); await tick(); await tick();
assert(get('auto-drain-operation-feedback').textContent.includes('submitted') && !get('auto-drain-operation-feedback').textContent.includes('failed to start'), `a failed read-back is not a failed start: ${get('auto-drain-operation-feedback').textContent}`);
assert(!drainButton('Start 2h window').disabled, 'the guard is released after a failed read-back');
failReadiness = false;
await fetchAndRenderOperations();

// Send pending results delivers recorded settlements and reports each outcome; a
// settlement that did not reach its owner is never reported as plain success.
stopOutcome = 'idle';
stopSettlements = [
  { owner: 'host:/owner', drain_run_id: 'jrun-old', task_id: 'ORB-1', leaf_run_id: 'jrun-leaf-1', outcome: 'settled' },
  { owner: 'host:/owner', drain_run_id: 'jrun-old', task_id: 'ORB-2', leaf_run_id: 'jrun-leaf-2', outcome: 'settled' },
  { owner: 'host:/owner', drain_run_id: 'jrun-old', task_id: 'ORB-3', leaf_run_id: null, outcome: 'owner_unreachable' },
  { owner: 'host:/owner', drain_run_id: 'jrun-old', task_id: 'ORB-4', leaf_run_id: null, outcome: 'launch_uncertain' },
];
drainButton('Send pending results').click(); await tick(); await tick(); await tick();
const settleFeedback = get('auto-drain-operation-feedback');
assert(settleFeedback.textContent.includes('No auto-delivery window was live'), `idle stop names what did not change: ${settleFeedback.textContent}`);
assert(settleFeedback.textContent.includes('2 settlements delivered'), `delivered settlements are counted: ${settleFeedback.textContent}`);
assert(settleFeedback.textContent.includes('1 waiting for the owner (unreachable)') && settleFeedback.textContent.includes('[ORB-3]'), `an unreachable owner is called out with its task: ${settleFeedback.textContent}`);
assert(settleFeedback.textContent.includes('1 launch uncertain — needs manual recovery, see the distributed drain runbook') && settleFeedback.textContent.includes('[ORB-4]'), `an uncertain launch points at the runbook: ${settleFeedback.textContent}`);
assert(settleFeedback.className.includes('error') && !settleFeedback.className.includes('success'), 'unfinished settlements are not styled as success');
stopSettlements = [{ owner: 'host:/owner', drain_run_id: 'jrun-old', task_id: 'ORB-1', leaf_run_id: 'jrun-leaf-1', outcome: 'settled' }];
drainButton('Send pending results').click(); await tick(); await tick(); await tick();
assert(settleFeedback.textContent.includes('1 settlement delivered') && settleFeedback.className.includes('success'), `a fully delivered pass is plain success: ${settleFeedback.textContent}`);
stopOutcome = 'stopped';
stopSettlements = undefined;

// Read-only reasons are visible text beside the button, not only a tooltip.
controlsAuthorized = false;
await fetchAndRenderOperations();
assert(drainButton('Send pending results').disabled && drainText().includes('requires an authorized operator session'), 'an unauthorized session sees why the control is off as visible text');
controlsAuthorized = true;
await fetchAndRenderOperations();

// A live window: header link in short form, time left for a window this
// browser started, and Stop enabled.
drainRunId = 'jrun-20260923-0400-a1';
drainPhase = 'draining';
await fetchAndRenderOperations();
const liveLink = descendants(get('auto-drain-live')).find(node => String(node.href || '').includes('#runs/'));
assert(liveLink?.textContent === 'jrun-…0400-a1' && String(liveLink.title).includes('jrun-20260923-0400-a1'), 'header links the live run by its short id');
assert(/(1h 59m|2h 00m) left/.test(get('auto-drain-live').textContent), `header shows server time left: ${get('auto-drain-live').textContent}`);
assert(get('auto-drain-live').querySelector('.drain-window-count').textContent.includes('This window: 1 running of 3 admitted'), 'live counts label this window separately from workspace slots');
assert(!get('auto-drain-live').textContent.includes('Approving proposed tasks'), 'a window started without approve-proposed says nothing about it');
approvalsFixture = { enabled: true, drain_run_id: drainRunId, approved_total: 3, approved: ['ORB-1'], awaiting_pilot: 1, held_total: 2, held_by_reason: { missing_complexity: 1, pilot_held: 1 }, held: [{ task_id: 'ORB-8', reason: 'missing_complexity' }, { task_id: 'ORB-9', reason: 'pilot_held' }] };
await fetchAndRenderOperations();
const approvalsNode = get('auto-drain-live').querySelector('.drain-approvals');
assert(approvalsNode?.textContent === 'Approving proposed tasks · 3 approved · 2 held', `the live window shows approve-proposed with counts: ${approvalsNode?.textContent}`);
assert(String(approvalsNode.title).includes('1 × missing complexity') && String(approvalsNode.title).includes('ORB-9: pilot held'), `hold reasons are in the tooltip: ${approvalsNode.title}`);
approvalsFixture = { enabled: true, drain_run_id: drainRunId };
await fetchAndRenderOperations();
assert(get('auto-drain-live').querySelector('.drain-approvals').textContent === 'Approving proposed tasks', 'a payload without counts shows only the flag');
approvalsFixture = { enabled: false };
await fetchAndRenderOperations();
drainButton('Stop').click(); await tick(); await tick(); await tick();
assert(requests.some(r => r.path === '/api/workflows/auto/stop' && r.workspace === 'one'), 'stop posts to the stop endpoint');
assert(confirmations.at(-1).includes('This is not cancellation.') && confirmations.at(-1).includes('jrun-20260923-0400-a1'), 'stop confirms and names the window');
assert(get('auto-drain-operation-feedback').textContent.includes('Admissions stopped') && get('auto-drain-operation-feedback').textContent.includes('1 admitted worker still running.'), 'stop result lands in the card status line');
// Once admissions are stopped the button offers the settle-only pass instead.
drainAdmissionsStopped = true;
await fetchAndRenderOperations();
assert(drainButton('Send pending results') && !drainButton('Send pending results').disabled && drainText().includes('Admissions are already stopped for jrun-20260923-0400-a1'), 'a stopped window still offers to settle recorded work');
drainAdmissionsStopped = false;
drainRunId = null;
drainPhase = 'winding_down';

// A replica's live pull drain has no auto window, yet Stop acts on it: the
// button reads Stop (not Send pending results), and the confirm names the pull drain
// and does not claim that no window is live or that nothing is stopped.
drainPhase = 'idle';
pullDrainRunId = 'jrun-pull-drain-0001';
await fetchAndRenderOperations();
assert(drainButton('Stop') && !drainButton('Send pending results') && !drainButton('Stop').disabled, 'a live pull drain alone offers Stop, not Send pending results');
assert(get('auto-drain-live').textContent.includes('Pull drain') && descendants(get('auto-drain-live')).some(node => String(node.title || '').includes('jrun-pull-drain-0001')), `header shows the pull drain: ${get('auto-drain-live').textContent}`);
drainButton('Stop').click(); await tick(); await tick(); await tick();
assert(requests.some(r => r.path === '/api/workflows/auto/stop' && r.workspace === 'one'), 'stopping a pull drain posts to the stop endpoint');
assert(confirmations.at(-1).includes('pull drain') && confirmations.at(-1).includes('jrun-pull-drain-0001') && !confirmations.at(-1).includes('No auto-delivery window'), `confirm names the pull drain: ${confirmations.at(-1)}`);
// Once its admissions are stopped the readiness says so and the button offers the settle-only pass.
pullDrainStopped = true;
await fetchAndRenderOperations();
assert(drainButton('Send pending results') && drainText().includes('Admissions are already stopped for pull drain jrun-pull-drain-0001'), `a stopped pull drain is reported as stopped: ${drainText()}`);
pullDrainStopped = false;
pullDrainRunId = null;
drainPhase = 'winding_down';

// No concrete workspace: the card is read-only and fetches nothing.
setWorkspace(null);
const readinessRequests = requests.filter(r => r.path === '/api/workflows/auto/readiness').length;
await fetchAndRenderOperations();
assert(drainText().includes('All-workspace mode is read-only') && get('auto-drain-live').textContent === 'read-only', 'aggregate mode is read-only');
assert(requests.filter(r => r.path === '/api/workflows/auto/readiness').length === readinessRequests, 'aggregate mode does not fetch readiness');
setWorkspace('one');
drainPhase = 'idle';
await fetchAndRenderOperations();
// Routines are grouped by whether they will fire, the toggle is a switch that
// still reads Enable/Disable, and the row names the job it runs.
const routineGroups = descendants(get('routines-body')).filter(node => String(node.className || '').includes('operation-group-title')).map(node => node.textContent);
assert(routineGroups.some(text => text.startsWith('Active1')) && routineGroups.some(text => text.startsWith('Paused1')), `routines grouped by state: ${routineGroups}`);
const routineSwitch = descendants(get('routines-body')).find(node => String(node.className || '').includes('operation-switch'));
assert(routineSwitch?.getAttribute('role') === 'switch' && routineSwitch.getAttribute('aria-checked') === 'true' && routineSwitch.textContent === 'Disable', 'routine toggle is a switch named by its action');
assert(descendants(get('routines-body')).some(node => node.getAttribute?.('href') === '#operations/jobs?job=fixture'), 'routine row links the job it runs');
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

// A workspace switch clears the previous jobs view while the new workspace's
// runs are still pending. A routines refresh must not repaint the old run list,
// and a failed B read must leave only its error state in the panel.
workspaceTwoRunId = 'jrun-workspace-two';
delayJobRunsWorkspace = 'two';
failJobRunsWorkspace = 'two';
setWorkspace('two');
const workspaceTwoJobs = fetchAndRenderOperationsPane('jobs');
await tick(); await tick();
assert(releaseJobRuns, 'workspace B job-runs request is pending');
await fetchAndRenderOperationsPane('routines');
const pendingJobsText = get('jobs-body').textContent;
assert(pendingJobsText === 'Loading…' && !pendingJobsText.includes('jrun-fixture-running'), `pending B jobs show only loading state: ${pendingJobsText}`);
releaseJobRuns();
const failedWorkspaceTwoJobs = await Promise.allSettled([workspaceTwoJobs]);
assert(failedWorkspaceTwoJobs[0].status === 'rejected', 'workspace B job-runs failure reaches the panel');
const failedJobsText = get('jobs-body').textContent;
assert(failedJobsText.startsWith('Unable to load:') && !failedJobsText.includes('jrun-fixture-running'), `failed B jobs show only the error state: ${failedJobsText}`);
assert(requests.some(request => request.path === '/api/job-runs' && request.workspace === 'two'), 'workspace B jobs request uses its selected workspace');
delayJobRunsWorkspace = null;
failJobRunsWorkspace = null;
await fetchAndRenderOperationsPane('jobs');
assert(get('jobs-body').textContent.includes('jrun-workspace-two') && !get('jobs-body').textContent.includes('jrun-fixture-running'), 'workspace B run list appears after its jobs load');
setWorkspace('one');
await fetchAndRenderOperations();
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

clock = { ...healthyClock, enabled: false, running: true, health: 'unhealthy', schedulable: true, effective_cadence_seconds: 60, health_issue: 'systemd timer is disabled but still active' };
await fetchAndRenderOperations();
assert(get('clock-body').textContent.includes('service active (disabled)'), 'active disabled timer is not labeled paused');
assert(get('clock-body').textContent.includes('unhealthy'), 'active disabled timer has an unhealthy badge');
assert(!get('clock-body').textContent.includes('Paused'), 'active disabled timer does not show a paused next tick');
assert(button('clock-body', 'Pause clock') && !button('clock-body', 'Enable clock'), 'active disabled timer offers Pause');
button('clock-body', 'Pause clock').click(); await tick(); await tick();
const activePause = requests.filter(r => r.path === '/api/routines/clock').at(-1);
assert(activePause.body.action === 'disable' && activePause.body.expected_enabled === false, 'active disabled timer sends the pause action');
clock = healthyClock;
await fetchAndRenderOperations();

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
// The browser runs in America/Los_Angeles: the 22:00Z mint is a local 15:00
// and says so, beside cron triggers that are stated in the host zone.
assert(autoStats.some(text => text.startsWith('Next mint') && text.endsWith('15:00 PDT')), `next mint names its local zone: ${autoStats}`);
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

// Plugin-off definitions: hidden by default with an offer to show them; shown,
// they sit in their own group, outside every count and next-fire summary.
setWorkspace('one');
enabled.one = true;
responseError = null;
await fetchAndRenderOperations();
const autoCountBefore = get('auto-tasks-count').textContent;
const routineCountBefore = get('routines-count').textContent;
assert(!get('auto-tasks-body').textContent.includes('graph-reindex'), 'a plugin-off auto-task is hidden by default');
assert(!get('routines-body').textContent.includes('graph-refresh'), 'a plugin-off routine is hidden by default');
const showHidden = button('auto-tasks-body', 'Show 1 hidden · plugin off');
assert(showHidden && showHidden.getAttribute('aria-pressed') === 'false', 'the auto-task pane offers the hidden definition');
assert(button('routines-body', 'Show 1 hidden · plugin off'), 'the routine pane offers the hidden routine');
operationsSubtab = 'auto-tasks';
const beforeToggle = requests.length;
await showHidden.click(); await tick(); await tick();
assert(requests.slice(beforeToggle).every(request => request.path === '/api/auto-tasks'), 'plugin toggle refreshes the displayed pane');
operationsSubtab = 'routines';
await fetchAndRenderOperationsPane();
for (const [pane, name] of [['auto-tasks-body', 'graph-reindex'], ['routines-body', 'graph-refresh']]) {
  const group = descendants(get(pane)).find(node => String(node.className || '').includes('inactive-plugin-group'));
  assert(group && group.textContent.includes(name) && group.textContent.includes('Plugin off'), `${pane} lists ${name} in the plugin-off group`);
  assert(group.textContent.includes('--scope workspace'), `${pane} shows the skip reason naming the enable command`);
  assert(button(pane, 'Hide plugin-off definitions')?.getAttribute('aria-pressed') === 'true', `${pane} can hide them again`);
}
assert(get('auto-tasks-count').textContent === autoCountBefore, `auto-task counts exclude the inactive definition: ${get('auto-tasks-count').textContent}`);
assert(get('routines-count').textContent === routineCountBefore, 'routine counts exclude the inactive routine');
const autoSummary = descendants(get('auto-tasks-body')).find(node => String(node.className || '').includes('auto-tasks-summary'));
assert(autoSummary && !autoSummary.textContent.includes('20:05'), 'the inactive definition never becomes the next mint');
assert(!descendants(get('auto-tasks-body')).some(node => String(node.className || '').includes('auto-task-card') && node.textContent.includes('graph-reindex')), 'no toggle or mint row for the inactive definition');
await button('routines-body', 'Hide plugin-off definitions').click(); await tick(); await tick();
operationsSubtab = 'auto-tasks'; await fetchAndRenderOperationsPane();
assert(!get('auto-tasks-body').textContent.includes('graph-reindex') && !get('routines-body').textContent.includes('graph-refresh'), 'hiding again restores the default view');

globalThis.setDrainFixturePhase = async (phase) => {
  drainPhase = phase;
  drainRunId = phase === 'draining' ? 'jrun-20260923-0400-a1' : null;
  await fetchAndRenderAutoDrainPane();
};
globalThis.setDrainFixturePull = async ({ runId = null, admissionsStopped = false } = {}) => {
  pullDrainRunId = runId;
  pullDrainStopped = admissionsStopped;
  await fetchAndRenderAutoDrainPane();
};
globalThis.setDrainFixtureApprovals = async (approvals) => {
  approvalsFixture = approvals;
  await fetchAndRenderAutoDrainPane();
};
globalThis.setDrainFixtureReadiness = async ({ capacity = {}, tasks = null } = {}) => {
  drainCapacityOverride = capacity;
  drainTasksOverride = tasks;
  await fetchAndRenderAutoDrainPane();
};
globalThis.startPendingDrainReadinessRefresh = () => {
  delayReadiness = true;
  drainReadinessRefresh = fetchAndRenderAutoDrainPane();
};
globalThis.drainReadinessRequestPending = () => readinessPending;
globalThis.drainReadinessConcurrency = () => lastReadiness()?.concurrency;
globalThis.releasePendingDrainReadinessRefresh = async () => {
  // Later readiness reads in the scenario must not wait on a release.
  delayReadiness = false;
  releaseReadiness?.();
  await drainReadinessRefresh;
  drainReadinessRefresh = null;
};

// Late Start/Stop acknowledgements belong to the workspace visit that submitted them.
setWorkspace('one'); drainPhase='idle'; drainRunId=null; responseError=null; failReadiness=false; controlsAuthorized=true;
await fetchAndRenderOperations();
delayPost=true;
drainButton('Start 2h window').click(); await tick();
setWorkspace('two'); await fetchAndRenderOperations();
const beforeLateStart=requests.length;
releasePost(); await tick(); await tick(); await tick();
assert(!get('auto-drain-operation-feedback').textContent.includes('jrun-20260923-0400-a1'),'late Start cannot announce A run in B');
assert(requests.length===beforeLateStart,'late Start cannot refresh B');

setWorkspace('one'); drainPhase='draining'; drainRunId='jrun-one'; await fetchAndRenderOperations();
drainButton('Stop').click(); await tick();
setWorkspace('two'); drainPhase='idle'; drainRunId=null; await fetchAndRenderOperations();
const beforeLateStop=requests.length;
releasePost(); await tick(); await tick(); await tick();
assert(!get('auto-drain-operation-feedback').textContent.includes('Admissions stopped'),'late Stop cannot announce A outcome in B');
assert(requests.length===beforeLateStop,'late Stop cannot refresh B');
setWorkspace('one'); await fetchAndRenderOperations();
drainButton('Start 2h window').click(); await tick();
setWorkspace('two'); setWorkspace('one'); await fetchAndRenderOperations();
responseError='old visit refused';releasePost();await tick();await tick();await tick();
assert(!get('auto-drain-operation-feedback').textContent.includes('old visit refused'),'A to B to A suppresses errors from the old A visit');
delayPost=false;responseError=null;

// A refresh tick requests only what the active Automation subtab displays.
for (const [subtab, expected] of [
  ['auto-tasks', ['/api/auto-tasks']],
  ['routines', ['/api/routines']],
  ['jobs', ['/api/job-runs', '/api/routines']],
]) {
  operationsSubtab = subtab;
  const before = requests.length;
  await fetchAndRenderOperationsPane();
  const paths = requests.slice(before).filter(request => request.method === 'GET').map(request => request.path).sort();
  assert(JSON.stringify(paths) === JSON.stringify(expected), `${subtab} tick: ${JSON.stringify(paths)}`);
}
operationsSubtab = 'routines';
await fetchAndRenderOperationsPane();
const diagnostic = get('routines-body').querySelector('.automation-diagnostic');
assert(diagnostic.textContent.includes('Pending members999'), 'projected pending count is rendered');
assert(diagnostic.textContent.includes('Fresh / ready251 / 125'), 'projected fresh and ready counts are rendered');
assert(diagnostic.textContent.includes('Withheld members1000') && diagnostic.textContent.includes('Exhausted inputs200'), 'withheld and exhausted totals are preserved');
assert(diagnostic.querySelector('pre').textContent.split('\n').length === 20, 'the disclosure shows twenty withheld reasons');
assert(diagnostic.textContent.includes('Waiting for dependency 19'), 'withheld reasons are visible');
assert(diagnostic.textContent.includes('Batch member-active'), 'the active member batch remains visible');
assert(!diagnostic.textContent.includes('Usage'), 'member diagnostics have no dead Usage field');
assert(!get('auto-tasks-body').querySelector('.automation-diagnostic').textContent.includes('Usage'), 'delivery diagnostics have no dead Usage field');
const fullRequests = () => requests.filter(request => request.path.startsWith('/api/automation/'));
assert(fullRequests().length === 0, 'polling and rendering never load full membership');
const disclosure = diagnostic.querySelector('details');
disclosure.open = true;
await tick(); await tick();
assert(fullRequests().length === 1 && fullRequests()[0].workspace === 'one', 'full state loads on disclosure in the selected workspace');
assert(disclosure.textContent.includes('full-input'), 'full membership is displayed');
await fetchAndRenderOperationsPane(); await tick(); await tick();
assert(fullRequests().length === 1, 'polling reuses explicitly loaded full state');
const rebuilt = get('routines-body').querySelector('.automation-diagnostic details');
assert(rebuilt.open && rebuilt.textContent.includes('full-input'), 'full disclosure survives a poll');
fullStateError = true;
rebuilt.querySelector('button').click(); await tick(); await tick();
assert(rebuilt.textContent.includes('full state unavailable'), 'on-demand fetch failures are visible');
fullStateError = false;
rebuilt.querySelector('button').click(); await tick(); await tick();
assert(rebuilt.textContent.includes('full-input'), 'full state can be retried after an error');
// Leave the harness on the normal routines view for the layout scenarios.
rebuilt.open = false; await tick();
globalThis.operationsTestsPassed = true;
