// Regression scenarios driven through the shipped dashboard and disposable
// API fixtures. Imported by dashboard_task_panel_browser.mjs; no live writes.
import assert from 'node:assert/strict';

export async function checkTaskPresentation({ openPage, row, waitShown, capture, results }) {
  const assertReachable = async (control) => assert.equal(await control.evaluate(n => {
    const r = n.getBoundingClientRect();
    return document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2)?.closest('button') === n;
  }), true, 'Focused controls remain visible and unobscured');
  for (const width of [1440, 375]) {
    const { page, errors, context } = await openPage({ width, height: width === 375 ? 812 : 900 });
    await page.evaluate(() => {
      panelFixture.tasks.find(t => t.id === 'PANEL-6').description = 'Long overview content for scrolling. '.repeat(500);
      panelFixture.readiness = [
        { task_id: 'PANEL-6', eligible: false, reason: 'context_lock_conflict',
          conflicts: [{ locking_task_id: 'PANEL-1' }, { locking_task_id: 'PANEL-1' }],
          blocking_task_ids: ['PANEL-2', 'PANEL-3', 'PANEL-4', 'PANEL-5'], run_ids: ['jrun-fixture-live'] },
        { task_id: 'PANEL-7', eligible: false, reason: 'context_lock_conflict' },
        { task_id: 'PANEL-8', eligible: false, reason: 'unmet_dependency', dependencies: [{ task_id: 'PANEL-1', status: 'review' }] },
        { task_id: 'PANEL-9', eligible: false, reason: 'claimed_by_live_child', run_ids: ['jrun-fixture-live'] },
      ];
    });
    // Delivery attention: badge on the failed review row, banner in its detail.
    const badge = row(page, 'PANEL-3').locator('.delivery-attention-badge');
    assert.equal(await badge.innerText(), 'review · CI failed');
    assert.match(await badge.getAttribute('title'), /complete_pr/);
    assert.match(await badge.getAttribute('title'), /attempt 1 of 3/);
    assert.equal(await row(page, 'PANEL-1').locator('.delivery-attention-badge').count(), 0, 'Unaffected rows have no delivery badge');
    await row(page, 'PANEL-3').locator('.title').click();
    await waitShown(page, 'PANEL-3');
    const banner = page.locator('#detail-PANEL-3 .delivery-attention');
    await banner.waitFor();
    assert.match(await banner.innerText(), /Failed step: complete_pr/);
    assert.match(await banner.innerText(), /Blocker: failed_checks/);
    assert.match(await banner.innerText(), /Observed head: abc123/);
    assert.match(await banner.innerText(), /Auto-fix attempt 1 of 3/);
    assert.equal(await banner.getByRole('link', { name: 'View run' }).getAttribute('href') !== null, true);
    assert.equal(await banner.getByRole('link', { name: 'PR #42' }).getAttribute('href'), 'https://github.com/o/r/pull/42');
    await capture(page, `delivery-attention-dark-${width}.png`);
    await page.getByRole('button', { name: width === 375 ? 'Back' : 'Close task detail', exact: true }).click();
    await page.locator('#task-detail-pane').waitFor({ state: 'hidden' });

    await row(page, 'PANEL-6').locator('.title').click();
    await waitShown(page, 'PANEL-6');
    const detail = page.locator('#detail-PANEL-6');
    const readiness = detail.locator('.readiness-drain');
    await readiness.locator('.readiness-drain-head strong').waitFor();
    assert.equal(await readiness.locator('.readiness-drain-head').innerText(), 'Waiting on locks', 'Expanded lock status must not repeat its holders');
    const held = readiness.locator('.readiness-drain-line').filter({ has: page.getByText('Held by', { exact: true }) });
    assert.deepEqual(await held.locator('a').allTextContents(), ['PANEL-1', 'PANEL-2', 'PANEL-3', 'PANEL-4', 'PANEL-5'], 'Each reported holder renders once');
    for (const id of ['PANEL-1', 'PANEL-2', 'PANEL-3', 'PANEL-4', 'PANEL-5']) {
      assert.equal(await readiness.getByRole('link', { name: id, exact: true }).count(), 1);
      assert.match(await readiness.getByRole('link', { name: id, exact: true }).getAttribute('href'), new RegExp(`workspace=one.*open=${id}`), 'Holder links preserve the workspace');
    }
    assert.match(await readiness.locator('.readiness-drain-line').filter({ hasText: 'Live run' }).locator('a').getAttribute('href'), /workspace=one.*jrun-fixture-live/);
    if (width === 375) {
      const tops = await held.locator('a').evaluateAll(nodes => nodes.map(n => n.getBoundingClientRect().top));
      assert.ok(new Set(tops).size > 1, 'Multiple holders wrap on a phone');
    }
    await capture(page, `presentation-pane-dark-${width}.png`);

    const body = page.locator('#task-detail-body');
    const footer = detail.locator('.task-action-footer[data-footer-tab="overview"]');
    const visibleFooter = async () => {
      await page.waitForFunction(() => document.getAnimations().every(a => a.playState !== 'running' || !a.effect?.target?.closest?.('#task-detail-pane')));
      const b = await body.boundingBox();
      const f = await footer.boundingBox();
      assert.ok(f && f.y >= b.y && Math.abs(f.y + f.height - b.y - b.height) <= 1, `The footer stays at the bottom of its own visible scroll viewport: ${JSON.stringify({ viewport: b, footer: f })}`);
    };
    await visibleFooter();
    await detail.getByRole('button', { name: 'Show full description', exact: true }).click();
    for (const part of [0, 0.5, 1]) {
      await body.evaluate((n, part) => { n.scrollTop = (n.scrollHeight - n.clientHeight) * part; }, part);
      await visibleFooter();
      if (part === 0.5) await capture(page, `presentation-pane-scrolled-dark-${width}.png`);
    }
    assert.equal(await detail.getByRole('group', { name: 'Task timestamps' }).count(), 0, 'Task details omit the timestamp footer');
    await capture(page, `presentation-overview-end-dark-${width}.png`);
    // A field editor is content in the scroll viewport, outside the footer.
    await detail.getByRole('button', { name: 'Edit description for PANEL-6', exact: true }).click();
    const editor = detail.getByRole('textbox', { name: 'Edit description for PANEL-6', exact: true });
    await editor.focus();
    const editorBox = await editor.boundingBox();
    assert.ok(editorBox.y < (await footer.boundingBox()).y, 'A focused content editor is reachable above the sticky footer');
    const cancelEdit = detail.locator('.description-block button.action.cancel');
    await cancelEdit.focus();
    await assertReachable(cancelEdit);
    await page.keyboard.press('Enter');
    await footer.getByRole('button', { name: 'reject', exact: true }).click();
    const form = footer.locator('.reject-form');
    assert.equal(await form.locator('textarea').evaluate(n => n === document.activeElement), true, 'Reject focuses its reason field');
    await form.locator('textarea').fill('Disposable fixture rejection');
    // Even a manually enlarged composer leaves its submit/cancel reachable.
    await form.locator('textarea').evaluate(n => { n.style.height = '600px'; });
    await form.getByRole('button', { name: 'cancel', exact: true }).focus();
    await visibleFooter();
    await assertReachable(form.getByRole('button', { name: 'cancel', exact: true }));
    await capture(page, `presentation-reject-dark-${width}.png`);
    await page.keyboard.press('Enter');
    assert.equal(await footer.getByRole('button', { name: 'reject', exact: true }).evaluate(n => n === document.activeElement), true, 'Cancel restores Reject focus');
    await footer.getByRole('button', { name: 'reject', exact: true }).click();
    await form.locator('textarea').fill('Disposable fixture rejection');
    await form.getByRole('button', { name: 'submit', exact: true }).click();
    await page.waitForFunction(() => panelFixture.lifecycle.length === 1);
    assert.deepEqual(await page.evaluate(() => panelFixture.lifecycle[0]), { path: '/api/tasks/PANEL-6/reject', body: { note: 'Disposable fixture rejection' } });
    await page.locator('#task-detail-pane').waitFor({ state: 'hidden' });

    // Remaining blocker variants and real task-link navigation.
    for (const id of ['PANEL-7', 'PANEL-8', 'PANEL-9']) {
      await row(page, id).locator('.title').click();
      await waitShown(page, id);
      const card = page.locator(`#detail-${id} .readiness-drain`);
      await card.locator('.readiness-drain-head strong').waitFor();
      if (id === 'PANEL-7') {
        assert.match(await card.innerText(), /Held by\s+Lock holder unavailable/);
        await capture(page, `presentation-unknown-holder-dark-${width}.png`);
      }
      if (id === 'PANEL-8') {
        assert.match(await card.innerText(), /Depends on\s+PANEL-1\s+review/);
        await card.getByRole('link', { name: 'PANEL-1', exact: true }).click();
        await waitShown(page, 'PANEL-1');
      }
      if (id === 'PANEL-9') {
        assert.match(await card.innerText(), /Live run\s+jrun-fixture-live/);
        assert.doesNotMatch(await card.innerText(), /unavailable/, 'A known live run is an honest blocker without a task holder');
      }
      await page.getByRole('button', { name: width === 375 ? 'Back' : 'Close task detail', exact: true }).click();
      await page.locator('#task-detail-pane').waitFor({ state: 'hidden' });
    }
    if (width === 1440) {
      await row(page, 'PANEL-6').locator('.title').click();
      await waitShown(page, 'PANEL-6');
      await page.getByRole('button', { name: 'Open full page', exact: true }).click();
      await page.locator('#tasks-main.task-page-mode').waitFor();
      for (const part of [0, 0.5, 1]) {
        await body.evaluate((n, part) => { n.scrollTop = (n.scrollHeight - n.clientHeight) * part; }, part);
        await visibleFooter();
        await capture(page, `presentation-page-${part === 0 ? 'top' : part === 1 ? 'end' : 'scrolled'}-dark-${width}.png`);
      }
      await detail.getByRole('tab', { name: /^Comments/ }).click();
      assert.equal(await footer.isVisible(), false, 'The footer is absent outside Overview');
      await detail.getByRole('tab', { name: /^Overview/ }).click();
      // Confirmation cancellation must never send a request.
      page.removeAllListeners('dialog');
      page.once('dialog', dialog => dialog.dismiss());
      await footer.getByRole('button', { name: 'archive', exact: true }).click();
      assert.equal(await page.evaluate(() => panelFixture.lifecycle.length), 1);
      page.once('dialog', dialog => dialog.accept());
      await footer.getByRole('button', { name: 'archive', exact: true }).click();
      await page.waitForFunction(() => panelFixture.lifecycle.length === 2);
    }
    // The Drain wait card opens the same task renderer and scroll viewport.
    if (width === 375) {
      await page.locator('button.tab[data-tab="drain"]').click();
      const drainLink = page.locator('#drain-waits-panel .drain-wait-task-links').getByRole('link', { name: 'PANEL-6', exact: true });
      await drainLink.click();
      await waitShown(page, 'PANEL-6');
      await visibleFooter();
      await capture(page, `presentation-from-drain-dark-${width}.png`);
      await readiness.getByRole('link', { name: 'PANEL-2', exact: true }).click();
      await waitShown(page, 'PANEL-2');
      await capture(page, `presentation-short-task-dark-${width}.png`);
      const shortFooter = page.locator('#detail-PANEL-2 .task-action-footer[data-footer-tab="overview"]');
      const b = await body.boundingBox();
      const f = await shortFooter.boundingBox();
      assert.ok(Math.abs(f.y + f.height - b.y - b.height) <= 1, 'Short task actions also anchor to the viewport bottom');
    }
    assert.deepEqual(errors, []);
    await context.close();
  }
  results.taskPresentation = { lockHolders: true, unknownAndLiveRun: true, dependenciesAndNavigation: true, panePageSheetScrolling: true, drainEntry: true, timestampFooterRemoved: true, rejectAndArchive: true };
}
