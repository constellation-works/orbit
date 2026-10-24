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

// One host's own dashboard API, as the forward would relay it.
function hostApi(host, req, rest, url, res) {
  const json = (body, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
  const workspaces = WORKSPACES[host] || [];
  const workspace = url.searchParams.get('workspace') || (workspaces.find((ws) => ws.is_default) || {}).id;
  if (req.method !== 'GET') {
    const approved = rest.match(/^tasks\/([^/]+)\/approve$/);
    if (approved) return json({ ...tasksFor(host, workspace).find((task) => task.id === approved[1]), status: 'backlog' });
    return json({ ok: true });
  }
  if (rest === 'workspaces') {
    if (workspaceHolds.has(host)) { heldWorkspaces.push({ host, res }); return; }
    return json(workspaces);
  }
  if (rest === 'tasks') return json({ items: tasksFor(host, workspace), total: 4, limit: 50, truncated: false });
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
  if (rest === 'workflows/auto/readiness') return json({ controls_authorized: true, tasks: [], capacity: { active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4 } });
  if (rest === 'routines') return json({ machine_name: host, capabilities: {}, clock: {}, routines: [] });
  const run = rest.match(/^runs\/([^/]+)$/);
  if (run) return json({ run_id: decodeURIComponent(run[1]), job_name: 'auto', state: 'running', steps: [], workspace_id: workspace });
  if (rest.startsWith('runs/')) return json([]);
  if (rest === 'audit/summary') return json({ window: '24h', events: 0, failed_runs: 0 });
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

  // 1. Picker order, the switch, and every request after it.
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
  assert((await taskRow('alpha-LOCAL').locator('.task-quick-cell .exec-origin').textContent()) === 'on laptop', 'an unregistered machine keeps its text');
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
  assert(!routeLog.some((entry) => entry.method !== 'GET' && entry.path.startsWith('/api/on/')), 'no write is sent to a read-only host');
  await capture('host-read-only-1440');
  forwardWrites = { authorized: true, reason: null };
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

  if (pageErrors.length) throw new Error(`page errors: ${pageErrors.join(' | ')}`);
  fs.writeFileSync(path.join(evidence, 'host-switch-assertions.json'), `${JSON.stringify({
    assertions: [
      'browser-host-picker-lists-serving-host-first',
      'browser-every-request-follows-the-selected-host',
      'browser-host-url-and-memory-precedence',
      'browser-native-serving-host-task-and-run-links-override-remembered-remote',
      'browser-workspaces-rebuild-per-host',
      'browser-host-level-failure-and-way-back',
      'browser-host-skew-banner',
      'browser-remote-run-links',
      'browser-remote-writes-disabled-with-reason',
      'browser-host-picker-keyboard-and-focus',
      'browser-log-tail-retires-the-old-host-at-the-switch',
      'browser-history-restores-the-host-each-entry-names',
      'browser-history-restores-aggregate-after-a-single-workspace-host',
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
