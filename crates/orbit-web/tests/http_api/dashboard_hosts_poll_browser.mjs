// Full shipped Settings Hosts refresh with ten workspaces. No real host probes
// or stores: the fixture API records every request, including topbar reads.
// Usage: node dashboard_hosts_poll_browser.mjs <playwright> <evidence> [--baseline]
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';
const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
const baseline = process.argv.includes('--baseline');
fs.mkdirSync(evidence, { recursive: true });
const server = http.createServer((req, res) => {
  const pathname = new URL(req.url, 'http://fixture').pathname;
  const asset = dashboardFile(pathname);
  if (!asset) { res.writeHead(404).end(); return; }
  res.setHeader('content-type', asset.type);
  const oldFile = pathname.match(/^\/(?:static\/)?js\/(hosts|host-resources)\.js$/);
  res.end(baseline && oldFile ? execFileSync('git', ['show', `HEAD:crates/orbit-web/assets/dashboard/js/${oldFile[1]}.js`]) : asset.data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.clock.install();
  await page.addInitScript(() => {
    globalThis.EventSource = class { close() {} };
    globalThis.requests = [];
    globalThis.broken = false;
    const workspaces = Array.from({ length: 10 }, (_, n) => ({ id: `ws_${n}`, name: `Workspace ${n}`, status: 'active', is_default: n === 0, role: 'owner' }));
    const capacity = n => ({ drain_phase: n === 1 ? 'draining' : n === 2 ? 'winding_down' : 'idle',
      drain_run_id: n === 1 ? 'drain-fixture' : null,
      pull_drain_run_id: n === 1 || n === 2 ? 'pull-fixture' : null, pull_drain_admissions_stopped: n === 2 });
    globalThis.fetch = async input => {
      const url = new URL(input, location.href);
      globalThis.requests.push(url.pathname + url.search);
      const api = url.pathname.replace(/^\/api\/on\/[^/]+/, '/api');
      const json = (body, status = 200) => new Response(JSON.stringify(body), { status });
      if (api === '/api/hosts') return json({ hosts: [
        { name: 'local', machine_id: 'local-id', local: true, reachable: true, workspaces },
        { name: 'unprobed', machine_id: 'remote-id', ssh: 'remote', reachable: null, workspaces: [] },
      ], host_edit: { authorized: false, reason: 'Fixture read-only session' } });
      if (api === '/api/workspaces') return json(workspaces);
      if (api.endsWith('/connection')) return json({ reachable: true, forward_writes: { authorized: false } });
      if (api === '/api/host/resources') return json({
        cpu: { percent: 120, severity: 'elevated' }, memory: { percent: null, unknown_reason: 'sample missing' },
        disk: { percent: 35, severity: 'ok' }, sample_age_seconds: 1, max_age_seconds: 30,
        throttle: false, stale: false, thresholds: { enabled: true }, reason: 'open',
      });
      if (api === '/api/fleet/drains' && url.pathname.includes('/unprobed/')) return json({ error: 'Host not probed' }, 502);
      if (api === '/api/fleet/drains') return json({ items: workspaces.filter((_, n) => !(globalThis.broken && n === 8)).map(w => {
        const n = Number(w.id.replace('ws_', ''));
        if (globalThis.broken && n === 9) return { workspace_id: w.id, error: 'readiness unavailable', phase: 'unavailable' };
        const c = capacity(n);
        return { workspace_id: w.id, workspace_name: w.name, phase: c.drain_phase, drain_phase: c.drain_phase,
          pull_active: Boolean(c.pull_drain_run_id), pull_admissions_stopped: c.pull_drain_admissions_stopped,
          slots_used: 0, slots_ceiling: 4, eligible_count: 0, holds: [] };
      }) });
      if (api === '/api/workflows/auto/readiness') return json({ controls_authorized: false, capacity: capacity(Number(url.searchParams.get('workspace')?.replace('ws_', '') || 0)), tasks: [] });
      if (api === '/api/routines') return json({ routines: [], clock: {} });
      return json({ items: [], total: 0 });
    };
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_0#config/hosts`);
  await page.waitForFunction(() => document.querySelector('.host-drain')?.title.includes('Workspace 9:'));
  await page.waitForFunction(() => document.getElementById('meta-text')?.textContent.startsWith('refreshed'));
  const measure = async () => page.evaluate(async () => {
    requests.length = 0;
    document.getElementById('refresh-btn').click();
    // A macrotask lets the full refresh settle, including resource reads that
    // are intentionally independent of the dashboard's completion promise.
    await new Promise(resolve => { const id = setInterval(() => {
      if (document.getElementById('meta-text').textContent.startsWith('refreshed')) { clearInterval(id); resolve(); }
    }, 0); });
    return { requests: [...requests], drain: document.querySelector('.host-drain').title,
      workspaces: document.querySelectorAll('.host-row.local .host-workspace').length };
  });
  // The clock remains paused: run only the refresh's short settlement timer.
  const pending = measure();
  await page.clock.runFor(20);
  const measured = await pending;
  assert.equal(measured.workspaces, 10);
  assert.match(measured.drain, /Workspace 1: Draining, pull drain/);
  assert.match(measured.drain, /Workspace 2: Winding down, pull drain/);
  assert.ok(!measured.requests.some(url => url === '/api/hosts'), 'settled refresh never probes');
  if (!baseline) {
    assert.ok(measured.requests.length <= 10, JSON.stringify(measured.requests));
    assert.equal(measured.requests.filter(url => url.includes('/fleet/drains')).length, 1);
    assert.equal(measured.requests.filter(url => url.includes('/host/resources')).length, 1, 'Hosts shares the fresh topbar resource read');
    assert.ok(!measured.requests.some(url => url.includes('/workflows/auto/readiness') && url.includes('workspace=ws_9')));
    // Healthy resource readings survive another resource's unknown state.
    assert.match(await page.locator('.host-row.local .host-health-readings').innerText(), /1\.2×/);
    assert.equal(await page.locator('.host-row.local [data-resource="memory"] .v').innerText(), '-');
  }
  await page.clock.runFor(400);
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.screenshot({ path: path.join(evidence, `hosts-${baseline ? 'before' : 'after'}-dark-${width}.png`) });
  }
  // Capture the actual visible one-shot 30s poll, including a 5s resource
  // timer if it falls in the same window. Virtual time avoids timing races.
  await page.clock.runFor(29_000);
  await page.evaluate(() => { requests.length = 0; });
  await page.clock.runFor(1000);
  await page.waitForFunction(() => document.getElementById('meta-text')?.textContent.startsWith('refreshed'));
  const scheduledRequests = await page.evaluate(() => [...requests]);
  assert.ok(scheduledRequests.some(url => url.includes('/hosts?probe=false')), 'the visible 30s timer actually refreshed Hosts');
  if (!baseline) assert.ok(scheduledRequests.length <= 10, JSON.stringify(scheduledRequests));
  if (!baseline) {
    await page.evaluate(() => { globalThis.broken = true; });
    const failure = measure();
    await page.clock.runFor(20);
    const failed = await failure;
    assert.match(failed.drain, /Workspace 8: drain unavailable/);
    assert.match(failed.drain, /Workspace 9: readiness unavailable/);
    assert.match(failed.drain, /Workspace 1: Draining, pull drain/);
  }
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidence, 'requests.json'), JSON.stringify({ baseline, ...measured, scheduledRequests, errors,
    scenario: 'Ten local workspaces and one unprobed remote; settled full dashboard refresh uses the same refreshDashboard path as the visible 30-second poll; fixture APIs, shipped assets' }, null, 2));
  console.log(JSON.stringify(measured));
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
