// Usage: node dashboard_provider_limits_browser.mjs /absolute/path/to/playwright/index.mjs /evidence/directory
//
// [ORB-14698] Provider usage limits in Chromium. The shipped app runs against
// a disposable fixture server whose `/api/config/effective` crews carry the
// server's per-crew `limit` and whose `/api/workflows/auto/readiness` carries
// `provider_limits` and a task waiting with reason `provider_limit`. It checks
// Settings › Crews' Limit column (used percent, window, reset and the gated
// badge, a dash for an uncovered crew) and the Drain card's provider-limit
// note and waiting task with its detail, under the light and the dark colour
// scheme, and both views against a server that predates provider limits. No
// Orbit store, dashboard or host is contacted.
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
const RESETS = new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString();
const CODEX_RESETS = new Date(Date.now() + 3 * 60 * 60 * 1000).toISOString();
const OBSERVED = new Date(Date.now() - 5 * 60 * 1000).toISOString();
const HOSTS = {
  host_file: '/home/fixture/.orbit/hosts.toml', legacy: false, generation: 1, load_error: null,
  host_edit: { authorized: true, reason: null },
  hosts: [{
    name: SERVING, machine_id: 'hm_alpha', ssh: null, task_prefix: 'ALP', local: true, legacy: false,
    reachable: true, error: null, binary_version: VERSION, protocol_fingerprint: 'fp', skew: false, skew_fields: [], workspaces: [],
  }],
};
const WORKSPACES = [{ id: 'ws_orbit', name: 'orbit', status: 'active', is_default: true }];

const limit = (crew, provider, used, resets, gated) => ({
  crew, provider, scope: null, window: 'five_hour', used_percent: used, exhausted: false,
  resets_at: resets, threshold: 90, gated, until: resets,
});
const crew = (name, provider, model, extra = {}) => ({
  name, enabled: true, provider, model, effort: null, tags: [], description: null,
  source: 'workspace', referenced_by: [], limit: null, ...extra,
});
const EFFECTIVE = {
  scope: 'effective',
  layers: { built_in: { label: 'built-in' }, global: { path: '/home/fixture/.orbit/config.toml', exists: true }, workspace: { path: '/srv/orbit/.orbit/config.toml', exists: true } },
  workspace_binding: null,
  sections: [{ id: 'crews', kind: 'crews', title: 'Crews', blurb: 'Named provider and model pairs.', rows: [], counts: { set: 3, total: 3 } }],
  crews: [
    crew('opus', 'claude', 'opus-model', { referenced_by: ['workflow.system_crew'], limit: limit('opus', 'claude', 93, RESETS, true) }),
    crew('sol', 'codex', 'gpt-6-sol', { referenced_by: ['workflow.default_crew'], limit: limit('sol', 'codex', 40, CODEX_RESETS, false) }),
    crew('gem', 'gemini', 'gem-model'),
  ],
  paths: [],
  crew_fields: ['provider', 'model', 'effort', 'tags', 'description', 'enabled'],
  write_scope_default: 'workspace',
  workspace_file_exists: true,
  review: null,
  config_set: { authorized: true, reason: null },
};
const DETAIL = `claude five_hour at 93% (limit 90%) until ${RESETS}; crews opus skipped`;
const READINESS = {
  controls_authorized: true,
  capacity: { active_leaf_runs: 0, max_active_leaf_runs: 4, free_slots: 4, drain_phase: 'idle' },
  approvals: { enabled: false },
  provider_limits: [
    { provider: 'claude', scope: null, window: 'five_hour', used_percent: 93, exhausted: false, resets_at: RESETS, source: 'event', observed_at: OBSERVED, gating: true, threshold: 90, gated: true, until: RESETS, crews: ['opus'] },
    { provider: 'codex', scope: null, window: 'five_hour', used_percent: 40, exhausted: false, resets_at: CODEX_RESETS, source: 'event', observed_at: OBSERVED, gating: true, threshold: 90, gated: false, until: CODEX_RESETS, crews: ['sol'] },
  ],
  total: 2,
  tasks: [
    { task_id: 'ORB-101', status: 'backlog', eligible: false, reason: 'provider_limit', detail: DETAIL },
    { task_id: 'ORB-102', status: 'backlog', eligible: true, reason: 'ready' },
  ],
};

// An older host's server sends neither `provider_limits` nor a crew `limit`.
let legacy = false;
const withoutLimits = () => ({
  effective: { ...EFFECTIVE, crews: EFFECTIVE.crews.map(({ limit: _limit, ...rest }) => rest) },
  readiness: (({ provider_limits: _limits, ...rest }) => ({ ...rest, tasks: [{ task_id: 'ORB-101', status: 'backlog', eligible: false, reason: 'provider_limit' }] }))(READINESS),
});

let routeLog = [];
function api(req, rest, res) {
  const json = (body, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
  if (req.method !== 'GET') return json({ ok: true });
  if (rest === 'workspaces') return json(WORKSPACES);
  if (rest === 'tasks') return json({ items: [], total: 0, limit: 50, truncated: false });
  if (rest === 'tasks/all') return json({ items: [], total: 0 });
  if (rest === 'tasks/locks') return json({ total_locked: 0, total_tasks: 0, by_task: [] });
  if (rest === 'crews') return json({ default_crew: 'sol', crews: [] });
  if (rest === 'host/resources') return json({ sampled_at: new Date().toISOString(), cpu: { used_pct: 20 }, memory: { used_pct: 20, used_bytes: 1, total_bytes: 5 }, disks: [] });
  if (rest === 'log') return json({ events: [], offset: 0, agent_offset: 0 });
  if (rest === 'log/stream') {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    res.write(': open\n\n');
    return;
  }
  if (rest === 'workflows/auto/readiness') return json(legacy ? withoutLimits().readiness : READINESS);
  if (rest === 'routines') return json({ machine_name: SERVING, capabilities: {}, clock: {}, routines: [] });
  if (rest.startsWith('runs')) return json([]);
  if (rest === 'audit/summary') return json({ window: '24h', events: 0, failed_runs: 0 });
  if (rest === 'doctor') return json({ ran_at: null, age_ms: null, duration_ms: null, failures: null, warnings: null, checks: null });
  if (rest === 'config/effective') return json(legacy ? withoutLimits().effective : EFFECTIVE);
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
    return json({
      host: SERVING, machine_id: 'hm_alpha', local: true, origin: 'local', binary_version: VERSION, protocol_fingerprint: 'fp',
      skew: false, skew_fields: [], error: null, forward_writes: { authorized: true, reason: null }, reachable: true,
    });
  }
  return api(req, name.slice('/api/'.length), res);
});
const sockets = new Set();
server.on('connection', (socket) => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

const assert = (condition, message) => { if (!condition) throw new Error(message); };
// The tab pane fades in; measure and capture it once it is fully drawn.
const settled = (page) => page.waitForFunction(() => [...document.querySelectorAll('.tab-pane.active')]
  .every((pane) => getComputedStyle(pane).opacity === '1'));

let browser;
const pageErrors = [];
const evidenceLog = { schemes: {} };

// The contrast ratio of two `rgb(...)` colours, by WCAG relative luminance.
const contrast = (fg, bg) => {
  const lum = (rgb) => {
    const [r, g, b] = rgb.match(/[\d.]+/g).slice(0, 3).map(Number).map((v) => {
      const c = v / 255;
      return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const [hi, lo] = [lum(fg), lum(bg)].sort((a, b) => b - a);
  return (hi + 0.05) / (lo + 0.05);
};

try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  for (const scheme of ['light', 'dark']) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles', colorScheme: scheme });
    const page = await context.newPage();
    page.on('pageerror', (error) => pageErrors.push(`${scheme}: ${error.message}`));
    const log = {};

    // 1. Settings › Crews: the Limit column.
    await page.goto(`${origin}/#config/crews`);
    await page.locator('[data-key="crews.opus"] .config-crew-limit').waitFor({ state: 'visible' });
    await settled(page);
    const crews = await page.evaluate(() => {
      const dimmedBy = (node) => {
        const out = [];
        for (let at = node; at; at = at.parentElement) {
          const style = getComputedStyle(at);
          if (style.opacity !== '1' || style.filter !== 'none') out.push(`${at.tagName}.${at.className}#${at.id} ${style.opacity} ${style.filter}`);
        }
        return out;
      };
      const head = [...document.querySelector('.config-crew-head').children].map((cell) => cell.textContent);
      const at = head.indexOf('Limit');
      const row = (name) => {
        const cell = document.querySelector(`[data-key="crews.${name}"] .config-crew-cells`).children[at];
        const badge = cell.querySelector('.config-crew-gated');
        const panel = cell.closest('.panel');
        return {
          text: cell.lastElementChild.textContent,
          gated: Boolean(badge),
          badgeTitle: badge?.title || null,
          badgeColor: badge ? getComputedStyle(badge).color : null,
          valueColor: getComputedStyle(cell.querySelector('.config-value')).color,
          background: getComputedStyle(panel).backgroundColor,
        };
      };
      const cell = document.querySelector('[data-key="crews.opus"] .config-crew-limit');
      const top = document.elementFromPoint(...(() => { const r = cell.getBoundingClientRect(); return [r.left + 4, r.top + 4]; })());
      return { head, at, opus: row('opus'), sol: row('sol'), gem: row('gem'), overflow: document.scrollingElement.scrollWidth > innerWidth, dimmed: dimmedBy(cell), top: `${top.tagName}.${top.className}#${top.id}` };
    });
    log.crews = crews;
    assert(crews.dimmed.length === 0 && crews.top.includes('config-value'), `the Limit cell is drawn on top and undimmed: ${JSON.stringify(crews)}`);
    assert(crews.at === 3, `Limit follows Model in the crew header: ${JSON.stringify(crews.head)}`);
    assert(/^93%five_hour.*gated.*resets /.test(crews.opus.text), `the gated crew shows its use, window, badge and reset: ${crews.opus.text}`);
    assert(crews.opus.gated && /Admission skips this crew until .* \(limit 90%\)/.test(crews.opus.badgeTitle), `the badge says until when: ${crews.opus.badgeTitle}`);
    assert(/^40%five_hourresets /.test(crews.sol.text) && !crews.sol.gated, `a crew below its threshold has no badge: ${crews.sol.text}`);
    assert(crews.gem.text === '—' && !crews.gem.gated, `an uncovered crew shows the placeholder: ${crews.gem.text}`);
    assert(!crews.overflow, 'the crew table fits at 1440px');
    assert(contrast(crews.opus.badgeColor, crews.opus.background) >= 4.5, `the gated badge is legible: ${crews.opus.badgeColor} on ${crews.opus.background}`);
    assert(contrast(crews.opus.valueColor, crews.opus.background) >= 4.5, `the used percent is legible: ${crews.opus.valueColor} on ${crews.opus.background}`);
    await page.screenshot({ path: path.join(evidence, `provider-limits-crews-${scheme}.png`), fullPage: true });

    // 2. The Drain card: the gated providers and the task waiting on one.
    await page.goto(`${origin}/#auto-drain`);
    await page.locator('#auto-drain-body .drain-provider-limit-note').waitFor({ state: 'visible' });
    await settled(page);
    const drain = await page.evaluate(() => {
      const card = document.getElementById('auto-drain-panel');
      const note = card.querySelector('.drain-provider-limit-note');
      const list = card.querySelector('.drain-provider-limit-waits');
      const box = card.getBoundingClientRect();
      return {
        note: note.textContent,
        noteColor: getComputedStyle(note).color,
        background: getComputedStyle(card).backgroundColor,
        label: list?.getAttribute('aria-label'),
        rows: [...(list?.querySelectorAll('.drain-blocked-row') || [])].map((row) => row.textContent),
        overflowing: [...card.querySelectorAll('*')]
          .filter((node) => node.getClientRects().length > 0 && node.getBoundingClientRect().right > box.right + 1)
          .map((node) => node.className || node.tagName),
      };
    });
    log.drain = drain;
    assert(/^Provider limits: claude five_hour 93% ≥ 90% until .+: opus skipped\.$/.test(drain.note), `the note names the gated provider: ${drain.note}`);
    assert(!drain.note.includes('codex'), `a provider below its threshold holds nothing: ${drain.note}`);
    assert(drain.label === 'Tasks waiting for a provider usage limit to reset', `the waiting list is labelled: ${drain.label}`);
    assert(drain.rows.length === 1 && drain.rows[0].includes('ORB-101') && drain.rows[0].includes('provider limit') && drain.rows[0].includes(DETAIL), `the waiting task shows its detail: ${JSON.stringify(drain.rows)}`);
    assert(drain.overflowing.length === 0, `the Drain card content fits: ${JSON.stringify(drain.overflowing)}`);
    assert(contrast(drain.noteColor, drain.background) >= 4.5, `the note is legible: ${drain.noteColor} on ${drain.background}`);
    await page.screenshot({ path: path.join(evidence, `provider-limits-drain-${scheme}.png`), fullPage: true });

    log.colorScheme = await page.evaluate(() => getComputedStyle(document.documentElement).colorScheme);
    evidenceLog.schemes[scheme] = log;
    await context.close();
  }

  // 3. Failure: a server that predates provider limits. The column shows the
  //    placeholder, the card shows no note, and a `provider_limit` task with
  //    no detail still says what it waits for.
  legacy = true;
  {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
    const page = await context.newPage();
    page.on('pageerror', (error) => pageErrors.push(`legacy: ${error.message}`));
    await page.goto(`${origin}/#config/crews`);
    await page.locator('[data-key="crews.opus"] .config-crew-limit').waitFor({ state: 'visible' });
    const cells = await page.evaluate(() => [...document.querySelectorAll('.config-crew-limit')].map((cell) => cell.textContent));
    assert(cells.length === 3 && cells.every((text) => text === '—'), `without limits every crew shows the placeholder: ${JSON.stringify(cells)}`);
    await page.goto(`${origin}/#auto-drain`);
    await page.locator('#auto-drain-body .drain-provider-limit-waits').waitFor({ state: 'visible' });
    const legacyDrain = await page.evaluate(() => ({
      note: Boolean(document.querySelector('.drain-provider-limit-note')),
      rows: [...document.querySelectorAll('.drain-provider-limit-waits .drain-blocked-row')].map((row) => row.textContent),
    }));
    evidenceLog.legacy = { cells, ...legacyDrain };
    assert(!legacyDrain.note, 'no provider-limit note without readings');
    assert(legacyDrain.rows.length === 1 && legacyDrain.rows[0].includes('waits for a provider usage limit to reset'), `a task with no detail names its wait: ${JSON.stringify(legacyDrain.rows)}`);
    await context.close();
  }

  assert(pageErrors.length === 0, `no page errors: ${pageErrors.join('; ')}`);
  assert(routeLog.every((entry) => entry.method === 'GET'), `the views only read: ${JSON.stringify(routeLog.filter((entry) => entry.method !== 'GET'))}`);
  fs.writeFileSync(path.join(evidence, 'provider-limits-assertions.json'), `${JSON.stringify({
    passed: true,
    assertions: [
      'browser-crews-limit-column-shows-use-window-reset-and-gated-badge',
      'browser-crews-limit-column-placeholder-for-uncovered-crew',
      'browser-drain-card-names-gated-providers',
      'browser-drain-card-lists-provider-limit-waits-with-detail',
      'browser-provider-limits-legible-in-light-and-dark-schemes',
      'browser-provider-limits-absent-on-older-server',
    ],
    ...evidenceLog,
  }, null, 2)}\n`);
  console.log('Dashboard provider-limit browser checks passed in the light and dark schemes and against an older server: Crews Limit column and Drain card provider_limit reasons.');
} catch (error) {
  fs.writeFileSync(path.join(evidence, 'provider-limits-assertions.json'), JSON.stringify({ passed: false, error: error.message, pageErrors, ...evidenceLog }, null, 2));
  throw error;
} finally {
  await browser?.close();
  for (const socket of sockets) socket.destroy();
  await new Promise((resolve) => server.close(resolve));
}
