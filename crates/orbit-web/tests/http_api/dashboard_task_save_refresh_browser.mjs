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
  fs.writeFileSync(path.join(evidence, 'task-save-refresh-result.json'), JSON.stringify({ cases }, null, 2));
  assert.deepEqual(cases.filter(test => !test.matches), [], 'A pre-save list response must not replace saved detail or its governed transitions');
  results.taskSaveRefresh = true;
}
