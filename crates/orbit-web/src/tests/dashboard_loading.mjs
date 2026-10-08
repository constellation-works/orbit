// Full-app scenarios run in Chromium via dashboard_loading_browser.mjs,
// the required dashboard-browser scenario in the QA sweep inventory.
const node = id => document.getElementById(id);
const check = (condition, message) => { if (!condition) throw new Error(message); };
const settle = async () => { for (let i = 0; i < 5; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
const response = (payload, status = 200) => ({ ok: status === 200, status, json: async () => payload, text: async () => JSON.stringify(payload) });
let heldPath = '/api/tasks';
let networkDown = false;
let metricsError = false;
let healthFixture = false;
const healthQueries = [];
let marker = 'first';
let taskPaging = false;
let mutationTask = null;
let shipFixture = null;
let shipLive = false;
let shipRequests = 0;
const pendingShips = [];
let terminalRunFixture = false;
let liveDrain = false;
let readinessReads = 0;
let crewReads = 0;
let summaryReads = 0;
let frictionTitle = 'stable friction title';
let frictionBody = 'stable friction body';
let frictionDuring = 'ORB-100';
const runQueries = [];
const runDetailRequests = [];
const terminalRuns = ['success', 'failed', 'timeout', 'cancelled', 'interrupted'].map(state => ({
  run_id: `terminal-${state}`, job_id: 'fixture', state,
}));
const pendingReads = [];
const errorsRetentionStart = new Date(Date.now() - 30 * 3600000).toISOString();
const list = items => ({ items, total: items.length, limit: 50, truncated: false });
function fixture(url) {
  const workspace = url.searchParams.get('workspace');
  switch (url.pathname) {
    case '/api/workspaces': return ['one', 'two'].map(id => ({ id, name: id, status: 'active', is_default: id === 'one' }));
    case '/api/tasks': {
      if (mutationTask) return list([mutationTask]);
      if (shipFixture) return list([{ ...shipFixture }]);
      if (!taskPaging) return list([{ id: 'TEST-1', title: marker, status: 'in-progress', priority: 'medium' }]);
      const cursor = url.searchParams.get('cursor');
      const page = cursor === 'page-2' ? 2 : cursor === 'page-1' ? 1 : 0;
      const size = page === 2 ? 15 : 20;
      return {
        items: Array.from({ length: size }, (_, index) => ({
          id: `PAGE-${page}-${index}`,
          title: `task page ${page}`,
          status: 'in-progress',
          priority: 'medium',
        })),
        total: 55,
        limit: 20,
        truncated: true,
        offset: page * 20,
        next_cursor: page < 2 ? `page-${page + 1}` : null,
      };
    }
    case '/api/tasks/all': return list([{
      id: 'AGGREGATE-1', title: 'Aggregate crew fixture', status: 'in-progress', priority: 'medium',
      crew: 'opus', resolved_crew: 'opus', workspace_id: 'one', workspace_name: 'one',
    }]);
    case '/api/crews': return { default_crew: 'opus', crews: [{ name: 'opus' }] };
    case '/api/workflows/auto/readiness': return {
      capacity: {
        active_leaf_runs: liveDrain ? 1 : 0, max_active_leaf_runs: 4, free_slots: liveDrain ? 3 : 4,
        drain_run_id: liveDrain ? 'jrun-live-fixture' : null,
        drain_phase: liveDrain ? 'draining' : 'idle',
        drain_status_run_id: liveDrain ? 'jrun-live-fixture' : null,
        ends_at: liveDrain ? new Date(Date.now() + 60 * 60 * 1000).toISOString() : null,
        running_admitted_workers: liveDrain ? 1 : 0, admitted_workers: liveDrain ? 1 : 0,
      },
      tasks: [],
    };
    case '/api/job-runs': {
      runQueries.push(url.searchParams.get('state'));
      if (!terminalRunFixture) return list([{ run_id: marker, job_id: 'fixture', state: 'failed' }]);
      const runs = url.searchParams.get('state') === 'failed'
        ? terminalRuns.filter(run => ['failed', 'timeout', 'interrupted'].includes(run.state))
        : terminalRuns;
      return list(runs);
    }
    case '/api/runs/cross-workspace-run': return {
      run: { run_id: 'cross-workspace-run', job_id: 'fixture', state: 'success' },
      steps: [],
    };
    case '/api/diagnostics/errors': {
      const selectedWindow = url.searchParams.get('since');
      healthQueries.push({ path: url.pathname, window: selectedWindow });
      const since = new Date(Date.now() - (selectedWindow === '7d' ? 7 * 24 : 24) * 3600000).toISOString();
      // Retained logs reach back 30h: the whole 24h window, but not 7d.
      const coverage_since = selectedWindow === '7d' ? errorsRetentionStart : since;
      return { since, coverage_since, items: healthFixture ? [
        { event_id: 'process', message: 'build failed: dependency unavailable', source: 'process', target: 'orbit.job.step_finished' },
        { event_id: 'retry', message: 'error=apply_patch verification failed: Failed to find expected lines in /home/operator/project/.orbit/state/worktrees/orbit-jrun-fixture/src/lib.rs', source: 'agent-stderr', target: 'codex_core::tools::router' },
        { event_id: 'timeout', message: 'failed to refresh available models: request timed out', source: 'agent-stderr', target: 'codex_models_manager::manager' },
        { event_id: 'other', message: 'unexpected tool crash', source: 'agent-stderr', target: 'codex_core::tools::router' },
      ] : [{ message: marker, source: 'fixture' }] };
    }
    case '/api/diagnostics/metrics':
      healthQueries.push({ path: url.pathname, window: url.searchParams.get('since') });
      return healthFixture ? [{ ts: new Date().toISOString(), actor_identity: 'fixture', token_usage: 1379713 }] : [];
    case '/api/audit/incidents': {
      const selectedClass = url.searchParams.get('class');
      const limit = Number(url.searchParams.get('limit'));
      healthQueries.push({ path: url.pathname, window: url.searchParams.get('since'), class: selectedClass, limit });
      // Older unexpected incidents fall outside All's recency cap, but remain
      // reachable when the server filters by class before applying that cap.
      const incidents = Array.from({ length: limit + 3 }, (_, index) => ({
        incident_id: `fixture-${index}`,
        class: index === 1 || index >= limit ? 'unexpected' : 'expected',
        message: `failure ${index}`,
        event_count: 1,
        last_ts: new Date(Date.now() - index * 1000).toISOString(),
      }));
      const selected = incidents.filter(incident => !selectedClass || incident.class === selectedClass);
      const shown = selected.slice(0, limit);
      const counts = { expected: limit - 1, unexpected: 4 };
      return { window: url.searchParams.get('since'), class: selectedClass, incident_count: incidents.length,
        shown_incident_count: shown.length, matching_incident_count: selected.length,
        raw_failed_events: incidents.length, total_events: incidents.length * 10, truncated: false,
        incidents_by_class: counts, raw_events_by_class: counts,
        failure_categories: { unexpected: { incidents: counts.unexpected } }, incidents: shown };
    }
    case '/api/routines': return { machine_name: marker, routines: [{ name: marker, source: workspace, enabled: true }], clock: {} };
    case '/api/auto-tasks': return { definitions: [] };
    case '/api/audit/summary': summaryReads++; return { events: summaryReads, failed_runs: terminalRunFixture ? 3 : 0 };
    case '/api/frictions': {
      const status = url.searchParams.get('status');
      const visible = status === null || status === 'all' || status === 'open';
      const item = {
        id: 'FRIC-1',
        title: frictionTitle,
        body: frictionBody,
        status: 'open',
        tags: ['dashboard'],
        created_at: '2026-01-15T12:00:00Z',
        during_task: frictionDuring,
      };
      return { items: visible ? [item] : [], tags: ['dashboard'], total: visible ? 1 : 0 };
    }
    case '/api/frictions/stats': return { open: 1, triaged: 0, resolved_this_month: 0, total: 1 };
    default: return [];
  }
}
globalThis.fetch = async (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  if (/^\/api\/runs\/cross-workspace-run(?:\/|$)/.test(url.pathname)) {
    runDetailRequests.push({ path: url.pathname, workspace: url.searchParams.get('workspace') });
  }
  if (shipFixture && url.pathname === '/api/workflows/ship') {
    check(options.method === 'POST', 'Ship uses the dispatch endpoint');
    check(JSON.stringify(JSON.parse(options.body).task_ids) === JSON.stringify([shipFixture.id]), 'Ship dispatches only the selected task');
    shipRequests++;
    if (shipLive) return response({ error: 'Ship run is already in flight', code: 'ship_run_in_flight' }, 409);
    return new Promise(resolve => pendingShips.push(resolve));
  }
  if (mutationTask && options.method === 'PATCH' && url.pathname === `/api/tasks/${encodeURIComponent(mutationTask.id)}`) {
    mutationTask = { ...mutationTask, ...JSON.parse(options.body) };
    return response(mutationTask);
  }
  if (url.pathname === '/api/workflows/auto/readiness') readinessReads += 1;
  if (url.pathname === '/api/crews') crewReads += 1;
  if (networkDown) throw new TypeError('Fixture network unavailable');
  if (metricsError && url.pathname === '/api/diagnostics/metrics') return response({ error: 'Metrics fixture failure' }, 500);
  const payload = fixture(url);
  if (url.pathname === heldPath) return new Promise((resolve, reject) => pendingReads.push({ payload, resolve, reject }));
  return response(payload);
};
await import('./app.js');
await settle();
const { persistScopeToUrl, setWorkspace, setWindow, formatDateTime } = await import('./js/common.js');
const { navigateToRun, setActiveTab } = await import('./js/router.js');
const refresh = () => {
  const button = node('refresh-btn');
  if (button.listeners) button.listeners.click(); else button.click();
};
const click = target => target.listeners ? target.listeners.click() : target.click();
const release = (request, payload = request.payload) => request.resolve(response(payload));
const text = id => node(id).textContent;
const busy = id => node(id).getAttribute ? node(id).getAttribute('aria-busy') : node(id)['aria-busy'];
check(text('tasks-body').includes('Loading'), 'cold Tasks must visibly load');
check(!text('tasks-body').includes('No tasks'), 'cold Tasks cannot claim empty');
for (const request of pendingReads.splice(0)) release(request, list([]));
await settle();
check(text('tasks-body').includes('No tasks'), 'successful empty Tasks produces empty state');

// A first same-key expiry must not remove feedback created by a later undo.
heldPath = null;
mutationTask = { id: 'FEEDBACK-1', title: 'Feedback expiry fixture', status: 'in-progress', priority: 'medium' };
refresh();
await settle();
const realFeedbackTimeout = globalThis.setTimeout;
const feedbackExpiries = [];
globalThis.setTimeout = (fn, ms, ...args) => {
  if (ms === 8500) {
    feedbackExpiries.push({ fn, args });
    return { feedbackExpiry: true };
  }
  return realFeedbackTimeout(fn, ms, ...args);
};
const changeFeedbackCrew = async value => {
  const select = node('tasks-body').querySelector('.task-crew-select');
  check(select, 'feedback fixture exposes the crew control');
  select.value = value;
  select.dispatchEvent(new Event('change', { bubbles: true }));
  await settle();
};
await changeFeedbackCrew('opus');
check(feedbackExpiries.length === 1, 'first crew feedback schedules its expiry');
const firstUndo = node('tasks-body').querySelector('.mutation-undo');
check(firstUndo, 'first crew feedback offers undo');
click(firstUndo);
await settle();
check(feedbackExpiries.length === 2, 'undo feedback schedules its own expiry');
feedbackExpiries[0].fn(...feedbackExpiries[0].args);
await settle();
check(node('tasks-body').querySelector('.mutation-undo'), 'earlier expiry preserves the newer undo window');
feedbackExpiries[1].fn(...feedbackExpiries[1].args);
await settle();
check(!node('tasks-body').querySelector('.mutation-feedback'), 'newer expiry removes its own feedback');
globalThis.setTimeout = realFeedbackTimeout;
mutationTask = null;
refresh();
await settle();

// The aggregate list carries each task's crew, but there is no single
// workspace crew registry to validate it against or edit through.
heldPath = null;
liveDrain = true;
refresh();
await settle();
check(node('global-drain-state').textContent.includes('Draining'), 'a live workspace drain appears before switching to aggregate view');
const selectedWorkspaceReadinessReads = readinessReads;
const selectedWorkspaceCrewReads = crewReads;
setWorkspace(null);
persistScopeToUrl();
check(new URL(window.location.href).searchParams.get('workspace') === 'all', 'aggregate scenario is represented by workspace=all');
setActiveTab('tasks');
await settle();
refresh();
await settle();
const aggregateCrewRow = node('tasks-body').querySelector('[data-key="task-AGGREGATE-1"]');
const aggregateCrewCell = aggregateCrewRow?.querySelector('.crew-cell');
check(aggregateCrewCell?.textContent === 'opus', `aggregate crew remains visible without a missing label: ${aggregateCrewCell?.textContent}`);
check(!aggregateCrewCell.querySelector('.task-crew-select'), 'aggregate crew cell does not offer a workspace-scoped edit');
const aggregateDrainIndicator = node('global-drain-state');
check(!aggregateDrainIndicator.hidden && aggregateDrainIndicator.textContent.includes('Per-workspace drain status'), 'aggregate header explains that drain status is workspace-scoped');
check(aggregateDrainIndicator.getAttribute('aria-label').includes('Select a workspace'), 'aggregate drain indicator explains how to inspect live status');
check(readinessReads === selectedWorkspaceReadinessReads, 'aggregate view does not fetch one workspace drain status as if it were global');
check(crewReads === selectedWorkspaceCrewReads, 'aggregate view does not request a workspace crew registry');
liveDrain = false;
// The run exists in workspace two. Switching to All workspaces on its detail
// page must replace the content with a scope prompt and issue no unscoped reads.
navigateToRun('cross-workspace-run', 'two');
await settle();
check(text('run-detail-title') === 'Run cross-workspace-run', 'selected workspace loads the existing cross-workspace run');
check(runDetailRequests.length === 3, 'selected workspace loads run details, events and logs');
check(runDetailRequests.every(request => request.workspace === 'two'), 'selected-workspace run requests carry workspace scope');
const scopedRunRequests = runDetailRequests.length;
setWorkspace(null);
persistScopeToUrl();
refresh();
await settle();
check(text('run-detail-meta').includes('Select a workspace to view this run.'), 'aggregate run detail asks for a workspace');
check(!text('run-detail-meta').includes('Run not found'), 'aggregate run detail does not report an existing run as missing');
check(runDetailRequests.length === scopedRunRequests, 'aggregate run detail sends no run, events or logs requests');
setActiveTab('tasks');
setWorkspace('one');
persistScopeToUrl();
await settle();
refresh();
await settle();

const surfaces = [
  { route: 'tasks', body: 'tasks-body', path: '/api/tasks', empty: list([]), emptyText: 'No tasks' },
  { route: 'diagnostics/runs', body: 'runs-body', path: '/api/job-runs', empty: list([]), emptyText: 'No job runs' },
  { route: 'diagnostics/errors', body: 'diag-body', path: '/api/diagnostics/errors', empty: { items: [], since: null, coverage_since: null }, emptyText: 'No error events' },
  { route: 'operations/routines', body: 'routines-body', path: '/api/routines', empty: { routines: [], clock: {} }, emptyText: 'No routines' },
];
for (const surface of surfaces) {
  heldPath = surface.path;
  setWorkspace('one');
  setWorkspace('two');
  marker = 'current-scope-data';
  setActiveTab(surface.route);
  await settle();
  if (!pendingReads.length) { refresh(); await settle(); }
  check(text(surface.body).includes('Loading'), `${surface.route}: cold load visible`);
  check(!text(surface.body).includes(surface.emptyText), `${surface.route}: cold load cannot be empty`);
  check(busy(surface.body) === 'true', `${surface.route}: loading marked busy`);
  const status = Array.from(node(surface.body).children).find(child => child.dataset.panelStatus);
  const attribute = name => status?.getAttribute ? status.getAttribute(name) : status?.[name];
  check(attribute('role') === 'status' && attribute('aria-live') === 'polite', `${surface.route}: accessible loading feedback`);
  if (status.getBoundingClientRect) {
    const box = status.getBoundingClientRect();
    check(box.width > 0 && box.height > 0 && box.top < window.innerHeight, `${surface.route}: loading feedback visible in browser`);
  }
  for (const request of pendingReads.splice(0)) release(request);
  await settle();
  check(text(surface.body).includes(marker), `${surface.route}: loaded data visible`);
  check(text(surface.body).includes('Updated.'), `${surface.route}: success visible`);

  marker = 'late-old-scope';
  refresh(); await settle();
  const old = pendingReads.splice(0);
  check(text(surface.body).includes('Refreshing') && text(surface.body).includes('current-scope-data'), `${surface.route}: refresh retains labeled data`);
  check(!node('refresh-btn').disabled, `${surface.route}: refresh remains usable`);
  setWorkspace('one');
  check(!text(surface.body).includes('current-scope-data'), `${surface.route}: workspace change clears synchronously`);
  marker = 'new-scope-data';
  refresh(); await settle();
  for (const request of pendingReads.splice(0)) release(request);
  await settle();
  for (const request of old) release(request);
  await settle();
  check(text(surface.body).includes('new-scope-data') && !text(surface.body).includes('late-old-scope'), `${surface.route}: old scope response discarded`);

  marker = 'older-overlap'; refresh(); await settle();
  const earlier = pendingReads.splice(0);
  marker = 'newer-overlap'; refresh(); await settle();
  for (const request of pendingReads.splice(0)) release(request);
  await settle();
  for (const request of earlier) release(request);
  await settle();
  check(text(surface.body).includes('newer-overlap') && !text(surface.body).includes('older-overlap'), `${surface.route}: latest request wins`);

  marker = 'late-before-roundtrip'; refresh(); await settle();
  const roundtrip = pendingReads.splice(0);
  setWorkspace('two'); setWorkspace('one');
  for (const request of roundtrip) release(request);
  await settle();
  check(text(surface.body).includes('Loading') && !text(surface.body).includes('late-before-roundtrip'), `${surface.route}: A→B→A rejects old response`);
  marker = 'newer-overlap'; refresh(); await settle();
  for (const request of pendingReads.splice(0)) release(request);
  await settle();
  refresh(); await settle();
  for (const request of pendingReads.splice(0)) request.reject(new TypeError('Fixture refresh failure'));
  await settle();
  check(text(surface.body).includes('stale data') && text(surface.body).includes('newer-overlap'), `${surface.route}: refresh failure retains labeled stale data`);
  check(busy(surface.body) === 'false', `${surface.route}: failure clears busy`);
  setWorkspace('two'); refresh(); await settle();
  for (const request of pendingReads.splice(0)) request.reject(new TypeError('Fixture cold failure'));
  await settle();
  check(text(surface.body).includes('Unable to load') && !text(surface.body).includes(surface.emptyText), `${surface.route}: cold error differs from empty`);
  refresh(); await settle();
  for (const request of pendingReads.splice(0)) release(request, surface.empty);
  await settle();
  check(text(surface.body).includes(surface.emptyText) && !text(surface.body).includes('Unable to load'), `${surface.route}: empty success recovers`);
}
heldPath = null;
terminalRunFixture = true;
setActiveTab('tasks');
refresh(); await settle();
check(text('rail-count-diag-runs') === '3', 'the Runs rail count covers all three failure outcomes');
check(node('rail-count-diag-runs').classList.contains('alert'), 'a failed-run count reads as an alert on the rail');
setActiveTab('diagnostics/runs'); await settle();
click(Array.from(node('runs-body').querySelectorAll('.runs-filter-button')).find(button => button.textContent === 'Failed')); await settle();
check(runQueries.at(-1) === 'failed', 'the Failed filter requests the server failure group');
check(new URL(window.location.href).searchParams.get('run_state') === 'failed', 'the Failed filter is kept in the address');
const failureRows = Array.from(node('runs-body').children).filter(row => row.dataset.key?.startsWith('run-'));
check(failureRows.length === Number(text('rail-count-diag-runs')), 'the Runs rail count equals the rendered failed run count');
for (const state of ['failed', 'timeout', 'interrupted']) {
  check(failureRows.some(row => row.textContent.includes(`terminal-${state}`)), `${state} run remains visible under the Failed filter`);
}
check(!text('runs-body').includes('terminal-success') && !text('runs-body').includes('terminal-cancelled'), 'Failed filter excludes successful and cancelled runs');
terminalRunFixture = false;

// Both entrypoints share a pending-request guard, but an accepted run must not
// lock Ship for the page lifetime after cancellation or failed delivery.
setActiveTab('tasks');
for (const origin of ['detail', 'row']) {
  shipFixture = { id: `SHIP-${origin}`, title: `Ship ${origin} fixture`, status: 'backlog', priority: 'medium', job_run_id: null };
  shipLive = false;
  const rowShip = () => node('tasks-body').querySelector('.task-quick.ship');
  const detailShip = () => node(`detail-${shipFixture.id}`)?.querySelector('.action.ship');
  const openDetail = async () => {
    if (!detailShip()) {
      click(node('tasks-body').querySelector(`[data-key="task-${shipFixture.id}"] > .title`));
      await settle();
    }
  };
  const bothEnabled = () => rowShip() && detailShip() && !rowShip().disabled && !detailShip().disabled;
  refresh(); await settle();
  await openDetail();
  check(bothEnabled(), `${origin}: backlog task enables both Ship controls`);
  const requestsBefore = shipRequests;
  click(origin === 'detail' ? detailShip() : rowShip()); await settle();
  check(shipRequests === requestsBefore + 1 && pendingShips.length === 1, `${origin}: one dispatch is pending`);
  refresh(); await settle();
  check(rowShip().disabled && detailShip().disabled, `${origin}: refresh preserves both pending Ship controls`);
  // Dispatch synthetic events to exercise the handler guards as well as the
  // disabled DOM controls while the first request has not answered.
  for (const control of [rowShip(), detailShip()]) control.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await settle();
  check(shipRequests === requestsBefore + 1, `${origin}: pending duplicate never reaches the server`);
  pendingShips.shift()(response({ error: 'Dispatch fixture failed' }, 503));
  await settle();
  check(bothEnabled(), `${origin}: dispatch failure after refresh enables retry in both controls`);
  check(text('tasks-body').includes('Dispatch fixture failed'), `${origin}: dispatch failure appears in the current detail or row`);
  click(origin === 'detail' ? detailShip() : rowShip()); await settle();
  check(shipRequests === requestsBefore + 2 && pendingShips.length === 1, `${origin}: failed dispatch can be retried`);
  refresh(); await settle();
  shipLive = true;
  pendingShips.shift()(response({ run_id: `jrun-${origin}-live`, state: 'submitted' }));
  await settle();

  // A list may still show backlog after acceptance. The server's conflict
  // response must be surfaced if the other entrypoint tries the live run.
  await openDetail();
  click(origin === 'detail' ? rowShip() : detailShip()); await settle();
  check(shipRequests === requestsBefore + 3 && pendingShips.length === 0, `${origin}: live duplicate is refused by the server`);
  check(text('tasks-body').includes('Ship run is already in flight'), `${origin}: live duplicate error is visible`);

  shipFixture.status = 'in-progress';
  shipFixture.job_run_id = `jrun-${origin}-live`;
  refresh(); await settle();
  check(!rowShip() && !detailShip(), `${origin}: active task does not offer Ship`);
  shipLive = false;
  shipFixture.status = 'backlog';
  shipFixture.job_run_id = null;
  refresh(); await settle();
  await openDetail();
  check(bothEnabled(), `${origin}: both Ship controls recover after the run ends without reloading`);
  click(origin === 'detail' ? detailShip() : rowShip()); await settle();
  check(shipRequests === requestsBefore + 4 && pendingShips.length === 1, `${origin}: the returned backlog task can be shipped again`);
  shipFixture.status = 'in-progress';
  shipFixture.job_run_id = `jrun-${origin}-retry`;
  shipLive = true;
  pendingShips.shift()(response({ run_id: shipFixture.job_run_id, state: 'submitted' }));
  await settle();
}
shipFixture = null;
shipLive = false;
taskPaging = true;
setActiveTab('tasks');
refresh(); await settle();
check(text('tasks-count') === '1–20 of 55', 'first page exposes its matching range and total');
check(!node('tasks-next').disabled && node('tasks-previous').disabled, 'first page has accessible forward-only navigation');
click(node('tasks-next')); await settle();
check(text('tasks-count') === '21–40 of 55' && text('tasks-body').includes('task page 1'), 'Next reaches the second page');
check(!node('tasks-previous').disabled, 'second page enables Previous');
click(node('tasks-next')); await settle();
check(text('tasks-count') === '41–55 of 55' && node('tasks-next').disabled, 'last partial page has the correct range and no Next');
click(node('tasks-previous')); await settle();
check(text('tasks-count') === '21–40 of 55', 'Previous returns to the prior cursor');

heldPath = '/api/tasks';
marker = 'unused';
refresh(); await settle();
const olderPage = pendingReads.splice(0);
refresh(); await settle();
for (const request of pendingReads.splice(0)) release(request, {
  ...request.payload,
  items: request.payload.items.map(item => ({ ...item, title: 'new page response' })),
});
await settle();
for (const request of olderPage) release(request, {
  ...request.payload,
  items: request.payload.items.map(item => ({ ...item, title: 'stale page response' })),
});
await settle();
check(text('tasks-body').includes('new page response') && !text('tasks-body').includes('stale page response'), 'stale page response cannot overwrite newer navigation data');
heldPath = null;
taskPaging = false;
metricsError = true;
const priorSummaryReads = summaryReads;
setActiveTab('diagnostics/metrics');
await settle();
check(text('diag-body').includes('Metrics fixture failure'), 'HTTP failure visible in Metrics panel');
check(node('conn-status').className.includes('orange') && text('meta-text').includes('Metrics') && !text('meta-text').includes('offline'), 'one HTTP panel error is amber, names the failing panel, and does not imply offline');
check(summaryReads > priorSummaryReads && text('rail-count-audit') === String(summaryReads), 'audit summary renders updated data independently of failed panel');
networkDown = true;
refresh(); await settle();
check(node('conn-status').className.includes('red') && text('meta-text').includes('offline'), 'network outage reports offline');
networkDown = false;
metricsError = false;
refresh(); await settle();
check(node('conn-status').className.includes('green'), 'connection recovers');
check(text('meta-text').includes('refreshed') && !text('meta-text').includes('diagnostics/'), 'connection line names the destination, not its route');
check(document.title !== 'orbit' && document.title.endsWith('orbit'), 'the page title names the current destination');
// Exercise Health through the full request builder and DOM rendering path.
healthFixture = true;
for (const selectedWindow of ['24h', '7d']) {
  setWindow(selectedWindow);
  for (const subtab of ['metrics', 'errors']) {
    setActiveTab(`diagnostics/${subtab}`); await settle(); refresh(); await settle();
    check(healthQueries.at(-1).window === selectedWindow, `${subtab} requests selected window ${selectedWindow}`);
    check(text('diag-count').includes(selectedWindow), `${subtab} header labels selected range`);
  }
  const coverageLabel = `covers since ${formatDateTime(errorsRetentionStart)}`;
  check(text('diag-count').includes(coverageLabel) === (selectedWindow === '7d'),
    `errors header names its coverage start only when retention starts after the ${selectedWindow} window: ${text('diag-count')}`);
  check(node('diag-body').querySelector('.c-target').textContent === 'orbit.job.step_finished', 'process target is displayed');
  const internal = node('diag-body').querySelector('details.agent-diagnostics');
  check(internal && (selectedWindow !== '24h' || !internal.open), 'recoverable agent diagnostics start collapsed');
  const mainRows = [...node('diag-body').querySelectorAll('.diagnostics-errors-main tbody > tr')]
    .map(row => row.querySelector('.c-message').textContent);
  check(mainRows.length === 2 && mainRows.every(message => !/verification failed|request timed out/.test(message)), 'real codex patch and model-timeout rows leave the main table');
  check(internal.querySelectorAll('tbody > tr').length === 2, 'both recognised codex shapes collapse under agent diagnostics');
  internal.open = true;
  const shortened = internal.querySelector('.c-message');
  check(shortened.textContent.includes('[worktree]/src/lib.rs') && !shortened.textContent.includes('/home/operator'), 'worktree prefix shortened in message');
  check(shortened.title.includes('/home/operator'), 'full failure text remains available');
}
setActiveTab('diagnostics/metrics'); await settle();
check(node('diag-body').querySelector('.c-token_usage').textContent === '1,379,713', 'tokens use thousands grouping');
setActiveTab('diagnostics/incidents'); await settle();
check(healthQueries.at(-1).class === 'unexpected', 'default incident request isolates unexpected failures before limit');
const incidentRows = () => [...node('diag-body').querySelectorAll('.incident-row')];
const checkIncidentSelection = selectedClass => {
  const group = node('diag-body').querySelector('.incident-class-chips');
  check(group.getAttribute('role') === 'group' && group.getAttribute('aria-label'), 'incident filters have a named accessible group');
  const chips = [...group.querySelectorAll('button')];
  check(chips.length === 5, 'all incident classes are reachable');
  for (const chip of chips) {
    check(chip.getAttribute('type') === 'button', 'class chips have an explicit button type');
    check(chip.getAttribute('aria-pressed') === String(chip.dataset.class === selectedClass), 'only the active class chip is pressed');
  }
  const selected = group.querySelector('[aria-pressed="true"]');
  const other = group.querySelector('[aria-pressed="false"]');
  check(getComputedStyle(selected).backgroundColor !== getComputedStyle(other).backgroundColor, 'pressed incident chip renders a distinct selected background');
};
check(incidentRows().length === 4 && incidentRows().every(row => row.classList.contains('unexpected')), 'default list includes older unexpected failures beyond the All cap');
checkIncidentSelection('unexpected');
click(node('diag-body').querySelector('[data-class="all"]')); await settle();
const incidentLimit = healthQueries.at(-1).limit;
check(healthQueries.at(-1).class === null, 'All requests every class within the recency cap');
check(incidentRows().length === incidentLimit && incidentRows().every((row, index) => row.dataset.key === `incident-fixture-${index}`), 'All preserves server recency order across classes');
check(text('diag-count').includes(`Newest ${incidentLimit} of ${incidentLimit + 3}`), 'All labels its newest shown and matching counts even without scan truncation');
checkIncidentSelection('all');
click(node('diag-body').querySelector('[data-class="expected"]')); await settle();
check(incidentRows().length === incidentLimit - 1 && incidentRows().every(row => row.classList.contains('expected')), 'one click isolates expected paths');
checkIncidentSelection('expected');
click(node('diag-body').querySelector('[data-class="unexpected"]')); await settle();
check(incidentRows().length === 4 && incidentRows().every(row => row.classList.contains('unexpected')), 'one click restores older unexpected failures');
checkIncidentSelection('unexpected');
healthFixture = false;
setWindow('24h');
setActiveTab('diagnostics/metrics'); await settle();
const realTimeout = globalThis.setTimeout;
const realFetch = globalThis.fetch;
globalThis.setTimeout = (fn, ms, ...args) => realTimeout(fn, ms === 30000 ? 1 : ms, ...args);
globalThis.fetch = (_path, options) => new Promise((_resolve, reject) => {
  options.signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')));
});
refresh(); await settle();
check(busy('diag-body') === 'false' && text('diag-body').includes('timed out'), 'hung request times out and clears busy state');
check(!node('refresh-btn').disabled, 'timeout leaves retry available');
globalThis.setTimeout = realTimeout;
globalThis.fetch = realFetch;
const realClearTimeout = globalThis.clearTimeout;
const scheduledPolls = [];
globalThis.setTimeout = (fn, ms, ...args) => {
  if (ms === 0) return realTimeout(fn, ms, ...args);
  const handle = { cancelled: false, unref: () => {} };
  scheduledPolls.push({ fn, ms, args, handle });
  return handle;
};
globalThis.clearTimeout = (handle) => {
  if (typeof handle === 'object') handle.cancelled = true;
  else realClearTimeout(handle);
};
const activePoll = () => scheduledPolls.filter(entry => !entry.handle.cancelled).at(-1);
const runPoll = async () => {
  const poll = activePoll();
  check(poll, 'refresh poll must be scheduled');
  poll.handle.cancelled = true;
  poll.fn(...poll.args);
  await settle();
};
const setDocumentHidden = value => {
  try { document.hidden = value; } catch (_) {
    Object.defineProperty(document, 'hidden', { configurable: true, value });
  }
};
const fireVisibilityChange = () => {
  if (typeof documentListeners !== 'undefined') documentListeners.visibilitychange();
  else document.dispatchEvent(new Event('visibilitychange'));
};

// Pausing a dashboard removes its pending poll. Returning to it refreshes once,
// then failed polls double their delay and a successful retry restores 30s.
// Becoming visible schedules the poll that hiding must then cancel.
setDocumentHidden(false);
fireVisibilityChange();
await settle();
const visiblePoll = activePoll();
check(visiblePoll && visiblePoll.ms === 30000, 'visible dashboard schedules its normal 30s poll');
const hiddenSummaryReads = summaryReads;
setDocumentHidden(true);
fireVisibilityChange();
await settle();
check(visiblePoll.handle.cancelled, 'hiding cancels the pending poll');
// Run the poll regardless of cancellation: the hidden-tab guard alone must stop its request.
visiblePoll.fn(...visiblePoll.args);
await settle();
check(summaryReads === hiddenSummaryReads, 'hidden dashboard makes no audit-summary request');
setDocumentHidden(false);
fireVisibilityChange();
await settle();
check(summaryReads === hiddenSummaryReads + 1, 'visible dashboard refreshes exactly once');
check(activePoll().ms === 30000, 'successful refresh schedules the normal 30s interval');
networkDown = true;
await runPoll();
check(activePoll().ms === 60000, 'first failed refresh doubles the interval');
await runPoll();
check(activePoll().ms === 120000, 'consecutive failures continue exponential backoff');
networkDown = false;
await runPoll();
check(activePoll().ms === 30000, 'successful retry restores the 30s interval');
// A host snapshot that never answers must not pin the status line or the next
// poll. Other panels still render, and the open request keeps a 30s abort.
const hungHostSignals = [];
const fetchDuringHostHang = globalThis.fetch;
globalThis.fetch = (path, options = {}) => {
  const url = new URL(path, 'http://dashboard.test');
  if (url.pathname === '/api/host/resources') {
    return new Promise((_resolve, reject) => {
      hungHostSignals.push(options.signal || null);
      if (options.signal) {
        options.signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')));
      }
    });
  }
  return fetchDuringHostHang(path, options);
};
const summaryBeforeHostHang = summaryReads;
const eventsBeforeHostHang = text('rail-count-audit');
await runPoll();
const liveTimers = () => scheduledPolls.filter(entry => !entry.handle.cancelled);
const polls = liveTimers().filter(entry => String(entry.fn).includes('refreshDashboard'));
check(hungHostSignals.length >= 1 && hungHostSignals.every(signal => signal && !signal.aborted), 'host request stays unanswered and carries an abort signal');
check(summaryReads > summaryBeforeHostHang && text('rail-count-audit') !== eventsBeforeHostHang, 'other panels refresh while host resources never answer');
check(!text('meta-text').includes('fetching') && node('conn-status').className.includes('green'), 'hung host request does not leave the dashboard fetching');
check(polls.length === 1 && polls[0].ms === 30000, 'next poll is scheduled while host resources are still unanswered');
check(liveTimers().some(entry => entry !== polls[0] && entry.ms === 30000), 'unanswered host request remains bounded by the 30s timeout');
globalThis.fetch = fetchDuringHostHang;
globalThis.setTimeout = realTimeout;
globalThis.clearTimeout = realClearTimeout;

// The Knowledge row stays mounted while its change-hash is unchanged. Identity,
// status, tags, and created_at stay fixed; each painted field must repaint alone.
heldPath = null;
networkDown = false;
metricsError = false;
frictionTitle = 'stable friction title';
frictionBody = 'stable friction body';
frictionDuring = 'ORB-100';
setActiveTab('knowledge');
await settle();
const frictionText = () => text('frictions-body');
if (!frictionText().includes('stable friction title')) {
  refresh();
  await settle();
}
check(frictionText().includes('stable friction title'), 'knowledge list shows the friction title');
check(frictionText().includes('stable friction body'), 'knowledge list shows the friction body');
check(frictionText().includes('during ORB-100'), 'knowledge list shows the during-task');
frictionTitle = 'retitled friction';
refresh();
await settle();
check(frictionText().includes('retitled friction') && !frictionText().includes('stable friction title'), 'retitled friction replaces the list title');
check(frictionText().includes('stable friction body') && frictionText().includes('during ORB-100'), 'title-only refresh keeps the unchanged body and during-task');
frictionBody = 'rewritten friction body';
refresh();
await settle();
check(frictionText().includes('rewritten friction body') && !frictionText().includes('stable friction body'), 'edited friction body replaces the list summary');
frictionDuring = 'ORB-200';
refresh();
await settle();
check(frictionText().includes('during ORB-200') && !frictionText().includes('during ORB-100'), 'edited during-task replaces the list meta');
check(frictionText().includes('retitled friction') && frictionText().includes('rewritten friction body'), 'later field edits keep the updated title and body');

globalThis.showTaskPaginationEvidence = async () => {
  networkDown = false;
  metricsError = false;
  heldPath = null;
  taskPaging = true;
  setActiveTab('tasks');
  refresh();
  await settle();
};
globalThis.showDiagnosticsEvidence = async () => {
  taskPaging = false;
  setActiveTab('diagnostics/metrics');
  refresh();
  await settle();
};
globalThis.showHealthEvidence = async subtab => {
  healthFixture = true;
  heldPath = null;
  setWindow('7d');
  setActiveTab(`diagnostics/${subtab}`);
  refresh();
  await settle();
  if (subtab === 'metrics') {
    const { renderDiagnosticsSideCard } = await import('./js/diagnostics.js');
    renderDiagnosticsSideCard({
      completion_by_complexity: [{ complexity: 'unset', total: 999, statuses: [
        { status: 'done', count: 532 }, { status: 'rejected', count: 17 }, { status: 'archived', count: 450 },
      ] }],
      implement_one_by_complexity: [{ complexity: 'unset', n: 532, actors: [{ actor: 'long-provider/model-name', n: 532, avg: 125000, p50: 90000, p95: 400000 }] }],
    }, { fmtDuration: value => `${value / 1000}s` });
  }
};
globalThis.loadingTestsPassed = true;
console.log('Dashboard loading, ordering, empty, error and recovery scenarios passed.');
