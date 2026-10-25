// Usage: node dashboard_context_basename_color_browser.mjs /absolute/path/to/playwright/index.mjs .orbit/tmp/context-basename-browser
// Exercises the shipped task detail renderer and CSS: a contended context selector's basename keeps
// its contention colour (amber backlog, red active), including root-level and duplicate-basename
// files, and returns to the normal colour after a refresh clears contention.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

// Expected contention colours are the theme's warn (backlog) and danger
// (active) foregrounds, probed from the shipped CSS below.

const task = (id, status, context_files) => ({ id, status, context_files, title: `Basename fixture ${id}`, priority: 'medium', complexity: 'low',
  description: 'Disposable browser scenario', plan: 'Inspect contexts', comments: [], history: [], artifacts: [], tags: [], status_transitions: [] });
const tasks = [
  // Root-level file, two duplicate basenames, and one uncontended file.
  task('BASE-1', 'backlog', ['file:Makefile', 'file:src/shared.rs', 'file:tests/shared.rs', 'file:docs/notes.md']),
  task('BASE-2', 'in-progress', ['file:Makefile', 'dir:src']),
  task('BASE-3', 'in-progress', ['file:src/shared.rs', 'file:tests/shared.rs']),
];
// Selectors whose matches are reported by the server. Clearing it models contention ending.
const contended = new Set(['file:Makefile', 'file:src/shared.rs', 'file:tests/shared.rs', 'dir:src']);
const list = items => ({ items, total: items.length, limit: 100, truncated: false });
function locks() {
  const active = tasks.filter(item => item.status === 'in-progress');
  const context_contention = {};
  for (const item of tasks.filter(item => ['backlog', 'in-progress'].includes(item.status))) {
    context_contention[item.id] = { status: item.status, entries: item.context_files.map(selector => ({
      selector, known: true,
      contenders: contended.has(selector)
        ? active.filter(holder => holder.id !== item.id).map(holder => ({ id: holder.id, status: holder.status, selector: holder.context_files[0] }))
        : [],
    })) };
  }
  return { by_task: active.map(item => ({ id: item.id, context_files: item.context_files })), by_reservation: [], locked_files: [...new Set(active.flatMap(item => item.context_files))],
    total_locked: new Set(active.flatMap(item => item.context_files)).size, total_tasks: active.length, total_reservations: 0, context_contention };
}
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  url.pathname = url.pathname.replace(/^\/api\/on\/[^/]+\//, '/api/');
  if (url.pathname.startsWith('/api/')) {
    res.setHeader('content-type', 'application/json');
    let payload = list([]);
    if (url.pathname === '/api/workspaces') payload = [{ id: 'ws_fixture', name: 'fixture', status: 'active', is_default: true }];
    if (url.pathname === '/api/hosts') payload = { generation: 1, hosts: [
      { name: 'serving-host', machine_id: 'hm_serving', local: true, reachable: true },
      { name: 'fixture-host', machine_id: 'hm_fixture', local: false, reachable: true, workspaces: [] },
    ], forward_writes: { authorized: true, reason: null } };
    if (url.pathname === '/api/hosts/fixture-host/connection') payload = { reachable: true, forward_writes: { authorized: true, reason: null } };
    if (url.pathname === '/api/crews') payload = { crews: [], default_crew: null };
    if (url.pathname === '/api/tasks') payload = list(tasks);
    if (url.pathname === '/api/tasks/all') payload = list(tasks.map(item => ({ ...item, workspace_id: 'ws_fixture', workspace_name: 'fixture' })));
    const match = tasks.find(item => url.pathname === `/api/tasks/${item.id}`);
    if (match) payload = match;
    if (url.pathname === '/api/tasks/locks') payload = locks();
    if (url.pathname === '/api/workflows/auto/readiness') payload = { tasks: [], capacity: {} };
    res.end(JSON.stringify(payload));
    return;
  }
  const served = dashboardFile(url.pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  res.setHeader('content-type', served.type);
  res.end(served.data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
const results = [];
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto(`http://127.0.0.1:${server.address().port}/?host=fixture-host&workspace=ws_fixture&window=24h#tasks`);
  const open = async id => {
    // The open detail pane is modal over the list, so select tasks through the scoped hash.
    if (!await page.locator(`#detail-${id}`).count()) {
      await page.evaluate(id => { location.hash = `#tasks?open=${id}`; }, id);
      await page.locator(`#detail-${id}`).waitFor();
    }
    await page.locator(`#detail-${id} [role="tab"][data-tab="context_files"]`).click();
    await page.locator(`#detail-${id} .file-list`).waitFor();
  };
  const refresh = async () => {
    const response = page.waitForResponse(response => new URL(response.url()).pathname.endsWith('/tasks/locks'));
    await page.locator('#refresh-btn').click();
    await response;
    await page.waitForFunction(() => !document.getElementById('refresh-btn').disabled);
  };
  const contextBlock = id => page.locator(`#detail-${id} .field-block`).filter({ has: page.locator('.file-list') });
  // Computed colour of each basename and its selector text, keyed by the full selector.
  const basenames = id => page.locator(`#detail-${id} .file-list .context-selector`).evaluateAll(links => Object.fromEntries(links.map(link => {
    const base = link.querySelector('.selector-base');
    const selector = (link.getAttribute('title') || '').split(' · ')[0];
    return [selector, { text: base.textContent, color: getComputedStyle(base).color }];
  })));
  const probeColor = variable => page.evaluate(variable => {
    const probe = document.createElement('span');
    probe.style.color = `var(${variable})`;
    document.body.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return color;
  }, variable);
  const normal = await probeColor('--fg-strong');
  const AMBER = await probeColor('--warn-fg');
  const RED = await probeColor('--danger-fg');

  await open('BASE-1');
  await contextBlock('BASE-1').screenshot({ path: path.join(evidence, 'backlog-dark-1440.png'), animations: 'disabled' });
  const backlog = await basenames('BASE-1');
  const duplicateRows = await page.locator('#detail-BASE-1 .file-list .cf-row').evaluateAll(rows => rows.filter(row => row.querySelector('.selector-base')?.textContent === 'shared.rs').map(row => ({ selector: (row.querySelector('.context-selector')?.getAttribute('title') || '').split(' · ')[0], group: row.closest('.cf-group')?.querySelector('.cf-group-name')?.textContent })));
  assert.equal(duplicateRows.length, 2, 'both duplicate basenames are rendered');
  assert.deepEqual(duplicateRows.map(row => row.selector).sort(), ['file:src/shared.rs', 'file:tests/shared.rs'], 'duplicate basenames keep their full selector identity in the link titles');
  assert.deepEqual(duplicateRows.map(row => row.group).sort(), ['src', 'tests'], 'duplicate basenames are distinguished by their folder groups, not a duplicate marker');
  assert.equal(backlog['file:Makefile'].text, 'Makefile');
  assert.equal(backlog['file:Makefile'].color, AMBER, 'root-level contended backlog basename keeps amber');
  assert.equal(backlog['file:src/shared.rs'].color, AMBER, 'duplicate contended backlog basename keeps amber');
  assert.equal(backlog['file:tests/shared.rs'].color, AMBER, 'second duplicate contended backlog basename keeps amber');
  assert.equal(backlog['file:docs/notes.md'].color, normal, 'uncontended basename keeps the normal colour');
  results.push({ scenario: 'backlog root-level and duplicate basenames', passed: true, colors: backlog });

  await open('BASE-2');
  await contextBlock('BASE-2').screenshot({ path: path.join(evidence, 'active-dark-1440.png'), animations: 'disabled' });
  const active = await basenames('BASE-2');
  assert.equal(active['file:Makefile'].color, RED, 'root-level contended active basename keeps red');
  assert.equal(active['dir:src'].text, 'src');
  assert.equal(active['dir:src'].color, RED, 'contended active directory-selector basename keeps red');
  await open('BASE-3');
  const duplicateActive = await basenames('BASE-3');
  assert.equal(duplicateActive['file:src/shared.rs'].color, RED, 'duplicate contended active basename keeps red');
  assert.equal(duplicateActive['file:tests/shared.rs'].color, RED, 'second duplicate contended active basename keeps red');
  results.push({ scenario: 'active root-level, directory and duplicate basenames', passed: true, colors: { ...active, ...duplicateActive } });

  contended.clear();
  await refresh();
  for (const id of ['BASE-1', 'BASE-2', 'BASE-3']) {
    await open(id);
    await page.waitForFunction(id => !document.querySelector(`#detail-${id} .context-contended-backlog, #detail-${id} .context-contended-active`), id);
    for (const [selector, { color }] of Object.entries(await basenames(id))) {
      assert.equal(color, normal, `${selector} returns to the normal colour once contention clears`);
    }
  }
  results.push({ scenario: 'contention clears after refresh and basenames return to normal', passed: true, normal });

  await page.setViewportSize({ width: 375, height: 1100 });
  await open('BASE-1');
  await contextBlock('BASE-1').screenshot({ path: path.join(evidence, 'cleared-dark-375.png'), animations: 'disabled' });
  assert.deepEqual(errors, [], 'no browser runtime errors');
  fs.writeFileSync(path.join(evidence, 'results.json'), JSON.stringify({ passed: true, results }, null, 2));
  process.stdout.write(JSON.stringify({ passed: true, results }) + '\n');
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
