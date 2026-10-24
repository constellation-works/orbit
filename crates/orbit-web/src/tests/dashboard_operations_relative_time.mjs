// Runs in Chromium via dashboard_operations_browser.mjs with the clock frozen.
// A minute remainder that rounds up to 60 must carry into the hour, so no
// relative time reads "60 min" or "18 h 60 min" [ORB-17853].
const { setWorkspace } = await import('./js/common.js');
const { initOperations, fetchAndRenderOperations } = await import('./js/operations.js');
const get = id => document.getElementById(id);
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const response = payload => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
const allowed = { authorized: true, reason: null };
const capabilities = { routine_toggle: allowed, job_run: allowed, clock_service: allowed, clock_cadence: allowed, auto_task_toggle: allowed, auto_task_mint: allowed };
const now = Date.now();
const minute = 60_000;
let nextAt = now;
let lastFiredAt = now;
globalThis.fetch = async (path) => {
  const url = new URL(path, 'http://dashboard.test');
  if (url.pathname === '/api/auto-tasks') return response({
    workspace: 'one', capabilities, controls_authorized: true, cron_zone: { name: 'UTC', offset_seconds: 0 },
    definitions: [{
      name: 'code-org-sweep', enabled: true, template: { title: 'Code org sweep' }, template_summary: 'Code org sweep', dedupe: 'skip_if_open',
      schedule: { cron: '27 3 * * *' }, schedule_summary: 'daily 03:27 UTC',
      last_minted_task_id: 'ORB-00099', last_minted_task_status: 'done', last_evaluation: { kind: 'fired', last_task_id: 'ORB-00099', last_fired_at: new Date(lastFiredAt).toISOString() },
      next_evaluation: { state: 'scheduled', at: new Date(nextAt).toISOString() },
    }],
  });
  return response({});
};
setWorkspace('one');
initOperations({ getOperationsSubtab: () => 'auto-tasks', getWorkspaces: () => [{ id: 'one', name: 'one', status: 'active' }] });
// Show the Auto-tasks view through the shipped router so the screenshot is the real panel.
const { initRouter, setActiveTab } = await import('./js/router.js');
initRouter({
  getRunId: () => null, setRunId: () => {},
  getExpandedSteps: () => new Set(), setExpandedSteps: () => {},
  setRunDetail: () => {}, setRunEvents: () => {}, setRunLogs: () => {},
  getRunSubtab: () => 'steps', setRunSubtab: () => {},
  setTab: () => {}, getOperationsSubtab: () => 'auto-tasks', setOperationsSubtab: () => {},
  refreshDashboard: () => {},
});
setActiveTab('operations/auto-tasks');
const nextMint = () => Array.from(get('auto-tasks-body').querySelectorAll('.operation-stat')).find(node => node.textContent.startsWith('Next mint'))?.querySelector('.operation-stat-value')?.textContent;
const lastMintedAges = () => Array.from(get('auto-tasks-body').querySelectorAll('.operation-cell-sub')).map(node => node.textContent);

// Each offset is a whole-minute remainder that rounded up to 60 before the fix,
// or a value just under the boundary that must keep its minutes.
const cases = [
  [59 * minute + 45_000, 'in 1 h'],
  [59 * minute + 20_000, 'in 59 min'],
  [18 * 3_600_000 + 59 * minute + 40_000, 'in 19 h'],
];
for (const [offset, expected] of cases) {
  nextAt = now + offset;
  await fetchAndRenderOperations('auto-tasks');
  assert(nextMint() === expected, `next mint ${offset} ms ahead reads ${JSON.stringify(nextMint())}, expected ${expected}`);
}
lastFiredAt = now - (59 * minute + 45_000);
await fetchAndRenderOperations('auto-tasks');
assert(lastMintedAges().some(text => text.endsWith('1 h ago')), `last minted 59 min 45 s ago reads as one hour: ${lastMintedAges()}`);
assert(!lastMintedAges().some(text => text.includes('60 min')), `last minted never reads 60 min: ${lastMintedAges()}`);
globalThis.relativeTimeTestsPassed = true;
