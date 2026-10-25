import fs from 'node:fs';
import path from 'node:path';

// Public detail regression: admission parks backlog work without promotion
// warnings. Serve the shipped dashboard with the public task-record contract.
export async function assertPilotAdmission(browser, origin, evidence) {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const task = {
    id: 'ADMISSION-1', title: 'Validation work awaiting an operator',
    status: 'backlog', priority: 'medium', pilot_warnings: [],
    resolvable_pilot_admission_hold: true,
    description: 'The pilot held admission until an operator records evidence or accepts the finding.',
  };
  const posts = [];
  let refusal = false;
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const detail = page.locator('#task-detail-body');
  const clear = detail.locator('.pilot-admission-clear');
  const approve = detail.locator('.pilot-admission-approve-anyway');
  const refresh = async () => {
    await page.locator('#refresh-btn').click();
    await page.waitForFunction(expected => (document.querySelector('#task-detail-body .pilot-admission-clear') !== null) === expected, task.resolvable_pilot_admission_hold && !task.open_decisions?.length);
  };
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
      else if (pathname === '/api/tasks') body = { items: [task], total: 1, limit: 50 };
      else if (pathname === '/api/tasks/ADMISSION-1') body = task;
      else if (pathname === '/api/tasks/ADMISSION-1/pilot-admission') {
        check(request.method() === 'POST', 'Admission must use the typed POST route');
        posts.push(request.postDataJSON());
        if (refusal) {
          status = 403;
          body = { code: 'authorization_denied', error: 'Operator authority required' };
        } else {
          task.resolvable_pilot_admission_hold = false;
          body = task;
        }
      }
      await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
    });
    await page.goto(`${origin}/?workspace=one#tasks?status=backlog&open=ADMISSION-1`);
    await page.evaluate(async () => {
      globalThis.setInterval = () => 0;
      globalThis.EventSource = class { close() {} };
      await import('/app.js');
    });
    await detail.locator('.row-detail').waitFor({ state: 'visible' });
    const counts = [];
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: width === 375 ? 812 : 1000 });
      counts.push({ width, clear: await clear.count(), approve: await approve.count() });
      if (await clear.count()) await clear.scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, `admission-backlog-dark-${width}.png`) });
    }
    check(counts.every(item => item.clear === 1 && item.approve === 1), `Backlog admission controls missing: ${JSON.stringify(counts)}`);
    check(await detail.locator('.pilot-warnings').count() === 0, 'Backlog admission must not invent promotion warnings');
    check(!await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), 'Admission controls must fit phone width');
    await page.setViewportSize({ width: 1440, height: 1000 });
    let evidenceText = 'Operator verified the held requirement.';
    page.on('dialog', dialog => dialog.type() === 'confirm' ? dialog.accept() : dialog.accept(evidenceText));
    evidenceText = ' ';
    await clear.click();
    await detail.getByText(/pilot_admission_evidence_required/).waitFor();
    check(posts.length === 0, 'Empty evidence must not send a write');
    evidenceText = 'Operator verified the held requirement.';
    for (const verdict of ['clear', 'approve-anyway']) {
      task.resolvable_pilot_admission_hold = true;
      await refresh();
      await (verdict === 'clear' ? clear : approve).click();
      await clear.waitFor({ state: 'detached' });
      check(JSON.stringify(posts.at(-1)) === JSON.stringify({ verdict, evidence: evidenceText }), `Wrong typed request: ${JSON.stringify(posts)}`);
    }
    task.resolvable_pilot_admission_hold = true;
    refusal = true;
    await refresh();
    await clear.click();
    await detail.getByText(/authorization_denied/).waitFor();
    check(await clear.isEnabled(), 'Refused admission must keep controls available');
    refusal = false;
    await clear.click();
    await clear.waitFor({ state: 'detached' });
    task.resolvable_pilot_admission_hold = true;
    // The decision channel already offers answers for the same effective hold.
    task.open_decisions = [{ id: 'decision-admission', gate: 'pilot_admission', answers: ['clear', 'approve-anyway'], expires_at: '2099-01-01T00:00:00Z' }];
    await refresh();
    await detail.getByText('Decision needed', { exact: true }).waitFor();
    check(await clear.count() === 0 && await approve.count() === 0, 'An open admission decision must have only one action set');
    delete task.open_decisions;
    task.status = 'proposed';
    task.pilot_warnings = [{ kind: 'utility_warnings', text: 'A proposed promotion warning' }];
    await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('tasks?status=proposed&open=ADMISSION-1'));
    await clear.waitFor({ state: 'visible' });
    check(await detail.locator('.pilot-warning-findings').isVisible(), 'Proposed warning findings must remain visible');
    check(await clear.count() === 1 && await approve.count() === 1, 'Proposed held detail must retain one action set');
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: width === 375 ? 812 : 1000 });
      await clear.scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, `admission-proposed-dark-${width}.png`) });
    }
    check(errors.length === 0, errors.join('\n'));
    fs.writeFileSync(path.join(evidence, 'admission-result.json'), JSON.stringify({ passed: true, counts, posts, emptyEvidence: true, authorityRefusal: true, decisionDeduplication: true, proposedWarnings: true }, null, 2));
  } finally {
    await page.close();
  }
}
