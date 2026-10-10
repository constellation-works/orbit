// Runs in Chromium via dashboard_operations_browser.mjs with the browser in UTC.
// A state-triggered routine reports its own cadence, waiting text and latest
// run, and the next-hour strip plots every slot of a frequent cron routine.
const { setWorkspace } = await import('./js/common.js');
const { initOperations, fetchAndRenderOperations, peekRoutineFailures } = await import('./js/operations.js');
const get = id => document.getElementById(id);
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const response = payload => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
const allowed = { authorized: true, reason: null };
const capabilities = { routine_toggle: allowed, job_run: allowed, clock_service: allowed, clock_cadence: allowed, auto_task_toggle: allowed, auto_task_mint: allowed };
// The strip starts at :48, so `*/20` is due at :00, :20 and :40 and not again at :48.
const frozenNow = Date.parse('2026-10-08T12:48:00Z');
Date.now = () => frozenNow;
let streakCount = 3;
let truncated = false;
const recentFires = ['failed', 'error', 'timed_out', 'succeeded', 'dispatched', 'skipped'].map((state, i) => ({
  state, attempt: 1, run_id: state === 'error' ? null : `jrun-strip-${i}`,
  slot: new Date(frozenNow - 4 * 60_000 - i * 20 * 60_000).toISOString(),
}));
const routine = (name, extra) => ({
  name, source: 'one', target: 'job:fixture', enabled: true, effective: true, cron: '', trigger: {}, last_fire: null, ...extra,
});
globalThis.fetch = async (path) => {
  const url = new URL(path, 'http://dashboard.test');
  if (url.pathname === '/api/routines') return response({
    machine_name: 'fixture-host', capabilities, controls_authorized: true, cron_zone: { name: null, offset_seconds: 0 },
    routines: [
      routine('ci-failure-sweep-orbit', {
        recent_fires: recentFires, last_fire: recentFires[0], failure_streak: { count: streakCount, since: '2026-10-08T12:04:00Z', truncated },
        cron: '*/20 * * * *', next_evaluation: { state: 'scheduled', at: '2026-10-08T13:00:00Z', hypothetical: false },
      }),
      routine('daily-digest', {
        recent_fires: [], failure_streak: { count: 1, since: '2026-10-08T10:00:00Z', truncated: false },
        cron: '30 13 * * *', next_evaluation: { state: 'scheduled', at: '2026-10-08T13:30:00Z', hypothetical: false },
      }),
      routine('task-pilot-orbit', {
        trigger: { state: { kind: 'preparation_eligible', debounce_minutes: 2 } },
        next_evaluation: { state: 'waiting', at: null },
        last_fire: {
          state: 'succeeded', run_id: 'jrun-20261008-1245-t1', slot: '2026-10-08T12:45:00+00:00',
          started_at: '2026-10-08T12:45:00Z', finished_at: '2026-10-08T12:46:00Z', duration_ms: 60000,
        },
      }),
      routine('delivery-review', {
        trigger: { deliveries_landed: { threshold: 3, branch: 'agent-main' } },
        next_evaluation: { state: 'waiting', at: null },
      }),
      routine('another-workspace-failure', { source: 'two', failure_streak: { count: 7, since: '2026-10-08T06:00:00Z' } }),
    ],
    clock: { enabled: true, configured_cadence_seconds: 60, provider: 'fixture', health: 'healthy', loaded: true, running: true, schedulable: true },
    inactive_plugin_counts: {}, retired: [],
  });
  return response({});
};
setWorkspace('one');
initOperations({ getOperationsSubtab: () => 'routines', getWorkspaces: () => [{ id: 'one', name: 'one', status: 'active' }, { id: 'two', name: 'two', status: 'active' }] });
await fetchAndRenderOperations('routines');

const rowText = name => Array.from(get('routines-body').querySelectorAll('.operation-row')).find(row => row.textContent.includes(name))?.textContent || '';
const pilot = rowText('task-pilot-orbit');
assert(pilot.includes('on task creation/edit · debounce 2m'), `a state trigger describes its cadence: ${pilot}`);
assert(pilot.includes('Waiting for task creation or edits'), `a state trigger waits for task changes: ${pilot}`);
assert(!/deliver/i.test(pilot), `a state trigger never mentions deliveries: ${pilot}`);
assert(pilot.includes('jrun-20261008-1245-t1'), `the latest run of the state trigger is shown: ${pilot}`);
const delivery = rowText('delivery-review');
assert(delivery.includes('3 verified deliveries on agent-main') && delivery.includes('Waiting for deliveries'), `delivery routines keep their wording: ${delivery}`);

const ticks = Array.from(get('routines-body').querySelectorAll('.operation-timeline-tick'));
const named = name => ticks.filter(tick => tick.title.startsWith(`${name} ·`));
assert(named('ci-failure-sweep-orbit').length === 3, `*/20 from :48 is due at :00, :20 and :40: ${ticks.map(tick => tick.title).join(' | ')}`);
assert(named('daily-digest').length === 1, 'a daily routine is due once in the hour');
assert(named('task-pilot-orbit').length === 0, 'an event-driven routine has no slot to plot');
assert(ticks.filter(tick => tick.querySelector('.operation-timeline-label')).length === 2, 'only the first slot of a routine carries its name');
const summary = get('routines-body').querySelector('.operation-timeline-summary').textContent;
assert(summary === '4 fires from 4 active routines', `the header counts every slot: ${summary}`);

const sweepRow = () => Array.from(get('routines-body').querySelectorAll('.routine-card')).find(row => row.querySelector('strong').textContent === 'ci-failure-sweep-orbit');
const warning = get('routines-body').querySelector('.routine-failures');
assert(warning && warning.nextElementSibling?.classList.contains('operation-timeline'), 'failure warning appears above the timeline and rows');
assert(warning.textContent.includes('ci-failure-sweep-orbit') && !warning.textContent.includes('daily-digest') && !warning.textContent.includes('another-workspace-failure'), 'only the selected workspace routines with at least three consecutive failures are flagged');
const label = sweepRow().querySelector('.routine-failure-streak').textContent;
assert(label.includes('3 fires') && label.includes('12:04'), `streak label includes count and start time: ${label}`);
const strip = sweepRow().querySelector('.routine-fire-strip');
assert(strip.children.length === 6, 'each recent fire renders a dot');
assert(strip.firstElementChild.getAttribute('aria-label').startsWith('skipped') && strip.lastElementChild.getAttribute('aria-label').startsWith('failed'), 'recent fires are displayed oldest to newest');
assert(strip.querySelectorAll('a').length === 5 && strip.querySelectorAll('.operation-dot.failed').length === 3, 'failures, timeouts and errors share their failure tone, and only dispatched fires link');
const error = Array.from(strip.children).find(node => node.getAttribute('aria-label').startsWith('error'));
assert(error.tagName === 'SPAN' && error.title.includes('No run'), 'an undispatched error has an informative tooltip without a dead link');
const badge = get('rail-count-routine-failures');
assert(!badge.hidden && badge.textContent.startsWith('1 ') && badge.title.includes('ci-failure-sweep-orbit'), 'Health rail flags the failing routine independently of doctor');
assert(!Array.from(get('routines-body').querySelectorAll('.routine-card')).find(row => row.querySelector('strong').textContent === 'daily-digest').querySelector('.routine-fire-strip'), 'an empty history has no strip');

// Exercise the shipped router, so the dot must navigate to the correct run
// with the source workspace retained rather than merely containing an href.
const { initRouter, setActiveTab } = await import('./js/router.js');
let runId = null;
let expanded = new Set();
initRouter({
  getRunId: () => runId, setRunId: value => { runId = value; },
  getExpandedSteps: () => expanded, setExpandedSteps: value => { expanded = value; },
  setRunDetail: () => {}, setRunEvents: () => {}, setRunLogs: () => {},
  getRunSubtab: () => 'steps', setRunSubtab: () => {},
  setTab: () => {}, getOperationsSubtab: () => 'routines', setOperationsSubtab: () => {},
  refreshDashboard: () => {},
});
strip.lastElementChild.click();
assert(runId === 'jrun-strip-0' && location.hash === '#runs/jrun-strip-0' && new URL(location.href).searchParams.get('workspace') === 'one', 'clicking the newest dot opens its run in the originating workspace');
setActiveTab('operations/routines');

streakCount = 2;
await fetchAndRenderOperations('routines');
assert(!get('routines-body').querySelector('.routine-failures') && badge.hidden, 'two failures retain their row label but clear the header and Health warnings');
streakCount = 100;
truncated = true;
await fetchAndRenderOperations('routines');
assert(sweepRow().querySelector('.routine-failure-streak').textContent.includes('at least 100') && sweepRow().querySelector('.routine-failure-streak').textContent.includes('or earlier'), 'bounded history never claims an exact streak or exact starting time');

setWorkspace('two');
assert(badge.hidden, 'workspace change immediately clears the Health warning');
await peekRoutineFailures();
assert(!badge.hidden && badge.title.includes('another-workspace-failure'), 'background peek populates the warning without opening Routines');
const originalFetch = globalThis.fetch;
let release;
globalThis.fetch = () => new Promise(resolve => { release = resolve; });
const pending = peekRoutineFailures();
setWorkspace('one');
release(await originalFetch('/api/routines'));
await pending;
assert(badge.hidden, 'a stale peek reply cannot restore the previous workspace warning');
globalThis.fetch = originalFetch;
await peekRoutineFailures();
assert(!badge.hidden && badge.title.includes('ci-failure-sweep-orbit'), 'a fresh peek updates the current workspace');
// Two reads of the same visit can complete out of order too.
const releases = [];
globalThis.fetch = () => new Promise(resolve => releases.push(resolve));
streakCount = 0;
const olderPayload = await originalFetch('/api/routines');
const older = peekRoutineFailures();
streakCount = 3;
truncated = false;
const newerPayload = await originalFetch('/api/routines');
const newer = peekRoutineFailures();
releases[1](newerPayload);
await newer;
releases[0](olderPayload);
await older;
assert(!badge.hidden && badge.title.includes('ci-failure-sweep-orbit'), 'an older read cannot clear a newer warning in the same visit');
globalThis.fetch = originalFetch;
await fetchAndRenderOperations('routines');
globalThis.routineTriggerTestsPassed = true;
