// Run directly with the browser kit module and an evidence directory, or from
// dashboard_operations_browser.mjs. All state and API responses are disposable.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const { dashboardFile } = await import(process.env.ORBIT_DASHBOARD_FIXTURE_MODULE || './dashboard_static.mjs');
const fixture = `
globalThis.EventSource = class { close() {} };
globalThis.residentFixture = { writes: [], outcome: 'granted', refuse: false, refusePath: null };
const mandate = {
  task_id: 'RESIDENT-1', state: 'active', effective_authority: { rights: ['dispatch'], workspaces: ['one'] },
  mandate: { goal: 'Keep the release green', crew: 'sol', cadence: { floor_minutes: 15, heartbeat_minutes: 60 }, exit_criteria: ['Release is green'] },
  authority: { revision: 1, granted: [], narrowed: [] }, open_requests: [],
};
const otherState = (task_id, state) => ({ ...mandate, task_id, state });
globalThis.fetch = async (input, options = {}) => {
  const url = new URL(input, location.href);
  const api = url.pathname;
  const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
  if (options.method === 'POST') {
    const body = JSON.parse(options.body);
    residentFixture.writes.push({ path: api, body });
    if (residentFixture.refuse || (residentFixture.refusePath && api.endsWith(residentFixture.refusePath))) return json({ error: 'Not authorized for this mandate' }, 403);
    if (api === '/api/residents') return json({ task_id: 'RESIDENT-NEW' });
    return json({ outcome: residentFixture.outcome, request: residentFixture.outcome.startsWith('unchanged') ? null : { id: 'DR-GRANT' }, narrowing_error: 'Deadline must be earlier' });
  }
  if (api === '/api/workspaces') return json([{ id: 'one', name: 'one', status: 'active', is_default: true }]);
  if (api === '/api/hosts') return json({ hosts: [{ name: 'owner', machine_id: 'owner-id', local: true }] });
  if (api === '/api/residents') return json({
    mandates: [mandate, otherState('RESIDENT-2', 'handed_off'), otherState('RESIDENT-3', 'invalid'), otherState('RESIDENT-4', 'ended')], count: 4, crews: ['sol'], default_crew: 'sol',
    routine: { name: 'resident-cycle', source: 'one', target: 'job:resident_cycle_pipeline', machine_name: 'owner', enabled: false },
    capabilities: { resident_write: { authorized: true }, routine_toggle: { authorized: true } },
  });
  if (api === '/api/tasks') return json({ items: [], total: 0 });
  return json({ items: [], total: 0 });
};
await import('/app.js');
`;

export async function checkResidentForms(browser, evidence, { feedbackOnly = false } = {}) {
  fs.mkdirSync(evidence, { recursive: true });
  const server = http.createServer((req, res) => {
    const name = new URL(req.url, 'http://fixture').pathname;
    const served = dashboardFile(name);
    if (!served) { res.writeHead(404); res.end(); return; }
    res.setHeader('content-type', served.type);
    res.end(name === '/' ? served.data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, `<script type="module">${fixture}</script>`) : served.data);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=one#operations/residents`);
    const create = page.getByRole('form', { name: 'Start a resident', exact: true });
    const feedbackText = () => page.locator('#resident-operation-feedback').innerText();
    const openStart = async () => {
      await page.getByRole('button', { name: 'Start a resident', exact: true }).click();
      await create.waitFor();
    };
    const writesSince = (from) => page.evaluate(from => residentFixture.writes.slice(from).map(write => write.path), from);
    if (!feedbackOnly) {
      await openStart();
      const tiers = await create.locator('select[name="complexity"] option').evaluateAll(nodes => nodes.map(node => node.value));
      await create.locator('[name="complexity"]').selectOption(tiers.at(-1));
      await page.screenshot({ path: path.join(evidence, 'resident-create-dark-1440.png') });
      assert.deepEqual(tiers, ['low', 'medium', 'hard', 'xhard'], 'Residents offers only canonical assessed API complexity tiers');
      for (const complexity of tiers) {
        if (complexity !== tiers[0]) await openStart();
        await create.locator('[name="goal"]').fill(`Release mandate ${complexity}`);
        await create.locator('[name="exit_criteria"]').fill('Release is green');
        await create.locator('[name="complexity"]').selectOption(complexity);
        await create.getByRole('button', { name: 'Start resident', exact: true }).click();
        await page.waitForFunction(level => residentFixture.writes.some(write => write.path === '/api/residents' && write.body.complexity === level), complexity);
        await page.waitForFunction(() => document.getElementById('resident-operation-feedback').textContent.includes('Started RESIDENT-NEW'));
        await create.waitFor({ state: 'detached' });
      }
      // One flow: create, grant with the operator's words, then turn the
      // routine on, in that order.
      await openStart();
      const from = await page.evaluate(() => residentFixture.writes.length);
      await create.locator('[name="goal"]').fill('Release mandate with rights');
      await create.locator('[name="start-dispatch"]').check();
      await create.getByRole('button', { name: 'Start resident', exact: true }).click();
      assert.equal(await create.locator('.resident-start-status').innerText(), 'Add your words authorizing these rights.', 'Rights without words write nothing');
      assert.deepEqual(await writesSince(from), [], 'A refused draft creates nothing');
      await create.locator('[name="quote"]').fill('Dispatch in one');
      await create.getByRole('button', { name: 'Start resident', exact: true }).click();
      await page.waitForFunction(() => document.getElementById('resident-operation-feedback').textContent.includes('Started RESIDENT-NEW: it may dispatch; the cycle routine is on.'));
      assert.deepEqual(await writesSince(from), ['/api/residents', '/api/residents/RESIDENT-NEW/grant', '/api/routines/toggle'], 'Start flow sequences create, grant and routine');
      await create.waitFor({ state: 'detached' });
      const grantBody = await page.evaluate(() => residentFixture.writes.find(write => write.path.endsWith('/RESIDENT-NEW/grant')).body);
      assert.deepEqual(grantBody, { add: ['dispatch'], workspaces: ['one'], quote: 'Dispatch in one' });
      // A refused middle step names the step and what exists.
      await page.evaluate(() => { residentFixture.refusePath = '/grant'; });
      await openStart();
      await create.locator('[name="goal"]').fill('Release mandate refused grant');
      await create.locator('[name="start-merge"]').check();
      await create.locator('[name="quote"]').fill('Merge in one');
      await create.getByRole('button', { name: 'Start resident', exact: true }).click();
      await page.waitForFunction(() => document.getElementById('resident-operation-feedback').textContent.includes('Step 2 of 3'));
      await create.waitFor({ state: 'detached' });
      assert.match(await feedbackText(), /Step 2 of 3, grant its rights, was refused: Not authorized for this mandate\. RESIDENT-NEW exists with no rights/);
      await page.evaluate(() => { residentFixture.refusePath = null; });
      await openStart();
      await create.locator('[name="complexity"]').selectOption('xhard');
      await page.setViewportSize({ width: 375, height: 812 });
      await create.locator('[name="complexity"]').scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, 'resident-create-dark-375.png') });
      await page.setViewportSize({ width: 1440, height: 1000 });
      // A background refresh spares the form being typed in.
      await create.locator('[name="goal"]').fill('Draft kept across a refresh');
      await create.locator('[name="goal"]').focus();
      await page.evaluate(async () => { await (await import('/js/operations-residents.js')).fetchAndRenderResidents(); });
      assert.equal(await create.locator('[name="goal"]').inputValue(), 'Draft kept across a refresh', 'A background refresh leaves the focused start form untouched');
      assert.equal(await create.locator('[name="goal"]').evaluate(node => node === document.activeElement), true, 'Focus stays in the start form after a background refresh');
      // Cancel closes the form at once, though focus is inside it.
      await create.getByRole('button', { name: 'Cancel starting a resident', exact: true }).click();
      await create.waitFor({ state: 'detached', timeout: 1500 });
      // Enter in the goal field submits like the button: the form is gone.
      await openStart();
      const enterFrom = await page.evaluate(() => residentFixture.writes.length);
      await create.locator('[name="goal"]').fill('Release mandate via Enter');
      await create.locator('[name="goal"]').press('Enter');
      await page.waitForFunction(from => residentFixture.writes.length > from, enterFrom);
      await create.waitFor({ state: 'detached', timeout: 1500 });
      assert.equal(await page.locator('.resident-row[data-task="RESIDENT-1"]').count(), 1, 'The list is rendered after an Enter submit');
    }
    // Detail on demand: no detail form until the mandate is selected.
    assert.equal(await page.locator('.resident-detail').count(), 0, 'No mandate detail before selection');
    await page.locator('.resident-row[data-task="RESIDENT-1"]').click();
    if (!feedbackOnly) {
      // Reason only after choosing an action that needs one.
      assert.equal(await page.locator('.resident-composer').count(), 0, 'No reason input before an action is chosen');
      const from = await page.evaluate(() => residentFixture.writes.length);
      await page.getByRole('button', { name: 'Choose Stop RESIDENT-1', exact: true }).click();
      const stop = page.getByRole('form', { name: 'Stop RESIDENT-1', exact: true });
      assert.equal(await stop.getByRole('button', { name: 'Stop RESIDENT-1', exact: true }).isDisabled(), true, 'Stop waits for a reason');
      await stop.locator('[name="reason"]').fill('Goal met');
      await stop.getByRole('button', { name: 'Stop RESIDENT-1', exact: true }).click();
      await page.waitForFunction(from => residentFixture.writes.length > from, from);
      assert.deepEqual(await page.evaluate(from => residentFixture.writes[from], from), { path: '/api/residents/RESIDENT-1/control', body: { action: 'stop', reason: 'Goal met' } });
      await page.waitForFunction(() => !document.querySelector('.resident-composer'));
      // Cancel closes the composer at once, though focus is inside it.
      await page.getByRole('button', { name: 'Choose Pause RESIDENT-1', exact: true }).click();
      const pause = page.getByRole('form', { name: 'Pause RESIDENT-1', exact: true });
      await pause.locator('[name="reason"]').fill('Draft reason');
      await page.evaluate(async () => { await (await import('/js/operations-residents.js')).fetchAndRenderResidents(); });
      assert.equal(await pause.locator('[name="reason"]').inputValue(), 'Draft reason', 'A background refresh leaves the focused composer untouched');
      await pause.getByRole('button', { name: 'Cancel pause', exact: true }).click();
      await pause.waitFor({ state: 'detached', timeout: 1500 });
      // Enter in the reason field submits like the confirm button and closes
      // the composer; typing afterwards raises no page error.
      await page.getByRole('button', { name: 'Choose Pause RESIDENT-1', exact: true }).click();
      const enterWrites = await page.evaluate(() => residentFixture.writes.length);
      await pause.locator('[name="reason"]').fill('Pause via Enter');
      await pause.locator('[name="reason"]').press('Enter');
      await page.waitForFunction(from => residentFixture.writes.length > from, enterWrites);
      await pause.waitFor({ state: 'detached', timeout: 1500 });
      await page.keyboard.type('more');
      assert.deepEqual(errors, [], 'No page error after an Enter-submitted composer');
    }
    // Relay is offered to handed-off and invalid mandates, which the server
    // still accepts (a handed-off veto is a relay). An ended mandate offers none.
    for (const [id, label] of [['RESIDENT-2', 'handed-off'], ['RESIDENT-3', 'invalid']]) {
      await page.locator(`.resident-row[data-task="${id}"]`).click();
      const detail = page.locator(`#resident-detail-${id}`);
      for (const width of [1440, 375]) {
        await page.setViewportSize({ width, height: width === 375 ? 812 : 1000 });
        await detail.screenshot({ path: path.join(evidence, `resident-${label}-detail-dark-${width}.png`) });
      }
      await page.setViewportSize({ width: 1440, height: 1000 });
      assert.equal(await page.getByRole('button', { name: `Choose Relay ${id}`, exact: true }).count(), 1, `${label} ${id} offers Relay`);
      const from = await page.evaluate(() => residentFixture.writes.length);
      await page.getByRole('button', { name: `Choose Relay ${id}`, exact: true }).click();
      const relay = page.getByRole('form', { name: `Relay ${id}`, exact: true });
      await relay.locator('[name="relay"]').fill('Hold the release');
      await relay.getByRole('button', { name: `Relay ${id}`, exact: true }).click();
      await page.waitForFunction(from => residentFixture.writes.length > from, from);
      assert.deepEqual(await page.evaluate(from => residentFixture.writes[from], from), { path: `/api/residents/${id}/relay`, body: { quote: 'Hold the release' } }, `${label} ${id} relays to its relay route`);
      await page.waitForFunction(() => !document.querySelector('.resident-composer'));
    }
    await page.locator('.resident-ended').evaluate(node => { node.open = true; });
    await page.locator('.resident-row[data-task="RESIDENT-4"]').click();
    assert.equal(await page.locator('#resident-detail-RESIDENT-4').count(), 1, 'The ended mandate detail renders');
    assert.equal(await page.locator('#resident-detail-RESIDENT-4').getByRole('button', { name: /Relay/ }).count(), 0, 'An ended mandate offers no Relay');
    await page.locator('.resident-row[data-task="RESIDENT-1"]').click();
    const authority = page.getByRole('form', { name: 'Authority for RESIDENT-1', exact: true });
    await page.locator('.resident-authority > summary').click();
    for (const outcome of ['unchanged', 'granted', 'granted_and_narrowed', 'unchanged_and_narrowed', 'granted_narrowing_refused', 'unchanged_narrowing_refused']) {
      await page.locator('.resident-authority').evaluate(node => { node.open = true; });
      await authority.locator('[name="add-dispatch"]').check();
      await authority.locator('[name="quote"]').fill('Dispatch in the selected workspace');
      await authority.locator('[name="deadline"]').fill(outcome.includes('narrow') ? '2099-01-01T12:00' : '');
      await page.evaluate(outcome => { residentFixture.outcome = outcome; }, outcome);
      await authority.getByRole('button', { name: 'Widen RESIDENT-1', exact: true }).click();
      const feedback = page.locator('#resident-operation-feedback');
      await page.waitForFunction(() => !document.querySelector('.resident-authority button').disabled);
      const text = await feedback.innerText();
      if (outcome === 'unchanged') {
        for (const width of [1440, 375]) {
          await page.setViewportSize({ width, height: width === 375 ? 812 : 1000 });
          await feedback.scrollIntoViewIfNeeded();
          await page.screenshot({ path: path.join(evidence, `resident-unchanged-dark-${width}.png`) });
        }
        await page.setViewportSize({ width: 1440, height: 1000 });
      }
      if (outcome.startsWith('unchanged')) {
        assert.match(text, /already held.*no grant request was recorded/, `${outcome}: no authority or answered request was recorded`);
        assert.doesNotMatch(text, /recorded as your answered request/);
      } else {
        assert.match(text, /Widened RESIDENT-1/, `${outcome}: actual widening is reported`);
      }
      if (outcome.endsWith('refused')) assert.match(text, /Narrowing refused: Deadline must be earlier/);
      if (outcome === 'granted_and_narrowed') assert.match(text, /two changes/);
      if (outcome === 'unchanged_and_narrowed') assert.match(text, /Shortened its deadline/);

    }
    await page.evaluate(() => { residentFixture.refuse = true; });
    await page.locator('.resident-authority').evaluate(node => { node.open = true; });
    await authority.getByRole('button', { name: 'Widen RESIDENT-1', exact: true }).click();
    await page.waitForFunction(() => document.getElementById('resident-operation-feedback').textContent.includes('Refused:'));
    assert.equal(await authority.getByRole('button', { name: 'Widen RESIDENT-1', exact: true }).isEnabled(), true, 'Refused grant re-enables its control');
    assert.deepEqual(errors, []);
    fs.writeFileSync(path.join(evidence, 'resident-forms-result.json'), JSON.stringify({ passed: true, feedbackOnly, writes: await page.evaluate(() => residentFixture.writes) }, null, 2));
  } finally {
    await page.close();
    await new Promise(resolve => server.close(resolve));
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
  const browser = await chromium.launch({ headless: true });
  try { await checkResidentForms(browser, path.resolve(process.argv[3]), { feedbackOnly: process.argv.includes('--feedback') }); }
  finally { await browser.close(); }
}
