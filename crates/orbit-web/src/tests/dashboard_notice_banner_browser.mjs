import fs from 'node:fs';
import path from 'node:path';

// Fleet-notice banner scope. A banner read answers only the workspace visit it
// started in, and a scope switch clears what the previous scope left. The test
// holds selected `/api/notices` replies and answers them in its own order, so
// no timer decides which reply arrives first.

const ORIGINAL_TITLE = 'Original incident';
const NOTICE = {
  id: 'NOTICE-ONE', kind: 'incident', title: ORIGINAL_TITLE, scope: 'workspace', status: 'open',
  opened_at: '2026-11-02T10:00:00Z', expires_at: '2099-01-01T00:00:00Z', revision: 1,
  body: 'Workspace one is degraded.',
};
const noticesFor = workspace => (workspace === 'two'
  ? { open: [], banner: [], recent: [], can_write: false }
  : { open: [NOTICE], banner: [NOTICE], recent: [], can_write: false });

export async function assertNoticeBannerScope(browser, origin, evidence) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  // While `hold` names a workspace, its next notice read waits in `held`.
  const replies = { hold: null, held: [] };
  const scenarios = [];
  try {
    await page.route('**/api/**', async route => {
      const url = new URL(route.request().url());
      const pathname = url.pathname;
      const workspace = url.searchParams.get('workspace');
      const respond = (status, body) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
      if (pathname === '/api/notices') {
        if (replies.hold === workspace) {
          replies.held.push({
            workspace,
            answer: status => respond(status, status === 200 ? noticesFor(workspace) : { error: 'Notices unavailable' }),
          });
          return;
        }
        await respond(200, noticesFor(workspace));
        return;
      }
      let body = { items: [], total: 0, limit: 50 };
      if (pathname === '/api/workspaces') body = [
        { id: 'one', name: 'one', status: 'active', is_default: true },
        { id: 'two', name: 'two', status: 'active', is_default: false },
      ];
      else if (pathname === '/api/crews') body = { default_crew: null, crews: [] };
      else if (pathname === '/api/routines') body = { routines: [], clock: {} };
      else if (pathname === '/api/tasks') body = { items: [], total: 0, limit: 50 };
      await respond(200, body);
    });

    await page.goto(`${origin}/?workspace=one#tasks`);
    await page.evaluate(async () => {
      globalThis.setInterval = () => 0;
      globalThis.EventSource = class { close() {} };
      await import('/app.js');
    });
    await page.waitForLoadState('networkidle');

    const bannerState = () => page.evaluate(async () => {
      const banner = document.getElementById('notice-banner');
      const text = banner.textContent;
      return {
        banner: banner.hidden ? null : (text.includes('Original incident') ? 'Original incident' : text),
        count: document.getElementById('rail-count-notices').textContent,
        workspace: (await import('/js/common.js')).getWorkspace(),
      };
    });
    const expectBanner = async (expected, when) => {
      const actual = await bannerState();
      for (const key of Object.keys(expected)) {
        check(actual[key] === expected[key], `${when}: ${key} is ${JSON.stringify(actual[key])}, expected ${JSON.stringify(expected[key])} (${JSON.stringify(actual)})`);
      }
      scenarios.push(when);
    };
    const switchTo = id => page.evaluate(async id => (await import('/js/common.js')).setWorkspace(id), id);
    // Starts a banner read and keeps its promise on the page under `name`.
    const startRead = name => page.evaluate(name => {
      globalThis[name] = import('/js/notices.js').then(notices => notices.fetchAndRenderNoticeBanner());
    }, name);
    const finishRead = name => page.evaluate(name => globalThis[name], name);
    const takeHeld = async () => {
      const deadline = Date.now() + 10000;
      while (!replies.held.length) {
        if (Date.now() > deadline) throw new Error('The held notice read never reached the server');
        await new Promise(resolve => setTimeout(resolve, 20));
      }
      replies.hold = null;
      return replies.held.shift();
    };
    const capture = async (state) => {
      for (const width of [1440, 375]) {
        await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
        await page.screenshot({ path: path.join(evidence, `notice-banner-${state}-dark-${width}.png`) });
      }
      await page.setViewportSize({ width: 1440, height: 900 });
    };

    // The banner shows the workspace one visit's own incident.
    await startRead('baseline');
    await finishRead('baseline');
    await expectBanner({ banner: ORIGINAL_TITLE, count: '1', workspace: 'one' }, 'workspace one shows its incident');
    await capture('incident');

    // 1. A workspace-one reply still in flight when the operator moves to two
    // cannot restore one's incident after two's empty read has hidden it.
    replies.hold = 'one';
    await startRead('lateOne');
    const lateOne = await takeHeld();
    await switchTo('two');
    await startRead('twoEmpty');
    await finishRead('twoEmpty');
    await expectBanner({ banner: null, count: '', workspace: 'two' }, 'workspace two shows no banner after its read');
    await lateOne.answer(200);
    await finishRead('lateOne');
    await expectBanner({ banner: null, count: '', workspace: 'two' }, 'a late workspace-one reply must not restore its incident under two');

    // 2. A read started on one's earlier visit stays out after returning to one.
    await switchTo('one');
    replies.hold = 'one';
    await startRead('earlierVisit');
    const earlierVisit = await takeHeld();
    await switchTo('two');
    await switchTo('one');
    await expectBanner({ banner: null, count: '', workspace: 'one' }, 'returning to one shows nothing until its own read completes');
    await earlierVisit.answer(200);
    await finishRead('earlierVisit');
    await expectBanner({ banner: null, count: '', workspace: 'one' }, 'a read from an earlier visit of one must not restore the banner');
    await startRead('currentOne');
    await finishRead('currentOne');
    await expectBanner({ banner: ORIGINAL_TITLE, count: '1', workspace: 'one' }, 'the current visit of one shows its incident');

    // 3. Switching scope clears the previous banner before the new read, and a
    // failed read for the new scope leaves nothing of the previous one.
    await switchTo('two');
    await expectBanner({ banner: null, count: '', workspace: 'two' }, 'switching to two clears the previous banner before its read');
    await capture('cleared');
    replies.hold = 'two';
    await startRead('twoFails');
    const twoFails = await takeHeld();
    await expectBanner({ banner: null, count: '', workspace: 'two' }, 'the banner stays cleared while two is being read');
    await twoFails.answer(500);
    await finishRead('twoFails');
    await expectBanner({ banner: null, count: '', workspace: 'two' }, 'a failed read for two shows none of the previous banner');

    // 4. A failed read within the same visit keeps the banner it already shows.
    await switchTo('one');
    await startRead('oneAgain');
    await finishRead('oneAgain');
    await expectBanner({ banner: ORIGINAL_TITLE, count: '1', workspace: 'one' }, 'workspace one shows its incident again');
    replies.hold = 'one';
    await startRead('oneFails');
    const oneFails = await takeHeld();
    await oneFails.answer(500);
    await finishRead('oneFails');
    await expectBanner({ banner: ORIGINAL_TITLE, count: '1', workspace: 'one' }, 'a failed same-visit read keeps the banner it shows');

    check(errors.length === 0, `Notice banner scope checks raised page errors: ${errors.join('\n')}`);
    fs.writeFileSync(path.join(evidence, 'notice-banner-scope-result.json'), JSON.stringify({ passed: true, checks: scenarios }, null, 2));
  } finally {
    await context.close();
  }
}
