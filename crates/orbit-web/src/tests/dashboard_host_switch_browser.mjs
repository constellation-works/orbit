// Usage: node dashboard_host_switch_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
//
// ORB-14680 in Chromium: the dashboard host switcher. The shipped app runs
// against a disposable fixture server that answers the serving host's API and
// the `/api/on/<host>/…` forward for registered hosts, and logs every request,
// including the log EventSource, artifact reads and task actions. No Orbit
// store, dashboard, SSH session or host is contacted.
import { pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const SERVING = 'alpha';
const VERSION = '0.28.0';
const FINGERPRINT = 'fp-current';
const READ_ONLY_REASON = 'host.forward needs the operator session (dashboard started without --operator)';
const hostRow = (name, machine_id, extra = {}) => ({
  name, machine_id, ssh: `${name}-ssh`, task_prefix: name.toUpperCase().slice(0, 3), local: false, legacy: false,
  reachable: null, error: null, binary_version: null, protocol_fingerprint: null, skew: false, skew_fields: [], workspaces: [],
  ...extra,
});
const HOSTS = {
  host_file: '/home/fixture/.orbit/hosts.toml',
  legacy: false,
  generation: 1,
  load_error: null,
  host_edit: { authorized: true, reason: null },
  hosts: [
    hostRow(SERVING, 'hm_alpha', { ssh: null, local: true, reachable: true, binary_version: VERSION, protocol_fingerprint: FINGERPRINT }),
    hostRow('deadhost', 'hm_dead'),
    hostRow('flaky', 'hm_flaky'),
    hostRow('hostb', 'hm_b'),
    hostRow('imposter', 'hm_imposter'),
    hostRow('oldhost', 'hm_old'),
    hostRow('skewhost', 'hm_skew'),
  ],
};
const FAILURES = {
  deadhost: { status: 502, code: 'unreachable_destination', message: "ssh to host 'deadhost' failed: connect to host deadhost-ssh port 22: Connection refused (exit 255)" },
  imposter: { status: 409, code: 'host_identity_mismatch', message: "host 'imposter' answered as machine hm_other, but its host file entry names hm_imposter" },
  oldhost: { status: 409, code: 'host_too_old', message: "host 'oldhost' has no /api/hosts; its dashboard predates the host registry" },
};
const WORKSPACES = {
  alpha: [{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true }, { id: 'ws_alpha', name: 'alpha-only', status: 'active', is_default: false }],
  hostb: [{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: false }, { id: 'ws_b', name: 'b-only', status: 'active', is_default: true }],
  skewhost: [{ id: 'ws_skew', name: 'skew', status: 'active', is_default: true }],
  flaky: [{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true }],
};
let forwardWrites = { authorized: true, reason: null };
let routeLog = [];
let hideTasks = false;
// The log tail's snapshots per host. A held host's snapshot requests wait in
// `heldLogs` until the test settles them; open streams wait in `logStreams`
// for the test to push events.
let logSnapshots = {};
const logHolds = new Set();
let heldLogs = [];
let logStreams = [];
// A held host's workspace list waits in `heldWorkspaces` the same way.
const workspaceHolds = new Set();
let heldWorkspaces = [];
let heldConfigDelete = null;
let holdConfigDelete = false;
// A held host's drain readiness waits in `heldReadiness` until the test
// settles it, so a response from the host being left can arrive after the
// switch. `drainReadinessOverride` replaces only the hosts a scenario names.
const readinessHolds = new Set();
let heldReadiness = [];
let drainPosts = [];
let drainReadinessOverride = null;
let autoTaskDeletePosts = [];
const removedAutoTasks = new Set();

function readinessPayload(host) {
  if (typeof drainReadinessOverride === 'function') {
    const custom = drainReadinessOverride(host);
    if (custom) return custom;
  }
  return { controls_authorized: true, tasks: [], capacity: { active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4 } };
}

function releaseHeldReadiness(host) {
  const pending = [];
  const keep = [];
  for (const entry of heldReadiness) {
    if (entry.host === host) pending.push(entry);
    else keep.push(entry);
  }
  heldReadiness = keep;
  for (const entry of pending) {
    entry.res.writeHead(200, { 'content-type': 'application/json' });
    entry.res.end(JSON.stringify(readinessPayload(entry.host)));
  }
  return pending.length;
}
const redundantConfig = {
  config_set: { authorized: true },
  sections: [{ token: 'workflow', keys: ['workflow.base_branch', 'pr.draft'].map((key) => ({
    key, label: key, value: 'same', value_type: 'string', state: 'set', settable: true,
    source: { layer: 'workspace' }, fallback: { layer: 'global', value: 'same', state: 'set' },
  })).concat([{ key: 'workflow.resource_throttle.enabled', label: 'Enabled', value: true, value_type: 'bool', state: 'set', settable: true, source: { layer: 'global' } }]) }],
  crews: [{ name: 'fixture', provider: 'codex', model: 'fixture', source: 'workspace' }],
  crew_fields: ['model'],
};

function tasksFor(host, workspace) {
  // On the serving host a run recorded on hostb links there; on hostb it
  // links back to the serving host. An unregistered machine never links.
  const peer = host === SERVING ? { machine_id: 'hm_b', machine_name: 'hostb' } : { machine_id: 'hm_alpha', machine_name: SERVING };
  return [
    { id: `${host}-RUN`, title: `${host} remote run task`, status: 'in-progress', priority: 'high', workspace_id: workspace,
      job_run_id: 'jrun-remote-1', job_run_navigable: false, job_run_machine: peer },
    { id: `${host}-LOCAL`, title: `${host} unregistered machine task`, status: 'in-progress', priority: 'medium', workspace_id: workspace,
      job_run_id: 'jrun-elsewhere', job_run_navigable: false, job_run_machine: { machine_id: 'hm_unregistered', machine_name: 'laptop' } },
    { id: `${host}-NEW`, title: `${host} proposed task`, status: 'proposed', priority: 'medium', workspace_id: workspace },
    { id: `${host}-ART`, title: `${host} artifact task`, status: 'review', priority: 'low', workspace_id: workspace,
      artifacts: [{ path: 'notes.txt', media_type: 'text/plain', size_bytes: 5 }] },
  ];
}

function claimsFor(host, taskId) {
  const claim = (machine) => ({
    claim_id: `claim-${taskId}`, task_id: taskId, phase: 'running', unsettled: true,
    executed_on: { known: true, ...machine }, bound_run: { machine_id: machine.machine_id, run_id: 'jrun-claim-1' },
    bound_run_navigable: false, created_at: '2026-10-08T00:00:00Z', updated_at: '2026-10-08T00:00:00Z',
  });
  const allowed = { authorized: true, reason: null };
  const capabilities = { handoff_approve: allowed, handoff_revoke: allowed, claim_recover: allowed };
  if (taskId === `${host}-ART`) return { owner_workspace: true, claims: [claim({ machine_id: 'hm_b', machine_name: 'hostb' })], capabilities };
  if (taskId === `${host}-LOCAL`) return { owner_workspace: true, claims: [claim({ machine_id: 'hm_unregistered', machine_name: 'laptop' })], capabilities };
  return { owner_workspace: true, claims: [], capabilities };
}

const resources = (percent) => ({
  cpu: { percent, severity: 'ok' }, memory: { percent: 40, severity: 'ok' }, disk: { path: '/', percent: 30, severity: 'ok' },
  severity: 'ok', sample_age_seconds: 1, max_age_seconds: 15, throttle: false, pressures: [], reason: 'Below every threshold',
  thresholds: { enabled: true }, stale: false,
});

function connection(name) {
  const row = HOSTS.hosts.find((host) => host.name === name);
  if (!row) return { status: 404, body: { error: `no registered host named '${name}'`, code: 'unknown_host', host: name } };
  const base = { host: row.name, machine_id: row.machine_id, local: row.local, origin: row.local ? 'local' : 'attached',
    binary_version: VERSION, protocol_fingerprint: FINGERPRINT, skew: false, skew_fields: [], error: null, forward_writes: forwardWrites };
  if (FAILURES[name]) {
    const { code, message } = FAILURES[name];
    return { status: 200, body: { ...base, reachable: false, origin: null, binary_version: null, protocol_fingerprint: null, error: { code, message } } };
  }
  if (name === 'skewhost') {
    return { status: 200, body: { ...base, reachable: true, binary_version: '0.27.0', protocol_fingerprint: 'fp-older', skew: true, skew_fields: ['binary_version', 'protocol_fingerprint'] } };
  }
  return { status: 200, body: { ...base, reachable: true } };
}

// One /api/jobs row, in the shape list_jobs returns: a JSON array of catalog
// objects, not {items,total}. manual_runnable is the catalogue admission bit.
function jobCatalogRow(jobId, manualRunnable, lastRunState, lastRunAt) {
  return {
    job_id: jobId, schema_version: 2, kind: 'workflow', state: 'enabled', default_input: null,
    max_active_runs: 1, manual_runnable: manualRunnable, supports_no_input_submission: manualRunnable,
    steps: [], path: `jobs/${jobId}.json`, last_run_state: lastRunState, last_run_at: lastRunAt,
  };
}

// One host's own dashboard API, as the forward would relay it.
function hostApi(host, req, rest, url, res) {
  const json = (body, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
  const workspaces = WORKSPACES[host] || [];
  const workspace = url.searchParams.get('workspace') || (workspaces.find((ws) => ws.is_default) || {}).id;
  if (req.method !== 'GET') {
    if (holdConfigDelete && req.method === 'DELETE' && rest.startsWith('config/keys/')) {
      holdConfigDelete = false;
      heldConfigDelete = res;
      return;
    }
    const approved = rest.match(/^tasks\/([^/]+)\/approve$/);
    if (approved) return json({ ...tasksFor(host, workspace).find((task) => task.id === approved[1]), status: 'backlog' });
    if (rest === 'workflows/auto' || rest === 'workflows/auto/stop') {
      let raw = '';
      req.on('data', (chunk) => { raw += chunk; });
      req.on('end', () => {
        let body = null;
        try { body = raw ? JSON.parse(raw) : null; } catch { body = raw; }
        drainPosts.push({ host, rest, body });
        json({ ok: true });
      });
      return;
    }
    if (rest === 'auto-tasks/delete') {
      let raw = '';
      req.on('data', (chunk) => { raw += chunk; });
      req.on('end', () => {
        let body = null;
        try { body = raw ? JSON.parse(raw) : null; } catch { body = raw; }
        autoTaskDeletePosts.push({ host, body, workspace });
        if (body && body.force === true) {
          removedAutoTasks.add(`${host}:${body.name}`);
          json({
            name: body.name, deleted: true, opted_out: false, open_tasks: ['ORB-9001'],
            reason: body.reason, message: `Deleted auto-task ${body.name}.`,
          });
          return;
        }
        json({
          error: `auto-task '${body?.name}' still has open minted tasks: ORB-9001; close them, or pass --force to delete anyway`,
          code: 'open_minted_tasks',
          open_tasks: ['ORB-9001'],
        }, 409);
      });
      return;
    }
    return json({ ok: true });
  }
  if (rest === 'workspaces') {
    if (workspaceHolds.has(host)) { heldWorkspaces.push({ host, res }); return; }
    return json(workspaces);
  }
  if (rest === 'tasks') return json({ items: hideTasks ? [] : tasksFor(host, workspace), total: hideTasks ? 0 : 4, limit: 50, truncated: false });
  if (rest === 'tasks/all') return json({ items: workspaces.flatMap((ws) => tasksFor(host, ws.id)), total: 4 * workspaces.length });
  if (rest === 'tasks/locks') return json({ total_locked: 0, total_tasks: 0, by_task: [] });
  const artifact = rest.match(/^tasks\/([^/]+)\/artifacts\/(.+)$/);
  if (artifact) { res.writeHead(200, { 'content-type': 'text/plain' }); res.end(`hello from ${host}`); return; }
  const task = rest.match(/^tasks\/([^/]+)$/);
  if (task) return json(tasksFor(host, workspace).find((item) => item.id === decodeURIComponent(task[1])) || { error: 'not found' });
  if (rest === 'distributed/claims') return json(claimsFor(host, url.searchParams.get('task')));
  if (rest === 'crews') return json({ default_crew: null, crews: [] });
  if (rest === 'host/resources') return json(resources(host === SERVING ? 25 : 75));
  if (rest === 'log') {
    if (logHolds.has(host)) { heldLogs.push({ host, res }); return; }
    return json(logSnapshots[host] || { events: [], offset: 0, agent_offset: 0 });
  }
  if (rest === 'log/stream') {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    res.write(': open\n\n');
    logStreams.push({ host, query: url.search, res });
    return;
  }
  if (rest === 'workflows/auto/readiness') {
    if (readinessHolds.has(host)) { heldReadiness.push({ host, res }); return; }
    return json(readinessPayload(host));
  }
  if (rest === 'routines') return json({ machine_name: host, capabilities: {}, clock: {}, routines: [] });
  // Catalogue admission is this list. A run alone does not make a job runnable.
  if (rest === 'jobs') return json([
    jobCatalogRow('fixture_sweep', true, 'running', '2026-10-08T00:00:01Z'),
    jobCatalogRow('task_pr_pipeline', false, 'failed', '2026-10-08T00:05:00Z'),
  ]);
  if (rest === 'job-runs') return json({ items: [
    { run_id: `jrun-${host}-confirmed`, job_id: 'fixture_sweep', state: 'running', run_role: 'top-level',
      created_at: '2026-10-08T00:00:00Z', started_at: '2026-10-08T00:00:01Z', finished_at: null, duration_ms: null },
    { run_id: `jrun-${host}-delivery`, job_id: 'task_pr_pipeline', state: 'failed', run_role: 'child',
      created_at: '2026-10-08T00:04:00Z', started_at: '2026-10-08T00:04:01Z', finished_at: '2026-10-08T00:05:00Z', duration_ms: 59000 },
    { run_id: `jrun-${host}-unlisted`, job_id: 'unlisted_side_job', state: 'succeeded', run_role: 'top-level',
      created_at: '2026-10-08T00:06:00Z', started_at: '2026-10-08T00:06:01Z', finished_at: '2026-10-08T00:06:02Z', duration_ms: 1000 },
  ], total: 3, limit: 100, truncated: false });
  if (rest === 'auto-tasks') {
    const allowed = { authorized: true, reason: null };
    const definitions = [{ name: `${host}-confirmed-definition`, enabled: true, template: { title: 'Confirmed chore' }, template_summary: 'Confirmed chore',
      schedule_summary: 'every 15 minutes', description: 'A definition the read-only host still lists.', next_evaluation: { state: 'waiting' } }]
      .filter((definition) => !removedAutoTasks.has(`${host}:${definition.name}`));
    return json({ workspace, controls_authorized: true, capabilities: {
      auto_task_toggle: allowed, auto_task_mint: allowed, auto_task_schedule: allowed, auto_task_delete: allowed,
    }, definitions });
  }
  const run = rest.match(/^runs\/([^/]+)$/);
  if (run) return json({ run_id: decodeURIComponent(run[1]), job_name: 'auto', state: 'running', steps: [], workspace_id: workspace });
  if (rest.startsWith('runs/')) return json([]);
  if (rest === 'audit/summary') return json({ window: '24h', events: 0, failed_runs: 0 });
  if (rest === 'config/effective' || rest === 'config/file') return json(redundantConfig);
  if (rest.startsWith('config/')) return json({ entries: [], rows: [], config_set: { authorized: true, reason: null } });
  return json({ items: [], total: 0 });
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  const name = url.pathname;
  if (!name.startsWith('/api/')) {
    const served = dashboardFile(name);
    if (!served) { res.writeHead(404); res.end(); return; }
    res.setHeader('content-type', served.type);
    res.end(served.data);
    return;
  }
  routeLog.push({ method: req.method, path: `${name}${url.search}` });
  const json = (body, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
  if (name === '/api/hosts' && req.method === 'POST') {
    let body = '';
    req.on('data', chunk => { body += chunk; });
    req.on('end', () => {
      const data = JSON.parse(body);
      const row = hostRow(data.name, 'hm_added', { ssh: data.ssh });
      HOSTS.hosts.push(row);
      json({ action: 'added', entry: row });
    });
    return;
  }
  const editHost = name.match(/^\/api\/hosts\/(hm_added)$/);
  if (editHost && req.method === 'PATCH') {
    let body = '';
    req.on('data', chunk => { body += chunk; });
    req.on('end', () => {
      const row = HOSTS.hosts.find(item => item.machine_id === editHost[1]);
      const previous_name = row.name;
      row.name = JSON.parse(body).name;
      json({ previous_name, entry: row });
    });
    return;
  }
  if (editHost && req.method === 'DELETE') {
    const index = HOSTS.hosts.findIndex(item => item.machine_id === editHost[1]);
    const [row] = HOSTS.hosts.splice(index, 1);
    return json({ entry: row });
  }
  if (name === '/api/hosts') return json(HOSTS);
  const conn = name.match(/^\/api\/hosts\/([^/]+)\/connection$/);
  if (conn) {
    const { status, body } = connection(decodeURIComponent(conn[1]));
    return json(body, status);
  }
  const forwarded = name.match(/^\/api\/on\/([^/]+)\/(.+)$/);
  if (forwarded) {
    const host = decodeURIComponent(forwarded[1]);
    if (FAILURES[host]) return json({ error: FAILURES[host].message, code: FAILURES[host].code, host }, FAILURES[host].status);
    // flaky's connection reads reachable, but every forwarded request fails:
    // panels must collapse into one host-level state, not fail one by one.
    if (host === 'flaky') return json({ error: "the forward to host 'flaky' failed: connection reset", code: 'unreachable_destination', host }, 502);
    if (!WORKSPACES[host]) return json({ error: `no registered host named '${host}'`, code: 'unknown_host', host }, 404);
    if (req.method !== 'GET' && forwardWrites.authorized === false) {
      return json({ error: READ_ONLY_REASON, code: 'authorization_denied', operation: 'host.forward' }, 403);
    }
    return hostApi(host, req, forwarded[2], url, res);
  }
  return hostApi(SERVING, req, name.slice('/api/'.length), url, res);
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
const logged = (pattern) => routeLog.some((entry) => pattern.test(`${entry.method} ${entry.path}`));
const ALWAYS_SERVING = /^\/api\/hosts(?:[/?]|$)/;

let browser;
let context;
let page;
const pageErrors = [];
const evidenceLog = {};

async function openPage(url, { viewport = { width: 1440, height: 900 }, init = null, fresh = false } = {}) {
  if (fresh || !context) {
    if (context) await context.close();
    context = await browser.newContext({ viewport });
    if (init) await context.addInitScript(init);
  }
  page = await context.newPage();
  await page.setViewportSize(viewport);
  page.on('pageerror', (error) => pageErrors.push(`${url}: ${error.message}`));
  await page.goto(`${origin}${url}`);
  await page.locator('#host-select').waitFor({ state: 'attached' });
  return page;
}

const selectedHost = () => page.evaluate(() => new URL(window.location.href).searchParams.get('host'));
const taskRow = (id) => page.locator(`#tasks-body [data-key="task-${id}"]`);
const capture = (name) => page.screenshot({ path: path.join(evidence, `${name}.png`), fullPage: false });

async function captureDrainPanel(file, width) {
  await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
  const pane = page.locator('#dock-pane-drain');
  await pane.waitFor({ state: 'visible' });
  await page.waitForTimeout(100);
  await pane.screenshot({ path: path.join(evidence, file), animations: 'disabled' });
}

async function captureWidths(name) {
  for (const width of [1440, 768]) {
    await page.setViewportSize({ width, height: 900 });
    await page.waitForTimeout(100);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    assert(overflow <= 1, `${name} must not overflow at ${width}px (by ${overflow}px)`);
    await capture(`host-${name}-${width}`);
  }
  await page.setViewportSize({ width: 1440, height: 900 });
}

async function switchHost(name) {
  await page.locator('#host-select').selectOption(name);
  await until(`?host=${name || '(serving)'}`, async () => (await selectedHost()) === (name || null));
}

async function assertNamed(label) {
  const select = await page.locator('#host-select').evaluate((node) => node.options[node.selectedIndex].textContent);
  assert(select.includes(label), `the rail picker names ${label}: ${select}`);
  const chipLabel = await page.locator('#host-resource-group').getAttribute('aria-label');
  const detailLabel = await page.locator('#host-detail').getAttribute('aria-label');
  assert(chipLabel.includes(label), `provider usage names ${label}: ${chipLabel}`);
  assert(detailLabel.includes(label), `the host detail names ${label}: ${detailLabel}`);
  assert(await page.locator('#host-select').isVisible(), 'the host name stays visible on the rail');
  const conn = await page.locator('#conn-host').textContent();
  assert(conn.includes(label), `the connection line names ${label}: ${conn}`);
}

try {
  browser = await chromium.launch({ headless: true });

  // A bulk deletion cannot follow the picker into another host, even when
  // both hosts name the same workspace. Keep the first response held until
  // the destination Settings page has rendered.
  await openPage('/?workspace=ws_orbit#config/all-keys', { fresh: true });
  await page.evaluate(() => {
    const original = window.fetch;
    window.settingsSettled = 0;
    window.settingsDeletes = [];
    window.fetch = async (...args) => {
      if (args[1]?.method === 'DELETE') window.settingsDeletes.push(String(args[0]));
      const response = await original(...args);
      if (args[1]?.method === 'DELETE') {
        const text = response.text.bind(response);
        response.text = async () => { const body = await text(); window.settingsSettled++; return body; };
      }
      return response;
    };
  });
  const bulk = page.getByRole('button', { name: 'Remove all 2 workspace values', exact: true });
  await bulk.waitFor({ state: 'visible' });
  routeLog = [];
  holdConfigDelete = true;
  await bulk.click();
  await until('the held Settings DELETE', () => heldConfigDelete !== null);
  await switchHost('hostb');
  await until('the remote Settings payload', () => logged(/^GET \/api\/on\/hostb\/config\/effective\?workspace=ws_orbit$/));
  await bulk.waitFor({ state: 'visible' });
  heldConfigDelete.writeHead(200, { 'content-type': 'application/json' });
  heldConfigDelete.end('{"ok":true}');
  heldConfigDelete = null;
  await page.waitForFunction(() => window.settingsSettled >= 1);
  await page.evaluate(() => new Promise(requestAnimationFrame));
  const sentDeletes = await page.evaluate(() => window.settingsDeletes);
  assert(sentDeletes.length === 1, `a retired bulk loop begins no second fetch: ${JSON.stringify(sentDeletes)}`);
  const deletes = routeLog.filter((entry) => entry.method === 'DELETE');
  assert(deletes.length === 1 && deletes[0].path === '/api/config/keys/workflow.base_branch?scope=workspace&workspace=ws_orbit',
    `bulk removal stays in its initiating host and visit: ${JSON.stringify(deletes)}`);
  assert(!(await page.locator('#config-body').textContent()).includes('Removed '), 'the new host receives no stale bulk completion');
  evidenceLog.settingsBulkRoutes = deletes;
  for (const view of ['all-keys', 'machine', 'crews']) {
    await page.evaluate((view) => { window.location.hash = `config/${view}`; }, view);
    await until(`Settings ${view}`, () => page.evaluate(async (view) => (await import('/static/js/config.js')).getConfigSubtab() === view, view));
    if (view === 'machine') await page.locator('.config-sys-enabled .config-pencil').click();
    if (view === 'crews') await page.locator('.config-crew-row .config-pencil').click();
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
      if (view !== 'all-keys') await page.locator('.config-editor-actions').scrollIntoViewIfNeeded();
      await capture(`settings-${view}-dark-${width}`);
    }
    await page.setViewportSize({ width: 1440, height: 900 });
  }
  await page.close();

  // 1. Picker order, the switch, and every request after it.
  routeLog = [];
  await openPage('/', { fresh: true });
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  const options = await page.locator('#host-select option').evaluateAll((nodes) => nodes.map((node) => [node.value, node.textContent]));
  assert(options[0][0] === '' && options[0][1] === `${SERVING} (serving host)`, `the serving host is listed first: ${JSON.stringify(options)}`);
  assert(options.slice(1).map(([value]) => value).join(',') === 'deadhost,flaky,hostb,imposter,oldhost,skewhost',
    `registered remotes follow in host-file order: ${JSON.stringify(options)}`);
  assert(!logged(/\/connection/), 'opening the serving host reads no connection state');
  assert(!logged(/\/api\/hosts(?!\?probe=false)/), 'building the picker never probes');
  await assertNamed(`${SERVING} (serving host)`);
  await captureWidths('serving');

  routeLog = [];
  await switchHost('hostb');
  await taskRow('hostb-RUN').waitFor({ state: 'visible' });
  await assertNamed('hostb');
  assert(await page.locator('#host-announcer').textContent() === 'Showing hostb', 'the switch is announced');
  assert(await page.locator('#host-announcer').getAttribute('aria-live') === 'polite', 'announcements are polite');
  await until('the log stream on hostb', () => logged(/^GET \/api\/on\/hostb\/log\/stream\?/));
  await until('host resources on hostb', () => logged(/^GET \/api\/on\/hostb\/host\/resources$/));
  await page.locator('#tasks-body [data-key="task-hostb-ART"] .title').click();
  await page.locator('#task-detail-body').getByRole('tab', { name: /^Artifacts/ }).click();
  await page.locator('.artifact-row').first().click();
  await page.locator('.artifact-preview', { hasText: 'hello from hostb' }).waitFor({ state: 'visible' });
  await taskRow('hostb-NEW').locator('.task-quick.approve').click();
  await until('the approve action on hostb', () => logged(/^POST \/api\/on\/hostb\/tasks\/hostb-NEW\/approve\?workspace=ws_orbit$/));
  for (const route of ['jobs/history', 'runs/jrun-remote-1', 'operations/routines', 'diagnostics/incidents', 'config/effective', 'config/hosts']) {
    await page.evaluate((route) => { window.location.hash = route; }, route);
    await page.waitForTimeout(400);
  }
  await page.locator('.host-scope-selected').waitFor({ state: 'visible' });
  const hostsNote = await page.locator('.host-scope-selected').textContent();
  assert(hostsNote.includes('hostb') && hostsNote.includes(`${SERVING}'s host file`), `Settings › Hosts says whose host file it lists: ${hostsNote}`);
  assert(await page.locator('.config-note', { hasText: `host file of ${SERVING}` }).count() === 1, 'Settings › Hosts still lists the serving host file');
  await captureWidths('hostb');
  await page.evaluate(() => { window.location.hash = 'tasks'; });

  for (const pattern of [
    /^GET \/api\/hosts\/hostb\/connection(\?|$)/, /^GET \/api\/on\/hostb\/workspaces(\?|$)/, /^GET \/api\/on\/hostb\/tasks\?/,
    /^GET \/api\/on\/hostb\/log\?limit=50/, /^GET \/api\/on\/hostb\/tasks\/hostb-ART\/artifacts\/notes\.txt\?workspace=ws_orbit$/,
    /^GET \/api\/on\/hostb\/distributed\/claims\?/, /^GET \/api\/on\/hostb\/runs\/jrun-remote-1\?/, /^GET \/api\/on\/hostb\/routines/,
    /^GET \/api\/on\/hostb\/config\//, /^GET \/api\/hosts(\?|$)/,
  ]) {
    assert(logged(pattern), `expected a request matching ${pattern}: ${JSON.stringify(routeLog, null, 1)}`);
  }
  // Settings > Hosts reads each registered host's resource snapshot, including
  // the serving host. This is independent of the selected dashboard scope.
  const servingResources = entry => entry.method === 'GET' && entry.path === `/api/on/${SERVING}/host/resources`;
  const strays = routeLog.filter((entry) => !entry.path.startsWith('/api/on/hostb/') && !ALWAYS_SERVING.test(entry.path) && !servingResources(entry));
  assert(strays.length === 0, `every request after selecting hostb goes to /api/on/hostb/: ${JSON.stringify(strays)}`);
  evidenceLog.hostbRoutes = routeLog;

  // 2. URL and memory.
  await page.reload();
  await taskRow('hostb-RUN').waitFor({ state: 'visible' });
  assert(await selectedHost() === 'hostb' && await page.locator('#host-select').inputValue() === 'hostb', 'reload restores hostb from the URL');
  await page.goto(`${origin}/`);
  await taskRow('hostb-RUN').waitFor({ state: 'visible' });
  assert(await selectedHost() === 'hostb', 'a bare URL restores the remembered host and writes it to the URL');
  // A native tray or notification link explicitly selects the serving host,
  // even when this webview last visited a registered remote.
  routeLog = [];
  await page.goto(`${origin}/?host=&workspace=ws_orbit#tasks?open=alpha-ART`);
  await page.locator('#task-detail-body', { hasText: 'alpha artifact task' }).waitFor({ state: 'visible' });
  assert(await selectedHost() === null, 'an explicit serving-host link overrides the remembered remote host');
  assert(logged(/^GET \/api\/distributed\/claims\?task=alpha-ART&/), 'the local tray task detail reads on the serving host');
  await page.goto(`${origin}/?host=&workspace=ws_orbit#runs?run_id=jrun-remote-1`);
  await until('the local tray run link', () => logged(/^GET \/api\/runs\/jrun-remote-1\?workspace=ws_orbit$/));
  assert(await selectedHost() === null, 'an explicit serving-host run link overrides the remembered remote host');
  await page.goto(`${origin}/?host=skewhost`);
  await taskRow('skewhost-RUN').waitFor({ state: 'visible' });
  assert(await page.locator('#host-select').inputValue() === 'skewhost', 'the URL wins over the remembered host');
  await page.evaluate(() => localStorage.setItem('orbit.dashboard.host', 'gonehost'));
  await page.goto(`${origin}/`);
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  assert(await selectedHost() === null, 'a remembered host that is no longer registered falls back to the serving host');
  assert(await page.evaluate(() => localStorage.getItem('orbit.dashboard.host')) === null, 'the stale memory is dropped');
  await page.close();

  await openPage('/', { fresh: true, init: () => {
    Object.defineProperty(window, 'localStorage', { configurable: true, get() { throw new Error('storage denied'); } });
  } });
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  assert(await selectedHost() === null, 'a throwing localStorage falls back to the serving host');
  await switchHost('hostb');
  await taskRow('hostb-RUN').waitFor({ state: 'visible' });
  await page.close();

  // 3. Workspaces per host.
  await openPage('/?workspace=ws_alpha', { fresh: true });
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  await switchHost('hostb');
  await until('hostb default workspace', async () => await page.locator('#workspace-select').inputValue().catch(() => '') === 'ws_b');
  assert((await page.locator('#workspace-select option').allTextContents()).join(',') === 'All workspaces,orbit,b-only', 'the picker is rebuilt from hostb');
  await page.locator('#workspace-select').selectOption('ws_orbit');
  await switchHost('');
  await until('ws_orbit kept on the serving host', async () => (await page.evaluate(() => new URL(location.href).searchParams.get('workspace'))) === 'ws_orbit');
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  assert(await page.locator('#workspace-select').inputValue() === 'ws_orbit', 'a workspace id both hosts list stays selected');
  await switchHost('hostb');
  await taskRow('hostb-RUN').waitFor({ state: 'visible' });
  routeLog = [];
  await page.locator('#workspace-select').selectOption('');
  await until('hostb aggregate', () => logged(/^GET \/api\/on\/hostb\/tasks\/all/));
  assert(!routeLog.some((entry) => entry.path.startsWith('/api/tasks')), 'All workspaces aggregates the selected host only');
  await switchHost('skewhost');
  await taskRow('skewhost-RUN').waitFor({ state: 'visible' });
  assert(await page.locator('#workspace-select').count() === 0, 'a single-workspace host has no workspace picker');
  await page.close();

  // 4. Failure states and the way back, by keyboard.
  await openPage('/', { fresh: true });
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  for (const [host, code] of [['deadhost', 'unreachable_destination'], ['imposter', 'host_identity_mismatch'], ['oldhost', 'host_too_old'], ['flaky', 'unreachable_destination']]) {
    await switchHost(host);
    const state = page.locator('#host-state');
    await state.waitFor({ state: 'visible' });
    const text = await state.textContent();
    assert(text.includes(code) && text.includes(host), `${host} shows one host-level state with ${code}: ${text}`);
    if (FAILURES[host]) assert(text.includes(FAILURES[host].message), `${host} shows the forward's message: ${text}`);
    assert(!(await page.locator('.tab-pane.active').isVisible()), `${host} replaces the panels`);
    assert(await page.locator('.panel-placeholder.action-error:visible').count() === 0, `${host} shows no failed panels`);
    assert(await page.locator('#host-block .host-resource.unknown').count() === 3, `${host} shows no other host's readings`);
    if (host === 'deadhost') await captureWidths('unreachable');
    const back = page.locator('.host-state-back');
    assert((await back.textContent()) === `Back to ${SERVING}`, 'the way back names the serving host');
    await back.focus();
    await page.keyboard.press('Enter');
    await until('back on the serving host', async () => (await selectedHost()) === null);
    await taskRow('alpha-RUN').waitFor({ state: 'visible' });
    assert(!(await state.isVisible()), 'switching back clears the host-level state');
    assert(await page.locator('.tab-pane.active').isVisible(), 'switching back restores every panel');
  }
  await switchHost('deadhost');
  await page.locator('#host-state').waitFor({ state: 'visible' });
  await page.locator('#host-select').focus();
  await page.locator('#host-select').selectOption('hostb');
  await taskRow('hostb-RUN').waitFor({ state: 'visible' });
  assert(!(await page.locator('#host-state').isVisible()), 'the picker stays operable while a host is unavailable');

  // 5. Skew.
  await switchHost('skewhost');
  const skew = page.locator('#host-skew');
  await skew.waitFor({ state: 'visible' });
  const skewText = await skew.textContent();
  assert(skewText.includes('0.27.0') && skewText.includes(VERSION) && skewText.includes('skewhost') && skewText.includes(SERVING),
    `the skew banner names both versions: ${skewText}`);
  await page.locator('#refresh-btn').click();
  await page.waitForTimeout(300);
  assert(await skew.isVisible(), 'the skew banner persists across refreshes');
  assert(await page.locator('.tab-pane.active').isVisible(), 'skew never blocks the panels');
  await captureWidths('skewed');
  await page.close();

  // 6. Remote runs link to their host; unregistered machines stay text. The
  // page starts on a non-default window, so a link that drops it would show.
  await openPage('/?window=7d', { fresh: true });
  const remoteLink = taskRow('alpha-RUN').locator('.task-quick-cell a.remote-run-link');
  await remoteLink.waitFor({ state: 'visible' });
  assert(await remoteLink.getAttribute('href') === '?host=hostb&workspace=ws_orbit&window=7d#runs?run_id=jrun-remote-1', `in-progress row links to the run on hostb: ${await remoteLink.getAttribute('href')}`);
  // Follow a copied run link in a separate tab. Its URL must override the
  // remembered host and retain the selected time window on startup.
  const linkedTab = await context.newPage();
  try {
    await linkedTab.goto(new URL(await remoteLink.getAttribute('href'), page.url()).href);
    await linkedTab.waitForFunction(async () => {
      const { getHost, getWorkspace, getWindow } = await import('/static/js/common.js');
      return getHost() === 'hostb' && getWorkspace() === 'ws_orbit' && getWindow() === '7d'
        && document.querySelector('.tab-pane.active')?.dataset.tab === 'run-detail';
    });
    await until('a new tab fetching the linked run on its named host', () => logged(/^GET \/api\/on\/hostb\/runs\/jrun-remote-1\?workspace=ws_orbit/));
  } finally {
    await linkedTab.close();
  }
  assert(await taskRow('alpha-LOCAL').locator('.task-quick-cell a').count() === 0, 'an unregistered machine is not a link');
  assert((await taskRow('alpha-LOCAL').locator('.task-quick-cell .task-quick-link.unlinked').textContent()) === 'View run', 'an unregistered machine keeps a dimmed View run label');
  await page.locator('#tasks-body [data-key="task-alpha-ART"] .title').click();
  const deliveryTab = page.locator('#task-detail-body').getByRole('tab', { name: 'Delivery' });
  await deliveryTab.click();
  const claimLink = page.locator('.delivery-panel a.remote-run-link');
  await claimLink.waitFor({ state: 'visible' });
  assert(await claimLink.getAttribute('href') === '?host=hostb&workspace=ws_orbit&window=7d#runs?run_id=jrun-claim-1', 'the claim panel links to the run on hostb');
  await page.locator('#tasks-body [data-key="task-alpha-ART"] .title').click();
  await page.locator('#tasks-body [data-key="task-alpha-LOCAL"] .title').click();
  await deliveryTab.click();
  await page.locator('.delivery-panel .exec-origin', { hasText: 'on laptop' }).waitFor({ state: 'visible' });
  assert(await page.locator('.delivery-panel a.remote-run-link').count() === 0, 'an unregistered claim machine stays text');
  routeLog = [];
  await remoteLink.click();
  await page.waitForURL((url) => url.searchParams.get('host') === 'hostb');
  await until('the linked run on hostb', () => logged(/^GET \/api\/on\/hostb\/runs\/jrun-remote-1\?workspace=ws_orbit/));
  await page.close();

  // 7. Refused writes: disabled with the reason, and nothing is sent.
  forwardWrites = { authorized: false, reason: READ_ONLY_REASON };
  await openPage('/?host=hostb', { fresh: true });
  await taskRow('hostb-NEW').waitFor({ state: 'visible' });
  const readOnly = page.locator('#host-read-only');
  await readOnly.waitFor({ state: 'visible' });
  assert((await readOnly.textContent()).includes(READ_ONLY_REASON), 'the read-only note gives the refusal reason');
  const quick = taskRow('hostb-NEW').locator('.task-quick.approve');
  assert(await quick.isDisabled(), 'the quick action is disabled on a read-only host');
  assert((await quick.getAttribute('title')).includes(READ_ONLY_REASON), 'the disabled quick action says why');
  await page.locator('#tasks-body [data-key="task-hostb-ART"] .title').click();
  // Overview and Comments each render their own action row, and inactive
  // panels stay in the DOM, so each note is scoped to its visible panel.
  const overviewNote = page.locator('#task-detail-body .task-action-row.lifecycle-actions .host-read-only-note:visible');
  await overviewNote.waitFor({ state: 'visible' });
  assert((await overviewNote.textContent()).includes(READ_ONLY_REASON), 'the Overview note gives the refusal reason');
  assert(await page.locator('#task-detail-body .actions > .action.approve').first().isDisabled(), 'detail actions are disabled');
  await page.locator('#task-detail-body').getByRole('tab', { name: /^Comments/ }).click();
  const commentNote = page.locator('#task-detail-body .task-action-row.comment-actions .host-read-only-note:visible');
  await commentNote.waitFor({ state: 'visible' });
  assert((await commentNote.textContent()).includes(READ_ONLY_REASON), 'the Comments note gives the refusal reason');
  assert(await page.locator('#task-detail-body .task-action-row.comment-actions .action.comment:visible').isDisabled(), 'Comment is disabled on a read-only host');
  await page.locator('#task-detail-body').getByRole('tab', { name: 'Delivery' }).click();
  await page.locator('.claim-action-denied', { hasText: 'Read-only on hostb' }).first().waitFor({ state: 'visible' });
  await capture('host-read-only-1440');
  // Refused writes do not refuse reads: Jobs Live, the Catalogue and the
  // Auto-task definitions render hostb's successful GETs for the selected
  // workspace, with their write controls refused for the same reason.
  await page.evaluate(() => { window.location.hash = 'jobs/live'; });
  await page.locator('#jobs-live-body', { hasText: 'jrun-hostb-confirmed' }).waitFor({ state: 'visible' });
  assert(!(await page.locator('#jobs-live-body').textContent()).includes('Select a workspace'), 'Jobs Live lists the read-only host runs');
  await capture('host-read-only-jobs-live-dark-1440');
  await page.evaluate(() => { window.location.hash = 'jobs/catalogue'; });
  const catalogueJob = page.locator('#jobs-body [data-job="fixture_sweep"]');
  await catalogueJob.waitFor({ state: 'visible' });
  const catalogueJobs = await page.locator('#jobs-body [data-job]').evaluateAll((nodes) => nodes.map((node) => node.dataset.job));
  assert(catalogueJobs.length === 1 && catalogueJobs[0] === 'fixture_sweep', `the catalogue lists only the server-declared runnable job: ${catalogueJobs.join(', ')}`);
  const runControl = catalogueJob.getByRole('button', { name: 'Run ▸' });
  assert(await runControl.isDisabled(), 'Run stays refused on a read-only host');
  const runReason = await catalogueJob.locator('.operation-action-unavailable').getAttribute('aria-label');
  assert(runReason.includes(READ_ONLY_REASON), `Run says why it is off: ${runReason}`);
  assert((await page.locator('#jobs-body .operations-readonly-note').textContent()).includes(READ_ONLY_REASON), 'the Catalogue says why Run is off');
  await capture('host-read-only-jobs-catalogue-dark-1440');
  await page.evaluate(() => { window.location.hash = 'operations/auto-tasks'; });
  await page.locator('#auto-tasks-body', { hasText: 'hostb-confirmed-definition' }).waitFor({ state: 'visible' });
  assert(await page.locator('#auto-tasks-body').getByRole('button', { name: 'Mint now' }).isDisabled(), 'Mint stays refused on a read-only host');
  assert(await page.locator('#auto-tasks-body').getByRole('button', { name: 'Delete auto-task hostb-confirmed-definition' }).isDisabled(), 'Delete stays refused on a read-only host');
  assert(await page.locator('#auto-tasks-body [role="switch"]').first().isDisabled(), 'the auto-task switch stays refused on a read-only host');
  await capture('host-read-only-auto-tasks-dark-1440');
  assert(logged(/^GET \/api\/on\/hostb\/jobs\?/) && logged(/^GET \/api\/on\/hostb\/job-runs\?/) && logged(/^GET \/api\/on\/hostb\/auto-tasks\?/), 'the reads went to hostb');
  assert(!routeLog.some((entry) => entry.method !== 'GET' && entry.path.startsWith('/api/on/')), 'no write is sent to a read-only host');
  forwardWrites = { authorized: true, reason: null };
  await page.close();

  // A writable federated view deletes on the owning host. The first post is
  // refused because a minted task is open; Delete anyway is the force.
  autoTaskDeletePosts.length = 0;
  removedAutoTasks.clear();
  await openPage('/?host=hostb&workspace=ws_orbit', { fresh: true });
  await page.evaluate(() => { window.location.hash = 'operations/auto-tasks'; });
  await page.locator('#auto-tasks-body', { hasText: 'hostb-confirmed-definition' }).waitFor({ state: 'visible' });
  const scratch = process.env.ORBIT_SCRATCH_DIR;
  if (scratch) fs.mkdirSync(path.join(scratch, 'screenshots'), { recursive: true });
  const shot = async (file, width, locator) => {
    if (!scratch) return;
    await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
    await locator.screenshot({ path: path.join(scratch, 'screenshots', file), animations: 'disabled' });
  };
  const card = page.locator('#auto-tasks-body .auto-task-card', { hasText: 'hostb-confirmed-definition' });
  await shot('auto-task-delete-row-dark-1440-before.png', 1440, card);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.getByRole('button', { name: 'Delete auto-task hostb-confirmed-definition' }).click();
  const confirm = page.locator('.auto-task-delete-confirm');
  await confirm.waitFor({ state: 'visible' });
  await confirm.getByLabel('Reason (optional)').fill('retired from the federated view');
  const confirmText = await confirm.textContent();
  assert(confirmText.includes('hostb-confirmed-definition'), `confirmation names the definition: ${confirmText}`);
  assert(confirmText.includes('schedule state'), `confirmation says schedule state is removed: ${confirmText}`);
  assert(confirmText.includes('will not be reseeded'), `confirmation says a shipped default is not reseeded: ${confirmText}`);
  await shot('auto-task-delete-confirm-dark-1440.png', 1440, confirm);
  await shot('auto-task-delete-confirm-dark-375.png', 375, confirm);
  await page.setViewportSize({ width: 1440, height: 900 });
  await confirm.getByRole('button', { name: 'Delete', exact: true }).click();
  await until('the open-task refusal', () => autoTaskDeletePosts.length === 1);
  const refusal = page.locator('.auto-task-delete-refusal');
  await refusal.waitFor({ state: 'visible' });
  const refusalText = await refusal.textContent();
  assert(refusalText.includes('ORB-9001'), `the refusal names the open task: ${refusalText}`);
  await shot('auto-task-delete-refusal-dark-1440.png', 1440, confirm);
  await shot('auto-task-delete-refusal-dark-375.png', 375, confirm);
  await page.setViewportSize({ width: 1440, height: 900 });
  await refusal.getByRole('button', { name: 'Delete anyway' }).click();
  await until('the forced delete', () => autoTaskDeletePosts.length === 2);
  assert(autoTaskDeletePosts.every((post) => post.host === 'hostb'), `delete posts stay on hostb: ${JSON.stringify(autoTaskDeletePosts)}`);
  assert(autoTaskDeletePosts[0].body.force === false && autoTaskDeletePosts[1].body.force === true, `force is explicit: ${JSON.stringify(autoTaskDeletePosts)}`);
  assert(autoTaskDeletePosts.every((post) => post.body.name === 'hostb-confirmed-definition' && post.body.reason === 'retired from the federated view'), `posts keep the definition and reason: ${JSON.stringify(autoTaskDeletePosts)}`);
  assert(autoTaskDeletePosts.every((post) => post.workspace === 'ws_orbit'), `posts name the selected workspace: ${JSON.stringify(autoTaskDeletePosts)}`);
  const deletePaths = routeLog.filter((entry) => entry.method === 'POST' && entry.path.includes('auto-tasks/delete'));
  assert(deletePaths.length === 2 && deletePaths.every((entry) => entry.path.startsWith('/api/on/hostb/auto-tasks/delete?')), `delete posts to the owning host: ${JSON.stringify(deletePaths)}`);
  await page.locator('#auto-tasks-body', { hasText: 'hostb-confirmed-definition' }).waitFor({ state: 'hidden' });
  await page.close();

  // 8. Keyboard: Tab reaches the picker, with the 2px focus ring.
  await openPage('/', { fresh: true });
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  // Anchor traversal at a focusable element; body.focus() is a no-op and
  // leaves the browser's prior sequential focus position unchanged.
  await page.locator('#skip-link').focus();
  let reached = false;
  for (let index = 0; index < 6 && !reached; index++) {
    await page.keyboard.press('Tab');
    reached = await page.evaluate(() => document.activeElement?.id === 'host-select');
  }
  assert(reached, 'Tab reaches the host picker before the workspace picker and the destinations');
  const ring = await page.locator('#host-select').evaluate((node) => {
    const style = getComputedStyle(node);
    return { visible: node.matches(':focus-visible'), width: style.outlineWidth, style: style.outlineStyle, color: style.outlineColor };
  });
  assert(ring.visible && ring.width === '2px' && ring.style === 'solid' && ring.color === 'rgb(122, 167, 255)', `the picker shows the 2px accent focus ring: ${JSON.stringify(ring)}`);
  const order = await page.evaluate(() => {
    const host = document.getElementById('host-select');
    const workspace = document.getElementById('workspace-select');
    return !!(host.compareDocumentPosition(workspace) & Node.DOCUMENT_POSITION_FOLLOWING);
  });
  assert(order, 'the host picker sits above the workspace picker');
  await page.close();

  // 9. The log tail retires the old host's rows, buffered count and status
  // line at the switch, before the new host's snapshot settles.
  const logEvent = (message) => ({ ts: '2026-10-11T12:00:00Z', source: 'orbit.fixture', target: 'orbit::fixture',
    level: 'info', code: 'OK', message_html: message, agent_stdout: false });
  const pushLog = (host, id, message) => {
    for (const stream of logStreams.filter((entry) => entry.host === host && !entry.res.writableEnded && !entry.res.destroyed)) {
      stream.res.write(`id: ${id}\ndata: ${JSON.stringify(logEvent(message))}\n\n`);
    }
  };
  const settleLog = (host, outcome) => {
    const index = heldLogs.findIndex((entry) => entry.host === host);
    assert(index >= 0, `a held ${host} snapshot to settle`);
    const [{ res }] = heldLogs.splice(index, 1);
    res.writeHead(outcome.status || 200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(outcome.body));
  };
  const logView = () => page.evaluate(() => {
    const buffered = document.getElementById('log-buffered-count');
    return {
      rows: [...document.querySelectorAll('#logInner .log-line')].map((row) => row.querySelector('.m').textContent),
      shown: document.getElementById('log-count').textContent,
      buffered: buffered.style.display === 'none' ? null : buffered.textContent,
      status: document.getElementById('log-statusbar-message').textContent,
      source: document.getElementById('log-statusbar-source').textContent,
      time: document.getElementById('log-statusbar-time').textContent,
      empty: document.getElementById('log-statusbar').classList.contains('empty'),
    };
  });
  const assertRetired = async (label) => {
    const view = await logView();
    assert(view.rows.length === 0 && view.shown === '0', `${label}: no log rows remain: ${JSON.stringify(view)}`);
    assert(view.buffered === null, `${label}: no buffered count remains: ${JSON.stringify(view)}`);
    assert(view.status === '' && view.source === '' && view.time === '' && view.empty, `${label}: the status bar is cleared: ${JSON.stringify(view)}`);
  };
  logSnapshots = {
    [SERVING]: { events: [logEvent('alpha-only-event')], offset: 7, agent_offset: 2, cursor: '7@alpha-op:2@alpha-agent' },
    hostb: { events: [], offset: 3, agent_offset: 1, cursor: '3@hostb-op:1@hostb-agent' },
  };
  logStreams = [];
  await openPage('/', { fresh: true });
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  await until('the alpha stream from its snapshot cursor', () => logStreams.some((entry) => entry.host === SERVING && entry.query === '?from=7&agent_from=2&cursor=7%40alpha-op%3A2%40alpha-agent'));
  let view = await logView();
  assert(view.rows.join() === 'alpha-only-event' && view.status === 'alpha-only-event' && !view.empty, `alpha's snapshot is shown: ${JSON.stringify(view)}`);
  await page.evaluate(() => document.getElementById('log-follow-tail').click());
  pushLog(SERVING, '8:2', 'alpha-stream-event');
  await until('alpha buffers its paused event', async () => (await logView()).buffered === '1 buffered');

  logHolds.add('hostb');
  routeLog = [];
  await switchHost('hostb');
  await until('the held hostb snapshot', () => heldLogs.some((entry) => entry.host === 'hostb'));
  await assertRetired('a held hostb snapshot');
  await capture('host-log-held-1440');

  settleLog('hostb', { status: 500, body: { error: 'log read failed' } });
  await until('the failed hostb snapshot', async () => (await page.locator('#log-statusbar .sb-label').textContent()) === 'log stream unavailable, retrying');
  await assertRetired('a failed hostb snapshot');
  await until('the hostb snapshot retry', () => heldLogs.some((entry) => entry.host === 'hostb'), 5000);

  // Stale responses: a round trip through alpha settles the retried hostb
  // snapshot on alpha and alpha's snapshot back on hostb; neither may paint.
  // (Chromium's cache lock queues a second request for one URL behind the
  // first, so each stale request settles before its URL is asked again.)
  logHolds.add(SERVING);
  await switchHost('');
  await until('the held alpha snapshot', () => heldLogs.some((entry) => entry.host === SERVING));
  settleLog('hostb', { body: { events: [logEvent('stale-hostb-event')], offset: 5, agent_offset: 5 } });
  await page.waitForTimeout(300);
  await assertRetired('a stale hostb snapshot settling on alpha');
  await switchHost('hostb');
  await until('the next hostb snapshot', () => heldLogs.some((entry) => entry.host === 'hostb'));
  settleLog(SERVING, { body: logSnapshots[SERVING] });
  await page.waitForTimeout(300);
  await assertRetired('a stale alpha snapshot settling on hostb');

  settleLog('hostb', { body: logSnapshots.hostb });
  await until('the hostb stream from its own cursor', () => logStreams.some((entry) => entry.host === 'hostb' && entry.query === '?from=3&agent_from=1&cursor=3%40hostb-op%3A1%40hostb-agent'));
  await assertRetired('an empty hostb snapshot');
  const streamsSinceSwitch = routeLog.filter((entry) => entry.path.includes('/log/stream'));
  assert(streamsSinceSwitch.every((entry) => entry.path === '/api/on/hostb/log/stream?from=3&agent_from=1&cursor=3%40hostb-op%3A1%40hostb-agent'),
    `only hostb's stream opens, from hostb's cursor: ${JSON.stringify(streamsSinceSwitch)}`);
  logHolds.clear();

  pushLog('hostb', '4:1', 'hostb-stream-event');
  await until('hostb buffers its own event', async () => (await logView()).buffered === '1 buffered');
  view = await logView();
  assert(view.status === 'hostb-stream-event' && !view.empty, `the status bar follows hostb: ${JSON.stringify(view)}`);
  await page.evaluate(() => document.getElementById('log-follow-tail').click());
  view = await logView();
  assert(view.rows.join() === 'hostb-stream-event' && view.buffered === null, `resuming shows only hostb's line: ${JSON.stringify(view)}`);
  evidenceLog.logRoutes = routeLog.filter((entry) => entry.path.includes('/log'));
  await page.close();

  // 10. Back and Forward restore the host each entry names, through the host
  // switch: API routing, both pickers, write capability and the host's
  // workspaces follow the address, and an entry without a host is the serving
  // host. A host switch rewrites its own entry, so the entry before it keeps
  // the earlier host.
  const scopeNow = () => page.evaluate(async () => {
    const { getHost, getWorkspace, isAggregateView, isAggregateLinked, withHost, withWorkspace } = await import('/static/js/common.js');
    const query = new URL(window.location.href).searchParams;
    const workspaceSelect = document.getElementById('workspace-select');
    return {
      host: getHost(),
      workspace: getWorkspace(),
      aggregate: isAggregateView(),
      aggregateLinked: isAggregateLinked(),
      read: withHost('/api/tasks'),
      write: withWorkspace('/api/tasks/x/approve'),
      addressHost: query.get('host'),
      addressWorkspace: query.get('workspace'),
      hostPicker: document.getElementById('host-select').value,
      workspacePicker: workspaceSelect ? workspaceSelect.value : null,
      workspaces: workspaceSelect ? [...workspaceSelect.options].map((option) => option.textContent).join(',') : null,
      readOnly: !document.getElementById('host-read-only').hidden,
    };
  });
  const traverse = async (step, label, predicate) => {
    await page.evaluate((step) => history.go(step), step);
    await until(label, async () => predicate(await scopeNow()));
    return scopeNow();
  };
  // Rail clicks write each route's settled hash, so traversing back to one
  // routes it without pushing an entry that would drop the Forward history.
  const openRuns = async () => {
    await page.click('.tab[data-tab="jobs"]');
    await page.click('#jobs-subtabs [data-subtab="history"]');
    await page.locator('#jobs-history-main').waitFor({ state: 'visible' });
  };
  const openTasks = () => page.click('.tab[data-tab="tasks"]');
  const ALPHA_WORKSPACES = 'All workspaces,orbit,alpha-only';
  const HOSTB_WORKSPACES = 'All workspaces,orbit,b-only';

  // A workspace id both hosts list: only the host changes between the entries.
  await openPage('/?workspace=ws_orbit', { fresh: true });
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  await openRuns();
  await openTasks();
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  await switchHost('hostb');
  await taskRow('hostb-RUN').waitFor({ state: 'visible' });
  await until('hostb keeps ws_orbit', async () => {
    const scope = await scopeNow();
    return scope.workspaces === HOSTB_WORKSPACES && scope.workspacePicker === 'ws_orbit';
  });
  routeLog = [];
  let scope = await traverse(-1, 'Back on the serving host', (now) => now.host === null && now.workspaces === ALPHA_WORKSPACES);
  assert(scope.addressHost === null && scope.hostPicker === '' && scope.read === '/api/tasks'
    && scope.write === '/api/tasks/x/approve?workspace=ws_orbit', `Back restores the serving host's routing and picker: ${JSON.stringify(scope)}`);
  assert(scope.workspace === 'ws_orbit' && scope.workspacePicker === 'ws_orbit' && scope.addressWorkspace === 'ws_orbit',
    `Back keeps the shared workspace: ${JSON.stringify(scope)}`);
  await assertNamed(`${SERVING} (serving host)`);
  await until('serving reads after Back', () => logged(/^GET \/api\/workspaces(\?|$)/));
  evidenceLog.historySharedWorkspaceBack = routeLog;
  routeLog = [];
  scope = await traverse(1, 'Forward on hostb', (now) => now.host === 'hostb' && now.workspaces === HOSTB_WORKSPACES);
  assert(scope.addressHost === 'hostb' && scope.hostPicker === 'hostb' && scope.read === '/api/on/hostb/tasks'
    && scope.workspacePicker === 'ws_orbit', `Forward restores hostb: ${JSON.stringify(scope)}`);
  await assertNamed('hostb');
  await taskRow('hostb-RUN').waitFor({ state: 'visible' });
  await until('hostb connection read after Forward', () => logged(/^GET \/api\/hosts\/hostb\/connection(\?|$)/));
  // Writes follow the restored address too.
  await taskRow('hostb-NEW').locator('.task-quick.approve').click();
  await until('the approve action on hostb after Forward', () => logged(/^POST \/api\/on\/hostb\/tasks\/hostb-NEW\/approve\?workspace=ws_orbit$/));
  await traverse(-1, 'Back on the serving host again', (now) => now.host === null && now.workspaces === ALPHA_WORKSPACES);
  await openTasks();
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  routeLog = [];
  await taskRow('alpha-NEW').locator('.task-quick.approve').click();
  await until('the approve action on the serving host after Back', () => logged(/^POST \/api\/tasks\/alpha-NEW\/approve\?workspace=ws_orbit$/));
  assert(!routeLog.some((entry) => entry.method !== 'GET' && entry.path.startsWith('/api/on/')), `no write leaves for hostb after Back: ${JSON.stringify(routeLog)}`);
  await page.close();

  // Different workspace ids: the host and the workspace change together, and
  // hostb's refused writes come and go with it.
  forwardWrites = { authorized: false, reason: READ_ONLY_REASON };
  await openPage('/?workspace=ws_alpha', { fresh: true });
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  await openRuns();
  await openTasks();
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  await switchHost('hostb');
  await until('hostb on its default workspace', async () => {
    const now = await scopeNow();
    return now.workspacePicker === 'ws_b' && now.addressWorkspace === 'ws_b' && now.readOnly;
  });
  scope = await traverse(-1, 'Back on the serving host in ws_alpha', (now) => now.host === null && now.workspaces === ALPHA_WORKSPACES);
  assert(scope.workspace === 'ws_alpha' && scope.workspacePicker === 'ws_alpha' && scope.addressWorkspace === 'ws_alpha' && scope.addressHost === null,
    `Back restores the serving host and ws_alpha: ${JSON.stringify(scope)}`);
  assert(scope.write === '/api/tasks/x/approve?workspace=ws_alpha' && scope.hostPicker === '' && !scope.readOnly,
    `Back restores serving routing and write capability: ${JSON.stringify(scope)}`);
  scope = await traverse(1, 'Forward on hostb in ws_b', (now) => now.host === 'hostb' && now.workspaces === HOSTB_WORKSPACES && now.readOnly);
  assert(scope.workspace === 'ws_b' && scope.workspacePicker === 'ws_b' && scope.addressHost === 'hostb' && scope.hostPicker === 'hostb'
    && scope.write === '/api/on/hostb/tasks/x/approve?workspace=ws_b', `Forward restores hostb, ws_b and its refusal: ${JSON.stringify(scope)}`);
  await assertNamed('hostb');
  await taskRow('hostb-NEW').waitFor({ state: 'visible' });
  assert(await page.locator('.task-quick.approve').first().isDisabled(), 'hostb writes are disabled again after Forward');

  // A retired visit: hostb's workspace list is still loading when Back leaves
  // it. The serving host loads without waiting for it, and when it settles it
  // repaints neither picker nor the address.
  await traverse(-1, 'Back on the serving host before the hold', (now) => now.host === null && now.workspaces === ALPHA_WORKSPACES);
  workspaceHolds.add('hostb');
  await page.evaluate(() => history.forward());
  await until('the held hostb workspace list', () => heldWorkspaces.some((entry) => entry.host === 'hostb'));
  routeLog = [];
  await traverse(-1, 'Back on the serving host while hostb is held', (now) => now.host === null && now.workspacePicker === 'ws_alpha' && now.workspaces === ALPHA_WORKSPACES);
  await until('serving reads while hostb is held', () => logged(/^GET \/api\/(?!on\/|hosts)[^?]+\?(.*&)?workspace=ws_alpha(&|$)/));
  workspaceHolds.clear();
  for (const { res } of heldWorkspaces.splice(0)) {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(WORKSPACES.hostb));
  }
  await page.waitForTimeout(300);
  scope = await scopeNow();
  assert(scope.host === null && scope.workspace === 'ws_alpha' && scope.workspacePicker === 'ws_alpha' && scope.workspaces === ALPHA_WORKSPACES
    && scope.addressWorkspace === 'ws_alpha' && scope.addressHost === null && !scope.readOnly,
  `hostb's retired workspace list does not repaint the serving host: ${JSON.stringify(scope)}`);
  await assertNamed(`${SERVING} (serving host)`);
  forwardWrites = { authorized: true, reason: null };
  await page.close();

  // 11. Aggregate history must survive leaving a single-workspace host.
  // Hold destination discovery to check that the URL's intent is kept before
  // eligibility is known and no default-workspace panel request slips through.
  const settleWorkspaces = (host) => {
    workspaceHolds.delete(host);
    const index = heldWorkspaces.findIndex((entry) => entry.host === host);
    assert(index >= 0, `a held ${host} workspace list to settle`);
    const [{ res }] = heldWorkspaces.splice(index, 1);
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(WORKSPACES[host]));
  };
  const assertAggregate = async (label, endpoint) => {
    const now = await scopeNow();
    assert(now.host === 'hostb' && now.hostPicker === 'hostb' && now.addressHost === 'hostb'
      && now.workspace === null && now.aggregate && now.aggregateLinked
      && now.workspacePicker === '' && now.workspaces === HOSTB_WORKSPACES && now.addressWorkspace === 'all',
    `${label} restores aggregate state, both pickers and the URL: ${JSON.stringify(now)}`);
    await until(`${label} aggregate panel request`, () => routeLog.some((entry) => entry.method === 'GET'
      && entry.path.startsWith(`/api/on/hostb/${endpoint}?`)));
    assert(!logged(/^GET \/api\/on\/hostb\/(?:tasks|job-runs|crews|audit\/summary)\?/),
      `${label} sends no concrete-workspace panel request: ${JSON.stringify(routeLog)}`);
    return { scope: now, routes: routeLog.slice() };
  };
  const restoreAggregate = async (step, label, endpoint) => {
    workspaceHolds.add('hostb');
    routeLog = [];
    await page.evaluate((step) => history.go(step), step);
    await until(`${label} held destination list`, () => heldWorkspaces.some((entry) => entry.host === 'hostb'));
    const pending = await scopeNow();
    assert(pending.host === 'hostb' && pending.workspace === null && pending.aggregateLinked && pending.addressWorkspace === 'all',
      `${label} retains aggregate intent during discovery: ${JSON.stringify(pending)}`);
    assert(!logged(/^GET \/api\/on\/hostb\/(?:tasks|job-runs|crews|audit\/summary)(?:[/?]|$)/),
      `${label} waits for destination discovery before fetching panels: ${JSON.stringify(routeLog)}`);
    settleWorkspaces('hostb');
    await until(`${label} aggregate picker`, async () => {
      const now = await scopeNow();
      return now.aggregate && now.workspacePicker === '' && now.workspaces === HOSTB_WORKSPACES;
    });
    return assertAggregate(label, endpoint);
  };
  await openPage('/?host=hostb&workspace=all#tasks', { fresh: true });
  await until('initial hostb aggregate picker', async () => (await scopeNow()).aggregate);
  await openRuns();
  await openTasks();
  await switchHost('skewhost');
  await taskRow('skewhost-RUN').waitFor({ state: 'visible' });
  scope = await scopeNow();
  assert(scope.workspace === 'ws_skew' && !scope.aggregate && scope.workspacePicker === null && scope.addressWorkspace === 'ws_skew',
    `the single-workspace destination resolves aggregate intent to its only workspace: ${JSON.stringify(scope)}`);
  evidenceLog.historyAggregateBack = await restoreAggregate(-1, 'Back from a single-workspace host', 'job-runs/all');
  await traverse(1, 'Forward to the single-workspace host', (now) => now.host === 'skewhost' && now.workspace === 'ws_skew' && !now.aggregate);
  await taskRow('skewhost-RUN').waitFor({ state: 'visible' });
  await page.close();

  // Reverse the history order so Forward also enters an aggregate host from
  // a single-workspace host, instead of merely reusing the Back case.
  await openPage('/?host=skewhost&workspace=all#tasks', { fresh: true });
  await taskRow('skewhost-RUN').waitFor({ state: 'visible' });
  scope = await scopeNow();
  assert(scope.workspace === 'ws_skew' && !scope.aggregateLinked && scope.addressWorkspace === 'ws_skew',
    `an explicit aggregate link on a single-workspace host repairs its URL: ${JSON.stringify(scope)}`);
  await openRuns();
  await openTasks();
  await switchHost('hostb');
  await until('hostb default picker before choosing aggregate', async () => (await scopeNow()).workspacePicker === 'ws_b');
  await page.locator('#workspace-select').selectOption('');
  await until('hostb aggregate before Back', async () => (await scopeNow()).aggregate);
  routeLog = [];
  await traverse(-1, 'Back to the single-workspace host', (now) => now.host === 'skewhost' && now.workspace === 'ws_skew' && !now.aggregate);
  await until('the single-workspace Runs request after Back', () => logged(/^GET \/api\/on\/skewhost\/job-runs\?.*workspace=ws_skew/));
  evidenceLog.historyAggregateForward = await restoreAggregate(1, 'Forward from a single-workspace host', 'tasks/all');
  await page.close();

  // Settings publishes externally refreshed host-file rows to every reader.
  await openPage('/#config/hosts', { fresh: true });
  await page.locator('#config-body').waitFor({ state: 'visible' });
  await page.locator('.host-add-open').click();
  await page.locator('.host-input-ssh').fill('added-ssh');
  await page.locator('.host-input-name').fill('added-host');
  await page.locator('.host-editor .host-submit').click();
  await until('added host published', async () => await page.locator('#host-select option[value="added-host"]').count() === 1);
  assert(await page.evaluate(async () => !!(await import('/static/js/common.js')).findRegisteredHost('added-host')), 'new host appears in shared registry');
  await page.getByRole('button', { name: 'Rename added-host' }).click();
  await page.locator('.host-input-name').fill('renamed-host');
  await page.locator('.host-editor .host-submit').click();
  await until('renamed host published', async () => await page.locator('#host-select option[value="renamed-host"]').count() === 1);
  assert(await page.locator('#host-select option[value="added-host"]').count() === 0, 'old name leaves the picker');
  WORKSPACES['renamed-host'] = WORKSPACES.alpha;
  await page.getByRole('link', { name: 'Show renamed-host' }).click();
  await until('Show selects renamed host', async () => (await page.locator('#host-select').inputValue()) === 'renamed-host');
  WORKSPACES['selected-host'] = WORKSPACES.alpha;
  await page.getByRole('button', { name: 'Rename renamed-host' }).click();
  await page.locator('.host-input-name').fill('selected-host');
  await page.locator('.host-editor .host-submit').click();
  await until('selected rename follows its machine', async () => (await page.locator('#host-select').inputValue()) === 'selected-host');
  await page.getByRole('button', { name: 'Remove selected-host' }).click();
  await page.locator('.host-remove-confirm .host-confirm').click();
  await until('removed host published', async () => await page.evaluate(async () => !(await import('/static/js/common.js')).findRegisteredHost('selected-host')));
  assert((await page.locator('#host-select option[value="selected-host"]').textContent()).includes('not registered'), 'removed selected host is identified as unregistered');
  // A later external registry refresh also reaches the picker.
  HOSTS.hosts.push(hostRow('external-host', 'hm_external'));
  await page.evaluate(async () => (await import('/static/js/hosts.js')).fetchAndRenderHosts({ probe: false }));
  assert(await page.locator('#host-select option[value="external-host"]').count() === 1, 'external addition reaches picker');
  await page.close();
  // Fleet, execution location, host resources and the checkout-owner hint
  // read the same registry Settings just published.
  const localHost = HOSTS.hosts.find((host) => host.local);
  localHost.workspaces = [{ id: 'ws_orbit', role: 'replica', owner_machine_id: 'hm_external' }];
  hideTasks = true;
  await openPage('/?workspace=ws_orbit#tasks', { fresh: true });
  await until('replica owner hint', async () => (await page.locator('#tasks-body').textContent()).includes('pull replica of external-host'));
  assert(await page.getByRole('button', { name: 'Show on external-host' }).count() === 1, 'the checkout-owner hint names the refreshed owner');
  const shared = await page.evaluate(async () => {
    const common = await import('/static/js/common.js');
    const distributed = await import('/static/js/distributed.js');
    const fleet = await import('/static/js/fleet-drains.js');
    const added = common.findRegisteredHost('external-host');
    const located = distributed.registeredMachineHost({ known: true, machine_id: added.machine_id, machine_name: 'stale-name' });
    const drains = await fleet.fetchFleetDrains({ force: true });
    return {
      distributed: located && located.name,
      fleet: drains.items.some((row) => row.host === 'external-host'),
      registry: common.getRegisteredHosts().map((row) => row.name),
    };
  });
  assert(shared.distributed === 'external-host', 'execution location follows the refreshed registry');
  assert(shared.fleet, 'fleet membership includes the refreshed host');
  assert(shared.registry.includes('external-host') && shared.registry.includes('alpha'), 'shared registry lists the serving host and the addition');
  WORKSPACES['external-host'] = WORKSPACES.alpha;
  const resources = await page.evaluate(async () => {
    const common = await import('/static/js/common.js');
    const hostResources = await import('/static/js/host-resources.js');
    common.setHost('external-host');
    await hostResources.fetchHostResourcePayload();
    const row = common.findRegisteredHost(common.getHost());
    return { name: row && row.name, machine_id: row && row.machine_id, ssh: row && row.ssh };
  });
  assert(resources.name === 'external-host' && resources.machine_id === 'hm_external' && resources.ssh === 'external-host-ssh', 'host resources resolve the refreshed host');
  assert(logged(/^GET \/api\/on\/external-host\/host\/resources$/), 'host resources read the refreshed host');
  localHost.workspaces = [];
  hideTasks = false;
  delete WORKSPACES['external-host'];
  HOSTS.hosts.pop();
  await page.close();

  // The serving host's Show href, opened in a new tab of a browser that
  // remembers a remote, selects the serving host there; an ordinary click
  // on the same link switches in page to the same destination.
  await openPage('/?workspace=ws_orbit#tasks', { fresh: true });
  await taskRow('alpha-RUN').waitFor({ state: 'visible' });
  await switchHost('hostb');
  await taskRow('hostb-RUN').waitFor({ state: 'visible' });
  assert(await page.evaluate(() => localStorage.getItem('orbit.dashboard.host')) === 'hostb', 'hostb is the remembered remote');
  await page.evaluate(() => { window.location.hash = 'config/hosts'; });
  const servingShow = page.getByRole('link', { name: `Show ${SERVING}` });
  await servingShow.waitFor({ state: 'visible' });
  const servingHref = await servingShow.getAttribute('href');
  assert(new URL(servingHref, origin).searchParams.has('host') && new URL(servingHref, origin).searchParams.get('host') === '',
    `serving Show href carries an explicit empty host: ${servingHref}`);
  const tab2 = await context.newPage();
  tab2.on('pageerror', (error) => pageErrors.push(`serving Show tab: ${error.message}`));
  await tab2.goto(new URL(servingHref, origin).href);
  await tab2.locator('#host-select').waitFor({ state: 'attached' });
  await until('serving Show tab selects the serving host', async () => ['', null].includes(await tab2.evaluate(() => new URL(window.location.href).searchParams.get('host')))
    && await tab2.locator('#host-select').inputValue() === '');
  assert(await tab2.evaluate(() => localStorage.getItem('orbit.dashboard.host')) === 'hostb', 'the new tab did not rewrite the remembered remote in the original tab');
  await tab2.close();
  await servingShow.click();
  await until('ordinary Show click selects the serving host', async () => (await selectedHost()) === null && await page.locator('#host-select').inputValue() === '');
  evidenceLog.servingShowHref = servingHref;
  await page.close();

  // A host with a live drain, then a switch while that host's readiness is
  // still in flight: the card and the Start form belong to the host on screen,
  // and the late response does not paint the host that was left.
  const drainingAlphaReadiness = () => ({
    controls_authorized: true,
    approvals: { enabled: true, approved_total: 4, held_total: 1, held_by_reason: { warnings: 1 } },
    tasks: [],
    capacity: {
      active_leaf_runs: 9,
      max_active_leaf_runs: 3,
      free_slots: 0,
      drain_phase: 'draining',
      drain_run_id: 'jrun-20261101-alpha',
      drain_status_run_id: 'jrun-20261101-alpha',
      ends_at: new Date(Date.now() + 45 * 60 * 1000).toISOString(),
      admitted_workers: 5,
      running_admitted_workers: 2,
      last_admission_forecast: {
        mode: 'enforce',
        forecast_at: '2026-11-01T00:00:00Z',
        drain_live: false,
        admitted: [],
        skipped: [],
      },
    },
  });
  drainReadinessOverride = (host) => (host === SERVING ? drainingAlphaReadiness() : null);
  await openPage('/?workspace=ws_orbit#tasks?dock=drain', { fresh: true });
  await until('alpha drain is live', async () => {
    const text = await page.locator('#auto-drain-body').innerText();
    return text.includes('9/3 slots') && await page.locator('#auto-drain-body .drain-stop').count() > 0
      && text.includes('Last forecast') && await page.locator('#auto-drain-live').count() > 0;
  });
  const toggles = ['auto-drain-complete', 'auto-drain-approve', 'auto-drain-auto-fix', 'auto-drain-review'];
  await page.locator('[data-drain-focus="duration-4h"]').evaluate((node) => node.click());
  await until('4h selected', async () => (await page.locator('[data-drain-focus="duration-4h"]').getAttribute('aria-pressed')) === 'true');
  for (const id of toggles) {
    await page.locator(`#${id}`).evaluate((node) => node.click());
    await until(`${id} checked`, async () => page.locator(`#${id}`).isChecked());
  }
  await page.locator('#auto-drain-concurrency').evaluate((node) => {
    node.value = '7';
    node.dispatchEvent(new Event('input', { bubbles: true }));
  });
  await until('concurrency remembered on alpha', async () => (await page.locator('#auto-drain-concurrency').inputValue()) === '7');
  await captureDrainPanel('drain-panel-dark-1440-before.png', 1440);
  await captureDrainPanel('drain-panel-dark-375-before.png', 375);
  await page.setViewportSize({ width: 1440, height: 900 });
  readinessHolds.add(SERVING);
  readinessHolds.add('hostb');
  await page.locator('#refresh-btn').click();
  await until('the held alpha readiness', () => heldReadiness.some((entry) => entry.host === SERVING));
  await switchHost('hostb');
  const drainCardSnapshot = () => page.evaluate(() => {
    const body = document.getElementById('auto-drain-body');
    const panel = document.getElementById('auto-drain-panel');
    return {
      body: body ? body.innerText : '',
      when: document.getElementById('auto-drain-when')?.textContent || '',
      phase: document.getElementById('auto-drain-phase-label')?.textContent || '',
      stop: body ? body.querySelectorAll('.drain-stop').length : 0,
      live: document.getElementById('auto-drain-live') ? 1 : 0,
      alphaTitle: panel ? panel.querySelectorAll('[title*="jrun-20261101-alpha"]').length : 0,
    };
  });
  const assertDrainClear = (snap, when) => {
    assert(!snap.body.includes('9/3 slots'), `${when}: slots carried over: ${snap.body.slice(0, 300)}`);
    assert(snap.stop === 0, `${when}: Stop is still shown`);
    assert(snap.live === 0, `${when}: live counts are still shown`);
    assert(!snap.body.includes('Last forecast'), `${when}: forecast carried over: ${snap.body.slice(0, 300)}`);
    assert(!snap.body.includes('jrun-20261101-alpha') && !snap.body.includes('jrun-…alpha'), `${when}: run id carried over: ${snap.body.slice(0, 300)}`);
    assert(snap.alphaTitle === 0, `${when}: run title carried over`);
    assert(snap.when.trim() === '', `${when}: time left carried over (${snap.when})`);
    assert(!snap.phase.includes('Draining'), `${when}: phase is ${snap.phase}`);
  };
  assertDrainClear(await drainCardSnapshot(), 'immediately after the switch');
  await until('the held hostb readiness', () => heldReadiness.some((entry) => entry.host === 'hostb'));
  const releasedAlpha = releaseHeldReadiness(SERVING);
  assert(releasedAlpha >= 1, `the first host's readiness was still in flight: ${releasedAlpha}`);
  await page.waitForTimeout(500);
  assertDrainClear(await drainCardSnapshot(), 'after the late alpha readiness');
  readinessHolds.delete('hostb');
  const releasedHostb = releaseHeldReadiness('hostb');
  assert(releasedHostb >= 1, `hostb readiness was held across the switch: ${releasedHostb}`);
  await until('hostb drain is idle', async () => (await page.locator('#auto-drain-phase-label').innerText()).trim() === 'Idle');
  await page.locator('#auto-drain-quick-start').waitFor({ state: 'visible' });
  assert((await page.locator('[data-drain-focus="duration-1h"]').getAttribute('aria-pressed')) === 'true', 'hostb window length is its own default');
  assert((await page.locator('[data-drain-focus="duration-4h"]').getAttribute('aria-pressed')) === 'false', 'hostb does not keep alpha\'s window length');
  assert((await page.locator('#auto-drain-concurrency').inputValue()) === '', 'hostb parallel tasks is blank');
  for (const id of toggles) {
    assert(!(await page.locator(`#${id}`).isChecked()), `${id} is off on hostb`);
  }
  const hostbReads = routeLog.filter((entry) => entry.method === 'GET' && entry.path.includes('/api/on/hostb/workflows/auto/readiness'));
  assert(hostbReads.length >= 1 && hostbReads.every((entry) => !entry.path.includes('concurrency=7')), `hostb readiness uses the selected host's form: ${JSON.stringify(hostbReads)}`);
  assertDrainClear(await drainCardSnapshot(), 'after hostb readiness');
  await captureDrainPanel('drain-panel-dark-1440.png', 1440);
  await captureDrainPanel('drain-panel-dark-375.png', 375);
  await page.setViewportSize({ width: 1440, height: 900 });
  let startConfirm = '';
  page.once('dialog', async (dialog) => {
    startConfirm = dialog.message();
    await dialog.accept();
  });
  await page.locator('#auto-drain-quick-start .drain-start').click();
  await until('start posts to hostb', () => drainPosts.some((entry) => entry.host === 'hostb' && entry.rest === 'workflows/auto'));
  assert(startConfirm.includes('Duration: 1h') && !startConfirm.includes('4h'), `start confirms the selected host's window: ${startConfirm}`);
  const startPost = drainPosts.find((entry) => entry.host === 'hostb' && entry.rest === 'workflows/auto');
  assert(startPost.body?.for_duration === '1h' && startPost.body?.concurrency == null
    && startPost.body?.complete === false && startPost.body?.approve_proposed === false
    && startPost.body?.auto_fix === false && !startPost.body?.review, `start posts hostb's form: ${JSON.stringify(startPost)}`);
  const startRoute = routeLog.filter((entry) => entry.method === 'POST' && entry.path.includes('workflows/auto') && !entry.path.includes('/stop'));
  assert(startRoute.length === 1 && startRoute[0].path.startsWith('/api/on/hostb/workflows/auto'), `start targets hostb: ${JSON.stringify(startRoute)}`);
  readinessHolds.clear();
  await switchHost('');
  await until('the serving host\'s Stop returns', async () => await page.locator('#auto-drain-body .drain-stop').count() > 0);
  page.once('dialog', async (dialog) => { await dialog.accept(); });
  await page.locator('#auto-drain-body .drain-stop').click();
  await until('stop posts to the serving host', () => drainPosts.some((entry) => entry.host === SERVING && entry.rest === 'workflows/auto/stop'));
  assert(!drainPosts.some((entry) => entry.host !== SERVING && entry.rest === 'workflows/auto/stop'), `stop stays on the selected host: ${JSON.stringify(drainPosts)}`);
  const stopRoute = routeLog.filter((entry) => entry.method === 'POST' && entry.path.includes('workflows/auto/stop'));
  assert(stopRoute.some((entry) => entry.path.startsWith('/api/workflows/auto/stop')) && !stopRoute.some((entry) => entry.path.includes('/api/on/')), `stop targets the serving host: ${JSON.stringify(stopRoute)}`);
  evidenceLog.drainHostSwitch = { start: startPost.body, stopHost: SERVING, releasedAlpha, releasedHostb };
  drainReadinessOverride = null;
  readinessHolds.clear();
  for (const host of [...new Set(heldReadiness.map((entry) => entry.host))]) releaseHeldReadiness(host);
  drainPosts = [];
  await page.close();

  if (pageErrors.length) throw new Error(`page errors: ${pageErrors.join(' | ')}`);
  fs.writeFileSync(path.join(evidence, 'host-switch-assertions.json'), `${JSON.stringify({
    assertions: [
      'browser-settings-bulk-delete-retires-at-host-switch',
      'browser-host-picker-lists-serving-host-first',
      'browser-every-request-follows-the-selected-host',
      'browser-host-url-and-memory-precedence',
      'browser-native-serving-host-task-and-run-links-override-remembered-remote',
      'browser-workspaces-rebuild-per-host',
      'browser-host-level-failure-and-way-back',
      'browser-host-skew-banner',
      'browser-refreshed-registry-reaches-shared-consumers',
      'browser-remote-run-links',
      'browser-remote-writes-disabled-with-reason',
      'browser-read-only-host-still-lists-jobs-and-auto-tasks',
      'browser-host-picker-keyboard-and-focus',
      'browser-log-tail-retires-the-old-host-at-the-switch',
      'browser-history-restores-the-host-each-entry-names',
      'browser-history-restores-aggregate-after-a-single-workspace-host',
      'browser-serving-show-href-selects-serving-in-new-tab',
      'browser-drain-card-and-form-follow-the-selected-host',
    ],
    viewports: ['1440', '768'],
    ...evidenceLog,
  }, null, 2)}\n`);
  console.log('dashboard host switcher verified in a real browser');
} catch (error) {
  if (page && !page.isClosed()) {
    try { await capture('failure'); } catch (_) {}
    try { fs.writeFileSync(path.join(evidence, 'failure.html'), await page.content()); } catch (_) {}
  }
  fs.writeFileSync(path.join(evidence, 'failure-routes.json'), JSON.stringify(routeLog, null, 1));
  console.error(error);
  if (pageErrors.length) console.error(`page errors: ${pageErrors.join(' | ')}`);
  process.exitCode = 1;
} finally {
  if (browser) await browser.close();
  for (const socket of sockets) socket.destroy();
  server.close();
}
