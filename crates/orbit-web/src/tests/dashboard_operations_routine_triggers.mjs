// Runs in Chromium via dashboard_operations_browser.mjs with the browser in UTC.
// A state-triggered routine reports its own cadence, waiting text and latest
// run, and the next-hour strip plots every slot of a frequent cron routine.
const { setWorkspace } = await import('./js/common.js');
const { initOperations, fetchAndRenderOperations } = await import('./js/operations.js');
const get = id => document.getElementById(id);
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const response = payload => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
const allowed = { authorized: true, reason: null };
const capabilities = { routine_toggle: allowed, job_run: allowed, clock_service: allowed, clock_cadence: allowed, auto_task_toggle: allowed, auto_task_mint: allowed };
// The strip starts at :48, so `*/20` is due at :00, :20 and :40 and not again at :48.
const frozenNow = Date.parse('2026-10-08T12:48:00Z');
Date.now = () => frozenNow;
const routine = (name, extra) => ({
  name, source: 'one', target: 'job:fixture', enabled: true, effective: true, cron: '', trigger: {}, last_fire: null, ...extra,
});
globalThis.fetch = async (path) => {
  const url = new URL(path, 'http://dashboard.test');
  if (url.pathname === '/api/routines') return response({
    machine_name: 'fixture-host', capabilities, controls_authorized: true, cron_zone: { name: null, offset_seconds: 0 },
    routines: [
      routine('ci-failure-sweep-orbit', {
        cron: '*/20 * * * *', next_evaluation: { state: 'scheduled', at: '2026-10-08T13:00:00Z', hypothetical: false },
      }),
      routine('daily-digest', {
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
    ],
    clock: { enabled: true, configured_cadence_seconds: 60, provider: 'fixture', health: 'healthy', loaded: true, running: true, schedulable: true },
    inactive_plugin_counts: {}, retired: [],
  });
  return response({});
};
setWorkspace('one');
initOperations({ getOperationsSubtab: () => 'routines', getWorkspaces: () => [{ id: 'one', name: 'one', status: 'active' }] });
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
globalThis.routineTriggerTestsPassed = true;
