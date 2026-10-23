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
  globalThis.EventSource = class { close() {} };
  globalThis.fleetRequests = [];
  const budget = new URLSearchParams(location.search).get('budget') === '1';
  const liveLocal = new URLSearchParams(location.search).has('liveLocal');
  const noLive = new URLSearchParams(location.search).has('noLive');
  const fleetFailure = new URLSearchParams(location.search).has('fleetFailure');
  const holdFleet = new URLSearchParams(location.search).get('holdFleet') === '1';
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
      }
      return json({ items: host === 'remote' ? [row('busy', noLive ? 'idle' : 'draining', 0)] : [row('idle', 'idle', 4), row('busy', liveLocal && !noLive ? 'pulling' : 'idle', 0)] });
    }
    if (api === '/api/workspaces') return json(workspaces);
    if (api === '/api/workflows/auto/readiness') return json({ controls_authorized: false, capacity: { drain_phase: 'idle' }, tasks: [] });
    if (api === '/api/routines') return json({ routines: [], clock: {} });
    if (api === '/api/tasks/locks') return json({ by_task: [], total_locked: 0, total_tasks: 0 });
    if (api === '/api/crews') return json({ crews: [] });
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

async function assertPollBudget(browser, port) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await page.goto(`http://127.0.0.1:${port}/?budget=1&workspace=idle#tasks`);
    await page.waitForFunction(() => globalThis.fleetReady);
    await settleDashboard(page);
    await page.waitForFunction(() => document.querySelector('.global-drain-label')?.textContent.includes('1 of 3 workspaces draining'));
    const fleetReads = () => page.evaluate(() => globalThis.fleetRequests.filter(url => url.includes('/fleet/drains')).length);
    assert.equal(await fleetReads(), 2, 'two registered hosts are read once at startup; the first panel poll does not read the fleet');
    const polls = [];
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
      const counted = await measurePoll(page);
      const fleet = counted.filter(url => url.includes('/fleet/drains'));
      assert.deepEqual(fleet, [], `${label} poll reads fleet drains: ${counted.join(', ')}`);
      assert.ok(counted.length > 0 && counted.length <= 10, `${label} poll issued ${counted.length} requests: ${counted.join(', ')}`);
      polls.push({ view: label, count: counted.length, requests: counted });
    }
    const beforeCard = await fleetReads();
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
    assert.equal(await page.locator('#workspace-select').inputValue(), 'busy', 'picker choice restored on a bare URL');
    await page.locator('#workspace-select').selectOption('');
    await page.goto(bare);
    await page.waitForFunction(() => globalThis.fleetReady);
    assert.equal(await page.locator('#workspace-select').inputValue(), '', 'aggregate picker choice restored');
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
  const workspaceDefaults = await assertWorkspaceDefaults(browser, server.address().port);
  const warmup = await assertIndependentWarmup(browser, server.address().port);
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
  await page.locator('#global-drain-state').click();
  await page.locator('.fleet-drains').waitFor({ state: 'visible' });
  assert.equal(new URL(page.url()).searchParams.get('workspace'), 'all', 'header chip opens All workspaces');
  assert.equal(await page.locator('.fleet-drains .fleet-row').count(), 4);
  assert.equal(await page.locator('.fleet-unavailable [data-field="error"]').textContent(), 'Error Host unreachable');
  assert.equal(await page.locator('#auto-drain-start').isVisible(), false, 'fleet inspection exposes no workspace write controls');
  const measurements = [];
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await page.evaluate(() => document.fonts.ready);
    const layout = await page.evaluate(() => {
      const line = row => row.querySelector('.fleet-line').getBoundingClientRect().height;
      const rows = [...document.querySelectorAll('.fleet-drains .fleet-row:not(.fleet-unavailable)')];
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
    await page.locator('.fleet-drains .fleet-row').first().scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(evidence, 'fleet-' + width + '.png'), fullPage: true });
  }
  // Select a row on another host through its native keyboard control.
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
  await page.locator('#global-drain-state').click();
  await page.locator('.fleet-drains').waitFor({ state: 'visible' });
  await page.locator('.fleet-drains .fleet-row').first().locator('[data-field="slots"]').click();
  await page.waitForFunction(async () => {
    const common = await import('/js/common.js');
    return common.getHost() === null && common.getWorkspace() === 'idle';
  });
  await page.waitForFunction(() => document.querySelector('#workspace-select')?.value === 'idle');
  assert.deepEqual(errors, []);
  const polls = await assertPollBudget(browser, server.address().port);
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ status: 'passed', measurements, polls, warmup, workspaceDefaults,
    guarantees: ['initial Tasks and Run detail render during held remote fleet warmup', 'warmup and first refresh share in-flight fleet reads', 'late fleet completion updates the indicator and preserves the selected view', 'idle workspace fleet indicator', 'host failures retained', '375px no horizontal page scroll', 'one line per workspace with nothing waiting', 'keyboard remote scope selection', 'row click local scope selection', 'two-host poll stays within 10 requests', 'panel poll does not read fleet drains'], errors }, null, 2));
  console.log('fleet browser startup, scope, mobile, and poll-budget checks passed');
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
