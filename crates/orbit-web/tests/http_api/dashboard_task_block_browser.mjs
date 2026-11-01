// Usage: node dashboard_task_block_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/task-block-browser
// Exercises the shipped detail with disposable API responses. CLI regression
// coverage verifies that core produces these fields from real task records.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const { dashboardFile } = await import(process.env.ORBIT_DASHBOARD_FIXTURE_MODULE || '../../src/tests/dashboard_static.mjs');
const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
const before = process.argv.includes('--before');
fs.mkdirSync(evidence, { recursive: true });
const reason = 'The required validation tool is unavailable on the worker. Restore it before retrying.';
const nextAction = 'Restore the validation tool, then move this task back to backlog.';
const at = '2026-10-20T15:00:00Z';
const fixture = `
globalThis.EventSource = class { close() {} };
const tasks = [
  { id: 'BLOCK-1', title: 'Restore validation for task delivery', status: 'blocked',
    block: { cause: 'infra', reason: ${JSON.stringify(reason)}, at: '${at}', run_id: 'jrun-block-fixture', comment_index: 1, next_action: ${JSON.stringify(nextAction)} },
    next_step: { run_id: 'jrun-block-fixture', resume_command: null, requeue_command: 'orbit task update BLOCK-1 --status backlog' } },
  { id: 'BLOCK-2', title: 'Investigate an undocumented block', status: 'blocked', block: { at: null, reason: null, run_id: null, comment_index: null, next_action: null } },
  { id: 'BLOCK-4', title: 'Decide how to unblock a failed delivery', status: 'blocked',
    block: { cause: 'escalation', reason: 'Required check test-shard-2 failed on head abc123: api::v1::compat panicked after the v1 API was retired.', at: '${at}', run_id: 'jrun-block-fixture', comment_index: 1, next_action: null },
    open_decisions: [{ id: 'DR-BLOCK-4', gate: 'block_triage', expires_at: '2099-01-01T00:00:00Z', answers: ['1', '2', '3'],
      summary: 'A retired API still backs criterion 2, so the test fails on every retry.',
      options: [
        { id: '1', label: 'Drop criterion 2', consequence: 'The retired API stays retired; the task returns to backlog without that criterion.', recommended: false },
        { id: '2', label: 'Restore the v1 API', consequence: 'The v1 API comes back and the task is re-piloted with criterion 2 kept as written.', recommended: true },
        { id: '3', label: 'Split the compat work', consequence: 'Files a follow-up for the compat shim and returns this task to backlog.', recommended: false },
      ] }] },
  { id: 'PILOT-1', title: 'Add a dashboard filter', status: 'proposed',
    pilot_warnings: [{ kind: 'surface_warnings', text: 'The task edits crates/orbit-web/src/api but declares only the dashboard assets.' }],
    open_decisions: [{ id: 'DR-PILOT-1', gate: 'proposed_approval', expires_at: '2099-01-01T00:00:00Z', answers: ['1', '2'],
      summary: 'The declared surface misses the API handler the filter needs.',
      options: [
        { id: '1', label: 'Widen the surface and approve', consequence: 'Adds the API handler to the declared surface and moves the task to backlog.', recommended: true },
        { id: '2', label: 'Archive', consequence: 'The task is archived and never runs.', recommended: false },
      ] }] },
  // A stale projection must never put a block callout on another status.
  { id: 'BLOCK-3', title: 'Continue a queued task', status: 'backlog', block: { reason: 'stale block' } },
].map(task => ({ priority: 'medium', type: 'feature', complexity: 'medium', crew: 'sol',
  description: 'Keep delivery recoverable when a worker needs operator attention.',
  acceptance_criteria: ['Required validation completes on the worker'],
  created_at: '${at}', updated_at: '${at}', status_transitions: [],
  history: [], artifacts: [], comments: [
    { at: '2026-10-19T12:00:00Z', by: 'system', message: 'An older block has already been resolved.' },
    { at: '${at}', by: 'system', message: 'Final recovery requires operator attention.\\n\\n${reason}\\n\\n${nextAction}' },
  ], ...task }));
globalThis.fetch = async input => {
  const url = new URL(input, location.href);
  const json = body => new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
  if (url.pathname === '/api/workspaces') return json([{ id: 'one', name: 'one', is_default: true, status: 'active' }]);
  if (url.pathname === '/api/hosts') return json({ hosts: [{ name: 'owner', machine_id: 'owner-id', local: true }] });
  if (url.pathname === '/api/tasks') return json({ items: tasks, total: tasks.length, limit: 50, truncated: false });
  if (url.pathname === '/api/tasks/locks') return json({ by_task: [], total_locked: 0, total_tasks: 0 });
  if (url.pathname === '/api/crews') return json({ default_crew: 'sol', crews: [{ name: 'sol' }] });
  if (url.pathname === '/api/workflows/auto/readiness') return json({ controls_authorized: false, capacity: { drain_phase: 'idle' }, tasks: [] });
  const id = decodeURIComponent(url.pathname.split('/').pop());
  if (url.pathname.startsWith('/api/tasks/') && tasks.some(task => task.id === id)) return json(tasks.find(task => task.id === id));
  return json({ items: [], total: 0 });
};
await import('/app.js');
`;
const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const served = dashboardFile(name);
  if (!served) { res.writeHead(404); res.end(); return; }
  const data = name === '/' ? served.data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, `<script type="module">${fixture}</script>`) : served.data;
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true });
  for (const width of [1440, 375]) {
    const page = await browser.newPage({ viewport: { width, height: width === 375 ? 812 : 900 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    const origin = `http://127.0.0.1:${server.address().port}/?workspace=one`;
    for (const [view, hash] of [['dock', '#tasks?open=BLOCK-1'], ['page', '#tasks/BLOCK-1']]) {
      await page.goto(origin + hash);
      await page.locator('#detail-BLOCK-1 .task-levers').waitFor({ state: 'visible' });
      if (!before) {
        const callout = page.locator('.task-block-callout');
        await callout.waitFor({ state: 'visible' });
        assert.ok((await callout.innerText()).includes(reason), 'Overview shows the recorded reason');
        assert.ok((await callout.innerText()).includes(nextAction), 'Overview shows the recorded operator action');
        assert.equal(await callout.locator('details.task-block-details[open]').count(), 0, 'Details starts collapsed');
        await callout.locator('details.task-block-details > summary').click();
        assert.equal(await callout.locator('[data-clock-at]').count(), 1, 'The block carries an absolute timestamp');
        assert.equal(await callout.locator('[data-clock-at]').getAttribute('data-clock-at'), at);
        assert.ok((await callout.getByRole('link', { name: 'View run' }).getAttribute('href')).includes('run_id=jrun-block-fixture'));
        assert.equal(await page.locator('#task-detail-body [role="tab"][aria-selected="true"]').innerText(), 'Overview');
        const box = await callout.boundingBox();
        assert.ok(box.x >= 0 && box.x + box.width <= width + 1, 'The block explanation fits the viewport');
      }
      await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== 'running' || !animation.effect?.target?.closest?.('#task-detail-pane')));
      await page.screenshot({ path: path.join(evidence, `task-block-${view}-${before ? 'before-' : ''}dark-${width}.png`) });
    }
    if (!before) {
      await page.locator('.task-block-callout details.task-block-details').evaluate(node => { node.open = true; });
      await page.getByRole('button', { name: 'Read escalation' }).click();
      await page.locator('#comment-BLOCK-1-2').waitFor({ state: 'visible' });
      assert.match(await page.locator('#task-detail-body [role="tab"][aria-selected="true"]').innerText(), /^Comments/, 'The evidence action opens the matching comment');
      await page.goto(origin + '#tasks/BLOCK-2');
      await page.locator('.task-block-callout').waitFor({ state: 'visible' });
      assert.match(await page.locator('.task-block-reason').innerText(), /no.*reason.*recorded/i, 'A block without a reason explicitly says so');
      await page.locator('.task-block-callout details.task-block-details > summary').click();
      assert.match(await page.locator('.task-block-time').innerText(), /not recorded/i, 'An unknown timestamp is explicit');
      await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== 'running' || !animation.effect?.target?.closest?.('#task-detail-pane')));
      await page.screenshot({ path: path.join(evidence, `task-block-unknown-dark-${width}.png`) });
      // The decision card: summary, one button per option with its consequence,
      // the recommended one marked, a note field, and the raw diagnosis under Details.
      await page.goto(origin + '#tasks/BLOCK-4');
      const card = page.locator('.task-block-callout');
      await card.locator('.decision-card').waitFor({ state: 'visible' });
      assert.match(await card.locator('.decision-summary').innerText(), /retired API still backs criterion 2/);
      // The shared renderer lists the recommended option first (ORB-20737); the
      // rest keep server order, so option 1 still precedes option 3.
      assert.deepEqual(await card.locator('.decision-answer').allInnerTexts(), ['Restore the v1 API', 'Drop criterion 2', 'Split the compat work'], 'The recommended option leads and the others keep server order');
      assert.deepEqual(await card.locator('.decision-answer').evaluateAll(nodes => nodes.map(node => node.title)), ['Answer DR-BLOCK-4 with option 2', 'Answer DR-BLOCK-4 with option 1', 'Answer DR-BLOCK-4 with option 3'], 'Each button keeps its own option id');
      assert.deepEqual(await card.locator('.decision-consequence').allTextContents(), ['The v1 API comes back and the task is re-piloted with criterion 2 kept as written.', 'The retired API stays retired; the task returns to backlog without that criterion.', 'Files a follow-up for the compat shim and returns this task to backlog.'], 'Each consequence sits beside its own option');
      assert.equal(await card.locator('.decision-option').first().locator('.decision-recommended').count(), 1, 'The recommended option is the first listed');
      assert.equal(await card.locator('.decision-option.recommended .decision-recommended').count(), 1, 'One option is marked recommended');
      assert.equal(await card.locator('.decision-note').count(), 1, 'The card has a note field');
      assert.ok(!(await card.locator('.task-block-reason, .decision-summary').allInnerTexts()).join(' ').includes('test-shard-2'), 'The raw diagnosis stays out of the card face');
      const cardBox = await card.boundingBox();
      assert.ok(cardBox.x >= 0 && cardBox.x + cardBox.width <= width + 1, 'The decision card fits the viewport');
      await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== 'running' || !animation.effect?.target?.closest?.('#task-detail-pane')));
      await page.screenshot({ path: path.join(evidence, `task-block-decision-dark-${width}.png`) });
      await card.locator('details.task-block-details > summary').click();
      assert.match(await card.locator('.task-block-diagnosis').innerText(), /test-shard-2 failed on head abc123/, 'The raw diagnosis opens under Details');
      await page.screenshot({ path: path.join(evidence, `task-block-decision-details-dark-${width}.png`) });
      await page.goto(origin + '#tasks/PILOT-1');
      const hold = page.locator('.pilot-warnings');
      await hold.locator('.decision-card').waitFor({ state: 'visible' });
      assert.deepEqual(await hold.locator('.decision-answer').allInnerTexts(), ['Widen the surface and approve', 'Archive']);
      assert.equal(await hold.locator('.decision-option.recommended').count(), 1);
      assert.equal(await hold.locator('.decision-note').count(), 1);
      assert.equal(await hold.locator('details.task-block-details[open]').count(), 0, 'The pilot findings start collapsed');
      await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== 'running' || !animation.effect?.target?.closest?.('#task-detail-pane')));
      await page.screenshot({ path: path.join(evidence, `task-pilot-hold-dark-${width}.png`) });
      await page.goto(origin + '#tasks/BLOCK-3');
      await page.locator('#detail-BLOCK-3 .task-levers').waitFor({ state: 'visible' });
      assert.equal(await page.locator('.task-block-callout').count(), 0, 'Other statuses never show a block callout');
    }
    assert.deepEqual(errors, []);
    await page.close();
  }
  fs.writeFileSync(path.join(evidence, 'results.json'), JSON.stringify({ before, widths: [1440, 375], views: ['dock', 'page'], recordedReason: !before, missingReason: !before, otherStatuses: !before, evidenceNavigation: !before }, null, 2));
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
