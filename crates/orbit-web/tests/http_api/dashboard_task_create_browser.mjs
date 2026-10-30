// Real shipped composer and HTTP creation in a disposable browser profile.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const [modulePath, evidencePath, origin] = process.argv.slice(2);
const { chromium } = await import(pathToFileURL(modulePath).href);
fs.mkdirSync(evidencePath, { recursive: true });
const browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
const errors = [];
page.on('pageerror', error => errors.push(String(error)));
// Other dashboard panels need no state for this composer boundary fixture.
await page.route('**/static/app.js', route => route.fulfill({ contentType: 'text/javascript', body: '' }));
const requests = [];
let mode = 'normal';
let held;
let committed;
await page.route('**/api/tasks/create?*', async route => {
  requests.push({ url: route.request().url(), body: route.request().postDataJSON() });
  if (mode === 'denied') {
    await route.fulfill({ status: 403, contentType: 'application/json', body: JSON.stringify({ error: 'Operator permission revoked' }) }); return;
  }
  if (mode === 'lost') {
    const response = await route.fetch();
    assert.equal(response.status(), 200);
    committed = await response.json();
    await route.abort('failed'); return;
  }
  if (mode === 'held') {
    const response = await route.fetch();
    committed = await response.json();
    await new Promise(resolve => { held = resolve; });
    await route.fulfill({ response }); return;
  }
  await route.continue();
});
const boot = async () => page.evaluate(async () => {
  const common = await import('/static/js/common.js');
  window.fixtureCommon = common;
  common.setRegisteredHosts([{ name: 'fixture-host', machine_id: 'fixture', local: true }]);
  common.setMultiWorkspace(true);
  common.setWorkspace('ws_http_fixture');
  const rows = await fetch('/api/workspaces').then(response => response.json());
  window.fixtureWorkspaces = rows;
  const { initTaskCreate } = await import('/static/js/task-create.js');
  initTaskCreate({ getWorkspaces: () => window.fixtureWorkspaces });
  document.querySelector('.tab-pane[data-tab="tasks"]').classList.add('active');
});
const open = () => page.getByRole('button', { name: 'New task', exact: true }).click();
const fill = async title => {
  await page.getByLabel('Title', { exact: true }).fill(title);
  await page.getByLabel('Description', { exact: true }).fill('A human draft that must survive errors');
  await page.getByLabel('Assessed complexity').selectOption('low');
  await page.getByLabel('Acceptance criteria (one per line)').fill('One saved proposal');
};
const submit = () => page.locator('#task-create-submit').click();
const feedback = () => page.locator('#task-create-feedback').textContent();
const waitFeedback = text => page.waitForFunction(value => document.getElementById('task-create-feedback').textContent.includes(value), text);
try {
  await page.goto(`${origin}/#tasks`); await boot();
  await open(); await fill('Cancelled draft');
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  assert.equal(requests.length, 0, 'cancel writes nothing');
  await page.waitForFunction(() => document.getElementById('task-create-open').getAttribute('aria-expanded') === 'false');
  assert.equal(await page.locator('#task-create-open').evaluate(node => document.activeElement === node), true, 'Cancel returns focus to New task');
  // The dialog is a labelled modal: focus starts in Title and stays inside it,
  // the board behind it is inert, and Escape closes it without losing the draft.
  await open(); await fill('Escaped draft');
  assert.equal(await page.getByRole('dialog', { name: 'Propose a task' }).count(), 1);
  assert.equal(await page.locator('#task-create-panel').evaluate(node => node.matches(':modal')), true);
  assert.equal(await page.locator('#task-create-open').getAttribute('aria-expanded'), 'true');
  assert.equal(await page.evaluate(() => { const search = document.getElementById('task-search'); search.focus(); return document.activeElement === search; }), false, 'background is inert');
  for (const key of ['Tab', 'Tab', 'Tab', 'Tab', 'Tab', 'Tab', 'Tab', 'Tab', 'Tab', 'Shift+Tab', 'Shift+Tab']) {
    await page.keyboard.press(key);
    assert.equal(await page.evaluate(() => document.getElementById('task-create-panel').contains(document.activeElement)), true, `${key} keeps focus in the dialog`);
  }
  await page.keyboard.press('Escape');
  assert.ok(await page.locator('#task-create-panel').isHidden(), 'Escape closes the dialog');
  // The dialog's close event is queued, so its follow-up settles a tick later.
  await page.waitForFunction(() => document.getElementById('task-create-open').getAttribute('aria-expanded') === 'false');
  assert.equal(await page.locator('#task-create-open').evaluate(node => document.activeElement === node), true, 'Escape returns focus to New task');
  assert.equal(requests.length, 0, 'Escape writes nothing');
  await open();
  assert.equal(await page.getByLabel('Title', { exact: true }).inputValue(), 'Escaped draft', 'Escape keeps an unsent draft');
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await open();
  assert.equal(await page.getByLabel('Title', { exact: true }).inputValue(), '', 'Cancel discards an unsent draft');
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await open(); await fill('   '); await submit();
  await waitFeedback('meaningful title');
  assert.equal(requests.length, 0, 'whitespace input never submits');
  assert.equal(await page.getByLabel('Description', { exact: true }).inputValue(), 'A human draft that must survive errors');
  const unsafe = '<img src=x onerror="window.injected=true">';
  await fill(unsafe);
  mode = 'lost'; await submit(); await waitFeedback('retry to recover');
  assert.equal(requests.length, 1);
  assert.ok(committed.id);
  assert.equal(await page.getByLabel('Title', { exact: true }).inputValue(), unsafe);
  assert.ok(await page.getByLabel('Title', { exact: true }).isDisabled(), 'uncertain content is frozen');
  const originalRequest = requests[0];
  await page.reload(); await boot(); await open();
  await waitFeedback('may already exist');
  assert.equal(await page.getByLabel('Title', { exact: true }).inputValue(), unsafe);
  mode = 'denied'; await submit(); await waitFeedback('permission revoked');
  assert.ok(await page.getByLabel('Title', { exact: true }).isDisabled(), 'permission refusal after uncertainty retains the frozen request');
  assert.deepEqual(requests[1], originalRequest);
  // Switching workspace hides the pending form. Opening another workspace
  // cannot display its receipt or create another task with a fresh key.
  await page.evaluate(() => window.fixtureCommon.setWorkspace('ws_other'));
  assert.ok(await page.locator('#task-create-panel').isHidden());
  await open(); assert.ok(await page.locator('#task-create-submit').isDisabled());
  assert.ok(await page.locator('#task-create-success').isHidden());
  await page.evaluate(() => window.fixtureCommon.setWorkspace('ws_http_fixture'));
  await open(); mode = 'normal'; await submit(); await waitFeedback('Task saved');
  assert.deepEqual(requests[2], originalRequest, 'reload and workspace retry preserve target, key and payload');
  const link = page.locator('#task-create-success');
  assert.ok((await link.getAttribute('href')).includes(encodeURIComponent(committed.id)));
  assert.ok((await link.getAttribute('href')).includes('workspace=ws_http_fixture'));
  assert.ok((await link.getAttribute('href')).includes('host=fixture-host'));
  assert.equal(await page.evaluate(() => window.injected), undefined, 'draft and receipt render safely');
  const task = await page.request.get(`${origin}/api/tasks/${committed.id}?workspace=ws_http_fixture`);
  assert.equal((await task.json()).status, 'proposed');
  await page.getByRole('button', { name: 'New proposal' }).click();
  await fill('Held response stays with original scope');
  const beforeHeld = requests.length;
  mode = 'held'; await submit();
  await page.waitForFunction(() => document.getElementById('task-create-form').getAttribute('aria-busy') === 'true');
  for (let i = 0; i < 100 && !held; i++) await new Promise(resolve => setTimeout(resolve, 20));
  assert.ok(held);
  await page.locator('#task-create-form').evaluate(form => form.dispatchEvent(new Event('submit', { cancelable: true })));
  assert.equal(requests.length, beforeHeld + 1, 'double submit does not start another request');
  await page.keyboard.press('Escape');
  assert.ok(await page.locator('#task-create-panel').isHidden(), 'an in-flight submission can be dismissed');
  await open();
  assert.equal(await feedback(), 'Creating proposed task…', 'reopening shows the request still in flight');
  assert.ok(await page.locator('#task-create-submit').isDisabled(), 'reopening never resends an in-flight request');
  assert.ok(await page.getByLabel('Title', { exact: true }).isDisabled());
  await page.getByRole('button', { name: 'Close', exact: true }).click();
  assert.equal(requests.length, beforeHeld + 1, 'dismissal neither cancels nor resends');
  await page.evaluate(() => window.fixtureCommon.setHost('other-host'));
  held();
  await page.waitForTimeout(100);
  assert.ok(await page.locator('#task-create-panel').isHidden(), 'late success cannot reopen another host view');
  await open();
  assert.ok(await page.locator('#task-create-success').isHidden(), 'late result cannot leak into another host');
  assert.ok(await page.locator('#task-create-submit').isDisabled());
  await page.evaluate(() => { window.fixtureCommon.setHost(null); window.fixtureCommon.setWorkspace('ws_http_fixture'); });
  await open(); await waitFeedback('Task saved');
  // Aggregate mode requires a choice; server validation retains fields and
  // allows an explicit corrected submission following a definite refusal.
  await page.getByRole('button', { name: 'New proposal' }).click();
  await page.evaluate(() => window.fixtureCommon.setWorkspace(null));
  await open(); await fill('Invalid assessed input');
  assert.equal(await page.locator('#task-create-workspace').inputValue(), '');
  await page.locator('#task-create-workspace').selectOption('ws_http_fixture');
  await page.evaluate(() => {
    const option = document.createElement('option'); option.value = 'unassessed'; option.textContent = 'Unassessed';
    document.getElementById('task-create-complexity').append(option);
    document.getElementById('task-create-complexity').value = 'unassessed';
  });
  mode = 'normal'; await submit(); await waitFeedback('Correct the fields');
  assert.equal(await page.getByLabel('Title', { exact: true }).inputValue(), 'Invalid assessed input');
  assert.ok(await page.getByLabel('Title', { exact: true }).isEnabled());
  await page.getByLabel('Assessed complexity').selectOption('medium');
  await submit(); await waitFeedback('Task saved');
  await page.getByRole('button', { name: 'New proposal' }).click();
  await fill('First permission refusal preserves an editable draft');
  await page.locator('#task-create-workspace').selectOption('ws_http_fixture');
  mode = 'denied'; await submit(); await waitFeedback('Correct the fields');
  assert.ok(await page.getByLabel('Title', { exact: true }).isEnabled(), 'a first definite permission refusal allows correction');
  mode = 'normal'; await submit(); await waitFeedback('Task saved');
  await page.setViewportSize({ width: 375, height: 800 });
  await page.locator('#task-create-success').scrollIntoViewIfNeeded();
  await page.screenshot({ path: path.join(evidencePath, 'proposal-mobile.png') });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, 'mobile has no page overflow');
  // Also boot the complete application so module registration and workspace
  // loading are exercised without the focused composer's boot seam.
  const full = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  full.on('pageerror', error => errors.push(String(error)));
  await full.goto(`${origin}/#tasks`);
  await full.waitForFunction(() => document.querySelector('#task-create-open')?.getAttribute('aria-expanded') === 'false');
  await full.waitForFunction(async () => (await import('/static/js/common.js')).getWorkspace() === 'ws_http_fixture');
  await full.waitForFunction(() => !document.querySelector('#tasks-body .skeleton-state'));
  // Opening, validating, succeeding and closing the dialog must leave the board
  // and its Drain / Locks held / Log dock exactly where they were (the inline
  // form used to push both down), and must not move the page or the list.
  const layout = () => full.evaluate(() => {
    const box = id => { const rect = document.getElementById(id).getBoundingClientRect(); return [rect.left, rect.top, rect.width, rect.height].map(Math.round); };
    const dock = document.getElementById('tasks-dock');
    return {
      body: box('tasks-body'), panel: box('tasks-panel'), dock: box('tasks-dock'), side: box('side-dock'),
      lead: dock.style.getPropertyValue('--tasks-dock-lead'), page: [scrollX, scrollY],
      bodyScroll: document.getElementById('tasks-body').scrollTop, overflow: document.documentElement.scrollWidth - innerWidth,
    };
  });
  const settle = () => full.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(resolve, 100)))));
  await full.evaluate(() => { const body = document.getElementById('tasks-body'); body.scrollTop = Math.min(40, body.scrollHeight - body.clientHeight); });
  // The page may still settle a pixel after loading; the baseline is the first repeated reading.
  let resting = await layout();
  for (let i = 0; i < 20; i++) {
    await full.waitForTimeout(250); await settle();
    const next = await layout();
    const steady = JSON.stringify(next) === JSON.stringify(resting);
    resting = next;
    if (steady) break;
  }
  assert.ok(resting.dock[3] > 0 && resting.side[3] > 0, 'the dock is on screen beside the board');
  const insideViewport = async (page, label) => {
    const rect = await page.locator('#task-create-panel').evaluate(node => { const box = node.getBoundingClientRect(); return { left: box.left, top: box.top, right: box.right, bottom: box.bottom, overflowX: node.scrollWidth - node.clientWidth, width: innerWidth, height: innerHeight }; });
    assert.ok(rect.left >= 0 && rect.top >= 0 && rect.right <= rect.width && rect.bottom <= rect.height, `${label}: dialog stays inside the viewport ${JSON.stringify(rect)}`);
    assert.ok(rect.overflowX <= 0, `${label}: dialog has no horizontal overflow`);
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${label}: no page overflow`);
    for (const id of ['task-create-title', 'task-create-criteria', 'task-create-submit', 'task-create-cancel']) {
      await page.locator(`#${id}`).scrollIntoViewIfNeeded();
      const control = await page.locator(`#${id}`).evaluate(node => { const box = node.getBoundingClientRect(); return { top: box.top, bottom: box.bottom, left: box.left, right: box.right, height: innerHeight, width: innerWidth }; });
      assert.ok(control.top >= 0 && control.bottom <= control.height && control.left >= 0 && control.right <= control.width, `${label}: #${id} is reachable ${JSON.stringify(control)}`);
    }
  };
  await full.getByRole('button', { name: 'New task', exact: true }).click();
  await full.waitForFunction(() => document.querySelectorAll('#task-create-workspace option').length > 1);
  assert.ok(await full.getByLabel('Title', { exact: true }).isVisible());
  assert.equal(await full.evaluate(() => document.activeElement?.id), 'task-create-title', 'focus starts in Title');
  await settle();
  assert.deepEqual(await layout(), resting, 'opening the dialog moves neither the board nor the dock');
  await full.screenshot({ path: path.join(evidencePath, 'task-create-after-open-dark-1440.png') });
  // Dialog keystrokes are not task-pane or search shortcuts.
  await full.locator('#task-create-cancel').focus();
  await full.evaluate(() => { window.keySeen = []; document.addEventListener('keydown', event => window.keySeen.push([event.key, event.defaultPrevented])); });
  for (const key of ['/', 'j', 'k']) await full.keyboard.press(key);
  assert.deepEqual(await full.evaluate(() => window.keySeen), [['/', false], ['j', false], ['k', false]], 'dialog keystrokes trigger no list or pane shortcut');
  assert.equal(await full.evaluate(() => document.getElementById('task-create-panel').open), true);
  await full.getByLabel('Title', { exact: true }).fill('   ');
  await full.getByLabel('Assessed complexity').selectOption('low');
  await full.getByLabel('Acceptance criteria (one per line)').fill('Whitespace titles are refused');
  await full.getByRole('button', { name: 'Create proposed task' }).click();
  await full.waitForFunction(() => document.getElementById('task-create-feedback').textContent.includes('meaningful title'));
  await settle();
  assert.deepEqual(await layout(), resting, 'validation feedback moves neither the board nor the dock');
  await insideViewport(full, '1440');
  await full.getByLabel('Title', { exact: true }).fill('Layout stays put');
  await full.getByLabel('Assessed complexity').selectOption('low');
  await full.getByLabel('Acceptance criteria (one per line)').fill('The board and dock do not move');
  await full.getByRole('button', { name: 'Create proposed task' }).click();
  await full.waitForFunction(() => document.getElementById('task-create-feedback').textContent.includes('Task saved'));
  await settle();
  const saved = await layout();
  for (const key of ['body', 'panel', 'dock', 'side', 'lead']) assert.deepEqual(saved[key], resting[key], `success feedback leaves ${key} in place`);
  await full.screenshot({ path: path.join(evidencePath, 'task-create-after-success-dark-1440.png') });
  await full.getByRole('link', { name: /^Open / }).click();
  assert.ok(await full.locator('#task-create-panel').evaluate(node => !node.open), 'following the success link closes the dialog');
  await full.goto(`${origin}/#tasks`);
  await full.getByRole('button', { name: 'New task', exact: true }).click();
  await full.getByRole('button', { name: 'New proposal' }).click();
  await full.keyboard.press('Escape');
  await settle();
  const closed = await layout();
  for (const key of ['body', 'panel', 'dock', 'side', 'lead']) assert.deepEqual(closed[key], resting[key], `closing leaves ${key} in place`);
  assert.deepEqual(closed.page, resting.page, 'closing leaves the page scroll');
  // Short and narrow viewports keep every field and action reachable.
  for (const [width, height] of [[1280, 420], [375, 800], [375, 480]]) {
    await full.setViewportSize({ width, height });
    await full.getByRole('button', { name: 'New task', exact: true }).click();
    await settle();
    await insideViewport(full, `${width}x${height}`);
    await full.screenshot({ path: path.join(evidencePath, `task-create-after-open-dark-${width}x${height}.png`) });
    await full.keyboard.press('Escape');
  }
  await full.setViewportSize({ width: 1440, height: 1000 });
  await full.getByRole('button', { name: 'New task', exact: true }).click();
  await full.getByRole('button', { name: 'Cancel', exact: true }).click();
  await full.close();
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidencePath, 'proposal-result.json'), JSON.stringify({ requests, receipt: committed, checks: 'cancel, escape, focus containment, validation, lost response, reload, frozen retry, double submit, workspace/host switches, aggregate, correction, mobile, board/dock layout, short and narrow viewports', errors }, null, 2));
} finally { await browser.close(); }
