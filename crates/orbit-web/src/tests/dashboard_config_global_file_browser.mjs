// Usage: node dashboard_config_global_file_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
//
// Settings › Global file in Chromium. The shipped app runs against a
// disposable fixture server whose `/api/config/file?scope=global` reports each
// key the global file sets with its reach into the bound workspace, the way
// the real endpoint does: in effect, overridden by the workspace file (with
// its value and whether it equals the global one), not inherited (an
// execution key while a workspace file exists), or null when the layers do not
// load together. `?scope=workspace` serves the workspace file, which
// overrides one key with the same value and one with a different value, and
// sets its own value for one execution key. PUT and DELETE key routes change
// the fixture files, and one PUT is refused. It checks the machine card and a
// rename, the three reach counts and the card filter, each row's reach cell,
// the editor's reach warning for an in-effect, an overridden and a
// not-inherited key, a refused save, a removal, Add key, the sidebar's crew
// tables (never "No crew is defined"), 44px targets, and a 375px phone with
// no horizontal page scroll. Held responses exercise Save, Add, Rename and
// Remove across host switches and workspace round trips, including refused
// drafts and pending state through reload. No Orbit store is contacted.
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

// The registry: section, type, choices, the built-in default (absent when the
// key has none), whether a workspace file keeps it from the global file
// (`replaceOnly`), and the write facts the server reports.
const SECTIONS = [
  { token: 'machine', title: 'Machine (machine.*)', blurb: 'This machine\'s identity.', key_prefix: 'machine' },
  { token: 'delivery', title: 'Delivery (workflow.*)', blurb: 'How work is claimed and shipped.', key_prefix: 'workflow' },
  { token: 'execution', title: 'Execution (execution.*)', blurb: 'How agents run.', key_prefix: 'execution' },
  { token: 'review', title: 'Review (review.*)', blurb: 'Review before landing.', key_prefix: 'review' },
  { token: 'housekeeping', title: 'Housekeeping', blurb: 'Sweeps and retention.', key_prefix: '' },
];
const KEYS = [
  { key: 'machine.id', section: 'machine', type: 'string', readOnly: 'a machine identity is generated once by `orbit init` and never reused', globalOnly: true, description: 'Generated machine identity.' },
  { key: 'machine.name', section: 'machine', type: 'string', globalOnly: true, description: 'Display name for this machine.' },
  { key: 'machine.task_prefix', section: 'machine', type: 'string', readOnly: "the task-id namespace is fixed for the life of this machine's task store", globalOnly: true, description: 'Task ID namespace.' },
  { key: 'workflow.base_branch', section: 'delivery', type: 'string', builtIn: 'main', description: 'Branch task PRs target.' },
  { key: 'workflow.default_crew', section: 'delivery', type: 'string', builtIn: 'astra', description: 'Crew used when nothing else picks one.' },
  { key: 'workflow.max_active_runs', section: 'delivery', type: 'integer', builtIn: 4, description: 'Most runs active at once.' },
  { key: 'workflow.auto_merge', section: 'delivery', type: 'bool', builtIn: false, description: 'Merge a PR once checks pass.' },
  { key: 'workflow.distributed_completion', section: 'delivery', type: 'string', options: ['review', 'done'], builtIn: 'review', description: 'Where an accepted distributed handoff lands.' },
  { key: 'workflow.release_branch', section: 'delivery', type: 'string', description: 'Branch releases are cut from.' },
  { key: 'execution.codex.sandbox', section: 'execution', type: 'string', options: ['read-only', 'workspace-write', 'danger-full-access'], builtIn: 'workspace-write', replaceOnly: true, description: 'Codex sandbox mode.' },
  { key: 'execution.env.pass', section: 'execution', type: 'array<string>', builtIn: [], replaceOnly: true, description: 'Environment variables passed to agents.' },
  // The payload omits this row's reach, as it does when the two layers do
  // not load together; the view must say so rather than guess.
  { key: 'review.minutes', section: 'review', type: 'integer', builtIn: 30, unreported: true, description: 'Minutes a review may take.' },
  { key: 'security_alert_sweep.min_severity', section: 'housekeeping', type: 'string', options: ['low', 'moderate', 'high', 'critical'], builtIn: 'moderate', description: 'Minimum severity the alert sweep files.' },
];
const files = {
  global: {
    'machine.id': 'hm_fixture', 'machine.name': 'alpha', 'machine.task_prefix': 'ALP',
    'workflow.base_branch': 'agent-main', 'workflow.default_crew': 'nova', 'workflow.max_active_runs': 6, 'workflow.auto_merge': false,
    'execution.codex.sandbox': 'danger-full-access', 'execution.env.pass': ['GH_TOKEN'], 'review.minutes': 45,
  },
  // One override with the same value, one with a different value, one key
  // only the workspace file sets, and its own value for an execution key.
  workspace: {
    'workflow.base_branch': 'agent-main', 'workflow.default_crew': 'sol', 'workflow.distributed_completion': 'done',
    'execution.codex.sandbox': 'workspace-write',
  },
};
const CREW_TABLES = ['astra', 'nova'];
const same = (left, right) => JSON.stringify(left) === JSON.stringify(right);
const has = (file, key) => Object.prototype.hasOwnProperty.call(files[file], key);

function baseRow(def) {
  return {
    key: def.key, label: def.key.split('.').at(-1), value_type: def.type, options: def.options || [], section: def.section,
    description: def.description, settable: !def.readOnly, read_only_reason: def.readOnly || null, global_only: Boolean(def.globalOnly),
    built_in: def.builtIn === undefined ? { state: 'unset', value: null } : { state: 'default', value: def.builtIn },
    shadowed_by: [], fallback: null, equals: null, reach: null,
  };
}

// The global file's reach rule, as orbit-config layers it for the bound
// workspace: a workspace file that sets the key wins; a replace-only key is
// not inherited while a workspace file exists; otherwise the global value is
// in effect.
function reach(def, value) {
  if (def.unreported) return null;
  if (def.replaceOnly) return { state: 'not-inherited' };
  if (has('workspace', def.key)) return { state: 'overridden', workspace_value: files.workspace[def.key], equals_global: same(files.workspace[def.key], value) };
  return { state: 'in-effect' };
}

function fileRow(scope, def) {
  const row = baseRow(def);
  if (!has(scope, def.key)) {
    return Object.assign(row, { value: row.built_in.value, state: row.built_in.state, source: { layer: 'built-in', path: null } });
  }
  const value = files[scope][def.key];
  Object.assign(row, { value, state: 'set', source: { layer: scope, path: scope === 'global' ? GLOBAL_PATH : WORKSPACE_PATH } });
  row.equals = { global: null, built_in: row.built_in.state === 'default' && same(value, row.built_in.value) };
  if (scope === 'global') row.reach = reach(def, value);
  return row;
}

function fileView(scope) {
  const sections = SECTIONS.map((section) => {
    const keys = KEYS.filter((def) => def.section === section.token).map((def) => fileRow(scope, def));
    const count = (state) => keys.filter((row) => row.state === state).length;
    return { ...section, kind: 'keys', not_inherited: 0, counts: { set: count('set'), default: count('default'), unset: count('unset'), total: keys.length }, keys };
  });
  sections.push({ token: 'crews', title: 'Crews', blurb: 'Named crews.', key_prefix: 'crews', kind: 'crews', counts: { set: 0, default: 0, unset: 0, total: 0 }, keys: [] });
  return {
    scope,
    file: { path: scope === 'global' ? GLOBAL_PATH : WORKSPACE_PATH, exists: true },
    layers: {
      built_in: { label: 'built-in' }, global: { path: GLOBAL_PATH, exists: true }, workspace: { path: WORKSPACE_PATH, exists: true },
      execution_not_inherited: true, not_inherited_keys: [],
    },
    workspace_binding: null,
    sections,
    crews: [], crews_enumerated: false, crew_tables: scope === 'global' ? CREW_TABLES : [],
    paths: [{ label: 'config file', value: scope === 'global' ? GLOBAL_PATH : WORKSPACE_PATH }],
    crew_fields: [], write_scope_default: scope, workspace_file_exists: true,
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
    if (def.type === 'integer' && body.value < 1) return json({ error: `${key} must be at least 1` }, 400);
    files[body.scope][key] = body.value;
    return json({ ok: true });
  });
}

function api(rest, url, req, res) {
  const json = (body, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
  if (rest === 'config/file') return json(fileView(url.searchParams.get('scope')));
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
  if (url.pathname === '/global-file-race') {
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

const cards = () => page.locator('#config-body .cgf-card').evaluateAll((nodes) => Object.fromEntries(nodes.map((node) => [
  node.dataset.reach, { n: node.querySelector('.cwf-card-n').textContent, hint: node.querySelector('.cwf-card-hint').textContent, pressed: node.getAttribute('aria-pressed') },
])));
const rows = () => page.locator('#config-body .cgf-row').evaluateAll((nodes) => nodes.map((node) => ({
  key: node.dataset.key,
  table: node.closest('.cwf-group')?.dataset.table,
  reach: node.dataset.reach,
  value: node.querySelector('.cwf-value').textContent,
  cell: node.querySelector('.cgf-reach-cell').textContent,
  note: node.querySelector('.cgf-note')?.textContent || '',
})));
const rowOf = async (key) => (await rows()).find((entry) => entry.key === key);
const row = (key) => page.locator(`#config-body .cgf-row[data-key="${key}"]`);
const warning = (key) => row(key).locator('.cgf-reach-warn').evaluate((node) => ({ kind: node.dataset.reach, text: node.textContent }));
const noPageOverflow = () => page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);

// The shipped modules run without a poller. Explicit fetch and reload barriers
// choose the interleaving; a frame drains the resulting promise callbacks.
async function checkVisitRaces(browser, payload) {
  const racePage = await browser.newPage();
  const errors = [];
  racePage.on('pageerror', (error) => errors.push(error.message));
  try {
    await racePage.goto(`${origin}/global-file-race`);
    const cases = await racePage.evaluate(async (payload) => {
      const common = await import('/static/js/common.js');
      const view = await import('/static/js/config-global-file.js');
      const unsubscribe = common.onWorkspaceChange(view.resetGlobalFile);
      const body = document.getElementById('config-body');
      const assert = (condition, message) => { if (!condition) throw new Error(message); };
      const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
      const row = (key) => body.querySelector(`.cgf-row[data-key="${key}"]`);
      const button = (root, label) => [...root.querySelectorAll('button')].find((node) => node.textContent === label);
      const cases = [];
      let reloads = 0;
      let releaseReload;
      const context = {
        canEdit: true,
        render: () => { body.replaceChildren(); view.renderGlobalFile(body, payload, context); },
        reload: async () => {
          const visit = common.captureWorkspaceVisit();
          reloads += 1;
          await new Promise((resolve) => { releaseReload = resolve; });
          if (visit.isCurrent()) context.render();
        },
      };
      const start = (operation, value) => {
        let input;
        if (operation === 'add') {
          button(body.querySelector('.cwf-head'), '+ Add key').click();
          const picker = body.querySelector('#cgf-add-key');
          picker.value = 'workflow.release_branch';
          picker.dispatchEvent(new Event('change'));
          input = body.querySelector('#cgf-add-value');
        } else if (operation === 'rename') {
          button(body.querySelector('.cgf-id[data-key="machine.name"]'), 'Rename').click();
          input = body.querySelector('#cgf-rename-value');
        } else {
          button(row('review.minutes'), 'Edit').click();
          input = body.querySelector('#cgf-edit-value');
        }
        input.value = value;
        input.dispatchEvent(new Event('input'));
        const form = input.closest('form');
        const submit = operation === 'remove'
          ? () => button(form, 'Remove from file').dispatchEvent(new MouseEvent('click'))
          : () => form.dispatchEvent(new Event('submit', { cancelable: true }));
        submit();
        return submit;
      };
      const originalFetch = window.fetch;
      try {
        for (const boundary of ['host-switch', 'workspace-round-trip']) {
          for (const operation of ['save', 'add', 'rename', 'remove']) {
            for (const refused of [false, true]) {
              common.setHost(boundary === 'host-switch' ? null : 'beta');
              common.setWorkspace('ws_same');
              view.resetGlobalFile();
              context.render();
              reloads = 0;
              const requests = [];
              window.fetch = (target, options) => new Promise((resolve) => {
                requests.push({ target, method: options.method, body: options.body, release: (refused = false) => resolve({
                  ok: !refused, status: refused ? 400 : 200,
                  text: async () => JSON.stringify(refused ? { error: 'fixture write refused', code: 'invalid_value' } : { ok: true }),
                }) });
              });
              const label = `${boundary}/${operation}/${refused ? 'refused' : 'success'}`;
              const key = operation === 'add' ? 'workflow.release_branch' : operation === 'rename' ? 'machine.name' : 'review.minutes';
              const suffix = operation === 'remove' ? '?scope=global&workspace=ws_same' : '?workspace=ws_same';
              const prefix = boundary === 'host-switch' ? '/api/' : '/api/on/beta/';
              const obsoleteSubmit = start(operation, operation === 'save' || operation === 'remove' ? '80' : 'old draft');
              assert(requests.length === 1 && requests[0].target === `${prefix}config/keys/${key}${suffix}`,
                `${label}: the first mutation is held on its starting host/workspace`);
              assert(requests[0].method === (operation === 'remove' ? 'DELETE' : 'PUT'), `${label}: the operation sends its expected method`);
              if (operation !== 'remove') assert(JSON.parse(requests[0].body).scope === 'global', `${label}: the PUT writes the global file`);
              if (boundary === 'host-switch') common.setHost('beta');
              else { common.setWorkspace('ws_other'); common.setWorkspace('ws_same'); }
              context.render();
              const fresh = body.innerHTML;
              obsoleteSubmit();
              assert(requests.length === 1 && body.innerHTML === fresh,
                `${label}: an expired draft cannot dispatch a later mutation into the new visit`);
              const value = operation === 'save' || operation === 'remove' ? '45' : 'new draft';
              const submit = start(operation, value);
              assert(requests.length === 2 && requests[1].target === `/api/on/beta/config/keys/${key}${suffix}`,
                `${label}: a newer visit owns its mutation`);
              const before = body.innerHTML;
              requests[0].release(refused);
              await frame();
              assert(requests.length === 2 && reloads === 0 && body.innerHTML === before,
                `${label}: stale settlement sends no mutation, reload or new-visit draft/status change`);
              // A render exposes an invisible pending-state change; forced
              // events check admission independently of native disabled buttons.
              context.render();
              const input = body.querySelector('#cgf-add-value, #cgf-rename-value, #cgf-edit-value');
              assert(input.disabled && input.value === value, `${label}: the newer draft remains disabled and unchanged`);
              submit();
              if (operation === 'save' || operation === 'remove') {
                const form = input.closest('form');
                form.dispatchEvent(new Event('submit', { cancelable: true }));
                button(form, 'Remove from file').dispatchEvent(new MouseEvent('click'));
              }
              assert(requests.length === 2, `${label}: stale results cannot admit duplicate PUT or DELETE`);

              requests[1].release(refused);
              await frame();
              if (refused) {
                const field = body.querySelector('#cgf-add-value, #cgf-rename-value, #cgf-edit-value');
                assert(reloads === 0 && !field.disabled && field.value === value
                  && body.querySelector('[role="alert"]')?.textContent === 'fixture write refused',
                `${label}: the owning refusal preserves and re-enables its draft without a reload`);
                assert(document.activeElement === field, `${label}: the refused draft regains focus`);
                const form = field.closest('form');
                if (operation === 'remove') button(form, 'Remove from file').click();
                else form.dispatchEvent(new Event('submit', { cancelable: true }));
                assert(requests.length === 3, `${label}: the owner can retry the refused mutation`);
                requests[2].release();
                await frame();
              }
              assert(reloads === 1 && releaseReload, `${label}: owning success begins one reload`);
              context.render();
              assert(button(row('review.minutes'), 'Edit').disabled
                && button(body.querySelector('.cwf-head'), '+ Add key').disabled
                && button(body.querySelector('.cgf-id[data-key="machine.name"]'), 'Rename').disabled,
              `${label}: mutations remain disabled through the owning reload`);
              submit();
              assert(requests.length === (refused ? 3 : 2), `${label}: pending reload prevents another mutation`);
              releaseReload();
              releaseReload = null;
              await frame();
              assert(!body.querySelector('.cgf-editor, .cwf-add, .cgf-rename')
                && !button(row('review.minutes'), 'Edit').disabled && body.querySelector('.cwf-status.ok'),
              `${label}: the owning operation settles and releases its controls after reload`);
              cases.push({ label, requests: requests.map(({ release, ...request }) => request), reloads });
            }
          }
        }

        // A scope change during the owning reload also expires its finalizer.
        for (const boundary of ['host-switch', 'workspace-round-trip']) {
          common.setHost(boundary === 'host-switch' ? null : 'beta');
          common.setWorkspace('ws_same');
          view.resetGlobalFile();
          context.render();
          reloads = 0;
          const requests = [];
          window.fetch = (target, options) => new Promise((resolve) => {
            requests.push({ target, method: options.method, body: options.body, release: () => resolve({
              ok: true, status: 200, text: async () => JSON.stringify({ ok: true }),
            }) });
          });
          const label = `${boundary}/reload`;
          start('save', '80');
          requests[0].release();
          await frame();
          assert(reloads === 1 && releaseReload, `${label}: the first Save is reloading`);
          const releaseOldReload = releaseReload;
          if (boundary === 'host-switch') common.setHost('beta');
          else { common.setWorkspace('ws_other'); common.setWorkspace('ws_same'); }
          context.render();
          const submit = start('save', '45');
          const before = body.innerHTML;
          releaseOldReload();
          await frame();
          assert(body.innerHTML === before && reloads === 1, `${label}: the old reload cannot settle the newer visit`);
          context.render();
          assert(body.querySelector('#cgf-edit-value').disabled, `${label}: the newer write still owns pending state`);
          submit();
          assert(requests.length === 2, `${label}: an obsolete reload cannot unlock a duplicate write`);
          requests[1].release();
          await frame();
          assert(reloads === 2, `${label}: the newer Save starts its own reload`);
          releaseReload();
          releaseReload = null;
          await frame();
          assert(!body.querySelector('.cgf-editor') && !button(row('review.minutes'), 'Edit').disabled,
            `${label}: the newer Save finishes normally`);
          cases.push({ label, requests: requests.map(({ release, ...request }) => request), reloads });
        }
      } finally {
        window.fetch = originalFetch;
        unsubscribe();
      }
      return cases;
    }, payload);
    assert(errors.length === 0, `no visit-race page errors: ${errors.join('; ')}`);
    return cases;
  } finally {
    await racePage.close();
  }
}

const racePayload = { ...fileView('global'), workspace_file: fileView('workspace') };

try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
  page = await context.newPage();
  page.on('pageerror', (error) => pageErrors.push(error.message));

  // 1. The header names the file, who reads it and its key count; the
  //    machine card replaces the [machine] rows.
  await page.goto(`${origin}/?workspace=ws_orbit#config/global-file`);
  await page.locator('#config-body .cgf-row').first().waitFor({ state: 'visible' });
  const head = await page.locator('#config-body .cwf-head').textContent();
  assert(head.includes(GLOBAL_PATH) && head.includes('Read by every workspace on this machine · 10 keys') && head.includes('Settings · alpha'),
    `the header names the file, its readers and its key count: ${head}`);
  const ids = await page.locator('#config-body .cgf-id').evaluateAll((nodes) => nodes.map((node) => ({
    key: node.dataset.key, value: node.querySelector('.cgf-id-value').textContent, rename: Boolean(node.querySelector('button')),
  })));
  evidenceLog.machine = ids;
  assert(JSON.stringify(ids) === JSON.stringify([
    { key: 'machine.id', value: 'hm_fixture', rename: false },
    { key: 'machine.name', value: 'alpha', rename: true },
    { key: 'machine.task_prefix', value: 'ALP', rename: false },
  ]), `the machine card: id and prefix read-only, the name renames: ${JSON.stringify(ids)}`);
  assert(!(await rows()).some((entry) => entry.key.startsWith('machine.')), 'the identity keys are not table rows');

  // 2. Reach counts for the sidebar's workspace, with the sentence naming
  //    how many keys never reach it and the unreported one.
  const counts = await cards();
  evidenceLog.counts = counts;
  assert(counts.effect.n === '5' && counts.effect.hint === 'Includes the 3 machine identity keys.', `in effect: ${JSON.stringify(counts.effect)}`);
  assert(counts.overridden.n === '2' && counts.overridden.hint === '1 of them with the same value.', `overridden: ${JSON.stringify(counts.overridden)}`);
  assert(counts.noinh.n === '2', `not inherited: ${JSON.stringify(counts.noinh)}`);
  const labels = await page.locator('#config-body .cgf-card .cwf-card-label').allTextContents();
  assert(JSON.stringify(labels) === JSON.stringify(['In effect for ws_orbit', 'Overridden by its workspace file', 'Not inherited']), `card labels: ${JSON.stringify(labels)}`);
  const sentence = await page.locator('#config-body .cgf-reach-sentence').textContent();
  assert(sentence.startsWith('4 of the 10 keys here never reach ws_orbit:') && sentence.includes('Editing those keys changes other workspaces only.')
    && sentence.includes('The reach of 1 key is not reported'), `the reach sentence: ${sentence}`);

  // 3. Keys group by TOML table; each row's reach cell says what the key
  //    does in the workspace.
  const initial = await rows();
  evidenceLog.rows = initial;
  const tables = [...new Set(initial.map((entry) => entry.table))];
  assert(JSON.stringify(tables) === JSON.stringify(['workflow', 'execution.codex', 'execution.env', 'review']), `keys group by TOML table: ${JSON.stringify(tables)}`);
  const byKey = Object.fromEntries(initial.map((entry) => [entry.key, entry]));
  const expectCell = (key, reachId, cell) => assert(byKey[key]?.reach === reachId && byKey[key].cell === cell, `${key} reach cell: ${JSON.stringify(byKey[key])}`);
  expectCell('workflow.base_branch', 'overridden', 'Overridden · workspace file sets the same value');
  expectCell('workflow.default_crew', 'overridden', 'Overridden · workspace file sets sol');
  expectCell('workflow.max_active_runs', 'effect', 'In effect');
  expectCell('workflow.auto_merge', 'effect', 'In effect');
  expectCell('execution.codex.sandbox', 'noinh', 'Not inherited · workspace file sets workspace-write');
  expectCell('execution.env.pass', 'noinh', 'Not inherited · built-in default applies: empty');
  expectCell('review.minutes', 'unreported', 'Not reported · the global and workspace files did not load together');
  assert(byKey['workflow.auto_merge'].note === 'Same as the built-in default.' && byKey['workflow.max_active_runs'].note === '', 'a value equal to the built-in default is noted');
  const colhead = await page.locator('#config-body .cgf-colhead').textContent();
  assert(colhead === 'KeyValueIn ws_orbit', `the column names the workspace: ${colhead}`);

  // Each reach card filters the table and clears on a second press.
  await page.locator('#config-body .cgf-card[data-reach="overridden"]').click();
  await until('the overridden filter', async () => (await rows()).length === 2);
  assert((await rows()).every((entry) => entry.reach === 'overridden') && (await cards()).overridden.pressed === 'true', 'the Overridden card filters to its keys');
  await page.locator('#config-body .cgf-card[data-reach="noinh"]').click();
  await until('the not-inherited filter', async () => (await rows()).every((entry) => entry.reach === 'noinh') && (await rows()).length === 2);
  await page.locator('#config-body .cgf-card[data-reach="effect"]').click();
  await until('the in-effect filter', async () => (await rows()).length === 2);
  await page.getByRole('button', { name: 'Show all 7' }).click();
  await until('every key again', async () => (await rows()).length === 7);

  // 4. The sidebar: About this file names the crew tables, Not set lists the
  //    section nothing here sets, and no "No crew is defined" claim remains.
  const crews = await page.locator('#config-body [data-key="cgf-crews"]').textContent();
  assert(crews.includes('Defines 2 crew tables: astra, nova.'), `the crew tables: ${crews}`);
  const notSet = await page.locator('#config-body .cwf-notset li').evaluateAll((nodes) => nodes.map((node) => [node.dataset.section, node.children[1]?.textContent]));
  evidenceLog.notSet = notSet;
  assert(JSON.stringify(notSet) === JSON.stringify([['housekeeping', '0 of 1 key']]), `Not set in this file: ${JSON.stringify(notSet)}`);
  const bodyText = await page.locator('#config-body').textContent();
  assert(!bodyText.includes('No crew is defined') && !bodyText.includes('No key in this section is set'), 'no misleading crew or empty-section message');
  assert(await page.locator('#config-controls').isHidden(), 'the view carries its own actions');

  // 5. Editing an in-effect key: the warning says it changes what the
  //    workspace runs, and the preview is `orbit config set --global`.
  await row('workflow.max_active_runs').getByRole('button', { name: 'Edit workflow.max_active_runs' }).click();
  await page.locator('#cgf-edit-value').fill('9');
  const inEffect = await warning('workflow.max_active_runs');
  assert(inEffect.kind === 'reaches' && inEffect.text === 'This changes what ws_orbit runs, and every other workspace that does not set workflow.max_active_runs.', `in-effect warning: ${JSON.stringify(inEffect)}`);
  const cli = await row('workflow.max_active_runs').locator('.cwf-cli').textContent();
  assert(cli === 'orbit config set --global workflow.max_active_runs 9', `the CLI preview follows the draft: ${cli}`);
  await row('workflow.max_active_runs').getByRole('button', { name: 'Save to global file' }).click();
  await until('the saved value', async () => (await rowOf('workflow.max_active_runs'))?.value === '9');
  assert(JSON.stringify(writes.at(-1)) === JSON.stringify({ method: 'PUT', key: 'workflow.max_active_runs', value: 9, scope: 'global' }), `the PUT writes the global file: ${JSON.stringify(writes.at(-1))}`);
  assert((await page.locator('#config-body .cwf-status').textContent()) === 'Saved workflow.max_active_runs to the global file.', 'the save is confirmed');

  // A refused save stays in the editor with the server's reason.
  await row('workflow.max_active_runs').getByRole('button', { name: 'Edit workflow.max_active_runs' }).click();
  await page.locator('#cgf-edit-value').fill('0');
  await row('workflow.max_active_runs').getByRole('button', { name: 'Save to global file' }).click();
  await row('workflow.max_active_runs').locator('.cfx-notice.error').waitFor({ state: 'visible' });
  assert((await row('workflow.max_active_runs').locator('.cfx-notice.error').textContent()) === 'workflow.max_active_runs must be at least 1', 'a refused save keeps the editor open with its reason');
  await page.keyboard.press('Escape');
  await until('the editor closed', async () => await page.locator('#config-body .cgf-editor').count() === 0);

  // An overridden key and a not-inherited key: no effect on the workspace,
  // other workspaces pick the change up.
  await row('workflow.default_crew').getByRole('button', { name: 'Edit workflow.default_crew' }).click();
  const overridden = await warning('workflow.default_crew');
  assert(overridden.kind === 'elsewhere' && overridden.text === 'No effect on ws_orbit: its workspace file sets workflow.default_crew. Other workspaces that do not set it pick up the change.', `overridden warning: ${JSON.stringify(overridden)}`);
  await row('execution.codex.sandbox').getByRole('button', { name: 'Edit execution.codex.sandbox' }).click();
  const notInherited = await warning('execution.codex.sandbox');
  assert(notInherited.kind === 'elsewhere' && notInherited.text.startsWith('No effect on ws_orbit: it does not inherit execution.codex.sandbox from this file'), `not-inherited warning: ${JSON.stringify(notInherited)}`);
  assert(await page.locator('#config-body .cgf-editor').count() === 1, 'one editor at a time');
  evidenceLog.warnings = { inEffect, overridden, notInherited };
  await page.keyboard.press('Escape');

  // Remove from file sends one DELETE to the global file.
  await row('workflow.auto_merge').getByRole('button', { name: 'Edit workflow.auto_merge' }).click();
  await row('workflow.auto_merge').getByRole('button', { name: 'Remove workflow.auto_merge from the global file' }).click();
  await until('the removed key', async () => !(await rowOf('workflow.auto_merge')));
  assert(JSON.stringify(writes.at(-1)) === JSON.stringify({ method: 'DELETE', key: 'workflow.auto_merge', scope: 'global' }), `the DELETE targets the global file: ${JSON.stringify(writes.at(-1))}`);

  // 6. Rename the machine: the name card writes machine.name globally.
  await page.locator('#config-body .cgf-id[data-key="machine.name"]').getByRole('button', { name: /Rename/ }).click();
  await page.locator('#cgf-rename-value').fill('bravo');
  const renameCli = await page.locator('#config-body .cgf-rename .cwf-cli').textContent();
  assert(renameCli === 'orbit config set --global machine.name bravo', `the rename preview: ${renameCli}`);
  await page.locator('#config-body .cgf-rename').getByRole('button', { name: 'Rename' }).click();
  await until('the renamed machine', async () => (await page.locator('#config-body .cgf-id[data-key="machine.name"] .cgf-id-value').textContent()) === 'bravo');
  assert(JSON.stringify(writes.at(-1)) === JSON.stringify({ method: 'PUT', key: 'machine.name', value: 'bravo', scope: 'global' }), `the rename PUT: ${JSON.stringify(writes.at(-1))}`);

  // 7. Add key offers unset, settable keys only, and warns when the
  //    workspace file already sets the chosen key.
  await page.locator('#config-body .cwf-head').getByRole('button', { name: '+ Add key' }).click();
  const offered = await page.locator('#cgf-add-key option').evaluateAll((nodes) => nodes.map((node) => node.value));
  evidenceLog.addKeyOptions = offered;
  assert(JSON.stringify(offered) === JSON.stringify(['workflow.auto_merge', 'workflow.distributed_completion', 'workflow.release_branch', 'security_alert_sweep.min_severity']), `Add key offers the unset, settable keys: ${JSON.stringify(offered)}`);
  await page.locator('#cgf-add-key').selectOption('workflow.distributed_completion');
  const addWarn = await page.locator('#config-body .cwf-add .cgf-reach-warn').evaluate((node) => ({ kind: node.dataset.reach, text: node.textContent }));
  assert(addWarn.kind === 'elsewhere' && addWarn.text.startsWith('No effect on ws_orbit: its workspace file sets workflow.distributed_completion.'), `Add key warns about a workspace override: ${JSON.stringify(addWarn)}`);
  await page.locator('#cgf-add-key').selectOption('workflow.release_branch');
  await page.locator('#cgf-add-value').fill('release');
  assert((await page.locator('#config-body .cwf-add .cwf-cli').textContent()) === 'orbit config set --global workflow.release_branch release', 'Add key previews its CLI write');
  await page.getByRole('button', { name: 'Add to global file' }).click();
  await until('the added key', async () => (await rowOf('workflow.release_branch'))?.cell === 'In effect');
  assert(JSON.stringify(writes.at(-1)) === JSON.stringify({ method: 'PUT', key: 'workflow.release_branch', value: 'release', scope: 'global' }), `the added key is written: ${JSON.stringify(writes.at(-1))}`);

  // Primary controls are 44px targets.
  const small = await page.locator('#config-body .cgf-card, #config-body .cfx-btn, #config-body .cfx-input')
    .evaluateAll((nodes) => nodes.filter((node) => node.getBoundingClientRect().height < 44).map((node) => node.className));
  assert(small.length === 0, `every primary control is at least 44px tall: ${JSON.stringify(small)}`);
  await page.screenshot({ path: path.join(evidence, 'config-global-file-1440.png'), fullPage: true });

  // 8. A phone: rows stack and nothing scrolls sideways, with an editor open.
  for (const width of [768, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await row('workflow.default_crew').getByRole('button', { name: 'Edit workflow.default_crew' }).click();
    await page.waitForTimeout(150);
    const overflow = await noPageOverflow();
    assert(overflow <= 1, `no horizontal page scroll at ${width}px (by ${overflow}px)`);
    const clipped = await page.evaluate(() => {
      const body = document.getElementById('config-body').getBoundingClientRect();
      return [...document.querySelectorAll('#config-body .cgf-row, #config-body .cgf-card, #config-body .cgf-id, #config-body .cwf-side, #config-body .cgf-editor')]
        .filter((node) => node.getBoundingClientRect().right > body.right + 1).map((node) => node.className);
    });
    assert(clipped.length === 0, `nothing overflows the panel at ${width}px: ${JSON.stringify(clipped)}`);
    await page.screenshot({ path: path.join(evidence, `config-global-file-${width}.png`), fullPage: true });
    await page.keyboard.press('Escape');
  }

  evidenceLog.visitRaces = await checkVisitRaces(browser, racePayload);
  assert(pageErrors.length === 0, `no page errors: ${pageErrors.join('; ')}`);
  fs.writeFileSync(path.join(evidence, 'config-global-file-assertions.json'), `${JSON.stringify({
    passed: true,
    assertions: [
      'browser-config-global-file-header-and-machine-card',
      'browser-config-global-file-reach-counts',
      'browser-config-global-file-reach-cells',
      'browser-config-global-file-reach-card-filter',
      'browser-config-global-file-crew-tables-not-no-crew',
      'browser-config-global-file-editor-reach-warning',
      'browser-config-global-file-refused-save-and-remove',
      'browser-config-global-file-machine-rename',
      'browser-config-global-file-add-key',
      'browser-config-global-file-phone-no-horizontal-scroll',
      'browser-config-global-file-visit-and-operation-ownership',
    ],
    viewports: ['1440', '768', '375'],
    ...evidenceLog,
  }, null, 2)}\n`);
  console.log('Dashboard Settings › Global file browser checks passed at 1440px, 768px and 375px, including 16 held-response mutation cases and two stale reload cases.');
} catch (error) {
  fs.writeFileSync(path.join(evidence, 'config-global-file-assertions.json'), JSON.stringify({ passed: false, error: error.message, pageErrors, ...evidenceLog }, null, 2));
  await page?.screenshot({ path: path.join(evidence, 'config-global-file-failure.png'), fullPage: true }).catch(() => {});
  throw error;
} finally {
  await browser?.close();
  for (const socket of sockets) socket.destroy();
  await new Promise((resolve) => server.close(resolve));
}
