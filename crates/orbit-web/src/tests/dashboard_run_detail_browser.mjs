// Render the shipped run-detail module in the loading browser harness.
import path from 'node:path';
import fs from 'node:fs';

// Drive the full app's actions and its actual scheduled polls. A new render
// must keep feedback reachable, and its rebuilt buttons must use that host.
export async function assertRunDetailActions(page, evidence) {
  await page.evaluate(async () => {
    const { navigateToRun } = await import('/js/router.js');
    const { setWorkspace, describePullSettlements } = await import('/js/common.js');
    setWorkspace('one');
    const previousFetch = globalThis.fetch;
    const settlements = [{ outcome: 'settled' }, { outcome: 'pending_delivery' }, { outcome: 'pending_delivery' }];
    const fixture = globalThis.runActionFixture = {
      state: 'running', detailReads: 0, detailStatus: 200, cancelStatus: 200, requests: [],
      cancelError: 'Cancel fixture refused', replayError: 'Replay fixture refused',
      expectedSettlements: describePullSettlements(settlements).text,
      holdAction: false, release: null, previousFetch,
    };
    const response = (payload, status = 200) => ({
      ok: status === 200, status, json: async () => payload, text: async () => JSON.stringify(payload),
    });
    globalThis.fetch = async (input, options) => {
      const url = new URL(input, window.location.href);
      const match = url.pathname.match(/^\/api\/runs\/([^/]+)(?:\/(cancel|replay|events|logs))?$/);
      if (!match) return previousFetch(input, options);
      const [, runId, action] = match;
      fixture.requests.push({ runId, action: action || 'detail', workspace: url.searchParams.get('workspace') });
      if (action === 'events' || action === 'logs') return response([]);
      if (action === 'cancel' || action === 'replay') {
        if (fixture.holdAction) await new Promise(resolve => { fixture.release = resolve; });
        if (action === 'replay') return response({ error: fixture.replayError }, 503);
        if (fixture.cancelStatus !== 200) return response({ error: fixture.cancelError }, fixture.cancelStatus);
        fixture.state = 'cancelled';
        return response({ outcome: 'cancelled', pull_settlements: settlements });
      }
      fixture.detailReads += 1;
      if (fixture.detailStatus !== 200) return response({ error: 'Detail fixture unavailable' }, fixture.detailStatus);
      return response({ run: { run_id: runId, job_id: 'fixture', state: fixture.state, attempt: fixture.detailReads }, steps: [] });
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
    await replay.click();
    await expectError('Replay fixture refused');
    await poll();
    await expectError('Replay fixture refused');
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
    await replay.click();
    await expectError('Replay fixture refused');
    await page.evaluate(() => { globalThis.runActionFixture.holdAction = true; });
    await replay.click();
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
        'dismissal and next-action clearing', 'render during action', 'failed detail refresh and recovery', 'run and workspace isolation'],
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
