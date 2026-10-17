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
  const workspaces = [
    { id: 'idle', name: 'Idle workspace with ready work', status: 'active', is_default: true },
    { id: 'busy', name: 'BusyWorkspaceWithAVeryLongUnbrokenNameForMobileLayout', status: 'active' },
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
      if (host.startsWith('Unreachable')) return json({ error: 'Host unreachable' }, 502);
      return json({ items: host === 'remote' ? [row('busy', 'draining', 0)] : [row('idle', 'idle', 4), row('busy', 'idle', 0)] });
    }
    if (api === '/api/workspaces') return json(workspaces);
    if (api === '/api/workflows/auto/readiness') return json({ controls_authorized: false, capacity: { drain_phase: 'idle' }, tasks: [] });
    if (api === '/api/routines') return json({ routines: [], clock: {} });
    if (api === '/api/tasks/locks') return json({ by_task: [], total_locked: 0, total_tasks: 0 });
    if (api === '/api/crews') return json({ crews: [] });
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
    await page.waitForFunction(() => document.querySelector('.global-drain-label')?.textContent.includes('Draining · 1 of 3'));
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
    const card = await page.locator('.fleet-drains').innerText();
    assert.match(card, /draining/i);
    assert.match(card, /2 \/ 4/);
    assert.match(card, /4/);
    assert.match(card, /context lock conflict/);
    assert.match(card, /codex usage limit/);
    assert.match(card, /claude authentication failed/);
    assert.equal(await page.locator('.fleet-drains tbody tr').count(), 3);
    assert.deepEqual(errors, []);
    return polls;
  } finally {
    await page.close();
  }
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
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=idle#auto-drain`);
  await page.waitForFunction(() => globalThis.fleetReady);
  await page.waitForFunction(() => document.querySelector('.global-drain-label')?.textContent.includes('Draining · 1 of 3'));
  const startupFleet = await page.evaluate(() => globalThis.fleetRequests.filter(url => url.includes('/fleet/drains')));
  assert.equal(startupFleet.length, 3, 'startup reads each registered host once; the first panel poll does not read the fleet again');
  assert.equal(await page.locator('#global-drain-state').isVisible(), true, 'an idle default workspace still shows a live fleet drain');
  await page.locator('#global-drain-state').click();
  await page.locator('.fleet-drains').waitFor({ state: 'visible' });
  assert.equal(await page.locator('.fleet-drains tbody tr').count(), 4);
  assert.match(await page.locator('.fleet-drains').textContent(), /Host unreachable/);
  assert.equal(await page.locator('#auto-drain-start').isVisible(), false, 'fleet inspection exposes no workspace write controls');
  const measurements = [];
  for (const width of [1280, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await page.evaluate(() => document.fonts.ready);
    const layout = await page.evaluate(() => ({ viewport: innerWidth, pageWidth: document.documentElement.scrollWidth,
      tableWidth: document.querySelector('.fleet-drains').getBoundingClientRect().width,
      bodyWidth: document.querySelector('#auto-drain-body').getBoundingClientRect().width }));
    assert.ok(layout.pageWidth <= layout.viewport, JSON.stringify(layout));
    assert.ok(layout.tableWidth <= layout.bodyWidth, JSON.stringify(layout));
    assert.equal(await page.locator('.fleet-drains tbody td').first().evaluate(cell =>
      getComputedStyle(cell, '::before').content === JSON.stringify(cell.dataset.label) && Boolean(cell.dataset.label)), true,
      'stacked cells visibly label their fields');
    measurements.push(layout);
    await page.locator('.fleet-drains tbody tr').first().scrollIntoViewIfNeeded();
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
  // phase cell: the whole row, not only the workspace button, is navigable.
  await page.locator('#global-drain-state').click();
  await page.locator('.fleet-drains').waitFor({ state: 'visible' });
  await page.locator('.fleet-drains tbody tr').first().locator('td').nth(2).click();
  await page.waitForFunction(async () => {
    const common = await import('/js/common.js');
    return common.getHost() === null && common.getWorkspace() === 'idle';
  });
  await page.waitForFunction(() => document.querySelector('#workspace-select')?.value === 'idle');
  assert.deepEqual(errors, []);
  const polls = await assertPollBudget(browser, server.address().port);
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ status: 'passed', measurements, polls,
    guarantees: ['idle workspace fleet indicator', 'host failures retained', '375px no horizontal page scroll', 'keyboard remote scope selection', 'row click local scope selection', 'two-host poll stays within 10 requests', 'panel poll does not read fleet drains'], errors }, null, 2));
  console.log('fleet browser scope, mobile, and poll-budget checks passed');
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
