// Usage: node dashboard_config_topics_browser.mjs /path/to/playwright/index.mjs /evidence/directory
//
// Settings topic drawer in Chromium:
// (a) an array<string> row whose entries contain commas, saved with no edits,
//     writes that same array back without splitting on commas.
// (b) a cleared integer field does not write 0, and a non-numeric integer
//     draft is refused before the request, showing a drawer error.
// (c) the drawer's CLI preview for an array is a valid TOML array literal that
//     matches the PUT value when executed via `orbit config set`.
// (d) Raw TOML: the block shows the file for the latest Show click, whichever
//     read arrives last, and a failed read that is superseded never replaces a
//     newer success. The block names the file it shows.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardCsp, dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  const served = dashboardFile(url.pathname);
  if (!served) {
    res.writeHead(404);
    res.end();
    return;
  }
  res.setHeader('content-type', served.type);
  res.setHeader('content-security-policy', dashboardCsp());
  res.end(served.data);
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const port = server.address().port;

function cleanEnv(home) {
  const env = { ...process.env, HOME: home, USERPROFILE: home };
  const authorityVars = [
    'GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_INDEX_FILE',
    'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES',
    'ORBIT_ROOT', 'ORBIT_REGISTRY_ROOT', 'ORBIT_WORKSPACE',
    'ORBIT_WORKSPACE_CLAIM_TOKEN', 'ORBIT_WORKTREE_ROOT', 'ORBIT_JOB_DIR',
    'ORBIT_ACTIVITY_DIR', 'ORBIT_MANAGED_RUN_CONTEXT',
    'ORBIT_WORKER_CONTEXT_REQUIRED', 'ORBIT_RUN_ID', 'ORBIT_TASK_ID',
    'ORBIT_ACTIVE_TASK_ID', 'ORBIT_SESSION_ID', 'ORBIT_ACTIVITY_ID',
    'ORBIT_STEP_INDEX', 'ORBIT_AGENT_NAME', 'ORBIT_AGENT_MODEL',
    'ORBIT_ACTOR', 'ORBIT_OPERATOR', 'ORBIT_TASK_ACTOR_KIND',
    'ORBIT_ACTIVITY_TOOLS', 'ORBIT_ACTIVITY_TOOL_POLICY',
    'ORBIT_ACTIVITY_TOOLS_DENY', 'ORBIT_ACTIVITY_NAME',
    'ORBIT_ACTIVITY_FS_PROFILE', 'ORBIT_ACTIVITY_DEADLINE_UNIX_MS',
    'ORBIT_PROC_ALLOWED_PROGRAMS', 'ORBIT_PROC_PROGRAM_POLICY',
    'ORBIT_PROC_DISALLOWED_PROGRAMS', 'ORBIT_BIN', 'ORBIT_PLUGIN_BROKER',
  ];
  for (const v of authorityVars) delete env[v];
  return env;
}

const pageErrors = [];
let browser;
try {
  browser = await chromium.launch({
    headless: true,
    executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined,
  });

  const page = await browser.newPage({
    viewport: { width: 1440, height: 900 },
    timezoneId: 'America/Los_Angeles',
  });
  page.on('pageerror', (err) => {
    pageErrors.push(err.message);
    console.error('Page error:', err);
  });
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('Browser console error:', msg.text());
  });

  const capturedPuts = [];
  const rawFiles = {
    workspace: { text: '# workspace marker\n[workflow]\nbase_branch = "workspace"\n' },
    global: { text: '# global marker\n[machine]\nname = "global"\n' },
  };
  const rawFailures = {}; // scope -> error message returned for each read of that file
  const rawHolds = {}; // scope -> the next read of that file, held until released
  const holdRaw = (scope) => {
    const hold = {};
    hold.arrived = new Promise((resolve) => { hold.onArrive = resolve; });
    hold.wait = new Promise((resolve) => { hold.release = resolve; });
    hold.served = new Promise((resolve) => { hold.onServed = resolve; });
    rawHolds[scope] = hold;
    return hold;
  };
  const within = (promise, what) => Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), 10000)),
  ]);
  const releaseRaw = async (hold) => {
    hold.release();
    await within(hold.served, 'held raw response');
    // Give the page a moment to apply or drop the response before asserting.
    await new Promise((resolve) => setTimeout(resolve, 200));
  };
  const effectiveConfig = {
    config_set: { authorized: true },
    sections: [
      {
        token: 'workflow',
        title: 'Shipping',
        keys: [
          {
            key: 'workflow.required_validation_commands',
            label: 'Required validation commands',
            description: 'Validation commands required before delivery.',
            value_type: 'array<string>',
            value: [
              'make ci-guards',
              'cargo fmt --all -- --check, with commas',
              'echo a, b, c',
            ],
            state: 'set',
            settable: true,
            source: { layer: 'workspace' },
            built_in: { value: [] },
            shadowed_by: [],
          },
          {
            key: 'workflow.mid_run_steering.max_rounds',
            label: 'Max steering rounds',
            description: 'Most rounds of operator steering admitted.',
            value_type: 'integer',
            value: 3,
            state: 'set',
            settable: true,
            source: { layer: 'workspace' },
            built_in: { value: 3 },
            shadowed_by: [],
          },
        ],
      },
    ],
  };

  await page.route('**/api/**', async (route) => {
    const url = new URL(route.request().url());
    const method = route.request().method();

    if (method === 'PUT' && url.pathname.startsWith('/api/config/keys/')) {
      const key = decodeURIComponent(url.pathname.slice('/api/config/keys/'.length));
      const body = route.request().postDataJSON();
      capturedPuts.push({ key, body });
      await route.fulfill({ status: 200, json: { ok: true, key, value: body.value } });
      return;
    }

    switch (url.pathname) {
      case '/api/workspaces':
        await route.fulfill({ json: [{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true }] });
        break;
      case '/api/tasks':
      case '/api/tasks/locks':
      case '/api/crews':
      case '/api/workflows/auto/readiness':
      case '/api/routines':
      case '/api/frictions':
      case '/api/frictions/stats':
      case '/api/audit/summary':
      case '/api/job-runs':
        await route.fulfill({ json: { items: [], total: 0, limit: 50, truncated: false, crews: [], routines: [] } });
        break;
      case '/api/config/file': {
        const scope = url.searchParams.get('scope');
        const hold = rawHolds[scope];
        delete rawHolds[scope];
        if (hold) {
          hold.onArrive();
          await hold.wait;
        }
        if (rawFailures[scope]) await route.fulfill({ status: 500, json: { error: rawFailures[scope] } });
        else await route.fulfill({ json: rawFiles[scope] });
        hold?.onServed();
        break;
      }
      case '/api/config/effective':
        await route.fulfill({ json: effectiveConfig });
        break;
      default:
        await route.fulfill({ status: 200, json: {} });
        break;
    }
  });

  await page.goto(`http://127.0.0.1:${port}/#config/workflow`);

  // Wait for the workflow section and rows to render
  const arrayRow = page.locator('.topic-row[data-key="workflow.required_validation_commands"]');
  await arrayRow.waitFor({ state: 'visible' });

  // -------------------------------------------------------------------------
  // Case (a): array<string> with commas survives save without edit
  // -------------------------------------------------------------------------
  await arrayRow.locator('button.config-action:has-text("Edit")').click();
  const drawer = page.locator('aside.config-drawer');
  await drawer.waitFor({ state: 'visible' });

  const textarea = drawer.locator('textarea.config-input');
  await textarea.waitFor({ state: 'visible' });

  const expectedArrayValue = [
    'make ci-guards',
    'cargo fmt --all -- --check, with commas',
    'echo a, b, c',
  ];
  const initialText = await textarea.inputValue();
  assert.equal(initialText, expectedArrayValue.join('\n'));

  // Capture dark theme screenshot at 1440px
  await page.screenshot({ path: path.join(evidence, 'config-topics-drawer-dark-1440.png') });

  // Capture responsive phone screenshot at 375px
  await page.setViewportSize({ width: 375, height: 812 });
  await page.screenshot({ path: path.join(evidence, 'config-topics-drawer-dark-375.png') });
  await page.setViewportSize({ width: 1440, height: 900 });

  // Verify CLI line format
  const cliNode = drawer.locator('.drawer-cli');
  const cliText = (await cliNode.textContent()).trim();
  const expectedCli = `orbit config set workflow.required_validation_commands '["make ci-guards", "cargo fmt --all -- --check, with commas", "echo a, b, c"]'`;
  assert.equal(cliText, expectedCli);

  // Click Save without modifying textarea
  capturedPuts.length = 0;
  await drawer.locator('button.config-action:has-text("Save")').click();

  // Verify that the PUT was executed and carries the exact original array
  await page.waitForFunction(() => document.querySelector('aside.config-drawer') === null);
  assert.equal(capturedPuts.length, 1);
  assert.equal(capturedPuts[0].key, 'workflow.required_validation_commands');
  assert.deepEqual(capturedPuts[0].body.value, expectedArrayValue);
  assert.equal(capturedPuts[0].body.scope, 'workspace');

  // -------------------------------------------------------------------------
  // Case (b): integer cleared does not write 0; non-numeric refused before PUT
  // -------------------------------------------------------------------------
  const intRow = page.locator('.topic-row[data-key="workflow.mid_run_steering.max_rounds"]');
  await intRow.waitFor({ state: 'visible' });
  await intRow.locator('button.config-action:has-text("Edit")').click();
  await drawer.waitFor({ state: 'visible' });

  const intTextarea = drawer.locator('textarea.config-input');
  await intTextarea.waitFor({ state: 'visible' });
  assert.equal(await intTextarea.inputValue(), '3');

  // 1. Cleared field
  capturedPuts.length = 0;
  await intTextarea.fill('');
  await drawer.locator('button.config-action:has-text("Save")').click();

  // Must NOT send PUT, and must show drawer warning
  assert.equal(capturedPuts.length, 0);
  const warning = drawer.locator('.config-warning');
  await warning.waitFor({ state: 'visible' });
  assert.equal(await warning.textContent(), 'workflow.mid_run_steering.max_rounds takes a whole number');

  // 2. Non-numeric draft
  capturedPuts.length = 0;
  await intTextarea.fill('not_a_number');
  await drawer.locator('button.config-action:has-text("Save")').click();

  assert.equal(capturedPuts.length, 0);
  await warning.waitFor({ state: 'visible' });
  assert.equal(await warning.textContent(), 'workflow.mid_run_steering.max_rounds takes a whole number');

  // Close drawer
  await drawer.locator('button.config-action:has-text("Close")').click();
  await page.waitForFunction(() => document.querySelector('aside.config-drawer') === null);

  // -------------------------------------------------------------------------
  // Case (d): Raw TOML block follows the latest Show click and names its file
  // -------------------------------------------------------------------------
  await page.goto(`http://127.0.0.1:${port}/#config/all-keys`);
  const rawBlock = page.locator('section.config-section', { has: page.locator('.config-section-title', { hasText: 'Raw TOML' }) });
  const rawPre = rawBlock.locator('pre.topic-raw');
  const rawLabel = rawBlock.locator('.topic-raw-label');
  const showButton = (scope) => rawBlock.locator(`button.config-action:has-text("Show ${scope} file")`);
  // The block must name the file by its path; anchored so the global path does not satisfy the workspace check.
  const labels = {
    workspace: /\(\.orbit\/config\.toml\)$/,
    global: /\(~\/\.orbit\/config\.toml\)$/,
  };
  const waitForRawText = (text) => page.waitForFunction((t) => document.querySelector('pre.topic-raw')?.textContent === t, text);

  // Workspace read held, global clicked and answered, workspace answer arrives last.
  let hold = holdRaw('workspace');
  await showButton('workspace').click();
  await within(hold.arrived, 'workspace raw read');
  assert.match(await rawLabel.textContent(), labels.workspace);
  await showButton('global').click();
  await waitForRawText(rawFiles.global.text);
  await releaseRaw(hold);
  assert.equal(await rawPre.textContent(), rawFiles.global.text);
  assert.match(await rawLabel.textContent(), labels.global);

  // Global read held, workspace clicked and answered, global answer arrives last.
  hold = holdRaw('global');
  await showButton('global').click();
  await within(hold.arrived, 'global raw read');
  await showButton('workspace').click();
  await waitForRawText(rawFiles.workspace.text);
  await releaseRaw(hold);
  assert.equal(await rawPre.textContent(), rawFiles.workspace.text);
  assert.match(await rawLabel.textContent(), labels.workspace);

  // Workspace read fails, but only after a newer global read has succeeded.
  rawFailures.workspace = 'workspace file could not be read';
  hold = holdRaw('workspace');
  await showButton('workspace').click();
  await within(hold.arrived, 'failing workspace raw read');
  await showButton('global').click();
  await waitForRawText(rawFiles.global.text);
  await releaseRaw(hold);
  assert.equal(await rawPre.textContent(), rawFiles.global.text);
  assert.match(await rawLabel.textContent(), labels.global);
  delete rawFailures.workspace;

  await rawBlock.screenshot({ path: path.join(evidence, 'config-topics-raw-dark-1440.png') });
  await page.setViewportSize({ width: 375, height: 812 });
  await rawBlock.screenshot({ path: path.join(evidence, 'config-topics-raw-dark-375.png') });
  await page.setViewportSize({ width: 1440, height: 900 });

  // -------------------------------------------------------------------------
  // Case (c): array cliCmd line executed in isolated CLI fixture yields PUT value
  // -------------------------------------------------------------------------
  const fixtureBase = path.join(evidence, 'cli-child-fixture');
  fs.rmSync(fixtureBase, { recursive: true, force: true });
  const fixtureHome = path.join(fixtureBase, 'home');
  const fixtureWork = path.join(fixtureBase, 'work');
  fs.mkdirSync(fixtureHome, { recursive: true });
  fs.mkdirSync(path.join(fixtureWork, '.orbit'), { recursive: true });
  fs.writeFileSync(path.join(fixtureWork, '.orbit', 'config.toml'), '');

  const env = cleanEnv(fixtureHome);
  execFileSync('git', ['init'], { cwd: fixtureWork });
  execFileSync('orbit', ['init', '--non-interactive', '--machine-name', 'test', '--task-prefix', 'TST', '--skip-host-prerequisites'], { cwd: fixtureWork, env });
  execFileSync('orbit', ['workspace', 'init', '--force'], { cwd: fixtureWork, env });

  // Run the exact CLI preview command generated by the drawer
  execFileSync('bash', ['-c', cliText], { cwd: fixtureWork, env });

  // Read back the saved value via orbit config get
  const getOutput = execFileSync('orbit', ['config', 'get', 'workflow.required_validation_commands', '--json'], { cwd: fixtureWork, env });
  const getParsed = JSON.parse(getOutput.toString());
  assert.deepEqual(getParsed.value, expectedArrayValue);

  assert.equal(pageErrors.length, 0, `Page errors encountered: ${pageErrors.join('; ')}`);

  fs.writeFileSync(path.join(evidence, 'dashboard-config-topics-result.json'), JSON.stringify({
    passed: true,
    assertions: [
      'array-commas-preserved-on-save',
      'cleared-integer-not-written-to-zero',
      'non-numeric-integer-refused-before-request',
      'array-cli-preview-toml-literal-round-trips',
      'raw-latest-click-wins-when-earlier-read-arrives-last',
      'raw-latest-click-wins-when-later-read-arrives-last',
      'raw-superseded-failure-does-not-replace-newer-success',
      'raw-block-names-the-file-it-shows',
    ],
  }, null, 2));

  console.log('All dashboard config topics browser checks passed successfully.');
} finally {
  await browser?.close();
  await new Promise((resolve) => server.close(resolve));
}
