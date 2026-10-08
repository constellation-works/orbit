// Runs in Chromium via dashboard_operations_browser.mjs with the browser in UTC.
// Cron is evaluated in the host's zone (America/Los_Angeles here), so a trigger
// is labelled in that zone and names the same instant as the next fire beside it.
const { setWorkspace } = await import('./js/common.js');
const { initOperations, fetchAndRenderOperations } = await import('./js/operations.js');
const get = id => document.getElementById(id);
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const response = payload => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
const allowed = { authorized: true, reason: null };
const capabilities = { routine_toggle: allowed, job_run: allowed, clock_service: allowed, clock_cadence: allowed, auto_task_toggle: allowed, auto_task_mint: allowed };
const cronZone = { name: 'America/Los_Angeles', offset_seconds: -7 * 3600 };
const definition = (name, cron, at) => ({
  name, enabled: true, template: { title: name }, template_summary: name, dedupe: 'skip_if_open',
  schedule: { cron }, schedule_summary: cron, next_evaluation: { state: 'scheduled', at },
});
let zone = cronZone;
globalThis.fetch = async (path) => {
  const url = new URL(path, 'http://dashboard.test');
  if (url.pathname === '/api/auto-tasks') return response({
    workspace: 'one', capabilities, controls_authorized: true, cron_zone: zone,
    definitions: [
      definition('code-org-sweep', '0 7 * * 4', '2026-10-08T14:00:00Z'),
      definition('friction-curation', '15 7 * * *', '2026-10-08T14:15:00Z'),
    ],
  });
  if (url.pathname === '/api/routines') return response({
    machine_name: 'fixture-host', capabilities, controls_authorized: true, cron_zone: zone,
    routines: [{
      name: 'dependabot-alert-sweep', source: 'one', target: 'job:fixture', enabled: true, cron: '25 3 * * *',
      next_evaluation: { state: 'scheduled', at: '2026-10-08T10:25:00Z', hypothetical: false },
    }],
    clock: { enabled: true, configured_cadence_seconds: 60, provider: 'fixture', health: 'healthy', loaded: true, running: true, schedulable: true },
    inactive_plugin_counts: {}, retired: [],
  });
  return response({});
};
setWorkspace('one');
initOperations({ getOperationsSubtab: () => 'auto-tasks', getWorkspaces: () => [{ id: 'one', name: 'one', status: 'active' }] });
const text = id => get(id).textContent;
const rowText = (id, name) => Array.from(get(id).querySelectorAll('.operation-row')).find(row => row.textContent.includes(name))?.textContent || '';

await Promise.all([fetchAndRenderOperations('routines'), fetchAndRenderOperations('auto-tasks')]);
assert(Intl.DateTimeFormat().resolvedOptions().timeZone === 'UTC', 'the browser fixture runs in UTC');
const sweep = rowText('auto-tasks-body', 'code-org-sweep');
assert(sweep.includes('weekly Thu 07:00 PDT') && sweep.includes('14:00 UTC'), `07:00 PDT and 14:00 UTC name one instant: ${sweep}`);
assert(!sweep.includes('07:00 UTC'), `no host-local trigger is labelled UTC: ${sweep}`);
assert(rowText('auto-tasks-body', 'friction-curation').includes('daily 07:15 PDT'), 'daily triggers name the host zone');
const routine = rowText('routines-body', 'dependabot-alert-sweep');
assert(routine.includes('daily 03:25 PDT') && routine.includes('10:25 UTC'), `routine trigger names the host zone: ${routine}`);

// A host whose IANA name is unknown is still labelled by its offset.
zone = { name: null, offset_seconds: 5.5 * 3600 };
await fetchAndRenderOperations('auto-tasks');
assert(rowText('auto-tasks-body', 'code-org-sweep').includes('weekly Thu 07:00 UTC+05:30'), 'offset label when the zone name is unknown');
globalThis.cronZoneTestsPassed = true;
