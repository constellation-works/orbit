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
      await page.goto(`${origin}/#audit?role=operator`);
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
          const counts = { total: runs, succeeded: runs - 2, failed: 2, cancelled: 0, skipped: 0, in_flight: 0, unknown: 0 };
          const rate = { numerator: 2, denominator: runs, denominator_label: 'settled runs', value: 2 / runs, low_sample: true };
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
        window.confirm = () => true;
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
          if (pathname === '/api/tasks' || pathname === '/api/tasks/all') return respond({ items: [task], total: 1 });
          if (pathname === `/api/tasks/${task.id}`) return respond(task);
          if (pathname === '/api/audit') return respond([]);
          if (pathname === '/api/crews') return respond({ crews: [] });
          if (pathname === '/api/routines') return respond({
            machine_name: 'fixture', capabilities: { routine_toggle: allowed }, clock: {},
            routines: [{ name: 'scope-routine', source: 'one', target: 'job:fixture', enabled: routineEnabled }],
          });
          if (pathname === '/api/workflows/auto/readiness') return respond({
            controls_authorized: true, tasks: [],
            capacity: { active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4,
              drain_run_id: drainRunId, drain_phase: drainRunId ? 'draining' : 'idle' },
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
        await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('audit?role=operator'));
        await chip.waitFor({ state: 'visible' });
      }
      await page.locator('#audit-scope-chips [data-chip="actor"]').click();
      await page.locator('#audit-scope-chips [data-chip="actor"]').waitFor({ state: 'detached' });
      if (!(await chip.isVisible())) throw new Error('Removing an Audit filter must retain the workspace chip');

      await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('operations/routines'));
      const routine = page.locator('#routines-body .operation-switch');
      await routine.waitFor({ state: 'visible' });
      if (!(await routine.isEnabled())) throw new Error('Operations must remain enabled after chip interaction');
      await routine.click();
      await page.locator('#routine-operation-feedback.success').waitFor({ state: 'visible' });
      if (await page.locator('#routines-body .operations-readonly-note').count()) throw new Error('Single-workspace actions must not show an aggregate read-only note');

      await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('tasks'));
      await page.locator('#dock-mode-toggle [data-mode="drain"]').click();
      const start = page.locator('#auto-drain-start-body .drain-start');
      const stop = page.locator('#auto-drain-body .drain-stop');
      await start.waitFor({ state: 'visible' });
      if (!(await start.isEnabled()) || !(await stop.isEnabled())) throw new Error('Drain controls must remain enabled after chip interaction');
      await start.click();
      await page.locator('#auto-drain-operation-feedback.success').waitFor({ state: 'visible' });
      await stop.click();
      await page.locator('#auto-drain-operation-feedback.success').waitFor({ state: 'visible' });

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
        await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('diagnostics/incidents'));
        const railPositions = async () => page.locator('.rail .tab, .rail .subtab').evaluateAll(nodes =>
          nodes.map(node => ({ route: node.dataset.tab || node.dataset.subtab, top: node.getBoundingClientRect().top })));
        const initialRailPositions = await railPositions();
        const note = page.locator('#workspace-scope-note');

        await page.locator('#diag-subtabs [data-subtab="reliability"]').click();
        await note.waitFor({ state: 'visible' });
        if (!(await note.textContent()).includes('Workspace filter inactive')
          || await note.getAttribute('aria-hidden') !== 'false') {
          throw new Error('Reliability must expose the workspace-filter note visually and to assistive technology');
        }
        const reliabilityPositions = await railPositions();

        await page.locator('#diag-subtabs [data-subtab="scoreboard"]').click();
        await page.locator('#diag-subtabs [data-subtab="scoreboard"].active').waitFor({ state: 'visible' });
        await note.waitFor({ state: 'hidden' });
        if (await note.getAttribute('aria-hidden') !== 'true') {
          throw new Error('The workspace-filter note must be hidden from assistive technology outside Reliability');
        }
        const scoreboardPositions = await railPositions();
        for (const [label, positions] of [['Reliability', reliabilityPositions], ['Scoreboard', scoreboardPositions]]) {
          if (positions.length !== initialRailPositions.length || positions.some((entry, index) =>
            entry.route !== initialRailPositions[index].route || Math.abs(entry.top - initialRailPositions[index].top) > 0.1)) {
            throw new Error(`Opening ${label} must not move dashboard rail entries: ${JSON.stringify({ initialRailPositions, positions })}`);
          }
        }
        reliabilityEvidence = await assertReliabilityScope(page);
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

  await page.locator('#diag-subtabs [data-subtab="reliability"]').click();
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
