// Usage: node dashboard_config_workspace_file_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
//
// Settings › Workspace file in Chromium. The shipped app runs against a
// disposable fixture server whose `/api/config/file?scope=workspace` reports
// the workspace file's rows the way the real endpoint does: each key the file
// sets carries the fallback a removal reaches (the global value, or the
// built-in one for an execution key that skips the global file) and whether
// it restates the global or built-in value. Its PUT and DELETE key routes
// change the fixture file, and one DELETE is refused, so a bulk removal's
// per-key outcome is read back from the next payload. It checks the summary
// counts and card filter, the If removed column for a global-restating, a
// built-in-restating, a not-inherited and an unreported key, the Not set
// sidebar, inline edit with its CLI preview, Add key's candidates, selecting
// and removing the redundant keys with one refusal, and a 375px phone with
// no horizontal page scroll. Promise-held mutation responses also check host
// switches, workspace round trips, and ownership of a newer pending write.
// No Orbit store is contacted.
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
// the key has none), whether it skips the global file while a workspace file
// exists, and the write facts the server reports.
const SECTIONS = [
  { token: 'machine', title: 'Machine (machine.*)', blurb: 'This machine\'s identity.', key_prefix: 'machine' },
  { token: 'delivery', title: 'Delivery (workflow.*)', blurb: 'How work is claimed and shipped.', key_prefix: 'workflow' },
  { token: 'execution', title: 'Execution (execution.*)', blurb: 'How agents run.', key_prefix: 'execution' },
  { token: 'review', title: 'Review (review.*, operation.review_crew)', blurb: 'Review before landing.', key_prefix: 'review' },
  { token: 'housekeeping', title: 'Housekeeping', blurb: 'Sweeps and retention.', key_prefix: '' },
];
const KEYS = [
  { key: 'machine.id', section: 'machine', type: 'string', readOnly: 'a machine identity is generated once by `orbit init` and never reused', globalOnly: true, description: 'Generated machine identity.' },
  { key: 'machine.name', section: 'machine', type: 'string', globalOnly: true, description: 'Display name for this machine.' },
  { key: 'workflow.base_branch', section: 'delivery', type: 'string', builtIn: 'main', description: 'Branch task PRs target.' },
  { key: 'workflow.default_crew', section: 'delivery', type: 'string', builtIn: 'astra', description: 'Crew used when nothing else picks one.' },
  { key: 'workflow.max_active_runs', section: 'delivery', type: 'integer', builtIn: 4, description: 'Most runs active at once.' },
  { key: 'workflow.distributed_completion', section: 'delivery', type: 'string', options: ['review', 'done'], builtIn: 'review', description: 'Where an accepted distributed handoff lands.' },
  { key: 'workflow.auto_merge', section: 'delivery', type: 'bool', builtIn: false, description: 'Merge a PR once checks pass.' },
  { key: 'workflow.low_complexity_crews', section: 'delivery', type: 'array<string>', builtIn: [], description: 'Weighted crew pool for low-complexity tasks.' },
  { key: 'workflow.release_branch', section: 'delivery', type: 'string', description: 'Branch releases are cut from.' },
  { key: 'execution.codex.sandbox', section: 'execution', type: 'string', options: ['read-only', 'workspace-write', 'danger-full-access'], builtIn: 'workspace-write', replaceOnly: true, description: 'Codex sandbox mode.' },
  // The payload omits this row's fallback, as it does when the two layers
  // do not load together; the view must say so rather than guess.
  { key: 'execution.env.pass', section: 'execution', type: 'array<string>', builtIn: [], replaceOnly: true, unreported: true, description: 'Environment variables passed to agents.' },
  { key: 'review.before_pr', section: 'review', type: 'bool', builtIn: false, description: 'Review before the PR opens.' },
  { key: 'review.minutes', section: 'review', type: 'integer', builtIn: 30, description: 'Minutes a review may take.' },
  { key: 'operation.review_crew', section: 'review', type: 'array<string>', description: 'Crew for automatic review. Before-PR and before-landing review refuse to start without it.' },
  { key: 'security_alert_sweep.min_severity', section: 'housekeeping', type: 'string', options: ['low', 'moderate', 'high', 'critical'], builtIn: 'moderate', description: 'Minimum severity the alert sweep files.' },
];
const files = {
  global: {
    'machine.id': 'hm_fixture', 'machine.name': 'alpha', 'workflow.base_branch': 'agent-main', 'workflow.default_crew': 'nova',
    'workflow.max_active_runs': 6, 'execution.codex.sandbox': 'danger-full-access', 'execution.env.pass': ['GH_TOKEN'],
  },
  workspace: {
    'workflow.base_branch': 'agent-main', 'workflow.default_crew': 'nova', 'workflow.max_active_runs': 8,
    'workflow.distributed_completion': 'done', 'workflow.auto_merge': false, 'execution.codex.sandbox': 'danger-full-access',
    'execution.env.pass': ['GH_TOKEN', 'HOME'], 'review.before_pr': false, 'review.minutes': 120, 'operation.review_crew': ['sol'],
  },
};
// Removing this key is refused, as the server refuses a removal that would
// leave the file invalid.
const REFUSED_UNSET = { 'workflow.default_crew': "workflow.default_crew: removing it leaves crew 'astra', which is not defined in [crews.*]" };
const same = (left, right) => JSON.stringify(left) === JSON.stringify(right);
const has = (file, key) => Object.prototype.hasOwnProperty.call(files[file], key);

// One row of the workspace file view: a key the file sets, with its fallback
// and equality flags, or the built-in value a key the file omits would take.
function fileRow(def) {
  const builtIn = def.builtIn === undefined ? { state: 'unset', value: null } : { state: 'default', value: def.builtIn };
  const row = {
    key: def.key, label: def.key.split('.').at(-1), value_type: def.type, options: def.options || [], section: def.section,
    description: def.description, settable: !def.readOnly, read_only_reason: def.readOnly || null, global_only: Boolean(def.globalOnly),
    built_in: builtIn, shadowed_by: [], fallback: null, equals: null, reach: null,
  };
  if (!has('workspace', def.key)) {
    return Object.assign(row, { value: builtIn.value, state: builtIn.state, source: { layer: 'built-in', path: null } });
  }
  const value = files.workspace[def.key];
  const globalValue = has('global', def.key) ? files.global[def.key] : undefined;
  Object.assign(row, { value, state: 'set', source: { layer: 'workspace', path: WORKSPACE_PATH } });
  row.equals = { global: globalValue === undefined ? null : same(value, globalValue), built_in: builtIn.state === 'default' && same(value, builtIn.value) };
  if (!def.unreported) {
    row.fallback = globalValue !== undefined && !def.replaceOnly
      ? { layer: 'global', value: globalValue, state: 'set' }
      : { layer: 'built-in', value: builtIn.value, state: builtIn.state };
  }
  return row;
}

function workspaceFile() {
  const sections = SECTIONS.map((section) => {
    const keys = KEYS.filter((def) => def.section === section.token).map(fileRow);
    const count = (state) => keys.filter((row) => row.state === state).length;
    return { ...section, kind: 'keys', not_inherited: 0, counts: { set: count('set'), default: count('default'), unset: count('unset'), total: keys.length }, keys };
  });
  sections.push({ token: 'crews', title: 'Crews', blurb: 'Named crews.', key_prefix: 'crews', kind: 'crews', counts: { set: 0, default: 0, unset: 0, total: 0 }, keys: [] });
  return {
    scope: 'workspace',
    layers: {
      built_in: { label: 'built-in' }, global: { path: GLOBAL_PATH, exists: true }, workspace: { path: WORKSPACE_PATH, exists: true },
      execution_not_inherited: true, not_inherited_keys: [],
    },
    workspace_binding: null,
    sections,
    crews: [], crews_enumerated: false, crew_tables: [],
    paths: [{ label: 'config file', value: WORKSPACE_PATH }],
    crew_fields: [], write_scope_default: 'workspace', workspace_file_exists: true,
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
    if (REFUSED_UNSET[key]) return json({ error: REFUSED_UNSET[key] }, 400);
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
  if (rest === 'config/file' && url.searchParams.get('scope') === 'workspace') return json(workspaceFile());
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
  if (url.pathname === '/workspace-file-race') {
    res.setHeader('content-type', 'text/html');
    res.end('<!doctype html><html><body><div id="config-body"></div></body></html>');
    return;
  }
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

const cards = () => page.locator('#config-body .cwf-card').evaluateAll((nodes) => Object.fromEntries(nodes.map((node) => [
  node.dataset.category, { n: node.querySelector('.cwf-card-n').textContent, pressed: node.getAttribute('aria-pressed') },
])));
const rows = () => page.locator('#config-body .cwf-row').evaluateAll((nodes) => nodes.map((node) => ({
  key: node.dataset.key,
  table: node.closest('.cwf-group')?.dataset.table,
  category: node.dataset.category,
  value: node.querySelector('.cwf-value').textContent,
  layer: node.querySelector('.cwf-fallback').dataset.layer,
  fallback: node.querySelector('.cwf-fallback').textContent,
  note: node.querySelector('.cwf-note')?.textContent || '',
  error: node.querySelector('.cwf-row-error')?.textContent || '',
  checked: node.querySelector('.cwf-check input').checked,
})));
const rowOf = async (key) => (await rows()).find((entry) => entry.key === key);
const row = (key) => page.locator(`#config-body .cwf-row[data-key="${key}"]`);
const noPageOverflow = () => page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);

// Exercise the shipped modules in Chromium with explicit response barriers.
// This small page has no dashboard poller: the only requests are mutations
// dispatched by the real controls, and a frame drains settled promise callbacks.
async function checkVisitRaces(browser, payload) {
  const racePage = await browser.newPage();
  try {
    await racePage.goto(`${origin}/workspace-file-race`);
    return await racePage.evaluate(async (payload) => {
      const common = await import('/static/js/common.js');
      const view = await import('/static/js/config-workspace-file.js');
      const unsubscribe = common.onWorkspaceChange(view.resetWorkspaceFile);
      const body = document.getElementById('config-body');
      const assert = (condition, message) => { if (!condition) throw new Error(message); };
      const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
      const row = (key) => body.querySelector(`.cwf-row[data-key="${key}"]`);
      const button = (root, label) => [...root.querySelectorAll('button')].find((node) => node.textContent === label);
      const cases = [];
      let reloads = 0;
      const context = {
        canEdit: true,
        render: () => { body.replaceChildren(); view.renderWorkspaceFile(body, payload, context); },
        reload: async () => { reloads += 1; context.render(); },
      };
      const save = (value) => {
        button(row('review.minutes'), 'Edit').click();
        const input = body.querySelector('#cwf-edit-value');
        input.value = value;
        input.dispatchEvent(new Event('input'));
        input.closest('form').dispatchEvent(new Event('submit', { cancelable: true }));
      };
      const originalFetch = window.fetch;
      try {
        for (const boundary of ['host-switch', 'workspace-round-trip']) {
          for (const operation of ['remove', 'save', 'add']) {
            for (const refused of [false, true]) {
              common.setHost(boundary === 'host-switch' ? null : 'beta');
              common.setWorkspace('ws_same');
              view.resetWorkspaceFile();
              context.render();
              reloads = 0;
              const requests = [];
              window.fetch = (target, options) => new Promise((resolve) => {
                requests.push({ target, method: options.method, body: options.body, release: (refused = false) => resolve({
                  ok: !refused, status: refused ? 400 : 200,
                  text: async () => JSON.stringify(refused ? { error: 'fixture write refused', code: 'invalid_value' } : { ok: true }),
                }) });
              });
              const prefix = boundary === 'host-switch' ? '/api/' : '/api/on/beta/';
              if (operation === 'remove') {
                for (const key of ['workflow.base_branch', 'workflow.default_crew']) row(key).querySelector('input[type="checkbox"]').click();
                button(body.querySelector('.cwf-bulk'), 'Remove from file').click();
              } else if (operation === 'save') {
                save('80');
              } else {
                button(body.querySelector('.cwf-head'), '+ Add key').click();
                button(body.querySelector('.cwf-add'), 'Add to file').click();
              }
              const label = `${boundary}/${operation}/${refused ? 'refused' : 'success'}`;
              assert(requests.length === 1, `${label}: the first mutation is held`);
              const expectedKey = operation === 'remove' ? 'workflow.base_branch' : operation === 'save' ? 'review.minutes' : 'workflow.low_complexity_crews';
              const expectedTarget = `${prefix}config/keys/${expectedKey}${operation === 'remove' ? '?scope=workspace&workspace=ws_same' : '?workspace=ws_same'}`;
              assert(requests[0].target === expectedTarget, `${label}: mutation targets the starting host/workspace`);
              if (boundary === 'host-switch') common.setHost('beta');
              else { common.setWorkspace('ws_other'); common.setWorkspace('ws_same'); }
              context.render();
              save('45');
              assert(requests.length === 2 && requests[1].target === '/api/on/beta/config/keys/review.minutes?workspace=ws_same', `${label}: a newer visit owns its Save`);
              const before = body.innerHTML;
              requests[0].release(refused);
              await frame();
              assert(requests.length === 2, `${label}: stale removal sends no further DELETE`);
              assert(reloads === 0 && body.innerHTML === before, `${label}: stale settlement leaves the new visit untouched`);
              // Re-render exposes an invisible pending-state change, and a
              // programmatic submit checks admission even with a disabled button.
              context.render();
              const input = body.querySelector('#cwf-edit-value');
              assert(input.disabled && input.value === '45', `${label}: the newer draft remains pending`);
              input.closest('form').dispatchEvent(new Event('submit', { cancelable: true }));
              assert(requests.length === 2, `${label}: an old result cannot unlock a duplicate write`);
              requests[1].release();
              await frame();
              assert(reloads === 1 && !body.querySelector('.cwf-editor') && !button(row('review.minutes'), 'Edit').disabled,
                `${label}: the newer Save settles normally`);
              cases.push({ label, requests: requests.map(({ release, ...request }) => request), reloads });
            }
          }
        }
        const visit = common.captureWorkspaceVisit();
        common.setHostWrites({ authorized: false, reason: 'fixture read-only host' });
        const denied = await common.requestJson('/api/config/keys/review.minutes', 'PUT', { value: 45, scope: 'workspace' }, visit).catch((error) => error.code);
        assert(denied === 'authorization_denied', 'visit-bound writes retain remote-host authorization checks');
        common.setHost(null);
        const expired = await common.requestJson('/api/config/keys/review.minutes', 'PUT', { value: 45, scope: 'workspace' }, visit).catch((error) => error.code);
        assert(expired === 'stale_workspace_visit', 'an expired bound request is refused before fetch');
      } finally {
        window.fetch = originalFetch;
        unsubscribe();
      }
      return cases;
    }, payload);
  } finally {
    await racePage.close();
  }
}

const racePayload = workspaceFile();

try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
  page = await context.newPage();
  page.on('pageerror', (error) => pageErrors.push(error.message));

  // 1. The header names the file; the summary cards count each key by what
  //    removing it would do, and the tidy callout counts the redundant ones.
  await page.goto(`${origin}/?workspace=ws_orbit#config/workspace-file`);
  await page.locator('#config-body .cwf-row').first().waitFor({ state: 'visible' });
  const head = await page.locator('#config-body .cwf-head').textContent();
  assert(head.includes(WORKSPACE_PATH) && head.includes('10 keys · 5 TOML tables'), `the header names the file and its key count: ${head}`);
  const counts = await cards();
  evidenceLog.counts = counts;
  assert(counts.own.n === '6' && counts.global.n === '2' && counts.default.n === '2', `own / restates global / restates default: ${JSON.stringify(counts)}`);
  const segments = await page.locator('#config-body .cfx-seg').evaluateAll((nodes) => nodes.map((node) => [node.dataset.filter, node.querySelector('.cfx-seg-n').textContent]));
  assert(JSON.stringify(segments) === JSON.stringify([['all', '10'], ['own', '6'], ['global', '2'], ['default', '2']]), `the segments carry the same counts: ${JSON.stringify(segments)}`);
  const tidy = await page.locator('#config-body .cwf-tidy strong').textContent();
  assert(tidy === '4 keys restate a value the workspace would get anyway', `the tidy callout counts the redundant keys: ${tidy}`);

  // 2. Keys group by TOML table, in file order.
  const initial = await rows();
  evidenceLog.rows = initial;
  const tables = [...new Set(initial.map((entry) => entry.table))];
  assert(JSON.stringify(tables) === JSON.stringify(['workflow', 'execution.codex', 'execution.env', 'review', 'operation']), `keys group by TOML table: ${JSON.stringify(tables)}`);
  assert(await page.locator('#config-body .cwf-group[data-table="workflow"] .cwf-name').first().textContent() === 'base_branch', 'a row names its key within the table');

  // 3. If removed: a global-restating key falls to the same global value, a
  //    built-in-restating key to the same default, a not-inherited execution
  //    key to the built-in default despite the global value, a key with no
  //    default to unset, and an unreported fallback says it may change.
  const byKey = Object.fromEntries(initial.map((entry) => [entry.key, entry]));
  const base = byKey['workflow.base_branch'];
  assert(base.category === 'global' && base.layer === 'global' && base.fallback === 'globalagent-main· no change', `a global-restating key: ${JSON.stringify(base)}`);
  const autoMerge = byKey['workflow.auto_merge'];
  assert(autoMerge.category === 'default' && autoMerge.layer === 'built-in' && autoMerge.fallback === 'built-infalse· no change', `a built-in-restating key: ${JSON.stringify(autoMerge)}`);
  const sandbox = byKey['execution.codex.sandbox'];
  assert(sandbox.category === 'own' && sandbox.layer === 'built-in' && sandbox.fallback === 'built-inworkspace-write· value changes'
    && sandbox.note.includes('The global file sets the same value, but it is not inherited'), `a not-inherited execution key: ${JSON.stringify(sandbox)}`);
  const runs = byKey['workflow.max_active_runs'];
  assert(runs.category === 'own' && runs.fallback === 'global6· value changes', `an overriding key: ${JSON.stringify(runs)}`);
  const crew = byKey['operation.review_crew'];
  assert(crew.fallback === 'built-inunset· value changes' && crew.note.includes('ships no default') && crew.note.includes('refuse to start without it'), `a key with no built-in default: ${JSON.stringify(crew)}`);
  const env = byKey['execution.env.pass'];
  assert(env.category === 'own' && env.layer === 'not-reported' && env.fallback === 'not reported· may change', `an unreported fallback: ${JSON.stringify(env)}`);

  // A card filters the table to its category and clears on a second press.
  await page.locator('#config-body .cwf-card[data-category="global"]').click();
  await until('the global filter', async () => (await rows()).length === 2);
  assert((await rows()).every((entry) => entry.category === 'global') && (await cards()).global.pressed === 'true', 'the Restates global card filters to its keys');
  await page.locator('#config-body .cwf-card[data-category="global"]').click();
  await until('every key again', async () => (await rows()).length === 10);

  // 4. The sidebar lists the sections nothing here sets and links to
  //    Effective; no empty-section row or crew claim remains.
  const notSet = await page.locator('#config-body .cwf-notset li').evaluateAll((nodes) => nodes.map((node) => [node.dataset.section, node.children[1]?.textContent]));
  evidenceLog.notSet = notSet;
  assert(JSON.stringify(notSet) === JSON.stringify([['machine', 'global file only'], ['housekeeping', '0 of 1 key'], ['crews', 'none defined here']]), `Not set in this file: ${JSON.stringify(notSet)}`);
  const link = page.locator('#config-body .cwf-effective-link');
  assert(await link.getAttribute('href') === '#config/effective' && (await link.textContent()) === `See all ${KEYS.length} keys in Effective →`, 'the sidebar links to Effective');
  const bodyText = await page.locator('#config-body').textContent();
  assert(!bodyText.includes('No key in this section is set') && !bodyText.includes('No crew is defined'), 'no empty-section rows or misleading crew message');
  assert(await page.locator('#config-controls').isHidden(), 'the view carries its own actions');

  // 5. Inline edit shows the `orbit config set` preview and saves to the
  //    workspace file.
  await row('review.minutes').getByRole('button', { name: 'Edit' }).click();
  await page.locator('#cwf-edit-value').fill('90');
  const cli = await row('review.minutes').locator('.cwf-cli').textContent();
  assert(cli === 'orbit config set review.minutes 90', `the CLI preview follows the draft: ${cli}`);
  await row('review.minutes').getByRole('button', { name: 'Save', exact: true }).click();
  await until('the saved value', async () => (await rowOf('review.minutes'))?.value === '90');
  assert(JSON.stringify(writes.at(-1)) === JSON.stringify({ method: 'PUT', key: 'review.minutes', value: 90, scope: 'workspace' }), `the PUT writes the workspace file: ${JSON.stringify(writes.at(-1))}`);
  assert((await page.locator('#config-body .cwf-status').textContent()) === 'Saved review.minutes to the workspace file.', 'the save is confirmed');
  // A refused save stays in the editor with the server's reason.
  await row('review.minutes').getByRole('button', { name: 'Edit' }).click();
  await page.locator('#cwf-edit-value').fill('0');
  await row('review.minutes').getByRole('button', { name: 'Save', exact: true }).click();
  await row('review.minutes').locator('.cwf-editor .cfx-notice.error').waitFor({ state: 'visible' });
  assert((await row('review.minutes').locator('.cwf-editor .cfx-notice.error').textContent()) === 'review.minutes must be at least 1', 'a refused save keeps the editor open with its reason');
  await page.keyboard.press('Escape');
  await until('the editor closed', async () => await page.locator('#config-body .cwf-editor').count() === 0);

  // 6. Add key offers only registered keys this file does not set and that
  //    the workspace file may carry, then writes the chosen one.
  await page.locator('#config-body .cwf-head').getByRole('button', { name: '+ Add key' }).click();
  const offered = await page.locator('#cwf-add-key option').evaluateAll((nodes) => nodes.map((node) => node.value));
  evidenceLog.addKeyOptions = offered;
  assert(JSON.stringify(offered) === JSON.stringify(['workflow.low_complexity_crews', 'workflow.release_branch', 'security_alert_sweep.min_severity']), `Add key offers the unset, workspace-settable keys: ${JSON.stringify(offered)}`);
  await page.locator('#cwf-add-key').selectOption('workflow.release_branch');
  await page.locator('#cwf-add-value').fill('release');
  assert((await page.locator('#config-body .cwf-add .cwf-cli').textContent()) === 'orbit config set workflow.release_branch release', 'Add key previews its CLI write');
  await page.getByRole('button', { name: 'Add to file' }).click();
  await until('the added key', async () => (await rowOf('workflow.release_branch'))?.value === 'release');
  assert(JSON.stringify(writes.at(-1)) === JSON.stringify({ method: 'PUT', key: 'workflow.release_branch', value: 'release', scope: 'workspace' }), `the added key is written: ${JSON.stringify(writes.at(-1))}`);
  assert((await rowOf('workflow.release_branch')).fallback === 'built-inunset· value changes', 'the added key states its fallback');

  // 7. Select the redundant keys and remove them: one DELETE per key, in
  //    order; the refused one stays checked with its reason on its row.
  await page.locator('#config-body .cwf-tidy').getByRole('button', { name: 'Select the 4' }).click();
  const selected = (await rows()).filter((entry) => entry.checked).map((entry) => entry.key);
  assert(JSON.stringify(selected) === JSON.stringify(['workflow.base_branch', 'workflow.default_crew', 'workflow.auto_merge', 'review.before_pr']), `Select pre-checks the restating keys: ${JSON.stringify(selected)}`);
  const bulk = await page.locator('#config-body .cwf-bulk').textContent();
  assert(bulk.includes('4 selected') && bulk.includes('falls back to the value in the If removed column'), `the bulk bar: ${bulk}`);
  const before = writes.length;
  await page.locator('#config-body .cwf-bulk').getByRole('button', { name: 'Remove from file' }).click();
  await until('the bulk outcome', async () => await page.locator('#config-body .cwf-outcomes li').count() === 4);
  const deletes = writes.slice(before);
  assert(JSON.stringify(deletes) === JSON.stringify(selected.map((key) => ({ method: 'DELETE', key, scope: 'workspace' }))), `one unset per key, in order: ${JSON.stringify(deletes)}`);
  const outcomes = await page.locator('#config-body .cwf-outcomes li').evaluateAll((nodes) => nodes.map((node) => [node.dataset.outcome, node.className, node.textContent]));
  const status = await page.locator('#config-body .cwf-status .cfx-notice').textContent();
  evidenceLog.bulk = { deletes, outcomes, status };
  assert(status === 'Removed 3 keys from the workspace file. 1 key was not removed; the reason is on its row.', `the bulk summary: ${status}`);
  assert(outcomes[0][2] === 'workflow.base_branch now agent-main from the global file' && outcomes[2][2] === 'workflow.auto_merge now false from the built-in default'
    && outcomes[1][1] === 'error' && outcomes[1][2].startsWith('workflow.default_crew refused: '), `each key's outcome: ${JSON.stringify(outcomes)}`);
  const after = await rows();
  assert(!after.some((entry) => ['workflow.base_branch', 'workflow.auto_merge', 'review.before_pr'].includes(entry.key)), 'removed keys leave the file');
  const refused = after.find((entry) => entry.key === 'workflow.default_crew');
  assert(refused && refused.checked && refused.error === `Not removed: ${REFUSED_UNSET['workflow.default_crew']}`, `the refusal is on its row: ${JSON.stringify(refused)}`);
  assert(await row('workflow.default_crew').locator('.cwf-row-error').getAttribute('role') === 'alert', 'the refusal is announced');
  const recounted = await cards();
  assert(recounted.global.n === '1' && recounted.default.n === '0', `counts follow the removal: ${JSON.stringify(recounted)}`);

  // Primary controls are 44px targets.
  const small = await page.locator('#config-body .cwf-card, #config-body .cwf-check, #config-body .cfx-btn, #config-body .cfx-seg, #config-body .cfx-input')
    .evaluateAll((nodes) => nodes.filter((node) => node.getBoundingClientRect().height < 44).map((node) => node.className));
  assert(small.length === 0, `every primary control is at least 44px tall: ${JSON.stringify(small)}`);
  await page.screenshot({ path: path.join(evidence, 'config-workspace-file-1440.png'), fullPage: true });

  // 8. A phone: rows stack and nothing scrolls sideways.
  for (const width of [768, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await page.waitForTimeout(150);
    const overflow = await noPageOverflow();
    assert(overflow <= 1, `no horizontal page scroll at ${width}px (by ${overflow}px)`);
    const clipped = await page.evaluate(() => {
      const body = document.getElementById('config-body').getBoundingClientRect();
      return [...document.querySelectorAll('#config-body .cwf-row, #config-body .cwf-card, #config-body .cwf-side, #config-body .cwf-bulk')]
        .filter((node) => node.getBoundingClientRect().right > body.right + 1).map((node) => node.className);
    });
    assert(clipped.length === 0, `nothing overflows the panel at ${width}px: ${JSON.stringify(clipped)}`);
    await page.screenshot({ path: path.join(evidence, `config-workspace-file-${width}.png`), fullPage: true });
  }

  evidenceLog.visitRaces = await checkVisitRaces(browser, racePayload);
  assert(pageErrors.length === 0, `no page errors: ${pageErrors.join('; ')}`);
  fs.writeFileSync(path.join(evidence, 'config-workspace-file-assertions.json'), `${JSON.stringify({
    passed: true,
    assertions: [
      'browser-config-workspace-file-summary-counts',
      'browser-config-workspace-file-grouped-by-toml-table',
      'browser-config-workspace-file-if-removed-column',
      'browser-config-workspace-file-card-filter',
      'browser-config-workspace-file-not-set-sidebar',
      'browser-config-workspace-file-inline-edit-and-refused-save',
      'browser-config-workspace-file-add-key-candidates',
      'browser-config-workspace-file-bulk-remove-per-key-outcome',
      'browser-config-workspace-file-refused-unset-on-row',
      'browser-config-workspace-file-phone-no-horizontal-scroll',
      'browser-config-workspace-file-held-remove-host-switch',
      'browser-config-workspace-file-held-remove-workspace-round-trip',
      'browser-config-workspace-file-stale-save-and-add-settlement',
      'browser-config-workspace-file-pending-operation-ownership',
      'browser-config-workspace-file-visit-bound-write-admission',
    ],
    viewports: ['1440', '768', '375'],
    ...evidenceLog,
  }, null, 2)}\n`);
  console.log('Dashboard Settings › Workspace file browser checks passed at 1440px, 768px and 375px, including 12 held-response visit races and visit-bound write admission.');
} catch (error) {
  fs.writeFileSync(path.join(evidence, 'config-workspace-file-assertions.json'), JSON.stringify({ passed: false, error: error.message, pageErrors, ...evidenceLog }, null, 2));
  await page?.screenshot({ path: path.join(evidence, 'config-workspace-file-failure.png'), fullPage: true }).catch(() => {});
  throw error;
} finally {
  await browser?.close();
  for (const socket of sockets) socket.destroy();
  await new Promise((resolve) => server.close(resolve));
}
