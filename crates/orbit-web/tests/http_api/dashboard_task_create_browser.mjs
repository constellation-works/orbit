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
  await full.getByRole('button', { name: 'New task', exact: true }).click();
  await full.waitForFunction(() => document.querySelectorAll('#task-create-workspace option').length > 1);
  assert.ok(await full.getByLabel('Title', { exact: true }).isVisible());
  await full.getByRole('button', { name: 'Cancel', exact: true }).click();
  await full.close();
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidencePath, 'proposal-result.json'), JSON.stringify({ requests, receipt: committed, checks: 'cancel, validation, lost response, reload, frozen retry, double submit, workspace/host switches, aggregate, correction, mobile', errors }, null, 2));
} finally { await browser.close(); }
