// The intercepted real-module fixture from the performance review, extended
// with assertions, optional trends and repeatable sequential measurements.
// Usage: node <script> <playwright module> <evidence dir> [--base=<git ref>]
// [--measure] [--screenshots]. All fetches are in memory; no API service runs.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const output = path.resolve(process.argv[3]);
const base = process.argv.find(arg => arg.startsWith('--base='))?.slice(7);
const measure = process.argv.includes('--measure');
const screenshots = process.argv.includes('--screenshots');
fs.mkdirSync(output, { recursive: true });
const routes = ['tasks', 'jobs/catalogue', 'runs/fixture', 'knowledge/frictions', 'analytics/metrics'];
const trends = ['duration', 'completed', 'defects', 'tokens', 'tokens-total', 'turns', 'diff', 'friction'];
const samples = [];
const errors = [];
const browser = await chromium.launch({ headless: true });

async function fixturePage(route, width = 1440) {
  const page = await browser.newPage({ viewport: { width, height: 900 } });
  page.on('pageerror', error => errors.push({ route, message: error.message }));
  await page.route('**/*', async request => {
    const url = new URL(request.request().url());
    assert.equal(url.host, 'perf.invalid', 'fixture must intercept every network request');
    const asset = dashboardFile(url.pathname);
    if (!asset) return request.fulfill({ status: 404, body: '' });
    let body = asset.data;
    if (base && url.pathname.startsWith('/static/') && url.pathname.endsWith('.js')) {
      body = execFileSync('git', ['show', `${base}:crates/orbit-web/assets/dashboard/${url.pathname.slice(8)}`]);
    }
    if (url.pathname.endsWith('/app.js')) body = body.toString() + '\nglobalThis.__perfRefresh = refreshDashboard;';
    return request.fulfill({ status: 200, contentType: asset.type, body });
  });
  await page.addInitScript(() => {
    globalThis.__reads = [];
    globalThis.__logical = [];
    globalThis.__bytes = 0;
    globalThis.__offset = 0;
    const now = Date.now;
    Date.now = () => now() + globalThis.__offset;
    globalThis.EventSource = class { constructor() { this.readyState = 0; } close() {} };
    // The independent timers are verified by the fleet/host fixtures. Capture
    // them here so timing never changes which requests belong to one cycle.
    const timeout = globalThis.setTimeout;
    globalThis.__timers = [];
    globalThis.setTimeout = (fn, ms, ...args) => {
      if (ms >= 5000) { globalThis.__timers.push(ms); return 0; }
      return timeout(fn, ms, ...args);
    };
    globalThis.setInterval = () => 0;
    globalThis.__fixtureVersion = 0;
    const payload = url => {
      const empty = { items: [], total: 0, limit: 50, truncated: false };
      const version = globalThis.__fixtureVersion;
      const workspace = url.searchParams.get('workspace') || 'one';
      const notice = { id: 'notice', revision: version, kind: 'incident', scope: 'workspace', title: `${workspace} incident ${version}`, opened_at: '2026-11-03T00:00:00Z', expires_at: '2099-01-01T00:00:00Z' };
      if (url.pathname.startsWith('/api/diagnostics/')) {
        const window = { key: url.searchParams.get('since') || '24h', bucket_seconds: 3600,
          since: '2026-11-02T00:00:00Z', until: '2026-11-03T00:00:00Z' };
        const counts = { done: { total: 0, low: 0, medium: 0, hard: 0, other: 0 }, rejected: 0, archived: 0 };
        if (url.pathname.endsWith('/completions')) return { window, buckets: [], current: counts, previous: counts };
        if (url.pathname.endsWith('/defects')) return { window, buckets: [], current: { total: 0, lineage: {}, origin: {} }, previous: null };
        if (url.pathname.endsWith('/implement_one')) return { window, outcome: url.searchParams.get('outcome') || 'succeeded', buckets: [], current: {}, previous: {}, actors: [], slowest: [], implement_one_by_complexity: [] };
        return { window, buckets: [], bucket_starts: [], series: [], families: [], tags: [], total: { n: 0 }, overall: { tasks: 0 }, crews: [], coverage: {}, available_models: [], available_providers: [], available_tags: [] };
      }
      switch (url.pathname) {
        case '/api/workspaces': return [{ id: 'one', name: 'one', status: 'active', is_default: true }, { id: 'two', name: 'two', status: 'active' }];
        case '/api/hosts': return { hosts: [{ name: 'local', machine_id: 'hm-fixture', local: true, reachable: true, workspaces: [{ id: 'one', name: 'one', status: 'active' }, { id: 'two', name: 'two', status: 'active' }] }] };
        case '/api/notices': return { open: [notice], recent: [], banner: [notice], degraded: [] };
        case '/api/routines': return { routines: [], clock: {} };
        case '/api/crews': return { crews: [] };
        case '/api/workflows/auto/readiness': return { capacity: {}, tasks: [] };
        case '/api/fleet/drains': return { items: [] };
        case '/api/job-runs': return { ...empty, running_items: [] };
        case '/api/tasks/locks': return { total_locked: 0, by_task: [] };
        case '/api/jobs/failure-streaks': return { failing: version ? [{ job_id: 'fixture', count: version, since: '2026-11-03T00:00:00Z' }] : [] };
        case '/api/runs/fixture': return { run: { run_id: 'fixture', job_id: 'fixture', state: 'success' }, steps: [] };
        case '/api/jobs': return [];
        case '/api/config/effective': return {};
        case '/api/auto-tasks': return { definitions: [] };
        case '/api/frictions/stats': return {};
        default: return url.pathname.endsWith('/events') || url.pathname.endsWith('/logs') ? [] : empty;
      }
    };
    globalThis.fetch = async input => {
      const url = new URL(input, location.href);
      globalThis.__reads.push(url.pathname + url.search);
      const reply = url => {
        globalThis.__logical.push(url.pathname + url.search);
        const failed = globalThis.__failedPath === url.pathname;
        return { status: failed ? 503 : 200, body: JSON.stringify(failed ? { error: 'Fixture unavailable', code: 'fixture_unavailable' } : payload(url)) };
      };
      let response;
      if (url.pathname === '/api/dashboard/chrome') {
        if (globalThis.__missingChrome) return new Response('', { status: 404 });
        if (globalThis.__offlineChrome) throw new TypeError('Fixture transport unavailable');
        const component = pathname => {
          const target = new URL(pathname, location.href);
          target.searchParams.set('workspace', url.searchParams.get('workspace'));
          return reply(target);
        };
        response = { status: 200, body: JSON.stringify({
          notices: component('/api/notices'),
          summary: component('/api/audit/summary?since=' + url.searchParams.get('since')),
          routines: url.searchParams.get('routines') === 'true' ? component('/api/routines') : null,
          failures: url.searchParams.get('failures') === 'true' ? component('/api/jobs/failure-streaks') : null,
        }) };
      } else response = reply(url);
      globalThis.__bytes += new TextEncoder().encode(response.body).length;
      return new Response(response.body, { status: response.status, headers: { 'content-type': 'application/json' } });
    };
  });
  await page.goto(`http://perf.invalid/?workspace=one#${route}`);
  await page.waitForFunction(() => typeof globalThis.__perfRefresh === 'function');
  await page.waitForTimeout(50);
  return page;
}

async function cycle(page, mode, options = { poll: true }) {
  return page.evaluate(async ({ mode, options }) => {
    globalThis.__offset += mode === 'slow' ? 300001 : 30000;
    globalThis.__reads = []; globalThis.__logical = []; globalThis.__bytes = 0;
    const start = performance.now();
    const status = await globalThis.__perfRefresh(options);
    await new Promise(resolve => setTimeout(resolve, 0));
    return { paths: globalThis.__reads, logical_paths: globalThis.__logical,
      count: globalThis.__reads.length, response_bytes: globalThis.__bytes, status,
      elapsed_ms: performance.now() - start };
  }, { mode, options });
}

try {
  for (const route of routes) {
    const page = await fixturePage(route);
    for (const trend of route === 'analytics/metrics' ? (measure ? ['tokens'] : trends) : ['default']) {
      if (trend !== 'default') {
        await page.locator(`[data-trend="${trend}"]`).click();
        await page.waitForTimeout(0);
      }
      for (const mode of ['slow', 'regular']) {
        const observations = [];
        // One first observation and 20 warm samples, sequentially, with 250ms
        // after completion. No parallel load and no live traffic.
        for (let index = 0; index < (measure ? 21 : 1); index += 1) {
          const sample = await cycle(page, mode);
          observations.push(sample);
          if (!base) assert.ok(sample.count <= 10, `${route}/${trend}/${mode}: ${sample.count} API reads: ${sample.paths}`);
          if (measure) await page.waitForTimeout(250);
        }
        const warm = observations.slice(1).map(sample => sample.elapsed_ms).sort((a, b) => a - b);
        samples.push({ route, trend, mode, first: observations[0], warm: observations.slice(1),
          p50_ms: warm.length ? warm[Math.ceil(warm.length * .50) - 1] : null,
          p95_ms: warm.length ? warm[Math.ceil(warm.length * .95) - 1] : null });
      }
    }
    if (screenshots) {
      for (const width of [1440, 375]) {
        await page.setViewportSize({ width, height: 900 });
        await page.screenshot({ path: path.join(output, `${route.replaceAll('/', '-')}-dark-${width}.png`) });
      }
    }
    if (!base && route === 'tasks') {
      await page.evaluate(() => { globalThis.__fixtureVersion = 2; });
      await cycle(page, 'slow');
      assert.equal(await page.locator('.notice-banner-title').textContent(), 'one incident 2');
      assert.equal(await page.locator('#rail-count-jobs-failing').textContent(), '1 failing');
      await page.evaluate(() => { globalThis.__fixtureVersion = 3; globalThis.__failedPath = '/api/notices'; });
      await cycle(page, 'slow');
      assert.equal(await page.locator('.notice-banner-title').textContent(), 'one incident 2', 'component failure keeps this visit banner');
      assert.ok((await page.locator('#rail-count-jobs-failing').getAttribute('title')).includes('3 in a row'), 'another component still updates');
      await page.evaluate(() => { globalThis.__failedPath = null; });
      await cycle(page, 'slow');
      assert.equal(await page.locator('.notice-banner-title').textContent(), 'one incident 3', 'next poll recovers');
      const explicit = await cycle(page, 'regular', { explicit: true });
      assert.ok(explicit.paths.some(path => path.startsWith('/api/notices?')), 'explicit refresh reads fresh notices directly');
      await page.evaluate(async () => { (await import('/static/js/common.js')).setWorkspace('two'); });
      const scoped = await cycle(page, 'regular');
      assert.equal(await page.locator('.notice-banner-title').textContent(), 'two incident 3', 'scope switch never shares prior payload');
      assert.ok(scoped.paths.find(path => path.startsWith('/api/dashboard/chrome?')).includes('workspace=two'));
      assert.ok(scoped.logical_paths.some(path => path.startsWith('/api/decisions?')), 'scope change refreshes decisions');
      await page.evaluate(() => { globalThis.__offlineChrome = true; });
      const offline = await cycle(page, 'slow');
      assert.equal(offline.status, false, 'a failed combined transport reports a failed summary panel');
      assert.equal(await page.locator('.notice-banner-title').textContent(), 'two incident 3', 'transport failure keeps this visit banner');
      await page.evaluate(() => { globalThis.__offlineChrome = false; });
      await cycle(page, 'slow');
      assert.equal(await page.locator('.notice-banner-title').textContent(), 'two incident 3');
      await page.evaluate(() => { globalThis.__missingChrome = true; });
      const compatible = await cycle(page, 'slow');
      assert.equal(compatible.status, true, 'an older host still serves direct reads');
      assert.ok(compatible.paths.some(path => path.startsWith('/api/notices?')));

    }
    if (!base && route === 'analytics/metrics') {
      await page.evaluate(() => { globalThis.__failedPath = '/api/jobs/failure-streaks'; });
      await cycle(page, 'slow');
      await page.evaluate(() => { globalThis.__failedPath = null; });
      const retry = await cycle(page, 'regular');
      assert.ok(retry.paths.some(path => path.includes('/dashboard/chrome?') && path.includes('failures=true')), 'failed Metrics peek retries next poll');
      const retained = await cycle(page, 'regular');
      assert.ok(retained.paths.some(path => path.includes('/dashboard/chrome?') && path.includes('failures=false')), 'successful Metrics peek retains the slow cadence');
    }
    await page.close();
  }
  assert.deepEqual(errors, [], 'fixture raised render errors');
} finally {
  fs.writeFileSync(path.join(output, 'poll-counts.json'), JSON.stringify({ base: base || 'candidate', method: 'Intercepted real dashboard modules; fixed fixture, deterministic clock, no live API, nearest-rank warm percentiles', samples, errors }, null, 2));
  await browser.close();
}
console.log(JSON.stringify({ status: 'passed', base: base || 'candidate', cycles: samples.length, maximum: Math.max(...samples.map(sample => sample.first.count)) }));
