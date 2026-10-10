// Render the shipped run-detail module in the loading browser harness.
import path from 'node:path';
import fs from 'node:fs';

// Drive the full app's actions and its actual scheduled polls. A new render
// must keep feedback reachable, and its rebuilt buttons must use that host.
export async function assertRunDetailActions(page, evidence) {
  let replayPostCount = 0;
  await page.exposeFunction('recordReplayPost', () => { replayPostCount += 1; });
  await page.evaluate(async () => {
    const { navigateToRun } = await import('/js/router.js');
    const { setWorkspace, describePullSettlements } = await import('/js/common.js');
    setWorkspace('one');
    const previousFetch = globalThis.fetch;
    const settlements = [{ outcome: 'settled' }, { outcome: 'pending_delivery' }, { outcome: 'pending_delivery' }];
    const fixture = globalThis.runActionFixture = {
      state: 'running', detailReads: 0, detailStatus: 200, cancelStatus: 200, requests: [],
      activeRuns: [],
      cancelError: 'Cancel fixture refused', replayError: 'Replay fixture refused',
      expectedSettlements: describePullSettlements(settlements).text,
      holdAction: false, release: null, previousFetch,
    };
    const response = (payload, status = 200) => ({
      ok: status === 200, status, json: async () => payload, text: async () => JSON.stringify(payload),
    });
    globalThis.fetch = async (input, options) => {
      const url = new URL(input, window.location.href);
      if (url.pathname === '/api/job-runs' && url.searchParams.get('state') === 'active') {
        fixture.requests.push({ action: 'active-runs', workspace: url.searchParams.get('workspace') });
        return response({ items: fixture.activeRuns, total: fixture.activeRuns.length, truncated: false, state: 'active' });
      }
      const match = url.pathname.match(/^\/api\/runs\/([^/]+)(?:\/(cancel|replay|events|logs))?$/);
      if (!match) return previousFetch(input, options);
      const [, runId, action] = match;
      fixture.requests.push({ runId, action: action || 'detail', workspace: url.searchParams.get('workspace') });
      if (action === 'events' || action === 'logs') return response([]);
      if (action === 'cancel' || action === 'replay') {
        if (action === 'replay') await window.recordReplayPost();
        if (fixture.holdAction) await new Promise(resolve => { fixture.release = resolve; });
        if (action === 'replay') return response({ error: fixture.replayError }, 503);
        if (fixture.cancelStatus !== 200) return response({ error: fixture.cancelError }, fixture.cancelStatus);
        fixture.state = 'cancelled';
        return response({ outcome: 'cancelled', pull_settlements: settlements });
      }
      fixture.detailReads += 1;
      if (fixture.detailStatus !== 200) return response({ error: 'Detail fixture unavailable' }, fixture.detailStatus);
      return response({ run: {
        run_id: runId, job_id: 'fixture', state: fixture.state, attempt: fixture.detailReads,
        task_ids: ['ORB-42'], tasks: [{ id: 'ORB-42', title: 'Fixture task' }],
      }, steps: [] });
    };
    navigateToRun('jrun-action-feedback');
    // The loading scenarios parked the scheduler on a captured timer. Restore
    // its ordinary browser cadence through the production visibility handler.
    for (const hidden of [true, false]) {
      Object.defineProperty(document, 'hidden', { configurable: true, value: hidden });
      document.dispatchEvent(new Event('visibilitychange'));
    }
  });
  const notice = page.locator('#run-detail-meta .run-cancel-notice');
  const error = page.locator('#run-detail-meta .action-error[role="alert"]');
  const cancel = page.locator('#run-detail-meta .run-cancel');
  const replay = page.locator('#run-detail-meta .run-replay');
  const refresh = async () => {
    const before = await page.evaluate(() => globalThis.runActionFixture.detailReads);
    await page.locator('#refresh-btn').click();
    await page.waitForFunction(before => globalThis.runActionFixture.detailReads > before, before);
  };
  const poll = async () => {
    const before = await page.evaluate(() => globalThis.runActionFixture.detailReads);
    // The production router uses a 30-second one-shot timer after each poll.
    await page.waitForFunction(before => globalThis.runActionFixture.detailReads > before, before, { timeout: 45000 });
  };
  const clickCancel = async () => {
    page.once('dialog', dialog => dialog.accept());
    await cancel.click();
  };
  const clickReplay = async (expectedLiveRunId = null) => {
    const replayPostsBefore = replayPostCount;
    let prompt = null;
    let replayPostsAtPrompt = null;
    page.once('dialog', async dialog => {
      prompt = dialog.message();
      replayPostsAtPrompt = replayPostCount;
      await dialog.accept();
    });
    await replay.click();
    if (!prompt || !prompt.includes('fixture') || !prompt.includes('ORB-42')) {
      throw new Error(`Replay confirmation must identify its job and task: ${prompt || 'no dialog'}`);
    }
    if (replayPostsAtPrompt !== replayPostsBefore) throw new Error('Replay POST must wait until after confirmation');
    if (expectedLiveRunId && !prompt.includes(expectedLiveRunId)) throw new Error('Replay confirmation must name the existing live run for the task');
  };
  const expectError = async message => {
    await error.waitFor({ state: 'visible' });
    if (!(await error.textContent()).includes(message)) throw new Error(`Missing action error: ${message}`);
  };
  try {
    await cancel.waitFor({ state: 'visible' });
    await clickCancel();
    await notice.waitFor({ state: 'visible', timeout: 5000 });
    await page.waitForFunction(async () => (await import('/js/run-detail.js')).getActiveRunDetail()?.run.state === 'cancelled');
    const expected = await page.evaluate(() => globalThis.runActionFixture.expectedSettlements);
    if (!(await notice.textContent()).includes(expected) || !await notice.evaluate(node => node.classList.contains('error'))) {
      throw new Error('Cancel settlement report must show the delivered and undelivered counts after the refresh');
    }
    await poll();
    if (!(await notice.isVisible()) || !(await notice.textContent()).includes(expected)) throw new Error('Scheduled poll erased the cancel settlement report');
    await page.screenshot({ path: path.join(evidence, 'run-cancel-settlement.png'), fullPage: true });
    await notice.locator('button').click();
    await refresh();
    if (await notice.count()) throw new Error('Dismissed settlement report returned after a refresh');

    await page.evaluate(() => Object.assign(globalThis.runActionFixture, { state: 'running', cancelStatus: 503 }));
    await refresh();
    await clickCancel();
    await expectError('Cancel fixture refused');
    await poll();
    await expectError('Cancel fixture refused');
    await error.locator('button').click();
    await refresh();
    if (await error.count()) throw new Error('Dismissed cancel error returned after a refresh');
    await clickCancel();
    await expectError('Cancel fixture refused');
    await page.evaluate(() => { globalThis.runActionFixture.holdAction = true; });
    await clickCancel();
    await page.waitForFunction(() => typeof globalThis.runActionFixture.release === 'function');
    if (await error.count()) throw new Error('Starting another cancel must clear the previous error');
    // A render during the request must not detach its feedback host either.
    await refresh();
    await page.evaluate(() => {
      globalThis.runActionFixture.holdAction = false;
      globalThis.runActionFixture.release();
      globalThis.runActionFixture.release = null;
    });
    await expectError('Cancel fixture refused');

    await page.evaluate(async () => (await import('/js/router.js')).navigateToRun('jrun-replay-feedback'));
    await page.waitForFunction(() => document.getElementById('run-detail-title').textContent === 'Run jrun-replay-feedback');
    if (await error.count() || await notice.count()) throw new Error('Action feedback leaked into a different run');
    await page.evaluate(() => { globalThis.runActionFixture.state = 'failed'; });
    await refresh();
    await clickReplay();
    await expectError('Replay fixture refused');
    await poll();
    await expectError('Replay fixture refused');
    await page.evaluate(() => {
      globalThis.runActionFixture.activeRuns = [{ run_id: 'jrun-live-task', state: 'running', task_ids: ['ORB-42'] }];
    });
    await clickReplay('jrun-live-task');
    await expectError('Replay fixture refused');
    await page.evaluate(() => { globalThis.runActionFixture.activeRuns = []; });
    await page.evaluate(() => { globalThis.runActionFixture.detailStatus = 503; });
    await refresh();
    await page.locator('#run-detail-meta .empty-state').waitFor({ state: 'visible' });
    await expectError('Replay fixture refused');
    await page.evaluate(() => { globalThis.runActionFixture.detailStatus = 200; });
    await refresh();
    await expectError('Replay fixture refused');
    await page.screenshot({ path: path.join(evidence, 'run-replay-error.png'), fullPage: true });
    await error.locator('button').click();
    await refresh();
    if (await error.count()) throw new Error('Dismissed replay error returned after a refresh');
    await clickReplay();
    await expectError('Replay fixture refused');
    await page.evaluate(() => { globalThis.runActionFixture.holdAction = true; });
    await clickReplay();
    await page.waitForFunction(() => typeof globalThis.runActionFixture.release === 'function');
    if (await error.count()) throw new Error('Starting another replay must clear the previous error');
    await page.evaluate(async () => {
      const { setWorkspace } = await import('/js/common.js');
      setWorkspace('two');
      globalThis.runActionFixture.holdAction = false;
      globalThis.runActionFixture.release();
      globalThis.runActionFixture.release = null;
    });
    await refresh();
    if (await error.count() || await notice.count()) throw new Error('Action feedback leaked into a different workspace');
    fs.writeFileSync(path.join(evidence, 'run-actions-result.json'), JSON.stringify({
      passed: true, scheduledPolls: 3,
      scenarios: ['settlement counts after cancel refresh and poll', 'cancel error after poll', 'replay error after poll',
        'replay confirmation before POST', 'live task run warning', 'dismissal and next-action clearing',
        'render during action', 'failed detail refresh and recovery', 'run and workspace isolation'],
    }, null, 2));
  } catch (failure) {
    fs.writeFileSync(path.join(evidence, 'run-actions-failure.json'), JSON.stringify(await page.evaluate(() => ({
      ...globalThis.runActionFixture,
      meta: document.getElementById('run-detail-meta').outerHTML,
      title: document.getElementById('run-detail-title').textContent,
    })), null, 2));
    await page.screenshot({ path: path.join(evidence, 'run-actions-failure.png'), fullPage: true });
    throw failure;
  } finally {
    await page.evaluate(async () => {
      const fixture = globalThis.runActionFixture;
      fixture.release?.();
      globalThis.fetch = fixture.previousFetch;
      delete globalThis.runActionFixture;
      (await import('/js/common.js')).setWorkspace('one');
      (await import('/js/router.js')).setActiveTab('tasks');
    });
  }
}

export async function assertRunDetailPresentation(page, evidence) {
  await page.setViewportSize({ width: 1440, height: 1100 });
  const render = async (run, steps, logs = []) => page.evaluate(async ({ run, steps, logs }) => {
    const detail = await import('/js/run-detail.js');
    for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.dataset.tab === 'run-detail');
    document.getElementById('run-steps-body').style.display = '';
    detail.clearExpandedStepIndices();
    detail.setActiveRunDetail({ run: { run_id: 'jrun-presentation', ...run }, steps });
    detail.setActiveRunLogs(logs);
    detail.setActiveRunEvents([]);
    detail.renderRunDetailMeta();
    detail.renderRunSteps();
    detail.renderRunKnowledge();
    detail.renderRunGantt();
  }, { run, steps, logs });
  const waitingRun = {
    state: 'success', workspace_id: 'fixture-workspace',
    env_pass_unset: ['CLAUDE_CODE_OAUTH_TOKEN'],
    drain_last_pass: {
      queued: 7,
      deferred: [{ task_id: 'ORB-15181', reason: 'context_lock_conflict', blocked_by: ['ORB-15183'] }],
      deferred_total: 1,
      excluded: [
        { task_id: 'ORB-15196', reason: 'active_pilot_preparation' },
        { task_id: 'ORB-15071', reason: 'host_os_mismatch', detail: 'waits for a macos host (os:macos)' },
      ],
      excluded_total: 2,
    },
  };
  await render(waitingRun, []);
  const unsetNotice = page.locator('#run-detail-meta .child-dispatch-notice').first();
  const unsetText = await unsetNotice.textContent();
  if (!unsetText.includes('CLAUDE_CODE_OAUTH_TOKEN') || !unsetText.includes('execution.env.pass')) {
    throw new Error(`Unset environment notice must preserve config and environment-name case: ${unsetText}`);
  }
  if (await unsetNotice.evaluate(node => getComputedStyle(node).textTransform) === 'uppercase') {
    throw new Error('Run-detail notices must preserve the case of config keys and environment names');
  }
  const waitingNotice = page.locator('#run-detail-meta .still-waiting .child-dispatch-notice');
  const waitingText = await waitingNotice.textContent();
  if (!waitingText.includes('3 backlog tasks have recorded wait reasons (1 deferred, 2 excluded)')
    || !waitingText.includes('6 additional admissible tasks were not started and are not listed below')) {
    throw new Error(`Still-waiting summary must count reason rows and explain unlisted admissible tasks: ${waitingText}`);
  }
  if (await waitingNotice.evaluate(node => getComputedStyle(node).textTransform) === 'uppercase') {
    throw new Error('Still-waiting notice must preserve normal sentence casing');
  }
  const waitingRows = page.locator('#run-detail-meta .still-waiting .waiting-task');
  if (await waitingRows.count() !== 3) throw new Error('Still-waiting reason totals must match the rows shown');
  const expectedReasons = await page.evaluate(async tasks => {
    const { drainWaitBadge } = await import('/js/drain-waits.js');
    return tasks.map(task => `: ${drainWaitBadge(task).text}`);
  }, [...waitingRun.drain_last_pass.deferred, ...waitingRun.drain_last_pass.excluded]);
  const renderedReasons = await waitingRows.locator('.waiting-task-reason').allTextContents();
  if (JSON.stringify(renderedReasons) !== JSON.stringify(expectedReasons)) {
    throw new Error(`Run-detail waits must use the Drain card's human reason labels: ${JSON.stringify(renderedReasons)}`);
  }

  const waitingLinks = await page.locator('#run-detail-meta .still-waiting .waiting-task-link').evaluateAll(nodes => nodes.map(node => ({
    id: node.textContent.trim(), href: node.href,
  })));
  const expectedTaskIds = ['ORB-15181', 'ORB-15183', 'ORB-15196', 'ORB-15071'];
  const linksMatchTasks = waitingLinks.every(link => {
    const url = new URL(link.href);
    const [route, query] = url.hash.slice(1).split('?');
    const params = new URLSearchParams(query);
    return url.searchParams.get('workspace') === 'fixture-workspace'
      && route === 'tasks' && params.get('status') === 'all' && params.get('q') === link.id;
  });
  if (JSON.stringify(waitingLinks.map(link => link.id)) !== JSON.stringify(expectedTaskIds) || !linksMatchTasks) {
    throw new Error(`Every waiting task and blocker ID must link to its task: ${JSON.stringify(waitingLinks)}`);
  }

  const localDeferred = ['ORB-15208', 'ORB-15209', 'ORB-15210'].map(task_id => ({
    task_id, reason: 'context_lock_conflict', blocked_by: ['ORB-15183'],
  }));
  await render({
    state: 'running', job_id: 'workspace_auto_pipeline',
    drain_last_pass: { queued: 3, deferred: localDeferred, deferred_total: 3 },
  }, []);
  const localWaitingText = await page.locator('#run-detail-meta .still-waiting .child-dispatch-notice').textContent();
  if (!localWaitingText.includes('3 backlog tasks have recorded wait reasons (3 deferred, 0 excluded)')
    || localWaitingText.includes('6 additional admissible tasks')) {
    throw new Error(`Local deferred tasks must be counted once in the run-detail banner: ${localWaitingText}`);
  }

  const childDispatches = ['success', 'cancelled', 'failed', null, 'running', 'success'].map((state, index) => ({
    child_run_id: `jrun-child-${index}`, job_name: 'task_auto_pipeline',
    phase: 'submitted', child_status: 'running', parent_step_id: 'leaf_invoke', state,
    started_at: state ? new Date(Date.now() - 120000).toISOString() : null,
    duration_ms: state && state !== 'running' ? 125000 : null,
  }));
  await render({ state: 'success', child_dispatches: childDispatches }, []);
  const summary = page.locator('.child-dispatch-summary');
  const tally = await summary.textContent();
  for (const outcome of ['6 admitted', '2 succeeded', '1 failed', '1 cancelled', '1 running', '1 unknown']) {
    if (!tally.includes(outcome)) throw new Error(`Child tally lost ${outcome}: ${tally}`);
  }
  const rows = page.locator('.child-dispatch-row');
  const childStates = await rows.evaluateAll(nodes => nodes.map(node => node.dataset.state));
  if (JSON.stringify(childStates) !== JSON.stringify(['running', 'failed', 'success', 'success', 'cancelled', 'unknown'])) {
    throw new Error(`Children must group current outcomes rather than dispatch checkpoints: ${JSON.stringify(childStates)}`);
  }
  if (!(await rows.locator('.state-label').allTextContents()).every((state, index) => state === childStates[index])) {
    throw new Error('Child state labels must match their state dots');
  }
  const childColors = {};
  for (const state of ['failed', 'cancelled']) {
    childColors[state] = await page.locator(`.child-dispatch-row[data-state="${state}"] .state-label`).evaluate(node => ({
      dot: getComputedStyle(node, '::before').backgroundColor, text: getComputedStyle(node).color,
    }));
    if (childColors[state].dot !== childColors[state].text) throw new Error(`Child ${state} dot and outcome must use their state color`);
    if (await page.locator(`.child-dispatch-row[data-state="${state}"] .duration`).textContent() !== '2m 5s') throw new Error(`Child ${state} must show its final duration`);
  }
  if (childColors.failed.dot === childColors.cancelled.dot) throw new Error('Failed and cancelled children must be visually distinct');
  if (!(await page.locator('.child-dispatch-row[data-state="running"] .duration').textContent()).endsWith('↻')) throw new Error('Running child must show live elapsed duration');
  if (await page.locator('.child-dispatch-row[data-state="unknown"] .duration').textContent() !== '-') throw new Error('Unreadable child timing must be unavailable');
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: 1100 });
    if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw new Error(`Child outcomes must fit at ${width}px`);
    await page.screenshot({ path: path.join(evidence, `run-child-outcomes-${width}.png`), fullPage: true, animations: 'disabled' });
  }
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.evaluate(() => {
    globalThis.childOutcomePreviousFetch = globalThis.fetch;
    globalThis.fetch = async (input, options) => {
      const url = new URL(input, window.location.href);
      const match = url.pathname.match(/^\/api\/runs\/(jrun-child-\d+)(?:\/(events|logs))?$/);
      if (!match) return globalThis.childOutcomePreviousFetch(input, options);
      return new Response(JSON.stringify(match[2] ? [] : { run: { run_id: match[1], state: match[1] === 'jrun-child-1' ? 'cancelled' : 'failed' }, steps: [] }), { status: 200 });
    };
  });
  try {
    for (const [state, index] of [['failed', 2], ['cancelled', 1]]) {
      await render({ state: 'success', child_dispatches: childDispatches }, []);
      const link = page.locator(`.child-dispatch-row[data-state="${state}"] .back-action`);
      await link.focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(id => document.getElementById('run-detail-title').textContent === `Run ${id}`, `jrun-child-${index}`);
    }
  } finally {
    await page.evaluate(() => {
      globalThis.fetch = globalThis.childOutcomePreviousFetch;
      delete globalThis.childOutcomePreviousFetch;
    });
  }
  // A refreshed response changes both the outcome and its tally even when
  // the parent is terminal and the dispatch checkpoint remains submitted.
  await render({ state: 'success', child_dispatches: childDispatches.map(d => d.state === 'running' ? { ...d, state: 'success', duration_ms: 126000 } : d) }, []);
  if (await page.locator('.child-dispatch-row[data-state="running"]').count()
    || !(await summary.textContent()).includes('3 succeeded')) throw new Error('Refreshing child state must refresh the outcome tally');
  fs.writeFileSync(path.join(evidence, 'run-child-outcomes-result.json'), JSON.stringify({ passed: true, childStates, childColors, tally, navigation: ['failed', 'cancelled'], widths: [1440, 390], refreshedTally: await summary.textContent() }, null, 2));
  const step = {
    step_index: 0, target_type: 'activity', target_id: 'agent_implement', state: 'failed',
    duration_ms: 125000, exit_code: 1, started_at: '2026-10-07T05:00:00Z', finished_at: '2026-10-07T05:02:05Z',
  };
  const holdReason = 'Delivery awaits named external evidence; receipt queues a fresh review.';
  await render({ state: 'held', error_message: holdReason }, [{ ...step, state: 'held' }]);
  const hold = page.locator('.run-hold');
  if (!(await hold.isVisible()) || !(await hold.textContent()).includes(holdReason) || !(await hold.locator('p').textContent())) {
    throw new Error('Held run must show the stored reason and what clears the hold');
  }
  const heldColor = await page.locator('#run-detail-meta [data-state="held"]').evaluate(node => {
    const style = getComputedStyle(node);
    return { dot: style.getPropertyValue('--dot').trim(), held: style.getPropertyValue('--state-held').trim(), grey: style.getPropertyValue('--fg-dim').trim() };
  });
  if (!heldColor.held || heldColor.dot !== heldColor.held || heldColor.dot === heldColor.grey) throw new Error(`Held state needs its own color: ${JSON.stringify(heldColor)}`);
  await page.screenshot({ path: path.join(evidence, 'run-held-1440.png'), fullPage: true });

  await render({ state: 'failed', error_message: 'Agent exited with status 1' }, [step]);
  if (!(await page.locator('.run-failure-head').textContent()).includes('Failed at step 1 of 1')
    || await page.locator('#run-detail-count').textContent() !== '1 step'
    || await page.locator('.step-row .idx').textContent() !== '#1') {
    throw new Error('A failed single-step run must display one-based, singular step labels');
  }
  if (await page.locator('.step-header > span').count() !== 5 || !(await page.locator('.step-header .exit').textContent())) throw new Error('Step columns must identify the exit code');
  await page.locator('.step-row').click();
  if (!(await page.locator('.step-logs-empty').isVisible())) throw new Error('Empty step expansion must explain that it has no logs');
  await page.screenshot({ path: path.join(evidence, 'run-failed-empty-1440.png'), fullPage: true });
  await page.evaluate(async () => {
    const detail = await import('/js/run-detail.js');
    detail.setActiveRunLogsError('Log service unavailable');
    detail.renderRunSteps();
  });
  if (!(await page.locator('.step-logs-empty').textContent()).includes('unavailable')) throw new Error('A log fetch failure must replace stale no-logs feedback in an expanded step');
  const logAlert = page.locator('#run-steps-body .action-error');
  if (!(await logAlert.isVisible()) || await logAlert.getAttribute('role') !== 'alert') throw new Error('Run log fetch failures must render an accessible alert');
  await page.evaluate(async () => {
    const detail = await import('/js/run-detail.js');
    detail.setActiveRunLogs([{ step_index: 0, provider: 'claude' }]);
    detail.renderRunSteps();
  });
  if (!(await page.locator('.step-logs-empty').isVisible()) || await page.locator('.step-log-section').count()) throw new Error('A metadata-only log record must still show empty-log feedback');

  for (const knowledge_metrics of [undefined, null, {}]) {
    await render({ state: 'success', knowledge_metrics }, []);
    if (await page.locator('#run-knowledge-panel').isVisible() || await page.locator('#run-knowledge-panel').textContent()) {
      throw new Error('Absent or empty knowledge metrics must remove the Knowledge Pack content');
    }
  }
  await render({ state: 'success', knowledge_metrics: { raw_read_token_baseline: 0, knowledge_pack_tokens: 0 } }, []);
  if (!(await page.locator('#run-knowledge-panel').isVisible())) throw new Error('Recorded zero-valued knowledge metrics must remain visible');
  await render({ state: 'success' }, []);
  if (await page.locator('#run-knowledge-panel').isVisible()) throw new Error('Changing from recorded to absent metrics must hide stale knowledge');

  for (const state of ['pending', 'running', 'retrying', 'unknown']) {
    await render({ state }, [step]);
    const replay = page.locator('#run-detail-meta .run-replay');
    if (await replay.isEnabled() || !(await replay.getAttribute('title')).includes('finishes')) {
      throw new Error(`Replay must be disabled with a reason while the run is ${state}`);
    }
    if (state === 'running') await page.screenshot({ path: path.join(evidence, 'run-active-1440.png'), fullPage: true });
  }
  for (const state of ['success', 'failed', 'timeout', 'cancelled', 'interrupted', 'held']) {
    await render({ state }, []);
    if (!(await page.locator('#run-detail-meta .run-replay').isEnabled())) throw new Error(`Terminal ${state} run must retain replay`);
  }
  const actionLayout = await page.locator('#run-detail-meta .run-replay').evaluate(replay => {
    const back = document.querySelector('#run-detail-meta .run-detail-actions .back-action').getBoundingClientRect();
    const button = replay.getBoundingClientRect();
    return { backRight: back.right, replayLeft: button.left, primary: replay.classList.contains('approve') };
  });
  if (actionLayout.primary || actionLayout.replayLeft <= actionLayout.backRight + 8) {
    throw new Error(`Replay must be secondary and visually separated from Runs: ${JSON.stringify(actionLayout)}`);
  }

  const states = ['success', 'running', 'pending', 'failed', 'skipped', 'held'];
  await render({ state: 'success' }, states.map((state, step_index) => ({ ...step, state, step_index })));
  if (await page.locator('#run-detail-count').textContent() !== '6 steps') throw new Error('Multiple steps must use a plural count');
  for (const state of states) {
    const key = page.locator(`.gantt-legend [data-state="${state}"]`);
    const bar = page.locator(`.gantt-bar[data-state="${state}"]`);
    if (!(await key.isVisible()) || await key.evaluate(node => getComputedStyle(node).getPropertyValue('--dot'))
      !== await bar.evaluate(node => getComputedStyle(node).getPropertyValue('--dot'))) throw new Error(`Timeline legend must match the ${state} bar`);
  }
  await page.screenshot({ path: path.join(evidence, 'run-timeline-1440.png'), fullPage: true });

  const resultText = 'Rendered result with a long uninterrupted token: ' + 'abcdef'.repeat(400);
  const frame = { type: 'result', result: resultText, total_cost_usd: 0.12, usage: { input_tokens: 500, output_tokens: 100 }, stop_reason: 'tool_use', duration_api_ms: 38101 };
  const plain = '<img src=x onerror="window.logHtmlExecuted=true">';
  const truncated = '{"type":"result","result":"incomplete';
  const preview = [JSON.stringify(frame), plain, truncated].join('\n');
  await render({ state: 'success' }, [{ ...step, state: 'success', exit_code: 0 }], [{ step_index: 0, provider: 'claude', exit_code: 0, stdout_preview: preview, stderr_preview: 'diagnostic stderr' }]);
  await page.locator('.step-row').click();
  const pre = page.locator('.step-log-block.stdout pre');
  const content = await pre.textContent();
  if (!content.includes(resultText) || !content.includes('"total_cost_usd": 0.12') || !content.includes('"input_tokens": 500')
    || !content.includes(plain) || !content.includes(truncated) || await page.locator('.step-log-block img').count()) {
    throw new Error('JSON stdout must be formatted safely with result, cost, tokens and unrecognized lines preserved');
  }
  const layout = await pre.evaluate(node => {
    const rect = node.getBoundingClientRect();
    const panel = document.getElementById('run-detail-panel').getBoundingClientRect();
    return { scrollWidth: node.scrollWidth, clientWidth: node.clientWidth, left: rect.left, right: rect.right, panelRight: panel.right, viewport: window.innerWidth, whiteSpace: getComputedStyle(node).whiteSpace,
      lineDisplay: getComputedStyle(node.querySelector('.step-log-line')).display };
  });
  if (layout.scrollWidth > layout.clientWidth + 1 || layout.left < 0 || layout.right > Math.min(layout.panelRight, layout.viewport) + 1 || layout.whiteSpace !== 'pre-wrap' || layout.lineDisplay !== 'block') {
    throw new Error(`Wrapped JSON stdout must stay within the panel at 1440px: ${JSON.stringify(layout)}`);
  }
  await pre.scrollIntoViewIfNeeded();
  await page.screenshot({ path: path.join(evidence, 'run-stdout-1440.png'), fullPage: true });
  const toggle = page.locator('.step-log-block.stdout .log-wrap-toggle');
  await toggle.click();
  if (await toggle.getAttribute('aria-pressed') !== 'false' || !(await pre.evaluate(node => getComputedStyle(node).whiteSpace === 'pre' && getComputedStyle(node).overflowX === 'auto' && node.scrollWidth > node.clientWidth))) {
    throw new Error('Turning wrapping off must allow horizontal log scrolling without widening the page');
  }
  await toggle.click();
  if (await toggle.getAttribute('aria-pressed') !== 'true' || !(await pre.evaluate(node => node.scrollWidth <= node.clientWidth + 1))) throw new Error('Turning wrapping back on must restore readable stdout');
  if (await page.locator('.step-log-block.stderr .log-wrap-toggle').getAttribute('aria-pressed') !== 'true') throw new Error('Wrap toggles must act only on their own stream');
  if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth || window.logHtmlExecuted)) throw new Error('Log display must neither widen the page nor execute HTML');
  fs.writeFileSync(path.join(evidence, 'run-detail-result.json'), JSON.stringify({ passed: true, viewport: 1440, heldColor, stdoutLayout: layout, scenarios: ['held reason and color', 'one-based single step', 'empty logs', 'missing and zero knowledge metrics', 'active and terminal replay', 'timeline legend', 'mixed JSON and malformed stdout', 'independent wrap toggles', 'safe text rendering'] }, null, 2));
}
