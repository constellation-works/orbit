// Runs in Chromium via dashboard_operations_browser.mjs.
// An open editor keeps the schedule/trigger it observed when it opened, even
// after a background refresh and opening another editor (ORB-18356).
const { setWorkspace } = await import('./js/common.js');
const { initOperations, fetchAndRenderOperations } = await import('./js/operations.js');
const get = id => document.getElementById(id);
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const response = (payload, status = 200) => ({ ok: status === 200, status, json: async () => payload, text: async () => JSON.stringify(payload) });
const allowed = { authorized: true, reason: null };
const capabilities = { routine_toggle: allowed, routine_trigger: allowed, job_run: allowed, clock_service: allowed, clock_cadence: allowed, auto_task_toggle: allowed, auto_task_mint: allowed, auto_task_schedule: allowed };
const tick = async (n = 10) => { for (let i = 0; i < n; i++) await new Promise(r => setTimeout(r, 0)); };
const clone = v => JSON.parse(JSON.stringify(v));

let autoMinutes = { Alpha: 15, Beta: 20 };
let schedulePost = null;
let routineTrigger = { Alpha: { cron: '0 9 * * *', missed_run: 'skip' }, Beta: { cron: '0 8 * * *', missed_run: 'skip' } };
let routineTarget = { Alpha: 'job:alpha', Beta: 'job:beta' };
let triggerPost = null;

globalThis.fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  if (url.pathname === '/api/auto-tasks/schedule' && options.method === 'POST') {
    schedulePost = JSON.parse(options.body);
    if (JSON.stringify(schedulePost.expected_schedule) !== JSON.stringify({ every_minutes: autoMinutes[schedulePost.name] })) {
      return response({ error: 'stale_auto_task_schedule: schedule changed since the editor opened' }, 409);
    }
    autoMinutes[schedulePost.name] = schedulePost.schedule.every_minutes;
    return response({ message: 'Saved' });
  }
  if (url.pathname === '/api/routines/trigger' && options.method === 'POST') {
    triggerPost = JSON.parse(options.body);
    if (JSON.stringify(triggerPost.expected_trigger) !== JSON.stringify(routineTrigger[triggerPost.name])
      || triggerPost.target !== routineTarget[triggerPost.name]) {
      return response({ error: 'stale_routine_trigger: trigger changed since the editor opened' }, 409);
    }
    routineTrigger[triggerPost.name] = clone(triggerPost.trigger);
    return response({ message: 'Saved' });
  }
  if (url.pathname === '/api/auto-tasks') return response({
    cron_zone: { name: null, offset_seconds: 0 }, capabilities, definitions: [
      { name: 'Alpha', enabled: true, schedule: { every_minutes: autoMinutes.Alpha }, schedule_summary: `every ${autoMinutes.Alpha} minutes`, template: { title: 'Alpha' }, template_summary: 'Alpha', next_evaluation: { state: 'scheduled', at: '2026-09-07T22:00:00Z' } },
      { name: 'Beta', enabled: true, schedule: { every_minutes: autoMinutes.Beta }, schedule_summary: `every ${autoMinutes.Beta} minutes`, template: { title: 'Beta' }, template_summary: 'Beta', next_evaluation: { state: 'scheduled', at: '2026-09-07T22:00:00Z' } },
    ],
  });
  if (url.pathname === '/api/routines') return response({
    machine_name: 'fixture-host', cron_zone: { name: null, offset_seconds: 0 }, capabilities, session_explanation: 'fixture',
    routines: [
      { name: 'Alpha', source: 'one', target: routineTarget.Alpha, enabled: true, cron: routineTrigger.Alpha.cron, trigger: clone(routineTrigger.Alpha), next_evaluation: { state: 'scheduled', at: '2026-09-07T22:00:00Z' } },
      { name: 'Beta', source: 'one', target: routineTarget.Beta, enabled: true, cron: routineTrigger.Beta.cron, trigger: clone(routineTrigger.Beta), next_evaluation: { state: 'scheduled', at: '2026-09-07T22:00:00Z' } },
    ],
    clock: { enabled: true, configured_cadence_seconds: 60, provider: 'fixture', health: 'healthy', loaded: true, running: true, schedulable: true },
    inactive_plugin_counts: {}, retired: [],
  });
  return response({});
};
setWorkspace('one');
initOperations({ getOperationsSubtab: () => 'routines', getWorkspaces: () => [{ id: 'one', name: 'one', status: 'active' }] });

// --- Auto-task: Alpha 15 -> draft 30, intervening 60, open Beta, save Alpha ---
await fetchAndRenderOperations('auto-tasks');
const autoCard = name => Array.from(get('auto-tasks-body').querySelectorAll('.auto-task-card')).find(c => c.querySelector('strong')?.textContent === name);
const autoEditor = name => autoCard(name)?.querySelector('.schedule-editor');
const autoInput = name => autoEditor(name)?.querySelector('input.schedule-input');
autoCard('Alpha').querySelector('.schedule-edit').click();
assert(autoEditor('Alpha'), 'Alpha auto-task editor opens');
autoInput('Alpha').value = '30';
autoInput('Alpha').dispatchEvent(new Event('input', { bubbles: true }));
autoMinutes.Alpha = 60;
await fetchAndRenderOperations('auto-tasks');
assert(autoInput('Alpha')?.value === '30', `background refresh keeps Alpha draft 30, got ${autoInput('Alpha')?.value}`);
autoCard('Beta').querySelector('.schedule-edit').click();
assert(autoEditor('Beta') && autoEditor('Alpha'), 'opening Beta keeps Alpha editor');
assert(autoInput('Alpha')?.value === '30', 'Alpha draft survives Beta opening');
autoEditor('Alpha').querySelector('.schedule-editor-controls button').click();
await tick();
assert(schedulePost?.name === 'Alpha' && schedulePost.expected_schedule?.every_minutes === 15 && schedulePost.schedule?.every_minutes === 30,
  `save posts original expected_schedule 15, got ${JSON.stringify(schedulePost)}`);
assert(autoEditor('Alpha'), 'stale refusal keeps Alpha editor open');
assert(autoInput('Alpha')?.value === '30', 'stale refusal preserves draft 30');
assert(autoEditor('Alpha').querySelector('.schedule-error')?.textContent.includes('stale_auto_task_schedule'),
  `stale refusal is shown: ${autoEditor('Alpha').querySelector('.schedule-error')?.textContent}`);
autoEditor('Alpha').querySelectorAll('.schedule-editor-controls button')[1].click();
await tick();
autoEditor('Beta')?.querySelectorAll('.schedule-editor-controls button')[1]?.click();
await tick();

// --- Routine: Alpha cron 0 9 -> draft 0 10, intervening 0 11 + target, open Beta ---
await fetchAndRenderOperations('routines');
const routineCard = name => Array.from(get('routines-body').querySelectorAll('.routine-card')).find(c => c.querySelector('.operation-identity strong')?.textContent === name);
const routineEditorEl = name => routineCard(name)?.querySelector('.schedule-editor');
const routineInput = name => routineEditorEl(name)?.querySelector('input.schedule-input');
routineCard('Alpha').querySelector('.schedule-edit').click();
assert(routineEditorEl('Alpha'), 'Alpha routine editor opens');
routineInput('Alpha').value = '0 10 * * *';
routineInput('Alpha').dispatchEvent(new Event('input', { bubbles: true }));
routineTrigger.Alpha = { cron: '0 11 * * *', missed_run: 'skip' };
routineTarget.Alpha = 'job:alpha-changed';
await fetchAndRenderOperations('routines');
assert(routineInput('Alpha')?.value === '0 10 * * *', `background refresh keeps routine draft, got ${routineInput('Alpha')?.value}`);
routineCard('Beta').querySelector('.schedule-edit').click();
assert(routineEditorEl('Beta') && routineEditorEl('Alpha'), 'opening Beta routine keeps Alpha editor');
routineEditorEl('Alpha').querySelector('.schedule-editor-controls button').click();
await tick();
assert(triggerPost?.name === 'Alpha' && triggerPost.source === 'one' && triggerPost.target === 'job:alpha'
  && triggerPost.expected_trigger?.cron === '0 9 * * *' && triggerPost.trigger?.cron === '0 10 * * *',
  `routine save posts original source/target/expected_trigger, got ${JSON.stringify(triggerPost)}`);
assert(routineEditorEl('Alpha') && routineInput('Alpha')?.value === '0 10 * * *', 'routine stale refusal preserves draft');
assert(routineEditorEl('Alpha').querySelector('.schedule-error')?.textContent.includes('stale'),
  `routine stale refusal is shown: ${routineEditorEl('Alpha').querySelector('.schedule-error')?.textContent}`);

globalThis.scheduleStaleTestsPassed = true;
