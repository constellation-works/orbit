// Usage: node dashboard_fleet_drains_browser.mjs <playwright/index.mjs> <evidence-dir>
// Boot the shipped app, exercise the real fleet-to-scope callback, and measure
// the full page at 375px with long host/workspace names and a failed host.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';
const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const initializer = `
  const streamCadence = new URLSearchParams(location.search).has('streamCadence');
  globalThis.eventSources = [];
  globalThis.EventSource = class {
    constructor(url) {
      this.url = url;
      this.readyState = 0;
      globalThis.eventSources.push(this);
      if (streamCadence) queueMicrotask(() => {
        this.readyState = 1;
        this.opened = true;
        this.onopen?.();
      });
    }
    close() { this.readyState = 2; }
  };
  globalThis.fleetRequests = [];
  const budget = new URLSearchParams(location.search).get('budget') === '1';
  const liveLocal = new URLSearchParams(location.search).has('liveLocal');
  const noLive = new URLSearchParams(location.search).has('noLive');
  const fleetFailure = new URLSearchParams(location.search).has('fleetFailure');
  const forecastSource = new URLSearchParams(location.search).get('forecastSource');
  const storedForecastAt = new Date(Date.now() - 4 * 60 * 60 * 1000).toISOString();
  const holdFleet = new URLSearchParams(location.search).get('holdFleet') === '1';
  const remoteFleetFailure = new URLSearchParams(location.search).has('remoteFleetFailure');
  globalThis.remoteDraining = false;
  globalThis.remoteFleetPending = false;
  const remoteFleetReady = new Promise(resolve => { globalThis.releaseRemoteFleet = resolve; });
  const inactive = new URLSearchParams(location.search).has('inactive');
  const workspaces = [
    { id: 'idle', name: 'Idle workspace with ready work', status: 'active', is_default: true },
    { id: 'busy', name: 'BusyWorkspaceWithAVeryLongUnbrokenNameForMobileLayout', status: inactive ? 'inactive' : 'active' },
  ];
  const row = (id, phase, eligible) => ({ workspace_id: id, workspace_name: workspaces.find(w => w.id === id).name,
    phase, slots_used: 2, slots_ceiling: 4, eligible_count: eligible,
    top_wait_reason: 'context_lock_conflict', holds: ['codex usage limit', 'claude authentication failed'] });
  globalThis.fetch = async (input, options = {}) => {
    const url = new URL(input, location.href);
    globalThis.fleetRequests.push(url.pathname + url.search);
    const host = url.pathname.startsWith('/api/on/') ? url.pathname.split('/')[3] : 'owner';
    const api = url.pathname.replace(/^\\/api\\/on\\/[^/]+/, '/api');
    const json = (body, status = 200) => new Response(JSON.stringify(body), { status });
    if (api === '/api/hosts') {
      const hosts = [
        { name: 'owner', machine_id: 'owner-id', local: true },
        { name: 'remote', machine_id: 'remote-id', local: false },
      ];
      if (!budget) hosts.push({ name: 'UnreachableHostWithALongNameForMobileLayout', machine_id: 'offline-id', local: false });
      return json({ hosts });
    }
    if (api.endsWith('/connection')) return json({ reachable: true, forward_writes: { authorized: false } });
    if (api === '/api/fleet/drains') {
      if (fleetFailure) return json({ error: 'Fleet unavailable' }, 503);
      if (host.startsWith('Unreachable')) return json({ error: 'Host unreachable' }, 502);
      if (holdFleet && host === 'remote') {
        globalThis.remoteFleetPending = true;
        await remoteFleetReady;
        globalThis.remoteFleetPending = false;
        if (remoteFleetFailure) return json({ error: 'Host unreachable' }, 502);
      }
      const remotePhase = streamCadence ? (globalThis.remoteDraining ? 'draining' : 'idle') : (noLive ? 'idle' : 'draining');
      const ownerRows = streamCadence ? [row('idle', 'idle', 4)] : [row('idle', 'idle', 4), row('busy', liveLocal && !noLive ? 'pulling' : 'idle', 0)];
      return json({ items: host === 'remote' ? [row('busy', remotePhase, 0)] : ownerRows });
    }
    if (api === '/api/workspaces') return json(workspaces);
    if (api === '/api/workflows/auto/readiness') return json({ controls_authorized: false, capacity: { drain_phase: 'idle' }, tasks: [] });
    if (api === '/api/jobs/failure-streaks') {
      if (!budget) return json({ threshold: 5, failing: [] });
      if (globalThis.failJobFailures) return json({ error: 'streaks unavailable' }, 500);
      const workspace = url.searchParams.get('workspace');
      const busy = workspace === 'busy';
      return json({ threshold: 3, failing: [{
        job_id: busy ? 'sweep' : 'fixture',
        count: busy ? 6 : 4,
        since: '2026-10-02T00:00:00Z',
        latest_run_id: busy ? 'jrun-busy-failed' : 'jrun-idle-failed',
        latest_error: busy ? 'busy streak' : 'idle streak',
        same_error: true,
        truncated: false,
      }] });
    }
    if (budget && api === '/api/jobs') {
      const workspace = url.searchParams.get('workspace');
      return json([{ job_id: workspace === 'busy' ? 'sweep' : 'fixture', manual_runnable: true }]);
    }
    if (api === '/api/routines') {
      const workspace = url.searchParams.get('workspace');
      const routines = [];
      if (budget && (workspace === 'idle' || workspace === 'busy')) {
        const busy = workspace === 'busy';
        routines.push({
          name: 'Fixture routine',
          source: busy ? 'BusyWorkspaceWithAVeryLongUnbrokenNameForMobileLayout' : 'Idle workspace with ready work',
          target: busy ? 'job:sweep' : 'job:fixture',
          enabled: true,
        });
      }
      return json({ routines, clock: {} });
    }
    if (api === '/api/tasks/locks') return json({ by_task: [], total_locked: 0, total_tasks: 0 });
    if (api === '/api/crews') return json({ crews: [] });
    // Render real Metrics controls so every trend's poll can be measured.
    const window = { key: url.searchParams.get('since'), bucket_seconds: 3600 };
    if (api === '/api/diagnostics/implement_one') return json({ window,
      outcome: url.searchParams.get('outcome'), current: {}, previous: {}, buckets: [] });
    if (api === '/api/diagnostics/backlog_forecast') return json({
      ...(forecastSource === 'stored' ? { served_from: 'stored', generated_at: storedForecastAt, stored: { latest_at: storedForecastAt } } : {}),
      ...(forecastSource === 'live' ? { served_from: 'live', generated_at: new Date().toISOString() } : {}),
      backlog: { backlog: 3, in_progress: 1 },
      forecast: { time_to_drain_hours: { p50: 1, p90: 2 }, usd: { p50: 3, p90: 4 } },
    });
    if (api === '/api/diagnostics/completions') return json({ window, buckets: [],
      current: { done: { total: 0, low: 0, medium: 0, hard: 0, other: 0 }, rejected: 0, archived: 0 } });
    if (api === '/api/diagnostics/defects') return json({ error: 'Not supported by fixture host' }, 404);
    if (holdFleet && api === '/api/tasks') return json({ items: [
      { id: 'TASK-FIXTURE', title: 'Local task rendered during fleet warmup', status: 'backlog', priority: 'medium', type: 'bug' },
    ], total: 1 });
    if (holdFleet && api === '/api/runs/jrun-fixture') return json({
      run: { run_id: 'jrun-fixture', job_id: 'local-job-during-fleet-warmup', state: 'success',
        started_at: '2026-10-17T00:00:00Z', finished_at: '2026-10-17T00:01:00Z' }, steps: [],
    });
    if (holdFleet && api === '/api/runs/jrun-fixture/events') return json(url.searchParams.has('kind') ? [] : { events: [], total: 0, offset: 0 });
    if (holdFleet && api === '/api/runs/jrun-fixture/logs') return json([]);
    return json({ items: [], total: 0 });
  };
  globalThis.dashboardFetchesStarted = 0;
  globalThis.dashboardFetchesInFlight = 0;
  let resolveDashboardFetchIdle = null;
  globalThis.dashboardFetchIdle = Promise.resolve();
  globalThis.waitForDashboardFetches = () => globalThis.dashboardFetchIdle;
  const fixtureFetch = globalThis.fetch;
  globalThis.fetch = async (...args) => {
    if (globalThis.dashboardFetchesInFlight === 0) {
      globalThis.dashboardFetchIdle = new Promise(resolve => { resolveDashboardFetchIdle = resolve; });
    }
    globalThis.dashboardFetchesInFlight++;
    globalThis.dashboardFetchesStarted++;
    try {
      return await fixtureFetch(...args);
    } finally {
      globalThis.dashboardFetchesInFlight--;
      if (globalThis.dashboardFetchesInFlight === 0) {
        resolveDashboardFetchIdle?.();
        resolveDashboardFetchIdle = null;
      }
    }
  };
  await import('/app.js');
  globalThis.fleetReady = true;
`;
const POLL_VIEWS = [
  ['tasks', 'tasks'],
  ['run detail', 'runs/jrun-fixture'],
  ['frictions', 'knowledge/frictions'],
  ['analytics/metrics', 'analytics/metrics'],
  ['jobs live', 'jobs/live'],
  ['jobs history', 'jobs/history'],
  ['operations/auto-tasks', 'operations/auto-tasks'],
  ['operations/routines', 'operations/routines'],
  ['settings', 'config/effective'],
  ['workspaces', 'config/hosts'],
];

async function settleDashboard(page) {
  await page.waitForFunction(() => {
    const text = document.getElementById('meta-text')?.textContent || '';
    return text.length > 0 && !text.startsWith('fetching');
  });
}

async function assertIndependentWarmup(browser, port) {
  const results = [];
  for (const [label, route, selector, content, endpoint] of [
    ['tasks', 'tasks', '#tasks-body .title', 'Local task rendered during fleet warmup', '/api/tasks'],
    ['run detail', 'runs/jrun-fixture', '#run-detail-meta .value', 'local-job-during-fleet-warmup', '/api/runs/jrun-fixture'],
  ]) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    const snapshot = () => page.evaluate(() => ({
      pending: globalThis.remoteFleetPending, initialized: Boolean(globalThis.fleetReady),
      requests: globalThis.fleetRequests, url: location.href,
      indicator: document.querySelector('.global-drain-label')?.textContent,
    }));
    try {
      await page.goto(`http://127.0.0.1:${port}/?budget=1&holdFleet=1&workspace=idle#${route}`);
      await page.waitForFunction(() => globalThis.remoteFleetPending);
      // The response remains held until after a real local row/detail is visible.
      await page.locator(selector).filter({ hasText: content }).waitFor({ state: 'visible', timeout: 5000 });
      await page.waitForFunction(() => globalThis.fleetReady);
      const before = await snapshot();
      assert.equal(before.pending, true, `${label} renders while the remote fleet response is held`);
      const localReads = before.requests.filter(value => {
        const url = new URL(value, 'http://fixture');
        return url.pathname === endpoint && url.searchParams.get('workspace') === 'idle';
      });
      assert.equal(localReads.length, 1, `${label} requests the selected local workspace once before release`);
      const fleetReads = before.requests.filter(value => value.includes('/fleet/drains'));
      assert.deepEqual(fleetReads.map(value => new URL(value, 'http://fixture').pathname),
        ['/api/on/owner/fleet/drains', '/api/on/remote/fleet/drains'],
        `${label} warmup and initial view share one in-flight request per host`);
      await page.screenshot({ path: path.join(evidence, `warmup-${route.split('/')[0]}-held.png`), fullPage: true });
      await page.evaluate(() => globalThis.releaseRemoteFleet());
      await page.waitForFunction(() => document.querySelector('.global-drain-label')?.textContent.includes('1 of 3 workspaces draining'));
      await settleDashboard(page);
      const after = await snapshot();
      assert.equal(after.pending, false);
      assert.equal(after.url, before.url, `${label} late fleet completion preserves the route and workspace`);
      assert.equal(await page.locator(selector).filter({ hasText: content }).isVisible(), true,
        `${label} stays visible after late fleet completion`);
      assert.deepEqual(after.requests.filter(value => value.includes('/fleet/drains')), fleetReads,
        `${label} late completion does not refetch the fleet`);
      assert.deepEqual(errors, []);
      results.push({ view: label, before, after, errors });
    } catch (error) {
      fs.writeFileSync(path.join(evidence, 'warmup-failure.json'), JSON.stringify({ view: label, ...await snapshot(), errors }, null, 2));
      throw error;
    } finally {
      await page.close();
    }
  }
  return results;
}

async function assertFleetPartial(browser, port) {
  const results = [];
  for (const scenario of [
    { name: 'success', extra: '', settledPhase: 'draining' },
    { name: 'failure', extra: '&remoteFleetFailure=1', settledPhase: 'unavailable' },
  ]) {
    for (const width of [1440, 1024]) {
      const page = await browser.newPage({ viewport: { width, height: 900 } });
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      try {
        await page.goto(`http://127.0.0.1:${port}/?budget=1&holdFleet=1${scenario.extra}&workspace=all#tasks?dock=drain`);
        await page.waitForFunction(() => globalThis.remoteFleetPending);
        await page.locator('#auto-drain-body .fleet-row[data-phase="idle"]').first().waitFor({ state: 'visible', timeout: 5000 });
        const before = await page.evaluate(() => ({
          pending: globalThis.remoteFleetPending,
          requests: globalThis.fleetRequests,
          url: location.href,
          workspace: document.querySelector('#workspace-select')?.value,
          indicator: document.querySelector('.global-drain-label')?.textContent,
          rows: [...document.querySelectorAll('#auto-drain-body .fleet-row')].map(row => row.getAttribute('data-phase')),
          hostStates: [...document.querySelectorAll('#auto-drain-body .fleet-host')].map(section => section.getAttribute('data-host-state')),
        }));
        assert.equal(before.pending, true, `fleet partial ${scenario.name} ${width}: remote still held`);
        assert.equal(before.rows.filter(phase => phase === 'idle').length, 2, `fleet partial ${scenario.name} ${width}: ready local rows shown before release: ${JSON.stringify(before)}`);
        assert.equal(before.rows.filter(phase => phase === 'loading').length, 1, `fleet partial ${scenario.name} ${width}: pending host shown as loading, distinct from idle/unavailable: ${JSON.stringify(before)}`);
        assert.ok(before.hostStates.includes('loading'), `fleet partial ${scenario.name} ${width}: pending host heading marked loading`);
        assert.ok((before.indicator || '').includes('loading'), `fleet partial ${scenario.name} ${width}: indicator marks loading: ${before.indicator}`);
        assert.equal(before.workspace, '', `fleet partial ${scenario.name} ${width}: aggregate selection preserved`);
        const fleetReadsBefore = before.requests.filter(value => value.includes('/fleet/drains'));
        assert.deepEqual(fleetReadsBefore.map(value => new URL(value, 'http://fixture').pathname),
          ['/api/on/owner/fleet/drains', '/api/on/remote/fleet/drains'],
          `fleet partial ${scenario.name} ${width}: each host requested once`);
        assert.ok(await page.locator('#auto-drain-body .fleet-row[data-phase="loading"]').textContent().then(text => text.includes('loading')), 'pending row text names loading');
        // Focus a ready local workspace while the remote host is held; its
        // keyboard target must survive that other host settling.
        const readyButton = page.locator('#auto-drain-body .fleet-row[data-phase="idle"] .fleet-workspace').first();
        const readyLabel = await readyButton.getAttribute('aria-label');
        assert.ok(readyLabel, `fleet partial ${scenario.name} ${width}: ready workspace button has an accessible name`);
        await readyButton.focus();
        assert.equal(await page.evaluate(() => document.activeElement?.getAttribute('aria-label')), readyLabel,
          `fleet partial ${scenario.name} ${width}: ready workspace button focused before release`);
        if (scenario.name === 'success') {
          await page.screenshot({ path: path.join(evidence, `dashboard-fleet-remote-pending-dark-${width}.png`) });
        }
        await page.evaluate(() => globalThis.releaseRemoteFleet());
        await page.waitForFunction(() => !document.querySelector('#auto-drain-body .fleet-row[data-phase="loading"]'), null, { timeout: 5000 });
        // The settled host sits behind its tab; the focused host stays shown.
        await page.locator(`#auto-drain-body .fleet-row[data-phase="${scenario.settledPhase}"]`).first().waitFor({ state: 'attached', timeout: 5000 });
        const after = await page.evaluate(() => ({
          url: location.href,
          workspace: document.querySelector('#workspace-select')?.value,
          requests: globalThis.fleetRequests,
          rows: [...document.querySelectorAll('#auto-drain-body .fleet-row')].map(row => row.getAttribute('data-phase')),
        }));
        assert.equal(after.rows.filter(phase => phase === 'idle').length, 2, `fleet partial ${scenario.name} ${width}: ready rows kept after settle`);
        assert.equal(after.workspace, '', `fleet partial ${scenario.name} ${width}: selected workspace unchanged after settle`);
        assert.equal(after.url, before.url, `fleet partial ${scenario.name} ${width}: URL unchanged after settle`);
        assert.deepEqual(after.requests.filter(value => value.includes('/fleet/drains')), fleetReadsBefore,
          `fleet partial ${scenario.name} ${width}: settle does not refetch fleet`);
        assert.deepEqual(errors, []);
        if (scenario.name === 'success') {
          await page.screenshot({ path: path.join(evidence, `dashboard-fleet-remote-settled-dark-${width}.png`) });
        } else {
          await page.screenshot({ path: path.join(evidence, `dashboard-fleet-remote-failed-dark-${width}.png`) });
        }
        const focusAfter = await page.evaluate(() => {
          const active = document.activeElement;
          return { label: active?.getAttribute('aria-label'), connected: Boolean(active?.isConnected), tag: active?.tagName };
        });
        assert.deepEqual(focusAfter, { label: readyLabel, connected: true, tag: 'BUTTON' },
          `fleet partial ${scenario.name} ${width}: settling another host preserves the ready workspace focus: ${JSON.stringify(focusAfter)}`);
        await page.keyboard.press('Enter');
        await page.waitForFunction(() => document.querySelector('#workspace-select')?.value === 'idle', null, { timeout: 5000 });
        assert.deepEqual(errors, [], `fleet partial ${scenario.name} ${width}: Enter selects the focused ready workspace without page errors`);
        results.push({ scenario: scenario.name, width, before, after, readyLabel, focusAfter });
      } finally {
        await page.close();
      }
    }
  }
  fs.writeFileSync(path.join(evidence, 'fleet-partial.json'), JSON.stringify(results, null, 2));
  return results;
}

async function assertFleetCadenceWithConnectedStream(browser, port) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await page.clock.install();
    await page.goto(`http://127.0.0.1:${port}/?budget=1&streamCadence=1&workspace=idle#tasks`);
    await page.waitForFunction(() => globalThis.fleetReady);
    await page.waitForFunction(async () => {
      const { changeStreamLive } = await import('/js/change-stream.js');
      return changeStreamLive() && globalThis.eventSources.some(source => source.opened && source.readyState === 1);
    });
    await page.evaluate(() => globalThis.waitForDashboardFetches());
    const before = await page.evaluate(() => ({
      indicator: document.querySelector('.global-drain-label')?.textContent,
      workspace: document.querySelector('#workspace-select')?.value,
      fleetReads: globalThis.fleetRequests.filter(url => url.includes('/fleet/drains')),
    }));
    assert.equal(before.indicator, 'Fleet idle · 2 workspaces', `connected-stream baseline remains idle: ${JSON.stringify(before)}`);
    assert.equal(before.workspace, 'idle', 'the change stream stays scoped to the selected local workspace');
    assert.deepEqual(before.fleetReads.map(value => new URL(value, 'http://fixture').pathname),
      ['/api/on/owner/fleet/drains', '/api/on/remote/fleet/drains'],
      'startup reads both registered hosts once');

    await page.evaluate(() => { globalThis.remoteDraining = true; });
    const startedBeforePanelRefresh = await page.evaluate(() => globalThis.dashboardFetchesStarted);
    await page.locator('#refresh-btn').click();
    await page.evaluate(() => globalThis.waitForDashboardFetches());
    const afterPanelRefresh = await page.evaluate(() => ({
      indicator: document.querySelector('.global-drain-label')?.textContent,
      started: globalThis.dashboardFetchesStarted,
      fleetReads: globalThis.fleetRequests.filter(url => url.includes('/fleet/drains')),
    }));
    assert.ok(afterPanelRefresh.started > startedBeforePanelRefresh, 'the selected-workspace panel refresh completed');
    assert.deepEqual(afterPanelRefresh.fleetReads, before.fleetReads,
      'a selected-workspace panel refresh does not refetch fleet data outside the stream scope');
    assert.equal(afterPanelRefresh.indicator, before.indicator,
      'a selected-workspace panel refresh does not paint its cached fleet snapshot as a new fleet result');

    await page.clock.fastForward(60_001);
    await page.waitForFunction(() => document.querySelector('.global-drain-label')?.textContent === '1 of 2 workspaces draining');
    const after = await page.evaluate(() => ({
      indicator: document.querySelector('.global-drain-label')?.textContent,
      workspace: document.querySelector('#workspace-select')?.value,
      host: new URL(location.href).searchParams.get('host'),
      fleetReads: globalThis.fleetRequests.filter(url => url.includes('/fleet/drains')),
    }));
    assert.equal(after.workspace, 'idle', 'the cadence refresh does not switch workspace scope');
    assert.equal(after.host, null, 'the cadence refresh does not switch host scope');
    assert.equal(after.fleetReads.length, 4, `one fleet cadence adds one read per host: ${JSON.stringify(after.fleetReads)}`);
    assert.deepEqual(after.fleetReads.map(value => new URL(value, 'http://fixture').pathname),
      ['/api/on/owner/fleet/drains', '/api/on/remote/fleet/drains', '/api/on/owner/fleet/drains', '/api/on/remote/fleet/drains'],
      'the independent cadence reads each host once while the selected-workspace stream is connected');
    assert.deepEqual(errors, []);

    await page.screenshot({ path: path.join(evidence, 'fleet-indicator-live-stream-dark-1440.jpg'), type: 'jpeg', quality: 82, animations: 'disabled' });
    await page.setViewportSize({ width: 375, height: 812 });
    await page.screenshot({ path: path.join(evidence, 'fleet-indicator-live-stream-dark-375.jpg'), type: 'jpeg', quality: 82, animations: 'disabled' });
    const result = { before, afterPanelRefresh, after, connectedEventSource: true, errors };
    fs.writeFileSync(path.join(evidence, 'fleet-stream-cadence.json'), JSON.stringify(result, null, 2));
    return result;
  } finally {
    await page.close();
  }
}

// One refreshDashboard cycle. Host-resource reads stay out of the count: the
// dashboard also polls them on a separate 5s timer.
async function measurePoll(page) {
  const measured = await page.evaluate(async () => {
    const start = globalThis.fleetRequests.length;
    document.getElementById('refresh-btn').click();
    const deadline = performance.now() + 3000;
    let sawFetching = false;
    while (performance.now() < deadline) {
      const text = document.getElementById('meta-text')?.textContent || '';
      if (text.startsWith('fetching')) sawFetching = true;
      if (sawFetching && !text.startsWith('fetching')) break;
      await new Promise(resolve => setTimeout(resolve, 0));
    }
    return { sawFetching, requests: globalThis.fleetRequests.slice(start) };
  });
  assert.equal(measured.sawFetching, true, 'a poll cycle started');
  return measured.requests.filter(url => !url.includes('/host/resources'));
}

// Tokens per task must stay inside the ten-request refresh, and the failing-job
// rail and catalogue badge must follow the selected workspace across Metrics
// and Jobs. The arrival read is not part of the measured refresh.
async function assertJobFailureBadges(browser, port) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  fs.mkdirSync(path.join(evidence, 'screenshots'), { recursive: true });
  const rail = () => page.locator('#rail-count-jobs-failing');
  const go = async (route) => {
    await page.evaluate(async (next) => {
      const { setActiveTab } = await import('/js/router.js');
      const meta = document.getElementById('meta-text');
      setActiveTab(next);
      const deadline = performance.now() + 3000;
      let sawFetching = false;
      while (performance.now() < deadline) {
        const text = meta?.textContent || '';
        if (text.startsWith('fetching')) sawFetching = true;
        if (sawFetching && !text.startsWith('fetching')) return;
        await new Promise(resolve => setTimeout(resolve, 0));
      }
      throw new Error(`navigation did not settle on ${next}: ${meta?.textContent}`);
    }, route);
  };
  try {
    await page.goto(`http://127.0.0.1:${port}/?budget=1&workspace=idle#analytics/metrics`);
    await page.waitForFunction(() => globalThis.fleetReady);
    await settleDashboard(page);
    await page.waitForFunction(() => {
      const node = document.getElementById('rail-count-jobs-failing');
      return node && !node.hidden && node.textContent === '1 failing' && node.title.includes('fixture:');
    });
    await page.screenshot({ path: path.join(evidence, 'screenshots', 'metrics-job-failure-rail-dark-1280.png'), animations: 'disabled' });
    await page.getByRole('tab', { name: 'Tokens per task', exact: true }).click();
    const counted = await measurePoll(page);
    const paths = counted.map(value => new URL(value, 'http://fixture').pathname);
    assert.ok(counted.length > 0 && counted.length <= 10, `tokens poll issued ${counted.length}: ${counted.join(', ')}`);
    assert.ok(!paths.includes('/api/jobs/failure-streaks'), `tokens poll repeats failure streaks: ${counted.join(', ')}`);
    assert.ok(paths.includes('/api/diagnostics/task_tokens'), 'tokens poll reads the selected trend');
    for (const required of ['doctor', 'routines', 'audit/summary', 'job-runs', 'workflows/auto/readiness',
      'diagnostics/implement_one', 'diagnostics/completions', 'tasks/completion-by-complexity', 'diagnostics/defects']) {
      assert.ok(paths.includes(`/api/${required}`), `tokens poll retains ${required}: ${paths.join(', ')}`);
    }
    assert.equal(await rail().textContent(), '1 failing', 'the metrics poll keeps the idle failing-job count');

    await go('jobs/catalogue');
    await page.waitForFunction(() => {
      const badge = document.querySelector('#jobs-body .job-failure-badge');
      const node = document.getElementById('rail-count-jobs-failing');
      return badge && badge.textContent.includes('Failing: 4 in a row') && badge.textContent.includes('idle streak')
        && document.querySelector('#jobs-body .job-card')?.dataset.job === 'fixture'
        && node && !node.hidden && node.textContent === '1 failing' && node.title.includes('fixture:') && !node.title.includes('sweep:');
    });
    await page.locator('#jobs-body .job-failure-badge').scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(evidence, 'screenshots', 'jobs-catalogue-failure-dark-1280.png'), animations: 'disabled' });
    await page.setViewportSize({ width: 375, height: 900 });
    await page.locator('#jobs-body .job-failure-badge').scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(evidence, 'screenshots', 'jobs-catalogue-failure-dark-375.png'), animations: 'disabled' });
    await page.setViewportSize({ width: 1280, height: 900 });

    await page.locator('#workspace-select').selectOption('busy');
    await page.waitForFunction(() => {
      const badge = document.querySelector('#jobs-body .job-failure-badge');
      const node = document.getElementById('rail-count-jobs-failing');
      return badge && badge.textContent.includes('Failing: 6 in a row') && badge.textContent.includes('busy streak')
        && document.querySelector('#jobs-body .job-card')?.dataset.job === 'sweep'
        && node && !node.hidden && node.textContent === '1 failing' && node.title.includes('sweep:') && !node.title.includes('fixture:');
    });

    await go('analytics/metrics');
    const metricsPoll = await measurePoll(page);
    const metricsPaths = metricsPoll.map(value => new URL(value, 'http://fixture').pathname);
    assert.ok(metricsPoll.length > 0 && metricsPoll.length <= 10, `metrics poll after Jobs issued ${metricsPoll.length}: ${metricsPoll.join(', ')}`);
    assert.ok(!metricsPaths.includes('/api/jobs/failure-streaks'), `metrics poll after Jobs repeats streaks: ${metricsPoll.join(', ')}`);
    assert.equal(await rail().textContent(), '1 failing');
    assert.match(await rail().getAttribute('title'), /sweep:/);
    assert.doesNotMatch(await rail().getAttribute('title'), /fixture:/);

    await page.evaluate(() => { globalThis.failJobFailures = true; });
    await go('tasks');
    await go('analytics/metrics');
    await page.waitForFunction(() => document.getElementById('rail-count-jobs-failing')?.hidden === true);
    await page.evaluate(() => { globalThis.failJobFailures = false; });
    await go('jobs/catalogue');
    await go('analytics/metrics');
    await page.waitForFunction(() => {
      const node = document.getElementById('rail-count-jobs-failing');
      return node && !node.hidden && node.title.includes('sweep:') && !node.title.includes('fixture:');
    });
    assert.deepEqual(errors, []);
    const report = {
      tokensPoll: { count: counted.length, requests: counted },
      metricsPollAfterJobs: { count: metricsPoll.length, requests: metricsPoll },
    };
    fs.writeFileSync(path.join(evidence, 'job-failure-badges.json'), JSON.stringify(report, null, 2));
    return report;
  } finally {
    await page.close();
  }
}

async function assertPollBudget(browser, port) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await page.goto(`http://127.0.0.1:${port}/?budget=1&workspace=idle#tasks`);
    await page.waitForFunction(() => globalThis.fleetReady);
    await settleDashboard(page);
    await page.waitForFunction(() => document.querySelector('.global-drain-label')?.textContent.includes('1 of 3 workspaces draining'));
    await page.waitForFunction(() => document.querySelector('#auto-drain-body .drain-backlog-forecast-bars'));
    const backlogForecastReads = () => page.evaluate(() => globalThis.fleetRequests.filter(url =>
      new URL(url, location.href).pathname.endsWith('/diagnostics/backlog_forecast')).length);
    assert.ok(await backlogForecastReads() > 0, 'the Tasks dock loads its backlog forecast summary');
    const fleetReads = () => page.evaluate(() => globalThis.fleetRequests.filter(url => url.includes('/fleet/drains')).length);
    assert.equal(await fleetReads(), 2, 'two registered hosts are read once at startup; the first panel poll does not read the fleet');
    const polls = [];
    const recordPoll = async (label) => {
      const counted = await measurePoll(page);
      polls.push({ view: label, count: counted.length, requests: counted });
      // Keep the exact list even when the budget assertion fails.
      fs.writeFileSync(path.join(evidence, 'poll-requests.json'), JSON.stringify(polls, null, 2));
      const fleet = counted.filter(url => url.includes('/fleet/drains'));
      assert.deepEqual(fleet, [], `${label} poll reads fleet drains: ${counted.join(', ')}`);
      const forecasts = counted.filter(url => new URL(url, 'http://fixture').pathname.endsWith('/diagnostics/backlog_forecast'));
      assert.equal(forecasts.length, label === 'tasks' ? 1 : 0, `${label} poll reads the optional backlog forecast only while its card is shown`);
      assert.ok(counted.length > 0 && counted.length <= 10, `${label} poll issued ${counted.length} requests: ${counted.join(', ')}`);
      return counted.map(value => new URL(value, 'http://fixture').pathname);
    };
    for (const [label, route] of POLL_VIEWS) {
      const beforeFleet = await fleetReads();
      await page.evaluate(async (next) => {
        const { setActiveTab } = await import('/js/router.js');
        const meta = document.getElementById('meta-text');
        setActiveTab(next);
        const deadline = performance.now() + 3000;
        let sawFetching = false;
        while (performance.now() < deadline) {
          const text = meta?.textContent || '';
          if (text.startsWith('fetching')) sawFetching = true;
          if (sawFetching && !text.startsWith('fetching')) return;
          await new Promise(resolve => setTimeout(resolve, 0));
        }
        throw new Error(`navigation did not settle on ${next}: ${meta?.textContent}`);
      }, route);
      assert.equal(await fleetReads(), beforeFleet, `${label} navigation reads fleet drains`);
      await recordPoll(label);
      if (route === 'analytics/metrics') {
        for (const [trend, endpoint] of [
          ['Tasks completed', null], ['Defects', null],
          ['Tokens per task', 'task_tokens'], ['Lines changed', 'diff_stats'],
          ['Delivery friction', 'delivery_friction'], ['implement_one duration', null],
        ]) {
          // Use the shipped tab handler, then poll with that trend selected.
          // The basic fixture may report a trend unavailable; the request must
          // still occur and count, including reads for visible chrome.
          await page.getByRole('tab', { name: trend, exact: true }).click();
          const paths = await recordPoll(`${label}/${trend}`);
          for (const required of ['doctor', 'routines', 'audit/summary', 'job-runs', 'workflows/auto/readiness',
            'diagnostics/implement_one', 'diagnostics/completions', 'tasks/completion-by-complexity', 'diagnostics/defects']) {
            assert.ok(paths.includes(`/api/${required}`), `${trend} retains ${required}`);
          }
          assert.deepEqual(paths.filter(value => /\/(task_tokens|diff_stats|delivery_friction)$/.test(value)),
            endpoint ? [`/api/diagnostics/${endpoint}`] : [], `${trend} reads only its filtered trend`);
        }
      }
    }
    const beforeDrainForecast = await backlogForecastReads();
    await page.evaluate(async () => {
      const { setActiveTab } = await import('/js/router.js');
      const meta = document.getElementById('meta-text');
      setActiveTab('tasks?dock=drain');
      const deadline = performance.now() + 3000;
      let sawFetching = false;
      while (performance.now() < deadline) {
        const text = meta?.textContent || '';
        if (text.startsWith('fetching')) sawFetching = true;
        if (sawFetching && !text.startsWith('fetching')) return;
        await new Promise(resolve => setTimeout(resolve, 0));
      }
      throw new Error(`navigation did not settle on drain: ${meta?.textContent}`);
    });
    assert.ok(await backlogForecastReads() > beforeDrainForecast, 'the Drain route refreshes its backlog forecast');
    await page.waitForFunction(() => document.querySelector('#auto-drain-body .drain-backlog-forecast-bars'));
    assert.ok(await page.locator('#auto-drain-body .drain-backlog-forecast-bars').textContent(), 'the Tasks dock paints its backlog forecast summary');
    const beforeCard = await fleetReads();
    await page.setViewportSize({ width: 1440, height: 900 });
  await page.locator('#global-drain-state').click();
    await page.locator('.fleet-drains').waitFor({ state: 'visible' });
    assert.equal(await fleetReads(), beforeCard, 'opening the fleet card paints the snapshot without another host fan-out');
    const field = (row, name) => page.locator(`.fleet-drains .fleet-row[data-phase="${row}"] [data-field="${name}"]`).first().textContent();
    assert.equal(await field('draining', 'phase'), 'Phase draining');
    assert.equal(await field('draining', 'slots'), 'Slots 2/4');
    assert.equal(await field('idle', 'eligible'), 'Eligible 4 eligible');
    assert.equal(await field('idle', 'top-wait'), 'Top wait top wait: context lock conflict');
    assert.equal(await field('idle', 'holds'), 'Holds held: codex usage limit; claude authentication failed');
    assert.equal(await page.locator('.fleet-drains .fleet-row').count(), 3);
    assert.deepEqual(errors, []);
    return polls;
  } finally {
    await page.close();
  }
}

async function assertDrainForecastFreshness(browser, port) {
  const outcomes = [];
  for (const scenario of [
    { source: 'stored', expected: /^Stored \d+ h ago$/ },
    { source: 'live', expected: /^Live$/ },
    { source: 'unknown', expected: /^Forecast freshness unavailable$/ },
  ]) {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    try {
      await page.goto(`http://127.0.0.1:${port}/?workspace=idle&forecastSource=${scenario.source}#tasks?dock=drain`);
      await page.waitForFunction(() => globalThis.fleetReady);
      const link = page.locator('#auto-drain-body .drain-backlog-forecast');
      await link.waitFor({ state: 'visible' });
      const rendered = await link.evaluate(node => ({
        freshness: node.querySelector('.drain-backlog-forecast-freshness')?.textContent,
        freshnessVisible: Boolean(node.querySelector('.drain-backlog-forecast-freshness')?.getClientRects().length),
        title: node.title,
        accessibleText: node.textContent,
        figures: node.querySelector('.drain-backlog-forecast-bars')?.textContent,
      }));
      assert.ok(rendered.freshness, `${scenario.source} forecast exposes a freshness label`);
      assert.match(rendered.freshness, scenario.expected, `${scenario.source} forecast shows its freshness`);
      assert.equal(rendered.freshnessVisible, true, `${scenario.source} freshness is visible on the card`);
      assert.ok(rendered.title.includes(rendered.freshness), `${scenario.source} title labels the forecast details`);
      assert.ok(rendered.accessibleText.includes(rendered.freshness), `${scenario.source} accessible text labels the forecast details`);
      assert.ok(rendered.accessibleText.includes(rendered.figures), `${scenario.source} accessible text includes the labelled figures`);
      assert.ok(rendered.accessibleText.includes('3 backlog · 1 in progress'), `${scenario.source} accessible text includes the labelled counts`);
      assert.ok(await page.getByRole('link', { name: rendered.freshness, exact: false }).count() > 0,
        `${scenario.source} freshness is included in the link's accessible name`);
      assert.ok(rendered.title.includes('3 backlog · 1 in progress'), `${scenario.source} title includes counts`);
      assert.ok(rendered.title.includes('Time to drain'), `${scenario.source} title labels drain figures`);
      assert.ok(rendered.title.includes('Cost'), `${scenario.source} title labels cost figures`);
      assert.ok(rendered.figures.includes('1') && rendered.figures.includes('$'), `${scenario.source} visible figures render`);
      if (scenario.source === 'unknown') {
        assert.ok(!rendered.freshness.includes('Live'), 'missing source is not presented as live');
      }
      if (scenario.source === 'stored') {
        for (const width of [1440, 375]) {
          const capturePage = width === 1440 ? page : await browser.newPage({ viewport: { width, height: 900 } });
          try {
            if (capturePage !== page) {
              await capturePage.goto(`http://127.0.0.1:${port}/?workspace=idle&forecastSource=stored#tasks?dock=drain`);
              await capturePage.waitForFunction(() => globalThis.fleetReady);
            }
            const captureLink = capturePage.locator('#auto-drain-body .drain-backlog-forecast');
            if (width === 1440) {
              await captureLink.waitFor({ state: 'visible' });
              await captureLink.evaluate(node => node.scrollIntoView({ block: 'center' }));
            } else {
              await capturePage.waitForFunction(() => globalThis.fleetRequests.some(url =>
                new URL(url, location.href).pathname.endsWith('/diagnostics/backlog_forecast'))
                && document.querySelector('#auto-drain-body .drain-backlog-forecast'));
            }
            await capturePage.evaluate(() => document.fonts.ready);
            const screenshotName = width === 1440 ? 'drain-card-dark-1440.png' : 'tasks-dark-375.png';
            await capturePage.screenshot({ path: path.join(evidence, screenshotName), animations: 'disabled' });
          } finally {
            if (capturePage !== page) await capturePage.close();
          }
        }
      }
      outcomes.push({ source: scenario.source, freshness: rendered.freshness, passed: true });
    } finally {
      await page.close();
    }
  }
  fs.writeFileSync(path.join(evidence, 'drain-forecast-freshness.json'), JSON.stringify(outcomes, null, 2));
  return outcomes;
}

// Public page navigation, selection and reload exercise defaults through the
// shipped app, including a browser that refuses both storage reads and writes.
async function assertWorkspaceDefaults(browser, port) {
  const observations = [];
  for (const scenario of [
    { name: 'live drain before registry default', query: 'liveLocal=1', expected: 'busy' },
    { name: 'registry default without live drains', query: 'noLive=1', expected: 'idle' },
    { name: 'fleet failure retains registry fallback', query: 'fleetFailure=1', expected: 'idle' },
    { name: 'URL before remembered and live', query: 'liveLocal=1&workspace=idle', remembered: 'busy', expected: 'idle' },
    { name: 'aggregate URL before remembered', query: 'liveLocal=1&workspace=all', remembered: 'busy', expected: '' },
    { name: 'remembered before live', query: 'liveLocal=1', remembered: 'idle', expected: 'idle' },
    { name: 'stale remembered falls back to live', query: 'liveLocal=1', remembered: 'removed', expected: 'busy' },
    { name: 'inactive remembered workspace uses registry default', query: 'inactive=1&liveLocal=1', remembered: 'busy', expected: 'idle' },
    { name: 'stale URL falls back to remembered', query: 'workspace=removed', remembered: 'busy', expected: 'busy' },
    { name: 'remembered aggregate', query: 'liveLocal=1', remembered: 'all', expected: '' },
    { name: 'storage denied with live drain', query: 'liveLocal=1', denied: true, expected: 'busy' },
    { name: 'storage denied without live drain', query: 'noLive=1', denied: true, expected: 'idle' },
    { name: 'remote remembered workspace', query: 'host=remote&noLive=1', remoteRemembered: 'busy', expected: 'busy' },
    { name: 'local memory cannot select remote workspace', query: 'host=remote&noLive=1', remembered: 'busy', expected: 'idle' },
  ]) {
    const context = await browser.newContext();
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    try {
      await page.addInitScript(({ remembered, remoteRemembered, denied }) => {
        if (remembered) localStorage.setItem('orbit.dashboard.workspace.owner-id', remembered);
        if (remoteRemembered) localStorage.setItem('orbit.dashboard.workspace.remote-id', remoteRemembered);
        if (denied) {
          for (const method of ['getItem', 'setItem']) {
            Storage.prototype[method] = () => { throw new DOMException('Storage denied', 'SecurityError'); };
          }
        }
      }, scenario);
      await page.goto(`http://127.0.0.1:${port}/?${scenario.query}#tasks`);
      await page.waitForFunction(() => globalThis.fleetReady);
      assert.equal(await page.locator('#workspace-select').inputValue(), scenario.expected, scenario.name);
      assert.equal(new URL(page.url()).searchParams.get('workspace'), scenario.expected || 'all', scenario.name);
      // Explicit operator changes must still work when storage writes fail.
      const chosen = scenario.query.includes('inactive=1') ? 'idle' : 'busy';
      await page.locator('#workspace-select').selectOption(chosen);
      await page.reload();
      await page.waitForFunction(() => globalThis.fleetReady);
      assert.equal(await page.locator('#workspace-select').inputValue(), chosen, `${scenario.name}: reload keeps URL scope`);
      assert.deepEqual(errors, [], scenario.name);
      observations.push({ ...scenario, passed: true });
    } finally {
      await context.close();
    }
  }
  // A fresh bare URL must restore choices made through the picker and fleet
  // row (not merely inherit the scope left in the URL by a reload).
  const context = await browser.newContext();
  const page = await context.newPage();
  try {
    const bare = `http://127.0.0.1:${port}/?noLive=1#tasks`;
    await page.goto(bare);
    await page.waitForFunction(() => globalThis.fleetReady);
    await page.locator('#workspace-select').selectOption('busy');
    await page.goto(bare);
    await page.waitForFunction(() => globalThis.fleetReady);
    // ORB-17652: both rail pickers expose a programmatic name from
    // `aria-label`, not only a `title` fallback or a label hidden on
    // phones, and the two are named by the same mechanism.
    const workspacePicker = page.getByRole('combobox', { name: 'Workspace', exact: true });
    assert.equal(await workspacePicker.getAttribute('id'), 'workspace-select', 'workspace picker is named Workspace in the accessibility tree');
    assert.equal(await workspacePicker.getAttribute('aria-label'), 'Workspace', 'workspace picker name comes from aria-label, not only its title');
    const hostPicker = page.getByRole('combobox', { name: 'Host', exact: true });
    assert.equal(await hostPicker.getAttribute('id'), 'host-select', 'host picker is named Host in the accessibility tree');
    assert.equal(await hostPicker.getAttribute('aria-label'), 'Host', 'host picker name comes from aria-label, not only its visible label');
    assert.equal(await page.locator('#workspace-select').inputValue(), 'busy', 'picker choice restored on a bare URL');
    await page.locator('#workspace-select').selectOption('');
    await page.goto(bare);
    await page.waitForFunction(() => globalThis.fleetReady);
    assert.equal(await page.locator('#workspace-select').inputValue(), '', 'aggregate picker choice restored');
    await page.setViewportSize({ width: 1440, height: 900 });
  await page.locator('#global-drain-state').click();
    await page.getByRole('button', { name: 'Show Idle workspace with ready work on owner', exact: true }).click();
    await page.goto(bare);
    await page.waitForFunction(() => globalThis.fleetReady);
    assert.equal(await page.locator('#workspace-select').inputValue(), 'idle', 'fleet row choice restored');
    observations.push({ name: 'picker, aggregate and fleet row choices restored on bare URLs', passed: true });
  } finally {
    await context.close();
  }
  fs.writeFileSync(path.join(evidence, 'workspace-defaults.json'), JSON.stringify(observations, null, 2));
  return observations;
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  const served = url.pathname === '/fixture.mjs' ? { data: initializer, type: 'text/javascript' } : dashboardFile(url.pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  const data = url.pathname === '/' ? served.data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g,
    '<script type="module" src="/fixture.mjs"></script>') : served.data;
  res.setHeader('content-type', served.type); res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const drainForecastFreshness = await assertDrainForecastFreshness(browser, server.address().port);
  const workspaceDefaults = await assertWorkspaceDefaults(browser, server.address().port);
  const warmup = await assertIndependentWarmup(browser, server.address().port);
  const fleetPartial = await assertFleetPartial(browser, server.address().port);
  const fleetStreamCadence = await assertFleetCadenceWithConnectedStream(browser, server.address().port);
  const fleetPartialWidths = [...new Set(fleetPartial.map(({ width }) => width))].join(' and ');
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=idle#auto-drain`);
  await page.waitForFunction(() => globalThis.fleetReady);
  await page.waitForFunction(() => document.querySelector('.global-drain-label')?.textContent.includes('1 of 3 workspaces draining'));
  const startupFleet = await page.evaluate(() => globalThis.fleetRequests.filter(url => url.includes('/fleet/drains')));
  assert.equal(startupFleet.length, 3, 'startup reads each registered host once; the first panel poll does not read the fleet again');
  assert.equal(await page.locator('#global-drain-state').isVisible(), true, 'an idle default workspace still shows a live fleet drain');
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await page.screenshot({ path: path.join(evidence, `header-after-dark-${width}.png`), animations: 'disabled' });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.locator('#global-drain-state').click();
  await page.locator('.fleet-drains').waitFor({ state: 'visible' });
  assert.equal(new URL(page.url()).searchParams.get('workspace'), 'all', 'header chip opens All workspaces');
  assert.equal(await page.locator('.fleet-drains .fleet-row').count(), 4);
  assert.equal(await page.locator('.fleet-unavailable [data-field="error"]').textContent(), 'Error Host unreachable');
  assert.equal(await page.locator('#auto-drain-quick-start').isVisible(), false, 'fleet inspection exposes no workspace write controls');
  const measurements = [];
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await page.evaluate(() => document.fonts.ready);
    const layout = await page.evaluate(() => {
      const line = row => row.querySelector('.fleet-line').getBoundingClientRect().height;
      const rows = [...document.querySelectorAll('.fleet-drains .fleet-row:not(.fleet-unavailable)')].filter(row => row.offsetParent);
      return { viewport: innerWidth, pageWidth: document.documentElement.scrollWidth,
        cardWidth: document.querySelector('.fleet-drains').getBoundingClientRect().width,
        bodyWidth: document.querySelector('#auto-drain-body').getBoundingClientRect().width,
        rowHeights: rows.map(row => [row.getBoundingClientRect().height, line(row), Boolean(row.querySelector('.fleet-detail'))]) };
    });
    assert.ok(layout.pageWidth <= layout.viewport, JSON.stringify(layout));
    assert.ok(layout.cardWidth <= layout.bodyWidth, JSON.stringify(layout));
    // A long unbroken workspace name truncates rather than wrapping its row.
    for (const [height, line, detail] of layout.rowHeights) {
      assert.ok(line <= 24, `each workspace keeps its name, phase and slots on one line: ${JSON.stringify(layout)}`);
      if (!detail) assert.ok(height <= line + 10, `a row with nothing waiting is one line: ${JSON.stringify(layout)}`);
    }
    if (width >= 1000) await page.locator('.fleet-drains .fleet-row').first().scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(evidence, 'fleet-' + width + '.png'), fullPage: true });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  // Select a row on another host through its native keyboard control, after
  // choosing that host's tab.
  await page.getByRole('tab', { name: /^remote/ }).click();
  const remote = page.getByRole('button', { name: /Show BusyWorkspace.* on remote/ });
  await remote.focus();
  await remote.press('Enter');
  await page.waitForFunction(async () => {
    const common = await import('/js/common.js');
    return common.getHost() === 'remote' && common.getWorkspace() === 'busy';
  });
  await page.waitForFunction(() => document.querySelector('#workspace-select')?.value === 'busy');
  const url = new URL(page.url());
  assert.equal(url.searchParams.get('host'), 'remote');
  assert.equal(url.searchParams.get('workspace'), 'busy');
  await page.waitForFunction(() => globalThis.fleetRequests.some(url => url.startsWith('/api/on/remote/workflows/auto/readiness?workspace=busy')));
  // Return through the pill, then select a serving-host row by clicking its
  // slots: the whole row, not only the workspace button, is navigable.
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.locator('#global-drain-state').click();
  await page.locator('.fleet-drains').waitFor({ state: 'visible' });
  assert.equal(await page.locator('.fleet-host-tab[aria-selected="true"]').getAttribute('data-host'), 'remote', 'the chosen host tab is remembered');
  await page.locator('.fleet-host-tab').first().click();
  await page.locator('.fleet-drains .fleet-row').first().locator('[data-field="slots"]').click();
  await page.waitForFunction(async () => {
    const common = await import('/js/common.js');
    return common.getHost() === null && common.getWorkspace() === 'idle';
  });
  await page.waitForFunction(() => document.querySelector('#workspace-select')?.value === 'idle');
  assert.deepEqual(errors, []);
  const polls = await assertPollBudget(browser, server.address().port);
  const jobFailureBadges = await assertJobFailureBadges(browser, server.address().port);
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ status: 'passed', measurements, polls, jobFailureBadges, warmup, workspaceDefaults, fleetPartial, fleetStreamCadence, drainForecastFreshness,
    guarantees: ['initial Tasks and Run detail render during held remote fleet warmup', 'warmup and first refresh share in-flight fleet reads', 'late fleet completion updates the indicator and preserves the selected view', `ready fleet hosts shown while another host is loading at ${fleetPartialWidths}px`, 'pending host distinct from idle and unavailable and settles without dropping ready rows or changing selection', `ready workspace focus preserved and Enter selects it when another host settles (success and failure, ${fleetPartialWidths}px)`, 'connected selected-workspace stream keeps an independent one-read-per-host fleet cadence for remote drain changes', 'idle workspace fleet indicator', 'host failures retained', '375px no horizontal page scroll', 'one line per workspace with nothing waiting', 'host tabs show one host at a time and remember the choice', 'keyboard remote scope selection', 'row click local scope selection', 'two-host poll stays within 10 requests', 'panel poll does not read fleet drains', 'tokens poll omits a repeated failure-streak read', 'failing-job badges stay scoped across Metrics and Jobs'], errors }, null, 2));
  console.log('fleet browser startup, scope, mobile, and poll-budget checks passed');
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
