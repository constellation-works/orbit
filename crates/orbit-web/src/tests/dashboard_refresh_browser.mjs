// Usage: node dashboard_refresh_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/refresh-browser
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const server = http.createServer((req, res) => {
  const served = dashboardFile(new URL(req.url, 'http://fixture').pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  res.setHeader('content-type', served.type);
  res.end(served.data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

const retained = '.kpi:not(.host-resource), .rail-count, #global-drain-state';
const snapshots = [];
const pageErrors = [];
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  for (const width of [1440, 768]) {
    const page = await browser.newPage({ viewport: { width, height: 900 }, timezoneId: 'America/Los_Angeles' });
    page.on('pageerror', error => { pageErrors.push(error.message); console.error(error); });
    let offline = false;
    let failedPath = null;
    let failureStatus = 500;
    let events = 15700;
    const aborted = [];
    // Use real browser fetches and Playwright request aborts; only the API
    // responses are fixture data. The shipped app, markup and CSS run intact.
    await page.route('**/api/**', async route => {
      const url = new URL(route.request().url());
      if (offline) {
        aborted.push(url.pathname);
        await route.abort('connectionrefused');
        return;
      }
      if (url.pathname === failedPath) {
        await route.fulfill({ status: failureStatus, json: { error: 'Fixture panel unavailable' } });
        return;
      }
      let payload = [];
      switch (url.pathname) {
        case '/api/workspaces':
          payload = [{ id: 'one', name: 'one', status: 'active', is_default: true }];
          break;
        case '/api/tasks':
          payload = { items: [{ id: 'FIXTURE-1', title: 'Refresh fixture', status: 'in-progress', priority: 'medium' }], total: 1, limit: 50, truncated: false };
          break;
        case '/api/crews':
          payload = { crews: [] };
          break;
        case '/api/audit/summary':
          payload = { events, failed_runs: 99, denials: 4, active_long_runs: 2, window: '24h' };
          break;
        case '/api/workflows/auto/readiness':
          payload = { capacity: { drain_phase: 'draining', drain_run_id: 'fixture', drain_status_run_id: 'fixture', running_admitted_workers: 1, active_leaf_runs: 1, max_active_leaf_runs: 4, free_slots: 3, ends_at: new Date(Date.now() + 3600000).toISOString() }, tasks: [] };
          break;
        case '/api/config/file':
          payload = { sections: [], scope: 'workspace', crews: [], paths: [] };
          break;
        case '/api/runs/fixture':
          payload = { run_id: 'fixture', job_id: 'fixture', state: 'success', steps: [] };
          break;
        case '/api/routines':
          payload = { routines: [{ name: 'fixture', source: 'one', target: 'orbit://job/fixture', enabled: true }], clock: {} };
          break;
      }
      await route.fulfill({ json: payload });
    });
    const settled = () => page.waitForFunction(() => {
      const text = document.getElementById('meta-text').textContent;
      const dot = document.getElementById('conn-status');
      getComputedStyle(dot).backgroundColor;
      return /^(refreshed|offline|panel update failed)/.test(text)
        && dot.getAnimations().every(animation => animation.playState === 'finished');
    }, undefined, { timeout: 10000 });
    const refresh = async () => { await page.locator('#refresh-btn').click(); await settled(); };
    const snapshot = async name => {
      const state = await page.evaluate(selector => ({
        status: document.getElementById('conn-status').className,
        color: getComputedStyle(document.getElementById('conn-status')).backgroundColor,
        label: document.getElementById('meta-text').textContent,
        nodes: [...document.querySelectorAll(selector)].map(node => ({
          id: node.id, text: node.textContent.trim(), title: node.title,
          stale: node.classList.contains('refresh-stale'), opacity: Number(getComputedStyle(node).opacity),
          border: getComputedStyle(node).borderStyle,
        })),
        drainAnimation: getComputedStyle(document.querySelector('#global-drain-state .drain-dot')).animationName,
      }), retained);
      snapshots.push({ width, name, ...state });
      return state;
    };
    const expectStale = state => {
      assert(state.nodes.length >= 8, 'The health, rail count and drain surfaces must all be exercised');
      for (const node of state.nodes) {
        assert(node.stale && node.opacity < 1, `${node.id} must visibly dim retained data`);
        assert.match(node.title, /as of \d{2}:\d{2}/, `${node.id} must identify the last clean refresh`);
        assert.equal((node.title.match(/as of/g) || []).length, 1, 'Repeated failures must not duplicate freshness titles');
      }
      assert.equal(state.drainAnimation, 'none', 'A stale Draining pill must stop pulsing as live');
    };
    const expectClean = state => {
      assert.match(state.status, /green/);
      for (const node of state.nodes) {
        assert(!node.stale && node.opacity === 1, `${node.id} must clear stale styling after recovery`);
        assert(!node.title.includes('Stale'), `${node.id} must clear the freshness title after recovery`);
      }
    };

    await page.goto(`http://127.0.0.1:${server.address().port}/#tasks`);
    await settled();
    await page.locator('#global-drain-state').waitFor({ state: 'visible' });
    const clean = await snapshot('initial-clean');
    expectClean(clean);
    offline = true;
    await refresh();
    const down = await snapshot('offline');
    assert.match(down.status, /red/);
    assert.match(down.label, /offline/);
    assert(aborted.includes('/api/audit/summary') && aborted.includes('/api/workflows/auto/readiness'), 'Playwright must abort the health and drain API requests');
    expectStale(down);
    assert.deepEqual(down.nodes.map(node => node.text), clean.nodes.map(node => node.text), 'Offline chrome must retain its last values');
    await page.screenshot({ path: path.join(evidence, `offline-${width}.png`) });
    await refresh();
    expectStale(await snapshot('repeated-offline'));

    offline = false;
    await refresh();
    expectClean(await snapshot('offline-recovery'));
    await page.locator('.tab[data-tab="config"]').click();
    await page.locator('#config-subtabs [data-subtab="workspace-file"]').click();
    await settled();
    assert.match((await snapshot('workspace-file-clean')).label, /refreshed Settings/);
    failedPath = '/api/config/file';
    events = 18000;
    await refresh();
    const partial = await snapshot('workspace-file-failure');
    assert.match(partial.status, /orange/);
    assert.notEqual(partial.color, clean.color, 'A failed panel must not display the clean green dot');
    assert.match(partial.label, /Settings › Workspace file/);
    assert(!partial.label.includes('offline'), 'An HTTP panel failure must remain distinct from offline');
    assert.equal(partial.nodes.find(node => node.id === 'tile-events').text, '18.0kevents', 'Healthy panels must keep updating during a partial failure');
    expectStale(partial);
    await page.screenshot({ path: path.join(evidence, `partial-${width}.png`) });
    await refresh();
    expectStale(await snapshot('repeated-partial'));
    failedPath = null;
    await refresh();
    const recovered = await snapshot('clean-recovery');
    expectClean(recovered);
    assert.equal(recovered.nodes.find(node => node.id === 'tile-failed').title, clean.nodes.find(node => node.id === 'tile-failed').title, 'Recovery must preserve the KPI navigation tooltip');
    await page.screenshot({ path: path.join(evidence, `recovered-${width}.png`) });

    // Run-detail reads render their own feedback, but must still report failed
    // events/log panels to the connection line. Missing optional streams (404)
    // remain a clean response.
    await page.evaluate(() => { location.hash = 'runs/fixture'; });
    await page.waitForFunction(() => document.getElementById('meta-text').textContent.startsWith('refreshed Runs'));
    for (const [stream, label] of [['events', 'Run events'], ['logs', 'Run logs']]) {
      failedPath = `/api/runs/fixture/${stream}`;
      await refresh();
      const failure = await snapshot(`${stream}-failure`);
      assert.match(failure.status, /orange/);
      assert(failure.label.includes(label), 'The failing run stream must be named');
      failureStatus = 404;
      await refresh();
      expectClean(await snapshot(`${stream}-absent`));
      failureStatus = 500;
    }
    failedPath = null;
    await page.locator('.tab[data-tab="operations"]').click();
    await refresh();
    expectClean(await snapshot('routines-clean'));
    assert.equal(await page.locator('#rail-count-ops-routines').textContent(), '1/1');
    failedPath = '/api/routines';
    await refresh();
    const routineFailure = await snapshot('routines-failure');
    assert.match(routineFailure.label, /Automation › Routines/);
    assert(!routineFailure.label.includes('1/1'), 'A panel label must exclude its rail count badge');
    await page.close();
  }
  assert.deepEqual(pageErrors, [], 'The shipped dashboard must not raise uncaught browser errors');
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, snapshots, pageErrors }, null, 2));
  console.log('Dashboard refresh browser checks passed at 1440px and 768px: API abort, stale chrome, named amber failures, repeated failure and clean recovery.');
} catch (error) {
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: false, error: error.message, snapshots, pageErrors }, null, 2));
  throw error;
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
