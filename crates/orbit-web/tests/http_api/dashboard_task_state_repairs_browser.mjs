// Usage: node dashboard_task_state_repairs_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/task-state-repairs
// Drives the shipped Tasks view with disposable API data, one scenario per
// repaired defect: retired pilot and triage guards, copied comment links,
// a saved field beside another draft, related-task navigation from a full page,
// run links in the task's own workspace, and operator-record refusals in
// draft-bearing and pinned details. Deferred responses and Playwright's clock
// order every race; nothing sleeps on wall time. `--before` only labels the
// evidence directory when run against a revision that still has the defects.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const { dashboardFile } = await import(process.env.ORBIT_DASHBOARD_FIXTURE_MODULE || '../../src/tests/dashboard_static.mjs');
const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const fixture = `
globalThis.EventSource = class { close() {} };
const copied = [];
Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async text => { copied.push(String(text)); } } });
const at = '2026-10-20T15:00:00Z';
const longComment = n => 'Comment ' + n + ' body.\\n\\n' + Array.from({ length: 12 }, (_, i) => 'Line ' + (i + 1) + ' of comment ' + n + '.').join('\\n\\n');
const base = {
  priority: 'medium', type: 'feature', complexity: 'medium', crew: 'sol', created_at: at, updated_at: at,
  status_transitions: [], history: [], artifacts: [], tags: [],
  description: 'Original description.', acceptance_criteria: ['Original criterion'],
};
const owed = () => [1, 2].map(criterion => ({ criterion, state: 'owed', host: 'deploy-host', command: 'measure', target: 'passed' }));
const tasks = [
  { ...base, id: 'PIL-1', title: 'Proposed task held by pilot warnings', status: 'proposed', comments: [],
    escalation_triage: { available: true, subject: 'hold', run_id: null },
    pilot_warnings: [{ kind: 'utility_warnings', text: 'A held finding' }] },
  { ...base, id: 'DOC-1', title: 'Task with a long thread', status: 'backlog', resolvable_pilot_admission_hold: true, owed_criteria: owed(),
    comments: Array.from({ length: 10 }, (_, i) => ({ at, by: 'operator', message: longComment(i + 1) })),
    relations: [{ type: 'related_to', target: 'DOC-2' }, { type: 'regression_from', target: 'DOC-2', regression: { nature: 'defect_in_new', escape: 'no_test' } }] },
  { ...base, id: 'DOC-2', title: 'Related task', status: 'backlog', comments: [{ at, by: 'operator', message: 'Only comment.' }] },
  { ...base, id: 'DOC-3', title: 'Finished task outside the filter', status: 'done', resolvable_pilot_admission_hold: true, owed_criteria: owed(), comments: [{ at, by: 'operator', message: 'Done.' }] },
  { ...base, id: 'RUN-1', title: 'Running task owned elsewhere', status: 'in-progress', job_run_id: 'jrun-owned', job_run_navigable: true,
    workspace_id: 'second-owner', comments: [], started_at: at,
    job_run: { run_id: 'jrun-owned', state: 'running', step_id: 'implement', step_index: 1, step_count: 5, started_at: at } },
];
const fx = globalThis.fx = {
  copied, posts: [], patches: [], runPolls: 0, refuseAdmission: true, holdPatch: null, patchRelease: null, holdRecord: false, recordRelease: null, refuseRecord: false,
};
const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
globalThis.fetch = async (input, options = {}) => {
  const url = new URL(input, location.href);
  const method = (options.method || 'GET').toUpperCase();
  const api = url.pathname;
  if (api === '/api/workspaces') return json([{ id: 'one', name: 'one', is_default: true, status: 'active' }, { id: 'two', name: 'two', status: 'active' }]);
  if (api === '/api/hosts') return json({ hosts: [{ name: 'owner', machine_id: 'owner-id', local: true }] });
  if (api === '/api/tasks' || api === '/api/tasks/all') return json({ items: tasks, total: tasks.length, limit: 50, truncated: false });
  if (api === '/api/tasks/locks') return json({ by_task: [], total_locked: 0, total_tasks: 0 });
  if (api === '/api/crews') return json({ default_crew: 'sol', crews: [{ name: 'sol' }] });
  if (api === '/api/workflows/auto/readiness') return json({ controls_authorized: false, capacity: { drain_phase: 'idle' }, tasks: [] });
  const run = /^\\/api\\/runs\\/([^/]+)$/.exec(api);
  if (run) { fx.runPolls += 1; return json({ run: { run_id: run[1], state: 'running' }, steps: [] }); }
  const sub = /^\\/api\\/tasks\\/([^/]+)(\\/.*)?$/.exec(api);
  if (sub) {
    const id = decodeURIComponent(sub[1]);
    const task = tasks.find(candidate => candidate.id === id);
    if (!task) return json({ error: 'not found' }, 404);
    if (!sub[2] && method === 'GET') return json(task);
    if (!sub[2] && method === 'PATCH') {
      const body = JSON.parse(options.body);
      fx.patches.push({ id, body });
      if (fx.holdPatch === Object.keys(body)[0]) await new Promise(resolve => { fx.patchRelease = resolve; });
      Object.assign(task, body);
      return json(task);
    }
    if (sub[2] === '/pilot' || sub[2] === '/triage') {
      fx.posts.push(api);
      return json({ run_id: 'jrun-' + sub[2].slice(1) + '-1', state: 'submitted' });
    }
    if (sub[2] === '/post-deploy') {
      const body = JSON.parse(options.body);
      fx.posts.push({ api, body });
      if (fx.refuseRecord) return json({ code: 'authorization_denied', error: 'Operator authority required' }, 403);
      if (fx.holdRecord) await new Promise(resolve => { fx.recordRelease = resolve; });
      task.owed_criteria.find(entry => entry.criterion === body.criterion).state = 'met';
      return json(task);
    }
    if (sub[2] === '/pilot-admission') {
      fx.posts.push({ api, body: JSON.parse(options.body) });
      if (fx.refuseAdmission) return json({ code: 'authorization_denied', error: 'Operator authority required' }, 403);
      task.resolvable_pilot_admission_hold = false;
      return json(task);
    }
  }
  return json({ items: [], total: 0 });
};
await import('/app.js');
`;

const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  const data = name === '/'
    ? served.data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, `<script type="module">${fixture}</script>`)
    : served.data;
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const results = {};
let browser;

// Every scenario runs even when an earlier one fails, so a run against a
// revision that still has the defects reports each of them.
async function scenario(name, run) {
  try {
    await run();
    results[name] = { passed: true };
  } catch (error) {
    console.error(name, error);
    results[name] = { passed: false, message: String(error.message || error).split('\n')[0] };
  }
}

async function open(hash, { search = '?workspace=one', width = 1440, height = 900, clock = false, dialogs = true } = {}) {
  const context = await browser.newContext({ viewport: { width, height } });
  const page = await context.newPage();
  page.setDefaultTimeout(8000);
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  if (dialogs) page.on('dialog', dialog => dialog.type() === 'confirm' ? dialog.accept() : dialog.accept('Operator evidence.'));
  if (clock) await page.clock.install();
  await page.goto(`${origin}/${search}${hash}`);
  return { page, errors, context };
}

try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });

  // 1. A retired workspace visit releases the local pilot and triage guards.
  await scenario('retiredVisit', async () => {
    const { page, errors, context } = await open('#tasks?status=proposed&open=PIL-1', { clock: true });
    const detail = page.locator('#task-detail-body');
    for (const kind of ['pilot', 'triage']) {
      const button = detail.locator(`button.action.${kind}`);
      await button.waitFor();
      await button.click();
      await detail.locator(`.${kind}-run-link`).waitFor();
      // Leave the workspace before the next poll, then come back.
      await page.evaluate(async () => {
        const select = document.getElementById('workspace-select');
        select.value = 'two';
        select.dispatchEvent(new Event('change', { bubbles: true }));
      });
      await page.clock.fastForward(3500);
      await page.evaluate(async () => {
        const select = document.getElementById('workspace-select');
        select.value = 'one';
        select.dispatchEvent(new Event('change', { bubbles: true }));
      });
      await page.clock.fastForward(3500);
      // Returning shows the task again from the server's own readout.
      await page.locator('#tasks-body .row .title', { hasText: 'Proposed task held by pilot warnings' }).click();
      await page.locator('#task-detail-body button.action.' + kind).filter({ hasText: kind === 'pilot' ? 'run pilot' : 'triage' }).waitFor({ timeout: 5000 });
      assert.equal(await detail.locator(`.${kind}-run-link`).count(), 0, `${kind}: the retired run's guard must be gone`);
    }
    assert.deepEqual(errors, []);
    await context.close();
  });

  // 2. A copied comment link reopens the task and its thread in a fresh page.
  await scenario('commentLink', async () => {
    const { page, context } = await open('#tasks?status=backlog&open=DOC-1');
    await page.locator('#detail-DOC-1-tab-comments').click();
    await page.locator('#comment-DOC-1-9 .comment-action.permalink').click();
    const copied = await page.evaluate(() => fx.copied.at(-1));
    assert.match(copied, /\?workspace=one&window=24h#tasks\?open=DOC-1&comment=9$/);
    await context.close();
    const reopened = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const next = await reopened.newPage();
    await next.goto(copied.replace(/^https?:\/\/[^/]+/, origin));
    const card = next.locator('#comment-DOC-1-9');
    await card.waitFor({ state: 'visible' });
    await next.waitForFunction(() => {
      const box = document.getElementById('comment-DOC-1-9')?.getBoundingClientRect();
      return box && box.top >= 0 && box.top < innerHeight;
    });
    assert.match(await next.evaluate(() => location.hash), /open=DOC-1/);
    assert.match(await next.evaluate(() => location.hash), /comment=9/);
    assert.match(await next.evaluate(() => location.search), /workspace=one/);
    assert.equal(await next.locator('#task-detail-body .task-pane-title').innerText(), 'Task with a long thread');
    await reopened.close();
    // Older copies carried only the fragment; they open the same thread.
    const legacy = await open('#comment-DOC-1-9', { search: '?workspace=one' });
    await legacy.page.locator('#comment-DOC-1-9').waitFor({ state: 'visible' });
    assert.match(await legacy.page.evaluate(() => location.hash), /open=DOC-1/);
    await legacy.context.close();
    // The full page names itself in the link.
    const pageView = await open('#tasks/DOC-1');
    await pageView.page.locator('#detail-DOC-1-tab-comments').click();
    await pageView.page.locator('#comment-DOC-1-2 .comment-action.permalink').click();
    assert.match(await pageView.page.evaluate(() => fx.copied.at(-1)), /#tasks\/DOC-1\?comment=2$/);
    await pageView.context.close();
  });

  // 3. A saved field returns to its view while another draft stays open.
  await scenario('savedField', async () => {
    const { page, errors, context } = await open('#tasks?status=backlog&open=DOC-1');
    const detail = page.locator('#detail-DOC-1');
    // Two simultaneous editors: description (saved) and criteria (a draft).
    await page.evaluate(() => { fx.holdPatch = 'description'; });
    await detail.locator('.description-block button.field-edit').first().click();
    await detail.locator('.criteria-block button.field-edit').first().click();
    await detail.locator('.criteria-block textarea').fill('Original criterion\nAn unsaved second line');
    await detail.locator('.criteria-block textarea').focus();
    await detail.locator('.description-block textarea').fill('Saved description.');
    await detail.locator('.description-block button.action.save').click();
    await detail.locator('.description-block .field-editor-status').filter({ hasText: 'saving' }).waitFor();
    await page.evaluate(() => fx.patchRelease());
    await detail.locator('.description-block textarea').waitFor({ state: 'detached' });
    assert.equal(await detail.locator('.description-block .field-editor-cell .markdown-body').innerText(), 'Saved description.');
    await detail.locator('.description-block .mutation-feedback', { hasText: 'saved' }).waitFor();
    assert.equal(await detail.locator('.description-block button.field-edit').first().isEnabled(), true);
    assert.equal(await detail.locator('.criteria-block textarea').inputValue(), 'Original criterion\nAn unsaved second line');
    // A saved field beside a comment draft.
    await page.evaluate(() => { fx.holdPatch = null; });
    await detail.locator('.criteria-block button.action.cancel').click();
    await detail.locator('#detail-DOC-1-tab-comments').click();
    await detail.locator('button.action.comment').first().click();
    await detail.locator('.comment-form textarea').fill('A half-written comment');
    await detail.locator('#detail-DOC-1-tab-overview').click();
    await detail.locator('.description-block button.field-edit').first().click();
    await detail.locator('.description-block textarea').fill('Saved beside a comment draft.');
    await detail.locator('.description-block button.action.save').click();
    await detail.locator('.description-block .field-editor-cell .markdown-body', { hasText: 'Saved beside a comment draft.' }).waitFor();
    assert.equal(await detail.locator('.comment-form textarea').inputValue(), 'A half-written comment');
    assert.equal(await detail.locator('.comment-form button.action.cancel').isEnabled(), true);
    assert.deepEqual(errors, []);
    await context.close();
  });

  // 4. A related task opened from a full page keeps page, URL and Back in step.
  await scenario('relatedNavigation', async () => {
    const { page, errors, context } = await open('#tasks/DOC-1');
    const title = () => page.locator('#task-detail-body .task-pane-title').innerText();
    await page.locator('#task-detail-body .task-delivery-relation', { hasText: 'DOC-2' }).first().waitFor();
    for (const row of ['related_to', 'regression_from']) {
      await page.locator(`#task-detail-body [data-relation="${row}"] .task-delivery-relation`).first().click();
      await page.waitForFunction(() => location.hash.startsWith('#tasks/DOC-2'));
      assert.equal(await title(), 'Related task');
      assert.equal(await page.locator('#tasks-main.task-page-mode').count(), 1);
      assert.equal(await page.locator('#task-detail-body .task-pane-back').innerText(), 'Back to list');
      await page.goBack();
      await page.waitForFunction(() => location.hash === '#tasks/DOC-1');
      await page.waitForFunction(() => document.querySelector('#task-detail-body .task-pane-title')?.textContent === 'Task with a long thread');
    }
    await page.locator('#task-detail-body [data-relation="related_to"] .task-delivery-relation').first().click();
    await page.waitForFunction(() => location.hash === '#tasks/DOC-2');
    await page.reload();
    await page.waitForFunction(() => document.querySelector('#task-detail-body .task-pane-title')?.textContent === 'Related task');
    // The side pane still follows a relation in place.
    const side = await open('#tasks?status=backlog&open=DOC-1');
    await side.page.locator('#task-detail-body [data-relation="related_to"] .task-delivery-relation').first().click();
    await side.page.waitForFunction(() => document.querySelector('#task-detail-body .task-pane-title')?.textContent === 'Related task');
    assert.match(await side.page.evaluate(() => location.hash), /open=DOC-2/);
    await side.context.close();
    assert.deepEqual(errors, []);
    await context.close();
  });

  // 5. Run links name the task's own workspace in the aggregate view.
  await scenario('runLinks', async () => {
    const { page, context } = await open('#tasks?status=in-progress&open=RUN-1', { search: '?workspace=all&host=owner' });
    const quick = page.locator('#tasks-body a.task-quick-link', { hasText: 'View run' });
    await quick.waitFor();
    const hrefs = [await quick.getAttribute('href')];
    for (const href of hrefs) {
      assert.match(href, /workspace=second-owner/);
      assert.match(href, /host=owner/);
      assert.match(href, /window=/);
      assert.doesNotMatch(href, /workspace=all/);
    }
    const detailLink = page.locator('#task-detail-body .run-value a, #task-detail-body .task-stage-link').first();
    if (await detailLink.count()) assert.match(await detailLink.getAttribute('href'), /workspace=second-owner/);
    await context.close();
  });

  // 6. A refused operator record shows its refusal and gives controls back.
  for (const item of [
    { name: 'listed with a comment draft', hash: '#tasks?status=backlog&open=DOC-1', detail: '#detail-DOC-1', draft: true },
    { name: 'pinned with a comment draft', hash: '#tasks?status=backlog&open=DOC-3', detail: '#task-detail-body .task-detail', draft: true },
    { name: 'pinned without a draft', hash: '#tasks?status=backlog&open=DOC-3', detail: '#task-detail-body .task-detail', draft: false },
  ]) await scenario(`operatorRecord ${item.name}`, async () => {
    const { page, errors, context } = await open(item.hash);
    await page.evaluate(() => { fx.refuseAdmission = true; });
    const detail = page.locator('#task-detail-body');
    const clear = detail.locator('.pilot-admission-clear');
    await clear.waitFor();
    const tab = name => detail.locator(`[id$="-tab-${name}"]`).click();
    if (item.draft) {
      await tab('comments');
      await detail.locator('button.action.comment').first().click();
      await detail.locator('.comment-form textarea').fill('Keep this draft');
      await tab('overview');
    }
    await clear.click();
    await detail.locator('.pilot-admission-hold .operator-record-refusal', { hasText: 'authorization_denied' }).waitFor();
    assert.equal(await clear.isEnabled(), true, `${item.name}: Clear hold must be actionable`);
    assert.equal(await detail.locator('.pilot-admission-approve-anyway').isEnabled(), true, `${item.name}: Approve anyway must be actionable`);
    if (item.draft) {
      await tab('comments');
      assert.equal(await detail.locator('.comment-form textarea').inputValue(), 'Keep this draft');
      assert.equal(await detail.locator('.comment-form button.action.cancel').isEnabled(), true);
      assert.equal(await detail.locator('.comment-form button.action.comment').isEnabled(), true);
    }
    await page.evaluate(() => { fx.refuseAdmission = false; });
    await clear.click();
    await page.waitForFunction(() => fx.posts.filter(post => post.api && post.api.endsWith('/pilot-admission')).length === 2);
    if (item.draft) {
      // The settled record gives back every control it held but its own.
      await page.waitForFunction(() => document.querySelector('#task-detail-body .comment-form button.action.cancel')?.disabled === false);
      assert.equal(await detail.locator('.comment-form textarea').inputValue(), 'Keep this draft');
    }
    assert.deepEqual(errors, []);
    await context.close();
  });
  // A successful record owns only its criterion (or the pilot decision pair).
  // Deferred replies make the retained comment/field draft and focus explicit.
  for (const id of ['DOC-1', 'DOC-3']) for (const draft of ['comment', 'field']) {
    await scenario(`independent records ${id} ${draft}`, async () => {
      const { page, errors, context } = await open(`#tasks?status=backlog&open=${id}`);
      const detail = page.locator('#task-detail-body');
      await detail.locator('.pilot-admission-clear').waitFor();
      if (draft === 'comment') {
        await detail.locator('[id$="-tab-comments"]').click();
        await detail.locator('button.action.comment').first().click();
        await detail.locator('.comment-form textarea').fill('Keep independent record draft');
        await detail.locator('[id$="-tab-overview"]').click();
      } else {
        await detail.locator('.description-block button.field-edit').first().click();
        await detail.locator('.description-block textarea').fill('Keep independent record draft');
      }
      const textarea = detail.locator(draft === 'comment' ? '.comment-form textarea' : '.description-block textarea');
      const marks = detail.locator('.post-deploy-mark-met');
      assert.equal(await marks.count(), 2);
      // A control disabled by its own editor must stay disabled through success.
      const owned = detail.locator(draft === 'comment' ? '.comment-form button.action.comment' : '.description-block button.action.save');
      await owned.evaluate(node => { node.disabled = true; });
      await page.evaluate(() => { fx.refuseRecord = true; });
      await marks.nth(0).click();
      await detail.locator('.owed-criteria .operator-record-refusal', { hasText: 'authorization_denied' }).waitFor();
      assert.equal(await marks.nth(0).isEnabled(), true);
      assert.equal(await marks.nth(1).isEnabled(), true);
      assert.equal(await owned.isEnabled(), false);
      await page.evaluate(() => { fx.refuseRecord = false; fx.holdRecord = true; fx.refuseAdmission = false; });
      await marks.nth(0).click();
      await page.waitForFunction(() => Boolean(fx.recordRelease));
      assert.equal(await marks.nth(1).isEnabled(), false, 'pending record holds the other controls');
      if (draft === 'comment') await detail.locator('[id$="-tab-comments"]').click();
      await textarea.focus();
      await page.evaluate(() => { fx.holdRecord = false; fx.recordRelease(); });
      await page.waitForFunction(() => document.querySelector('#task-detail-body .pilot-admission-clear')?.disabled === false);
      assert.equal(await marks.nth(0).isEnabled(), false, 'settled criterion cannot be recorded twice');
      assert.equal(await marks.nth(1).isEnabled(), true, 'independent owed criterion remains actionable');
      assert.equal(await owned.isEnabled(), false, 'pre-existing disabled state is preserved');
      assert.equal(await textarea.inputValue(), 'Keep independent record draft');
      assert.equal(await textarea.evaluate(node => document.activeElement === node), true, 'refresh preserves draft focus');
      if (draft === 'comment') await detail.locator('[id$="-tab-overview"]').click();
      await marks.nth(0).evaluate(node => node.click());
      assert.equal(await page.evaluate(() => fx.posts.filter(post => post.api?.endsWith('/post-deploy')).length), 2);
      assert.equal(await detail.locator('.owed-criteria .operator-record-refusal').count(), 0, 'success clears a retained refusal');
      // Settling the pilot pair must also give back the independent criterion.
      await detail.locator('.pilot-admission-clear').click();
      await page.waitForFunction(() => document.querySelectorAll('#task-detail-body .post-deploy-mark-met')[1]?.disabled === false);
      assert.equal(await detail.locator('.pilot-admission-approve-anyway').isEnabled(), false);
      assert.equal(await marks.nth(0).isEnabled(), false);
      await page.screenshot({ path: path.join(evidence, `operator-records-${id}-${draft}-dark-1440.png`) });
      await page.setViewportSize({ width: 375, height: 812 });
      await marks.nth(1).scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, `operator-records-${id}-${draft}-dark-375.png`) });
      await marks.nth(1).click();
      await page.waitForFunction(() => fx.posts.filter(post => post.api?.endsWith('/post-deploy')).length === 3);
      assert.equal(await marks.nth(1).isEnabled(), false);
      assert.deepEqual(errors, []);
      await context.close();
    });
  }
  fs.writeFileSync(path.join(evidence, 'task-state-repairs-result.json'), JSON.stringify(results, null, 2));
  const failed = Object.entries(results).filter(([, result]) => !result.passed);
  assert.deepEqual(failed, [], `Task state repair scenarios failed:\n${JSON.stringify(failed, null, 2)}`);
  console.log('Dashboard task state repair checks passed.');
} finally {
  if (browser) await browser.close();
  server.close();
}
