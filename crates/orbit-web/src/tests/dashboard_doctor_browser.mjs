// Usage: node dashboard_doctor_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
//
// Health › Doctor in Chromium. The shipped app runs against a disposable
// fixture server that answers `/api/doctor` for the serving host and, through
// `/api/on/<host>/…`, for a current remote and for an older remote whose
// dashboard has no such route. It checks the rows (full message, remediation
// with copyable commands), the cached report's age, that Refresh runs doctor
// again while the dashboard's own poll never does, the busy state of a first
// run and of a refresh while the server holds its response (and the controls
// restored after a success or a failure), the Health rail count, that a
// cached read delayed in transit past a newer refresh or a host switch never
// replaces what the panel shows, and that no request carries anything but
// the read-only parameters. No Orbit
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
// The same checks once every problem is repaired.
const HEALTHY_ROWS = (host) => ROWS(host).map((row) => (
  row.status === 'skipped' ? row : { ...row, status: 'ok', remediation: null }
));

// Per host: the latest report, how many times doctor actually ran, and
// whether the next run finds nothing wrong.
const doctor = {};
let routeLog = [];
const FOUR_MINUTES = 4 * 60 * 1000;

function doctorState(host) {
  if (!doctor[host]) {
    // The serving host already holds a report another tab ran four minutes ago.
    doctor[host] = { runs: 0, finished: host === SERVING ? Date.now() - FOUR_MINUTES : null, healthy: false, nextHealthy: false };
  }
  return doctor[host];
}

function doctorBody(host, state) {
  if (state.finished == null) {
    return { ran_at: null, age_ms: null, duration_ms: null, failures: null, warnings: null, checks: null };
  }
  const checks = state.healthy ? HEALTHY_ROWS(host) : ROWS(host);
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

// A held doctor run: while armed, every run (not a cached peek) for `host`
// waits until the test settles the hold, so the panel's pending state is
// checked while a response is outstanding rather than after a delay. With
// `cached`, the hold takes the cached peeks instead: each is answered when it
// arrives, with the report cached then, and its response is held in transit
// until the test settles the hold.
let hold = null;
function holdDoctor(host, { cached = false } = {}) {
  let settle;
  const settled = new Promise((resolve) => { settle = resolve; });
  hold = { host, cached, requests: [], delivered: 0, settled };
  const armed = hold;
  return {
    requests: armed.requests,
    delivered: () => armed.delivered === armed.requests.length,
    settle: (outcome = {}) => {
      if (hold === armed) hold = null;
      settle(outcome);
    },
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
  const answer = () => {
    const recent = state.finished != null && Date.now() - state.finished < 10 * 60 * 1000;
    if (!cached && (refresh || !recent)) {
      state.runs += 1;
      state.finished = Date.now();
      state.healthy = state.nextHealthy;
    }
    return json(doctorBody(host, state));
  };
  if (hold && hold.host === host && hold.cached === cached) {
    const armed = hold;
    armed.requests.push(url.searchParams);
    const peeked = cached ? doctorBody(host, state) : null;
    armed.settled.then((outcome) => {
      if (outcome.error) json({ error: outcome.error }, 500);
      else if (peeked) json(peeked);
      else answer();
      armed.delivered += 1;
    });
    return undefined;
  }
  return answer();
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
const panelState = () => page.evaluate(() => {
  const button = document.getElementById('doctor-refresh');
  const body = document.getElementById('doctor-body');
  return {
    disabled: button.disabled,
    label: button.textContent,
    meta: document.getElementById('doctor-meta').textContent,
    busy: body.getAttribute('aria-busy'),
    body: body.textContent,
    rows: body.querySelectorAll('.doctor-row').length,
    problems: body.querySelectorAll('.doctor-row.error, .doctor-row.warning').length,
  };
});
const assertIdle = (state, label) => {
  assert(!state.disabled && state.label === 'Refresh' && state.busy === 'false' && !/[Rr]unning/.test(state.meta),
    `${label}: Refresh is restored and no progress shows: ${JSON.stringify(state)}`);
};
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

  // 3. Refreshing the cached report disables Refresh and shows progress for
  //    as long as the run is outstanding, keeping the previous rows. A failed
  //    run restores the controls over the previous result.
  let held = holdDoctor(SERVING);
  await page.locator('#doctor-refresh').click();
  await until('the held failing refresh', () => held.requests.length > 0);
  const refreshPending = await panelState();
  evidenceLog.refreshPending = refreshPending;
  assert(refreshPending.disabled && refreshPending.label === 'Running…', `Refresh is disabled while it runs: ${JSON.stringify(refreshPending)}`);
  assert(refreshPending.meta.endsWith(' · running again…') && refreshPending.busy === 'true', `the panel shows the refresh in progress: ${JSON.stringify(refreshPending)}`);
  assert(refreshPending.rows === 8, `the previous report stays while the refresh runs: ${JSON.stringify(refreshPending)}`);
  held.settle({ error: 'doctor did not finish' });
  await until('the failed refresh', async () => (await panelState()).body.includes('Doctor failed; showing the previous result: doctor did not finish'));
  const refreshFailed = await panelState();
  assertIdle(refreshFailed, 'after a failed refresh');
  assert(refreshFailed.rows === 8 && doctorState(SERVING).runs === 0, `a failed refresh keeps the previous report: ${JSON.stringify(refreshFailed)}`);

  // Refresh runs doctor again and the age resets.
  held = holdDoctor(SERVING);
  await page.locator('#doctor-refresh').click();
  await until('the held refresh', () => held.requests.length > 0);
  const retryPending = await panelState();
  assert(retryPending.disabled && retryPending.label === 'Running…' && retryPending.busy === 'true', `a retried Refresh is disabled while it runs: ${JSON.stringify(retryPending)}`);
  held.settle();
  await until('the refreshed report', async () => /^Ran (0s|just now|\ds) ago/.test(await meta()));
  const refreshed = await panelState();
  assertIdle(refreshed, 'after a refresh');
  assert(!refreshed.body.includes('Doctor failed'), `a successful refresh clears the failure: ${JSON.stringify(refreshed)}`);
  assert(doctorState(SERVING).runs === 1, 'Refresh runs doctor once');
  assert(doctorRequests().some((entry) => entry.path.startsWith('/api/doctor?') && query(entry).get('refresh') === 'true' && query(entry).get('workspace') === 'ws_orbit'), `Refresh asks for a new run: ${JSON.stringify(doctorRequests())}`);
  evidenceLog.metaAfterRefresh = await meta();
  await page.locator('#doctor-body details.doctor-passing summary').click();
  await capture('refreshed');

  // A poll's cached read that the network delays until after a newer refresh
  // was shown never replaces the refreshed report or its rail count.
  const late = holdDoctor(SERVING, { cached: true });
  await page.locator('#refresh-btn').click();
  await until('the held cached read', () => late.requests.length > 0);
  doctorState(SERVING).nextHealthy = true;
  await page.locator('#doctor-refresh').click();
  await until('the healthy refresh', async () => (await panelState()).body.includes('All 8 checks passed or were skipped.'));
  assert(doctorState(SERVING).runs === 2 && (await railCount()).text === '', 'the healthy refresh clears the rail count');
  late.settle();
  await until('the late cached response', () => late.delivered());
  // The response is on the wire; give the page time to act on it.
  await page.waitForTimeout(300);
  const afterLate = await panelState();
  const lateRail = await railCount();
  evidenceLog.afterLateCachedRead = { panel: afterLate, rail: lateRail };
  assert(afterLate.body.includes('All 8 checks passed or were skipped.') && afterLate.problems === 0,
    `a late cached read keeps the refreshed report: ${JSON.stringify(afterLate)}`);
  assert(/^Ran (0s|just now|\ds) ago/.test(afterLate.meta), `a late cached read keeps the refreshed age: ${afterLate.meta}`);
  assert(lateRail.text === '' && !lateRail.alert, `a late cached read keeps the rail count: ${JSON.stringify(lateRail)}`);

  // A newer report cached by another tab's run still reaches the panel on
  // the next poll, with its counts and age.
  const other = doctorState(SERVING);
  other.nextHealthy = false;
  other.healthy = false;
  other.finished = Date.now();
  await page.locator('#refresh-btn').click();
  await until('the newer cached report', async () => (await railCount()).text === '1 err · 2 warn');
  const newer = await panelState();
  assert(newer.problems === 3 && /^Ran (0s|just now|\ds) ago/.test(newer.meta), `a newer cached report replaces the panel: ${JSON.stringify(newer)}`);
  assert(doctorState(SERVING).runs === 2, 'reading another tab\'s report never runs doctor');

  // 4. The panel follows the selected host through the forward. A remote
  //    with no report shows doctor running, with Refresh disabled, until its
  //    first run answers.
  held = holdDoctor('hostb');
  await page.locator('#host-select').selectOption('hostb');
  await until('hostb doctor', () => held.requests.some((params) => params.get('workspace') === 'ws_b'));
  const firstPending = await panelState();
  evidenceLog.firstRunPending = firstPending;
  assert(firstPending.disabled && firstPending.label === 'Running…', `Refresh is disabled during the first run: ${JSON.stringify(firstPending)}`);
  assert(firstPending.meta === 'Running doctor…' && firstPending.busy === 'true', `the meta line shows the first run: ${JSON.stringify(firstPending)}`);
  assert(firstPending.body === 'Running orbit doctor…' && firstPending.rows === 0, `the body shows the first run, not "Not run yet": ${JSON.stringify(firstPending)}`);
  held.settle();
  await until('hostb rows', async () => (await page.locator('#doctor-body').textContent()).includes('/srv/hostb/repo/.orbit'));
  assertIdle(await panelState(), 'after the first run');
  assert(doctorState('hostb').runs === 1, 'a remote with no report runs doctor on first open');
  assert(/^Ran /.test(await meta()), 'the remote report shows its age');

  // 5. A host whose dashboard predates the panel. A cached read of the
  //    previous host that lands after the switch is discarded.
  const retired = holdDoctor('hostb', { cached: true });
  await page.locator('#refresh-btn').click();
  await until('the held hostb cached read', () => retired.requests.length > 0);
  await page.locator('#host-select').selectOption('oldhost');
  await until('the unavailable state', async () => (await page.locator('#doctor-body').textContent()).includes('Doctor is not available on this host'));
  retired.settle();
  await until('the retired host response', () => retired.delivered());
  await page.waitForTimeout(300);
  assert((await page.locator('#doctor-body').textContent()).includes('Doctor is not available on this host'), 'a retired host\'s late response is discarded');
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
      'browser-doctor-first-run-shows-busy-state',
      'browser-doctor-refresh-shows-busy-state-until-settled',
      'browser-doctor-poll-never-runs-doctor',
      'browser-doctor-health-rail-count',
      'browser-doctor-late-cached-read-never-replaces-refresh',
      'browser-doctor-newer-cached-report-updates-panel',
      'browser-doctor-retired-host-response-discarded',
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
