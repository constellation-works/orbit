// Usage: node dashboard_distributed_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
//
// ORB-12516 in a real browser. The Node DOM harness proves the logic; this
// proves the shipped markup, styles and modules actually render it — that the
// claim panel fits the detail column without forcing the page sideways, that
// its controls are reachable and operable from the keyboard, and that an
// operator decision travels from a real click to a real request.
//
// Everything it talks to is fixture data served from a disposable local HTTP
// server and a stubbed `fetch`. No Orbit store, scheduler, dashboard or host is
// contacted, and nothing is mutated anywhere.
import { fileURLToPath, pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const assets = fileURLToPath(new URL('../../assets/dashboard/', import.meta.url));
const test = fileURLToPath(new URL('./dashboard_distributed.mjs', import.meta.url));
const server = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://fixture').pathname;
  const file = name === '/test.mjs' ? test : path.join(assets, name === '/' ? 'index.html' : path.basename(name));
  if (!fs.existsSync(file)) { res.writeHead(404); res.end(); return; }
  let data = fs.readFileSync(file);
  // The page's own orchestrator would start fetching live endpoints; this
  // fixture drives one module directly.
  if (name === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', file.endsWith('.html') ? 'text/html' : file.endsWith('.css') ? 'text/css' : 'text/javascript');
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

let browser;
let page;
const capture = async (name) => {
  await page.screenshot({ path: path.join(evidence, `${name}.png`), fullPage: true });
};
const assertNoOverflow = async (label) => {
  const width = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, inner: window.innerWidth }));
  if (width.scroll > width.inner + 1) {
    await capture(`overflow-${label}`);
    throw new Error(`Horizontal overflow at ${label}: scrollWidth=${width.scroll} innerWidth=${width.inner}`);
  }
};

try {
  browser = await chromium.launch({ headless: true });
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const pageErrors = [];
  page.on('pageerror', error => { pageErrors.push(String(error)); console.error(error); });
  await page.goto(`http://127.0.0.1:${server.address().port}/`);

  // 1. The behaviour scenario, against the real DOM and the shipped modules.
  await page.addScriptTag({ type: 'module', url: '/test.mjs' });
  await page.waitForFunction(() => globalThis.distributedTestsPassed, undefined, { timeout: 20000 });
  if (pageErrors.length) throw new Error(`page errors during the scenario: ${pageErrors.join(' | ')}`);

  // 2. The panel rendered into the real task-detail column, at the widths the
  //    dashboard actually ships. A claim panel that forces the page sideways is
  //    unusable however correct its contents.
  const claimFixture = {
    claim_id: 'claim-1', task_id: 'ORB-2', request_id: 'pull-1',
    phase: 'handed_off',
    phase_summary: 'delivery handed off and awaiting completion authority — this is not a code review',
    authorizes_execution: false, unsettled: true,
    executed_on: { known: true, machine_id: 'hm_9ca6004473492f06', host_id: 'runner-2' },
    run_context: { run_id: 'drain-1', job_name: 'auto', host_id: null },
    bound_run: { machine_id: 'hm_9ca6004473492f06', run_id: 'jrun-20260919-2210-a1' },
    bound_run_navigable: false,
    inspect_on: 'inspect this run on machine hm_9ca6004473492f06 (no owner-local run exists)',
    footprint: ['file:crates/orbit-web/src/api/mod.rs', 'dir:crates/orbit-web/assets/dashboard'],
    footprint_protected: true,
    reservation: {
      id: 'res-1', expires_at: '2026-09-19T00:00:00+00:00', expired: true,
      note: 'reservation window elapsed — the claim is still live and its frozen footprint still protects these files; expiry is not revocation and not proof the attempt died',
    },
    created_at: '2026-09-18T00:00:00+00:00', updated_at: '2026-09-19T00:00:00+00:00',
    last_event: 'handoff_accepted', age_seconds: 90000,
    unresolved_merge_intent: null, landing_invalidated: false,
    handoff: {
      handoff_id: 'handoff1', accepted_at: '2026-09-19T00:00:00+00:00',
      task_id: 'ORB-2', claim_id: 'claim-1',
      executed_on: { known: true, machine_id: 'hm_9ca6004473492f06', host_id: null },
      run_id: 'jrun-20260919-2210-a1', execution_summary: 'Outcome: success',
      candidate: {
        repository: 'owner/repository', source_branch: 'orbit/ORB-2-6aaf7fd3',
        base_branch: 'agent-main', landing_branch: 'agent-main',
        candidate: { commit: 'a'.repeat(40), tree: 'b'.repeat(40) },
        base: { commit: 'c'.repeat(40), tree: 'd'.repeat(40) },
        delivery: { kind: 'pull_request', number: 2367 },
      },
      review: {
        policy: 'none', disposition: 'not_required', is_code_review: false,
        summary: 'review not required (policy none) — no reviewer ran and no verdict exists',
      },
      required_commands: ['make ci-fast', 'make ci-lint'],
      validation: [{ path: 'validation/ci-fast.json', sha256: 'e'.repeat(64) }],
      authority: { state: 'not_authorized', summary: 'no completion authority is recorded; this handoff waits for explicit owner approval', authorization_id: null, recorded_at: null },
      landing: { state: 'none', summary: 'no landing attempt has been reserved', attempt: null, job_run_id: null, evidence: null, merged: false, deployed: null },
      uncertain_merge_intent: null,
    },
  };

  const sent = await page.evaluate(async (claim) => {
    const requests = [];
    globalThis.fetch = async (url, options = {}) => {
      const method = (options && options.method) || 'GET';
      const body = {
        schema_version: 1, owner_workspace: true, refusal: null, refusal_detail: null,
        distributed_execution_enabled: false, claims: [claim],
        capabilities: {
          handoff_approve: { authorized: true, reason: null },
          handoff_revoke: { authorized: true, reason: null },
          claim_recover: { authorized: true, reason: null },
        },
      };
      if (method !== 'GET') {
        requests.push({ url: String(url), body: options.body ? JSON.parse(options.body) : null });
        return { ok: true, status: 200, json: async () => ({ ok: true }), text: async () => JSON.stringify({ ok: true, result: { handoff_id: 'handoff1', claim_id: 'claim-1', phase: 'handed_off', task_status: 'review' } }) };
      }
      return { ok: true, status: 200, json: async () => body, text: async () => JSON.stringify(body) };
    };
    const { buildDistributedBlock, invalidateDistributedConsole } = await import('/distributed.js');
    invalidateDistributedConsole();
    // The app orchestrator that activates a tab is stripped from this fixture,
    // so select the Tasks pane the way the router would before measuring.
    document.querySelector('.tab-pane[data-tab="tasks"]').classList.add('active');
    // The real detail column, so the panel is measured inside the grid it ships in.
    const host = document.getElementById('tasks-body');
    host.innerHTML = '<div class="row-detail split-layout"><div class="detail-main" id="fixture-detail-main"></div><div class="detail-side"></div></div>';
    document.getElementById('fixture-detail-main').appendChild(buildDistributedBlock('ORB-2'));
    await new Promise(resolve => setTimeout(resolve, 50));
    globalThis.fixtureRequests = requests;
    return requests;
  }, claimFixture);
  if (sent.length !== 0) throw new Error('rendering must not send a decision');

  await page.waitForSelector('.claim-panel', { timeout: 5000 });
  const rendered = await page.textContent('.claim-panel');
  for (const required of [
    'machine hm_9ca6004473492f06 · host runner-2',
    'inspect this run on machine hm_9ca6004473492f06',
    'expiry is not revocation',
    'not_required (policy none)',
    'this is not a code review',
    'pull request #2367',
    'waits for explicit owner approval',
  ]) {
    if (!rendered.includes(required)) throw new Error(`the rendered panel is missing: ${required}`);
  }
  if (/\brevoked\b/i.test(rendered)) throw new Error('an expired reservation must not read as revoked');

  for (const viewport of [
    { name: '1440', width: 1440, height: 1000 },
    { name: '1024', width: 1024, height: 900 },
    { name: '768', width: 768, height: 900 },
    { name: '390', width: 390, height: 844 },
  ]) {
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    await assertNoOverflow(`claim-panel-${viewport.name}`);
    await capture(`claim-panel-${viewport.name}`);
  }
  await page.setViewportSize({ width: 1440, height: 1000 });

  // 3. Keyboard operability: the block header is a real toggle, and the action
  //    is reachable and activatable without a pointer.
  const heading = await page.$('.distributed-block h4');
  if (!heading) throw new Error('the distributed block must ship a heading');
  if (await heading.getAttribute('role') !== 'button') throw new Error('the block heading must carry button semantics');
  if (await heading.getAttribute('aria-expanded') !== 'true') throw new Error('the block heading must report its expanded state');
  await heading.focus();
  await page.keyboard.press('Enter');
  if (await heading.getAttribute('aria-expanded') !== 'false') throw new Error('Enter must collapse the block');
  await page.keyboard.press('Enter');
  if (await heading.getAttribute('aria-expanded') !== 'true') throw new Error('Enter must re-expand the block');

  const approve = await page.$('button.claim-action.approve');
  if (!approve) throw new Error('an unauthorized handoff must offer approval');
  if (await approve.getAttribute('type') !== 'button') throw new Error('an action must be a real button');
  await approve.focus();
  await page.keyboard.press('Enter');
  await page.waitForFunction(() => globalThis.fixtureRequests.length > 0, undefined, { timeout: 5000 });
  const decision = await page.evaluate(() => globalThis.fixtureRequests[0]);
  if (!decision.url.includes('/api/distributed/handoffs/handoff1/approve')) {
    throw new Error(`the decision went to ${decision.url}`);
  }
  if (decision.body.expected_candidate_commit !== 'a'.repeat(40)) {
    throw new Error('the decision must carry the exact candidate the operator was shown');
  }
  if (!decision.body.request_id) throw new Error('the decision must carry a replay identity');
  await capture('claim-panel-after-approval');

  if (pageErrors.length) throw new Error(`page errors: ${pageErrors.join(' | ')}`);
  fs.writeFileSync(
    path.join(evidence, 'distributed-assertions.json'),
    `${JSON.stringify({
      assertions: [
        'browser-renders-claim-provenance',
        'browser-sends-exact-owner-decision',
        'browser-keeps-claim-panel-operable',
      ],
      viewports: ['1440', '1024', '768', '390'],
      decision,
    }, null, 2)}\n`,
  );
  console.log('dashboard distributed claim provenance verified in a real browser');
} catch (error) {
  if (page) {
    try { await capture('failure'); } catch (_) {}
    try { fs.writeFileSync(path.join(evidence, 'failure.html'), await page.content()); } catch (_) {}
  }
  console.error(error);
  process.exitCode = 1;
} finally {
  if (browser) await browser.close();
  server.close();
}
