// Chromium authoring regression fixture, invoked through dashboard_experiments_browser.mjs.
import assert from 'node:assert/strict';
import fs from 'node:fs';

// Usage: node dashboard_experiments_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/experiments-browser --authoring
// The fixture server stands in for the experiment routes and answers with
// core's own refusal text for the two refusals the flow provokes; the routes
// themselves are covered against the real server in experiments.rs.
export async function checkExperimentAuthoringBrowser(readout) {
  const { default: http } = await import('node:http');
  const { default: path } = await import('node:path');
  const { pathToFileURL } = await import('node:url');
  const { dashboardFile, dashboardCsp } = await import('../../src/tests/dashboard_static.mjs');
  const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
  const evidence = path.resolve(process.argv[3]);
  fs.mkdirSync(evidence, { recursive: true });
  const authoring = {
    create: { authorized: true, reason: null },
    update: { authorized: true, reason: null },
    start: { authorized: true, reason: null },
    enabled: true,
    max_running: 3,
    config_keys: ['workflow.explore.max_calls_per_run'],
    metrics: ['cost_usd', 'first_pass_acceptance', 'lead_time'],
    complexities: ['low', 'medium', 'hard', 'xhard'],
    crews: [{ name: 'opus', enabled: true }, { name: 'sonnet', enabled: true }],
  };
  const initializer = `
    import * as experiments from '/static/js/experiments.js';
    document.querySelector('.tab-pane[data-tab=analytics]').classList.add('active');
    document.querySelectorAll('.tab-pane[data-tab=analytics] > main').forEach(node => {
      node.style.display = node.id === 'analytics-experiments-main' ? 'grid' : 'none';
    });
    import * as common from '/static/js/common.js';
    import * as experimentForm from '/static/js/experiment-form.js';
    window.experiments = experiments;
    window.common = common;
    window.experimentForm = experimentForm;
    experiments.setExperimentId(null);
    await experiments.fetchAndRenderExperiments();
    window.fixtureReady = true;
  `;
  let store = [];
  let currentAuthoring = authoring;
  let holdNextList = null;
  const heldLists = new Set();
  const holdList = () => {
    let seen;
    const held = { seen: new Promise(resolve => { seen = resolve; }), markSeen: () => seen() };
    holdNextList = held;
    heldLists.add(held);
    return held;
  };
  const requests = [];
  let heldStartExperimentId = null;
  let heldStartResponse = null;
  let heldStartSeenResolve = null;
  const view = experiment => ({
    ...experiment,
    enrollment: experiment.arms.map(arm => ({ arm: arm.name, enrolled: 0, segment_enrolled: 0 })),
    forced: 0,
    excluded: 0,
  });
  const apply = (experiment, body) => {
    for (const name of ['name', 'hypothesis', 'population', 'primary_metric', 'guardrails', 'min_sample', 'max_days', 'mode']) {
      if (body[name] !== undefined) experiment[name] = body[name];
    }
    if (body.arms) experiment.arms = body.arms.map(arm => ({ ...arm, overlay_digest: 'sha256:fixture', retired_segment: null }));
  };
  const refusal = (res, status, error) => {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error }));
  };
  const readBody = req => new Promise(resolve => {
    let text = '';
    req.on('data', chunk => { text += chunk; });
    req.on('end', () => resolve(text ? JSON.parse(text) : {}));
  });
  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, 'http://fixture');
    const route = url.pathname.replace(/^\/api\/on\/[^/]+\//, '/api/');
    if (route.startsWith('/api/diagnostics/experiments')) {
      const body = req.method === 'GET' ? null : await readBody(req);
      requests.push({ method: req.method, path: url.pathname, workspace: url.searchParams.get('workspace'), body });
      const [, id, action] = route.replace('/api/diagnostics/experiments', '').split('/');
      const found = store.find(item => item.id === id);
      const json = value => { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); };
      if (!id && req.method === 'GET') {
        const payload = structuredClone({ experiments: store.map(view), authoring: currentAuthoring });
        if (holdNextList) {
          const held = holdNextList;
          holdNextList = null;
          held.release = (status = 200) => {
            heldLists.delete(held);
            if (status === 200) json(payload);
            else refusal(res, status, 'held destination read failed');
          };
          held.markSeen();
          return;
        }
        return json(payload);
      }
      for (const [metric, threshold] of Object.entries(body?.guardrails || {})) {
        if (!/^[+-]\d+(\.\d+)?(pt|%)?$/.test(threshold)) {
          return refusal(res, 400, `guardrail threshold \`${threshold}\` for \`${metric}\` must be a signed number with an optional \`pt\` or \`%\` unit, such as \`-5pt\` or \`+0\``);
        }
      }
      if (!id && req.method === 'POST') {
        const experiment = { id: `exp-${store.length + 1}`, status: 'draft', segment: 0, mode: 'shadow', hypothesis: '', population: {}, guardrails: {},
          primary_metric: null, min_sample: null, max_days: null, arms: [], events: [], created_at: new Date().toISOString() };
        apply(experiment, body);
        store.push(experiment);
        return json(experiment);
      }
      if (!found) return refusal(res, 400, `no experiment \`${id}\` in this workspace`);
      if (action === 'start' && req.method === 'POST' && found.id === heldStartExperimentId) {
        heldStartResponse = () => {
          heldStartResponse = null;
          refusal(res, 409, 'deterministically refused start');
        };
        heldStartSeenResolve?.();
        return;
      }
      if (!action && req.method === 'PATCH') {
        if (found.status !== 'draft') {
          return refusal(res, 400, `experiment \`${found.name}\` is ${found.status}: its arms, overlays, population, metrics and sample rules are editable only in draft. Reweight or retire arms with reweight, or clone it into a new draft`);
        }
        apply(found, body);
        return json(found);
      }
      if (action === 'start' && req.method === 'POST') {
        if (!found.primary_metric) return refusal(res, 400, `experiment \`${found.name}\` cannot start: it has no primary metric`);
        found.status = 'running';
        found.segment = 1;
        return json(found);
      }
      if (action === 'readout') {
        const fixture = structuredClone(readout);
        fixture.arms = found.arms.map(arm => ({ name: arm.name, control: arm.control, weight: arm.weight, n: 0 }));
        fixture.cumulative = { metric: 'pass_rate', window: fixture.cumulative.window, arms: [] };
        fixture.metrics = [];
        return json(fixture);
      }
      if (!action) return json({ ...found, conclude_authorized: true, authoring: currentAuthoring });
      return refusal(res, 404, 'unexpected route');
    }
    const served = url.pathname === '/fixture.mjs'
      ? { data: initializer, type: 'text/javascript' } : dashboardFile(url.pathname);
    if (!served) { res.writeHead(404); res.end(); return; }
    const data = url.pathname === '/' ? served.data.toString().replace(
      /<script[^>]*src="[^"]*app.js"[^>]*><\/script>/,
      '<script type="module" src="/fixture.mjs"></script>',
    ) : served.data;
    res.writeHead(200, { 'content-type': served.type, 'content-security-policy': dashboardCsp() });
    res.end(data);
  });
  const errors = [];
  const steps = [];
  let browser;
  try {
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
    page.on('pageerror', error => errors.push(String(error)));
    for (const width of [1440, 375]) {
      store = [];
      requests.length = 0;
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&case=authoring-${width}#analytics/experiments`);
      await page.waitForFunction(() => window.fixtureReady);
      const form = page.locator('#experiments-form form.exf');
      const labelled = (scope, label) => scope.locator('label.exf-field', { hasText: new RegExp(`^${label}`) });
      const shot = name => page.screenshot({ path: path.join(evidence, `${name}-dark-${width}.png`) });

      for (const scope of ['workspace', 'host']) {
        const selectScope = async name => page.evaluate(({ scope, name }) => {
          if (scope === 'workspace') window.common.setWorkspace(`ws_${name}`);
          else window.common.setHost(name);
        }, { scope, name });
        const refresh = () => page.evaluate(() => window.experiments.fetchAndRenderExperiments());
        const choices = name => ({ ...authoring, crews: [{ name, enabled: true }], config_keys: [`${name}.key`] });
        const crew = form.locator('select[aria-label="Crew"]').first();
        const config = form.locator('select[aria-label="Config key"]').first();
        const addChoices = async () => {
          const arm = form.locator('.exf-arm').first();
          await arm.locator('.exf-surface-crew summary').click();
          await arm.locator('.exf-add-crew').click();
          await arm.locator('.exf-surface-config summary').click();
          await arm.locator('.exf-surface-config button', { hasText: 'Add config value' }).click();
        };
        const assertChoices = async name => {
          assert.deepEqual(await crew.locator('option').evaluateAll(nodes => nodes.map(node => node.value).filter(Boolean)), [name]);
          assert.deepEqual(await config.locator('option').evaluateAll(nodes => nodes.map(node => node.value).filter(Boolean)), [`${name}.key`]);
        };
        await selectScope(`${scope}-a`);
        currentAuthoring = choices(`${scope}-a`);
        await refresh();
        await page.locator('#experiments-new').click();
        await labelled(form, 'Name').locator('input').fill('Retired draft');
        await selectScope(`${scope}-b`);
        currentAuthoring = choices(`${scope}-b`);
        const held = holdList();
        await page.evaluate(() => {
          window.heldRead = window.experiments.fetchAndRenderExperiments().catch(error => { window.heldError = error.message; });
        });
        await held.seen;
        assert.equal(await page.locator('#experiments-new').isHidden(), true, `${scope}: New experiment is hidden during held metadata`);
        await page.locator('#analytics-experiments-main').evaluate(node => node.scrollIntoView({ block: 'start' }));
        await shot(`scope-${scope}-held`);
        await page.evaluate(() => {
          document.querySelector('#experiments-new').click();
          window.experimentForm.openExperimentForm({ id: 'old-draft', status: 'draft', arms: [] });
        });
        assert.equal(await page.locator('#experiments-form').isHidden(), true, `${scope}: opening requires current metadata`);
        held.release(503);
        await page.evaluate(() => window.heldRead);
        assert.match(await page.evaluate(() => window.heldError), /held destination read failed/);
        assert.equal(await page.locator('#experiments-new').isHidden(), true, `${scope}: failed reads keep authoring retired`);
        await refresh();
        const destination = requests.at(-1);
        assert.match(destination.path, scope === 'host' ? /\/api\/on\/host-b\// : /^\/api\/diagnostics/);
        assert.equal(destination.workspace, 'ws_workspace-b');
        await page.locator('#experiments-new').click();
        await labelled(form, 'Name').locator('input').fill('Keep my destination draft');
        await addChoices();
        await assertChoices(`${scope}-b`);
        await crew.selectOption(`${scope}-b`);
        await config.selectOption(`${scope}-b.key`);
        await form.locator('.exf-arm').first().scrollIntoViewIfNeeded();
        await shot(`scope-${scope}-choices`);
        await crew.selectOption('');
        await config.selectOption('');
        const liveInput = await labelled(form, 'Name').locator('input').elementHandle();
        await refresh();
        assert.equal(await liveInput.evaluate(node => node.isConnected), true, 'unchanged polls preserve form nodes');
        currentAuthoring = choices(`${scope}-c`);
        currentAuthoring.start = { authorized: false };
        await refresh();
        await assertChoices(`${scope}-c`);
        assert.equal(await labelled(form, 'Name').locator('input').inputValue(), 'Keep my destination draft');
        assert.equal(await form.locator('.exf-save-start').count(), 0, 'refreshed capabilities remove Start');
        currentAuthoring = { ...currentAuthoring, create: { authorized: false } };
        await refresh();
        assert.equal(await form.locator('.exf-save').isDisabled(), true, 'revoked create disables Save');
        await form.locator('.exf-cancel').click();
        const stale = holdList();
        await page.evaluate(() => { window.staleRead = window.experiments.fetchAndRenderExperiments(); });
        await stale.seen;
        await selectScope(`${scope}-a`);
        await selectScope(`${scope}-b`);
        currentAuthoring = choices(`${scope}-b`);
        await refresh();
        stale.release();
        await page.evaluate(() => window.staleRead);
        await page.locator('#experiments-new').click();
        await addChoices();
        await assertChoices(`${scope}-b`);
        await form.locator('.exf-cancel').click();
        steps.push({ width, scope, heldReadHidden: true, failedReadHidden: true,
          destinationChoices: true, refreshedChoicesAndPermissions: true, inputPreserved: true, staleVisitIgnored: true });
      }
      await page.evaluate(() => { window.common.setHost(null); window.common.setWorkspace('ws_fixture'); });
      currentAuthoring = authoring;
      await page.evaluate(() => window.experiments.fetchAndRenderExperiments());

      // Hold Start while a changed list poll replaces its card, then refuse it.
      // A final poll with the same payload must keep the refusal and unlock the
      // current card; this orders the requests without timing delays.
      const lostStart = {
        id: 'exp-lost-start-refusal', name: 'race draft', status: 'draft', segment: 0,
        mode: 'shadow', hypothesis: 'Original hypothesis', primary_metric: 'cost_usd',
        min_sample: 20, max_days: 14, population: {}, guardrails: {}, events: [],
        created_at: '2026-10-28T00:00:00Z',
        arms: [
          { name: 'control', control: true, weight: 1, retired_segment: null },
          { name: 'treatment', control: false, weight: 1, retired_segment: null },
        ],
      };
      store.push(lostStart);
      await page.evaluate(async () => window.experiments.fetchAndRenderExperiments());
      const lostStartCard = page.locator('.ex-item', { hasText: lostStart.name });
      await lostStartCard.locator('.exf-start').click();
      await lostStartCard.locator('.exf-confirm textarea').fill('Preserve this reason');
      heldStartExperimentId = lostStart.id;
      const heldStartSeen = new Promise(resolve => { heldStartSeenResolve = resolve; });
      const routeBeforeStart = page.url();
      await lostStartCard.locator('.exf-confirm-start').click();
      await heldStartSeen;
      heldStartSeenResolve = null;
      assert.ok(heldStartResponse, 'the Start response is held');

      lostStart.hypothesis = 'Changed while Start was pending';
      await page.evaluate(async () => window.experiments.fetchAndRenderExperiments());
      assert.equal(await lostStartCard.locator('.ex-hypothesis').textContent(), lostStart.hypothesis);
      assert.equal(await lostStartCard.locator('.exf-confirm textarea').inputValue(), 'Preserve this reason');
      assert.equal(await lostStartCard.locator('.exf-confirm-start').textContent(), 'Starting…');
      assert.equal(await lostStartCard.locator('.exf-confirm-start').isDisabled(), true);
      assert.equal(await lostStartCard.locator('.exf-confirm button.action.cancel').isDisabled(), true);
      assert.equal(await lostStartCard.locator('.exf-edit').isDisabled(), true);

      const refuseHeldStart = heldStartResponse;
      refuseHeldStart();
      heldStartExperimentId = null;
      await lostStartCard.locator('.exf-error').waitFor();
      assert.equal(await lostStartCard.locator('.exf-error').textContent(), 'deterministically refused start');
      assert.equal(await lostStartCard.locator('.exf-confirm textarea').inputValue(), 'Preserve this reason');
      assert.equal(await lostStartCard.locator('.exf-confirm-start').isDisabled(), false, 'retry is enabled after refusal');
      assert.equal(await lostStartCard.locator('.exf-confirm button.action.cancel').isDisabled(), false, 'Cancel is enabled after refusal');
      assert.equal(await lostStartCard.locator('.exf-edit').isDisabled(), false, 'Edit is enabled after refusal');

      const unchangedListPayload = JSON.stringify(store.map(view));
      await lostStartCard.scrollIntoViewIfNeeded();
      await page.evaluate(async () => window.experiments.fetchAndRenderExperiments());
      assert.equal(JSON.stringify(store.map(view)), unchangedListPayload, 'the final poll returns the same list payload');
      assert.equal(page.url(), routeBeforeStart, 'the refusal is shown without navigation');
      assert.equal(await lostStartCard.locator('.exf-error').textContent(), 'deterministically refused start');
      assert.equal(await lostStartCard.locator('.exf-confirm-start').isDisabled(), false);
      assert.equal(await lostStartCard.locator('.exf-confirm button.action.cancel').isDisabled(), false);
      assert.equal(await lostStartCard.locator('.exf-edit').isDisabled(), false);
      // The list's changed-row highlight lasts 600 ms; let it finish before
      // capturing the stable repaired state.
      await page.waitForTimeout(650);
      await shot('lost-start-refusal');
      await lostStartCard.locator('.exf-confirm button.action.cancel').click();
      store = [];
      await page.evaluate(async () => window.experiments.fetchAndRenderExperiments());
      requests.length = 0;

      // Clearing optional draft fields is refused in place: the form stays
      // open, each cleared field names the refusal, no PATCH is sent and
      // reopening shows the stored values. Save and start refuses the same
      // way without entering its confirmation.
      const clearDraft = {
        id: `exp-clear-${width}`, name: 'clear-fields', status: 'draft', segment: 0,
        mode: 'shadow', hypothesis: 'Clearing keeps the stored values.',
        primary_metric: 'cost_usd', min_sample: 20, max_days: 14,
        population: {}, guardrails: {}, events: [],
        created_at: '2026-10-28T00:00:00Z',
        arms: [
          { name: 'control', control: true, weight: 1, retired_segment: null },
          { name: 'treatment', control: false, weight: 1, retired_segment: null },
        ],
      };
      store.push(clearDraft);
      await page.evaluate(async () => window.experiments.fetchAndRenderExperiments());
      const clearCard = page.locator('.ex-item', { hasText: 'clear-fields' });
      await clearCard.locator('.exf-edit').click();
      assert.equal(await labelled(form, 'Primary metric').locator('select').inputValue(), 'cost_usd', 'edit opens on the stored primary metric');
      assert.equal(await labelled(form, 'Minimum sample per arm').locator('input').inputValue(), '20', 'edit opens on the stored sample');
      assert.equal(await labelled(form, 'Maximum days').locator('input').inputValue(), '14', 'edit opens on the stored days');
      await labelled(form, 'Primary metric').locator('select').selectOption('');
      await labelled(form, 'Minimum sample per arm').locator('input').fill('');
      await labelled(form, 'Maximum days').locator('input').fill('');
      const clearPatchBefore = requests.filter(request => request.method === 'PATCH').length;
      await form.locator('.exf-save').click();
      for (const [label, field] of [['Primary metric', 'primary_metric'], ['Minimum sample per arm', 'min_sample'], ['Maximum days', 'max_days']]) {
        const refusal = labelled(form, label).locator('.exf-error');
        await refusal.waitFor();
        assert.match(await refusal.textContent(), /not supported/, `${field} names its refusal`);
      }
      assert.equal(await form.isVisible(), true, 'a refused clear keeps the form open');
      assert.equal(await page.locator('#experiments-form').isHidden(), false, 'a refused clear keeps the form host open');
      assert.equal(requests.filter(request => request.method === 'PATCH').length, clearPatchBefore, 'a refused clear sends no PATCH');
      assert.equal(clearDraft.primary_metric, 'cost_usd', 'a refused clear keeps the stored metric');
      assert.equal(clearDraft.min_sample, 20, 'a refused clear keeps the stored sample');
      assert.equal(clearDraft.max_days, 14, 'a refused clear keeps the stored days');
      await labelled(form, 'Primary metric').scrollIntoViewIfNeeded();
      await shot('clear-refusal');
      const clearRequestsBeforeStart = requests.length;
      await form.locator('.exf-save-start').click();
      assert.equal(await form.locator('.exf-confirm').count(), 0, 'a refused clear shows no Save and start confirmation');
      assert.equal(await form.isVisible(), true, 'a refused Save and start keeps the form open');
      assert.equal(requests.length, clearRequestsBeforeStart, 'a refused Save and start sends no write');
      await form.locator('.exf-cancel').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      await clearCard.locator('.exf-edit').click();
      assert.equal(await labelled(form, 'Primary metric').locator('select').inputValue(), 'cost_usd', 'reopening shows the kept metric');
      assert.equal(await labelled(form, 'Minimum sample per arm').locator('input').inputValue(), '20', 'reopening shows the kept sample');
      assert.equal(await labelled(form, 'Maximum days').locator('input').inputValue(), '14', 'reopening shows the kept days');
      await labelled(form, 'Primary metric').locator('select').selectOption('first_pass_acceptance');
      await labelled(form, 'Minimum sample per arm').locator('input').fill('30');
      await labelled(form, 'Maximum days').locator('input').fill('21');
      await form.locator('.exf-save').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const clearPatch = requests.find(request => request.method === 'PATCH' && request.body.primary_metric === 'first_pass_acceptance');
      assert.ok(clearPatch, 'a positive edit after the refusal still patches');
      assert.equal(clearPatch.body.min_sample, 30);
      assert.equal(clearPatch.body.max_days, 21);
      store = [];
      await page.evaluate(async () => window.experiments.fetchAndRenderExperiments());
      requests.length = 0;

      // Create: the form covers the definition; crew pickers offer only the
      // workspace's configured crews.
      await page.locator('#experiments-new').click();
      assert.equal(await form.isVisible(), true, 'New experiment opens the form');
      assert.equal(await page.locator('#experiments-new').getAttribute('aria-expanded'), 'true');
      await labelled(form, 'Name').locator('input').fill('trial');
      await labelled(form, 'Hypothesis').locator('textarea').fill('A smaller crew pool keeps first-pass acceptance.');
      await form.locator('.exf-population label.exf-check', { hasText: 'medium' }).locator('input').check();
      await labelled(form, 'Tags to include').locator('input').fill('ui, docs');
      const treatment = form.locator('.exf-arm').nth(1);
      await treatment.locator('.exf-surface-crew > summary').click();
      await treatment.locator('.exf-add-crew').click();
      const crewOptions = await treatment.locator('.exf-surface-crew select').evaluate(node => [...node.options].map(option => option.value));
      assert.deepEqual(crewOptions, ['', 'opus', 'sonnet'], 'the crew picker lists the configured crews');
      await treatment.locator('.exf-surface-crew select').selectOption('sonnet');
      await treatment.locator('.exf-surface-skills > summary').click();
      assert.match(await treatment.locator('.exf-surface-skills > summary').textContent(), /Claude on Unix only/);
      await form.locator('.exf-add-guardrail').click();
      await form.locator('.exf-guardrails select').selectOption('cost_usd');
      await form.locator('.exf-guardrails input').fill('5pt');
      await page.evaluate(() => document.getElementById('experiments-form').scrollIntoView({ block: 'start' }));
      await shot('experiment-form');
      await treatment.evaluate(node => node.scrollIntoView({ block: 'start' }));
      await shot('experiment-form-arms');

      // Core's refusal lands under the field it names.
      await form.locator('.exf-save').click();
      const guardrailError = form.locator('.exf-guardrails .exf-error');
      await guardrailError.waitFor();
      assert.match(await guardrailError.textContent(), /guardrail threshold `5pt` for `cost_usd`/);
      await guardrailError.scrollIntoViewIfNeeded();
      await shot('field-error');
      await form.locator('.exf-guardrails input').fill('-5pt');
      await form.locator('.exf-save').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const created = requests.find(request => request.method === 'POST' && request.path === '/api/diagnostics/experiments' && request.body.guardrails.cost_usd === '-5pt');
      assert.ok(created, 'the draft is created');
      assert.deepEqual(created.body.population, { complexity: ['medium'], tags_include: ['ui', 'docs'] });
      assert.deepEqual(created.body.arms.map(arm => [arm.name, arm.control, arm.weight]), [['control', true, 1], ['treatment', false, 1]]);
      assert.deepEqual(created.body.arms[1].overlay, { crew: { pool: [{ crew: 'sonnet', weight: 1 }] } });
      assert.equal(created.body.mode, 'shadow', 'mode defaults to shadow');

      // A draft card offers Edit and Start; a refused start says why.
      const card = page.locator('.ex-item', { hasText: 'trial' });
      await card.locator('.exf-start').click();
      await card.locator('.exf-confirm-start').click();
      await card.locator('.exf-error').waitFor();
      assert.match(await card.locator('.exf-error').textContent(), /it has no primary metric/);
      await card.locator('.exf-confirm .action.cancel').click();

      // Edit: the form opens on the stored definition and patches it.
      await card.locator('.exf-edit').click();
      assert.equal(await labelled(form, 'Name').locator('input').inputValue(), 'trial');
      assert.equal(await form.locator('.exf-arm').nth(1).locator('.exf-surface-crew select').inputValue(), 'sonnet');
      await labelled(form, 'Primary metric').locator('select').selectOption('first_pass_acceptance');
      await labelled(form, 'Minimum sample per arm').locator('input').fill('20');
      await labelled(form, 'Reason').locator('textarea').fill('Measure acceptance');
      await form.locator('.exf-save').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const patch = requests.find(request => request.method === 'PATCH');
      assert.equal(patch.body.primary_metric, 'first_pass_acceptance');
      assert.equal(patch.body.min_sample, 20);
      assert.equal(patch.body.reason, 'Measure acceptance');
      assert.equal(patch.body.name, undefined, 'an unchanged name is not sent as a rename');

      // Start: the confirmation names arms, weights and population.
      await card.locator('.exf-start').click();
      const confirmation = await card.locator('.exf-confirm').textContent();
      for (const fragment of ['control (control) weight 1', 'treatment weight 1', 'complexity medium', 'tags include ui, docs', 'Shadow']) {
        assert.ok(confirmation.includes(fragment), `confirmation names ${fragment}: ${confirmation}`);
      }
      await card.locator('.exf-confirm textarea').fill('Ready to measure');
      await card.locator('.exf-confirm').scrollIntoViewIfNeeded();
      await shot('start-confirm');
      await card.locator('.exf-confirm-start').click();
      await card.locator('.ex-status-running').waitFor();
      assert.deepEqual(await card.locator('.ex-arm-progress .ex-arm-name').allTextContents(), ['controlcontrol', 'treatment']);
      assert.equal(await card.locator('.exf-edit').count(), 0, 'a running experiment offers no Edit');
      assert.deepEqual(requests.filter(request => request.path.endsWith('/start')).at(-1).body, { reason: 'Ready to measure' });

      // Save and start: one confirmation, two writes.
      await page.locator('#experiments-new').click();
      await labelled(form, 'Name').locator('input').fill('trial-b');
      await labelled(form, 'Primary metric').locator('select').selectOption('cost_usd');
      await labelled(form, 'Mode').locator('select').selectOption('live');
      await form.locator('.exf-save-start').click();
      const summary = await form.locator('.exf-confirm').textContent();
      assert.match(summary, /Start trial-b\?/);
      assert.match(summary, /every task/);
      assert.match(summary, /Live: .*changes how those dispatches run/);
      await form.locator('.exf-confirm-start').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const second = page.locator('.ex-item', { hasText: 'trial-b' });
      await second.locator('.ex-status-running').waitFor();
      await page.locator('.ex-list').scrollIntoViewIfNeeded();
      await shot('running');

      // Save and start confirms the definition it shows: while the confirmation
      // is open its controls are disabled, and edits that still reach the form
      // (applied here directly) cannot change what Confirm starts.
      await page.locator('#experiments-new').click();
      await labelled(form, 'Name').locator('input').fill('frozen');
      await labelled(form, 'Primary metric').locator('select').selectOption('cost_usd');
      await form.locator('.exf-save-start').click();
      const frozenSummary = await form.locator('.exf-confirm').textContent();
      for (const fragment of ['Start frozen?', 'Shadow', 'treatment weight 1', 'every task']) {
        assert.ok(frozenSummary.includes(fragment), `confirmation names ${fragment}: ${frozenSummary}`);
      }
      const frozenControls = {
        mode: labelled(form, 'Mode').locator('select'),
        treatmentWeight: labelled(treatment, 'Weight').locator('input'),
        tags: labelled(form, 'Tags to include').locator('input'),
        complexity: form.locator('.exf-population label.exf-check', { hasText: 'medium' }).locator('input'),
      };
      await treatment.evaluate(node => node.scrollIntoView({ block: 'start' }));
      await shot('frozen-arms');
      await form.locator('.exf-confirm').scrollIntoViewIfNeeded();
      await shot('start-confirm-frozen');
      const disabled = {};
      for (const [name, control] of Object.entries(frozenControls)) disabled[name] = await control.isDisabled();
      await frozenControls.mode.evaluate(node => { node.value = 'live'; node.dispatchEvent(new Event('change')); });
      await frozenControls.treatmentWeight.evaluate(node => { node.value = '9'; node.dispatchEvent(new Event('input')); });
      await frozenControls.tags.evaluate(node => { node.value = 'changed-after-confirmation'; node.dispatchEvent(new Event('input')); });
      await frozenControls.complexity.evaluate(node => { node.checked = true; node.dispatchEvent(new Event('change')); });
      await form.locator('.exf-confirm-start').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const frozenCreate = requests.find(request => request.method === 'POST' && request.path === '/api/diagnostics/experiments' && request.body.name === 'frozen');
      assert.ok(frozenCreate, 'the frozen draft is created');
      assert.equal(frozenCreate.body.mode, 'shadow', 'the created mode is the confirmed shadow');
      assert.deepEqual(frozenCreate.body.arms.map(arm => [arm.name, arm.weight]), [['control', 1], ['treatment', 1]], 'the created weights are the confirmed ones');
      assert.deepEqual(frozenCreate.body.population, {}, 'the created population is the confirmed every task');
      assert.deepEqual(requests.filter(request => request.path.endsWith('/start')).at(-1).body, {}, 'the start carries no edited reason');
      assert.deepEqual(disabled, { mode: true, treatmentWeight: true, tags: true, complexity: true }, 'definition controls are disabled while the confirmation is open');

      // Back to editing unlocks the form, and a new Save and start confirms the
      // edited definition.
      await page.locator('#experiments-new').click();
      await labelled(form, 'Name').locator('input').fill('recomputed');
      await labelled(form, 'Primary metric').locator('select').selectOption('cost_usd');
      await form.locator('.exf-save-start').click();
      await form.locator('.exf-confirm button', { hasText: 'Back to editing' }).click();
      assert.equal(await labelled(form, 'Mode').locator('select').isDisabled(), false, 'Back to editing unlocks the form');
      await labelled(form, 'Mode').locator('select').selectOption('live');
      await labelled(form.locator('.exf-arm').nth(1), 'Weight').locator('input').fill('9');
      await labelled(form, 'Tags to include').locator('input').fill('changed-after-confirmation');
      await form.locator('.exf-save-start').click();
      const recomputedSummary = await form.locator('.exf-confirm').textContent();
      for (const fragment of ['Start recomputed?', 'Live', 'treatment weight 9', 'tags include changed-after-confirmation']) {
        assert.ok(recomputedSummary.includes(fragment), `recomputed confirmation names ${fragment}: ${recomputedSummary}`);
      }
      await form.locator('.exf-confirm-start').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const recomputedCreate = requests.find(request => request.method === 'POST' && request.body?.name === 'recomputed');
      assert.ok(recomputedCreate, 'the recomputed draft is created');
      assert.equal(recomputedCreate.body.mode, 'live');
      assert.equal(recomputedCreate.body.arms[1].weight, 9);
      assert.deepEqual(recomputedCreate.body.population, { tags_include: ['changed-after-confirmation'] });
      await page.locator('.ex-item', { hasText: 'recomputed' }).locator('.ex-status-running').waitFor();

      // Edit-draft Save and start: the same confirmation on a saved draft's form.
      await page.locator('#experiments-new').click();
      await labelled(form, 'Name').locator('input').fill('edited');
      await labelled(form, 'Primary metric').locator('select').selectOption('cost_usd');
      await form.locator('.exf-save').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const edited = page.locator('.ex-item', { hasText: 'edited' });
      await edited.locator('.exf-edit').click();
      await labelled(form.locator('.exf-arm').nth(1), 'Weight').locator('input').fill('2');
      await form.locator('.exf-save-start').click();
      const editedSummary = await form.locator('.exf-confirm').textContent();
      for (const fragment of ['Start edited?', 'treatment weight 2', 'every task']) {
        assert.ok(editedSummary.includes(fragment), `edit confirmation names ${fragment}: ${editedSummary}`);
      }
      await form.locator('.exf-confirm-start').click();
      await page.locator('#experiments-form').waitFor({ state: 'hidden' });
      const editPatch = requests.filter(request => request.method === 'PATCH').at(-1);
      assert.equal(editPatch.body.arms[1].weight, 2, 'the patch carries the confirmed weight');
      await edited.locator('.ex-status-running').waitFor();

      // A started experiment's detail says why there is no Edit.
      await page.evaluate(async () => {
        window.experiments.setExperimentId('exp-1');
        await window.experiments.fetchAndRenderExperiments();
      });
      await page.locator('.exf-locked').waitFor();
      assert.match(await page.locator('.exf-locked').textContent(), /running, so its definition can no longer be edited/);
      steps.push({ width, requests: requests.map(({ method, path: route }) => `${method} ${route}`),
        lostStartRefusal: { reasonPreserved: true, refusalVisibleAfterUnchangedPoll: true, retryCancelEditEnabled: true } });
    }
    assert.deepEqual(errors, [], 'no browser errors');
  } finally {
    fs.writeFileSync(path.join(evidence, 'authoring.json'), JSON.stringify({ steps, errors }, null, 2));
    if (heldStartResponse) heldStartResponse();
    for (const held of heldLists) held.release?.(503);
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}
