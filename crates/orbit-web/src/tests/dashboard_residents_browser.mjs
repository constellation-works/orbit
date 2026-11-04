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
globalThis.residentFixture = { writes: [], outcome: 'granted', refuse: false };
const mandate = {
  task_id: 'RESIDENT-1', state: 'active', effective_authority: { rights: ['dispatch'], workspaces: ['one'] },
  mandate: { goal: 'Keep the release green', crew: 'sol', cadence: { floor_minutes: 15, heartbeat_minutes: 60 }, exit_criteria: ['Release is green'] },
  authority: { revision: 1, granted: [], narrowed: [] }, open_requests: [],
};
globalThis.fetch = async (input, options = {}) => {
  const url = new URL(input, location.href);
  const api = url.pathname;
  const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
  if (options.method === 'POST') {
    const body = JSON.parse(options.body);
    residentFixture.writes.push({ path: api, body });
    if (residentFixture.refuse) return json({ error: 'Not authorized for this mandate' }, 403);
    if (api === '/api/residents') return json({ task_id: 'RESIDENT-NEW' });
    return json({ outcome: residentFixture.outcome, request: residentFixture.outcome.startsWith('unchanged') ? null : { id: 'DR-GRANT' }, narrowing_error: 'Deadline must be earlier' });
  }
  if (api === '/api/workspaces') return json([{ id: 'one', name: 'one', status: 'active', is_default: true }]);
  if (api === '/api/hosts') return json({ hosts: [{ name: 'owner', machine_id: 'owner-id', local: true }] });
  if (api === '/api/residents') return json({ mandates: [mandate], count: 1, crews: ['sol'], default_crew: 'sol', capabilities: { resident_write: { authorized: true } } });
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
    const create = page.getByRole('form', { name: 'New resident mandate', exact: true });
    await page.locator('.resident-create > summary').click();
    if (!feedbackOnly) {
      const tiers = await create.locator('select[name="complexity"] option').evaluateAll(nodes => nodes.map(node => node.value));
      await create.locator('[name="complexity"]').selectOption(tiers.at(-1));
      await page.screenshot({ path: path.join(evidence, 'resident-create-dark-1440.png') });
      assert.deepEqual(tiers, ['low', 'medium', 'hard', 'xhard'], 'Residents offers only canonical assessed API complexity tiers');
      for (const complexity of tiers) {
        await page.locator('.resident-create').evaluate(node => { node.open = true; });
        await create.locator('[name="goal"]').fill(`Release mandate ${complexity}`);
        await create.locator('[name="exit_criteria"]').fill('Release is green');
        await create.locator('[name="complexity"]').selectOption(complexity);
        await create.getByRole('button', { name: 'Create mandate', exact: true }).click();
        await page.waitForFunction(level => residentFixture.writes.some(write => write.path === '/api/residents' && write.body.complexity === level), complexity);
        await page.waitForFunction(() => document.getElementById('resident-operation-feedback').textContent.includes('Created mandate'));
        await page.waitForFunction(() => !document.querySelector('.resident-create button').disabled);
      }
      await page.locator('.resident-create').evaluate(node => { node.open = true; });
      await create.locator('[name="complexity"]').selectOption('xhard');
      await page.setViewportSize({ width: 375, height: 812 });
      await create.locator('[name="complexity"]').scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, 'resident-create-dark-375.png') });
      await page.setViewportSize({ width: 1440, height: 1000 });
    }
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
