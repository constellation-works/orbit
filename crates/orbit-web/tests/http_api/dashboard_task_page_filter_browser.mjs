import assert from 'node:assert/strict';

// The page owns its task independently of the saved list's filter and page.
export async function checkTaskPageFilter({ openPage, row, pane, waitShown, capture, results }) {
  const { page, context, errors } = await openPage();
  try {
    await page.evaluate(() => { location.hash = '#tasks?status=backlog'; });
    await row(page, 'PANEL-1').waitFor({ state: 'hidden' });
    await row(page, 'PANEL-6').locator('.title').click();
    await waitShown(page, 'PANEL-6');
    await page.getByRole('button', { name: 'Open full page' }).click();
    await page.waitForFunction(() => location.hash === '#tasks/PANEL-6');
    await pane(page).locator('.task-status-select').first().selectOption('in-progress');
    await page.waitForFunction(() => panelFixture.patches.some(p => p.id === 'PANEL-6' && p.body.status === 'in-progress'));
    await page.waitForFunction(async () => (await import('/js/task-shared.js')).taskState.statusFeedback.get('PANEL-6')?.kind === 'success');
    await capture(page, 'task-page-filter-dark-1440.png');
    assert.match(await pane(page).innerText(), /Panel fixture task 6/, 'A status edit outside the saved filter keeps the task readable');
    assert.doesNotMatch(await pane(page).innerText(), /Loading PANEL-6/);
    assert.equal(await page.evaluate(() => location.hash), '#tasks/PANEL-6');
    await page.locator('#refresh-btn').click();
    await pane(page).locator('.task-detail').waitFor({ state: 'visible' });
    await page.setViewportSize({ width: 375, height: 812 });
    await capture(page, 'task-page-filter-dark-375.png');
    await page.setViewportSize({ width: 1440, height: 900 });
    await pane(page).getByRole('button', { name: 'Back to list', exact: true }).click();
    await page.waitForFunction(() => location.hash.startsWith('#tasks?status=backlog'));
    assert.equal(await row(page, 'PANEL-6').count(), 0, 'Back keeps the saved filter');
    await page.goForward();
    await pane(page).locator('.task-detail').waitFor({ state: 'visible' });
    await page.reload();
    await pane(page).locator('.task-detail').waitFor({ state: 'visible' });
    assert.equal(await page.evaluate(() => location.hash), '#tasks/PANEL-6', 'Reload names the displayed task');

    // An external list refresh can omit the task entirely (pagination), while
    // its detail endpoint still serves a newer task/comment record.
    await page.evaluate(() => {
      panelFixture.omittedTaskIds = ['PANEL-6'];
      panelFixture.tasks.find(t => t.id === 'PANEL-6').title = 'External update outside the saved page';
    });
    await page.locator('#refresh-btn').click();
    await page.waitForFunction(() => document.querySelector('.task-pane-title')?.textContent === 'External update outside the saved page');
    assert.equal(await page.evaluate(() => location.hash), '#tasks/PANEL-6');
    await page.evaluate(() => { location.hash = '#tasks?status=backlog'; });
    await row(page, 'PANEL-8').locator('.title').click();
    await waitShown(page, 'PANEL-8');
    await page.getByRole('button', { name: 'Open full page' }).click();
    await page.waitForFunction(() => location.hash === '#tasks/PANEL-8');
    await page.evaluate(async () => {
      const { taskState, rerender } = await import('/js/task-shared.js');
      const { applyUpdatedTask } = await import('/js/task-data.js');
      const task = await (await fetch('/api/tasks/PANEL-8')).json();
      task.status = 'in-progress';
      task.title = 'Incoming status update outside the filter';
      applyUpdatedTask(task, taskState.paneContext);
      rerender(taskState.paneContext);
    });
    assert.match(await pane(page).innerText(), /Incoming status update outside the filter/);
    assert.equal(await page.evaluate(() => location.hash), '#tasks/PANEL-8');
    assert.deepEqual(errors, []);
    results.taskPageOutsideFilter = true;
  } finally {
    await context.close();
  }
}
