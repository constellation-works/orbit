// Usage: node dashboard_doctor_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
//
// Health › Doctor in Chromium. The shipped app runs against a disposable
// fixture server that answers `/api/doctor` for the serving host and, through
// `/api/on/<host>/…`, for a current remote and for an older remote whose
// dashboard has no such route. It checks the rows (full message, remediation
// with copyable commands), the cached report's age, that Refresh runs doctor
// again while the dashboard's own poll never does, the Health rail count, and
// that no request carries anything but the read-only parameters. No Orbit
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
    hostRow('hostb', 'hm_b'),
    hostRow('oldhost', 'hm_old'),
  ],
};
const WORKSPACES = {
  alpha: [{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true }],
  hostb: [{ id: 'ws_b', name: 'b-only', status: 'active', is_default: true }],
  oldhost: [{ id: 'ws_old', name: 'old', status: 'active', is_default: true }],
};

const LONG_MESSAGE = '2 lock file(s) with dead holder records: '
  + '/srv/orbit/repo/.orbit/state/.task-store-write.lock (dead pid 48213, op: task store write, since 2026-10-08T04:11:52Z); '
  + '/srv/orbit/repo/.orbit/state/.dispatch-admission.lock (dead pid 48219, op: dispatch admission, since 2026-10-08T04:12:03Z)';
const ROWS = (host) => [
  { check: 'config', duration_ms: 3, status: 'ok', message: `valid (/home/fixture/.orbit/config.toml)`, remediation: null },
  { check: 'database', duration_ms: 1, status: 'ok', message: 'database readable (integrity scan: orbit doctor --deep); schema version 41 matches this binary', remediation: null },
  { check: 'stale-locks', duration_ms: 2, status: 'warning', message: LONG_MESSAGE, remediation: 'Run `orbit doctor --fix-stale-locks`.' },
  { check: 'validation-env', duration_ms: 0, status: 'skipped', message: 'no `workflow.required_validation_commands`; required validation does not run here', remediation: null },
  { check: 'state-directory-permissions', duration_ms: 5292, status: 'warning', message: `2 Orbit state directories are group/world writable under: /home/fixture/.orbit (1 writable), /srv/${host}/repo/.orbit (1 writable)`, remediation: 'Remove group/world write permission from the writable directories under each named root (for example, `chmod go-w <directory>`), excluding run worktrees and target trees, then rerun `orbit doctor`.' },
  { check: 'provider:system', duration_ms: 4, status: 'error', message: "crew 'system' uses provider 'claude'; CLI 'claude' was not found", remediation: "Install the 'claude' CLI or change crew 'system' to an available provider." },
  { check: 'mcp-registration', duration_ms: 1, status: 'ok', message: 'Orbit MCP server registered for this workspace in: claude (workspace)', remediation: null },
  { check: 'hosts', duration_ms: 0, status: 'ok', message: `3 hosts registered; ${host} answers`, remediation: null },
];

// Per host: the latest report and how many times doctor actually ran.
const doctor = {};
let routeLog = [];
const FOUR_MINUTES = 4 * 60 * 1000;

function doctorState(host) {
  if (!doctor[host]) {
    // The serving host already holds a report another tab ran four minutes ago.
    doctor[host] = { runs: 0, finished: host === SERVING ? Date.now() - FOUR_MINUTES : null };
  }
  return doctor[host];
}

function doctorBody(host, state) {
  if (state.finished == null) {
    return { ran_at: null, age_ms: null, duration_ms: null, failures: null, warnings: null, checks: null };
  }
  const checks = ROWS(host);
  return {
    ran_at: new Date(state.finished - 6100).toISOString(),
    age_ms: Date.now() - state.finished,
    duration_ms: 6100,
    failures: checks.filter((row) => row.status === 'error').length,
    warnings: checks.filter((row) => row.status === 'warning').length,
    check_timeout_ms: 20000,
    checks,
  };
}

function doctorApi(host, url, json) {
  const params = [...url.searchParams.keys()].filter((key) => key !== 'workspace');
  for (const key of params) {
    if (key !== 'refresh' && key !== 'cached') return json({ error: `unsupported parameter '${key}'` }, 400);
  }
  const state = doctorState(host);
  const refresh = url.searchParams.get('refresh') === 'true';
  const cached = url.searchParams.get('cached') === 'true';
  const recent = state.finished != null && Date.now() - state.finished < 10 * 60 * 1000;
  if (!cached && (refresh || !recent)) {
    state.runs += 1;
    state.finished = Date.now();
  }
  return json(doctorBody(host, state));
}

function resources(pct) {
  return { sampled_at: new Date().toISOString(), cpu: { used_pct: pct }, memory: { used_pct: pct, used_bytes: 1, total_bytes: 2 }, disks: [] };
}

// One host's own dashboard API, as the forward would relay it.
function hostApi(host, req, rest, url, res) {
  const json = (body, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
  if (rest === 'doctor') {
    // An older host's router has no such route: axum's empty 404, which the
    // forward relays as an error with no text.
    if (host === 'oldhost') return json({ error: '' }, 404);
    return doctorApi(host, url, json);
  }
  if (req.method !== 'GET') return json({ ok: true });
  if (rest === 'workspaces') return json(WORKSPACES[host] || []);
  if (rest === 'tasks') return json({ items: [], total: 0, limit: 50, truncated: false });
  if (rest === 'tasks/all') return json({ items: [], total: 0 });
  if (rest === 'tasks/locks') return json({ total_locked: 0, total_tasks: 0, by_task: [] });
  if (rest === 'crews') return json({ default_crew: null, crews: [] });
  if (rest === 'host/resources') return json(resources(host === SERVING ? 25 : 75));
  if (rest === 'log') return json({ events: [], offset: 0, agent_offset: 0 });
  if (rest === 'log/stream') {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    res.write(': open\n\n');
    return;
  }
  if (rest === 'workflows/auto/readiness') return json({ controls_authorized: true, tasks: [], capacity: { active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4 } });
  if (rest === 'routines') return json({ machine_name: host, capabilities: {}, clock: {}, routines: [] });
  if (rest.startsWith('runs')) return json([]);
  if (rest === 'audit/summary') return json({ window: '24h', events: 0, failed_runs: 0 });
  if (rest.startsWith('config/')) return json({ entries: [], rows: [], config_set: { authorized: true, reason: null } });
  return json({ items: [], total: 0 });
}

function connection(name) {
  const row = HOSTS.hosts.find((host) => host.name === name);
  if (!row) return { status: 404, body: { error: `no registered host named '${name}'`, code: 'unknown_host', host: name } };
  return {
    status: 200,
    body: {
      host: row.name, machine_id: row.machine_id, local: row.local, origin: row.local ? 'local' : 'attached',
      binary_version: VERSION, protocol_fingerprint: FINGERPRINT, skew: false, skew_fields: [], error: null,
      forward_writes: { authorized: true, reason: null }, reachable: true,
    },
  };
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
  if (forwarded) return hostApi(decodeURIComponent(forwarded[1]), req, forwarded[2], url, res);
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
const doctorRequests = () => routeLog.filter((entry) => /\/doctor(\?|$)/.test(entry.path));
const query = (entry) => new URL(entry.path, 'http://fixture').searchParams;

let browser;
let page;
const pageErrors = [];
const evidenceLog = {};

const capture = async (name) => {
  for (const width of [1440, 768]) {
    await page.setViewportSize({ width, height: 900 });
    await page.waitForTimeout(100);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    assert(overflow <= 1, `${name} must not overflow at ${width}px (by ${overflow}px)`);
    await page.screenshot({ path: path.join(evidence, `doctor-${name}-${width}.png`), fullPage: true });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
};
const meta = () => page.locator('#doctor-meta').textContent();
const railCount = () => page.locator('#rail-count-diagnostics').evaluate((node) => ({
  text: node.textContent,
  alert: node.classList.contains('alert'),
  lines: node.getClientRects().length && Math.round(node.getBoundingClientRect().height / parseFloat(getComputedStyle(node).lineHeight)),
  clipped: node.getBoundingClientRect().right > node.closest('.tab').getBoundingClientRect().right,
}));

try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin });
  page = await context.newPage();
  page.on('pageerror', (error) => pageErrors.push(error.message));

  // 1. Opening the panel shows the serving host's cached report and its age;
  //    a recent report is reused, not run again.
  await page.goto(`${origin}/#diagnostics/doctor`);
  await page.locator('#doctor-body .doctor-row').first().waitFor({ state: 'visible' });
  assert(doctorState(SERVING).runs === 0, 'opening the panel reuses a recent cached report');
  const cachedMeta = await meta();
  assert(/^Ran 4m ago · took 6\.10 s/.test(cachedMeta), `the cached report shows its age and duration: ${cachedMeta}`);
  const count = await page.locator('#doctor-count').textContent();
  assert(count === '1 failure · 2 warnings · 8 checks', `the panel counts the report: ${count}`);
  const rail = await railCount();
  assert(rail.text === '1 err · 2 warn' && rail.alert, `the Health rail item counts failures and warnings: ${JSON.stringify(rail)}`);
  assert(rail.lines === 1 && !rail.clipped, `the rail count fits on one line inside the item: ${JSON.stringify(rail)}`);

  // Problems first, each with its full message and remediation.
  const problems = await page.locator('#doctor-body > .doctor-list .doctor-row').evaluateAll((rows) => rows.map((row) => ({
    status: row.querySelector('.doctor-status').textContent,
    check: row.querySelector('.doctor-check').textContent,
    message: row.querySelector('.doctor-message').textContent,
    messageClipped: row.querySelector('.doctor-message').scrollWidth > row.querySelector('.doctor-message').clientWidth + 1,
    remediation: row.querySelector('.doctor-remediation-text')?.textContent,
    commands: [...row.querySelectorAll('.doctor-command code')].map((code) => code.textContent),
  })));
  evidenceLog.problems = problems;
  assert(problems.map((row) => row.status).join(',') === 'error,warning,warning', `errors lead, then warnings: ${JSON.stringify(problems)}`);
  const stale = problems.find((row) => row.check === 'stale-locks');
  assert(stale.message === LONG_MESSAGE && !stale.messageClipped, 'the full message is shown, unclipped');
  assert(stale.commands.join() === 'orbit doctor --fix-stale-locks', `the remediation command is code: ${JSON.stringify(stale)}`);
  const perms = problems.find((row) => row.check === 'state-directory-permissions');
  assert(perms.commands.join('|') === 'chmod go-w <directory>|orbit doctor', `every command in a remediation is copyable: ${JSON.stringify(perms)}`);
  await page.locator('.doctor-row.warning .doctor-copy').first().click();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  assert(copied === 'orbit doctor --fix-stale-locks', `copy puts the command on the clipboard: ${copied}`);
  assert(await page.locator('#doctor-body details.doctor-passing summary').textContent() === '4 passing checks · 1 skipped', 'passing and skipped checks are folded away');
  await capture('cached');

  // 2. The dashboard's own refresh only peeks; it never runs doctor.
  const before = doctorRequests().length;
  for (let i = 0; i < 2; i += 1) {
    await page.locator('#refresh-btn').click();
    await until('a dashboard tick', () => doctorRequests().length > before + i);
  }
  await page.waitForTimeout(300);
  assert(doctorState(SERVING).runs === 0, 'a dashboard tick never runs doctor');
  assert(doctorRequests().slice(before).every((entry) => entry.path.includes('cached=true')), `ticks only peek: ${JSON.stringify(doctorRequests().slice(before))}`);

  // 3. Refresh runs doctor again and the age resets.
  await page.locator('#doctor-refresh').click();
  await until('the refreshed report', async () => /^Ran (0s|just now|\ds) ago/.test(await meta()));
  assert(doctorState(SERVING).runs === 1, 'Refresh runs doctor once');
  assert(doctorRequests().some((entry) => entry.path.startsWith('/api/doctor?') && query(entry).get('refresh') === 'true' && query(entry).get('workspace') === 'ws_orbit'), `Refresh asks for a new run: ${JSON.stringify(doctorRequests())}`);
  evidenceLog.metaAfterRefresh = await meta();
  await page.locator('#doctor-body details.doctor-passing summary').click();
  await capture('refreshed');

  // 4. The panel follows the selected host through the forward.
  await page.locator('#host-select').selectOption('hostb');
  await until('hostb doctor', () => routeLog.some((entry) => entry.path.startsWith('/api/on/hostb/doctor?') && query(entry).get('workspace') === 'ws_b'));
  await until('hostb rows', async () => (await page.locator('#doctor-body').textContent()).includes('/srv/hostb/repo/.orbit'));
  assert(doctorState('hostb').runs === 1, 'a remote with no report runs doctor on first open');
  assert(/^Ran /.test(await meta()), 'the remote report shows its age');

  // 5. A host whose dashboard predates the panel.
  await page.locator('#host-select').selectOption('oldhost');
  await until('the unavailable state', async () => (await page.locator('#doctor-body').textContent()).includes('Doctor is not available on this host'));
  assert(await meta() === 'Not available on this host', `the meta line names the state: ${await meta()}`);
  const oldRail = await railCount();
  assert(oldRail.text === '', 'an unavailable host shows no rail count');
  await capture('unavailable');

  // 6. No request ever carried a repair, --deep, or a write method.
  const requests = doctorRequests();
  evidenceLog.doctorRequests = requests;
  for (const entry of requests) {
    assert(entry.method === 'GET', `doctor is only read: ${entry.method} ${entry.path}`);
    const params = [...query(entry).keys()];
    assert(params.every((key) => ['workspace', 'refresh', 'cached'].includes(key)), `only read-only parameters: ${entry.path}`);
  }
  assert(pageErrors.length === 0, `no page errors: ${pageErrors.join('; ')}`);
  fs.writeFileSync(path.join(evidence, 'doctor-assertions.json'), `${JSON.stringify({
    passed: true,
    assertions: [
      'browser-doctor-rows-show-full-message-and-remediation',
      'browser-doctor-remediation-commands-copy',
      'browser-doctor-cached-report-shows-age',
      'browser-doctor-refresh-runs-again',
      'browser-doctor-poll-never-runs-doctor',
      'browser-doctor-health-rail-count',
      'browser-doctor-follows-selected-host',
      'browser-doctor-unavailable-on-older-host',
      'browser-doctor-requests-are-read-only',
    ],
    viewports: ['1440', '768'],
    ...evidenceLog,
  }, null, 2)}\n`);
  console.log('Dashboard doctor browser checks passed at 1440px and 768px: rows, remediation copy, cached age, Refresh, peek-only polling, host forward and unavailable host.');
} catch (error) {
  fs.writeFileSync(path.join(evidence, 'doctor-assertions.json'), JSON.stringify({ passed: false, error: error.message, pageErrors, ...evidenceLog }, null, 2));
  throw error;
} finally {
  await browser?.close();
  for (const socket of sockets) socket.destroy();
  await new Promise((resolve) => server.close(resolve));
}
