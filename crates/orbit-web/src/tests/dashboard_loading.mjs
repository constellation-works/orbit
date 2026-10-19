// Full-app scenarios run in Chromium via dashboard_loading_browser.mjs,
// the required dashboard-browser scenario in the QA sweep inventory.
const node = id => document.getElementById(id);
const check = (condition, message) => { if (!condition) throw new Error(message); };
const settle = async () => { for (let i = 0; i < 5; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
const response = (payload, status = 200, headers = {}) => new Response(JSON.stringify(payload), { status, headers });
// The summary read on every tick paints the Tools view's tool-call total.
const summaryToolCalls = () => document.getElementById('audit-summary-body').textContent.match(/\/ (\d+) tool calls/)?.[1];
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
const fleetRequests = [];
let readinessReads = 0;
let crewReads = 0;
let summaryReads = 0;
const summaryQueries = [];
const auditQueries = [];
let summaryDenials = 0;
const codebaseQueries = [];
let frictionTitle = 'stable friction title';
let frictionBody = 'stable friction body';
let frictionDuring = 'ORB-100';
const runQueries = [];
const runRequests = [];
const runDetailRequests = [];
const terminalRuns = ['success', 'failed', 'timeout', 'cancelled', 'interrupted'].map(state => ({
  run_id: `terminal-${state}`, job_id: 'fixture', state,
}));
const pendingReads = [];
const errorsRetentionStart = new Date(Date.now() - 30 * 3600000).toISOString();
const longErrorMessage = `build failed: dependency unavailable. ${'details '.repeat(40)}`;
const list = items => ({ items, total: items.length, limit: 50, truncated: false });
function metricsWindow(url) {
  return {
    key: url.searchParams.get('since'), bucket_seconds: 3600,
    since: '2026-10-11T10:00:00+00:00', until: '2026-10-11T12:30:00+00:00',
    previous_since: '2026-10-11T07:30:00+00:00', previous_until: '2026-10-11T10:00:00+00:00',
  };
}
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
    // The fleet stays visible even when the selected workspace is idle.
    // Return both rows regardless of the request's selected workspace.
    case '/api/fleet/drains': {
      fleetRequests.push(url.pathname + url.search);
      return { items: ['one', 'two'].map(id => ({
        workspace_id: id, workspace_name: id, phase: id === 'two' ? 'draining' : 'idle',
        slots_used: id === 'two' ? 1 : 0, slots_ceiling: 4,
        eligible_count: id === 'one' ? 2 : 0, holds: [],
      })) };
    }
    case '/api/workflows/auto/readiness': return {
      capacity: {
        active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4,
        drain_run_id: null, drain_phase: 'idle', drain_status_run_id: null,
        ends_at: null, running_admitted_workers: 0, admitted_workers: 0,
      },
      tasks: [],
    };
    case '/api/job-runs': {
      if (url.searchParams.get('state') === 'active') return list(['pending', 'running'].map(state => ({ run_id: `live-${state}`, job_id: 'fixture', state })));
      runQueries.push(url.searchParams.get('state'));
      runRequests.push(url.search);
      if (!terminalRunFixture) return list([{ run_id: marker, job_id: 'fixture', state: 'failed' }]);
      const runs = url.searchParams.get('state') === 'failed'
        ? terminalRuns.filter(run => ['failed', 'timeout', 'interrupted'].includes(run.state))
        : terminalRuns;
      return list(runs);
    }
    // Fleet-wide in-flight runs: the rail asks for one item, the total counts all.
    case '/api/job-runs/all': return url.searchParams.get('state') === 'active'
      ? { items: [{ run_id: 'live-fleet', job_id: 'fixture', state: 'running', workspace_id: 'one' }], total: 22, limit: 1, truncated: true }
      : [];
    case '/api/audit': auditQueries.push(url.searchParams); return url.searchParams.has('before')
      ? [{ id: 8999 }]
      : [{ id: 9000 }];
    case '/api/runs/cross-workspace-run': return {
      run: { run_id: 'cross-workspace-run', job_id: 'fixture', state: 'success' },
      steps: [],
    };
    case '/api/diagnostics/errors': {
      const selectedWindow = url.searchParams.get('since');
      healthQueries.push({ path: url.pathname, window: selectedWindow, limit: Number(url.searchParams.get('limit')) });
      const since = new Date(Date.now() - (selectedWindow === '7d' ? 7 * 24 : 24) * 3600000).toISOString();
      // Retained logs reach back 30h: the whole 24h window, but not 7d.
      const coverage_since = selectedWindow === '7d' ? errorsRetentionStart : since;
      return { since, coverage_since, items: healthFixture ? [
        { event_id: 'process', message: longErrorMessage, source: 'process', job_run: 'jrun-health-fixture', workspace_id: workspace || 'one', step: 'failed-step', target: 'orbit.job.step_finished', recovered: true },
        { event_id: 'retry', message: 'error=apply_patch verification failed: Failed to find expected lines in /home/operator/project/.orbit/state/worktrees/orbit-jrun-fixture/src/lib.rs', source: 'agent-stderr', target: 'codex_core::tools::router' },
        { event_id: 'timeout', message: 'failed to refresh available models: request timed out', source: 'agent-stderr', target: 'codex_models_manager::manager' },
        { event_id: 'other', message: 'unexpected tool crash', source: 'agent-stderr', target: 'codex_core::tools::router' },
      ] : [{ message: marker, source: 'fixture' }] };
    }
    case '/api/diagnostics/implement_one': {
      healthQueries.push({ path: url.pathname, window: url.searchParams.get('since') });
      const stats = (n, avg) => (healthFixture ? { n, avg, p50: avg, p95: avg * 2 } : null);
      const buckets = [0, 1, 2].map(i => ({
        start: new Date(Date.UTC(2026, 9, 11, 10 + i)).toISOString(),
        all: stats(4, 1500000 + i * 60000), low: stats(2, 1200000), medium: null, hard: stats(2, 1800000), other_runs: 0,
      }));
      const totals = (all) => ({ all, low: stats(6, 1200000), medium: null, hard: stats(6, 1800000), xhard: null, unset: null });
      return {
        outcome: url.searchParams.get("outcome") || "succeeded",
        window: metricsWindow(url),
        truncated: false,
        buckets,
        current: totals(stats(12, 1560000)),
        previous: totals(stats(9, 1200000)),
        actors: healthFixture ? [{ actor: 'claude / claude-opus-5-5', n: 12, avg: 1560000, p50: 1500000, p95: 3000000, previous_avg: 1200000, trend: [1500000, 1560000, 1620000] }] : [],
        slowest: [],
        implement_one_by_complexity: [],
      };
    }
    case '/api/diagnostics/completions': {
      const counts = (total) => ({ done: { total, low: total, medium: 0, hard: 0, other: 0 }, rejected: 0, archived: 0 });
      return {
        window: metricsWindow(url),
        buckets: [0, 1, 2].map(i => ({ start: new Date(Date.UTC(2026, 9, 11, 10 + i)).toISOString(), ...counts(healthFixture ? i : 0) })),
        current: counts(healthFixture ? 3 : 0),
        previous: counts(healthFixture ? 2 : 0),
      };
    }
    case '/api/tasks/completion-by-complexity': return { by_complexity: [] };
    case '/api/diagnostics/defects': {
      const counts = (total) => ({ total, lineage: { fix_for_fix: 0, regression: 0, unattributed: total }, origin: { other: total } });
      return {
        window: metricsWindow(url),
        lineages: ['fix_for_fix', 'regression', 'unattributed'],
        origins: ['other'],
        buckets: [0, 1, 2].map(i => ({ start: new Date(Date.UTC(2026, 9, 11, 10 + i)).toISOString(), ...counts(healthFixture ? i : 0) })),
        current: counts(healthFixture ? 3 : 0),
        previous: counts(healthFixture ? 2 : 0),
      };
    }
    case '/api/audit/incidents': {
      const selectedClass = url.searchParams.get('class');
      const limit = Number(url.searchParams.get('limit'));
      healthQueries.push({ path: url.pathname, window: url.searchParams.get('since'), class: selectedClass, limit });
      // Older unexpected incidents fall outside a mixed-class recency cap, but
      // remain reachable when the server filters by class before applying it.
      const incidents = Array.from({ length: limit + 3 }, (_, index) => ({
        incident_id: `fixture-${index}`,
        class: index === 1 || index >= limit ? 'unexpected' : 'expected',
        message: `failure ${index}`,
        event_count: 1,
        last_ts: new Date(Date.now() - index * 1000).toISOString(),
        events: index === 1 ? [{
          id: 100,
          execution_id: 'execution-fixture',
          ts: new Date().toISOString(),
          status: 'failed',
          actor: 'agent',
          surface: 'fixture.surface',
          tool: 'fixture-tool',
          run_id: 'jrun-incident-fixture',
          task_id: 'ORB-123',
          workspace_id: workspace || 'one',
          message: 'incident evidence fixture',
        }] : undefined,
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
    case '/api/audit/summary': summaryReads++; summaryQueries.push(url.searchParams.get('since')); return { events: summaryReads, failed_runs: terminalRunFixture ? 3 : 0, denials: summaryDenials, denials_sql: summaryDenials, denials_v2: 0, window: '24h',
      tool_call_failure_rate: { failed: 0, total: summaryReads, denied: 0 } };
    case '/api/codebase/analytics': {
      codebaseQueries.push(url.searchParams);
      const empty = { rows: [], below_support: 0 };
      return { window: url.searchParams.get('window'), rollup: url.searchParams.get('rollup'), min_support: Number(url.searchParams.get('min_support')),
        touched: 'declared_footprint', tasks_scanned: 0, computed_at: new Date().toISOString(), cache_ttl_seconds: 300,
        regressions: empty, lock_contention: { ...empty, wait_time_recorded: false }, footprint_misses: empty,
        change_coupling: { ...empty, excluded_large: 0, large_footprint_cap: 12 }, blocked: { ...empty, causes: [] },
        rejected: { ...empty, excluded_without_run: 0 }, overlap: { rows: [], ranked_paths: 0 },
        task_pilot: { ...empty, assessed: 0, unattributed: 0, without_assessment: 0, excluded_large: 0, excluded_no_diff: 0 } };
    }
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
    runDetailRequests.push({ path: url.pathname, kind: url.searchParams.get('kind'), workspace: url.searchParams.get('workspace') });
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
  if (metricsError && url.pathname === '/api/diagnostics/implement_one') return response({ error: 'Metrics fixture failure' }, 500);
  const payload = fixture(url);
  if (url.pathname === heldPath) return new Promise((resolve, reject) => pendingReads.push({ payload, resolve, reject }));
  const headers = url.pathname === '/api/audit' && !url.searchParams.has('before')
    ? { 'x-audit-next-before': 'fixture-audit-cursor' }
    : {};
  return response(payload, 200, headers);
};
await import('./app.js');
await settle();
const { persistScopeToUrl, setWorkspace, setWindow, getHost, getServingHost, getWorkspace, formatDateTime, fetchJsonPage } = await import('./js/common.js');
// Run links name the selected host, or the serving host when none is selected.
const runLinkHost = () => getHost() || getServingHost()?.name || null;
const { navigateToRun, setActiveTab } = await import('./js/router.js');
const refresh = () => {
  const button = node('refresh-btn');
  if (button.listeners) button.listeners.click(); else button.click();
};
const click = target => target.listeners ? target.listeners.click() : target.click();
const release = (request, payload = request.payload) => request.resolve(response(payload));
const text = id => node(id).textContent;
const busy = id => node(id).getAttribute ? node(id).getAttribute('aria-busy') : node(id)['aria-busy'];
const auditHeadPage = await fetchJsonPage('/api/audit?limit=50');
check(auditHeadPage.header('x-audit-next-before') === 'fixture-audit-cursor', 'continuing audit fixture response exposes its paging cursor');
const auditTerminalPage = await fetchJsonPage('/api/audit?limit=50&before=fixture-audit-cursor');
check(auditTerminalPage.header('x-audit-next-before') === null, 'terminal audit fixture response exposes an absent cursor as null');
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
refresh();
await settle();
const fleetIndicator = node('global-drain-state');
const fleetLabel = () => fleetIndicator.querySelector('.global-drain-label').textContent;
check(fleetRequests.length > 0, 'the header reads the fleet API');
check(getWorkspace() === 'one' && node('auto-drain-panel').dataset.drainState === 'idle', 'the selected workspace drain card stays idle');
check(!fleetIndicator.hidden && fleetIndicator.dataset.drainState === 'draining'
  && fleetLabel() === '1 of 2 workspaces draining', 'the selected workspace header includes the other workspace drain');
const selectedFleetLabel = fleetLabel();
const selectedWorkspaceReadinessReads = readinessReads;
const selectedWorkspaceCrewReads = crewReads;
click(fleetIndicator);
await settle();
check(getWorkspace() === null && node('auto-drain-panel').dataset.drainState === 'fleet', 'the header opens the aggregate fleet card');
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
check(!aggregateDrainIndicator.hidden && fleetLabel() === selectedFleetLabel
  && aggregateDrainIndicator.dataset.drainState === 'draining', 'aggregate header retains the fleet drain count');
check(aggregateDrainIndicator.getAttribute('aria-label').includes('Open fleet drains'), 'fleet indicator names its navigation action');
const fleetRows = [...node('auto-drain-body').querySelectorAll('.fleet-drains tbody tr')]
  .map(row => [...row.querySelectorAll('td')].map(cell => cell.textContent));
check(fleetRows.length === 2 && fleetRows[0][1] === 'one' && fleetRows[0][2] === 'idle'
  && fleetRows[0][4] === '2' && fleetRows[1][1] === 'two' && fleetRows[1][2] === 'draining'
  && fleetRows[1][3] === '1 / 4', 'aggregate fleet card shows idle eligible work beside the live drain');
check(readinessReads === selectedWorkspaceReadinessReads, 'aggregate view does not fetch one workspace drain status as if it were global');
check(crewReads === selectedWorkspaceCrewReads, 'aggregate view does not request a workspace crew registry');
check(text('rail-count-jobs') === '22', `aggregate Jobs rail shows the fleet total, not the one-item page: ${text('rail-count-jobs')}`);
globalThis.fleetScopeEvidence = {
  source: 'controlled API responses; shipped dashboard assets; no production drain exercised',
  selected: { workspace: 'one', drainPhase: 'idle', fleetLabel: selectedFleetLabel },
  aggregate: { workspace: getWorkspace(), fleetLabel: fleetLabel(), rows: fleetRows },
  requests: [...fleetRequests],
};
// The run exists in workspace two. Switching to All workspaces on its detail
// page must replace the content with a scope prompt and issue no unscoped reads.
navigateToRun('cross-workspace-run', 'two');
await settle();
check(text('run-detail-title') === 'Run cross-workspace-run', 'selected workspace loads the existing cross-workspace run');
for (const [suffix, kind] of [['', null], ['/events', null], ['/events', 'step_retry'], ['/logs', null]]) {
  check(runDetailRequests.some(request => request.path === `/api/runs/cross-workspace-run${suffix}` && request.kind === kind),
    `selected workspace loads run ${suffix || 'detail'}${kind ? ` filtered by ${kind}` : ''}`);
}
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
  { route: 'jobs/history', body: 'runs-body', path: '/api/job-runs', empty: list([]), emptyText: 'No job runs' },
  { route: 'diagnostics/incidents/errors', body: 'diag-body', path: '/api/diagnostics/errors', empty: { items: [], since: null, coverage_since: null }, emptyText: 'No error events' },
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
check(text('rail-count-jobs') === '2' && !node('rail-count-jobs').classList.contains('alert'), 'Jobs counts pending and running work in flight');
click(node('rail-count-jobs')); await settle();
check(window.location.hash === '#jobs/live' && text('jobs-live-body').includes('live-pending') && text('jobs-live-body').includes('live-running'), 'the in-flight badge opens Live with both active states');
setActiveTab('jobs/history'); await settle();
click(Array.from(node('runs-body').querySelectorAll('.runs-filter-button')).find(button => button.textContent === 'Failed')); await settle();
check(runQueries.at(-1) === 'failed', 'the Failed filter requests the server failure group');
check(new URL(window.location.href).searchParams.get('run_state') === 'failed', 'the Failed filter is kept in the address');
const failureRows = Array.from(node('runs-body').children).filter(row => row.dataset.key?.startsWith('run-'));
check(failureRows.length === 3, 'History includes every failure outcome');
for (const state of ['failed', 'timeout', 'interrupted']) {
  check(failureRows.some(row => row.textContent.includes(`terminal-${state}`)), `${state} run remains visible under the Failed filter`);
}
check(!text('runs-body').includes('terminal-success') && !text('runs-body').includes('terminal-cancelled'), 'Failed filter excludes successful and cancelled runs');
const runsSearch = () => node('runs-body').querySelector('.runs-query');
// Ordinary Jobs navigation keeps the search on screen.
setActiveTab('jobs/history'); await settle();
runsSearch().value = 'ORB-15159';
runsSearch().dispatchEvent(new Event('input')); await settle();
click(document.querySelector('.tab[data-tab="jobs"]')); await settle();
check(new URL(window.location.href).searchParams.get('task_id') === 'ORB-15159' && runsSearch().value === 'ORB-15159', 'the Jobs label keeps the task search');
runsSearch().value = '';
runsSearch().dispatchEvent(new Event('input')); await settle();
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
      click(node('tasks-body').querySelector(`[data-key="task-${shipFixture.id}"] .title`));
      // Task disclosure may use a view transition; wait for the rendered
      // detail instead of assuming five microtasks outlast its animation.
      for (let attempt = 0; attempt < 50 && !detailShip(); attempt++) {
        await new Promise(resolve => setTimeout(resolve, 10));
      }
    }
  };
  // The open detail sits in the side pane beside the list.
  const taskText = () => text('tasks-body') + text('task-detail-body');
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
  check(taskText().includes('Dispatch fixture failed'), `${origin}: dispatch failure appears in the current detail or row`);
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
  check(taskText().includes('Ship run is already in flight'), `${origin}: live duplicate error is visible`);

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
  pendingShips.shift()(response({ run_id: shipFixture.job_run_id, state: 'submitted', warning: 'Admissions throttled: cpu 97%. Running work is not touched.' }));
  await settle();
  // The ship receipt's warning is shown with the run it started.
  const notice = document.querySelector('.task-action-notice')?.textContent || '';
  check(notice === `${shipFixture.id}: ship run ${shipFixture.job_run_id} submitted\nWarning: Admissions throttled: cpu 97%. Running work is not touched.`, `${origin}: the ship warning follows the run: ${notice}`);
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
setActiveTab('analytics/metrics');
await settle();
check(text('metrics-body').includes('Metrics fixture failure'), 'HTTP failure visible in Metrics panel');
check(node('conn-status').className.includes('orange') && text('meta-text').includes('Metrics') && !text('meta-text').includes('offline'), 'one HTTP panel error is amber, names the failing panel, and does not imply offline');
check(summaryReads > priorSummaryReads && summaryToolCalls() === String(summaryReads), 'audit summary renders updated data independently of failed panel');
networkDown = true;
refresh(); await settle();
check(node('conn-status').className.includes('red') && text('meta-text').includes('offline'), 'network outage reports offline');
networkDown = false;
metricsError = false;
refresh(); await settle();
check(node('conn-status').className.includes('green'), 'connection recovers');
check(text('meta-text').includes('refreshed') && !text('meta-text').includes('analytics/'), 'connection line names the destination, not its route');
check(document.title !== 'orbit' && document.title.endsWith('orbit'), 'the page title names the current destination');
// Exercise Health through the full request builder and DOM rendering path.
healthFixture = true;
for (const selectedWindow of ['24h', '7d']) {
  setWindow(selectedWindow);
  for (const subtab of ['metrics', 'errors']) {
    setActiveTab(subtab === 'metrics' ? 'analytics/metrics' : 'diagnostics/incidents/errors'); await settle(); refresh(); await settle();
    check(healthQueries.at(-1).window === selectedWindow, `${subtab} requests selected window ${selectedWindow}`);
    check(text(subtab === 'metrics' ? 'metrics-count' : 'diag-count').includes(selectedWindow), `${subtab} header labels selected range`);
  }
  const coverageLabel = `covers since ${formatDateTime(errorsRetentionStart)}`;
  check(text('diag-count').includes(coverageLabel) === (selectedWindow === '7d'),
    `errors header names its coverage start only when retention starts after the ${selectedWindow} window: ${text('diag-count')}`);
  check(node('diag-body').querySelector('.c-target').textContent === 'orbit.job.step_finished', 'process target is displayed');
  check(node('diag-body').querySelector('.c-recovered').textContent === 'recovered — run succeeded', 'historical step failure shows the later run recovery');
  check(!node('diag-body').querySelector('.c-provider'), 'all-empty provider column is removed from the Errors table');
  const messageDisclosure = node('diag-body').querySelector('.diagnostics-errors-main .c-message details.error-message');
  check(messageDisclosure && !messageDisclosure.open && messageDisclosure.querySelector('summary'), 'error message starts as a two-line expandable disclosure');
  const errorsMain = node('diag-body').querySelector('.diagnostics-errors-main');
  const errorsBounds = errorsMain.getBoundingClientRect();
  const panelBounds = node('diag-body').getBoundingClientRect();
  const previewStyle = getComputedStyle(messageDisclosure.querySelector('.error-message-preview'));
  check(errorsBounds.left >= panelBounds.left - 1 && errorsBounds.right <= panelBounds.right + 1
    && getComputedStyle(errorsMain).overflowX === 'auto'
    && previewStyle.webkitLineClamp === '2', 'at 1440x900 the Errors table stays in its panel and the message preview wraps to two lines');
  messageDisclosure.open = true;
  check(messageDisclosure.querySelector('.error-message-full').textContent === longErrorMessage, 'expanding an error message reveals its full text');
  const errorRunLink = node('diag-body').querySelector('.diagnostics-errors-main .c-job_run a');
  const errorRunUrl = new URL(errorRunLink.href);
  const currentDashboardUrl = new URL(window.location.href);
  check(errorRunUrl.hash === '#runs?run_id=jrun-health-fixture'
    && errorRunUrl.searchParams.get('workspace') === getWorkspace()
    && errorRunUrl.searchParams.get('host') === runLinkHost(), 'Errors run link opens its run with the current host and workspace');
  if (selectedWindow === '7d') {
    const loadMore = node('diag-count').querySelector('.diagnostics-load-more');
    check(loadMore, 'coverage truncation exposes a Load more errors control');
    const loadMoreUrl = new URL(loadMore.href);
    check(loadMore && loadMoreUrl.searchParams.get('diag') === '100'
      && loadMoreUrl.searchParams.get('workspace') === currentDashboardUrl.searchParams.get('workspace')
      && loadMoreUrl.searchParams.get('host') === currentDashboardUrl.searchParams.get('host'), 'older-errors control increases the cap and preserves dashboard scope');
    click(loadMore); await settle();
    check(healthQueries.at(-1).limit === 100 && new URL(window.location.href).searchParams.get('diag') === '100', 'loading older errors refetches the selected window at the higher cap');
  }
  const internal = node('diag-body').querySelector('details.agent-diagnostics');
  check(internal && (selectedWindow !== '24h' || !internal.open), 'recoverable agent diagnostics start collapsed');
  const mainRows = [...node('diag-body').querySelectorAll('.diagnostics-errors-main tbody > tr')]
    .map(row => row.querySelector('.c-message').textContent);
  check(mainRows.length === 2 && mainRows.every(message => !/verification failed|request timed out/.test(message)), 'real codex patch and model-timeout rows leave the main table');
  check(internal.querySelectorAll('tbody > tr').length === 2, 'both recognised codex shapes collapse under agent diagnostics');
  internal.open = true;
  const shortened = internal.querySelector('.error-message-preview');
  const internalMessage = internal.querySelector('.c-message');
  check(shortened.textContent.includes('[worktree]/src/lib.rs') && !shortened.textContent.includes('/home/operator'), 'worktree prefix shortened in the message preview');
  internalMessage.querySelector('details.error-message').open = true;
  check(internalMessage.querySelector('.error-message-full').textContent.includes('/home/operator')
    && internalMessage.title.includes('/home/operator'), 'expanded diagnostic retains the full failure text');
}
click(node('diag-body').querySelector('.diagnostics-errors-main .c-job_run a')); await settle();
check(window.location.hash === '#runs/jrun-health-fixture', 'Errors run link navigates to the run detail route');
setActiveTab('analytics/metrics'); await settle();
check(node('metrics-body').querySelectorAll('.mx-sc').length === 4 && node('metrics-body').querySelector('.mx-sc-value').textContent === '26.0m',
  'Metrics renders the four series cards, All first with the window average');
setActiveTab('diagnostics/incidents'); await settle();
check(healthQueries.at(-1).class === 'unexpected', 'default incident request isolates unexpected failures before limit');
const incidentRows = () => [...node('diag-body').querySelectorAll('.incident-row')];
const checkIncidentSelection = selectedClass => {
  const group = node('diag-body').querySelector('.incident-tiles');
  check(group.getAttribute('role') === 'group' && group.getAttribute('aria-label'), 'incident class tiles have a named accessible group');
  const tiles = [...group.querySelectorAll('button')];
  check(tiles.map(tile => tile.dataset.class).join() === 'unexpected,expected,denied,diagnostic,deferred', 'every incident class has a tile');
  for (const tile of tiles) {
    check(tile.getAttribute('type') === 'button', 'class tiles have an explicit button type');
    check(tile.getAttribute('aria-pressed') === String(tile.dataset.class === selectedClass), 'only the active class tile is pressed');
  }
  const selected = group.querySelector('[aria-pressed="true"]');
  const other = group.querySelector('[aria-pressed="false"]');
  check(getComputedStyle(selected).backgroundColor !== getComputedStyle(other).backgroundColor, 'pressed incident tile renders a distinct selected background');
};
check(incidentRows().length === 4 && incidentRows().every(row => row.classList.contains('unexpected')), 'default list includes older unexpected failures beyond a mixed-class recency cap');
checkIncidentSelection('unexpected');
click(node('diag-body').querySelector('.incident-tile[data-class="expected"]')); await settle();
const incidentLimit = healthQueries.at(-1).limit;
check(healthQueries.at(-1).class === 'expected', 'a class tile requests its class before the limit');
check(incidentRows().length === incidentLimit - 1 && incidentRows().every(row => row.classList.contains('expected')), 'one click isolates expected paths');
checkIncidentSelection('expected');
click(node('diag-body').querySelector('.incident-tile[data-class="unexpected"]')); await settle();
check(incidentRows().length === 4 && incidentRows().every(row => row.classList.contains('unexpected')), 'one click restores older unexpected failures');
checkIncidentSelection('unexpected');
check(incidentRows()[0].dataset.key === 'incident-single:fixture-1' && incidentRows()[0].getAttribute('aria-pressed') === 'true', 'the newest incident is selected for the detail panel');
click(node('diag-body').querySelector('.incident-evidence-details summary')); await settle();
const evidenceRow = node('diag-body').querySelector('.incident-evidence tbody tr');
const evidenceRunLink = evidenceRow.querySelector('td:nth-child(8) a');
const evidenceTaskLink = evidenceRow.querySelector('td:nth-child(9) a');
const evidenceRunUrl = new URL(evidenceRunLink.href);
const evidenceTaskUrl = new URL(evidenceTaskLink.href);
check(evidenceRunUrl.hash === '#runs?run_id=jrun-incident-fixture'
  && evidenceTaskUrl.hash === '#tasks?open=ORB-123'
  && evidenceRunUrl.searchParams.get('workspace') === getWorkspace()
  && evidenceTaskUrl.searchParams.get('workspace') === getWorkspace()
  && evidenceRunUrl.searchParams.get('host') === runLinkHost()
  && evidenceTaskUrl.searchParams.get('host') === getHost(), 'incident evidence links open the run and task while retaining host and workspace');
click(evidenceTaskLink); await settle();
check(document.querySelector('.tab-pane.active')?.dataset.tab === 'tasks', 'incident evidence task link opens the Tasks view');
healthFixture = false;
setWindow('24h');
setActiveTab('analytics/metrics'); await settle();
const realTimeout = globalThis.setTimeout;
const realFetch = globalThis.fetch;
globalThis.setTimeout = (fn, ms, ...args) => realTimeout(fn, ms === 30000 ? 1 : ms, ...args);
globalThis.fetch = (_path, options) => new Promise((_resolve, reject) => {
  options.signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')));
});
refresh(); await settle();
check(busy('metrics-body') === 'false' && text('metrics-body').includes('timed out'), 'hung request times out and clears busy state');
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
const toolCallsBeforeHostHang = summaryToolCalls();
await runPoll();
const liveTimers = () => scheduledPolls.filter(entry => !entry.handle.cancelled);
const polls = liveTimers().filter(entry => entry.fn.name === 'runRefresh');
check(hungHostSignals.length >= 1 && hungHostSignals.every(signal => signal && !signal.aborted), 'host request stays unanswered and carries an abort signal');
check(summaryReads > summaryBeforeHostHang && summaryToolCalls() !== toolCallsBeforeHostHang, 'other panels refresh while host resources never answer');
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

// Analytics › Codebase: its hash restores the view through the router. The
// denial badge on Health opens Health › Incidents › Events filtered to
// denials; the Events tab itself opens on failures and denials.
summaryDenials = 4;
window.location.hash = '#analytics/codebase?window=30d&rollup=crate';
await settle();
check(node('analytics-codebase-main').style.display !== 'none' && document.querySelector('.tab-pane.active')?.dataset.tab === 'analytics', '#analytics/codebase restores the Codebase view');
check(document.querySelector('#analytics-subtabs .subtab.active')?.dataset.subtab === 'codebase' && node('reliability-views').hidden, 'Codebase is the active Analytics subtab, without the Reliability views');
const pressedValue = id => node(id).querySelector('[aria-pressed="true"]')?.dataset.value;
check(pressedValue('codebase-window') === '30d' && pressedValue('codebase-rollup') === 'crate' && node('codebase-min-support').value === '15', 'the hash restores the codebase controls');
const codebaseQuery = codebaseQueries.at(-1);
check(codebaseQuery && codebaseQuery.get('window') === '30d' && codebaseQuery.get('rollup') === 'crate' && codebaseQuery.get('min_support') === '15', 'the restored view requests its selection');
check(node('codebase-body').querySelectorAll('.codebase-tile[aria-pressed]').length === 8, 'the codebase view renders its eight ranking tiles');
const denialBadge = node('rail-count-denials');
check(denialBadge.closest('.tab')?.dataset.tab === 'diagnostics', 'the denial badge sits on the Health rail entry');
check(!denialBadge.hidden && denialBadge.textContent === '4 denied', 'the denial badge shows the canonical denial count');
denialBadge.click();
await settle();
check(window.location.hash === '#diagnostics/incidents/events?since=24h&status=denied' && node('diagnostics-events-main').style.display !== 'none', `the denial badge opens Events filtered to denials: ${window.location.hash}`);
check(document.querySelector('#incidents-views .subtab.active')?.dataset.view === 'events', 'the badge lands on the Events view of Incidents');
click(document.querySelector('#incidents-views [data-view="incidents"]')); await settle();
check(window.location.hash.startsWith('#diagnostics/incidents?') && node('diagnostics-events-main').style.display === 'none', 'the Incidents view tab leaves Events');
click(document.querySelector('#incidents-views [data-view="events"]')); await settle();
check(new URLSearchParams(window.location.hash.split('?')[1]).get('status') === 'denied', 'Events keeps the filter it was left with');
summaryDenials = 0;

// Tools uses Analytics' selected window rather than a remembered Events
// cutoff. A tool row then opens evidence for the window its summary shows.
setActiveTab('diagnostics/incidents/events?since=1h&status=denied'); await settle();
setActiveTab('analytics/reliability/tools?window=7d'); await settle();
check(summaryQueries.at(-1) === '7d', 'Tools fetches the Analytics window independently of Events');
click(document.querySelector('.rail .tab[data-tab="diagnostics"]')); await settle();
check(new URLSearchParams(window.location.hash.split('?')[1]).get('since') === '1h', 'opening Tools preserves the remembered Events cutoff');
setActiveTab('analytics/reliability/tools?window=all'); await settle();
check(summaryQueries.at(-1) === '24h', 'Tools keeps the bounded summary fallback for an all window');
setActiveTab('analytics/reliability/tools?window=7d'); await settle();
const auditRenderer = await import('./js/audit.js');
auditRenderer.renderAuditSummary({ window: '7d', tool_call_failures_by_tool: [
  { tool: 'orbit.task.show', failed: 1, total: 5, rate: 0.2, unexpected: 1, denied: 0 },
] });
click(node('audit-summary-body').querySelector('tbody tr')); await settle();
const toolRoute = new URLSearchParams(window.location.hash.split('?')[1]);
check(toolRoute.get('since') === '7d' && toolRoute.get('tool') === 'orbit.task.show' && toolRoute.get('status') === 'denied',
  'Tools drill-down carries the displayed window and keeps the Events status filter');
setActiveTab('diagnostics/incidents/events?ids=9001'); await settle();
check(document.querySelector('#audit-filter .chip.active[data-status]')?.dataset.status === 'all',
  'exact incident evidence highlights all statuses because its request has no status restriction');
check(auditQueries.at(-1)?.get('ids') === '9001' && !auditQueries.at(-1).has('status'),
  'exact incident evidence requests its IDs without a status filter');
setWindow('24h');
persistScopeToUrl();
setActiveTab('diagnostics/incidents/events?since=24h&status=denied'); await settle();

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
  setActiveTab('analytics/metrics');
  refresh();
  await settle();
};
globalThis.showHealthEvidence = async subtab => {
  healthFixture = true;
  heldPath = null;
  setWindow('7d');
  setActiveTab({ metrics: 'analytics/metrics', incidents: 'diagnostics/incidents', errors: 'diagnostics/incidents/errors' }[subtab]);
  refresh();
  await settle();
};
globalThis.loadingTestsPassed = true;
console.log('Dashboard loading, ordering, empty, error and recovery scenarios passed.');
