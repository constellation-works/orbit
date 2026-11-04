import assert from 'node:assert/strict';
import path from 'node:path';

export async function checkDecisionNoteRefresh({ browser, origin, evidence }) {
  for (const id of ['BLOCK-4', 'PILOT-1', 'BLOCK-3']) {
    for (const mode of ['normal', 'pinned', 'page']) {
      const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      page.on('dialog', dialog => dialog.accept());
      try {
        const hash = mode === 'page' ? `#tasks/${id}` : `#tasks?status=${mode === 'pinned' ? 'none' : 'all'}&open=${id}`;
        await page.goto(origin + hash);
        const note = page.locator('.decision-note');
        await note.waitFor({ state: 'visible' });
        const draft = `Evidence being written for ${id} in ${mode}`;
        await note.fill(draft);
        await page.evaluate(async id => {
          const { taskState, rerender } = await import('/js/task-shared.js');
          const { applyUpdatedTask } = await import('/js/task-data.js');
          const task = blockFixture.tasks.find(task => task.id === id);
          task.comments.push({ by: 'operator', at: '2026-11-01T00:00:00Z', message: 'Incoming evidence' });
          applyUpdatedTask(structuredClone(task), taskState.paneContext);
          rerender(taskState.paneContext);
        }, id);
        if (id === 'BLOCK-4' && mode === 'normal') {
          for (const width of [1440, 375]) {
            await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
            await note.scrollIntoViewIfNeeded();
            await page.screenshot({ path: path.join(evidence, `decision-note-update-dark-${width}.png`) });
          }
          await page.setViewportSize({ width: 1440, height: 900 });
          await note.focus();
        }
        assert.equal(await note.inputValue(), draft, `${id}/${mode}: incoming task/comment updates preserve unsent decision evidence`);
        await page.evaluate(async id => {
          const { taskState, rerender } = await import('/js/task-shared.js');
          const { cacheCrewPayload } = await import('/js/task-crew.js');
          cacheCrewPayload({ default_crew: 'sol', crews: [{ name: 'sol' }, { name: 'other' }] });
          taskState.crewFeedback.set(id, { kind: 'success', text: 'Crew refreshed' });
          rerender(taskState.paneContext);
        }, id);
        assert.equal(await note.inputValue(), draft, `${id}/${mode}: feedback and crew refreshes preserve notes`);
        assert.equal(await page.evaluate(() => document.activeElement?.classList.contains('decision-note')), true, 'Refresh restores the focused decision input');
        if (mode === 'page') {
          for (const width of [1440, 375]) {
            await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
            await note.scrollIntoViewIfNeeded();
            await page.screenshot({ path: path.join(evidence, `decision-note-${id.toLowerCase()}-dark-${width}.png`) });
          }
        }
        await page.evaluate(() => { blockFixture.refuseAnswer = true; });
        await page.locator('.decision-answer').first().click();
        await page.locator('.task-detail .action-error').waitFor({ state: 'visible' });
        assert.equal(await note.inputValue(), draft, 'Refused answers keep the note');
        await page.evaluate(() => { blockFixture.refuseAnswer = false; });
        await page.locator('.decision-answer').first().click();
        await note.waitFor({ state: 'detached' });
        assert.equal(await page.evaluate(() => blockFixture.answers.at(-1).note), draft, 'The submitted answer records the draft');
        assert.equal(await page.locator('.decision-card').count(), 0, 'Answered cards retire');
        assert.deepEqual(errors, []);
      } finally {
        await page.close();
      }
    }
  }
  // A draft belongs to the currently open detail, not to a future visit.
  const page = await browser.newPage();
  try {
    await page.goto(origin + '#tasks?open=BLOCK-4');
    await page.locator('.decision-note').fill('Discard when leaving');
    await page.getByRole('button', { name: 'Close task detail', exact: true }).click();
    await page.locator('#tasks-body .row[data-key="task-BLOCK-4"] .title').click();
    await page.locator('.decision-note').waitFor({ state: 'visible' });
    assert.equal(await page.locator('.decision-note').inputValue(), '', 'Leaving retires the note');
    await page.evaluate(async () => {
      const { taskState, rerender } = await import('/js/task-shared.js');
      const { applyUpdatedTask } = await import('/js/task-data.js');
      const task = blockFixture.tasks.find(task => task.id === 'BLOCK-4');
      task.open_decisions.push({ ...task.open_decisions[0], id: 'DR-SECOND', gate: 'operator_question' });
      applyUpdatedTask(structuredClone(task), taskState.paneContext);
      rerender(taskState.paneContext);
    });
    const first = page.locator('.decision-card[data-request-id="DR-BLOCK-4"] .decision-note');
    const second = page.locator('.decision-card[data-request-id="DR-SECOND"] .decision-note');
    await first.fill('First request evidence');
    await second.fill('Second request evidence');
    // Keep a separate editor open while the first request is answered elsewhere.
    await page.getByRole('button', { name: 'Edit description for BLOCK-4', exact: true }).click();
    await page.evaluate(async () => {
      const { taskState, rerender } = await import('/js/task-shared.js');
      const { applyUpdatedTask } = await import('/js/task-data.js');
      const task = blockFixture.tasks.find(task => task.id === 'BLOCK-4');
      task.open_decisions = task.open_decisions.filter(request => request.id !== 'DR-BLOCK-4');
      applyUpdatedTask(structuredClone(task), taskState.paneContext);
      rerender(taskState.paneContext);
    });
    assert.equal(await first.count(), 0, 'Closed requests retire even while another editor retains the detail');
    assert.equal(await second.inputValue(), 'Second request evidence', 'Remaining requests keep their own notes');
  } finally {
    await page.close();
  }
}
