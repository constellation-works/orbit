import fs from 'node:fs';
import path from 'node:path';

// [ORB-21238] The Readiness card's "Skip pilot" control: shown only while the
// drain holds a backlog task on its pilot state, it adds `no-pilot` through
// the guarded task PATCH, and the card then shows the re-read admission
// state. A refusal keeps the control; removing the tag brings the hold back.
export async function assertSkipPilot(browser, origin, evidence) {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const task = {
    id: 'SKIP-1', title: 'Design task rewritten after its pilot',
    status: 'backlog', priority: 'medium', tags: ['docs'], pilot_warnings: [], history: [],
    description: 'The operator settled the footprint and criteria after the first pilot.',
  };
  const stale = {
    task_id: 'SKIP-1', eligible: false, reason: 'pilot_assessment_stale',
    detail: 'The task description or acceptance criteria changed after its latest pilot. Dispatch is held until a new explicit assessment completes. Run `orbit task repilot SKIP-1` on the owner, or have an operator skip the pilot by adding the `no-pilot` tag (`orbit task update SKIP-1 --tag docs,no-pilot`; `--tag` replaces the list).',
  };
  let entry = stale;
  const patches = [];
  let refusal = false;
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const detail = page.locator('#task-detail-body');
  const skip = detail.locator('.readiness-list .skip-pilot');
  const refresh = async () => { await page.locator('#refresh-btn').click(); };
  try {
    await page.route('**/api/**', async route => {
      const request = route.request();
      const { pathname } = new URL(request.url());
      let status = 200;
      let body = { items: [], total: 0, limit: 50 };
      if (pathname === '/api/hosts') body = { hosts: [{ name: 'fixture', local: true, reachable: true }] };
      else if (pathname === '/api/workspaces') body = [{ id: 'one', name: 'one', status: 'active', is_default: true }];
      else if (pathname === '/api/crews') body = { default_crew: null, crews: [] };
      else if (pathname === '/api/diagnostics/friction') body = [];
      else if (pathname === '/api/workflows/auto/readiness') body = { tasks: [entry] };
      else if (pathname === '/api/tasks') body = { items: [task], total: 1, limit: 50 };
      else if (pathname === '/api/tasks/SKIP-1' && request.method() === 'PATCH') {
        const patch = request.postDataJSON();
        patches.push(patch);
        if (refusal) {
          status = 403;
          body = { code: 'authorization_denied', error: 'only an operator can add `no-pilot` to task SKIP-1' };
        } else {
          task.tags = patch.tags;
          task.history = [...task.history, { event: 'pilot_skipped', by: 'human:operator', at: '2026-11-02T05:00:00Z' }];
          entry = { task_id: 'SKIP-1', eligible: true };
          body = task;
        }
      } else if (pathname === '/api/tasks/SKIP-1') body = task;
      await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
    });
    await page.goto(`${origin}/?workspace=one#tasks?status=backlog&open=SKIP-1`);
    await page.evaluate(async () => {
      globalThis.setInterval = () => 0;
      globalThis.EventSource = class { close() {} };
      await import('/app.js');
    });
    await skip.waitFor({ state: 'visible' });
    await detail.getByText(/orbit task repilot SKIP-1/).waitFor();
    check(await detail.getByText(/no-pilot/).count() > 0, 'The stale hold must name the no-pilot skip');
    check(await detail.getByText('Dispatch without a new pilot assessment').count() === 1, 'Skip pilot must say what it does');
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: width === 375 ? 812 : 1000 });
      await skip.scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, `skip-pilot-held-dark-${width}.png`) });
    }
    check(!await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), 'Skip pilot must fit phone width');
    await page.setViewportSize({ width: 1440, height: 1000 });
    let accept = true;
    page.on('dialog', dialog => (accept ? dialog.accept() : dialog.dismiss()));

    accept = false;
    await skip.click();
    check(patches.length === 0, 'A declined confirmation must not write');
    accept = true;

    refusal = true;
    await skip.click();
    await detail.getByText(/authorization_denied/).waitFor();
    check(await skip.isEnabled(), 'A refused skip must keep the control available');
    check(JSON.stringify(patches.at(-1)) === JSON.stringify({ tags: ['docs', 'no-pilot'] }), `Wrong PATCH: ${JSON.stringify(patches)}`);
    refusal = false;

    await skip.click();
    await skip.waitFor({ state: 'detached' });
    await detail.locator('.readiness-pilot-skipped').waitFor({ state: 'visible' });
    check(JSON.stringify(patches.at(-1)) === JSON.stringify({ tags: ['docs', 'no-pilot'] }), `Wrong PATCH: ${JSON.stringify(patches)}`);
    check(await detail.getByText(/orbit task repilot SKIP-1/).count() === 0, 'The card must drop the stale hold once skipped');
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: width === 375 ? 812 : 1000 });
      await detail.locator('.readiness-pilot-skipped').scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, `skip-pilot-skipped-dark-${width}.png`) });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });

    // Removing the tag restores the pilot gate, and the card shows it.
    task.tags = ['docs'];
    entry = stale;
    await refresh();
    await skip.waitFor({ state: 'visible' });
    check(await detail.locator('.readiness-pilot-skipped').count() === 0, 'An untagged task is not skipped');

    // A wait that is not pilot state offers no skip.
    entry = { task_id: 'SKIP-1', eligible: false, reason: 'unmet_dependency', dependencies: [{ task_id: 'SKIP-0', status: 'backlog' }] };
    await refresh();
    await skip.waitFor({ state: 'detached' });

    // [ORB-21278] A refusal while a description draft keeps the detail open
    // repaints beside Skip pilot. The draft text and its selection survive the
    // refusal, and a retried skip that lands clears it with the editor open.
    task.tags = ['docs'];
    entry = stale;
    await refresh();
    await skip.waitFor({ state: 'visible' });
    await detail.locator('.description-block button.field-edit').first().click();
    const draft = detail.locator('.description-block textarea');
    await draft.fill('Unsaved operator draft');
    await draft.evaluate(node => node.setSelectionRange(3, 9));
    const feedback = detail.locator('.readiness-skip-pilot .operator-record-refusal');
    refusal = true;
    await skip.click();
    await feedback.filter({ hasText: 'authorization_denied' }).waitFor();
    check(await skip.isEnabled(), 'A refused skip with an open draft must keep the control available');
    check(await draft.isVisible(), 'A refused skip must keep the description editor open');
    check(await draft.inputValue() === 'Unsaved operator draft', 'A refused skip must keep the unsaved draft text');
    check(await draft.evaluate(node => node.selectionStart === 3 && node.selectionEnd === 9), 'A refused skip must keep the draft selection');
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: width === 375 ? 812 : 1000 });
      await feedback.scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, `skip-pilot-refusal-draft-dark-${width}.png`) });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
    refusal = false;
    await skip.click();
    await skip.waitFor({ state: 'detached' });
    check(await feedback.count() === 0, 'A landed skip must clear the refusal beside the action');
    check(await draft.isVisible(), 'A landed skip must keep the description editor open');
    check(await draft.inputValue() === 'Unsaved operator draft', 'A landed skip must keep the unsaved draft text');
    check(await draft.evaluate(node => node.selectionStart === 3 && node.selectionEnd === 9), 'A landed skip must keep the draft selection');

    check(errors.length === 0, errors.join('\n'));
    fs.writeFileSync(path.join(evidence, 'skip-pilot-result.json'), JSON.stringify({ passed: true, patches, refusal: true, declined: true, restoredOnRemoval: true, hiddenWhenNotPilotHeld: true, refusalWithOpenDraft: true }, null, 2));
  } finally {
    await page.close();
  }
}
