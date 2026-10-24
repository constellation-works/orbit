// Usage: node dashboard_incidents_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
//
// Health › Incidents in Chromium. The shipped app runs against a disposable
// fixture server whose `/api/audit/incidents` answers like the real endpoint:
// it filters by `class` before its limit and dates every incident relative to
// now, so the shared window axis has known positions. It checks the class
// tiles (counts, filtering, pressed state), lifespan bars for a recurring and
// a single-burst signature, the detail panel (full message and its copy, the
// truncation note and its audit drill-down, related signatures, runs), the
// last-hour window insight and no-message filters, the local 6h window, the
// sticky panel and a 375px phone with no horizontal page scroll. It also checks grouping by
// cause [ORB-15434]: cause headers over signatures that share one, a
// single-signature cause as one row, the By signature toggle, and the
// detail panel's same-cause list and grouping basis, and that a message the
// feed cut to its list cap loads in full on demand [ORB-15527]. Independent
// host stores reuse an audit row ID; held responses check message and Copy
// isolation across host switches and workspace visits. No Orbit store is
// contacted.
import { pathToFileURL } from 'node:url';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const MINUTE = 60 * 1000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const HOSTS = {
  host_file: '/home/fixture/.orbit/hosts.toml', legacy: false, generation: 1, load_error: null,
  host_edit: { authorized: true, reason: null },
  hosts: [{
    name: 'alpha', machine_id: 'hm_alpha', ssh: null, task_prefix: 'ALP', local: true, legacy: false,
    reachable: true, error: null, binary_version: '0.28.2', protocol_fingerprint: 'fp', skew: false, skew_fields: [], workspaces: [],
  }, {
    name: 'beta', machine_id: 'hm_beta', ssh: 'fixture@beta', task_prefix: 'BET', local: false, legacy: false,
    reachable: true, error: null, binary_version: '0.28.2', protocol_fingerprint: 'fp', skew: false, skew_fields: [], workspaces: [],
  }],
};

const LOCK_MESSAGE = 'task store write failed: lock held by another writer (pid 4121, op: task store write)';
const LONG_MESSAGE = [
  'cargo nextest run failed: 3 tests failed in orbit-core',
  '  FAIL runtime::task::claim::tests::claim_rejects_stale_lease',
  '  FAIL runtime::task::claim::tests::claim_retries_after_contention',
  '  FAIL runtime::dispatch::tests::admission_waits_for_free_slot',
  'error: test run failed, to rerun pass `-p orbit-core --lib`',
].join('\n');
const CUT_MESSAGE = 'workspace sync failed: git fetch origin refs/heads/agent-main exited 128: fatal: unable to access the remote: Could not resolve host: github.com; retrying with the mirror at…';
const VALIDATE_MESSAGE = "execution failed: action `fixture_validate` failed: required check 'make check' did not pass on candidate 9f8e7d6c";
const VALIDATE_CAUSE = { key: 'cause-validate', label: 'action `fixture_validate`', basis: 'step_message' };
const SANDBOX_MESSAGE = '[blocked_by_agent] kind=sandbox_refused\nThe host refuses unprivileged user namespaces.';
const SANDBOX_CAUSE = { key: 'cause-sandbox', label: 'kind=sandbox_refused', basis: 'code' };
// The feed's per-message cap; a longer message arrives cut, flagged
// `truncated`, with the audit row that holds it in full.
const LIST_MESSAGE_MAX_BYTES = 2048;
const REFUSAL_MESSAGE = ['transition refused: task is not in review',
  ...Array.from({ length: 60 }, (_, index) => `  blocker ${index}: required evidence is missing for criterion ${index}`)].join('\n');
const REFUSAL_EVENT_ID = 3001;
const BETA_REFUSAL_MESSAGE = REFUSAL_MESSAGE.replace('transition refused:', 'beta transition refused:');

const refusalMessage = (host) => host === 'beta' ? BETA_REFUSAL_MESSAGE : REFUSAL_MESSAGE;

// One incident per (run, signature), as the store keys them.
function incident(id, fields) {
  const { cls = 'unexpected', role = 'codex', surface, message, runId, taskId, firstAgo, lastAgo = firstAgo, events = 1, cause, now } = fields;
  const msg = (message || '').toLowerCase().replace(/\d+/g, 'N');
  const signature = `${cls}|role=${role}|surface=${surface}|msg=${msg}`;
  const ts = (ago) => new Date(now - ago).toISOString();
  const eventRows = Array.from({ length: events }, (_, index) => ({
    id: id * 100 + index + 1, execution_id: `exec-${id}-${index}`, ts: ts(firstAgo - (firstAgo - lastAgo) * index / Math.max(1, events - 1)),
    status: 'failure', actor: role, surface, tool: surface, run_id: runId, task_id: taskId, message, workspace_id: 'ws_orbit',
  }));
  return {
    incident_id: `inc-${id}`, signature,
    class: cls, class_label: null, actor: role, surface, activity_id: 'implement', message,
    event_count: events, root_event_count: events, propagated_event_count: 0,
    first_ts: ts(firstAgo), last_ts: ts(lastAgo), run_ids: runId ? [runId] : [], task_ids: taskId ? [taskId] : [],
    has_tool_identity: true, sample_events: eventRows.slice(0, 3), events: eventRows, propagation: [],
    // With no structured evidence the server falls back to the signature.
    cause: cause || { key: `cause-of-${signature}`, label: surface, basis: 'signature' },
  };
}

function allIncidents(now, host) {
  const rows = [];
  // A: the same failure in six runs, two hours apart; last seen 20 min ago.
  for (let index = 0; index < 6; index += 1) {
    const ago = 20 * MINUTE + (5 - index) * 2 * HOUR;
    rows.push(incident(10 + index, { surface: 'orbit.task.update', message: LOCK_MESSAGE, runId: `run-lock-${index}`, taskId: 'ORB-7001', firstAgo: ago, now }));
  }
  // B: one burst of four events three hours ago, whose message was cut short.
  rows.push(incident(20, { surface: 'orbit.workspace.sync', message: CUT_MESSAGE, runId: 'run-sync-1', taskId: 'ORB-7002', firstAgo: 3 * HOUR + 5 * MINUTE, lastAgo: 3 * HOUR + 2 * MINUTE, events: 4, now }));
  // C: a failure that recorded no message.
  rows.push(incident(21, { surface: 'orbit.run.start', message: null, runId: 'run-start-1', firstAgo: 5 * HOUR, now }));
  // D: A's message from another surface: a separate signature.
  rows.push(incident(22, { role: 'claude', surface: 'orbit.task.claim', message: LOCK_MESSAGE, runId: 'run-claim-1', firstAgo: 7 * HOUR, now }));
  // E: a multi-line failure 40 min ago.
  rows.push(incident(23, { surface: 'orbit.validation.run', message: LONG_MESSAGE, runId: 'run-tests-1', taskId: 'ORB-7003', firstAgo: 40 * MINUTE, events: 2, now }));
  // F: one failed action reported by a parent's wait and by the worker, two
  //    signatures with one cause; the worker's recurs in two runs.
  rows.push(incident(24, { role: 'admin', surface: 'fixture.wait', message: VALIDATE_MESSAGE, runId: 'run-val-wait', firstAgo: 9 * HOUR, cause: VALIDATE_CAUSE, now }));
  rows.push(incident(25, { role: 'human', surface: 'job worker', message: VALIDATE_MESSAGE, runId: 'run-val-1', firstAgo: 11 * HOUR, cause: VALIDATE_CAUSE, now }));
  rows.push(incident(26, { role: 'human', surface: 'job worker', message: VALIDATE_MESSAGE, runId: 'run-val-2', firstAgo: 10 * HOUR, cause: VALIDATE_CAUSE, now }));
  // G: a structured code only one signature carries.
  rows.push(incident(27, { role: 'admin', surface: 'fixture.wait', message: SANDBOX_MESSAGE, runId: 'run-sandbox', firstAgo: 12 * HOUR, cause: SANDBOX_CAUSE, now }));
  const refusal = incident(30, { cls: 'expected', surface: 'orbit.task.transition', message: refusalMessage(host).slice(0, LIST_MESSAGE_MAX_BYTES), firstAgo: 2 * HOUR, now });
  refusal.truncated = true;
  refusal.message_event_id = REFUSAL_EVENT_ID;
  for (const event of refusal.events) event.truncated = true;
  rows.push(refusal);
  // Enough policy denials to make the list taller than the viewport.
  for (let index = 0; index < 30; index += 1) {
    rows.push(incident(40 + index, { cls: 'denied', role: 'codex', surface: `orbit.denied.tool_${index}`, message: `denied by policy: tool ${index} is not in the crew allowlist`, runId: `run-deny-${index}`, firstAgo: (index + 1) * 30 * MINUTE, now }));
  }
  return rows;
}

const incidentRequests = [];
const messageRequests = [];
const routeLog = [];
let nextMessageHold = null;
function holdNextMessage(host, workspace) {
  if (nextMessageHold) throw new Error('a message hold is already armed');
  nextMessageHold = { host, workspace, release: null };
  return nextMessageHold;
}
function incidentsApi(url, host) {
  const now = Date.now();
  const window = url.searchParams.get('since') || '24h';
  const cls = url.searchParams.get('class');
  const limit = Number(url.searchParams.get('limit')) || 50;
  const span = { '1h': HOUR, '6h': 6 * HOUR, '24h': DAY, '7d': 7 * DAY }[window];
  const since = span ? now - span : null;
  const inWindow = allIncidents(now, host).filter((row) => since == null || Date.parse(row.last_ts) >= since);
  const classes = ['unexpected', 'expected', 'denied', 'diagnostic', 'deferred'];
  const stats = (rows) => ({
    incidents: rows.length,
    raw_events: rows.reduce((sum, row) => sum + row.event_count, 0),
    affected_runs: new Set(rows.flatMap((row) => row.run_ids)).size,
  });
  const failure_categories = Object.fromEntries(classes.map((key) => [key, stats(inWindow.filter((row) => row.class === key))]));
  const matching = cls ? inWindow.filter((row) => row.class === cls) : inWindow;
  const shown = matching.slice(0, limit);
  const all = stats(inWindow);
  return {
    window, since: since == null ? null : new Date(since).toISOString(), class: cls,
    total_events: 4000, raw_failed_events: all.raw_events, incident_count: all.incidents, affected_run_count: all.affected_runs,
    matching_incident_count: matching.length, shown_incident_count: shown.length, limit,
    failure_categories, class_labels: {}, truncated: false, incidents: shown,
  };
}

function api(host, rest, url, res) {
  const json = (body, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
  if (rest === 'audit/incidents') {
    incidentRequests.push({ host, ...Object.fromEntries(url.searchParams) });
    return json(incidentsApi(url, host));
  }
  if (rest === `audit/events/${REFUSAL_EVENT_ID}/message`) {
    const workspace = url.searchParams.get('workspace') || '';
    messageRequests.push({ host, workspace, path: url.pathname });
    const body = { id: REFUSAL_EVENT_ID, message: refusalMessage(host) };
    if (nextMessageHold?.host === host && nextMessageHold.workspace === workspace) {
      const held = nextMessageHold;
      nextMessageHold = null;
      held.release = (refused = false) => refused ? json({ error: 'held message unavailable' }, 503) : json(body);
      return;
    }
    return json(body);
  }
  if (rest === 'hosts') return json(HOSTS);
  if (rest === 'hosts/alpha/connection' || rest === 'hosts/beta/connection') {
    const name = rest.split('/')[1];
    return json({ host: name, machine_id: `hm_${name}`, local: name === 'alpha', origin: name === 'alpha' ? 'local' : 'remote', binary_version: '0.28.2', protocol_fingerprint: 'fp', skew: false, skew_fields: [], error: null, forward_writes: { authorized: true, reason: null }, reachable: true });
  }
  if (rest === 'workspaces') return json([
    { id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true },
    { id: 'ws_other', name: 'other', status: 'active', is_default: false },
  ]);
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
  if (rest === 'routines') return json({ machine_name: host, capabilities: {}, clock: {}, routines: [] });
  if (rest.startsWith('runs') || rest === 'audit') return json([]);
  if (rest === 'audit/summary') return json({ window: '24h', events: 0, failed_runs: 0 });
  if (rest.startsWith('config/')) return json({ entries: [], rows: [], config_set: { authorized: true, reason: null } });
  return json({ items: [], total: 0 });
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (!url.pathname.startsWith('/api/')) {
    // The dashboard currently gates Incidents behind a concrete workspace.
    // Exercise its aggregate cache directly with the shipped modules and
    // common.js routing, without widening the dashboard's supported views.
    if (url.pathname === '/message-scope') {
      res.setHeader('content-type', 'text/html');
      res.end('<!doctype html><link rel="stylesheet" href="/static/dashboard.css"><div id="diag-count"></div><div id="diag-body"></div>');
      return;
    }
    const served = dashboardFile(url.pathname);
    if (!served) { res.writeHead(404); res.end(); return; }
    res.setHeader('content-type', served.type);
    res.end(served.data);
    return;
  }
  routeLog.push(url.pathname + url.search);
  const forwarded = url.pathname.match(/^\/api\/on\/([^/]+)\/(.+)$/);
  return api(forwarded ? forwarded[1] : 'alpha', forwarded ? forwarded[2] : url.pathname.slice('/api/'.length), url, res);
});
const sockets = new Set();
server.on('connection', (socket) => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

const assert = (condition, message) => { if (!condition) throw new Error(message); };
const near = (actual, expected, tolerance, message) => assert(Math.abs(actual - expected) <= tolerance, `${message}: ${actual} vs ${expected}`);
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

const tiles = () => page.locator('#diag-body .incident-tile').evaluateAll((nodes) => nodes.map((node) => ({
  cls: node.dataset.class,
  pressed: node.getAttribute('aria-pressed'),
  number: node.querySelector('.incident-tile-number').textContent,
  sub: node.querySelector('.incident-tile-sub').textContent,
})));
const rows = () => page.locator('#diag-body .incident-row').evaluateAll((nodes) => nodes.map((node) => {
  const code = node.querySelector('.incident-code');
  const track = node.querySelector('.incident-track').getBoundingClientRect();
  const markNode = node.querySelector('.incident-mark');
  const mark = markNode.getBoundingClientRect();
  return {
    headline: node.querySelector('.incident-headline').textContent,
    sub: node.querySelector('.incident-sub').textContent,
    flag: node.querySelector('.incident-flag')?.textContent || null,
    count: node.querySelector('.incident-count').textContent,
    live: node.querySelector('.incident-last').classList.contains('live'),
    pressed: node.getAttribute('aria-pressed'),
    child: node.classList.contains('child'),
    code: code ? code.textContent : null,
    dot: markNode.classList.contains('dot'),
    markLive: markNode.classList.contains('live'),
    leftPct: (mark.left - track.left) / track.width * 100,
    rightPct: (mark.right - track.left) / track.width * 100,
    centerPct: ((mark.left + mark.right) / 2 - track.left) / track.width * 100,
  };
}));
const detail = () => page.locator('#diag-body .incident-detail').evaluate((node) => ({
  pills: [...node.querySelectorAll('.incident-pill')].map((pill) => pill.textContent),
  title: node.querySelector('.incident-detail-title').textContent,
  sentence: node.querySelector('.incident-sentence').textContent,
  message: node.querySelector('.incident-full-message').textContent,
  messageClipped: node.querySelector('.incident-full-message').scrollWidth > node.querySelector('.incident-full-message').clientWidth + 1,
  note: node.querySelector('.incident-message-note')?.textContent || null,
  related: [...node.querySelectorAll('.incident-related-item')].map((item) => item.textContent),
  sameCause: [...node.querySelectorAll('.incident-same-cause .incident-related-item')].map((item) => item.textContent),
  basis: node.querySelector('.incident-cause-basis')?.textContent || null,
  facts: Object.fromEntries([...node.querySelectorAll('.incident-facts > div')].map((fact) => [fact.querySelector('dt').textContent, fact.querySelector('dd').textContent])),
  actions: [...node.querySelectorAll('.incident-actions button')].map((button) => button.textContent),
}));
const rowNamed = (text) => page.locator('#diag-body .incident-row', { hasText: text });
// The list in display order: cause headers and signature rows.
const listing = () => page.locator('#diag-body .incident-rows > *').evaluateAll((nodes) => nodes.map((node) => {
  if (node.classList.contains('incident-cause-head')) {
    return {
      head: true,
      headline: node.querySelector('.incident-headline').textContent,
      code: node.querySelector('.incident-code').textContent,
      sub: node.querySelector('.incident-sub').textContent,
      count: node.querySelector('.incident-count').textContent,
      marks: node.querySelectorAll('.incident-track .incident-mark').length,
      interactive: node.matches('button, [role="button"], [tabindex]'),
    };
  }
  return { head: false, sub: node.querySelector('.incident-sub')?.textContent || '', child: node.classList.contains('child') };
}));
const lastRequest = () => incidentRequests.at(-1);
const noPageOverflow = () => page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);

try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin });
  // Routing disables Chromium's HTTP cache, allowing two held requests to
  // the same URL to reach the server independently and settle out of order.
  await context.route('**/api/**', (route) => route.continue());
  // A settlement marker runs in the next task after the app consumes the
  // response body, so releasing a held response never relies on a sleep.
  await context.addInitScript(() => {
    window.messageSettlements = 0;
    const nativeFetch = window.fetch.bind(window);
    window.fetch = async (...args) => {
      const response = await nativeFetch(...args);
      if (/\/audit\/events\/\d+\/message(?:\?|$)/.test(String(args[0]))) {
        for (const method of ['json', 'text']) {
          const consume = response[method].bind(response);
          response[method] = (...bodyArgs) => consume(...bodyArgs).finally(() => {
            setTimeout(() => { window.messageSettlements += 1; }, 0);
          });
        }
      }
      return response;
    };
  });
  page = await context.newPage();
  page.on('pageerror', (error) => pageErrors.push(error.message));

  // 1. Class tiles count every class in the window; Unexpected is selected
  //    and the request asks the server for that class only.
  await page.goto(`${origin}/?workspace=ws_orbit#diagnostics/incidents?window=24h`);
  await page.locator('#diag-body .incident-row').first().waitFor({ state: 'visible' });
  assert(await page.locator('#diag-body .incident-toolbar').getByRole('button', { name: 'Last hour only' }).count() === 0, 'the toolbar has no duplicate last-hour filter');
  assert(lastRequest().class === 'unexpected' && lastRequest().since === '24h', `the request names the class and window: ${JSON.stringify(lastRequest())}`);
  const tileState = await tiles();
  evidenceLog.tiles = tileState;
  assert(tileState.map((tile) => tile.cls).join() === 'unexpected,expected,denied,diagnostic,deferred', `five class tiles: ${JSON.stringify(tileState)}`);
  assert(JSON.stringify(tileState[0]) === JSON.stringify({ cls: 'unexpected', pressed: 'true', number: '14', sub: '18 events · 14 runs' }), `the Unexpected tile counts incidents, events and runs: ${JSON.stringify(tileState[0])}`);
  assert(tileState.slice(1).every((tile) => tile.pressed === 'false'), 'only the selected tile is pressed');
  assert(tileState[3].number === '0', 'an empty class still has its tile');
  const denominator = await page.locator('#diag-body .incident-denominator').textContent();
  assert(denominator === '45 incidents in 24h: 49 failed events across 44 runs, out of 4,000 audited events (1.2%).', `one denominator line: ${denominator}`);

  // 2. Rows: one per signature, newest last seen first; the recurring one is
  //    a bar over its lifespan, the single burst a dot, and neither repeats
  //    the class or shows the grouping signature.
  const initial = await rows();
  evidenceLog.rows = initial;
  assert(initial.length === 8, `six runs of one signature roll up into one row: ${initial.length}`);
  assert(initial.slice(0, 5).map((row) => row.headline).join('|') === [
    LOCK_MESSAGE, LONG_MESSAGE.split('\n')[0], CUT_MESSAGE, 'orbit.run.start failed; no message recorded', LOCK_MESSAGE,
  ].join('|'), `rows sort by last seen and title with the message: ${JSON.stringify(initial.map((row) => row.headline))}`);
  assert(initial.every((row) => !row.headline.includes('|msg=') && !row.sub.includes('Unexpected')), 'no row shows its signature or repeats its class');
  const [recurring, live, burst, silent, quiet] = initial;
  assert(recurring.count === '6 runs' && recurring.sub === 'orbit.task.update · codex · every 2 h', `the recurring row counts runs and its cadence: ${JSON.stringify(recurring)}`);
  assert(!recurring.dot && recurring.markLive && recurring.live, 'a signature seen 20 minutes ago is live');
  near(recurring.leftPct, (DAY - (10 * HOUR + 20 * MINUTE)) / DAY * 100, 1, 'the recurring bar starts at its first sighting');
  near(recurring.rightPct, (DAY - 20 * MINUTE) / DAY * 100, 1, 'the recurring bar ends at its last sighting');
  assert(burst.dot && !burst.markLive && !burst.live && burst.count === '4 events', `a three-minute burst is a quiet dot: ${JSON.stringify(burst)}`);
  near(burst.centerPct, (DAY - (3 * HOUR + 3.5 * MINUTE)) / DAY * 100, 1, 'the dot sits at the burst');
  assert(silent.flag === 'no message' && quiet.count === '1 event' && live.live, 'a missing message is flagged in words');
  const axis = await page.locator('#diag-body .incident-axis-row .incident-axis').evaluate((node) => [...node.children].map((child) => child.textContent));
  assert(axis.join() === '24h ago,12h,now', `the shared axis is labelled: ${axis}`);

  // A poll whose answer has not changed keeps every row node, so none is
  // rebuilt and flashed, even though the axis moved with the clock.
  await page.evaluate(() => document.querySelectorAll('#diag-body .incident-row').forEach((row, index) => { row.dataset.probe = String(index); }));
  const polled = incidentRequests.length;
  await page.locator('#refresh-btn').click();
  await until('a dashboard poll', () => incidentRequests.length > polled);
  await page.waitForTimeout(300);
  const kept = await page.locator('#diag-body .incident-row').evaluateAll((nodes) => nodes.map((node) => node.dataset.probe));
  assert(kept.join() === '0,1,2,3,4,5,6,7', `a poll keeps unchanged rows: ${JSON.stringify(kept)}`);

  // 3. The detail panel: the newest signature is selected on load.
  assert(recurring.pressed === 'true', 'the newest signature starts selected');
  let panel = await detail();
  assert(panel.message === LOCK_MESSAGE && panel.note === null, 'the full message, with no truncation note');
  assert(panel.pills.join() === 'Unexpected failure,Failed in the last hour', `class and live pills: ${panel.pills}`);
  assert(/^6 runs since 10h( \d+m)? ago, every 2 h\. Last seen 20m ago\.$/.test(panel.sentence), `one summary sentence: ${panel.sentence}`);
  assert(panel.related.length === 1 && panel.related[0].startsWith('orbit.task.claim · claude'), `the same message from another surface is a related signature: ${panel.related}`);
  assert(panel.actions.join() === 'View runs (6),Audit events', `drill-down actions: ${panel.actions}`);
  assert(panel.facts['Cause code'] === 'none recorded' && panel.sameCause.length === 0 && panel.basis === null,
    `a signature with no structured cause names none and lists no cause siblings: ${JSON.stringify(panel)}`);
  evidenceLog.recurringDetail = panel;
  await page.locator('#diag-body .incident-actions button', { hasText: 'View runs' }).click();
  const runLinks = await page.locator('#diag-body .incident-runs[open] .incident-run-links a').allTextContents();
  assert(runLinks.length === 6 && runLinks[0] === 'run-lock-5', `View runs opens every run, newest first: ${runLinks}`);
  await page.locator('#diag-body .incident-related-item').click();
  assert((await detail()).facts.Surface === 'orbit.task.claim', 'a related signature opens in the panel');

  // The multi-line message is shown whole and wrapped, and Copy copies it.
  await rowNamed(LONG_MESSAGE.split('\n')[0]).click();
  panel = await detail();
  assert(panel.message === LONG_MESSAGE && !panel.messageClipped, 'every line of the message is shown, wrapped');
  await page.locator('#diag-body .incident-copy').click();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  assert(copied === LONG_MESSAGE, `Copy copies the full message: ${copied}`);
  assert(panel.actions.join() === 'View run,Audit events', `a single run opens directly: ${panel.actions}`);

  // A message cut before it was stored says so and links its audit events.
  await rowNamed('workspace sync failed').click();
  panel = await detail();
  evidenceLog.truncatedDetail = panel;
  assert(panel.message === CUT_MESSAGE && panel.note !== null, `a cut message carries the note: ${JSON.stringify(panel)}`);
  assert(panel.pills.some((text) => /^Quiet for 3h/.test(text)), `a quiet pill: ${panel.pills}`);
  assert(/^4 events, all 3h( \d+m)? ago\.$/.test(panel.sentence), `a burst reads as one moment: ${panel.sentence}`);
  await page.screenshot({ path: path.join(evidence, 'incidents-detail-1440.png'), fullPage: true });

  // 4. The last-hour insight selects the 1h window and refetches the list.
  const beforeLastHourRequest = incidentRequests.length;
  const lastHourChip = page.locator('#diag-body .incident-insight[data-insight="last-hour"]');
  assert(await lastHourChip.textContent() === '2 signatures failed in the last hour', 'the chip counts live signatures');
  await lastHourChip.click();
  await until('the 1h answer', async () => incidentRequests.length > beforeLastHourRequest
    && lastRequest().class === 'unexpected' && lastRequest().since === '1h' && (await rows()).length === 2);
  let filtered = await rows();
  assert(filtered.length === 2 && filtered.every((row) => row.live), `the 1h window keeps the recent rows: ${JSON.stringify(filtered.map((row) => row.headline))}`);
  assert(await page.locator('#diag-body .incident-seg[data-window="1h"]').getAttribute('aria-pressed') === 'true', 'the chip selects the 1h window segment');
  assert(await lastHourChip.getAttribute('aria-pressed') === 'true', 'the chip shows it is on');
  await page.locator('#diag-body .incident-toolbar').screenshot({ path: path.join(evidence, 'incidents-toolbar-1440.png') });
  const beforeRestoreRequest = incidentRequests.length;
  await page.locator('#diag-body .incident-seg[data-window="24h"]').click();
  await until('the restored 24h answer', async () => incidentRequests.length > beforeRestoreRequest
    && lastRequest().since === '24h' && (await rows()).length === 8);

  // No-message chip: only the silent signature, selected.
  await page.locator('#diag-body .incident-insight[data-insight="no-message"]').click();
  filtered = await rows();
  assert(filtered.length === 1 && filtered[0].pressed === 'true', 'the no-message chip selects the silent signature');
  assert((await detail()).message === 'orbit.run.start failed; no message recorded', 'a missing message says so instead of showing the signature');
  await page.locator('#diag-body .incident-insight[data-insight="no-message"]').click();

  // Search narrows by run id as well as message.
  await page.locator('#incident-search').fill('run-claim-1');
  filtered = await rows();
  assert(filtered.length === 1 && filtered[0].sub.startsWith('orbit.task.claim'), 'search matches a run id');
  await page.locator('#incident-search').fill('');

  // 5. The truncation note opens the exact audit events in Incidents › Events.
  await rowNamed('workspace sync failed').click();
  await page.locator('#diag-body .incident-message-note button').click();
  await until('the Events drill-down', () => page.evaluate(() => location.hash.startsWith('#diagnostics/incidents/events?')));
  const auditHash = await page.evaluate(() => decodeURIComponent(location.hash));
  assert(auditHash.includes('ids=2001,2002,2003,2004'), `the drill-down names every event: ${auditHash}`);
  assert(await page.locator('#diagnostics-events-main').isVisible()
    && await page.locator('#incidents-views .subtab.active').getAttribute('data-view') === 'events', 'the drill-down opens the Events view of Incidents');

  // 6. By cause, the default: signatures sharing a cause sit under one
  //    header that counts signatures, a cause with one signature is one row
  //    carrying its code, and By signature restores the flat list.
  await page.goto(`${origin}/?workspace=ws_orbit#diagnostics/incidents?window=24h`);
  await page.locator('#diag-body .incident-row').first().waitFor({ state: 'visible' });
  const modes = () => page.locator('#diag-body .incident-modes .incident-seg').evaluateAll((nodes) => nodes.map((node) => `${node.textContent}:${node.getAttribute('aria-pressed')}`));
  const listTitle = () => page.locator('#diag-body .incident-list-title').textContent();
  assert((await modes()).join() === 'By cause:true,By signature:false', `the list starts grouped by cause: ${await modes()}`);
  assert(/8 signatures in 7 causes$/.test(await listTitle()), `the heading counts signatures and causes: ${await listTitle()}`);
  const grouped = await listing();
  evidenceLog.byCause = grouped;
  const heads = grouped.filter((item) => item.head);
  assert(heads.length === 1 && grouped.length === 9, `one header over the shared cause, every signature still listed: ${JSON.stringify(grouped)}`);
  const headAt = grouped.indexOf(heads[0]);
  assert(headAt === 5, `the cause sorts by its newest signature: ${headAt}`);
  assert(heads[0].headline === VALIDATE_MESSAGE && heads[0].code === 'action `fixture_validate`', `the header names the cause: ${JSON.stringify(heads[0])}`);
  assert(heads[0].count === '2 sigs' && heads[0].marks === 2 && /^2 signatures · first 11h( \d+m)? ago$/.test(heads[0].sub),
    `the header counts signatures and draws each one's lifespan: ${JSON.stringify(heads[0])}`);
  assert(!heads[0].interactive, 'the header is a label, not a control');
  const [waitItem, workerItem, sandboxItem] = grouped.slice(headAt + 1);
  assert(waitItem.child && workerItem.child && waitItem.sub === 'fixture.wait · admin' && workerItem.sub.startsWith('job worker · human'),
    `the cause's signatures are indented beneath it, newest first: ${JSON.stringify([waitItem, workerItem])}`);
  assert(!sandboxItem.child && !sandboxItem.head, 'a single-signature cause is one row');
  let causeRows = await rows();
  const sandboxRow = causeRows.find((row) => row.headline === '[blocked_by_agent] kind=sandbox_refused');
  assert(sandboxRow && sandboxRow.code === 'kind=sandbox_refused', `the single row carries its cause code: ${JSON.stringify(sandboxRow)}`);
  assert(causeRows.filter((row) => row.code !== null).length === 1, 'rows grouped by their own signature show no cause code');
  const [waitRow, workerRow] = causeRows.filter((row) => row.child);
  assert(waitRow.count === '1 event' && workerRow.count === '2 runs', `counts stay per signature: ${waitRow.count} / ${workerRow.count}`);

  // The detail panel lists the other signature with this cause and says why.
  await page.locator('#diag-body .incident-row.child').first().click();
  panel = await detail();
  evidenceLog.sameCauseDetail = panel;
  assert(panel.facts.Surface === 'fixture.wait' && panel.facts['Cause code'] === 'action `fixture_validate`', `the facts name the cause: ${JSON.stringify(panel.facts)}`);
  assert(panel.basis === 'Grouped by the same step and message: action `fixture_validate`', `the grouping basis is shown: ${panel.basis}`);
  assert(panel.sameCause.length === 1 && panel.sameCause[0].startsWith('job worker · human') && panel.sameCause[0].includes('2 runs'),
    `the same-cause list names the other signature with its own count: ${panel.sameCause}`);
  assert(panel.related.length === 1, `a same-cause sibling is not listed again as a same-message one: ${panel.related}`);
  await page.locator('#diag-body .incident-same-cause .incident-related-item').click();
  panel = await detail();
  assert(panel.facts.Surface === 'job worker' && panel.actions.join() === 'View runs (2),Audit events', `the sibling opens with its own drill-downs: ${JSON.stringify(panel)}`);
  assert((await rows()).find((row) => row.pressed === 'true').sub.startsWith('job worker'), 'the sibling row is selected');

  // By signature: no headers or indentation, the cause code after surface
  // and actor, and the selection kept.
  await page.locator('#diag-body .incident-modes .incident-seg[data-mode="signature"]').click();
  assert((await modes()).join() === 'By cause:false,By signature:true', 'the toggle shows the flat list is on');
  const flat = await listing();
  evidenceLog.bySignature = flat;
  assert(flat.length === 8 && flat.every((item) => !item.head && !item.child), `the flat list has one row per signature: ${JSON.stringify(flat)}`);
  assert(flat[5].sub === 'fixture.wait · admin · action `fixture_validate`' && flat[7].sub === 'fixture.wait · admin · kind=sandbox_refused',
    `flat rows name their cause: ${JSON.stringify(flat.map((item) => item.sub))}`);
  assert(/8 signatures$/.test(await listTitle()), `the flat heading counts signatures only: ${await listTitle()}`);
  assert((await rows()).find((row) => row.pressed === 'true').sub.startsWith('job worker'), 'the selection survives the toggle');
  await page.screenshot({ path: path.join(evidence, 'incidents-by-signature-1440.png'), fullPage: true });
  await page.locator('#diag-body .incident-modes .incident-seg[data-mode="cause"]').click();
  assert((await listing()).filter((item) => item.head).length === 1, 'By cause restores the headers');
  await page.locator('#diag-body .incident-cause-head').scrollIntoViewIfNeeded();
  await page.screenshot({ path: path.join(evidence, 'incidents-by-cause-1440.png'), fullPage: true });

  // 7. A class tile asks the server for that class and filters the list.
  await page.goto(`${origin}/?workspace=ws_orbit#diagnostics/incidents?window=24h`);
  await page.locator('#diag-body .incident-row').first().waitFor({ state: 'visible' });
  await page.locator('#diag-body .incident-tile[data-class="expected"]').click();
  await until('expected rows', async () => (await rows()).length === 1);
  assert(lastRequest().class === 'expected', `the tile asks for its class: ${JSON.stringify(lastRequest())}`);
  assert((await tiles()).find((tile) => tile.cls === 'expected').pressed === 'true', 'the chosen tile is pressed');
  assert((await rows())[0].headline === 'transition refused: task is not in review', 'only the chosen class is listed');
  // The feed cut this message to its cap: the panel shows the start and
  // fetches the rest from its audit row only when asked.
  panel = await detail();
  evidenceLog.cappedDetail = { note: panel.note, length: panel.message.length };
  assert(panel.message === REFUSAL_MESSAGE.slice(0, LIST_MESSAGE_MAX_BYTES) && panel.note !== null, `a capped message shows its start and a note: ${JSON.stringify(evidenceLog.cappedDetail)}`);
  assert(messageRequests.length === 0, 'the full message is not fetched until asked for');
  await page.locator('#diag-body .incident-message-note button').click();
  await until('the full message', async () => (await detail()).message === REFUSAL_MESSAGE);
  assert((await detail()).note === null && messageRequests.length === 1, `one fetch loads the full message: ${messageRequests}`);
  await page.locator('#diag-body .incident-copy').click();
  assert(await page.evaluate(() => navigator.clipboard.readText()) === REFUSAL_MESSAGE, 'Copy copies the loaded full message');

  // 8. A long list keeps the detail panel in view while it scrolls.
  await page.locator('#diag-body .incident-tile[data-class="denied"]').click();
  await until('denied rows', async () => (await rows()).length === 30);
  const related = await detail();
  assert(related.related.length === 5, `related signatures are capped at five: ${related.related.length}`);
  assert(await page.locator('#diag-body .incident-related-more').textContent() === '24 more signatures share this message.', 'the rest are counted');
  const sticky = await page.evaluate(async () => {
    const panelNode = document.querySelector('#diag-body .incident-detail-panel');
    const master = document.querySelector('#diag-body .incident-master');
    const pane = panelNode.closest('.tab-pane');
    const before = panelNode.getBoundingClientRect().top;
    pane.scrollTop = before - pane.getBoundingClientRect().top + 600;
    await new Promise(requestAnimationFrame);
    return {
      height: panelNode.offsetHeight, viewport: innerHeight,
      before, scrolled: pane.scrollTop, pane: pane.getBoundingClientRect().top,
      after: panelNode.getBoundingClientRect().top, masterTop: master.getBoundingClientRect().top, position: getComputedStyle(panelNode).position,
    };
  });
  evidenceLog.sticky = sticky;
  assert(sticky.scrolled > 0 && sticky.masterTop < sticky.pane && sticky.after - sticky.pane >= 0 && sticky.after - sticky.pane <= 40,
    `the detail panel sticks while the list scrolls: ${JSON.stringify(sticky)}`);
  await page.screenshot({ path: path.join(evidence, 'incidents-sticky-1440.png') });
  await page.evaluate(() => { document.querySelector('#diag-body .incident-detail-panel').closest('.tab-pane').scrollTop = 0; });
  await page.screenshot({ path: path.join(evidence, 'incidents-denied-1440.png'), fullPage: true });

  // 9. 6h is this page's own window: the request narrows, the shared window
  //    does not move, and the axis is relabelled.
  await page.locator('#diag-body .incident-tile[data-class="unexpected"]').click();
  await until('unexpected rows', async () => (await rows()).length === 8);
  await page.locator('#diag-body .incident-seg[data-window="6h"]').click();
  await until('the 6h answer', async () => lastRequest().since === '6h' && (await rows()).length === 4);
  assert(await page.locator('#diag-body .incident-seg[data-window="6h"]').getAttribute('aria-pressed') === 'true', 'the 6h segment is pressed');
  const sixAxis = await page.locator('#diag-body .incident-axis-row .incident-axis').evaluate((node) => [...node.children].map((child) => child.textContent));
  assert(sixAxis.join() === '6h ago,3h,now', `the axis follows the window: ${sixAxis}`);
  const sixHour = (await rows())[0];
  assert(sixHour.count === '3 runs', `only the runs inside the window count: ${sixHour.count}`);
  near(sixHour.leftPct, (6 * HOUR - (4 * HOUR + 20 * MINUTE)) / (6 * HOUR) * 100, 1, 'the bar is placed on the 6h axis');
  assert(new URL(page.url()).hash.includes('window=24h'), `the shared window stays 24h: ${page.url()}`);

  // 10. A phone: tiles, rows and the panel stack with no horizontal scroll.
  for (const width of [768, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await page.waitForTimeout(150);
    const overflow = await noPageOverflow();
    assert(overflow <= 1, `no horizontal page scroll at ${width}px (by ${overflow}px)`);
    const stack = await page.evaluate(() => {
      const master = document.querySelector('#diag-body .incident-master').getBoundingClientRect();
      const panelNode = document.querySelector('#diag-body .incident-detail-panel');
      const box = panelNode.getBoundingClientRect();
      const body = document.getElementById('diag-body');
      const clipped = [...document.querySelectorAll('#diag-body .incident-row, #diag-body .incident-tile, #diag-body .incident-detail-panel')]
        .filter((node) => node.getBoundingClientRect().right > body.getBoundingClientRect().right + 1).length;
      return { below: box.top >= master.bottom - 1, position: getComputedStyle(panelNode).position, clipped, bodyScroll: body.scrollWidth - body.clientWidth };
    });
    assert(stack.below && stack.position === 'static' && stack.clipped === 0 && stack.bodyScroll <= 1, `the panel stacks below the list at ${width}px: ${JSON.stringify(stack)}`);
    await page.screenshot({ path: path.join(evidence, `incidents-${width}.png`), fullPage: true });
    for (const [part, selector] of [['list', '.incident-master'], ['detail', '.incident-detail-panel']]) {
      await page.locator(`#diag-body ${selector}`).evaluate((node) => node.scrollIntoView({ block: 'start' }));
      await page.screenshot({ path: path.join(evidence, `incidents-${width}-${part}.png`) });
    }
  }

  // 11. A completed message cache follows the selected host in both All
  // workspaces and a concrete workspace shared by the two stores.
  await page.setViewportSize({ width: 1440, height: 900 });
  const messageButton = () => page.locator('#diag-body .incident-message-note button');
  const copyMessage = async () => {
    await page.locator('#diag-body .incident-copy').click();
    return page.evaluate(() => navigator.clipboard.readText());
  };
  const aggregateScope = () => new URL(page.url()).pathname === '/message-scope';
  const renderAggregate = () => page.evaluate(async () => {
    const { common, incidents } = window.incidentScope;
    incidents.renderIncidents(await common.fetchJson(`/api/audit/incidents?since=24h&class=${incidents.getIncidentClass()}&limit=50`));
  });
  const openRefusal = async (workspace) => {
    if (!workspace) {
      await page.goto(`${origin}/message-scope?workspace=all`);
      await page.evaluate(async (hosts) => {
        const common = await import('/static/js/common.js');
        const incidents = await import('/static/js/incidents.js');
        common.setRegisteredHosts(hosts);
        common.setMultiWorkspace(true);
        window.incidentScope = { common, incidents };
      }, HOSTS.hosts);
      await renderAggregate();
    } else {
      await page.goto(`${origin}/?host=alpha&workspace=${workspace}#diagnostics/incidents?window=24h`);
    }
    await page.locator('#diag-body .incident-tile[data-class="expected"]').click();
    if (aggregateScope()) await renderAggregate();
    await until('alpha refusal excerpt', async () => (await detail()).message === REFUSAL_MESSAGE.slice(0, LIST_MESSAGE_MAX_BYTES));
  };
  const switchHost = async (host) => {
    if (aggregateScope()) {
      await page.evaluate((host) => window.incidentScope.common.setHost(host), host);
      await renderAggregate();
    } else {
      await page.locator('#host-select').selectOption(host);
    }
    const message = refusalMessage(host).slice(0, LIST_MESSAGE_MAX_BYTES);
    await until(`${host || 'alpha'} refusal excerpt`, async () => (await detail()).message === message && (await detail()).note !== null);
  };
  const loadMessage = async (message) => {
    await messageButton().click();
    await until('loaded message', async () => (await detail()).message === message);
    assert((await detail()).note === null && await copyMessage() === message, 'the panel and Copy use the loaded message');
  };
  for (const workspace of ['', 'ws_orbit']) {
    await openRefusal(workspace);
    await loadMessage(REFUSAL_MESSAGE);
    await switchHost('beta');
    const selectedWorkspace = aggregateScope() ? await page.evaluate(() => window.incidentScope.common.getWorkspace() || '') : await page.locator('#workspace-select').inputValue();
    assert(selectedWorkspace === workspace, 'the workspace selection survives the host switch');
    await loadMessage(BETA_REFUSAL_MESSAGE);
    assert(JSON.stringify(messageRequests.at(-1)) === JSON.stringify({ host: 'beta', workspace, path: `/api/on/beta/audit/events/${REFUSAL_EVENT_ID}/message` }), 'the second store reads its own audit row, including in All workspaces');
    const loaded = messageRequests.length;
    const requests = incidentRequests.length;
    if (aggregateScope()) await renderAggregate();
    else await page.locator('#refresh-btn').click();
    await until('a same-scope refresh', () => incidentRequests.length > requests && lastRequest().host === 'beta');
    await rowNamed(BETA_REFUSAL_MESSAGE.split('\n')[0]).click();
    assert((await detail()).message === BETA_REFUSAL_MESSAGE && messageRequests.length === loaded, 'a loaded message stays cached within its scope');
  }

  // 12. Hold alpha's response until beta has loaded the same audit row ID.
  // Its late completion must leave both beta's panel and Copy untouched.
  await openRefusal('');
  const oldHostMessage = holdNextMessage('alpha', '');
  await messageButton().click();
  await until('held alpha message', () => oldHostMessage.release);
  await switchHost('beta');
  await loadMessage(BETA_REFUSAL_MESSAGE);
  let settlements = await page.evaluate(() => window.messageSettlements);
  oldHostMessage.release();
  await until('late alpha message settled', () => page.evaluate((count) => window.messageSettlements > count, settlements));
  assert((await detail()).message === BETA_REFUSAL_MESSAGE && await copyMessage() === BETA_REFUSAL_MESSAGE, 'a late alpha response cannot replace beta display or Copy');

  // 13. Returning to the same host/workspace is a new visit. Even though
  // the key matches, an old success or error cannot settle a newer load.
  const visitCases = [];
  evidenceLog.messageScope = { messageRequests, routeLog, visitCases };
  for (const boundary of ['host', 'workspace']) {
    for (const refused of [false, true]) {
      evidenceLog.currentVisit = { boundary, refused };
      await openRefusal('ws_orbit');
      const oldMessage = holdNextMessage('alpha', 'ws_orbit');
      await messageButton().click();
      await until('held old-visit message', () => oldMessage.release);
      if (boundary === 'host') {
        await switchHost('beta');
        await switchHost('');
      } else {
        const requests = incidentRequests.length;
        await page.locator('#workspace-select').selectOption('ws_other');
        await until('other workspace feed', () => incidentRequests.slice(requests).some((request) => request.workspace === 'ws_other'));
        await page.locator('#diag-body .incident-row').first().waitFor({ state: 'visible' });
        await page.locator('#workspace-select').selectOption('ws_orbit');
        await until('returned workspace excerpt', async () => (await detail()).message === REFUSAL_MESSAGE.slice(0, LIST_MESSAGE_MAX_BYTES));
      }
      const newerMessage = holdNextMessage('alpha', 'ws_orbit');
      await messageButton().click();
      await until('held new-visit message', () => newerMessage.release);
      settlements = await page.evaluate(() => window.messageSettlements);
      oldMessage.release(refused);
      await until('old visit settled', () => page.evaluate((count) => window.messageSettlements > count, settlements));
      assert(await messageButton().isDisabled(), `${boundary} round trip: the newer message request still owns Loading after old ${refused ? 'error' : 'success'}`);
      assert((await detail()).message === REFUSAL_MESSAGE.slice(0, LIST_MESSAGE_MAX_BYTES) && await copyMessage() === REFUSAL_MESSAGE.slice(0, LIST_MESSAGE_MAX_BYTES), `${boundary} round trip: the late response cannot replace the excerpt or Copy`);
      newerMessage.release();
      await until('new visit full message', async () => (await detail()).message === REFUSAL_MESSAGE);
      assert(await copyMessage() === REFUSAL_MESSAGE, 'the current visit completes normally');
      visitCases.push({ boundary, refused });
    }
  }

  assert(pageErrors.length === 0, `no page errors: ${pageErrors.join('; ')}`);
  fs.writeFileSync(path.join(evidence, 'incidents-assertions.json'), `${JSON.stringify({
    passed: true,
    assertions: [
      'browser-incidents-class-tiles-count-and-filter',
      'browser-incidents-denominator-line',
      'browser-incidents-poll-keeps-unchanged-rows',
      'browser-incidents-recurring-lifespan-bar',
      'browser-incidents-single-burst-dot',
      'browser-incidents-detail-full-message-and-copy',
      'browser-incidents-truncation-note-opens-audit-events',
      'browser-incidents-related-signatures',
      'browser-incidents-last-hour-chip-selects-1h-window',
      'browser-incidents-no-message-filter',
      'browser-incidents-local-6h-window',
      'browser-incidents-sticky-detail-panel',
      'browser-incidents-phone-no-horizontal-scroll',
      'browser-incidents-cause-headers-group-signatures',
      'browser-incidents-by-signature-toggle',
      'browser-incidents-same-cause-signatures-and-basis',
      'browser-incidents-capped-message-loads-full-text',
      'browser-incidents-full-message-cache-follows-host',
      'browser-incidents-late-message-keeps-selected-host',
      'browser-incidents-message-host-round-trip',
      'browser-incidents-message-workspace-round-trip',
    ],
    viewports: ['1440', '768', '375'],
    ...evidenceLog,
  }, null, 2)}\n`);
  console.log('Dashboard incidents browser checks passed at 1440px, 768px and 375px: tiles, lifespan bars, detail panel, truncation note, filters, cause grouping and its toggle, 6h window and sticky panel.');
} catch (error) {
  fs.writeFileSync(path.join(evidence, 'incidents-assertions.json'), JSON.stringify({ passed: false, error: error.message, pageErrors, messageRequests, routeLog, currentDetail: page ? await detail().catch(() => null) : null, ...evidenceLog }, null, 2));
  await page?.screenshot({ path: path.join(evidence, 'incidents-failure.png'), fullPage: true }).catch(() => {});
  throw error;
} finally {
  await browser?.close();
  for (const socket of sockets) socket.destroy();
  await new Promise((resolve) => server.close(resolve));
}
