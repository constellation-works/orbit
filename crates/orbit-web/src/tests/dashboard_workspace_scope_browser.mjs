import fs from 'node:fs';
import path from 'node:path';

// Drive scope chips and their downstream actions through the full shipped app.
export async function assertWorkspaceScope(browser, origin, evidence) {
  for (const workspaceCount of [1, 2]) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    let reliabilityEvidence = null;
    try {
      await page.goto(`${origin}/#diagnostics/incidents/events?role=operator`);
      await page.evaluate(async workspaceCount => {
        const task = { id: 'SCOPE-1', title: 'Workspace scope fixture', status: 'review', priority: 'medium' };
        const claim = {
          claim_id: 'scope-claim', task_id: task.id, phase: 'handed_off', unsettled: true,
          handoff: {
            handoff_id: 'scope-handoff',
            candidate: { candidate: { commit: 'a'.repeat(40) }, base: { commit: 'b'.repeat(40) } },
            authority: { state: 'not_authorized' },
          },
        };
        const allowed = { authorized: true };
        let routineEnabled = true;
        let drainRunId = null;
        globalThis.scopeRequests = [];
        // Reliability reads are fleet-wide per host; a host in `reliabilityHoldHosts`
        // answers only when the test settles its entry in `reliabilityHeld`.
        globalThis.reliabilityHoldHosts = new Set();
        globalThis.reliabilityHeld = [];
        const reliabilityReport = (host, label) => {
          const runs = host === 'beta' ? 5 : 17;
          const workspace = `${host}-only-workspace`;
          const counts = globalThis.reliabilityAccounting?.counts
            || { total: runs, succeeded: runs - 2, failed: 2, cancelled: 0, skipped: 0, held: 0, in_flight: 0, unknown: 0 };
          const rate = globalThis.reliabilityAccounting?.rate
            || { numerator: 2, denominator: runs, denominator_label: 'settled runs', value: 2 / runs, low_sample: true };
          return {
            scope: 'fleet',
            window: { label, since: '2026-10-10T00:00:00Z', until: '2026-10-11T00:00:00Z', bucket: '1h' },
            totals: { job_runs: { counts, failure_rate: rate, truncated: true },
              recovery: { per_step_invocation: rate, per_job_run: rate } },
            workspaces: [{ workspace_id: workspace, workspace_name: workspace,
              job_runs: { counts, failure_rate: rate, by_job: [{ job_id: `${host}-job`, counts, failure_rate: rate }],
                over_time: [{ bucket_start: '2026-10-10T12:00:00Z', counts }] },
              recovery: { by_activity: [{ activity_id: `${host}-recover`, invocation_count: 3, job_run_count: 2, role: 'recovery' }] } }],
            unreadable_workspaces: [`${host}-unreadable`],
          };
        };
        globalThis.reliabilityView = () => {
          const text = id => document.getElementById(id).textContent;
          const status = document.querySelector('#reliability-status [data-panel-status]');
          return {
            status: status ? status.textContent : '',
            badgeHidden: document.getElementById('reliability-scope-badge').hidden,
            report: Object.fromEntries(['reliability-summary', 'reliability-denominator-note', 'reliability-truncation-note',
              'reliability-over-time', 'reliability-over-time-axis', 'reliability-breakdown', 'reliability-activities',
              'reliability-meta', 'reliability-count'].map(id => [id, text(id)])),
          };
        };
        globalThis.scopeConfirmations = [];
        window.confirm = message => { globalThis.scopeConfirmations.push(message); return true; };
        globalThis.EventSource = class { close() {} };
        globalThis.fetch = async (url, options = {}) => {
          const parsed = new URL(url, window.location.href);
          const workspace = parsed.searchParams.get('workspace');
          const pathname = parsed.pathname;
          const respond = (payload, status = 200) => new Response(JSON.stringify(payload), { status });
          if (options.method === 'POST') {
            globalThis.scopeRequests.push({ path: pathname, workspace });
            if (workspace !== 'one') return respond({ code: 'workspace_required', error: 'Select a workspace' }, 400);
            if (pathname.endsWith('/approve')) claim.handoff.authority.state = 'authorized';
            if (pathname.endsWith('/revoke')) claim.handoff.authority.state = 'revoked';
            if (pathname.endsWith('/recover')) { claim.unsettled = false; claim.phase = 'recovered'; }
            if (pathname === '/api/routines/toggle') routineEnabled = JSON.parse(options.body).enabled;
            if (pathname === '/api/workflows/auto') {
              drainRunId = 'scope-drain';
              return respond({ run_id: drainRunId, state: 'submitted' });
            }
            if (pathname === '/api/workflows/auto/stop') { drainRunId = null; return respond({ outcome: 'stopped' }); }
            return respond({ ok: true, message: 'Saved', result: { claim_id: claim.claim_id, phase: claim.phase } });
          }
          if (pathname.endsWith('/metrics/reliability')) {
            const host = pathname.startsWith('/api/on/') ? decodeURIComponent(pathname.split('/')[3]) : 'alpha';
            const label = parsed.searchParams.get('window');
            if (!globalThis.reliabilityHoldHosts.has(host)) return respond(reliabilityReport(host, label));
            return new Promise(resolve => globalThis.reliabilityHeld.push({ host, label, settle: status =>
              resolve(status === 200 ? respond(reliabilityReport(host, label)) : respond({ error: `${host} reliability unavailable` }, status)) }));
          }
          if (pathname === '/api/workspaces') return respond(['one', 'two'].slice(0, workspaceCount)
            .map(id => ({ id, name: id, status: 'active', is_default: id === 'one' })));
          if (pathname === '/api/tasks' || pathname === '/api/tasks/all') return respond({ items: [task, { id: 'PROPOSAL-1', title: 'Approve the proposed dashboard task', status: 'proposed', priority: 'medium' }], total: 2 });
          if (pathname === `/api/tasks/${task.id}`) return respond(task);
          if (pathname === '/api/audit') return respond([]);
          if (pathname === '/api/crews') return respond({ crews: [] });
          if (pathname === '/api/routines') return respond({
            machine_name: 'fixture', capabilities: { routine_toggle: allowed }, clock: {},
            routines: [{ name: 'scope-routine', source: 'one', target: 'job:fixture', enabled: routineEnabled }],
          });
          if (pathname === '/api/fleet/drains') return respond({ items: [{
            workspace_id: 'one', workspace_name: 'Fixture workspace', phase: drainRunId ? 'draining' : 'idle',
            slots_used: drainRunId ? 1 : 0, slots_ceiling: 4, eligible_count: 1, holds: [],
          }] });
          if (pathname === '/api/runs/scope-drain') return respond({ run: {
            run_id: 'scope-drain', state: 'running', job_id: 'workspace_auto_pipeline',
            drain_last_pass: { queued: 3, deferred_total: 1, excluded_total: 2, recorded_at: '2026-10-18T20:00:00Z' },
          }, steps: [] });
          if (pathname === '/api/workflows/auto/readiness') return respond({
            controls_authorized: true, total: 2, tasks: [
              { task_id: 'READY-1', eligible: true },
              { task_id: 'WAIT-1', eligible: false, reason: 'unmet_dependency' },
            ],
            capacity: { active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4,
              drain_run_id: drainRunId, drain_phase: drainRunId ? 'draining' : 'idle',
              ends_at: drainRunId ? new Date(Date.now() + 3600000).toISOString() : null,
              admitted_workers: 3, running_admitted_workers: 1,
              last_admission_forecast: {
                mode: 'enforce', forecast_at: '2026-10-18T20:00:00Z',
                cpu_peak: { minute: 10, value: 3, percent: 60 },
                memory_peak: { minute: 5, value: 1000, percent: 40 },
                headroom: { admit_cpu_percent: 89, admit_memory_percent: 74 },
                series: { bucket_minutes: 5,
                  cpu_percent: [20, 40, 60, 50, 40, 30, 20, 20, 20, 20, 20, 20, 20],
                  memory_percent: [30, 40, 35, 35, 35, 35, 35, 35, 35, 35, 35, 35, 35] },
                admitted: [], skipped: [],
              } },
          });
          if (pathname === '/api/distributed/claims') return respond({
            owner_workspace: true, claims: [claim],
            capabilities: { handoff_approve: allowed, handoff_revoke: allowed, claim_recover: allowed },
          });
          return respond({ items: [], total: 0 });
        };
        await import('/app.js');
      }, workspaceCount);
      const chip = page.locator('#audit-scope-chips [data-chip="workspace"]');
      await chip.waitFor({ state: 'visible' });
      if (workspaceCount === 1) {
        if (await page.locator('#workspace-select').count()) throw new Error('Single workspace must be initialized without a picker');
        if (await chip.evaluate(node => node.matches('button, [role="button"]') || node.tabIndex >= 0 || !!node.querySelector('.scope-chip-x'))) {
          throw new Error('The sole workspace chip must have no remove control');
        }
        await chip.click();
        if (await page.evaluate(async () => (await import('/js/common.js')).getWorkspace()) !== 'one') {
          throw new Error('Interacting with the sole workspace chip must preserve its request scope');
        }
      } else {
        await chip.click();
        await chip.waitFor({ state: 'detached' });
        if (await page.evaluate(async () => (await import('/js/common.js')).getWorkspace()) !== null
          || new URL(page.url()).searchParams.get('workspace') !== 'all') {
          throw new Error('Removing a multi-workspace chip must select and persist aggregate scope');
        }
        await page.locator('#workspace-select').selectOption('one');
        await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('diagnostics/incidents/events?role=operator'));
        await chip.waitFor({ state: 'visible' });
      }
      await page.locator('#audit-scope-chips [data-chip="actor"]').click();
      await page.locator('#audit-scope-chips [data-chip="actor"]').waitFor({ state: 'detached' });
      if (!(await chip.isVisible())) throw new Error('Removing an Events filter must retain the workspace chip');

      await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('operations/routines'));
      const routine = page.locator('#routines-body .operation-switch');
      await routine.waitFor({ state: 'visible' });
      if (!(await routine.isEnabled())) throw new Error('Operations must remain enabled after chip interaction');
      await routine.click();
      await page.locator('#routine-operation-feedback.success').waitFor({ state: 'visible' });
      if (await page.locator('#routines-body .operations-readonly-note').count()) throw new Error('Single-workspace actions must not show an aggregate read-only note');

      // Routes are exercised through the shipped router, including in-place
      // legacy normalization. Every page retains a real link to Drain.
      for (const [legacy, expected] of [
        ['auto-drain', '#drain'], ['operations/auto-drain', '#drain'],
        ['tasks/drain', '#drain'], ['tasks/log', '#drain/log'],
        ['tasks?dock=drain', '#drain'], ['tasks?dock=log', '#drain/log'],
      ]) {
        await page.evaluate(async route => (await import('/js/router.js')).setActiveTab(route), legacy);
        if (new URL(page.url()).hash !== expected || !await page.locator('.tab-pane[data-tab="drain"]').isVisible()) {
          throw new Error(`Legacy drain route ${legacy} did not reach ${expected}: ${page.url()}`);
        }
        const mode = await page.locator('#side-dock').getAttribute('data-mode');
        if (mode !== (expected.endsWith('/log') ? 'log' : 'drain')) throw new Error(`Wrong Drain view for ${legacy}: ${mode}`);
      }
      for (const route of ['tasks', 'jobs/live', 'drain', 'diagnostics/incidents', 'analytics/metrics',
        'operations/routines', 'knowledge/frictions', 'plugins', 'config/effective']) {
        await page.evaluate(async route => (await import('/js/router.js')).setActiveTab(route), route);
        const indicator = page.locator('#global-drain-state');
        if (!await indicator.isVisible() || await indicator.getAttribute('href') !== '#drain') {
          throw new Error(`Drain chip must link to #drain on ${route}`);
        }
      }
      await page.evaluate(() => { location.hash = '#tasks?dock=log'; });
      await page.waitForFunction(() => location.hash === '#drain/log');
      await page.locator('#dock-tab-drain').click();
      await page.waitForFunction(() => location.hash === '#drain');
      await page.locator('#log-statusbar-open').click();
      await page.waitForFunction(() => location.hash === '#drain/log');
      await page.locator('.tab[data-tab="drain"]').click();
      await page.locator('#drain-waits-body').filter({ hasText: 'waits on deps' }).waitFor();
      await page.locator('#drain-fleet-body .fleet-drains').waitFor();
      await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('drain'));
      await page.locator('#dock-mode-toggle [data-mode="drain"]').click();
      const start = page.locator('#auto-drain-start-body .drain-start');
      const stop = page.locator('#auto-drain-body .drain-stop');
      await start.waitFor({ state: 'visible' });
      if (!(await start.isEnabled()) || !(await stop.isEnabled())) throw new Error('Drain controls must remain enabled after chip interaction');
      await start.click();
      await page.locator('#auto-drain-operation-feedback.success').waitFor({ state: 'visible' });
      await page.locator('#drain-pass-body').filter({ hasText: '3 queued · 1 deferred · 2 excluded' }).waitFor();
      if (await page.locator('#auto-drain-panel .drain-forecast-spark').count() !== 2) throw new Error('Drain must show both forecast sparklines');
      const passLink = new URL(await page.locator('#drain-pass-body a').getAttribute('href'), page.url());
      if (passLink.hash !== '#runs?run_id=scope-drain' || passLink.searchParams.get('workspace') !== 'one') {
        throw new Error(`Admission run link lost its scope: ${passLink}`);
      }
      for (const width of [1440, 375]) {
        await page.setViewportSize({ width, height: 1000 });
        await page.locator('#dock-pane-drain').evaluate(node => { node.scrollTop = 0; });
        await page.screenshot({ path: path.join(evidence, `drain-window-dark-${width}.png`), animations: 'disabled' });
        if (width === 375) {
          await page.locator('#auto-drain-start').scrollIntoViewIfNeeded();
          await page.screenshot({ path: path.join(evidence, 'drain-controls-dark-375.png'), animations: 'disabled' });
        }
      }
      const confirmations = await page.evaluate(() => globalThis.scopeConfirmations);
      if (!confirmations.some(message => message.includes('Start'))) throw new Error('Starting a window must confirm');
      await stop.click();
      await page.locator('#auto-drain-operation-feedback.success').waitFor({ state: 'visible' });
      await page.locator('#drain-pass-body').filter({ hasText: 'No active admission window.' }).waitFor();
      if (!(await page.evaluate(() => globalThis.scopeConfirmations)).some(message => /Stop/.test(message))) {
        throw new Error('Stopping admissions must confirm');
      }

      await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('tasks'));
      if (await page.locator('#tasks-main #auto-drain-panel').count()) throw new Error('Tasks must not contain Drain');
      await page.locator('#tasks-body [data-key="task-PROPOSAL-1"] .task-quick.approve').waitFor();
      await page.getByText('Awaiting approval', { exact: true }).waitFor();
      for (const width of [1440, 375]) {
        await page.setViewportSize({ width, height: 1000 });
        await page.screenshot({ path: path.join(evidence, `tasks-approval-dark-${width}.png`), animations: 'disabled' });
      }
      await page.setViewportSize({ width: 1280, height: 900 });
      await page.locator('#tasks-body [data-key="task-SCOPE-1"] > .title').click();
      await page.locator('button.claim-action.approve').click();
      await page.locator('button.claim-action.revoke').waitFor({ state: 'visible' });
      await page.locator('input.claim-reason[data-reason-for="revoke"]').fill('Scope regression');
      await page.locator('button.claim-action.revoke').click();
      await page.locator('button.claim-action.revoke').waitFor({ state: 'detached' });
      await page.locator('input.claim-reason[data-reason-for="recover"]').fill('Scope regression');
      await page.locator('button.claim-action.recover[data-status="backlog"]').click();
      await page.locator('button.claim-action.recover').first().waitFor({ state: 'detached' });
      const requests = await page.evaluate(() => globalThis.scopeRequests);
      const expectedPaths = ['/api/routines/toggle', '/api/workflows/auto', '/api/workflows/auto/stop',
        '/api/distributed/handoffs/scope-handoff/approve', '/api/distributed/handoffs/scope-handoff/revoke',
        '/api/distributed/claims/scope-claim/recover'];
      if (requests.length !== expectedPaths.length || expectedPaths.some((pathname, index) =>
        requests[index]?.path !== pathname || requests[index]?.workspace !== 'one')) {
        throw new Error(`Actions must succeed with workspace=one: ${JSON.stringify(requests)}`);
      }
      if (workspaceCount > 1) {
        await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('analytics/metrics'));
        const railPositions = async () => page.locator('.rail .tab, .rail .subtab').evaluateAll(nodes =>
          nodes.map(node => ({ route: node.dataset.tab || node.dataset.subtab, top: node.getBoundingClientRect().top })));
        const initialRailPositions = await railPositions();
        const note = page.locator('#workspace-scope-note');

        await page.locator('#analytics-subtabs [data-subtab="reliability"]').click();
        await note.waitFor({ state: 'visible' });
        if (!(await note.textContent()).includes('Workspace filter inactive')
          || await note.getAttribute('aria-hidden') !== 'false') {
          throw new Error('Reliability must expose the workspace-filter note visually and to assistive technology');
        }
        const reliabilityPositions = await railPositions();
        // Tools reads the per-workspace audit summary, so the workspace
        // filter applies there again.
        await page.locator('#reliability-views [data-view="tools"]').click();
        await note.waitFor({ state: 'hidden' });
        await page.locator('#reliability-views [data-view="pipeline"]').click();
        await note.waitFor({ state: 'visible' });

        await page.locator('#analytics-subtabs [data-subtab="cost"]').click();
        await page.locator('#analytics-subtabs [data-subtab="cost"].active').waitFor({ state: 'visible' });
        await note.waitFor({ state: 'hidden' });
        if (await note.getAttribute('aria-hidden') !== 'true') {
          throw new Error('The workspace-filter note must be hidden from assistive technology outside Reliability › Pipeline');
        }
        const costPositions = await railPositions();
        for (const [label, positions] of [['Reliability', reliabilityPositions], ['Cost', costPositions]]) {
          if (positions.length !== initialRailPositions.length || positions.some((entry, index) =>
            entry.route !== initialRailPositions[index].route || Math.abs(entry.top - initialRailPositions[index].top) > 0.1)) {
            throw new Error(`Opening ${label} must not move dashboard rail entries: ${JSON.stringify({ initialRailPositions, positions })}`);
          }
        }
        const accounting = await assertReliabilityAccounting(page, evidence);
        reliabilityEvidence = await assertReliabilityScope(page);
        reliabilityEvidence.accounting = accounting;
      }
      if (errors.length) throw new Error(errors.join('\n'));
      fs.writeFileSync(path.join(evidence, `workspace-scope-${workspaceCount}.json`), JSON.stringify({ passed: true, workspaceCount, requests, reliability: reliabilityEvidence }, null, 2));
    } catch (error) {
      await page.screenshot({ path: path.join(evidence, `workspace-scope-${workspaceCount}-failure.png`), fullPage: true });
      throw error;
    } finally {
      await page.close();
    }
  }
  console.log('Single-workspace scope preservation and multi-workspace chip clearing passed.');
}

// Exercise held accounting through the shipped app's public fetch/render path.
// The captured incident populations must reconcile without changing their rates.
async function assertReliabilityAccounting(page, evidenceDirectory) {
  await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('analytics/reliability'));
  const cases = [
    { name: 'captured-7d', window: '7d',
      counts: { total: 7060, succeeded: 6555, failed: 431, cancelled: 54, skipped: 0, held: 13, in_flight: 7, unknown: 0 },
      settled: 6986, excluded: 74, display: '6.2% (n=6986)' },
    { name: 'captured-30d', window: '30d',
      counts: { total: 16790, succeeded: 14880, failed: 1710, cancelled: 171, skipped: 0, held: 22, in_flight: 7, unknown: 0 },
      settled: 16590, excluded: 200, display: '10.3% (n=16590)' },
    { name: 'all-excluded-states', window: '7d',
      counts: { total: 38, succeeded: 25, failed: 5, cancelled: 1, skipped: 2, held: 2, in_flight: 1, unknown: 2 },
      settled: 30, excluded: 8, display: '16.7% (n=30)' },
    { name: 'held-only', window: '7d',
      counts: { total: 13, succeeded: 0, failed: 0, cancelled: 0, skipped: 0, held: 13, in_flight: 0, unknown: 0 },
      settled: 0, excluded: 13, display: 'n/a' },
    { name: 'zero-excluded-low-sample', window: '7d',
      counts: { total: 3, succeeded: 2, failed: 1, cancelled: 0, skipped: 0, held: 0, in_flight: 0, unknown: 0 },
      settled: 3, excluded: 0, display: '1/3 · n too small', lowSample: true },
    { name: 'rolled-up-wrappers', window: '24h',
      counts: { total: 9, succeeded: 4, failed: 1, cascaded: 2, cancelled: 1, skipped: 0, held: 1, in_flight: 0, unknown: 0 },
      settled: 5, excluded: 2, display: '1/5 · n too small', lowSample: true },
  ];
  const observations = [];
  const failures = [];
  for (const scenario of cases) {
    const rate = { numerator: scenario.counts.failed, denominator: scenario.settled,
      denominator_label: 'settled runs', value: scenario.settled ? scenario.counts.failed / scenario.settled : null,
      low_sample: scenario.lowSample || false };
    await page.evaluate(({ counts, rate }) => {
      globalThis.reliabilityAccounting = { counts, rate };
    }, { counts: scenario.counts, rate });
    await page.locator(`#reliability-window-selector [data-window="${scenario.window}"]`).click();
    await page.evaluate(async () => (await import('/js/reliability.js')).fetchAndRenderReliability());
    const shown = await page.evaluate(() => {
      const tiles = Object.fromEntries(Array.from(document.querySelectorAll('#reliability-summary .rel-tile')).map(tile => [
        tile.querySelector('.rel-tile-label').textContent,
        { value: tile.querySelector('.rel-tile-value, .rel-rate').textContent,
          title: tile.querySelector('.rel-rate')?.title, hint: tile.querySelector('.rel-tile-hint').textContent },
      ]));
      return { tiles, note: document.getElementById('reliability-denominator-note').textContent,
        count: document.getElementById('reliability-count').textContent,
        scope: document.getElementById('reliability-scope-badge').textContent };
    });
    const runs = shown.tiles['Runs in window'];
    const failureRate = shown.tiles['Job-run failure rate'];
    const cascaded = scenario.counts.cascaded || 0;
    const accounting = cascaded > 0
      ? runs.hint.match(/^(\d+) settled · (\d+) rolled up · (\d+) not counted$/)
      : runs.hint.match(/^(\d+) settled · (\d+) not counted$/);
    const settledShown = accounting ? Number(accounting[1]) : NaN;
    const excludedShown = accounting ? Number(accounting[cascaded > 0 ? 3 : 2]) : NaN;
    const rolledShown = cascaded > 0 && accounting ? Number(accounting[2]) : 0;
    if (runs.value !== String(scenario.counts.total) || !accounting
      || settledShown !== scenario.settled || excludedShown !== scenario.excluded || rolledShown !== cascaded
      || Number(runs.value) !== settledShown + rolledShown + excludedShown) {
      failures.push(`${scenario.name}: Runs in window must reconcile total = settled + rolled up + excluded: ${JSON.stringify(runs)}`);
    }
    if (!shown.note.includes('rolled into that leaf')
      || !shown.note.includes('numerator') || !shown.note.includes('denominator')
      || (cascaded > 0 && !shown.note.includes(`${cascaded} wrapper`))) {
      failures.push(`${scenario.name}: the denominator note must state that wrapper echoes roll into the leaf and sit outside the rate: ${shown.note}`);
    }
    const explainedExcluded = shown.note.match(/(\d+) further run/);
    if (!['held', 'cancelled', 'skipped', 'flight', 'unrecognized'].every(state => shown.note.includes(state))
      || (scenario.excluded > 0 ? Number(explainedExcluded?.[1]) !== scenario.excluded : !shown.note.includes('No cancelled'))) {
      failures.push(`${scenario.name}: the denominator explanation must account for every excluded state: ${shown.note}`);
    }
    if (failureRate.value !== scenario.display
      || failureRate.hint !== `${scenario.counts.failed} failed of ${scenario.settled} settled runs`
      || (scenario.settled > 0 && !failureRate.title.startsWith(`${rate.numerator} / ${rate.denominator} `))
      || shown.count !== `${scenario.counts.total} runs / ${scenario.window}` || shown.scope !== 'Fleet-wide') {
      failures.push(`${scenario.name}: failure-rate counts, rounding, low-sample handling and scope must remain unchanged: ${JSON.stringify(shown)}`);
    }
    observations.push({ scenario, rate, shown,
      originalMissing: scenario.counts.total - scenario.settled
        - scenario.counts.cancelled - scenario.counts.skipped - scenario.counts.in_flight - scenario.counts.unknown });
  }
  fs.writeFileSync(path.join(evidenceDirectory, 'reliability-accounting.json'), JSON.stringify({ observations, failures }, null, 2));
  if (failures.length) throw new Error(failures.join('\n'));
  await page.evaluate(() => { delete globalThis.reliabilityAccounting; });
  await page.locator('#reliability-window-selector [data-window="24h"]').click();
  await page.evaluate(async () => (await import('/js/reliability.js')).fetchAndRenderReliability());
  return observations;
}

// Reliability's report belongs to one host and window. A replacement read that
// is held or fails must show its own load state over a retired report, never
// the previous scope's figures; a same-scope refresh keeps its report.
async function assertReliabilityScope(page) {
  const view = () => page.evaluate(() => globalThis.reliabilityView());
  const refresh = () => page.evaluate(async () => {
    (await import('/js/reliability.js')).fetchAndRenderReliability().catch(() => {});
  });
  const held = count => page.waitForFunction(count => globalThis.reliabilityHeld.length >= count, count);
  const settle = status => page.evaluate(status => {
    for (const entry of globalThis.reliabilityHeld.splice(0)) entry.settle(status);
  }, status);
  const statusShows = text => page.waitForFunction(text =>
    (document.querySelector('#reliability-status [data-panel-status]')?.textContent || '').includes(text), text);
  const reportNames = text => page.waitForFunction(text =>
    document.getElementById('reliability-breakdown').textContent.includes(text), text);
  const assertRetired = (snapshot, when) => {
    const shown = Object.entries(snapshot.report).filter(([id, text]) =>
      text !== '' && !(text === '—' && (id === 'reliability-meta' || id === 'reliability-count')));
    if (shown.length || !snapshot.badgeHidden) {
      throw new Error(`${when}: the previous scope's Reliability report must be retired: ${JSON.stringify(snapshot)}`);
    }
  };
  const evidence = {};

  await page.locator('#analytics-subtabs [data-subtab="reliability"]').click();
  await reportNames('alpha-only-workspace');
  const alpha = await view();
  if (alpha.report['reliability-count'] !== '17 runs / 24h' || !alpha.report['reliability-meta'].includes('unreadable')
    || !alpha.report['reliability-activities'].includes('alpha-recover') || alpha.badgeHidden) {
    throw new Error(`The alpha Reliability report must render: ${JSON.stringify(alpha)}`);
  }
  evidence.alpha = alpha;

  // Same host and window: the report stays, labeled as refreshing and then stale.
  await page.evaluate(() => globalThis.reliabilityHoldHosts.add('alpha'));
  await refresh();
  await held(1);
  await statusShows('Refreshing');
  const refreshing = await view();
  if (JSON.stringify(refreshing.report) !== JSON.stringify(alpha.report)) {
    throw new Error(`A same-scope refresh must keep its report while pending: ${JSON.stringify(refreshing)}`);
  }
  await settle(503);
  await statusShows('Refresh failed; showing stale data');
  const stale = await view();
  if (JSON.stringify(stale.report) !== JSON.stringify(alpha.report)) {
    throw new Error(`A failed same-scope refresh must keep its report labeled stale: ${JSON.stringify(stale)}`);
  }
  evidence.sameScopeFailure = stale;
  await page.evaluate(() => globalThis.reliabilityHoldHosts.clear());
  await refresh();
  await statusShows('Updated');

  // Another window: the change itself retires the 24h report, and a held then
  // failed 7d read never shows it again.
  await page.evaluate(() => globalThis.reliabilityHoldHosts.add('alpha'));
  await page.locator('#reliability-window-selector [data-window="7d"]').click();
  assertRetired(await view(), 'After a window change');
  await page.waitForFunction(() => globalThis.reliabilityHeld.some(entry => entry.label === '7d'));
  await statusShows('Loading');
  const windowPending = await view();
  assertRetired(windowPending, 'While the replacement window read is pending');
  evidence.windowPending = windowPending;
  await settle(503);
  await statusShows('Unable to load');
  const windowFailed = await view();
  assertRetired(windowFailed, 'After the replacement window read fails');
  evidence.windowFailure = windowFailed;
  await page.evaluate(() => globalThis.reliabilityHoldHosts.clear());
  await refresh();
  await reportNames('alpha-only-workspace');
  const alphaWeek = await view();
  if (alphaWeek.report['reliability-count'] !== '17 runs / 7d') {
    throw new Error(`The alpha 7d Reliability report must render once its read succeeds: ${JSON.stringify(alphaWeek)}`);
  }

  // Another host: the switch itself retires alpha's report, and a held then
  // failed beta read never shows it again.
  const switched = await page.evaluate(async () => {
    const common = await import('/js/common.js');
    globalThis.reliabilityHoldHosts.add('beta');
    common.setHost('beta');
    return { host: common.getHost(), ...globalThis.reliabilityView() };
  });
  if (switched.host !== 'beta') throw new Error(`The fixture must select host beta: ${JSON.stringify(switched)}`);
  assertRetired(switched, 'After a host switch');
  await refresh();
  await held(1);
  await statusShows('Loading');
  const pending = await view();
  assertRetired(pending, 'While the replacement host read is pending');
  evidence.hostPending = pending;
  await settle(503);
  await statusShows('Unable to load');
  const failed = await view();
  assertRetired(failed, 'After the replacement host read fails');
  if (!failed.status.includes('beta reliability unavailable')) {
    throw new Error(`The failed beta read must report its own failure: ${JSON.stringify(failed)}`);
  }
  evidence.hostFailure = failed;
  await page.evaluate(() => globalThis.reliabilityHoldHosts.clear());
  await refresh();
  await reportNames('beta-only-workspace');
  const beta = await view();
  if (beta.report['reliability-count'] !== '5 runs / 7d' || beta.badgeHidden || beta.report['reliability-breakdown'].includes('alpha')) {
    throw new Error(`The beta Reliability report must render once its read succeeds: ${JSON.stringify(beta)}`);
  }
  return evidence;
}
