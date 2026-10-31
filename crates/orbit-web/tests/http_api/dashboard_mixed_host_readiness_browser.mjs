// Usage: node <script> "$PLAYWRIGHT_MODULE" .orbit/tmp/mixed-browser .orbit/tmp/mixed-host-readiness.json [--before]
// The payload comes from the public core readiness regression, not a second
// implementation of readiness. Drive the shipped app's SSE invalidation path.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile, dashboardCsp } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
const source = JSON.parse(fs.readFileSync(process.argv[4], 'utf8'));
const before = process.argv.includes('--before');
fs.mkdirSync(evidence, { recursive: true });
const task = { ...source.task, title: 'Run macOS validation after the shared files are free',
  context_files: source.locked.conflicts.map(row => row.requested_file),
  readiness: { ready: true, gaps: [] }, status_transitions: [] };
const holders = source.holders.map((row, index) => ({ ...row, status: 'in-progress',
  title: index ? 'Update shared runtime checks' : 'Repair workspace admission', status_transitions: [] }));
const tasks = [task, ...holders];
let entry = source.locked;
const legacy = { task_id: task.id, status: 'backlog', eligible: false,
  reason: 'host_os_mismatch', detail: 'waits for a macos host (os:macos)', host_os: 'linux',
  host_fleet: 'registered macos host(s): hm_mac pulling (claimed here within 7 days)' };
const streams = new Set();
const list = items => ({ items, total: items.length, limit: 100, truncated: false });
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (url.pathname === '/api/events/stream') {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    res.write(`id: 1\ndata: ${JSON.stringify({ kind: 'drain', phase: 'idle' })}\n\n`);
    streams.add(res);
    req.on('close', () => streams.delete(res));
    return;
  }
  if (url.pathname.startsWith('/api/')) {
    res.setHeader('content-type', 'application/json');
    let payload = list([]);
    if (url.pathname === '/api/workspaces') payload = [{ id: 'ws_fixture', name: 'Mixed-host workspace', status: 'active', is_default: true }];
    if (url.pathname === '/api/hosts') payload = { generation: 1, hosts: [{ name: 'linux-owner', machine_id: 'hm_linux', local: true, reachable: true }], forward_writes: { authorized: false } };
    if (url.pathname === '/api/crews') payload = { crews: [{ name: 'sol' }], default_crew: 'sol' };
    if (url.pathname === '/api/tasks') payload = list(tasks);
    const selected = tasks.find(row => url.pathname === `/api/tasks/${row.id}`);
    if (selected) payload = selected;
    if (url.pathname === '/api/tasks/locks') payload = { by_task: holders, by_reservation: [], total_locked: 2, total_tasks: 2, total_reservations: 0, locked_files: task.context_files };
    if (url.pathname === '/api/workflows/auto/readiness') payload = { tasks: [before ? legacy : entry], capacity: {} };
    res.end(JSON.stringify(payload));
    return;
  }
  const served = dashboardFile(url.pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  res.setHeader('content-type', served.type);
  res.setHeader('content-security-policy', dashboardCsp());
  res.end(served.data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
const results = [];
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  for (const width of [1440, 375]) {
    entry = source.locked;
    const page = await browser.newPage({ viewport: { width, height: 1000 }, colorScheme: 'dark' });
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture#tasks?status=backlog&open=${task.id}`);
    const detail = page.locator(`#detail-${task.id} .readiness-drain`);
    await detail.waitFor();
    const row = page.locator(`[data-key="task-${task.id}"] .row-reason`);
    if (before) {
      await page.waitForFunction(id => document.querySelector(`#detail-${id} .readiness-drain`)?.textContent.includes('needs macos host'), task.id);
    } else {
      const expectPool = async pool => {
        await page.waitForFunction(({ id, pool }) => document.querySelector(`#detail-${id} .readiness-drain`)?.dataset.pool === pool
          && document.querySelector(`[data-key="task-${id}"] .ready-dot`)?.dataset.pool === pool, { id: task.id, pool });
      };
      await expectPool('locks');
      for (const holder of holders) assert((await detail.textContent()).includes(holder.id), 'detail names every real holder');
      for (const selector of task.context_files) assert((await detail.textContent()).includes(selector), 'detail names every overlap');
      for (const run of source.locked.run_ids) assert((await detail.textContent()).includes(run), 'detail names the live holder run');
      assert((await detail.textContent()).includes('On this linux host'), 'local mismatch is scoped');
      assert(!(await row.textContent()).includes('Needs macOS host'), 'row reports the lock wait');
      const update = async (state, kind) => {
        entry = source[state];
        for (const stream of streams) stream.write(`id: ${Date.now()}\ndata: ${JSON.stringify({ kind, phase: 'idle' })}\n\n`);
        await expectPool(state === 'locked' ? 'locks' : 'other');
        await page.waitForFunction(({ id, state }) => {
          const text = document.querySelector(`#detail-${id} .readiness-drain`)?.textContent || '';
          return state === 'locked' ? text.includes('Held by') : state === 'free' ? text.includes('waiting for follower admission')
            : state === 'unavailable' ? text.includes('executor evidence unavailable') : text.includes('no recent compatible executor');
        }, { id: task.id, state });
      };
      for (const [state, kind] of [['stale', 'drain'], ['locked', 'drain'], ['ineligible', 'drain'], ['locked', 'drain'], ['free', 'locks'], ['unavailable', 'drain'], ['locked', 'drain']]) {
        await update(state, kind);
      }
      assert.equal(errors.length, 0, errors.join('\n'));
      results.push({ width, holders: holders.map(row => row.id), selectors: task.context_files,
        transitions: 'stale, restart, ineligible, restart, lock release, unavailable, restart via SSE; no manual refresh', passed: true });
    }
    await detail.scrollIntoViewIfNeeded();
    const bounds = await detail.evaluate(node => ({ width: node.clientWidth, scroll: node.scrollWidth }));
    assert(bounds.scroll <= bounds.width + 1, `readiness card wraps at ${width}px: ${JSON.stringify(bounds)}`);
    await page.screenshot({ path: path.join(evidence, `mixed-host-detail-dark-${width}${before ? '-before' : ''}.png`), animations: 'disabled' });
    // The mobile detail is a modal sheet; close it to capture the actual row.
    if (width === 375) await page.keyboard.press('Escape');
    await row.scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(evidence, `mixed-host-row-dark-${width}${before ? '-before' : ''}.png`), animations: 'disabled' });
    await page.close();
  }
  fs.writeFileSync(path.join(evidence, 'results.json'), JSON.stringify({ before, source: 'public OrbitRuntime::workspace_auto_readiness regression', results }, null, 2));
} finally {
  if (browser) await browser.close();
  for (const stream of streams) stream.end();
  await new Promise(resolve => server.close(resolve));
}
