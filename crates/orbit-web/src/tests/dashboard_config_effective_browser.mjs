// Usage: node dashboard_config_effective_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
//
// Settings › Effective in Chromium. The shipped app runs against a disposable
// fixture server whose `/api/config/effective` resolves a small registry the
// way the real endpoint does: built-in, then global, then workspace, with
// execution keys skipping the global file while a workspace file exists.
// Its PUT and DELETE key routes change the fixture files, so a save or a
// removal is read back from the next payload. It checks the layer counts,
// the restates-global filter, the inspector chain for an overridden key and
// for a not-inherited one, a save to the chosen file with its CLI preview,
// a refused save, removing a workspace override, crews by provider, paths,
// and a 375px phone with no horizontal page scroll. No Orbit store is contacted.
import { pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const HOSTS = {
  host_file: '/home/fixture/.orbit/hosts.toml', legacy: false, generation: 1, load_error: null,
  host_edit: { authorized: true, reason: null },
  hosts: [{
    name: 'alpha', machine_id: 'hm_alpha', ssh: null, task_prefix: 'ALP', local: true, legacy: false,
    reachable: true, error: null, binary_version: '0.28.2', protocol_fingerprint: 'fp', skew: false, skew_fields: [], workspaces: [],
  }],
};
const GLOBAL_PATH = '/home/fixture/.orbit/config.toml';
const WORKSPACE_PATH = '/home/fixture/orbit/.orbit/config.toml';

// The registry: section, type, choices, the built-in default (absent when
// the key has none) and the two write facts the server reports.
const SECTIONS = [
  { token: 'machine', title: 'Machine (machine.*)', blurb: 'This machine\'s identity.', key_prefix: 'machine' },
  { token: 'delivery', title: 'Delivery (workflow.*)', blurb: 'How work is claimed and shipped.', key_prefix: 'workflow' },
  { token: 'execution', title: 'Execution (execution.*)', blurb: 'How agents run.', key_prefix: 'execution', not_inherited: true },
];
const KEYS = [
  { key: 'machine.id', section: 'machine', type: 'string', readOnly: 'a machine identity is generated once by `orbit init` and never reused', globalOnly: true, description: 'Generated machine identity.' },
  { key: 'machine.name', section: 'machine', type: 'string', globalOnly: true, description: 'Display name for this machine.' },
  { key: 'workflow.base_branch', section: 'delivery', type: 'string', builtIn: 'main', description: 'Branch task PRs target.' },
  { key: 'workflow.merge_method', section: 'delivery', type: 'string', options: ['merge', 'squash', 'rebase'], builtIn: 'merge', description: 'How a landed PR is merged.' },
  { key: 'workflow.max_active_runs', section: 'delivery', type: 'integer', builtIn: 4, description: 'Most runs active at once.' },
  { key: 'workflow.default_crew', section: 'delivery', type: 'string', builtIn: 'astra', description: 'Crew used when nothing else picks one.' },
  { key: 'workflow.low_complexity_crews', section: 'delivery', type: 'array<string>', builtIn: [], description: 'Weighted crew pool for low-complexity tasks.' },
  { key: 'workflow.auto_merge', section: 'delivery', type: 'bool', builtIn: false, description: 'Merge a PR once checks pass.' },
  { key: 'workflow.release_branch', section: 'delivery', type: 'string', description: 'Branch releases are cut from.' },
  { key: 'execution.codex.sandbox', section: 'execution', type: 'string', options: ['read-only', 'workspace-write', 'danger-full-access'], builtIn: 'workspace-write', replaceOnly: true, description: 'Codex sandbox mode.' },
  { key: 'execution.env.pass', section: 'execution', type: 'array<string>', builtIn: [], replaceOnly: true, description: 'Environment variables passed to agents.' },
  { key: 'execution.codex.approval_policy', section: 'execution', type: 'string', options: ['never', 'on-request'], builtIn: 'never', replaceOnly: true, description: 'When Codex asks before acting.' },
];
const files = {
  global: {
    'machine.id': 'hm_fixture', 'machine.name': 'alpha', 'workflow.base_branch': 'agent-main', 'workflow.merge_method': 'squash',
    'workflow.max_active_runs': 6, 'workflow.default_crew': 'astra', 'execution.codex.sandbox': 'danger-full-access', 'execution.env.pass': ['GH_TOKEN'],
  },
  workspace: {
    'workflow.base_branch': 'agent-main', 'workflow.merge_method': 'squash', 'workflow.max_active_runs': 8,
    'workflow.low_complexity_crews': ['astra:2', 'nova:1'], 'execution.codex.sandbox': 'read-only',
  },
};
const same = (left, right) => JSON.stringify(left) === JSON.stringify(right);
const has = (file, key) => Object.prototype.hasOwnProperty.call(files[file], key);

function resolveRow(def) {
  const builtIn = def.builtIn === undefined ? { state: 'unset', value: null } : { state: 'default', value: def.builtIn };
  const row = {
    key: def.key, label: def.key.split('.').at(-1), value_type: def.type, options: def.options || [], section: def.section,
    description: def.description, settable: !def.readOnly, read_only_reason: def.readOnly || null, global_only: Boolean(def.globalOnly),
    built_in: builtIn, shadowed_by: [], fallback: null,
  };
  const globalValue = has('global', def.key) ? files.global[def.key] : undefined;
  if (has('workspace', def.key)) {
    Object.assign(row, { value: files.workspace[def.key], state: 'set', source: { layer: 'workspace', path: WORKSPACE_PATH } });
    if (globalValue !== undefined) row.shadowed_by.push({ layer: 'global', value: globalValue, reason: 'overridden', note: null });
    row.fallback = globalValue !== undefined && !def.replaceOnly
      ? { layer: 'global', value: globalValue, state: 'set' }
      : { layer: 'built-in', value: builtIn.value, state: builtIn.state };
  } else if (globalValue !== undefined && !def.replaceOnly) {
    Object.assign(row, { value: globalValue, state: 'set', source: { layer: 'global', path: GLOBAL_PATH } });
  } else {
    Object.assign(row, { value: builtIn.value, state: builtIn.state, source: { layer: 'built-in', path: null } });
    if (globalValue !== undefined) row.shadowed_by.push({ layer: 'global', value: globalValue, reason: 'not-inherited', note: 'execution.* is not inherited' });
  }
  row.equals = { global: globalValue === undefined ? null : same(row.value, globalValue), built_in: builtIn.state === 'default' && same(row.value, builtIn.value) };
  return row;
}

function effective() {
  const sections = SECTIONS.map((section) => {
    const keys = KEYS.filter((def) => def.section === section.token).map(resolveRow);
    const count = (state) => keys.filter((row) => row.state === state).length;
    return {
      ...section, kind: 'keys', not_inherited: Boolean(section.not_inherited),
      counts: { set: count('set'), default: count('default'), unset: count('unset'), total: keys.length }, keys,
    };
  });
  sections.push({ token: 'crews', title: 'Crews', blurb: 'Named crews.', key_prefix: 'crews', kind: 'crews', counts: { set: 0, default: 0, unset: 0, total: 0 }, keys: [] });
  const limit = (crew, used, window) => ({ crew, provider: 'codex', scope: null, window, used_percent: used, exhausted: false, resets_at: '2026-10-11T20:00:00Z', threshold: 90, gated: false, until: null });
  return {
    scope: 'effective',
    layers: {
      built_in: { label: 'built-in' }, global: { path: GLOBAL_PATH, exists: true }, workspace: { path: WORKSPACE_PATH, exists: true },
      execution_not_inherited: true, not_inherited_keys: ['execution.env.pass'],
    },
    workspace_binding: {
      base_branch: 'agent-main', ship_mode: 'pr', owner_machine_id: 'hm_9ca6004473492f06', repo_root: '/home/fixture/orbit', source: 'registry',
      workflow_base_branch: files.workspace['workflow.base_branch'] ?? files.global['workflow.base_branch'], base_branch_matches_workflow: true,
    },
    sections,
    crews: [
      { name: 'astra', enabled: true, provider: 'codex', model: 'gpt-6-astra', effort: 'high', tags: [], source: 'global', referenced_by: ['workflow.default_crew', 'workflow.low_complexity_crews', 'auto-task delivery-code-review'], limit: limit('astra', 62, '5h') },
      { name: 'nova', enabled: true, provider: 'codex', model: 'gpt-6-nova', effort: 'medium', tags: [], source: 'workspace', referenced_by: ['workflow.low_complexity_crews'], limit: limit('nova', 62, '5h') },
      { name: 'sol', enabled: true, provider: 'claude', model: 'claude-opus-5-5', effort: 'high', tags: [], source: 'global', referenced_by: [], limit: null },
      { name: 'claude-default', enabled: true, provider: 'claude', model: 'claude-sonnet-5-5', effort: 'medium', tags: [], source: 'built-in', referenced_by: [], limit: null },
    ],
    crews_enumerated: true,
    paths: [
      { label: 'global root', value: '/home/fixture/.orbit' },
      { label: 'workspace root', value: '/home/fixture/orbit/.orbit' },
      { label: 'local root', value: '/home/fixture/orbit/.orbit' },
      { label: 'audit db', value: '/home/fixture/orbit/.orbit/state/audit.db' },
    ],
    crew_fields: [], write_scope_default: 'workspace', workspace_file_exists: true,
    review: {
      healthy: true,
      before_pr: { enabled: false, source: 'built-in', minutes: 20, minutes_source: 'built-in', crews: [], crew_source: 'built-in', problems: [], line: 'off (built-in)' },
      before_landing: { enabled: true, source: 'workspace', hosts: [], hosts_source: 'built-in', problems: [], warnings: [], line: 'on (workspace)' },
      after_landing: { enabled: true, source: 'auto-task delivery-code-review', next_batch_due: null, health: { healthy: true, problems: [], line: 'healthy' }, line: 'on (auto-task delivery-code-review)' },
    },
    config_set: { authorized: true, reason: null },
  };
}

const writes = [];
function keyWrite(req, url, res, key, json) {
  const def = KEYS.find((entry) => entry.key === key);
  if (!def) return json({ error: `unknown key ${key}` }, 400);
  if (req.method === 'DELETE') {
    const scope = url.searchParams.get('scope');
    writes.push({ method: 'DELETE', key, scope });
    delete files[scope][key];
    return json({ ok: true });
  }
  let raw = '';
  req.on('data', (chunk) => { raw += chunk; });
  req.on('end', () => {
    const body = JSON.parse(raw || '{}');
    writes.push({ method: 'PUT', key, ...body });
    if (def.readOnly) return json({ error: `${key} is read-only` }, 400);
    if (body.scope === 'workspace' && def.globalOnly) return json({ error: `${key} may only be set in the global config` }, 400);
    if (def.type === 'integer' && body.value < 1) return json({ error: `${key} must be at least 1` }, 400);
    files[body.scope][key] = body.value;
    return json({ ok: true });
  });
}

function api(rest, url, req, res) {
  const json = (body, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
  if (rest === 'config/effective') return json(effective());
  if (rest.startsWith('config/keys/') && (req.method === 'PUT' || req.method === 'DELETE')) {
    return keyWrite(req, url, res, decodeURIComponent(rest.slice('config/keys/'.length)), json);
  }
  if (rest === 'hosts') return json(HOSTS);
  if (rest === 'hosts/alpha/connection') {
    return json({ host: 'alpha', machine_id: 'hm_alpha', local: true, origin: 'local', binary_version: '0.28.2', protocol_fingerprint: 'fp', skew: false, skew_fields: [], error: null, forward_writes: { authorized: true, reason: null }, reachable: true });
  }
  if (rest === 'workspaces') return json([{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true }]);
  if (rest === 'tasks') return json({ items: [], total: 0, limit: 50, truncated: false });
  if (rest === 'tasks/all') return json({ items: [], total: 0 });
  if (rest === 'tasks/locks') return json({ total_locked: 0, total_tasks: 0, by_task: [] });
  if (rest === 'crews') return json({ default_crew: null, crews: [] });
  if (rest === 'host/resources') return json({ sampled_at: new Date().toISOString(), cpu: { used_pct: 10 }, memory: { used_pct: 10, used_bytes: 1, total_bytes: 2 }, disks: [] });
  if (rest === 'log') return json({ events: [], offset: 0, agent_offset: 0 });
  if (rest === 'log/stream') {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    res.write(': open\n\n');
    return;
  }
  if (rest === 'workflows/auto/readiness') return json({ controls_authorized: true, tasks: [], capacity: { active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4 } });
  if (rest === 'routines') return json({ machine_name: 'alpha', capabilities: {}, clock: {}, routines: [] });
  if (rest.startsWith('runs') || rest === 'audit') return json([]);
  if (rest === 'audit/summary') return json({ window: '24h', events: 0, failed_runs: 0 });
  return json({ items: [], total: 0 });
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (!url.pathname.startsWith('/api/')) {
    const served = dashboardFile(url.pathname);
    if (!served) { res.writeHead(404); res.end(); return; }
    res.setHeader('content-type', served.type);
    res.end(served.data);
    return;
  }
  const forwarded = url.pathname.match(/^\/api\/on\/[^/]+\/(.+)$/);
  return api(forwarded ? forwarded[1] : url.pathname.slice('/api/'.length), url, req, res);
});
const sockets = new Set();
server.on('connection', (socket) => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

const assert = (condition, message) => { if (!condition) throw new Error(message); };
const until = async (label, predicate, timeout = 10000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    if (await predicate()) return;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
};

let browser;
let page;
const pageErrors = [];
const evidenceLog = {};

const layers = () => page.locator('#config-body .cfx-layer').evaluateAll((nodes) => nodes.map((node) => ({
  layer: node.dataset.layer,
  pressed: node.getAttribute('aria-pressed'),
  number: node.querySelector('.cfx-layer-number')?.textContent ?? null,
  count: node.querySelector('.cfx-layer-count')?.textContent ?? null,
})));
const rows = () => page.locator('#config-body .cfx-row').evaluateAll((nodes) => nodes.map((node) => ({
  key: node.dataset.key,
  layer: node.dataset.layer,
  status: node.querySelector('.cfx-status')?.textContent || '',
  pressed: node.getAttribute('aria-pressed'),
})));
const inspector = () => page.locator('#config-body .cfx-inspector').evaluate((node) => ({
  key: node.querySelector('.cfx-inspector-key')?.textContent,
  chain: [...node.querySelectorAll('.cfx-chain-step')].map((step) => ({
    layer: step.dataset.layer,
    cls: step.className.replace('cfx-chain-step', '').trim(),
    value: step.querySelector('.cfx-chain-value').textContent,
    struck: getComputedStyle(step.querySelector('.cfx-chain-value')).textDecorationLine.includes('line-through'),
    note: step.querySelector('.cfx-chain-note')?.textContent || '',
    wins: Boolean(step.querySelector('.cfx-wins')),
  })),
  cli: node.querySelector('#cfx-cli')?.textContent || null,
  fallback: node.querySelector('.cfx-fallback')?.textContent || null,
  notice: node.querySelector('.cfx-notice')?.textContent || null,
  remove: Boolean([...node.querySelectorAll('button')].find((button) => button.textContent === 'Remove workspace override')),
}));
const row = (key) => page.locator(`#config-body .cfx-row[data-key="${key}"]`);
const select = async (key) => {
  await row(key).click();
  await until(`${key} in the inspector`, async () => (await inspector()).key === key);
};
const noPageOverflow = () => page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);

try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
  page = await context.newPage();
  page.on('pageerror', (error) => pageErrors.push(error.message));

  // 1. The layer strip counts what each layer supplies: keys at their
  //    built-in default, and for each file the keys it wins and carries.
  await page.goto(`${origin}/?workspace=ws_orbit#config/effective`);
  await page.locator('#config-body .cfx-row').first().waitFor({ state: 'visible' });
  const strip = await layers();
  evidenceLog.layers = strip;
  assert(strip.map((card) => card.layer).join() === 'built-in,global,workspace,registry', `four layer cards: ${JSON.stringify(strip)}`);
  assert(strip[0].number === '3' && strip[0].count.includes('keys at default'), `three keys sit at their built-in default: ${JSON.stringify(strip[0])}`);
  assert(strip[1].number === '3' && strip[1].count.includes('8 in file') && strip[1].pressed === 'false', `global wins 3 of its 8 keys: ${JSON.stringify(strip[1])}`);
  assert(strip[2].number === '5' && strip[2].count.includes('all 5 in file'), `workspace wins all 5 of its keys: ${JSON.stringify(strip[2])}`);
  const registry = await page.locator('#config-body .cfx-layer.registry').textContent();
  assert(registry.includes('agent-main') && registry.includes('hm_9ca6004473492f06') && registry.includes('✓ base_branch matches'), `the registry card names its binding: ${registry}`);
  const initial = await rows();
  assert(initial.length === 8 && initial.every((entry) => entry.layer !== 'built-in'), `All set lists the 8 set keys: ${JSON.stringify(initial)}`);
  const statusOf = Object.fromEntries(initial.map((entry) => [entry.key, entry.status]));
  assert(statusOf['workflow.base_branch'] === '= global' && statusOf['workflow.max_active_runs'] === 'global: 6'
    && statusOf['execution.codex.sandbox'] === 'global not inherited' && statusOf['workflow.default_crew'] === '= built-in default'
    && statusOf['machine.id'] === 'read-only', `each row states its relation to the other layers: ${JSON.stringify(statusOf)}`);
  const tokens = await row('workflow.low_complexity_crews').locator('.cfx-tok').evaluateAll((nodes) => nodes.map((node) => [node.textContent, node.querySelector('.cfx-weight')?.textContent]));
  assert(JSON.stringify(tokens) === JSON.stringify([['astra:2', ':2'], ['nova:1', ':1']]), `a pool renders one token per crew with its weight dimmed: ${JSON.stringify(tokens)}`);

  // The Global card filters the table to the keys the global file wins.
  await page.locator('#config-body .cfx-layer[data-layer="global"]').click();
  await until('the global filter', async () => (await rows()).every((entry) => entry.layer === 'global'));
  assert((await rows()).length === 3, 'the Global card shows the 3 keys it wins');
  assert((await layers())[1].pressed === 'true', 'the Global card is pressed while it filters');
  await page.locator('#config-body .cfx-layer[data-layer="global"]').click();
  await until('every set key again', async () => (await rows()).length === 8);

  // 2. The restates callout names the workspace keys that copy the global
  //    value and filters to exactly those.
  const restates = page.locator('#config-body .cfx-callout[data-callout="restates"]');
  assert((await restates.locator('strong').textContent()) === '2 workspace keys restate the global value', 'the callout counts the restating keys');
  const notInherited = await page.locator('#config-body .cfx-callout[data-callout="not-inherited"]').textContent();
  assert(notInherited.includes('execution.env.pass'), `the not-inherited callout names the ignored global value: ${notInherited}`);
  await restates.getByRole('button', { name: 'Show the 2 keys' }).click();
  await until('the restates filter', async () => (await rows()).length === 2);
  const restating = await rows();
  evidenceLog.restating = restating;
  assert(restating.map((entry) => entry.key).join() === 'workflow.base_branch,workflow.merge_method', `exactly the restating keys: ${JSON.stringify(restating)}`);
  assert(await page.locator('#config-body .cfx-seg[data-filter="restates"]').getAttribute('aria-pressed') === 'true', 'the Restates global segment is pressed');
  await page.locator('#config-body .cfx-seg[data-filter="set"]').click();
  await until('All set again', async () => (await rows()).length === 8);

  // 3. The inspector for an overridden key: the built-in and global values
  //    are struck, the workspace value is effective, and removal falls back
  //    to the global value.
  await select('workflow.max_active_runs');
  const overridden = await inspector();
  evidenceLog.overridden = overridden;
  assert(overridden.chain.map((step) => `${step.layer}:${step.cls}:${step.value}`).join('|') === 'built-in:over:4|global:over:6|workspace:win:8', `the chain marks the winner: ${JSON.stringify(overridden.chain)}`);
  assert(overridden.chain[0].struck && overridden.chain[1].struck && !overridden.chain[2].struck && overridden.chain[2].wins, 'overridden values are struck through and the winner is marked effective');
  assert(overridden.fallback === 'Removing it falls back to the global value, 6.', `removal names the fallback: ${overridden.fallback}`);
  assert(overridden.cli === 'orbit config set workflow.max_active_runs 8', `the CLI preview writes the workspace file: ${overridden.cli}`);

  // 4. The inspector for a not-inherited key: a workspace value pins the
  //    sandbox, the ignored global value says why, and removal reaches the
  //    built-in default, not the global value.
  await select('execution.codex.sandbox');
  const pinned = await inspector();
  evidenceLog.pinned = pinned;
  const pinnedGlobal = pinned.chain.find((step) => step.layer === 'global');
  assert(pinnedGlobal.cls === 'ignored' && pinnedGlobal.struck && pinnedGlobal.note.startsWith('Not inherited'), `the global value is marked not inherited: ${JSON.stringify(pinnedGlobal)}`);
  assert(pinned.fallback === 'Removing it falls back to the built-in default, workspace-write (the global value is not inherited).', `the fallback skips the global value: ${pinned.fallback}`);
  // A key whose global value is ignored without a workspace value is found
  // under its section's default keys.
  await page.locator('#config-body .cfx-group[data-section="execution"] .cfx-held-toggle').click();
  await row('execution.env.pass').waitFor({ state: 'visible' });
  await select('execution.env.pass');
  const ignored = await inspector();
  assert((await rows()).find((entry) => entry.key === 'execution.env.pass').status === 'global not inherited', 'the table marks the ignored global value');
  assert(ignored.chain.map((step) => `${step.layer}:${step.cls}:${step.value}`).join('|') === 'built-in:win:empty|global:ignored:GH_TOKEN|workspace:none:not set', `the built-in default wins over an ignored global value: ${JSON.stringify(ignored.chain)}`);
  assert(!ignored.remove, 'a key with no workspace value offers no removal');

  // A global-only key cannot be written to the workspace file; a read-only
  // key says why it has no editor.
  await select('machine.name');
  const scopes = await page.locator('#config-body .cfx-radio input').evaluateAll((nodes) => nodes.map((node) => [node.value, node.disabled, node.checked]));
  assert(JSON.stringify(scopes) === JSON.stringify([['workspace', true, false], ['global', false, true]]), `a global-only key writes the global file: ${JSON.stringify(scopes)}`);
  await select('machine.id');
  const readOnly = await page.locator('#config-body .cfx-readonly').textContent();
  assert(readOnly.startsWith('Read-only. A machine identity is generated once by `orbit init`') && await page.locator('#config-body .cfx-editor').count() === 0, `a read-only key explains itself: ${readOnly}`);

  // 5. Save writes the chosen file through the key route; the CLI preview
  //    follows the draft, and the result is read back from the server.
  await select('workflow.max_active_runs');
  await page.locator('#cfx-edit-value').fill('10');
  await page.locator('#config-body .cfx-radio[data-scope="global"]').click();
  await until('the global CLI preview', async () => (await inspector()).cli === 'orbit config set --global workflow.max_active_runs 10');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await until('the saved global value', async () => (await inspector()).chain.find((step) => step.layer === 'global')?.value === '10');
  const saved = await inspector();
  evidenceLog.saved = { writes: [...writes], inspector: saved };
  assert(JSON.stringify(writes.at(-1)) === JSON.stringify({ method: 'PUT', key: 'workflow.max_active_runs', value: 10, scope: 'global' }), `the PUT carries the typed value and chosen file: ${JSON.stringify(writes.at(-1))}`);
  assert(saved.notice === 'Saved workflow.max_active_runs to the global file.', `the save is confirmed: ${saved.notice}`);
  assert((await rows()).find((entry) => entry.key === 'workflow.max_active_runs').status === 'global: 10', 'the table reads the new global value back');

  // A write the server refuses keeps the draft and says why; nothing changes.
  // After a save the next draft writes the key's own file again.
  assert(await page.locator('#config-body .cfx-radio[data-scope="workspace"] input').isChecked(), 'the next edit targets the winning file');
  await page.locator('#cfx-edit-value').fill('0');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.locator('#config-body .cfx-notice.error').waitFor({ state: 'visible' });
  const refused = await inspector();
  evidenceLog.refused = { write: writes.at(-1), inspector: refused };
  assert(JSON.stringify(writes.at(-1)) === JSON.stringify({ method: 'PUT', key: 'workflow.max_active_runs', value: 0, scope: 'workspace' }), `the refused PUT was sent: ${JSON.stringify(writes.at(-1))}`);
  assert(refused.notice === 'workflow.max_active_runs must be at least 1' && await page.locator('#config-body .cfx-notice.error').getAttribute('role') === 'alert', `the refusal is announced: ${refused.notice}`);
  assert(await page.locator('#cfx-edit-value').inputValue() === '0', 'the refused value stays in the draft');
  assert(refused.chain.find((step) => step.layer === 'workspace').value === '8', 'nothing is persisted');

  // 6. Removing a workspace override deletes it from the workspace file and
  //    the key falls back to the global value it restated.
  await select('workflow.base_branch');
  assert((await inspector()).fallback === 'Removing it falls back to the global value, agent-main.', 'removal names the global fallback');
  await page.getByRole('button', { name: 'Remove workspace override' }).click();
  await until('the removed override', async () => (await rows()).find((entry) => entry.key === 'workflow.base_branch')?.layer === 'global');
  const removed = await inspector();
  evidenceLog.removed = { write: writes.at(-1), inspector: removed };
  assert(JSON.stringify(writes.at(-1)) === JSON.stringify({ method: 'DELETE', key: 'workflow.base_branch', scope: 'workspace' }), `the DELETE names the workspace file: ${JSON.stringify(writes.at(-1))}`);
  assert(removed.notice === 'Removed the workspace override. workflow.base_branch now resolves to agent-main from the global layer.', `the removal is confirmed: ${removed.notice}`);
  assert(removed.chain.find((step) => step.layer === 'global').cls === 'win' && !removed.remove, 'the global value now wins');
  assert((await restates.locator('strong').textContent()) === '1 workspace key restates the global value', 'the restates count follows the removal');

  // 7. Descriptions hide on request; crews group by provider with one limit
  //    bar each and flag a crew nothing references; local root is named.
  assert(await page.locator('#config-body .cfx-desc').count() > 0, 'rows show a description');
  await page.locator('#config-body .cfx-desc-toggle').click();
  await until('descriptions hidden', async () => await page.locator('#config-body .cfx-desc').count() === 0);
  const providers = await page.locator('#config-body .cfx-provider').evaluateAll((nodes) => nodes.map((node) => ({
    provider: node.dataset.provider,
    bars: node.querySelectorAll('.cfx-meter').length,
    used: node.querySelector('.cfx-limit-used')?.textContent || null,
    crews: [...node.querySelectorAll('.cfx-crew')].map((crew) => [crew.dataset.crew, crew.querySelector('.cfx-crew-use').textContent, crew.querySelector('.cfx-crew-use').title]),
  })));
  evidenceLog.providers = providers;
  assert(providers.map((card) => card.provider).join() === 'codex,claude', `one card per provider: ${JSON.stringify(providers)}`);
  assert(providers[0].bars === 1 && providers[0].used === '62%' && providers[1].bars === 0, 'one limit bar per provider that reports one');
  assert(providers[0].crews[0][1] === 'used by 3 settings and auto-tasks' && providers[0].crews[0][2].includes('auto-task delivery-code-review'), 'used by N lists its references on hover');
  assert(providers[1].crews[0][1] === 'not referenced by any setting or auto-task', 'an unreferenced crew is flagged');
  assert(providers[1].crews[1][1] === 'built-in crew, not referenced', 'an unreferenced built-in crew is not flagged');
  const localRoot = await page.locator('#config-body .cfx-path', { hasText: 'local root' }).locator('dd').textContent();
  assert(localRoot === 'same as workspace root', `local root equal to the workspace root says so: ${localRoot}`);

  // Primary controls are 44px targets.
  const small = await page.locator('#config-body .cfx-row, #config-body .cfx-seg, #config-body .cfx-layer, #config-body .cfx-btn, #config-body .cfx-radio, #config-body .cfx-input')
    .evaluateAll((nodes) => nodes.filter((node) => node.getBoundingClientRect().height < 44).map((node) => node.className));
  assert(small.length === 0, `every primary control is at least 44px tall: ${JSON.stringify(small)}`);
  // Keyboard focus is visible on a row.
  await row('workflow.merge_method').focus();
  await page.keyboard.press('Shift+Tab');
  await page.keyboard.press('Tab');
  const outline = await page.evaluate(() => getComputedStyle(document.activeElement).outlineStyle);
  assert(outline !== 'none', `a focused row shows an outline: ${outline}`);
  await page.screenshot({ path: path.join(evidence, 'config-effective-1440.png'), fullPage: true });

  // 8. Narrow viewports: the inspector stacks under the table and nothing
  //    scrolls sideways.
  for (const width of [768, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await page.waitForTimeout(150);
    const overflow = await noPageOverflow();
    assert(overflow <= 1, `no horizontal page scroll at ${width}px (by ${overflow}px)`);
    const stack = await page.evaluate(() => {
      const table = document.querySelector('#config-body .cfx-keys').getBoundingClientRect();
      const panel = document.querySelector('#config-body .cfx-inspector');
      const body = document.getElementById('config-body').getBoundingClientRect();
      const clipped = [...document.querySelectorAll('#config-body .cfx-row, #config-body .cfx-layer, #config-body .cfx-inspector, #config-body .cfx-provider, #config-body .cfx-step')]
        .filter((node) => node.getBoundingClientRect().right > body.right + 1).length;
      return { below: panel.getBoundingClientRect().top >= table.bottom - 1, position: getComputedStyle(panel).position, clipped };
    });
    assert(stack.below && stack.position === 'static' && stack.clipped === 0, `the inspector stacks below the table at ${width}px: ${JSON.stringify(stack)}`);
    await page.screenshot({ path: path.join(evidence, `config-effective-${width}.png`), fullPage: true });
  }

  assert(pageErrors.length === 0, `no page errors: ${pageErrors.join('; ')}`);
  fs.writeFileSync(path.join(evidence, 'config-effective-assertions.json'), `${JSON.stringify({
    passed: true,
    assertions: [
      'browser-config-effective-layer-counts',
      'browser-config-effective-layer-card-filter',
      'browser-config-effective-row-status-and-pool-tokens',
      'browser-config-effective-restates-global-filter',
      'browser-config-effective-inspector-overridden-key',
      'browser-config-effective-inspector-not-inherited-key',
      'browser-config-effective-global-only-and-read-only',
      'browser-config-effective-save-to-chosen-file',
      'browser-config-effective-refused-save-keeps-draft',
      'browser-config-effective-remove-workspace-override',
      'browser-config-effective-crews-by-provider',
      'browser-config-effective-paths-local-root',
      'browser-config-effective-44px-targets-and-focus',
      'browser-config-effective-phone-no-horizontal-scroll',
    ],
    viewports: ['1440', '768', '375'],
    ...evidenceLog,
  }, null, 2)}\n`);
  console.log('Dashboard Settings › Effective browser checks passed at 1440px, 768px and 375px: layer counts, restates filter, inspector chains, save, remove override, crews and paths.');
} catch (error) {
  fs.writeFileSync(path.join(evidence, 'config-effective-assertions.json'), JSON.stringify({ passed: false, error: error.message, pageErrors, ...evidenceLog }, null, 2));
  await page?.screenshot({ path: path.join(evidence, 'config-effective-failure.png'), fullPage: true }).catch(() => {});
  throw error;
} finally {
  await browser?.close();
  for (const socket of sockets) socket.destroy();
  await new Promise((resolve) => server.close(resolve));
}
