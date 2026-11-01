// Runs in Chromium via dashboard_operations_browser.mjs with the browser in UTC
// and its clock frozen to an explicit summer, winter, or pre-transition instant.
// Cron is evaluated in the host's zone (America/Los_Angeles here). A row that
// has a next evaluation is labelled with that instant's abbreviation, so the
// wall time and the UTC next fire name one instant even when the browser clock
// is on the other side of a daylight-saving change. A row with no next
// evaluation uses the stable generic name and never today's abbreviation.
const { setWorkspace } = await import('./js/common.js');
const { initOperations, fetchAndRenderOperations, fetchAndRenderJobs } = await import('./js/operations.js');
const { initRouter, setActiveTab } = await import('./js/router.js');
const get = id => document.getElementById(id);
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const response = payload => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
const allowed = { authorized: true, reason: null };
const capabilities = {
  routine_toggle: allowed, routine_trigger: allowed, job_run: allowed,
  clock_service: allowed, clock_cadence: allowed,
  auto_task_toggle: allowed, auto_task_mint: allowed, auto_task_schedule: allowed,
};
// 2026-11-01T09:00:00Z is the Pacific fallback. 08:59Z is still PDT; 15:00Z is 07:00 PST.
const SUMMER_AT = '2026-10-08T14:00:00Z';
const WINTER_AT = '2026-12-10T15:00:00Z';
const TRANSITION_AT = '2026-11-01T15:00:00Z';
const FROZEN_CLOCKS = ['2026-07-15T18:00:00.000Z', '2026-12-15T18:00:00.000Z', '2026-11-01T08:59:00.000Z'];
const definition = (name, cron, next) => ({
  name, enabled: true, template: { title: name }, template_summary: name, dedupe: 'skip_if_open',
  schedule: { cron }, schedule_summary: cron, next_evaluation: next,
});
const routine = (name, cron, at, target) => ({
  name, source: 'one', target, enabled: true, cron, trigger: { cron },
  next_evaluation: at ? { state: 'scheduled', at, hypothetical: false } : { state: 'waiting', at: null },
});
const catalogJobs = [
  { job_id: 'fixture', manual_runnable: true },
  { job_id: 'winter', manual_runnable: true },
  { job_id: 'transition', manual_runnable: true },
  { job_id: 'unscheduled', manual_runnable: true },
];
let zone = { name: 'America/Los_Angeles', offset_seconds: -7 * 3600 };
globalThis.fetch = async (path) => {
  const url = new URL(path, 'http://dashboard.test');
  if (url.pathname === '/api/auto-tasks') return response({
    workspace: 'one', capabilities, controls_authorized: true, cron_zone: zone,
    definitions: [
      definition('code-org-sweep', '0 7 * * 4', { state: 'scheduled', at: SUMMER_AT }),
      definition('friction-curation', '15 7 * * *', { state: 'scheduled', at: '2026-10-08T14:15:00Z' }),
      definition('winter-digest', '0 7 * * *', { state: 'scheduled', at: WINTER_AT }),
      definition('transition-digest', '0 7 * * *', { state: 'scheduled', at: TRANSITION_AT }),
      definition('unscheduled', '0 7 * * 4', { state: 'waiting', at: null }),
    ],
  });
  if (url.pathname === '/api/routines') return response({
    machine_name: 'fixture-host', capabilities, controls_authorized: true, cron_zone: zone,
    routines: [
      routine('dependabot-alert-sweep', '25 3 * * *', '2026-10-08T10:25:00Z', 'job:fixture'),
      routine('winter-sweep', '0 7 * * *', WINTER_AT, 'job:winter'),
      routine('transition-sweep', '0 7 * * *', TRANSITION_AT, 'job:transition'),
      routine('unscheduled-sweep', '0 7 * * 4', null, 'job:unscheduled'),
    ],
    clock: { enabled: true, configured_cadence_seconds: 60, provider: 'fixture', health: 'healthy', loaded: true, running: true, schedulable: true },
    inactive_plugin_counts: {}, retired: [],
  });
  if (url.pathname === '/api/jobs') return response(catalogJobs);
  if (url.pathname === '/api/jobs/failure-streaks') return response({ failing: [] });
  if (url.pathname === '/api/job-runs') return response({ items: [], total: 0, limit: 100, truncated: false });
  return response({});
};
let jobsSubtab = 'catalogue';
setWorkspace('one');
initOperations({ getOperationsSubtab: () => 'auto-tasks', getWorkspaces: () => [{ id: 'one', name: 'one', status: 'active' }] });
initRouter({
  getRunId: () => null, setRunId: () => {},
  getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
  setRunDetail: () => {}, setRunEvents: () => {}, setRunLogs: () => {},
  getRunSubtab: () => 'steps', setRunSubtab: () => {},
  setTab: () => {}, getOperationsSubtab: () => 'auto-tasks', setOperationsSubtab: () => {},
  getJobsSubtab: () => jobsSubtab, setJobsSubtab: (value) => { jobsSubtab = value; },
  refreshDashboard: () => {},
});
const rowEl = (id, name) => Array.from(get(id).querySelectorAll('.operation-row')).find(row => row.textContent.includes(name)) || null;
const rowText = (id, name) => rowEl(id, name)?.textContent || '';
const cellText = (id, name, label) => rowEl(id, name)?.querySelector(`.operation-cell[data-label="${label}"]`)?.textContent || '';
const chipText = (jobId) => get('jobs-body')?.querySelector(`.job-card[data-job="${jobId}"]`)?.textContent || '';
const clockLabel = () => new Intl.DateTimeFormat('en-US', { timeZone: 'America/Los_Angeles', timeZoneName: 'short' })
  .formatToParts(new Date()).find(part => part.type === 'timeZoneName')?.value || '';

try {
  await Promise.all([fetchAndRenderOperations('routines'), fetchAndRenderOperations('auto-tasks')]);
  await fetchAndRenderJobs();
  setActiveTab('operations/auto-tasks');
  globalThis.showCronZonePanel = (route) => setActiveTab(route);
  globalThis.cronZoneFrozenObserved = new Date().toISOString();
  globalThis.cronZoneRendered = true;

  assert(Intl.DateTimeFormat().resolvedOptions().timeZone === 'UTC', 'the browser fixture runs in UTC');
  const frozen = globalThis.cronZoneFrozenObserved;
  assert(FROZEN_CLOCKS.includes(frozen), `browser clock is an explicit summer, winter or transition instant, not wall time: ${frozen}`);
  const today = clockLabel();

  const sweep = rowText('auto-tasks-body', 'code-org-sweep');
  assert(sweep.includes('weekly Thu 07:00 PDT') && sweep.includes('14:00 UTC'), `07:00 PDT and 14:00 UTC name one instant: ${sweep}`);
  assert(!sweep.includes('07:00 UTC'), `no host-local trigger is labelled UTC: ${sweep}`);
  assert(!sweep.includes('weekly Thu 07:00 PST'), `October fire is not labelled with the other season (${today} now): ${sweep}`);
  assert(rowText('auto-tasks-body', 'friction-curation').includes('daily 07:15 PDT'), 'daily triggers name the host zone');

  const winter = rowText('auto-tasks-body', 'winter-digest');
  assert(winter.includes('daily 07:00 PST') && winter.includes('15:00 UTC'), `07:00 PST and 15:00 UTC name the winter instant: ${winter}`);
  assert(!winter.includes('daily 07:00 PDT'), `winter fire is not labelled with today's ${today}: ${winter}`);
  const transition = rowText('auto-tasks-body', 'transition-digest');
  assert(transition.includes('daily 07:00 PST') && transition.includes('15:00 UTC'), `07:00 PST and 15:00 UTC name the post-fallback instant: ${transition}`);
  assert(!transition.includes('daily 07:00 PDT'), `pre-transition clock ${today} is not applied to the 15:00Z fire: ${transition}`);

  const missing = cellText('auto-tasks-body', 'unscheduled', 'Trigger');
  assert(missing.includes('weekly Thu 07:00 PT'), `a cadence with no next evaluation uses the generic zone: ${missing}`);
  assert(!missing.includes('PDT') && !missing.includes('PST'), `no current-clock abbreviation on an unscheduled cadence (${today} now): ${missing}`);

  const routineRow = rowText('routines-body', 'dependabot-alert-sweep');
  assert(routineRow.includes('daily 03:25 PDT') && routineRow.includes('10:25 UTC'), `routine trigger names the host zone: ${routineRow}`);
  const winterRoutine = cellText('routines-body', 'winter-sweep', 'Cadence');
  assert(winterRoutine.includes('daily 07:00 PST') && !winterRoutine.includes('PDT'), `routine cadence uses the winter instant, not ${today}: ${winterRoutine}`);
  const missingRoutine = cellText('routines-body', 'unscheduled-sweep', 'Cadence');
  assert(missingRoutine.includes('weekly Thu 07:00 PT') && !missingRoutine.includes('PDT') && !missingRoutine.includes('PST'), `routine with no next evaluation stays generic: ${missingRoutine}`);

  const winterChip = chipText('winter');
  assert(winterChip.includes('winter-sweep') && winterChip.includes('daily 07:00 PST') && !winterChip.includes('daily 07:00 PDT'), `job chip uses the routine's next instant, not ${today}: ${winterChip}`);
  const missingChip = chipText('unscheduled');
  assert(missingChip.includes('unscheduled-sweep') && missingChip.includes('weekly Thu 07:00 PT'), `job chip with no next evaluation uses the generic zone: ${missingChip}`);
  assert(!missingChip.includes('PDT') && !missingChip.includes('PST'), `job chip does not borrow today's ${today}: ${missingChip}`);

  const openPreview = (bodyId, name) => {
    const edit = rowEl(bodyId, name)?.querySelector('button.schedule-edit');
    assert(edit && !edit.disabled, `schedule editor opens for ${name}`);
    edit.click();
    return rowEl(bodyId, name)?.querySelector('.schedule-preview')?.textContent || '';
  };
  const closePreview = (bodyId, name) => {
    const cancel = Array.from(rowEl(bodyId, name)?.querySelectorAll('button') || []).find((button) => button.textContent === 'Cancel');
    assert(cancel, `schedule editor cancels for ${name}`);
    cancel.click();
  };
  const summerPreview = openPreview('auto-tasks-body', 'code-org-sweep');
  assert(summerPreview.includes('weekly Thu 07:00 PDT'), `editor preview uses the row's next instant: ${summerPreview}`);
  closePreview('auto-tasks-body', 'code-org-sweep');
  const missingPreview = openPreview('auto-tasks-body', 'unscheduled');
  assert(missingPreview.includes('weekly Thu 07:00 PT') && !missingPreview.includes('PDT') && !missingPreview.includes('PST'), `editor preview with no next evaluation stays generic: ${missingPreview}`);
  closePreview('auto-tasks-body', 'unscheduled');
  const routinePreview = openPreview('routines-body', 'transition-sweep');
  assert(routinePreview.includes('daily 07:00 PST'), `routine editor preview uses the post-fallback instant: ${routinePreview}`);
  closePreview('routines-body', 'transition-sweep');

  // A host whose IANA name is unknown is still labelled by its offset.
  zone = { name: null, offset_seconds: 5.5 * 3600 };
  await fetchAndRenderOperations('auto-tasks');
  await fetchAndRenderOperations('routines');
  assert(rowText('auto-tasks-body', 'code-org-sweep').includes('weekly Thu 07:00 UTC+05:30'), 'offset label when the zone name is unknown');
  const offsetWinter = cellText('auto-tasks-body', 'winter-digest', 'Trigger');
  assert(offsetWinter.includes('daily 07:00 UTC+05:30') && !offsetWinter.includes('PST') && !offsetWinter.includes('PDT'), `offset zone does not invent a seasonal abbreviation: ${offsetWinter}`);
  const offsetMissing = cellText('auto-tasks-body', 'unscheduled', 'Trigger');
  assert(offsetMissing.includes('weekly Thu 07:00 UTC+05:30') && !offsetMissing.includes('PT'), `offset zone does not invent a generic name: ${offsetMissing}`);

  zone = { name: 'Not/AZone', offset_seconds: -7 * 3600 };
  await fetchAndRenderOperations('auto-tasks');
  const unresolved = cellText('auto-tasks-body', 'transition-digest', 'Trigger');
  assert(unresolved.includes('daily 07:00 UTC-07:00') && !unresolved.includes('PDT') && !unresolved.includes('PST'), `an unresolvable zone name falls back to the reported offset: ${unresolved}`);

  zone = { name: 'Not/AZone' };
  await fetchAndRenderOperations('auto-tasks');
  const noOffset = cellText('auto-tasks-body', 'code-org-sweep', 'Trigger');
  assert(noOffset.includes('weekly Thu 07:00') && !noOffset.includes('PDT') && !noOffset.includes('PST') && !noOffset.includes('UTC') && !/PT/.test(noOffset), `no zone claim when the name and offset are both unknown: ${noOffset}`);

  // Screenshots show the named zone again, after the offset cases have run.
  zone = { name: 'America/Los_Angeles', offset_seconds: -7 * 3600 };
  await Promise.all([fetchAndRenderOperations('routines'), fetchAndRenderOperations('auto-tasks')]);
  await fetchAndRenderJobs();
  setActiveTab('operations/auto-tasks');
  assert(cellText('auto-tasks-body', 'code-org-sweep', 'Trigger').includes('weekly Thu 07:00 PDT'), 'restored summer label');
  assert(cellText('auto-tasks-body', 'winter-digest', 'Trigger').includes('daily 07:00 PST'), 'restored winter label');
  assert(cellText('auto-tasks-body', 'transition-digest', 'Trigger').includes('daily 07:00 PST'), 'restored transition label');
  assert(cellText('auto-tasks-body', 'unscheduled', 'Trigger').includes('weekly Thu 07:00 PT'), 'restored generic label');

  globalThis.cronZoneTestsPassed = true;
} catch (error) {
  globalThis.cronZoneError = error instanceof Error ? error.message : String(error);
}
