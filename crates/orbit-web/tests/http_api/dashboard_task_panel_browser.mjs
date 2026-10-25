// Usage: node dashboard_task_panel_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/task-panel-browser
// Drives the shipped Tasks view with disposable API data: a row opens its
// detail in the side pane beside a list that keeps its place, the pane's tabs,
// width and keyboard, the full-page route and its way back, deep links, row
// quick actions that leave the pane alone, and the 375px full-screen sheet.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { checkTaskPresentation } from './dashboard_task_presentation_browser.mjs';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

// Runs in the page before the app: a fixed task list and the reads it makes.
const fixture = `
globalThis.EventSource = class { close() {} };
const statuses = ['proposed', 'proposed', 'review', 'in-progress', 'in-progress', ...Array(25).fill('backlog'), 'done'];
const tasks = statuses.map((status, index) => ({
  id: 'PANEL-' + (index + 1),
  title: 'Panel fixture task ' + (index + 1) + ': ' + ['Tighten the readiness card', 'Retire the legacy dock route', 'Measure drain admission latency'][index % 3],
  status, priority: 'medium', projection: 'summary', crew: index % 2 ? 'opus' : null,
  job_run_id: status === 'in-progress' ? 'jrun-fixture-' + (index + 1) : null,
  status_transitions: [{ status: status === 'backlog' ? 'in-progress' : 'backlog' }],
  comment_count: index === 1 ? 0 : 2, artifact_count: 1,
  ...(index === 2 ? {
    external_refs: [{ system: 'github-pr', id: '42', url: 'https://github.com/o/r/pull/42' }],
    delivery_attention: { run_id: 'jrun-delivery-failed', run_state: 'failed', failed_step_id: 'complete_pr', blocker: 'failed_checks', head_sha: 'abc123', attempt: 1, budget: 3, repairing: false, pr_number: '42', as_of: '2026-10-18T12:00:00Z', observed_at: '2026-10-18T12:00:00Z' },
  } : {}),
}));
const sampleComments = [
  { at: '2026-10-18T11:00:00Z', by: 'operator', message: 'Please keep the list still while I read.' },
  { at: '2026-10-18T11:30:00Z', by: 'claude', message: 'Done: the detail now opens in a side panel.' },
];
const sampleCriteria = [
  'Task detail opens a long Acceptance criteria list collapsed, showing its count and the leading criteria, with a Show full AC button that expands it and a Show less button that collapses it again; a short list shows in full with no toggle.',
  'The toggle is a keyboard-reachable button whose aria-expanded reflects its state, and an expanded list stays expanded when the detail panel rebuilds for the same task.',
  'The Edit action on Acceptance criteria still edits every criterion whether the list is collapsed or expanded.',
  'Dark-theme screenshots of the task detail at 1440px and 375px, collapsed and expanded, plus a before shot from the base revision, are attached as task artifacts.',
  'All criteria are cited by number and rendered with inline Markdown formatting preserved.',
  'The count indicator in the field header accurately reflects the total number of acceptance criteria.',
  'Keyboard navigation via Tab reaches the toggle button and Enter/Space triggers the expand/collapse action.',
  'The collapsed mask smoothly fades out the bottom of the list without clipping the field header or count.',
  'Detail panel refresh preserves the expanded state across periodic background polls.',
  'Inline field editor remains fully usable without horizontal overflow or clipped textareas.'
];
const full = task => ({
  ...task, projection: undefined,
  description: task.description || '## Why\\n\\nOperators compare tasks side by side. ' + task.title + ' needs a **clear** spec.\\n\\n- keep the list still\\n- show the detail beside it',
  acceptance_criteria: task.acceptance_criteria || ['The list keeps its scroll position', 'The panel opens beside the list'],
  plan: '1. Read the code\\n2. Change it', complexity: 'medium', type: 'feature', tags: ['dashboard', 'ui'],
  created_by: 'operator', created_at: '2026-10-18T10:00:00Z', updated_at: '2026-10-18T12:00:00Z',
  history: Array.from({ length: 6 }, (_, i) => ({ at: '2026-10-18T1' + i + ':00:00Z', by: 'operator', event: 'updated', note: 'edit ' + (i + 1) })),
  comments: task.comments || (task.id === 'PANEL-2' ? [] : sampleComments),
  artifacts: [{ path: 'notes/summary.txt', size: 42, media_type: 'text/plain' }],
});
globalThis.panelFixture = {
  tasks, reads: [], writes: [], patches: [], sampleCriteria,
  holdCommentPost: false, releaseCommentPost: null, failCommentPost: false,
  ships: [], holdShipPost: false, releaseShipPost: null,
  deliveryClaim: false, readiness: [], lifecycle: [],
};
globalThis.fetch = async (input, options = {}) => {
  const url = new URL(input, location.href);
  const api = url.pathname;
  globalThis.panelFixture.reads.push(api + url.search);
  const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
  if (api === '/api/workspaces') return json([{ id: 'one', name: 'one', status: 'active', is_default: true }]);
  if (api === '/api/hosts') return json({ hosts: [{ name: 'owner', machine_id: 'owner-id', local: true }] });
  if (api === '/api/tasks') return json({ items: globalThis.panelFixture.tasks, total: globalThis.panelFixture.tasks.length, limit: 50, truncated: false });
  if (api === '/api/tasks/locks') return json({ by_task: [], total_locked: 0, total_tasks: 0 });
  if (api === '/api/crews') return json({ default_crew: 'opus', crews: [{ name: 'opus' }, { name: 'sol' }] });
  if (api === '/api/workflows/auto/readiness') return json({ controls_authorized: false, capacity: { drain_phase: 'idle' }, tasks: globalThis.panelFixture.readiness });
  if (api === '/api/distributed/claims') {
    const taskId = url.searchParams.get('task');
    const claims = globalThis.panelFixture.deliveryClaim && ['PANEL-1', 'PANEL-3'].includes(taskId) ? [{
      task_id: taskId, claim_id: 'claim-panel-fixture', phase: 'claimed', last_event: 'claim_bound',
      unsettled: taskId === 'PANEL-3', created_at: '2026-10-18T10:00:00Z', footprint: [],
    }] : [];
    return json({ claims });
  }
  if (api === '/api/job-runs') {
    const id = url.searchParams.get('task_id');
    return json({ items: id ? [
      { run_id: 'jrun-' + id + '-2', job_id: 'ship', state: 'running', started_at: '2026-10-18T12:00:00Z', duration_ms: null },
      { run_id: 'jrun-' + id + '-1', job_id: 'ship', state: 'failed', started_at: '2026-10-18T09:00:00Z', finished_at: '2026-10-18T09:12:00Z', duration_ms: 720000, error_message: 'make ci-fast failed' },
    ] : [], total: id ? 2 : 0, limit: 20 });
  }
  if (api === '/api/workflows/ship' && options.method === 'POST') {
    globalThis.panelFixture.ships.push(JSON.parse(options.body));
    if (globalThis.panelFixture.holdShipPost) {
      globalThis.panelFixture.holdShipPost = false;
      await new Promise(resolve => { globalThis.panelFixture.releaseShipPost = resolve; });
    }
    return json({ run_id: 'jrun-panel-ship', state: 'submitted' });
  }
  const approve = api.match(/^\\/api\\/tasks\\/([^/]+)\\/approve$/);
  if (approve && options.method === 'POST') {
    globalThis.panelFixture.writes.push(api);
    const task = globalThis.panelFixture.tasks.find(item => item.id === decodeURIComponent(approve[1]));
    if (task) task.status = 'backlog';
    return json({ ok: true });
  }
  const comment = api.match(/^\\/api\\/tasks\\/([^/]+)\\/comments$/);
  if (comment && options.method === 'POST') {
    const task = globalThis.panelFixture.tasks.find(item => item.id === decodeURIComponent(comment[1]));
    if (!task) return json({ error: 'not found' }, 404);
    globalThis.panelFixture.writes.push(api);
    if (globalThis.panelFixture.failCommentPost) {
      globalThis.panelFixture.failCommentPost = false;
      return json({ error: 'comment refused' }, 422);
    }
    if (globalThis.panelFixture.holdCommentPost) {
      globalThis.panelFixture.holdCommentPost = false;
      await new Promise(resolve => { globalThis.panelFixture.releaseCommentPost = resolve; });
    }
    const prior = task.comments || (task.id === 'PANEL-2' ? [] : sampleComments);
    task.comments = [...prior, { at: '2026-10-20T12:00:00Z', by: 'human', message: JSON.parse(options.body).message }];
    task.comment_count = task.comments.length;
    return json({ ok: true });
  }
  const lifecycle = api.match(/^\\/api\\/tasks\\/([^/]+)\\/(reject|archive)$/);
  if (lifecycle && options.method === 'POST') {
    globalThis.panelFixture.lifecycle.push({ path: api, body: options.body ? JSON.parse(options.body) : null });
    return json({ ok: true });
  }
  const detail = api.match(/^\\/api\\/tasks\\/([^/]+)$/);
  if (detail) {
    const task = globalThis.panelFixture.tasks.find(item => item.id === decodeURIComponent(detail[1]));
    if (task && options.method === 'PATCH') {
      const body = JSON.parse(options.body);
      globalThis.panelFixture.patches.push({ id: task.id, body });
      if (Object.hasOwn(body, 'crew')) task.crew = body.crew;
      if (body.acceptance_criteria) task.acceptance_criteria = body.acceptance_criteria;
      if (body.context_files) task.context_files = body.context_files;
    }
    return task ? json(full(task)) : json({ error: 'not found' }, 404);
  }
  return json({ items: [], total: 0 });
};
await import('/app.js');
`;

const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, `<script type="module">${fixture}</script>`);
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({ headless: true });
const results = {};

async function openPage({ width = 1440, height = 900, url = '/?workspace=one#tasks', storage = 'ok', context = null, clock = false } = {}) {
  const owner = context || await browser.newContext({ viewport: { width, height } });
  // A viewer whose storage refuses every read and write still gets a pane.
  if (storage === 'refuses') await owner.addInitScript(() => {
    for (const method of ['getItem', 'setItem', 'removeItem']) {
      Storage.prototype[method] = () => { throw new DOMException('storage refused', 'SecurityError'); };
    }
  });
  const page = await owner.newPage();
  if (clock) await page.clock.install();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('dialog', dialog => dialog.accept());
  await page.goto(origin + url);
  await page.locator('#tasks-body .row[data-key="task-PANEL-1"]').waitFor({ state: 'visible' });
  return { page, errors, context: owner };
}

const row = (page, id) => page.locator(`#tasks-body .row[data-key="task-${id}"]`);
const pane = page => page.locator('#task-detail-pane');
const openParam = page => page.evaluate(() => new URLSearchParams(location.hash.split('?')[1] || '').get('open'));
const listState = (page, id) => page.evaluate(id => {
  const body = document.getElementById('tasks-body');
  const node = body.querySelector(`.row[data-key="task-${id}"]`);
  return { scrollTop: body.scrollTop, top: node?.getBoundingClientRect().top, marked: node?.dataset.probe === 'kept' };
}, id);
const focusedRow = page => page.evaluate(() => document.activeElement?.closest?.('#tasks-body .row')?.dataset.key || document.activeElement?.className || null);
// The detail fades in; capture evidence once the pane's animations finish.
const capture = async (page, name) => {
  await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== 'running' || !animation.effect?.target?.closest?.('#task-detail-pane')));
  await page.screenshot({ path: path.join(evidence, name) });
};
const waitShown = async (page, id) => {
  await page.locator(`#task-detail-body #detail-${id}`).waitFor({ state: 'visible' });
  await page.waitForFunction(id => new URLSearchParams(location.hash.split('?')[1] || '').get('open') === id, id);
};

try {
  await checkTaskPresentation({ openPage, row, waitShown, capture, results });
  // Task lifecycle controls follow the active Overview panel; Comment follows
  // the thread in Comments, and neither appears in the other task panels.
  for (const width of [1440, 375]) {
    const { page, context } = await openPage({ width, height: width === 375 ? 812 : 900 });
    await row(page, 'PANEL-1').locator('.title').click();
    await waitShown(page, 'PANEL-1');
    const detail = pane(page).locator('#detail-PANEL-1');
    const tabNames = await detail.locator('[role="tab"]').evaluateAll(nodes => nodes.map(node => node.dataset.tab));
    for (const name of tabNames) {
      await detail.locator(`[role="tab"][data-tab="${name}"]`).click();
      const panel = detail.locator(`#detail-PANEL-1-panel-${name}`);
      if (name === 'overview') {
        const actions = detail.locator('.task-action-footer .lifecycle-actions:visible');
        assert.deepEqual(await actions.locator('.action').allTextContents(), ['approve', 'reject', 'archive'], 'A proposed task keeps its gated lifecycle actions in Overview');
        assert.equal(await panel.locator('.action.comment:visible').count(), 0, 'Comment is absent from Overview');
      } else if (name === 'comments') {
        const footer = detail.locator('.task-action-footer[data-footer-tab="comments"]');
        assert.equal(await footer.locator('.task-action-row.comment-actions:visible').count(), 1, 'Comments has its own sticky Comment action row');
        assert.equal(await footer.getByRole('button', { name: 'comment', exact: true }).isVisible(), true, 'Comment keeps its accessible name in Comments');
        assert.equal(await detail.locator('.task-action-row.lifecycle-actions:visible').count(), 0, 'Lifecycle actions are absent from Comments');
        await panel.locator('.comment-card').first().waitFor({ state: 'visible' });
      } else {
        assert.equal(await panel.locator('.task-action-row:visible').count(), 0, `${name} has no task-level action row`);
      }
      if (['overview', 'runs', 'comments'].includes(name)) {
        if (name === 'runs') await panel.locator('.task-run').first().waitFor({ state: 'visible' });
        if (name === 'overview') await detail.locator('.lifecycle-actions').scrollIntoViewIfNeeded();
        await capture(page, `task-detail-${name}-after-dark-${width}.png`);
      }
    }
    await context.close();
  }

  // Delivery is exposed only for a claim. Its own panel remains while task
  // lifecycle and comment actions stay in their Overview and Comments tabs.
  {
    const { page, errors, context } = await openPage();
    await page.evaluate(() => { globalThis.panelFixture.deliveryClaim = true; });
    await row(page, 'PANEL-1').locator('.title').click();
    await waitShown(page, 'PANEL-1');
    const detail = pane(page).locator('#detail-PANEL-1');
    const deliveryTab = detail.locator('[role="tab"][data-tab="delivery"]');
    await deliveryTab.waitFor({ state: 'visible' });
    await deliveryTab.click();
    const deliveryPanel = detail.locator('#detail-PANEL-1-panel-delivery');
    await deliveryPanel.locator('.claim-panel').waitFor({ state: 'visible' });
    assert.equal(await deliveryPanel.locator('.task-action-row:visible').count(), 0, 'Delivery shows no task-level action row');
    assert.equal(await deliveryPanel.locator('.claim-panel').isVisible(), true, 'The task claim remains in the Delivery panel');
    assert.deepEqual(errors, []);
    await context.close();
    results.deliveryPanelActionSeparation = true;
  }

  // A review task with a live distributed claim must keep the handoff approval
  // safeguard instead of posting the ordinary task approval.
  {
    const { page, errors, context } = await openPage();
    await page.evaluate(() => { globalThis.panelFixture.deliveryClaim = true; });
    await row(page, 'PANEL-3').locator('.title').click();
    await waitShown(page, 'PANEL-3');
    const detail = pane(page).locator('#detail-PANEL-3');
    await detail.locator('[role="tab"][data-tab="overview"]').click();
    await detail.getByRole('button', { name: 'approve', exact: true }).click();
    const refusal = detail.locator('.action-error');
    await refusal.waitFor({ state: 'visible' });
    assert.match(await refusal.innerText(), /claim has not handed off yet/, 'Review approval refuses a claim that has not handed off');
    assert.equal(await page.evaluate(() => globalThis.panelFixture.writes.some(write => write === '/api/tasks/PANEL-3/approve')), false, 'The review safeguard sends no ordinary task approval');
    assert.deepEqual(errors, []);
    await context.close();
    results.reviewClaimApprovalGuard = true;
  }

  // Status gating stays attached to the same handlers after the controls move.
  {
    const { page, errors, context } = await openPage();
    for (const [id, expected] of [
      ['PANEL-3', ['approve', 'reject', 'archive']],
      ['PANEL-6', ['ship', 'reject', 'archive']],
    ]) {
      await row(page, id).locator('.title').click();
      await waitShown(page, id);
      const detail = pane(page).locator(`#detail-${id}`);
      await detail.locator('[role="tab"][data-tab="overview"]').click();
      assert.deepEqual(await detail.locator('.lifecycle-actions .action').allTextContents(), expected, `${id} retains its status-gated lifecycle actions`);
      await detail.locator('[role="tab"][data-tab="comments"]').click();
      assert.equal(await detail.locator('.lifecycle-actions:visible').count(), 0, `${id} has no lifecycle action on Comments`);
      assert.equal(await detail.locator('.comment-actions .action.comment:visible').count(), 1, `${id} keeps Comment on Comments`);
      if (id === 'PANEL-6') {
        await detail.locator('[role="tab"][data-tab="overview"]').click();
        await page.evaluate(() => { globalThis.panelFixture.holdShipPost = true; });
        const ship = detail.locator('.lifecycle-actions .action.ship');
        await ship.click();
        await page.waitForFunction(() => globalThis.panelFixture.ships.length === 1);
        assert.equal(await ship.isDisabled(), true, 'Ship enters its disabled pending state');
        await ship.evaluate(node => node.dispatchEvent(new MouseEvent('click', { bubbles: true })));
        assert.deepEqual(await page.evaluate(() => globalThis.panelFixture.ships), [{ task_ids: ['PANEL-6'] }], 'The duplicate-ship guard sends only one request');
        await page.evaluate(() => globalThis.panelFixture.releaseShipPost());
        await pane(page).waitFor({ state: 'hidden' });
      } else {
        await page.getByRole('button', { name: 'Close task detail' }).click();
        await pane(page).waitFor({ state: 'hidden' });
      }
    }
    assert.deepEqual(errors, []);
    await context.close();
    results.taskActionTabsAndStatusGating = true;
  }

  // An empty thread still owns Comment. Its unsent draft survives a tab switch
  // and the 30-second dashboard refresh, then posts and reloads the thread.
  {
    const { page, errors, context } = await openPage({ clock: true });
    await row(page, 'PANEL-2').locator('.title').click();
    await waitShown(page, 'PANEL-2');
    const detail = pane(page).locator('#detail-PANEL-2');
    const commentsPanel = detail.locator('#detail-PANEL-2-panel-comments');
    await detail.locator('[role="tab"][data-tab="comments"]').click();
    assert.equal(await commentsPanel.getByText('No comments yet.').isVisible(), true, 'The empty Comments tab explains that there is no thread yet');
    const commentFooter = detail.locator('.task-action-footer[data-footer-tab="comments"]');
    assert.equal(await commentFooter.getByRole('button', { name: 'comment', exact: true }).isVisible(), true, 'An empty thread still offers Comment');
    await commentFooter.getByRole('button', { name: 'comment', exact: true }).click();
    const form = commentFooter.locator('.comment-form');
    const draft = 'Keep this unsent comment through tabs and refresh.';
    await form.locator('textarea').fill(draft);
    await detail.locator('[role="tab"][data-tab="overview"]').click();
    assert.equal(await detail.locator('.comment-form:visible').count(), 0, 'The comment composer is hidden outside Comments');
    await detail.locator('[role="tab"][data-tab="comments"]').click();
    assert.equal(await form.locator('textarea').inputValue(), draft, 'Switching tabs keeps the comment draft');

    const readsBefore = await page.evaluate(() => globalThis.panelFixture.reads.filter(read => read.startsWith('/api/tasks?')).length);
    await page.clock.fastForward(30_000);
    await page.waitForFunction(before => globalThis.panelFixture.reads.filter(read => read.startsWith('/api/tasks?')).length > before, readsBefore);
    assert.equal(await form.locator('textarea').inputValue(), draft, 'The 30-second refresh keeps the comment draft');
    assert.equal(await detail.locator('[role="tab"][aria-selected="true"]').getAttribute('data-tab'), 'comments', 'Refresh leaves the Comments tab selected');

    await page.evaluate(() => { globalThis.panelFixture.failCommentPost = true; });
    await form.getByRole('button', { name: 'post', exact: true }).click();
    const failure = detail.locator('.action-error');
    await failure.waitFor({ state: 'visible' });
    assert.match(await failure.innerText(), /comment failed: comment refused/, 'A refused post keeps its error feedback');
    assert.equal(await form.locator('textarea').inputValue(), draft, 'A refused post keeps the unsent draft');
    assert.equal(await form.getByRole('button', { name: 'post', exact: true }).isDisabled(), false, 'A refused post releases the pending state for retry');

    await page.evaluate(() => { globalThis.panelFixture.holdCommentPost = true; });
    await form.getByRole('button', { name: 'post', exact: true }).click();
    await page.waitForFunction(() => globalThis.panelFixture.writes.includes('/api/tasks/PANEL-2/comments'));
    assert.equal(await form.getByRole('button', { name: 'post', exact: true }).isDisabled(), true, 'Posting disables the submit control while pending');
    assert.equal(await form.getByRole('button', { name: 'cancel', exact: true }).isDisabled(), true, 'Posting disables cancel while pending');
    await page.evaluate(() => globalThis.panelFixture.releaseCommentPost());
    await detail.locator('.comment-card').filter({ hasText: draft }).waitFor({ state: 'visible' });
    assert.equal(await detail.locator('.comment-card').count(), 1, 'Posting refreshes the empty thread with the new comment');
    assert.equal(await detail.locator('.comment-card .comment-by').first().innerText(), 'human', 'The refreshed comment keeps the human author');
    assert.equal(await detail.locator('.task-action-footer[data-footer-tab="comments"] .comment-actions .action.comment:visible').count(), 1, 'The Comment action returns after posting');
    assert.deepEqual(errors, []);
    await context.close();
    results.emptyCommentDraftRefreshAndPost = true;
  }

  // Selection opens the pane beside a list that stays where it was.
  {
    const { page, errors, context } = await openPage();
    assert.equal(await pane(page).isHidden(), true, 'No task is selected on a plain visit, so the pane stays closed');
    await page.evaluate(() => { document.getElementById('tasks-body').scrollTop = 260; });
    await row(page, 'PANEL-8').evaluate(node => { node.dataset.probe = 'kept'; });
    // Hovering a row reveals its editors; measure once the pointer is there.
    await row(page, 'PANEL-8').locator('.title').hover();
    await page.waitForTimeout(150);
    const before = await listState(page, 'PANEL-8');
    await row(page, 'PANEL-8').locator('.title').click();
    await waitShown(page, 'PANEL-8');
    const title = row(page, 'PANEL-8').locator('.title');
    assert.equal(await title.getAttribute('aria-expanded'), 'true', 'The selected row reports its open pane');
    assert.equal(await title.getAttribute('aria-controls'), 'task-detail-pane', 'The row disclosure names the pane it opens');
    const selected = await listState(page, 'PANEL-8');
    assert.ok(selected.marked, 'Selecting must keep the row node rather than rebuild the list');
    assert.ok(Math.abs(selected.top - before.top) <= 1, `Selecting must not move the clicked row: ${JSON.stringify({ before, selected })}`);
    const paneBox = await pane(page).boundingBox();
    const listBox = await page.locator('#tasks-body').boundingBox();
    assert.ok(paneBox.x >= listBox.x + listBox.width, 'The pane sits to the right of the list');
    assert.ok(paneBox.width > 1440 * 0.3 && paneBox.width < 1440 * 0.5, `The default pane takes about 40% of the view: ${paneBox.width}`);
    const reads = await page.evaluate(() => globalThis.panelFixture.reads.filter(read => read.startsWith('/api/tasks?')).length);
    await page.locator('#refresh-btn').click();
    await page.waitForFunction(count => globalThis.panelFixture.reads.filter(read => read.startsWith('/api/tasks?')).length > count, reads);
    await page.waitForTimeout(200);
    const refreshed = await listState(page, 'PANEL-8');
    assert.ok(refreshed.marked && refreshed.scrollTop === selected.scrollTop, `A refresh must keep the list's rows and scroll: ${JSON.stringify({ selected, refreshed })}`);
    assert.equal(await openParam(page), 'PANEL-8', 'A refresh keeps the selection');

    // The six tabs reuse the detail renderers; Runs is read only when shown.
    const tabs = page.locator('#task-detail-body [role="tab"]');
    assert.deepEqual((await tabs.allInnerTexts()).map(text => text.replace(/\s*\d+$/, '').trim()), ['Overview', 'Context files', 'History', 'Runs', 'Artifacts', 'Comments']);
    assert.ok(await page.locator('#task-detail-body .acceptance-list, #task-detail-body .ac-list, #task-detail-body li', { hasText: 'The list keeps its scroll position' }).first().isVisible(), 'Overview shows the acceptance criteria');
    assert.equal(await page.evaluate(() => globalThis.panelFixture.reads.filter(read => read.startsWith('/api/job-runs?task_id=PANEL-8')).length), 0, 'Runs are not read until their tab opens');
    const contextTab = page.locator('#task-detail-body [data-tab="context_files"]');
    assert.equal(await contextTab.locator('.detail-tab-count').innerText(), '0', 'Empty editable context stays available with a zero count');
    assert.equal(await page.locator('#task-detail-body .detail-side .file-list').count(), 0, 'Context selectors leave the Overview side column');
    await contextTab.click();
    const contextPanel = page.locator('#' + await contextTab.getAttribute('aria-controls'));
    assert.equal(await contextPanel.getAttribute('aria-labelledby'), await contextTab.getAttribute('id'));
    await page.locator('#detail-PANEL-8 .task-action-footer[data-footer-tab="context_files"]').getByRole('button', { name: 'Edit context files for PANEL-8', exact: true }).click();
    await contextPanel.getByRole('textbox', { name: 'Edit context files for PANEL-8', exact: true }).fill('file:src/main.rs\n dir:docs');
    await contextPanel.getByRole('button', { name: 'save', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('#detail-PANEL-8-tab-context_files .detail-tab-count')?.textContent === '2');
    assert.equal(await contextTab.getAttribute('aria-selected'), 'true', 'Saving keeps Context files selected');
    assert.deepEqual(await contextPanel.locator('.context-selector').evaluateAll(links => links.map(link => link.getAttribute('title').split(' · ')[0])), ['dir:docs', 'file:src/main.rs'], 'full selector identity remains in the link titles, sorted by path');
    assert.deepEqual(await contextPanel.locator('.context-selector').allTextContents(), ['dir:docs', 'src/main.rs'], 'file rows render the path and dir rows render the kind-prefixed name');
    assert.deepEqual(await page.evaluate(() => globalThis.panelFixture.patches.at(-1).body), { context_files: ['file:src/main.rs', 'dir:docs'] }, 'Save patches only selectors');
    await capture(page, 'context-files-dark-1440.png');
    await contextTab.focus();
    await page.keyboard.press('ArrowRight');
    assert.match(await page.locator('#task-detail-body [aria-selected="true"]').innerText(), /^History/, 'ArrowRight advances from Context files');
    await page.keyboard.press('ArrowLeft');
    assert.equal(await contextTab.getAttribute('aria-selected'), 'true');
    await page.keyboard.press('End');
    assert.match(await page.locator('#task-detail-body [aria-selected="true"]').innerText(), /^Comments/, 'End reaches the last tab');

    await page.locator('#task-detail-body').getByRole('tab', { name: /^Runs/ }).click();
    await page.locator('#task-detail-body .task-run').nth(1).waitFor({ state: 'visible' });
    assert.ok((await page.locator('#task-detail-body .task-run-error').innerText()).includes('make ci-fast failed'), 'A failed run shows its error');
    await page.locator('#task-detail-body').getByRole('tab', { name: /^History/ }).click();
    assert.equal(await page.locator('#task-detail-body .history-line:visible').count(), 6, 'History lists every recorded entry');
    await page.locator('#task-detail-body').getByRole('tab', { name: /^Comments/ }).click();
    assert.equal(await page.locator('#task-detail-body .comment-card:visible').count(), 2, 'Comments shows the thread');
    await page.keyboard.press('Home');
    assert.equal(await page.locator('#task-detail-body [role="tab"][aria-selected="true"]').innerText(), 'Overview', 'Home on the tab list returns to Overview');
    await page.keyboard.press('ArrowLeft');
    assert.match(await page.locator('#task-detail-body [role="tab"][aria-selected="true"]').innerText(), /^Comments/, 'ArrowLeft wraps across the tab list');
    await page.locator('#task-detail-body').getByRole('tab', { name: /^Artifacts/ }).click();
    await page.locator('#task-detail-body .artifact-row').first().waitFor({ state: 'visible' });
    await page.locator('#task-detail-body').getByRole('tab', { name: /^Overview/ }).click();
    await capture(page, 'tasks-panel-dark-1440.png');

    // The width follows a drag and the keyboard and is remembered per viewer.
    const resizer = page.locator('#task-pane-resizer');
    const handle = await resizer.boundingBox();
    await page.mouse.move(handle.x + handle.width / 2, handle.y + 200);
    await page.mouse.down();
    await page.mouse.move(handle.x - 200, handle.y + 200, { steps: 5 });
    await page.mouse.up();
    const dragged = Number(await resizer.getAttribute('aria-valuenow'));
    assert.ok(dragged > 50, `Dragging the separator left widens the pane: ${dragged}`);
    assert.ok((await pane(page).boundingBox()).width > paneBox.width + 150, 'The pane follows the drag');
    await resizer.focus();
    await page.keyboard.press('End');
    assert.equal(await resizer.getAttribute('aria-valuenow'), '70', 'End clamps to the widest pane');
    await page.keyboard.press('ArrowLeft');
    assert.equal(await resizer.getAttribute('aria-valuenow'), '70', 'The width stays clamped');
    await page.keyboard.press('Shift+ArrowRight');
    assert.equal(await resizer.getAttribute('aria-valuenow'), '60', 'Shift+ArrowRight narrows by ten points');
    assert.equal(await page.evaluate(() => localStorage.getItem('orbit.dashboard.taskPaneWidth')), '60', 'The width is stored for this viewer');
    await page.reload();
    await row(page, 'PANEL-8').waitFor({ state: 'visible' });
    await waitShown(page, 'PANEL-8');
    assert.equal(await page.locator('#task-pane-resizer').getAttribute('aria-valuenow'), '60', 'A reload restores the stored width and the open= selection');
    await page.locator('#task-pane-resizer').dblclick();
    assert.equal(await page.locator('#task-pane-resizer').getAttribute('aria-valuenow'), '42', 'A double click restores the default width');
    assert.deepEqual(errors, []);
    await context.close();
    results.selection = true;
    results.tabs = true;
    results.resize = true;
  }

  // Storage that refuses costs only the remembered width.
  {
    const { page, errors, context } = await openPage({ storage: 'refuses' });
    await row(page, 'PANEL-6').locator('.title').click();
    await waitShown(page, 'PANEL-6');
    assert.equal(await page.locator('#task-pane-resizer').getAttribute('aria-valuenow'), '42', 'Refused storage falls back to the default width');
    await page.locator('#task-pane-resizer').focus();
    await page.keyboard.press('ArrowLeft');
    assert.equal(await page.locator('#task-pane-resizer').getAttribute('aria-valuenow'), '44', 'Resizing still works when storage refuses');
    assert.deepEqual(errors, [], 'Refused storage raises no page error');
    await context.close();
    results.storageRefused = true;
  }

  // The keyboard moves the selection, opens the full page and closes the pane.
  {
    const { page, errors, context } = await openPage();
    await page.evaluate(() => { document.getElementById('tasks-body').scrollTop = 180; });
    await row(page, 'PANEL-10').locator('.title').click();
    await waitShown(page, 'PANEL-10');
    await page.keyboard.press('j');
    await waitShown(page, 'PANEL-11');
    assert.equal(await focusedRow(page), 'task-PANEL-11', 'j moves focus to the next row');
    await page.keyboard.press('ArrowDown');
    await waitShown(page, 'PANEL-12');
    await page.keyboard.press('k');
    await page.keyboard.press('ArrowUp');
    await waitShown(page, 'PANEL-10');
    // j inside the pane keeps the keyboard in the pane, on the new heading.
    await page.locator('#task-detail-body .task-pane-title').focus();
    await page.keyboard.press('j');
    await waitShown(page, 'PANEL-11');
    assert.equal(await page.evaluate(() => document.activeElement?.id), 'task-pane-title', 'j in the pane focuses the next task heading');
    await row(page, 'PANEL-11').locator('.title').focus();
    const scrollBefore = (await listState(page, 'PANEL-11')).scrollTop;
    await page.keyboard.press('Enter');
    await page.waitForFunction(() => location.hash === '#tasks/PANEL-11');
    assert.ok(await page.locator('#tasks-main.task-page-mode').isVisible(), 'Enter opens the full page');
    assert.equal(await page.locator('.col-tasks').isHidden(), true, 'The full page hides the list');
    assert.equal(await page.evaluate(() => document.activeElement?.id), 'task-pane-title', 'The full page focuses its heading');
    assert.equal(await page.locator('#task-detail-body .task-pane-full').count(), 0, 'The full page offers no second full page');
    await capture(page, 'tasks-page-dark-1440.png');
    await page.getByRole('button', { name: 'Back to list' }).click();
    await waitShown(page, 'PANEL-11');
    assert.ok(await row(page, 'PANEL-11').isVisible(), 'Back returns to the list');
    assert.equal((await listState(page, 'PANEL-11')).scrollTop, scrollBefore, 'Back restores the list scroll');
    assert.equal(await focusedRow(page), 'task-PANEL-11', 'Back returns focus to the selected row');
    await page.keyboard.press('Escape');
    await pane(page).waitFor({ state: 'hidden' });
    assert.equal(await openParam(page), null, 'Esc clears open= from the URL');
    assert.equal(await row(page, 'PANEL-11').locator('.title').getAttribute('aria-expanded'), 'false', 'Esc deselects the row');
    assert.equal(await focusedRow(page), 'task-PANEL-11', 'Esc leaves focus on the row');
    // The pane's close control does the same.
    await row(page, 'PANEL-8').locator('.title').click();
    await waitShown(page, 'PANEL-8');
    await page.getByRole('button', { name: 'Close task detail' }).click();
    await pane(page).waitFor({ state: 'hidden' });
    assert.deepEqual(errors, []);
    await context.close();
    results.keyboard = true;
    results.fullPage = true;
  }

  // Deep links: open= selects and reveals the row; #tasks/<id> is the full page.
  {
    const { page, errors, context } = await openPage({ url: '/?workspace=one#tasks?open=PANEL-28' });
    await waitShown(page, 'PANEL-28');
    // The reveal scrolls smoothly; wait for the row to arrive.
    const visible = await page.waitForFunction(() => {
      const body = document.getElementById('tasks-body').getBoundingClientRect();
      const node = document.querySelector('#tasks-body .row[data-key="task-PANEL-28"]').getBoundingClientRect();
      return node.top >= body.top && node.bottom <= body.bottom;
    }, undefined, { timeout: 3000 }).then(() => true, () => false);
    assert.ok(visible, 'An open= link scrolls its row into view');
    await page.goto(origin + '/?workspace=one#tasks/PANEL-3');
    await page.locator('#task-detail-body #detail-PANEL-3').waitFor({ state: 'visible' });
    assert.ok(await page.locator('#tasks-main.task-page-mode').isVisible(), 'A #tasks/<id> link opens the full page');
    await page.getByRole('button', { name: 'Back to list' }).click();
    await waitShown(page, 'PANEL-3');
    assert.ok(await row(page, 'PANEL-3').isVisible(), 'Back from a linked full page shows the list with the task selected');
    assert.deepEqual(errors, []);
    await context.close();
    results.deepLinks = true;
  }

  // Row quick actions work without opening the pane.
  {
    const { page, errors, context } = await openPage();
    await row(page, 'PANEL-1').locator('.task-quick.approve').click();
    await page.waitForFunction(() => globalThis.panelFixture.writes.includes('/api/tasks/PANEL-1/approve'));
    assert.equal(await pane(page).isHidden(), true, 'Approve from the row leaves the pane closed');
    assert.equal(await openParam(page), null, 'Approve from the row selects nothing');
    await row(page, 'PANEL-1').locator('.task-crew-select').selectOption('sol');
    await page.waitForFunction(() => globalThis.panelFixture.patches.length === 1);
    assert.deepEqual(await page.evaluate(() => globalThis.panelFixture.patches), [{ id: 'PANEL-1', body: { crew: 'sol' } }], 'Crew editing still saves from a list row');
    await row(page, 'PANEL-4').getByRole('link', { name: 'View run' }).click();
    await page.waitForFunction(() => location.hash.startsWith('#runs'));
    assert.match(page.url(), /run_id=jrun-fixture-4/, 'View run opens the task\'s run');
    await page.goBack();
    await row(page, 'PANEL-4').waitFor({ state: 'visible' });
    assert.equal(await pane(page).isHidden(), true, 'View run opened no pane');
    assert.deepEqual(errors, []);
    await context.close();
    results.inlineActions = true;
  }

  // A comment or reject draft keeps the detail node alive across crew saves.
  // The crew control must still follow the saved task so its original value can
  // be selected again and sent as a second update, readiness refreshes must
  // not reset the select, and selecting the workspace default must clear it.
  {
    const { page, errors, context } = await openPage();
    await row(page, 'PANEL-2').locator('.title').click();
    await waitShown(page, 'PANEL-2');
    const crew = pane(page).locator('#detail-PANEL-2-crew');
    assert.equal(await crew.inputValue(), 'opus', 'The detail starts with the task\'s explicit crew');

    // Readiness-only refresh with no draft held repaints in place without resetting crew.
    await page.waitForFunction(() => globalThis.panelFixture.reads.some(r => r.includes('/api/workflows/auto/readiness?task=PANEL-2')));
    await page.waitForFunction(() => {
      const list = document.querySelector('#detail-PANEL-2 .readiness-list');
      return list && list.dataset.drainSignature !== 'true-';
    });
    assert.equal(await crew.inputValue(), 'opus', 'A readiness-only refresh with no draft held preserves explicit crew');

    // An open reject composer also retains the explicit crew.
    await pane(page).getByRole('button', { name: 'reject', exact: true }).click();
    const reject = pane(page).locator('.reject-form');
    await reject.waitFor({ state: 'visible' });
    assert.equal(await crew.inputValue(), 'opus', 'An open reject composer preserves explicit crew');
    await reject.locator('.action.cancel').click();
    await reject.waitFor({ state: 'hidden' });
    assert.equal(await crew.inputValue(), 'opus', 'Canceling the reject composer leaves explicit crew intact');

    // Open comment composer from its Comments tab, including for the empty thread.
    await pane(page).getByRole('tab', { name: /^Comments/ }).click();
    await pane(page).getByRole('button', { name: 'comment', exact: true }).click();
    const comment = pane(page).locator('.comment-form');
    await comment.waitFor({ state: 'visible' });
    assert.equal(await crew.inputValue(), 'opus', 'Opening comment composer preserves the task\'s explicit crew');

    await crew.selectOption('sol');
    await page.waitForFunction(() => globalThis.panelFixture.tasks.find(task => task.id === 'PANEL-2')?.crew === 'sol');
    await page.waitForTimeout(25);
    const firstFeedbackVisible = await pane(page).locator('.crew-cell .mutation-feedback.success').isVisible();
    const draftStillOpenAfterSave = await comment.isVisible();
    assert.equal(await crew.inputValue(), 'sol', 'The draft-held detail reflects the first saved crew');
    assert.equal(await row(page, 'PANEL-2').locator('.task-crew-select').inputValue(), 'sol', 'The list row reflects the saved crew');
    assert.equal(await page.evaluate(() => globalThis.panelFixture.tasks.find(task => task.id === 'PANEL-2').crew), 'sol', 'The API fixture stores the saved crew');
    await capture(page, 'tasks-crew-draft-dark-1440.png');
    await page.setViewportSize({ width: 375, height: 812 });
    await capture(page, 'tasks-crew-draft-dark-375.png');
    await page.setViewportSize({ width: 1440, height: 900 });

    await crew.selectOption('opus');
    await page.waitForFunction(() => globalThis.panelFixture.patches.length === 2, undefined, { timeout: 2000 }).catch(() => {});
    const patches = await page.evaluate(() => globalThis.panelFixture.patches);
    assert.deepEqual(patches, [
      { id: 'PANEL-2', body: { crew: 'sol' } },
      { id: 'PANEL-2', body: { crew: 'opus' } },
    ], 'Reverting to the crew shown before the draft-held change sends a second PATCH');
    assert.equal(firstFeedbackVisible, true, 'A saved crew paints feedback in the draft-held detail');
    assert.equal(draftStillOpenAfterSave, true, 'Saving the crew leaves the comment draft open');
    await page.waitForFunction(() => globalThis.panelFixture.tasks.find(task => task.id === 'PANEL-2')?.crew === 'opus');
    await page.waitForTimeout(25);
    assert.equal(await crew.inputValue(), 'opus', 'The detail shows the reverted crew');
    assert.equal(await row(page, 'PANEL-2').locator('.task-crew-select').inputValue(), 'opus', 'The list row shows the reverted crew');
    assert.equal(await page.evaluate(() => globalThis.panelFixture.tasks.find(task => task.id === 'PANEL-2').crew), 'opus', 'The API fixture stores the reverted crew');
    assert.equal(await comment.isVisible(), true, 'Both crew changes leave the comment draft open');

    // Selecting the workspace default still clears the explicit crew.
    await crew.selectOption('');
    await page.waitForFunction(() => globalThis.panelFixture.patches.length === 3, undefined, { timeout: 2000 }).catch(() => {});
    const patchesAfterDefault = await page.evaluate(() => globalThis.panelFixture.patches);
    assert.deepEqual(patchesAfterDefault, [
      { id: 'PANEL-2', body: { crew: 'sol' } },
      { id: 'PANEL-2', body: { crew: 'opus' } },
      { id: 'PANEL-2', body: { crew: null } },
    ], 'Selecting the workspace default sends a third PATCH with crew: null');
    await page.waitForFunction(() => globalThis.panelFixture.tasks.find(task => task.id === 'PANEL-2')?.crew === null);
    await page.waitForTimeout(25);
    assert.equal(await crew.inputValue(), '', 'The detail select shows the workspace default');
    assert.equal(await row(page, 'PANEL-2').locator('.task-crew-select').inputValue(), '', 'The list row shows the workspace default');
    assert.equal(await page.evaluate(() => globalThis.panelFixture.tasks.find(task => task.id === 'PANEL-2').crew), null, 'The API fixture stores null for the cleared crew');
    assert.equal(await comment.isVisible(), true, 'Selecting default crew leaves the comment draft open');

    await comment.locator('.action.cancel').click();
    await comment.waitFor({ state: 'hidden' });

    // A task with a missing explicit crew preserves it and marks it stale.
    await page.evaluate(() => {
      const task = globalThis.panelFixture.tasks.find(item => item.id === 'PANEL-14');
      if (task) task.crew = 'stale-crew';
    });
    await row(page, 'PANEL-14').locator('.title').click();
    await waitShown(page, 'PANEL-14');
    const staleCrew = pane(page).locator('#detail-PANEL-14-crew');
    assert.equal(await staleCrew.inputValue(), 'stale-crew', 'The detail starts with the missing explicit crew');
    assert.equal(await staleCrew.getAttribute('data-stale-current-value'), 'stale-crew', 'The select records the stale crew in its dataset');
    await pane(page).getByRole('tab', { name: /^Comments/ }).click();
    await pane(page).getByRole('button', { name: 'comment', exact: true }).click();
    await pane(page).locator('.comment-form').waitFor({ state: 'visible' });
    assert.equal(await staleCrew.inputValue(), 'stale-crew', 'Draft-held refresh preserves the missing explicit crew');
    assert.equal(await staleCrew.getAttribute('data-stale-current-value'), 'stale-crew', 'The select retains the stale crew in its dataset');

    assert.deepEqual(errors, []);
    await context.close();
    results.draftHeldCrewRevert = true;
  }

  // Long acceptance criteria open collapsed behind a Show full AC toggle,
  // while short criteria show in full with no toggle. The toggle reflects
  // aria-expanded, toggles via click and keyboard, preserves expanded state
  // across detail rebuilds, and editing criteria shows all criteria in full.
  {
    const { page, errors, context } = await openPage();
    // Short criteria (PANEL-1)
    await row(page, 'PANEL-1').locator('.title').click();
    await waitShown(page, 'PANEL-1');
    const shortDetail = pane(page).locator('#detail-PANEL-1');
    const shortCriteriaBlock = shortDetail.locator('.criteria-block');
    await shortCriteriaBlock.waitFor({ state: 'visible' });
    assert.equal(await shortCriteriaBlock.evaluate(node => node.classList.contains('clamped')), false, 'Short criteria list does not open clamped');
    assert.equal(await shortCriteriaBlock.locator('.ac-toggle').count(), 0, 'Short criteria list has no toggle button');
    assert.equal(await shortCriteriaBlock.locator('.ac-list > li').count(), 2, 'Short criteria list shows all criteria in full');

    // Long criteria (PANEL-15)
    await page.evaluate(() => {
      const task = globalThis.panelFixture.tasks.find(item => item.id === 'PANEL-15');
      if (task) task.acceptance_criteria = globalThis.panelFixture.sampleCriteria;
    });

    await row(page, 'PANEL-15').locator('.title').click();
    await waitShown(page, 'PANEL-15');
    const detail = pane(page).locator('#detail-PANEL-15');
    const criteriaBlock = detail.locator('.criteria-block');
    await criteriaBlock.waitFor({ state: 'visible' });
    assert.equal(await criteriaBlock.evaluate(node => node.classList.contains('clamped')), true, 'Long criteria list opens clamped');
    assert.equal(await criteriaBlock.locator('.field-count').innerText(), '10', 'Long criteria shows total count in header');
    assert.equal(await criteriaBlock.locator('.ac-list > li').count(), 10, 'Long criteria renders leading and remaining list elements');

    const toggle = criteriaBlock.locator('.ac-toggle');
    await toggle.waitFor({ state: 'visible' });
    assert.equal(await toggle.evaluate(node => node.tagName), 'BUTTON', 'Toggle is a button element');
    assert.equal(await toggle.innerText(), 'Show full AC', 'Collapsed toggle button reads "Show full AC"');
    assert.equal(await toggle.getAttribute('aria-expanded'), 'false', 'Collapsed toggle button has aria-expanded="false"');

    // In clamped state, criteria rows must not shrink below their content height or overlap.
    const listHeight = await criteriaBlock.locator('.ac-list').evaluate(node => node.getBoundingClientRect().height);
    assert.ok(listHeight <= 180, 'Clamped criteria list cuts off at clamp height');

    const rowMetrics = await criteriaBlock.evaluate(block => {
      const list = block.querySelector('.ac-list');
      const listRect = list.getBoundingClientRect();
      const rows = Array.from(list.querySelectorAll(':scope > li'));
      return rows.map((li, index) => {
        const rect = li.getBoundingClientRect();
        return {
          index,
          top: rect.top,
          bottom: rect.bottom,
          height: rect.height,
          scrollHeight: li.scrollHeight,
          visible: rect.top < listRect.bottom && rect.bottom > listRect.top,
        };
      });
    });

    const visibleRows = rowMetrics.filter(row => row.visible);
    assert.ok(visibleRows.length >= 2, 'Clamped list renders visible leading criteria rows');
    for (const row of visibleRows) {
      assert.ok(
        row.height >= row.scrollHeight - 1,
        `Visible criterion row ${row.index} bounding box height (${row.height}) must be at least its scrollHeight (${row.scrollHeight})`
      );
    }

    for (let i = 0; i < rowMetrics.length - 1; i++) {
      const current = rowMetrics[i];
      const next = rowMetrics[i + 1];
      assert.ok(
        current.bottom <= next.top + 0.5,
        `Consecutive criteria rows ${i} and ${i + 1} bounding boxes must not intersect (current bottom: ${current.bottom}, next top: ${next.top})`
      );
      assert.ok(
        current.top + current.scrollHeight <= next.top + 1,
        `Consecutive criteria rows ${i} and ${i + 1} content must not overlap (current content bottom: ${current.top + current.scrollHeight}, next top: ${next.top})`
      );
    }

    await capture(page, 'task-detail-collapsed-dark-1440.png');

    // Edit action while collapsed: editor shows every criterion and hides toggle
    const editBtn = criteriaBlock.locator('.field-edit');
    await editBtn.click();
    const textarea = criteriaBlock.locator('.field-editor-input');
    await textarea.waitFor({ state: 'visible' });
    assert.equal(await toggle.isVisible(), false, 'Toggle button is hidden while editing');
    const textareaLines = (await textarea.inputValue()).split('\n').filter(Boolean);
    assert.equal(textareaLines.length, 10, 'Editor includes all 10 criteria while collapsed');
    await criteriaBlock.locator('button.action.cancel').click();
    await textarea.waitFor({ state: 'hidden' });
    assert.equal(await toggle.isVisible(), true, 'Toggle button reappears after cancelling edit');
    assert.equal(await toggle.innerText(), 'Show full AC', 'Toggle button still reads "Show full AC"');

    // Expanding via click
    await toggle.click();
    assert.equal(await criteriaBlock.evaluate(node => node.classList.contains('clamped')), false, 'Clicking toggle expands the criteria list');
    assert.equal(await toggle.innerText(), 'Show less', 'Expanded toggle button reads "Show less"');
    assert.equal(await toggle.getAttribute('aria-expanded'), 'true', 'Expanded toggle button has aria-expanded="true"');

    await capture(page, 'task-detail-expanded-dark-1440.png');

    // Edit action while expanded: editor still shows all criteria
    await editBtn.click();
    await textarea.waitFor({ state: 'visible' });
    assert.equal(await toggle.isVisible(), false, 'Toggle button is hidden while editing expanded criteria');
    assert.equal((await textarea.inputValue()).split('\n').filter(Boolean).length, 10, 'Editor shows all 10 criteria while expanded');
    await criteriaBlock.locator('button.action.cancel').click();
    await textarea.waitFor({ state: 'hidden' });
    assert.equal(await toggle.innerText(), 'Show less', 'Toggle button still reads "Show less" after cancel');
    assert.equal(await toggle.getAttribute('aria-expanded'), 'true', 'Toggle button still has aria-expanded="true" after cancel');

    // Collapsing via keyboard (Enter on focused button)
    await toggle.focus();
    await page.keyboard.press('Enter');
    assert.equal(await criteriaBlock.evaluate(node => node.classList.contains('clamped')), true, 'Pressing Enter collapses the list');
    assert.equal(await toggle.innerText(), 'Show full AC', 'Toggle reads "Show full AC" after keyboard collapse');
    assert.equal(await toggle.getAttribute('aria-expanded'), 'false', 'Toggle has aria-expanded="false" after keyboard collapse');

    // Expanding via keyboard (Space on focused button)
    await page.keyboard.press('Space');
    assert.equal(await criteriaBlock.evaluate(node => node.classList.contains('clamped')), false, 'Pressing Space expands the list');
    assert.equal(await toggle.innerText(), 'Show less', 'Toggle reads "Show less" after keyboard expand');
    assert.equal(await toggle.getAttribute('aria-expanded'), 'true', 'Toggle has aria-expanded="true" after keyboard expand');

    // Expanded state survives detail rebuild for the same task
    await page.locator('#refresh-btn').click();
    await waitShown(page, 'PANEL-15');
    const rebuiltDetail = pane(page).locator('#detail-PANEL-15');
    const rebuiltBlock = rebuiltDetail.locator('.criteria-block');
    await rebuiltBlock.waitFor({ state: 'visible' });
    assert.equal(await rebuiltBlock.evaluate(node => node.classList.contains('clamped')), false, 'Expanded criteria list stays expanded after detail refresh');
    const rebuiltToggle = rebuiltBlock.locator('.ac-toggle');
    assert.equal(await rebuiltToggle.innerText(), 'Show less', 'Rebuilt toggle reads "Show less"');
    assert.equal(await rebuiltToggle.getAttribute('aria-expanded'), 'true', 'Rebuilt toggle has aria-expanded="true"');

    // Clicking another row and clicking back keeps expanded state for PANEL-15
    await row(page, 'PANEL-1').locator('.title').click();
    await waitShown(page, 'PANEL-1');
    await row(page, 'PANEL-15').locator('.title').click();
    await waitShown(page, 'PANEL-15');
    const reopenedBlock = pane(page).locator('#detail-PANEL-15 .criteria-block');
    await reopenedBlock.waitFor({ state: 'visible' });
    assert.equal(await reopenedBlock.evaluate(node => node.classList.contains('clamped')), false, 'Expanded criteria stays expanded when returning to task');
    assert.equal(await reopenedBlock.locator('.ac-toggle').innerText(), 'Show less', 'Toggle button stays "Show less" when returning to task');

    // 375px responsive phone captures
    await page.setViewportSize({ width: 375, height: 812 });
    await capture(page, 'task-detail-expanded-dark-375.png');
    await reopenedBlock.locator('.ac-toggle').click();
    assert.equal(await reopenedBlock.evaluate(node => node.classList.contains('clamped')), true, 'Clicking toggle at 375px collapses the list');

    const rowMetrics375 = await reopenedBlock.evaluate(block => {
      const list = block.querySelector('.ac-list');
      const listRect = list.getBoundingClientRect();
      const rows = Array.from(list.querySelectorAll(':scope > li'));
      return rows.map((li, index) => {
        const rect = li.getBoundingClientRect();
        return {
          index,
          top: rect.top,
          bottom: rect.bottom,
          height: rect.height,
          scrollHeight: li.scrollHeight,
          visible: rect.top < listRect.bottom && rect.bottom > listRect.top,
        };
      });
    });

    const visibleRows375 = rowMetrics375.filter(row => row.visible);
    assert.ok(visibleRows375.length >= 2, 'Clamped list at 375px renders visible leading criteria rows');
    for (const row of visibleRows375) {
      assert.ok(
        row.height >= row.scrollHeight - 1,
        `Visible criterion row ${row.index} at 375px bounding box height (${row.height}) must be at least its scrollHeight (${row.scrollHeight})`
      );
    }
    for (let i = 0; i < rowMetrics375.length - 1; i++) {
      assert.ok(
        rowMetrics375[i].bottom <= rowMetrics375[i + 1].top + 0.5,
        `Consecutive criteria rows ${i} and ${i + 1} at 375px bounding boxes must not intersect`
      );
      assert.ok(
        rowMetrics375[i].top + rowMetrics375[i].scrollHeight <= rowMetrics375[i + 1].top + 1,
        `Consecutive criteria rows ${i} and ${i + 1} at 375px content must not overlap`
      );
    }
    await capture(page, 'task-detail-collapsed-dark-375.png');

    // Copy shots to screenshots directory
    const screenshotDir = path.resolve('.orbit/tmp/screenshots');
    fs.mkdirSync(screenshotDir, { recursive: true });
    for (const name of ['task-detail-collapsed-dark-1440.png', 'task-detail-expanded-dark-1440.png', 'task-detail-collapsed-dark-375.png', 'task-detail-expanded-dark-375.png']) {
      fs.copyFileSync(path.join(evidence, name), path.join(screenshotDir, name));
    }

    assert.deepEqual(errors, []);
    await context.close();
    results.criteriaCollapse = true;
  }

  // Filter membership must not change the full-page crew's label, position,
  // styling or ability to save. Each width compares a done task with a listed
  // task, using the same crew value so their control geometry is comparable.
  for (const width of [1440, 375]) {
    const { page, errors, context } = await openPage({ width, height: width === 375 ? 812 : 900 });
    const layouts = [];
    const shots = [];
    for (const [kind, id] of [['pinned', 'PANEL-31'], ['listed', 'PANEL-5']]) {
      await page.goto(origin + '/?workspace=one&compare=' + id + '#tasks/' + id);
      await page.locator('#task-detail-body .task-levers').waitFor({ state: 'visible' });
      const name = `tasks-page-${kind}-dark-${width}.png`;
      await capture(page, name);
      shots.push(fs.readFileSync(path.join(evidence, name)).toString('base64'));
      assert.equal(await page.locator('#task-detail-body .pinned-external').isVisible(), false, 'The full page has no outside-the-list box');
      assert.equal(await pane(page).locator('.task-crew-select').count(), 1, 'The full page has one crew select');
      const crew = pane(page).locator('.task-levers .task-chip-crew');
      await crew.waitFor({ state: 'visible', timeout: 2000 });
      assert.equal(await crew.locator('label').isVisible(), true, 'The crew has a visible field label');
      assert.equal(await crew.getByLabel('Crew', { exact: true }).count(), 1, 'The visible label names its select');
      layouts.push(await crew.evaluate(node => {
        const levers = node.parentElement;
        const control = node.querySelector('select');
        const style = getComputedStyle(control);
        return {
          afterComplexity: node.previousElementSibling.classList.contains('complexity-cell'),
          beforeHost: node.nextElementSibling.classList.contains('task-chip-host'),
          offset: node.getBoundingClientRect().top - levers.getBoundingClientRect().top,
          height: node.getBoundingClientRect().height,
          style: ['fontFamily', 'fontSize', 'borderRadius', 'color', 'backgroundColor', 'appearance'].map(key => style[key]),
        };
      }));
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), true, 'The full page fits its viewport');
      await crew.locator('select').selectOption('sol');
      await page.waitForFunction(id => globalThis.panelFixture.patches.some(patch => patch.id === id), id);
      await crew.locator('.mutation-feedback.success').waitFor({ state: 'visible' });
      assert.equal(await crew.locator('select').inputValue(), 'sol', 'The full page keeps the saved crew');
      assert.deepEqual(await page.evaluate(() => globalThis.panelFixture.patches), [{ id, body: { crew: 'sol' } }], 'Crew changes target the displayed task');
    }
    assert.ok(layouts.every(layout => layout.afterComplexity && layout.beforeHost), 'Crew sits between complexity and host for either filter membership');
    assert.deepEqual(layouts[0], layouts[1], 'The crew has the same lever-row position and styling inside and outside the filter');
    // A browser-rendered comparison artifact keeps both complete viewports
    // side by side without adding an image-processing dependency.
    const comparison = await context.newPage();
    await comparison.setViewportSize({ width: width * 2, height: width === 375 ? 812 : 900 });
    await comparison.setContent(`<body style="margin:0;display:flex">${shots.map(shot => `<img width="${width}" src="data:image/png;base64,${shot}">`).join('')}</body>`);
    await comparison.locator('img').evaluateAll(images => Promise.all(images.map(image => image.decode())));
    await comparison.screenshot({ path: path.join(evidence, `tasks-page-comparison-dark-${width}.png`) });
    assert.deepEqual(errors, []);
    await context.close();
    results[`crewLayout${width}`] = layouts;
  }

  {
    const { page, errors, context } = await openPage({ url: '/?workspace=one#tasks?open=PANEL-31' });
    await page.locator('#task-detail-body .pinned-note').waitFor({ state: 'visible' });
    await capture(page, 'tasks-dock-pinned-dark-1440.png');
    assert.equal(await page.locator('#task-detail-body .pinned-note').innerText(), 'Not in the current list · done', 'The dock keeps the outside-the-list note');
    assert.equal(await page.locator('#task-detail-body .task-crew-select').count(), 1, 'The dock shows the crew through one select');
    assert.equal(await page.locator('#task-detail-body .task-levers .task-crew-select').count(), 1, 'The dock keeps crew editing in the lever row');
    await page.setViewportSize({ width: 375, height: 812 });
    assert.equal(await pane(page).locator('.pinned-note').isVisible(), true, 'The pinned phone sheet keeps the outside-the-list note');
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), true, 'The pinned phone sheet fits its viewport');
    await capture(page, 'tasks-dock-pinned-dark-375.png');
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.locator('#task-detail-body .task-crew-select').selectOption('opus');
    await page.waitForFunction(() => globalThis.panelFixture.patches.length === 1);
    assert.deepEqual(await page.evaluate(() => globalThis.panelFixture.patches), [{ id: 'PANEL-31', body: { crew: 'opus' } }], 'Changing the crew beside the list saves it to the pinned task');
    await pane(page).locator('.crew-cell .mutation-feedback.success').waitFor({ state: 'visible' });
    await pane(page).getByRole('button', { name: 'Open full page' }).click();
    assert.equal(await pane(page).locator('.pinned-external').isVisible(), false, 'Opening the pinned dock as a page hides the list-only note');
    assert.equal(await pane(page).getByLabel('Crew', { exact: true }).inputValue(), 'opus', 'Opening the full page keeps the saved crew and its label');
    await pane(page).getByRole('button', { name: 'Back to list' }).click();
    await pane(page).locator('.pinned-note').waitFor({ state: 'visible' });
    assert.deepEqual(errors, []);
    await context.close();
  }

  // At 375px the pane is a full-screen modal sheet with Back.
  {
    const { page, errors, context } = await openPage({ width: 375, height: 812 });
    await row(page, 'PANEL-5').locator('.title').click();
    await waitShown(page, 'PANEL-5');
    const sheet = page.locator('#task-detail-pane[role="dialog"][aria-modal="true"]');
    await sheet.waitFor({ state: 'visible' });
    assert.equal(await sheet.getAttribute('aria-labelledby'), 'task-pane-title', 'The sheet is named by its heading');
    assert.deepEqual(await sheet.boundingBox(), { x: 0, y: 0, width: 375, height: 812 }, 'The sheet covers the screen');
    assert.equal(await page.evaluate(() => document.activeElement?.id), 'task-pane-title', 'Opening the sheet focuses its heading');
    assert.equal(await page.evaluate(() => document.getElementById('tasks-body').closest('[inert]') !== null), true, 'The list behind the sheet is inert');
    assert.equal(await page.locator('#task-detail-body .task-pane-close').isVisible(), false, 'The sheet closes with Back, not a corner button');
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), true, 'The sheet does not overflow the phone width');
    const strip = page.locator('#task-detail-body .detail-tabs');
    assert.equal(await strip.evaluate(node => getComputedStyle(node).overflowX), 'auto', 'The phone tab strip scrolls within the sheet');
    await sheet.getByRole('tab', { name: /^Overview/ }).focus();
    await page.keyboard.press('End');
    const commentsTab = sheet.getByRole('tab', { name: /^Comments/ });
    assert.equal(await commentsTab.getAttribute('aria-selected'), 'true', 'Keyboard reaches the last tab on a narrow sheet');
    assert.equal(await commentsTab.evaluate(node => {
      const bounds = node.getBoundingClientRect();
      const strip = node.closest('.detail-tabs').getBoundingClientRect();
      return bounds.left >= strip.left && bounds.right <= strip.right;
    }), true, 'Keyboard focus scrolls the last tab into view');
    await page.keyboard.press('Home');
    await page.keyboard.press('ArrowRight');
    const contextTab = sheet.getByRole('tab', { name: /^Context files/ });
    assert.equal(await contextTab.getAttribute('aria-selected'), 'true', 'Keyboard reaches Context files on the phone');
    await sheet.getByRole('button', { name: 'Edit context files for PANEL-5', exact: true }).click();
    await sheet.getByRole('textbox', { name: 'Edit context files for PANEL-5', exact: true }).fill('file:src/main.rs');
    await sheet.getByRole('button', { name: 'save', exact: true }).click();
    await sheet.locator('.context-selector').waitFor({ state: 'visible' });
    assert.equal(await contextTab.getAttribute('aria-selected'), 'true');
    await capture(page, 'context-files-dark-375.png');
    await sheet.getByRole('tab', { name: /^Overview/ }).click();
    await capture(page, 'tasks-sheet-dark-375.png');
    await sheet.getByRole('button', { name: 'Back', exact: true }).click();
    await pane(page).waitFor({ state: 'hidden' });
    assert.equal(await page.evaluate(() => document.querySelector('[inert]') === null), true, 'Back releases the background');
    assert.equal(await pane(page).getAttribute('role'), null, 'A closed pane is no dialog');
    assert.equal(await openParam(page), null, 'Back clears open= from the URL');
    assert.equal(await focusedRow(page), 'task-PANEL-5', 'Back returns focus to the row');
    assert.deepEqual(errors, []);
    await context.close();
    results.phoneSheet = true;
  }

  fs.writeFileSync(path.join(evidence, 'task-panel-result.json'), JSON.stringify({ passed: true, ...results }, null, 2));
  console.log('Dashboard task panel browser checks passed.');
} finally {
  await browser.close();
  server.close();
}
