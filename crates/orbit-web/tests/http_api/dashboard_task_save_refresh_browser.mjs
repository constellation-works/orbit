// Reproduce a list response crossing a task save through the shipped UI and
// API fixture. Promise barriers control response ordering without wall sleeps.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

export async function checkTaskSaveRefresh({ openPage, row, pane, waitShown, capture, results, evidence }) {
  const cases = [];
  for (const width of [1440, 375]) {
    for (const field of ['status', 'crew']) {
      const { page, errors, context } = await openPage({ width, height: width === 375 ? 812 : 900, clock: true });
      await row(page, 'PANEL-6').locator('.title').click();
      await waitShown(page, 'PANEL-6');
      if (field === 'status') {
        // Status saves keep the full page open; a dock status save folds it.
        await page.evaluate(() => { location.hash = '#tasks/PANEL-6'; });
        await page.locator('#tasks-main.task-page-mode').waitFor();
      }
      const detail = pane(page).locator('#detail-PANEL-6');
      await detail.locator('.task-status-select').waitFor();
      const detailReads = () => page.evaluate(() => panelFixture.reads.filter(read => read.startsWith('/api/tasks/PANEL-6?')).length);
      const settleList = () => page.waitForFunction(() => document.getElementById('tasks-body').getAttribute('aria-busy') === 'false');

      // A normal refresh still uses the bundled full projection, avoiding a
      // duplicate detail GET. This also establishes the selected-detail scope.
      await settleList();
      const initialReads = await detailReads();
      await page.clock.fastForward(30_000);
      await settleList();
      assert.equal(await detailReads(), initialReads, 'An ordinary list refresh primes detail without a duplicate GET');

      await page.evaluate(() => { panelFixture.holdTaskList = true; });
      await page.clock.fastForward(30_000);
      await page.waitForFunction(() => typeof panelFixture.releaseTaskList === 'function');
      const savedValue = field === 'status' ? 'in-progress' : 'sol';
      await detail.locator(`.task-${field}-select`).selectOption(savedValue);
      await detail.locator(`.${field}-cell .mutation-feedback.success`).waitFor();
      // The save has completed while the list response still holds old bytes.
      await page.evaluate(() => panelFixture.releaseTaskList());
      await settleList();

      const fresh = await page.evaluate(async () => {
        const task = await (await fetch('/api/tasks/PANEL-6?workspace=one')).json();
        return { status: task.status, crew: task.crew || '', transitions: task.status_transitions.map(t => t.status) };
      });
      const shown = await detail.evaluate(node => ({
        status: node.querySelector('.status-cell').dataset.status,
        crew: node.querySelector('.task-crew-select').value,
        transitions: Array.from(node.querySelectorAll('.task-status-select > option')).map(option => option.value).filter(Boolean),
      }));
      await capture(page, `task-save-refresh-${field}-dark-${width}.png`);
      cases.push({ field, width, expected: fresh, actual: shown, matches: JSON.stringify(shown) === JSON.stringify(fresh) });
      assert.deepEqual(errors, []);
      await context.close();
    }
  }
  // Aggregate polling starts a direct full-detail read against a new summary.
  // Hold its serialized pre-save response while the visible cached detail saves.
  for (const field of ['description', 'complexity', 'status', 'crew']) {
    const { page, errors, context } = await openPage({ url: field === 'crew' ? '/?workspace=one#tasks' : '/?workspace=all#tasks', clock: true });
    await row(page, 'PANEL-6').locator('.title').click();
    await waitShown(page, 'PANEL-6');
    if (field === 'status') {
      await page.evaluate(() => { location.hash = '#tasks/PANEL-6'; });
      await page.locator('#tasks-main.task-page-mode').waitFor();
    }
    const detail = pane(page).locator('#detail-PANEL-6');
    await detail.locator('.task-status-select').waitFor();
    await page.evaluate(() => { panelFixture.holdTaskDetail = true; panelFixture.omitBundledDetail = true; });
    await page.clock.fastForward(30_000);
    await page.waitForFunction(() => typeof panelFixture.releaseTaskDetail === 'function');
    if (field === 'description') {
      const cell = detail.locator('.description-block');
      await cell.locator('.field-edit').click();
      await cell.locator('.field-editor-input').fill('Saved description from the operator');
      await cell.locator('button.action.save').click();
      await page.waitForFunction(() => panelFixture.patches.some(p => p.body.description));
    } else {
      const value = field === 'status' ? 'in-progress' : field === 'crew' ? 'sol' : 'hard';
      await detail.locator(`.task-${field}-select`).selectOption(value);
      await page.waitForFunction(field => panelFixture.patches.some(p => p.body[field]), field);
      await detail.locator(`.${field}-cell .mutation-feedback.success`).waitFor();
    }
    await page.evaluate(() => panelFixture.releaseTaskDetail());
    await page.waitForFunction(() => !panelFixture.holdTaskDetail);
    // Drain the deferred fetch and its promise callbacks before asserting.
    await page.clock.runFor(1);
    const current = await page.evaluate(async () => (await (await fetch('/api/tasks/PANEL-6?workspace=one')).json()));
    if (field === 'description') assert.match(await detail.innerText(), /Saved description from the operator/);
    else if (field === 'status') assert.equal(await detail.locator('.status-cell').getAttribute('data-status'), current.status);
    else assert.equal(await detail.locator(`.task-${field}-select`).inputValue(), current[field]);
    assert.deepEqual(errors, []);
    await context.close();
  }
  fs.writeFileSync(path.join(evidence, 'task-save-refresh-result.json'), JSON.stringify({ cases }, null, 2));
  assert.deepEqual(cases.filter(test => !test.matches), [], 'A pre-save list response must not replace saved detail or its governed transitions');
  results.taskSaveRefresh = true;
}
