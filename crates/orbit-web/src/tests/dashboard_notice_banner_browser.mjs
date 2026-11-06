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

export async function assertNoticeBannerRefresh(browser, origin, evidence) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  const errors = [];
  const apiRequests = [];
  let noticeReads = 0;
  page.on('pageerror', error => errors.push(error.message));
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  try {
    await page.route('**/api/**', async route => {
      const url = new URL(route.request().url());
      apiRequests.push(url.pathname + url.search);
      let body = { items: [], total: 0, limit: 50 };
      if (url.pathname === '/api/notices') {
        noticeReads += 1;
        const title = noticeReads === 1 ? ORIGINAL_TITLE : 'Fresh incident';
        const notice = { ...NOTICE, title, revision: noticeReads };
        body = { open: [notice], banner: [notice], recent: [], can_write: false };
      } else if (url.pathname === '/api/workspaces') {
        body = [{ id: 'one', name: 'one', status: 'active', is_default: true }];
      } else if (url.pathname === '/api/crews') {
        body = { default_crew: null, crews: [] };
      } else if (url.pathname === '/api/routines') {
        body = { routines: [], clock: {} };
      }
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
    });

    await page.goto(`${origin}/?workspace=one#notices`);
    await page.evaluate(async () => {
      globalThis.setInterval = () => 0;
      globalThis.EventSource = class { close() {} };
      const realTimeout = globalThis.setTimeout.bind(globalThis);
      globalThis.__noticePollTimers = [];
      globalThis.setTimeout = (fn, ms, ...args) => {
        if (ms >= 30000 && new Error().stack?.includes('/js/polling.js')) {
          globalThis.__noticePollTimers.push({ run: () => fn(...args), ms });
          return 0;
        }
        return realTimeout(fn, ms, ...args);
      };
      const realNow = Date.now.bind(Date);
      let now = realNow();
      Date.now = () => now;
      globalThis.__advanceNoticeClock = milliseconds => { now += milliseconds; };
      await import('/app.js');
    });
    await page.waitForFunction(() => document.querySelector('.notice-banner-title')?.textContent === 'Original incident');
    await page.waitForFunction(() => document.querySelector('.notice-title')?.textContent === 'Original incident');
    await page.waitForFunction(() => globalThis.__noticePollTimers.length > 0);
    check(noticeReads === 1, `initial banner and Notices panel must share one read, got ${noticeReads}`);
    const capture = async state => {
      for (const width of [1440, 375]) {
        await page.setViewportSize({ width, height: width === 375 ? 1100 : 900 });
        await page.screenshot({
          path: path.join(evidence, `notice-refresh-${state}-dark-${width}.jpg`),
          type: 'jpeg',
          quality: 80,
        });
      }
      await page.setViewportSize({ width: 1440, height: 900 });
    };
    await capture('before');

    apiRequests.length = 0;
    const explicitNoticeResponse = page.waitForResponse(response => new URL(response.url()).pathname === '/api/notices');
    await page.locator('#refresh-btn').click();
    await explicitNoticeResponse;
    await page.waitForFunction(() => document.querySelector('.notice-banner-title')?.textContent === 'Fresh incident');
    await page.waitForFunction(() => document.querySelector('.notice-title')?.textContent === 'Fresh incident');
    check(noticeReads === 2, `explicit Refresh must issue exactly one fresh notices read, got ${noticeReads}`);
    check(apiRequests.filter(request => request.startsWith('/api/notices')).length === 1,
      `explicit Refresh must join its in-flight read between banner and panel, got ${JSON.stringify(apiRequests)}`);
    const refreshedReads = noticeReads;
    await capture('after');

    await page.evaluate(() => globalThis.__advanceNoticeClock(15001));
    apiRequests.length = 0;
    const pollTimerCount = await page.evaluate(() => globalThis.__noticePollTimers.length);
    const pollNoticeResponse = page.waitForResponse(response => new URL(response.url()).pathname === '/api/notices');
    await page.evaluate(() => globalThis.__noticePollTimers.at(-1)?.run());
    await pollNoticeResponse;
    await page.waitForFunction(count => globalThis.__noticePollTimers.length > count, pollTimerCount);
    check(noticeReads === refreshedReads + 1, `scheduled poll should perform one fresh shared notice read, got ${noticeReads - refreshedReads}`);
    const pollNoticeReads = apiRequests.filter(request => request.startsWith('/api/notices')).length;
    check(pollNoticeReads === 1, `scheduled poll must share one notices read across banner and panel, got ${pollNoticeReads}`);
    check(apiRequests.length <= 10, `scheduled Notices poll exceeded ten API requests: ${JSON.stringify(apiRequests)}`);
    check(errors.length === 0, `Notice refresh checks raised page errors: ${errors.join('\n')}`);
    fs.writeFileSync(path.join(evidence, 'notice-refresh-result.json'), JSON.stringify({
      passed: true,
      checks: [
        'initial banner and Notices panel share one in-flight read',
        'explicit Refresh reads and renders a fresh notice within 15 seconds',
        'scheduled poll shares one fresh notice read and stays within ten API requests',
      ],
      scheduled_poll_api_requests: apiRequests.length,
    }, null, 2));
  } finally {
    await context.close();
  }
}

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

// ORB-21548: a numeric revision bump must refresh the banner even when the
// displayed expiry label is unchanged. `text()` maps every numeric revision
// to "", so the old `${text(revision)}:${expiryLabel}` hash kept the keyed
// old node until the label itself changed. The hash now covers the numeric
// revision, the rendered content (kind/title/scope) and both clock labels
// (age/expiry), while titles still render as text nodes.
export async function assertNoticeBannerRevision(browser, origin, evidence) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const scenarios = [];
  try {
    await page.route('**/api/**', async route => {
      const url = new URL(route.request().url());
      let body = { items: [], total: 0, limit: 50 };
      if (url.pathname === '/api/workspaces') body = [
        { id: 'one', name: 'one', status: 'active', is_default: true },
      ];
      else if (url.pathname === '/api/crews') body = { default_crew: null, crews: [] };
      else if (url.pathname === '/api/routines') body = { routines: [], clock: {} };
      else if (url.pathname === '/api/tasks') body = { items: [], total: 0, limit: 50 };
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
    });

    await page.goto(`${origin}/?workspace=one#tasks`);
    await page.evaluate(async () => {
      globalThis.setInterval = () => 0;
      globalThis.EventSource = class { close() {} };
      await import('/app.js');
    });
    await page.waitForSelector('#notice-banner', { state: 'attached' });

    const render = (script, arg) => page.evaluate(script, arg);
    const bannerState = () => page.evaluate(() => ({
      title: document.querySelector('.notice-banner-title')?.textContent,
      meta: document.querySelector('.notice-banner-meta')?.textContent,
      chip: document.querySelector('.notice-banner-row .notice-chip')?.textContent,
      rowClass: document.querySelector('.notice-banner-row')?.className,
      hash: document.querySelector('.notice-banner-row')?.dataset.hash,
      titleChildren: document.querySelector('.notice-banner-title')?.children.length,
      titleHtml: document.querySelector('.notice-banner-title')?.innerHTML,
      hidden: document.getElementById('notice-banner').hidden,
    }));
    const capture = async (state) => {
      for (const width of [1440, 375]) {
        await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
        await page.screenshot({ path: path.join(evidence, `notice-banner-revision-${state}-dark-${width}.png`) });
      }
      await page.setViewportSize({ width: 1440, height: 900 });
    };

    // Freeze the clock so both renders share one expiry label.
    await render(async () => {
      const now = Date.parse('2026-11-02T12:00:00Z');
      globalThis.__revisionNow = now;
      globalThis.__revisionRealNow = Date.now;
      Date.now = () => globalThis.__revisionNow;
      const notices = await import('/js/notices.js');
      globalThis.__renderBanner = payload => notices.renderNoticeBanner(payload);
      globalThis.__bannerBase = {
        id: 'ntc_review', kind: 'incident', scope: 'workspace', status: 'open',
        opened_at: '2026-11-02T11:00:00Z',
        expires_at: '2026-11-02T14:00:00Z',
      };
      const first = { ...globalThis.__bannerBase, revision: 1, title: 'Original incident' };
      globalThis.__renderBanner({ open: [first], banner: [first] });
    });
    try {
      const first = await bannerState();
      check(first.title === 'Original incident', `revision fixture must start on Original incident: ${JSON.stringify(first)}`);
      check(first.meta === 'workspace · 1h old · expires in 2h', `unexpected first banner meta: ${JSON.stringify(first)}`);
      check(first.chip === 'incident', `unexpected first banner chip: ${JSON.stringify(first)}`);
      scenarios.push('revision 1 shows Original incident with its clock labels');
      await capture('before');

      await render(async () => {
        const second = { ...globalThis.__bannerBase, revision: 2, title: 'Updated incident' };
        globalThis.__renderBanner({ open: [second], banner: [second] });
      });
      const second = await bannerState();
      check(second.hash !== first.hash, `numeric revision 1 to 2 must change the banner hash: ${JSON.stringify({ first, second })}`);
      check(second.title === 'Updated incident', `revision 2 kept the stale title ${JSON.stringify(second.title)} with the same expiry label (numeric revision hash): ${JSON.stringify(second)}`);
      check(second.meta === 'workspace · 1h old · expires in 2h', `revision 2 must keep the shared clock labels: ${JSON.stringify(second)}`);
      scenarios.push('revision 2 updates the title with the same expiry label');
      await capture('after');

      await render(async () => {
        const kindScope = { ...globalThis.__bannerBase, revision: 2, title: 'Updated incident', kind: 'info', scope: 'global' };
        globalThis.__renderBanner({ open: [kindScope], banner: [kindScope] });
      });
      const kindScope = await bannerState();
      check(kindScope.chip === 'info', `kind change must update the banner chip: ${JSON.stringify(kindScope)}`);
      check(kindScope.meta === 'global · 1h old · expires in 2h', `scope change must update the banner meta: ${JSON.stringify(kindScope)}`);
      check(kindScope.rowClass.includes('notice-info'), `kind change must update the banner row class: ${JSON.stringify(kindScope)}`);
      scenarios.push('kind and scope update the banner with the same revision and expiry label');

      await render(async () => {
        const aged = { ...globalThis.__bannerBase, revision: 2, title: 'Updated incident', kind: 'info', scope: 'global', opened_at: '2026-11-02T11:50:00Z' };
        globalThis.__renderBanner({ open: [aged], banner: [aged] });
      });
      const aged = await bannerState();
      check(aged.meta === 'global · 10m old · expires in 2h', `age-label change must update the banner with the same expiry label: ${JSON.stringify(aged)}`);
      scenarios.push('age label updates the banner with the same expiry label');

      const markupTitle = '<b>markup</b> & "quoted"';
      await render(async (title) => {
        const markup = { ...globalThis.__bannerBase, revision: 3, title, kind: 'info', scope: 'global', opened_at: '2026-11-02T11:50:00Z' };
        globalThis.__renderBanner({ open: [markup], banner: [markup] });
      }, markupTitle);
      const markup = await bannerState();
      check(markup.title === markupTitle, `markup title must render its text: ${JSON.stringify(markup)}`);
      check(markup.titleChildren === 0, `banner title must stay a text node, not markup: ${JSON.stringify(markup)}`);
      check(!markup.titleHtml.includes('<b>'), `banner title must escape markup: ${JSON.stringify(markup)}`);
      scenarios.push('banner title renders markup as text');
    } finally {
      await render(() => {
        Date.now = globalThis.__revisionRealNow;
        delete globalThis.__revisionNow;
        delete globalThis.__revisionRealNow;
        delete globalThis.__renderBanner;
        delete globalThis.__bannerBase;
      });
    }

    check(errors.length === 0, `Notice banner revision checks raised page errors: ${errors.join('\n')}`);
    fs.writeFileSync(path.join(evidence, 'notice-banner-revision-result.json'), JSON.stringify({ passed: true, checks: scenarios }, null, 2));
  } finally {
    await context.close();
  }
}

// The Notices list: a mixed-severity list scans as one row per notice with
// incidents first, a reason is asked only once Close is chosen, one switch
// shows the two lists, and a refused post or close keeps the operator's words.
// `/api/notices` answers from this test's own list, so no clock or server order
// decides the result.
export async function assertNoticesList(browser, origin, evidence) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const notice = (id, kind, title, extra = {}) => ({
    id, kind, title, scope: 'workspace', status: 'open', revision: 1, body: `${title} body`,
    opened_at: '2026-11-02T10:00:00Z', expires_at: '2099-01-01T00:00:00Z', reports: { count: 1 },
    author: { class: 'operator' }, ...extra,
  });
  const state = {
    can_write: true,
    open: [
      notice('N-INFO', 'info', 'Info first in the reply'),
      notice('N-MAINT', 'maintenance', 'Maintenance second in the reply'),
      notice('N-INC', 'incident', 'Incident last in the reply', { reports: { count: 3 }, linked_task: { id: 'TASK-1' } }),
    ],
    recent: [],
  };
  const writes = [];
  let refuseWrites = false;
  await page.route('**/api/**', async route => {
    const request = route.request();
    const url = new URL(request.url());
    const reply = (body, status = 200) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
    if (request.method() === 'POST' && url.pathname.startsWith('/api/notices')) {
      if (refuseWrites) return reply({ error: 'an operator session is required to write notices', code: 'authorization_denied' }, 403);
      writes.push({ path: url.pathname, body: request.postDataJSON() });
      if (url.pathname === '/api/notices') state.open.push(notice('N-NEW', request.postDataJSON().kind, request.postDataJSON().title));
      else {
        const closed = state.open.find(row => url.pathname.includes(row.id));
        state.open = state.open.filter(row => row !== closed);
        state.recent = [{ ...closed, status: 'closed', ended_at: '2026-11-03T10:00:00Z', ended_label: 'closed by operator: done' }];
      }
      return reply({ notice: {} });
    }
    if (url.pathname === '/api/notices') return reply({ ...state, banner: [] });
    if (url.pathname === '/api/workspaces') return reply([{ id: 'one', name: 'one', status: 'active', is_default: true }]);
    if (url.pathname === '/api/crews') return reply({ default_crew: null, crews: [] });
    if (url.pathname === '/api/routines') return reply({ routines: [], clock: {} });
    return reply({ items: [], total: 0, limit: 50 });
  });
  const titles = () => page.locator('#notices-body .notice-title').allTextContents();
  const row = id => page.locator(`#notices-body [data-key="notice-${id}"]`);
  try {
    await page.goto(`${origin}/?workspace=one#notices`);
    await page.evaluate(async () => {
      globalThis.setInterval = () => 0;
      globalThis.EventSource = class { close() {} };
      await import('/app.js');
    });
    await page.waitForSelector('#notices-body .notice-row');
    check((await titles()).join() === 'Incident last in the reply,Maintenance second in the reply,Info first in the reply',
      `incidents must sort first, got ${await titles()}`);
    const heights = await page.locator('#notices-body .notice-row').evaluateAll(rows => rows.map(node => node.getBoundingClientRect().height));
    check(heights.every(height => height <= 46), `each notice must be one row at 1440, got ${heights}`);
    check(await page.locator('.notice-close-reason').count() === 0, 'no reason input before Close is chosen');
    check(await page.locator('#notices-count').textContent() === '3' && await page.locator('#notices-recent-count').textContent() === '0', 'the switch must show both counts');

    await row('N-INC').locator('.notice-row-toggle').click();
    check((await row('N-INC').locator('.notice-detail').textContent()).includes('3 reports'), 'the open notice must show its reports');
    check(await row('N-INC').locator('.notice-facts a').getAttribute('href') !== null, 'the open notice must link its task');
    check(await page.locator('.notice-close-reason').count() === 0, 'opening a notice must not ask for a reason');

    // A refused close keeps the row, its reason and the server's words.
    refuseWrites = true;
    await row('N-MAINT').locator('.notice-close-open').click();
    await row('N-MAINT').locator('.notice-close-reason').fill('over');
    await row('N-MAINT').locator('.notice-close-submit').click();
    await row('N-MAINT').locator('.notice-feedback').waitFor();
    check((await row('N-MAINT').locator('.notice-feedback').textContent()).includes('operator session'), 'a refused close must say why');
    check(await row('N-MAINT').locator('.notice-close-reason').inputValue() === 'over', 'a refused close must keep its reason');

    // A refused post keeps the dialog and its draft.
    await page.locator('#notices-post-toggle').click();
    await page.locator('#notices-post-title').fill('Posted title');
    await page.locator('input[name="notices-post-kind"][value="incident"]').check();
    await page.locator('#notices-post-body').fill('Posted body');
    await page.locator('#notices-post-submit').click();
    await page.locator('#notices-post-feedback:not(:empty)').waitFor();
    check(await page.locator('#notices-post-title').inputValue() === 'Posted title', 'a refused post must keep its draft');
    refuseWrites = false;
    await page.locator('#notices-post-submit').click();
    await page.waitForFunction(() => !document.getElementById('notices-post-dialog').open);
    await page.waitForFunction(() => document.getElementById('notices-count').textContent === '4');
    const posted = writes.find(write => write.path === '/api/notices');
    check(posted?.body.title === 'Posted title' && posted.body.kind === 'incident' && posted.body.body === 'Posted body' && posted.body.scope === 'workspace'
      && posted.body.linked_task === undefined && posted.body.expires_in_hours === undefined, `title, severity and body must be enough to post: ${JSON.stringify(posted)}`);

    await row('N-MAINT').locator('.notice-close-submit').click();
    await page.waitForFunction(() => document.getElementById('notices-recent-count').textContent === '1');
    check(writes.some(write => write.path === '/api/notices/N-MAINT/close' && write.body.reason === 'over'), 'close must send its reason');
    await page.locator('#notices-tab-recent').click();
    check((await titles()).join() === 'Maintenance second in the reply', `the switch must show one list, got ${await titles()}`);
    check(await page.locator('.notice-close-open').count() === 0, 'an ended notice offers no Close');
    state.recent = [];
    await page.locator('#refresh-btn').click();
    await page.waitForFunction(() => document.querySelector('#notices-body .panel-placeholder:not([data-panel-status])'));
    const note = await page.locator('#notices-body .panel-placeholder:not([data-panel-status])').boundingBox();
    check(note.height <= 40, `the empty view must take one line, got ${note.height}`);
    await page.locator('#notices-tab-open').click();

    // No page scroll at phone width, reading or posting.
    await page.setViewportSize({ width: 375, height: 812 });
    await row('N-INC').locator('.notice-close-open').click();
    const fits = () => page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth
      && document.querySelector('.tab-pane[data-tab="notices"]').scrollWidth <= document.querySelector('.tab-pane[data-tab="notices"]').clientWidth);
    check(await fits(), 'asking a reason must not scroll the page sideways at 375');
    await page.locator('#notices-post-toggle').click();
    check(await fits(), 'the post dialog must not scroll the page sideways at 375');
    await page.locator('#notices-post-cancel').click();
    await page.setViewportSize({ width: 1440, height: 900 });

    // Without an operator session the page reads but offers no write.
    state.can_write = false;
    await page.locator('#refresh-btn').click();
    await page.waitForFunction(() => document.getElementById('notices-post-toggle').disabled);
    check(await page.locator('.notice-close-open').count() === 0 && await page.locator('.notice-close-reason').count() === 0, 'a read-only session offers no Close');
    check((await page.locator('#notices-post-slot .info-hint').textContent()).includes('operator session'), 'a read-only session says why Post is off');
    check(errors.length === 0, `Notices list checks raised page errors: ${errors.join('\n')}`);
    fs.writeFileSync(path.join(evidence, 'notices-list-result.json'), JSON.stringify({ passed: true }, null, 2));
  } finally {
    await page.close();
  }
}
