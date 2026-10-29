// Deterministic public-UI regression for ORB-19656: while the visible Tasks
// view has a connected change stream, task-only and lock-only notices must
// refresh `/api/workflows/auto/readiness` within the normal notice batching
// cadence, without a run or drain-phase event.
//
// Usage: node dashboard_change_stream_readiness_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/change-readiness
// The harness boots the shipped dashboard assets in Chromium with fixture API
// responses and a controllable EventSource, then emits task/locks notices.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3] || '.orbit/tmp/change-readiness');
fs.mkdirSync(evidence, { recursive: true });

const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  if (name === '/') {
    const served = dashboardFile(name);
    if (!served) { res.writeHead(404); res.end(); return; }
    // Strip the auto-boot script so the test installs its fetch/EventSource
    // fixtures before importing the app module itself.
    const data = served.data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
    res.setHeader('content-type', served.type);
    res.end(data);
    return;
  }
  const served = dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  res.setHeader('content-type', served.type);
  res.end(served.data);
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

let browser;
try {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const failures = [];
  page.on('pageerror', (error) => failures.push(String(error && error.message ? error.message : error)));
  await page.goto(`${origin}/?workspace=one#tasks`);

  // Install fixture fetch + controllable EventSource before booting the app.
  await page.evaluate(() => {
    globalThis.setInterval = () => 0;
    globalThis.__counts = { tasks: 0, locks: 0, readiness: 0, crews: 0 };
    globalThis.__readinessUrls = [];
    globalThis.__fixture = {
      tasks: [{ id: 'READY-1', title: 'Blocked task', status: 'backlog', priority: 'medium' }],
      readiness: [{
        task_id: 'READY-1', eligible: false, reason: 'unmet_dependency',
        dependencies: [{ task_id: 'DEP-1', status: 'backlog' }],
        conflicts: [], run_ids: [], blocking_task_ids: [],
      }],
      locks: { total_locked: 0, total_tasks: 1, by_task: [] },
    };
    globalThis.__streams = [];
    globalThis.EventSource = class FixtureStream {
      static CLOSED = 2;
      constructor(url) {
        globalThis.__streams.push(this);
        this.url = String(url);
        this.readyState = 1;
        queueMicrotask(() => {
          if (this.readyState !== 2 && typeof this.onopen === 'function') this.onopen();
        });
      }
      close() { this.readyState = 2; }
    };
    const respond = (payload) => new Response(JSON.stringify(payload), {
      status: 200, headers: { 'content-type': 'application/json' },
    });
    globalThis.fetch = async (pathName, options = {}) => {
      const url = new URL(String(pathName), window.location.href);
      const p = url.pathname;
      if (p === '/api/workspaces') {
        return respond([{ id: 'one', name: 'one', status: 'active', is_default: true }]);
      }
      if (p === '/api/hosts') return respond([]);
      if (p.startsWith('/api/hosts/') && p.endsWith('/connection')) return respond({});
      if (p === '/api/tasks') {
        globalThis.__counts.tasks += 1;
        const items = globalThis.__fixture.tasks;
        return respond({ items, total: items.length, limit: 50, truncated: false });
      }
      if (p === '/api/tasks/locks') {
        globalThis.__counts.locks += 1;
        return respond(globalThis.__fixture.locks);
      }
      if (p === '/api/workflows/auto/readiness') {
        globalThis.__counts.readiness += 1;
        globalThis.__readinessUrls.push(String(pathName));
        return respond({
          capacity: {
            active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4,
            drain_run_id: null, drain_phase: 'idle', drain_status_run_id: null,
            ends_at: null, running_admitted_workers: 0, admitted_workers: 0,
          },
          tasks: globalThis.__fixture.readiness,
        });
      }
      if (p === '/api/crews') {
        globalThis.__counts.crews += 1;
        return respond({ default_crew: null, crews: [] });
      }
      if (p === '/api/job-runs' || p === '/api/job-runs/all') {
        return respond({ items: [], total: 0, limit: 1, truncated: false });
      }
      if (p === '/api/fleet/drains') {
        return respond({ items: [{
          workspace_id: 'one', workspace_name: 'one', phase: 'idle',
          slots_used: 0, slots_ceiling: 4, eligible_count: 1, holds: [],
        }] });
      }
      if (p === '/api/routines' || p === '/api/auto-tasks') return respond({ routines: [], definitions: [], clock: {} });
      if (p === '/api/audit/summary') {
        return respond({
          events: 0, failed_runs: 0, denials: 0, denials_sql: 0, denials_v2: 0,
          window: '24h', tool_call_failure_rate: { failed: 0, total: 0, denied: 0 },
        });
      }
      if (p === '/api/host/resources') return respond({ resources: {} });
      if (p === '/api/doctor') return respond({ report: null, cached: true });
      return respond({});
    };
  });

  await page.addScriptTag({ type: 'module', url: '/app.js' });

  const check = (condition, message) => { if (!condition) throw new Error(message); };

  // Initial load: one backlog row with the stale "Waits on deps" badge.
  await page.waitForFunction(() => document.getElementById('tasks-body')
    && document.getElementById('tasks-body').textContent.includes('READY-1'), undefined, { timeout: 15000 });
  await page.waitForFunction(() => (globalThis.__counts.readiness || 0) >= 1, undefined, { timeout: 15000 });
  await page.waitForFunction(() => document.body.textContent.includes('Waits on deps'), undefined, { timeout: 15000 });
  const initial = await page.evaluate(() => ({ ...globalThis.__counts }));
  check(initial.readiness >= 1 && initial.tasks >= 1, `initial load must read tasks and readiness: ${JSON.stringify(initial)}`);
  await page.waitForFunction(() => globalThis.__streams.length > 0, undefined, { timeout: 15000 });
  const streamLive = await page.evaluate(() => globalThis.__streams.some((s) => s.readyState === 1));
  check(streamLive, 'change stream must be connected on the visible Tasks view');

  // Keep the row selected so the test also proves list/pane state survives.
  await page.locator('#tasks-body [data-key="task-READY-1"] .title').click();
  await page.waitForFunction(() => document.getElementById('task-detail-body')
    && document.getElementById('task-detail-body').textContent.includes('READY-1'));

  // Task-only invalidation: dependency removed. Fixture now eligible; the
  // titles also change to prove the list refetch ran alongside readiness.
  await page.evaluate(() => {
    globalThis.__fixture.tasks = [{ id: 'READY-1', title: 'Dependency removed', status: 'backlog', priority: 'medium' }];
    globalThis.__fixture.readiness = [{ task_id: 'READY-1', eligible: true, reason: 'eligible' }];
  });
  const beforeTaskOnly = await page.evaluate(() => ({ ...globalThis.__counts }));
  await page.evaluate(() => {
    for (const stream of globalThis.__streams) {
      if (typeof stream.onmessage === 'function') {
        stream.onmessage({ data: JSON.stringify({ kind: 'task', id: 'READY-1', status: 'backlog' }) });
      }
    }
  });
  await page.waitForFunction((before) => globalThis.__counts.readiness > before.readiness, beforeTaskOnly, { timeout: 10000 });
  await page.waitForFunction(() => document.body.textContent.includes('Eligible now'), undefined, { timeout: 10000 });
  const afterTaskOnly = await page.evaluate(() => ({ ...globalThis.__counts }));
  check(afterTaskOnly.tasks > beforeTaskOnly.tasks, `task notice must refetch the list: ${JSON.stringify({ beforeTaskOnly, afterTaskOnly })}`);
  check(afterTaskOnly.readiness === beforeTaskOnly.readiness + 1, `task-only notice must refresh readiness exactly once without a run/drain event: ${JSON.stringify({ beforeTaskOnly, afterTaskOnly })}`);
  const taskRowText = await page.locator('#tasks-body [data-key="task-READY-1"]').textContent();
  check(taskRowText.includes('Dependency removed'), `task notice must update the row title: ${taskRowText.slice(0, 200)}`);
  check(taskRowText.includes('Eligible now'), `task notice must update the readiness badge: ${taskRowText.slice(0, 300)}`);
  const detailStillOpen = await page.evaluate(() => Boolean(document.getElementById('task-detail-body')
    && document.getElementById('task-detail-body').textContent.includes('READY-1')));
  check(detailStillOpen, 'task notice must preserve the open task pane');

  // Lock-only invalidation: go back to waiting on a lock, then clear it with
  // only a locks notice. Readiness must follow even though no task changed.
  await page.evaluate(() => {
    globalThis.__fixture.readiness = [{
      task_id: 'READY-1', eligible: false, reason: 'context_lock_conflict',
      conflicts: [{ locking_task_id: 'HOLDER-1' }], run_ids: [], blocking_task_ids: [],
    }];
    globalThis.__fixture.locks = {
      total_locked: 1, total_tasks: 1,
      by_task: [{ id: 'READY-1', title: 'Dependency removed', status: 'backlog', priority: 'medium', context_files: ['file:locked.rs'], job_run_id: 'jrun-locked' }],
    };
  });
  // Force the waiting state through a drain notice once, then measure the
  // lock-only step from there (a drain event is the old workaround).
  await page.evaluate(() => {
    for (const stream of globalThis.__streams) {
      if (typeof stream.onmessage === 'function') {
        stream.onmessage({ data: JSON.stringify({ kind: 'drain', phase: 'idle' }) });
      }
    }
  });
  await page.waitForFunction(() => {
    const row = document.querySelector('#tasks-body [data-key="task-READY-1"]');
    return row && row.textContent.includes('Waits on HOLDER-1');
  }, undefined, { timeout: 10000 });
  await page.evaluate(() => {
    globalThis.__fixture.readiness = [{ task_id: 'READY-1', eligible: true, reason: 'eligible' }];
    globalThis.__fixture.locks = { total_locked: 0, total_tasks: 1, by_task: [] };
  });
  const beforeLockOnly = await page.evaluate(() => ({ ...globalThis.__counts }));
  await page.evaluate(() => {
    for (const stream of globalThis.__streams) {
      if (typeof stream.onmessage === 'function') {
        stream.onmessage({ data: JSON.stringify({ kind: 'locks' }) });
      }
    }
  });
  await page.waitForFunction((before) => globalThis.__counts.readiness > before.readiness, beforeLockOnly, { timeout: 10000 });
  await page.waitForFunction(() => {
    const row = document.querySelector('#tasks-body [data-key="task-READY-1"]');
    return row && row.textContent.includes('Eligible now');
  }, undefined, { timeout: 10000 });
  const afterLockOnly = await page.evaluate(() => ({ ...globalThis.__counts }));
  check(afterLockOnly.readiness === beforeLockOnly.readiness + 1, `lock-only notice must refresh readiness exactly once: ${JSON.stringify({ beforeLockOnly, afterLockOnly })}`);

  // Filtered-out dependency: a done dependency finishes. Its notice carries a
  // status outside the visible filter, so the list correctly stays, but the
  // visible row's readiness must still refresh.
  await page.evaluate(() => {
    globalThis.__fixture.readiness = [{
      task_id: 'READY-1', eligible: false, reason: 'unmet_dependency',
      dependencies: [{ task_id: 'DEP-1', status: 'backlog' }],
      conflicts: [], run_ids: [], blocking_task_ids: [],
    }];
  });
  await page.evaluate(() => {
    for (const stream of globalThis.__streams) {
      if (typeof stream.onmessage === 'function') {
        stream.onmessage({ data: JSON.stringify({ kind: 'drain', phase: 'idle' }) });
      }
    }
  });
  await page.waitForFunction(() => document.body.textContent.includes('Waits on deps'), undefined, { timeout: 10000 });
  await page.evaluate(() => {
    globalThis.__fixture.readiness = [{ task_id: 'READY-1', eligible: true, reason: 'eligible' }];
  });
  const beforeFiltered = await page.evaluate(() => ({ ...globalThis.__counts, tasks: globalThis.__counts.tasks }));
  await page.evaluate(() => {
    for (const stream of globalThis.__streams) {
      if (typeof stream.onmessage === 'function') {
        stream.onmessage({ data: JSON.stringify({ kind: 'task', id: 'DEP-1', status: 'done' }) });
      }
    }
  });
  await page.waitForFunction((before) => globalThis.__counts.readiness > before.readiness, beforeFiltered, { timeout: 10000 });
  await page.waitForFunction(() => {
    const row = document.querySelector('#tasks-body [data-key="task-READY-1"]');
    return row && row.textContent.includes('Eligible now');
  }, undefined, { timeout: 10000 });
  const afterFiltered = await page.evaluate(() => ({ ...globalThis.__counts }));
  check(afterFiltered.readiness === beforeFiltered.readiness + 1, `filtered-out dependency notice must refresh readiness: ${JSON.stringify({ beforeFiltered, afterFiltered })}`);
  check(afterFiltered.tasks === beforeFiltered.tasks, `filtered-out dependency must not refetch the hidden list: ${JSON.stringify({ beforeFiltered, afterFiltered })}`);

  // Combined burst batches to a single readiness read.
  await page.evaluate(() => {
    globalThis.__fixture.readiness = [{
      task_id: 'READY-1', eligible: false, reason: 'unmet_dependency',
      dependencies: [{ task_id: 'DEP-1', status: 'backlog' }],
      conflicts: [], run_ids: [], blocking_task_ids: [],
    }];
  });
  await page.evaluate(() => {
    for (const stream of globalThis.__streams) {
      if (typeof stream.onmessage === 'function') {
        stream.onmessage({ data: JSON.stringify({ kind: 'drain', phase: 'idle' }) });
      }
    }
  });
  await page.waitForFunction(() => document.body.textContent.includes('Waits on deps'), undefined, { timeout: 10000 });
  await page.evaluate(() => {
    globalThis.__fixture.readiness = [{ task_id: 'READY-1', eligible: true, reason: 'eligible' }];
  });
  const beforeBurst = await page.evaluate(() => ({ ...globalThis.__counts }));
  await page.evaluate(() => {
    for (const stream of globalThis.__streams) {
      if (typeof stream.onmessage !== 'function') continue;
      stream.onmessage({ data: JSON.stringify({ kind: 'task', id: 'READY-1', status: 'backlog' }) });
      stream.onmessage({ data: JSON.stringify({ kind: 'locks' }) });
    }
  });
  await page.waitForFunction((before) => globalThis.__counts.readiness > before.readiness, beforeBurst, { timeout: 10000 });
  await page.waitForTimeout(600);
  const afterBurst = await page.evaluate(() => ({ ...globalThis.__counts, urls: [...globalThis.__readinessUrls] }));
  check(afterBurst.readiness === beforeBurst.readiness + 1, `task+locks burst must batch to one readiness read: ${JSON.stringify({ beforeBurst, afterBurst })}`);
  // Scoped read: no backlog forecast, and the open backlog task rides along.
  const lastReadinessUrl = afterBurst.urls[afterBurst.urls.length - 1];
  check(!lastReadinessUrl.includes('forecast'), `readiness must stay scoped without a forecast read: ${lastReadinessUrl}`);

  // Slow cadence preserved: crew registry (a slow read) does not refetch on
  // change notices.
  const crewsBefore = afterBurst.crews;
  check(typeof crewsBefore === 'number', 'crew count must be tracked');
  await page.waitForTimeout(600);
  const crewsAfter = await page.evaluate(() => globalThis.__counts.crews);
  check(crewsAfter === crewsBefore, `notices must not restore slow reads: crews ${crewsBefore} -> ${crewsAfter}`);

  if (failures.length) throw new Error(failures.join('\n'));

  // Evidence: desktop and phone widths of the fixed Tasks view (dark-only).
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: path.join(evidence, 'change-stream-readiness-dark-1440.png') });
  await page.setViewportSize({ width: 375, height: 900 });
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: path.join(evidence, 'change-stream-readiness-dark-375.png') });

  fs.writeFileSync(path.join(evidence, 'change-stream-readiness-result.json'), JSON.stringify({
    passed: true,
    initial,
    afterTaskOnly,
    afterLockOnly,
    afterFiltered,
    afterBurst: { tasks: afterBurst.tasks, locks: afterBurst.locks, readiness: afterBurst.readiness, crews: afterBurst.crews },
    widths: [1440, 375],
  }, null, 2));
  console.log('Change-stream task/lock readiness regression passed.');
} finally {
  await browser?.close();
  if (server.listening) await new Promise((resolve) => server.close(resolve));
}
