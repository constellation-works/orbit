// The UTC/Local clock switch repaints times a render keeps. The API answers
// the same bytes before and after each switch, so every task detail and run
// row hash is unchanged; what has to move is only the zone they read in. The
// open task detail is kept, not rebuilt: an expanded section, a comment draft
// with its caret and the focused control all survive both switches.
import path from 'node:path';

const TASK_ID = 'ORB-9000';
const CREATED = '2026-10-07T06:31:00Z';
const UPDATED = '2026-10-07T06:35:00Z';
const HISTORY_AT = '2026-10-07T06:40:00Z';
const COMMENT_AT = '2026-10-07T06:45:00Z';
const RUN_AT = '2026-10-07T06:50:00Z';

const task = {
  id: TASK_ID,
  title: 'Clock fixture task',
  status: 'in-progress',
  priority: 'medium',
  description: 'Fixed payload',
  plan: 'Step one of the plan.',
  created_at: CREATED,
  updated_at: UPDATED,
  history: [{ at: HISTORY_AT, by: 'operator', event: 'status_changed', note: 'backlog -> in-progress' }],
  comments: [{ at: COMMENT_AT, by: 'operator', message: 'A posted comment.' }],
};
const run = { run_id: 'jrun-clock-1', job_id: 'task_pr_pipeline', state: 'success', started_at: RUN_AT, finished_at: RUN_AT, duration_ms: 1000 };

const UTC = {
  created: '2026-10-07 06:31 UTC',
  updated: '2026-10-07 06:35 UTC',
  history: '2026-10-07 06:40 UTC',
  comment: '2026-10-07 06:45 UTC',
  run: '2026-10-07 06:50 UTC',
};
const LOCAL = {
  created: '2026-10-06 23:31 PDT',
  updated: '2026-10-06 23:35 PDT',
  history: '2026-10-06 23:40 PDT',
  comment: '2026-10-06 23:45 PDT',
  run: '2026-10-06 23:50 PDT',
};

export async function assertClockRepaint(browser, origin, evidence) {
  const context = await browser.newContext({ timezoneId: 'America/Los_Angeles', viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  try {
    await page.route('**/api/**', async route => {
      const pathname = new URL(route.request().url()).pathname.replace(/^\/api\/on\/[^/]+/, '/api');
      let body = { items: [], total: 0, limit: 50 };
      if (pathname === '/api/workspaces') body = [{ id: 'one', name: 'one', status: 'active', is_default: true }];
      else if (pathname === '/api/diagnostics/friction') body = [];
      else if (pathname === '/api/crews') body = { default_crew: null, crews: [] };
      else if (pathname === '/api/routines') body = { routines: [], clock: {} };
      else if (pathname === '/api/tasks') body = { items: [task], total: 1, limit: 50 };
      else if (pathname === `/api/tasks/${TASK_ID}`) body = task;
      else if (pathname === '/api/job-runs') body = { items: [run], total: 1, limit: 25 };
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
    });
    await page.goto(`${origin}/?workspace=one#tasks?open=${TASK_ID}`);
    await page.evaluate(async () => {
      localStorage.removeItem('orbit.dashboard.clock');
      globalThis.setInterval = () => 0;
      globalThis.EventSource = class { close() {} };
      await import('/app.js');
    });
    const detail = page.locator(`#task-detail-body [data-key="detail-${TASK_ID}"]`);
    await detail.locator('.task-meta-facts').waitFor({ state: 'visible' });

    const readTimes = () => detail.evaluate((node) => {
      const fact = (label) => [...node.querySelectorAll('.task-meta-facts .property-row')]
        .find((row) => row.querySelector('.label')?.textContent === label)?.querySelector('.value')?.textContent;
      return {
        created: fact('Created'),
        updated: fact('Updated'),
        history: node.querySelector('.history-line')?.textContent.match(/^\[(.*)\]/)?.[1],
        comment: node.querySelector('.comment-at')?.textContent,
        commentTitle: node.querySelector('.comment-age')?.title,
      };
    });
    const runTitle = () => page.locator('#runs-body .when').first().getAttribute('title');
    const expectTimes = async (expected, when) => {
      const times = await readTimes();
      const want = { created: expected.created, updated: expected.updated, history: expected.history, comment: expected.comment, commentTitle: expected.comment };
      check(JSON.stringify(times) === JSON.stringify(want), `${when}: task detail times ${JSON.stringify(times)}, expected ${JSON.stringify(want)}`);
    };
    // A refresh starts synchronously in the click; it is done once the meta
    // line reports the refresh on the newly chosen clock.
    const refreshed = label => page.waitForFunction(label => {
      const text = document.getElementById('meta-text').textContent;
      return text.startsWith('refreshed') && text.endsWith(label);
    }, label);
    const switchClock = async (zone, label) => {
      const button = page.locator(`.rail-foot .clock-zone-btn[data-zone="${zone}"]`);
      await button.click();
      await refreshed(label);
      check(await button.evaluate((node) => document.activeElement === node), `The refresh after choosing ${zone} must leave focus on its button`);
    };
    // The switch and refresh the button runs, made while focus stays in the
    // detail: a kept detail must not lose the focus or caret it holds.
    const switchClockInPlace = async (zone, label) => {
      await page.evaluate(async (zone) => {
        const { setClockZone } = await import('/js/common.js');
        setClockZone(zone);
        document.getElementById('refresh-btn').dispatchEvent(new MouseEvent('click'));
      }, zone);
      await refreshed(label);
    };

    await expectTimes(UTC, 'UTC on load');
    // Detail state an operator builds up: the collapsed Plan opened.
    const planToggle = detail.locator('.field-block.collapsible h4').filter({ has: page.locator('.field-title', { hasText: /^Plan$/ }) });
    await planToggle.click();
    await detail.evaluate((node) => { node.clockFixtureIdentity = 'kept'; });
    const detailState = () => detail.evaluate((node) => ({
      identity: node.clockFixtureIdentity || null,
      planOpen: [...node.querySelectorAll('.field-block.collapsible')]
        .some((block) => block.querySelector('.field-title')?.textContent === 'Plan' && !block.classList.contains('collapsed')),
      draft: node.querySelector('.comment-form textarea')?.value ?? null,
      draftFocused: document.activeElement === node.querySelector('.comment-form textarea'),
      selection: node.querySelector('.comment-form textarea')
        ? [node.querySelector('.comment-form textarea').selectionStart, node.querySelector('.comment-form textarea').selectionEnd]
        : null,
    }));
    const before = await detailState();
    check(before.planOpen, `fixture setup must open Plan: ${JSON.stringify(before)}`);

    await switchClock('local', 'PDT');
    await expectTimes(LOCAL, 'after choosing Local');
    const afterLocal = await detailState();
    check(afterLocal.identity === 'kept', 'Choosing Local must repaint the open task detail in place, not rebuild it');
    check(afterLocal.planOpen, 'Choosing Local must keep the opened Plan section open');
    await page.screenshot({ path: path.join(evidence, 'clock-task-local-dark-1440.png') });

    // A comment draft holds the detail verbatim across refreshes; its thread
    // times still follow the clock, and the draft, caret and focus stay.
    await detail.locator('.actions .action.comment').click();
    const textarea = detail.locator('.comment-form textarea');
    await textarea.fill('Draft written before the switch');
    await textarea.focus();
    await textarea.evaluate((input) => input.setSelectionRange(6, 13));
    await switchClockInPlace('utc', 'UTC');
    await expectTimes(UTC, 'after choosing UTC with a draft open');
    const afterUtc = await detailState();
    check(afterUtc.identity === 'kept', 'Choosing UTC must keep the detail that holds a draft');
    check(afterUtc.draft === 'Draft written before the switch', `The comment draft must survive the switch: ${JSON.stringify(afterUtc)}`);
    check(afterUtc.draftFocused && JSON.stringify(afterUtc.selection) === '[6,13]', `The draft must keep focus and its selection: ${JSON.stringify(afterUtc)}`);
    check(afterUtc.planOpen, 'Choosing UTC must keep the opened Plan section open');
    await switchClock('local', 'PDT');
    await expectTimes(LOCAL, 'after choosing Local again with a draft open');
    const again = await detailState();
    check(again.identity === 'kept' && again.draft === 'Draft written before the switch' && JSON.stringify(again.selection) === '[6,13]',
      `Choosing Local from its button must keep the draft and its selection: ${JSON.stringify(again)}`);
    await page.setViewportSize({ width: 375, height: 812 });
    await page.screenshot({ path: path.join(evidence, 'clock-task-local-dark-375.png') });
    await page.setViewportSize({ width: 1440, height: 900 });

    // A keyed list row whose hash covers only its data (the run row) is
    // rebuilt once the time it shows reads differently.
    await page.evaluate(async () => (await import('/js/router.js')).setActiveTab('jobs/history'));
    await page.locator('#runs-body .when').first().waitFor({ state: 'visible' });
    check(await runTitle() === LOCAL.run, `Run row must open on the Local clock: ${await runTitle()}`);
    await switchClock('utc', 'UTC');
    check(await runTitle() === UTC.run, `An unchanged run row must read UTC after the switch: ${await runTitle()}`);
    await switchClock('local', 'PDT');
    check(await runTitle() === LOCAL.run, `An unchanged run row must read Local after the switch: ${await runTitle()}`);
    check(errors.length === 0, `Clock switching raised page errors: ${errors.join('\n')}`);
  } finally {
    await context.close();
  }
}
