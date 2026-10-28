// Drive Start and Stop through the shipped dashboard with an overflow task.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

export async function checkDrainActionReadiness({ openPage, row, pane, waitShown, capture, results, evidence }) {
  const cases = [];
  for (const [id, status] of [['PANEL-51', 'backlog'], ['PANEL-52', 'proposed']]) {
    const { page, errors, context } = await openPage({ url: '/?workspace=one&drain-actions#tasks', clock: true });
    try {
      await row(page, id).scrollIntoViewIfNeeded();
      await row(page, id).locator('.title').click();
      await waitShown(page, id);
      // Opening an overflow task may read it once. The next regular poll
      // bundles it and establishes the shared-snapshot state before Start.
      await page.clock.fastForward(30_000);
      await page.waitForFunction(id => panelFixture.reads.some(read => new URL(read, location.href).searchParams.get('also_task') === id), id);
      await page.locator('#auto-drain-body[aria-busy="false"]').waitFor({ state: 'attached' });
      const detail = pane(page).locator(`#detail-${id} .readiness-list`);
      await detail.waitFor();

      for (const action of ['Start', 'Stop']) {
        const startRead = await page.evaluate(() => panelFixture.reads.length);
        // The Tasks pane hides the dock while a task is selected. Dispatch
        // the control's DOM event to exercise its shipped action handler
        // without closing the selection whose shared readiness must survive.
        const button = page.locator(action === 'Start' ? '.drain-start' : '.drain-stop[aria-label="Stop admissions"]');
        await button.dispatchEvent('click');
        await page.waitForFunction(action => panelFixture.drainActions.at(-1) === action, action);
        await page.locator('#auto-drain-body[aria-busy="false"]').waitFor({ state: 'attached' });
        await page.waitForFunction(() => !document.querySelector('.drain-start')?.disabled && !document.querySelector('.drain-stop')?.disabled);
        await page.waitForFunction(({ id, action }) => document.querySelector(`#detail-${id} .readiness-list`)?.textContent.includes(`Readiness after ${action}`), { id, action });
        const reads = await page.evaluate(start => panelFixture.reads.slice(start), startRead);
        const snapshots = reads.filter(read => new URL(read, 'http://fixture').pathname === '/api/workflows/auto/readiness');
        const perTask = snapshots.filter(read => new URL(read, 'http://fixture').searchParams.has('task'));
        const dot = await row(page, id).locator('.ready-dot:not(.none)').count();
        for (const width of [1440, 375]) {
          await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
          if (width === 1440) await row(page, id).scrollIntoViewIfNeeded();
          await capture(page, `drain-action-${status}-${action.toLowerCase()}-dark-${width}.png`);
        }
        await page.setViewportSize({ width: 1440, height: 900 });
        cases.push({ id, status, action, snapshots, perTask, dot });
        fs.writeFileSync(path.join(evidence, 'drain-action-result.json'), JSON.stringify({ cases }, null, 2));
        assert.deepEqual(perTask, [], `${action} must not read the selected ${status} task separately`);
        assert.equal(snapshots.length, 1, `${action} refreshes readiness once`);
        assert.equal(new URL(snapshots[0], 'http://fixture').searchParams.get('also_task'), id, `${action} includes the selected ${status} task past the 50-task bound`);
        if (status === 'backlog') assert.equal(dot, 1, `${action} keeps the overflow row readiness dot`);
      }
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
    }
  }
  results.drainActionReadiness = true;
}
