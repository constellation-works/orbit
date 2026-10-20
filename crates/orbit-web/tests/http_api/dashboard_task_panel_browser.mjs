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
  comment_count: 2, artifact_count: 1,
}));
const full = task => ({
  ...task, projection: undefined,
  description: '## Why\\n\\nOperators compare tasks side by side. ' + task.title + ' needs a **clear** spec.\\n\\n- keep the list still\\n- show the detail beside it',
  acceptance_criteria: ['The list keeps its scroll position', 'The panel opens beside the list'],
  plan: '1. Read the code\\n2. Change it', complexity: 'medium', type: 'feature', tags: ['dashboard', 'ui'],
  created_by: 'operator', created_at: '2026-10-18T10:00:00Z', updated_at: '2026-10-18T12:00:00Z',
  history: Array.from({ length: 6 }, (_, i) => ({ at: '2026-10-18T1' + i + ':00:00Z', by: 'operator', event: 'updated', note: 'edit ' + (i + 1) })),
  comments: [
    { at: '2026-10-18T11:00:00Z', by: 'operator', message: 'Please keep the list still while I read.' },
    { at: '2026-10-18T11:30:00Z', by: 'claude', message: 'Done: the detail now opens in a side panel.' },
  ],
  artifacts: [{ path: 'notes/summary.txt', size: 42, media_type: 'text/plain' }],
});
globalThis.panelFixture = { tasks, reads: [], writes: [], patches: [] };
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
  if (api === '/api/workflows/auto/readiness') return json({ controls_authorized: false, capacity: { drain_phase: 'idle' }, tasks: [] });
  if (api === '/api/job-runs') {
    const id = url.searchParams.get('task_id');
    return json({ items: id ? [
      { run_id: 'jrun-' + id + '-2', job_id: 'ship', state: 'running', started_at: '2026-10-18T12:00:00Z', duration_ms: null },
      { run_id: 'jrun-' + id + '-1', job_id: 'ship', state: 'failed', started_at: '2026-10-18T09:00:00Z', finished_at: '2026-10-18T09:12:00Z', duration_ms: 720000, error_message: 'make ci-fast failed' },
    ] : [], total: id ? 2 : 0, limit: 20 });
  }
  const approve = api.match(/^\\/api\\/tasks\\/([^/]+)\\/approve$/);
  if (approve && options.method === 'POST') {
    globalThis.panelFixture.writes.push(api);
    const task = globalThis.panelFixture.tasks.find(item => item.id === decodeURIComponent(approve[1]));
    if (task) task.status = 'backlog';
    return json({ ok: true });
  }
  const detail = api.match(/^\\/api\\/tasks\\/([^/]+)$/);
  if (detail) {
    const task = globalThis.panelFixture.tasks.find(item => item.id === decodeURIComponent(detail[1]));
    if (task && options.method === 'PATCH') {
      const body = JSON.parse(options.body);
      globalThis.panelFixture.patches.push({ id: task.id, body });
      task.crew = body.crew;
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

async function openPage({ width = 1440, height = 900, url = '/?workspace=one#tasks', storage = 'ok', context = null } = {}) {
  const owner = context || await browser.newContext({ viewport: { width, height } });
  // A viewer whose storage refuses every read and write still gets a pane.
  if (storage === 'refuses') await owner.addInitScript(() => {
    for (const method of ['getItem', 'setItem', 'removeItem']) {
      Storage.prototype[method] = () => { throw new DOMException('storage refused', 'SecurityError'); };
    }
  });
  const page = await owner.newPage();
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

    // The five tabs reuse the detail renderers; Runs is read only when shown.
    const tabs = page.locator('#task-detail-body [role="tab"]');
    assert.deepEqual((await tabs.allInnerTexts()).map(text => text.replace(/\s*\d+$/, '').trim()), ['Overview', 'History', 'Runs', 'Artifacts', 'Comments']);
    assert.ok(await page.locator('#task-detail-body .acceptance-list, #task-detail-body .ac-list, #task-detail-body li', { hasText: 'The list keeps its scroll position' }).first().isVisible(), 'Overview shows the acceptance criteria');
    assert.equal(await page.evaluate(() => globalThis.panelFixture.reads.filter(read => read.startsWith('/api/job-runs?task_id=PANEL-8')).length), 0, 'Runs are not read until their tab opens');
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
    assert.equal(await page.evaluate(() => {
      const strip = document.querySelector('#task-detail-body .detail-tabs');
      return strip.scrollWidth <= strip.clientWidth;
    }), true, 'All five tabs fit the phone sheet');
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
