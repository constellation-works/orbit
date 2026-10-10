import fs from 'node:fs';
import path from 'node:path';

// Drive scope chips and their downstream actions through the full shipped app.
export async function assertWorkspaceScope(browser, origin, evidence) {
  for (const workspaceCount of [1, 2]) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
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
      const start = page.locator('#auto-drain-body .drain-start');
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
      }
      if (errors.length) throw new Error(errors.join('\n'));
      fs.writeFileSync(path.join(evidence, `workspace-scope-${workspaceCount}.json`), JSON.stringify({ passed: true, workspaceCount, requests }, null, 2));
    } catch (error) {
      await page.screenshot({ path: path.join(evidence, `workspace-scope-${workspaceCount}-failure.png`), fullPage: true });
      throw error;
    } finally {
      await page.close();
    }
  }
  console.log('Single-workspace scope preservation and multi-workspace chip clearing passed.');
}
