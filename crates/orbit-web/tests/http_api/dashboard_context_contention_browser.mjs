// Run with the host Playwright kit and a checkout-local evidence directory.
// Exercises shipped app polling, detail rendering, CSS and editing with disposable API data.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const selectors = ['file:src/shared.rs', 'dir:src', 'symbol:src/shared.rs#two:function', `file:unrelated/${'long-directory/'.repeat(10)}other.rs`];
const tasks = [
  { id: 'CTX-1', status: 'backlog', context_files: selectors },
  { id: 'CTX-2', status: 'in-progress', context_files: ['dir:src'] },
  { id: 'CTX-3', status: 'in-progress', context_files: ['file:src/shared.rs'] },
  { id: 'CTX-4', status: 'review', context_files: selectors },
].map(task => ({ ...task, title: `Context fixture ${task.id}`, priority: 'medium', complexity: 'low',
  description: 'Disposable browser scenario', plan: 'Inspect contexts', comments: [], history: [], artifacts: [], tags: [], status_transitions: [] }));
let readiness = { task_id: 'CTX-1', eligible: false, reason: 'context_lock_conflict',
  conflicts: [{ locking_task_id: 'CTX-2', requested_file: 'file:src/shared.rs' }, { blocking_task_id: 'CTX-3', requested_file: 'dir:src' }],
  blocking_task_ids: ['CTX-3', 'CTX-2'], detail: 'waits on CTX-2: file:src/shared.rs' };
let locksUnavailable = false;
let matches = true;
const list = items => ({ items, total: items.length, limit: 100, truncated: false });
function locks(workspace) {
  const active = tasks.filter(task => task.status === 'in-progress');
  const context_contention = {};
  for (const task of tasks.filter(task => ['backlog', 'in-progress'].includes(task.status))) {
    context_contention[task.id] = { status: task.status, entries: task.context_files.map((selector, index) => ({
      selector, known: true,
      // These are API results, not a frontend overlap implementation. The HTTP
      // contract fixture separately checks authoritative directory/symbol rules.
      contenders: workspace === 'ws_fixture' && matches && (task.id !== 'CTX-1' || index < 3)
        ? active.filter(holder => holder.id !== task.id).map(holder => ({ id: holder.id, status: holder.status, selector: holder.context_files[0] })) : [],
    })) };
  }
  return { by_task: active, total_locked: 2, total_tasks: active.length, context_contention };
}
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  url.pathname = url.pathname.replace(/^\/api\/on\/[^/]+\//, '/api/');
  if (url.pathname.startsWith('/api/')) {
    res.setHeader('content-type', 'application/json');
    let payload = list([]);
    if (url.pathname === '/api/workspaces') payload = [
      { id: 'ws_fixture', name: 'fixture', status: 'active', is_default: true },
      { id: 'ws_other', name: 'other', status: 'active' },
    ];
    if (url.pathname === '/api/hosts') payload = { generation: 1, hosts: [
      { name: 'serving-host', machine_id: 'hm_serving', local: true, reachable: true },
      { name: 'fixture-host', machine_id: 'hm_fixture', local: false, reachable: true, workspaces: [] },
    ], forward_writes: { authorized: true, reason: null } };
    if (url.pathname === '/api/hosts/fixture-host/connection') payload = { reachable: true, forward_writes: { authorized: true, reason: null } };
    if (url.pathname === '/api/crews') payload = { crews: [], default_crew: null };
    if (url.pathname === '/api/tasks') payload = list(tasks);
    if (url.pathname === '/api/tasks/all') payload = list(tasks.map(task => ({ ...task, workspace_id: 'ws_fixture', workspace_name: 'fixture' })));
    const task = tasks.find(task => url.pathname === `/api/tasks/${task.id}`);
    if (task) payload = task;
    if (url.pathname === '/api/tasks/locks') {
      if (locksUnavailable) { res.writeHead(503); res.end(JSON.stringify({ error: 'lock read unavailable' })); return; }
      payload = locks(url.searchParams.get('workspace'));
    }
    if (url.pathname === '/api/workflows/auto/readiness') payload = { tasks: [readiness], capacity: {} };
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
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto(`http://127.0.0.1:${server.address().port}/?host=fixture-host&workspace=ws_fixture&window=24h#tasks`);
  const open = async id => {
    if (!await page.locator(`#detail-${id}`).count()) await page.locator(`[data-key="task-${id}"] .title`).click();
    await page.locator(`#detail-${id} .file-list`).waitFor();
  };
  const refresh = async () => {
    const response = page.waitForResponse(response => new URL(response.url()).pathname.endsWith('/tasks/locks'));
    await page.locator('#refresh-btn').click();
    await response;
    await page.waitForFunction(() => !document.getElementById('refresh-btn').disabled);
  };
  const highlights = id => page.locator(`#detail-${id} .file-list .context-contended-backlog, #detail-${id} .file-list .context-contended-active`);
  await open('CTX-1');
  await page.waitForFunction(() => document.querySelectorAll('#detail-CTX-1 .context-contended-backlog').length === 3);
  assert.equal(await highlights('CTX-1').count(), 3, 'unrelated entry stays normal');
  await page.evaluate(async () => { (await import('/static/js/task-runtime.js')).setTaskLocks(null); });
  await page.locator('#detail-CTX-1 .context-contention-unknown').first().waitFor();
  assert(((await page.locator('#detail-CTX-1 .context-selector').first().getAttribute('aria-label')) || '').includes('Contention unknown'), 'unknown data is accessible without extra context text');
  await refresh();
  await page.waitForFunction(() => document.querySelectorAll('#detail-CTX-1 .context-contended-backlog').length === 3);
  assert.deepEqual(await page.locator('#detail-CTX-1 .context-selector').allTextContents(), selectors, 'selector text remains intact');
  assert.equal(await page.locator('#detail-CTX-1 .context-contention').count(), 0, 'context contains selectors alone');
  const assertScoped = async (locator, hash) => {
    const href = new URL(await locator.getAttribute('href'), page.url());
    assert.equal(href.hash, hash);
    assert.equal(href.searchParams.get('host'), 'fixture-host');
    assert.equal(href.searchParams.get('workspace'), 'ws_fixture');
    assert.equal(href.searchParams.get('window'), '24h');
  };
  for (const link of await page.locator('#detail-CTX-1 .context-selector').all()) await assertScoped(link, '#tasks?open=CTX-1');
  const drain = page.locator('#detail-CTX-1 .readiness-drain');
  await drain.locator('a').first().waitFor();
  assert.deepEqual(await drain.locator('a').allTextContents(), ['CTX-2', 'CTX-3'], 'all unique blockers linked');
  for (const id of ['CTX-2', 'CTX-3']) await assertScoped(drain.getByRole('link', { name: id, exact: true }), `#tasks?open=${id}`);
  assert.equal(await drain.textContent(), 'Waiting on locksWaits onCTX-2CTX-3', 'one category and one wait, without paths or raw reason');
  await drain.screenshot({ path: path.join(evidence, 'multiple-blockers.png') });
  const measureContrast = node => {
    const rgb = color => color.match(/[\d.]+/g).slice(0, 3).map(Number);
    const luminance = color => rgb(color).map(value => { value /= 255; return value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4; }).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
    const fg = getComputedStyle(node).color;
    const bg = getComputedStyle(node.closest('.field-block')).backgroundColor;
    return { fg, bg, ratio: (luminance(fg) + .05) / (luminance(bg) + .05) };
  };
  const contrast = await page.locator('#detail-CTX-1 .context-selector').first().evaluate(measureContrast);
  assert(contrast.ratio >= 4.5, 'amber text meets normal-text contrast');
  await page.locator('#detail-CTX-1 .field-block').filter({ has: page.locator('.file-list') }).screenshot({ path: path.join(evidence, 'backlog.png'), animations: 'disabled' });
  results.push({ scenario: 'backlog, directory/symbol rows, unrelated, links, contrast', passed: true, contrast });
  await open('CTX-2');
  assert.equal(await page.locator('#detail-CTX-2 .context-contended-active').count(), 1);
  await assertScoped(page.locator('#detail-CTX-2 .context-selector'), '#tasks?open=CTX-2');
  const red = await page.locator('#detail-CTX-2 .context-selector').evaluate(measureContrast);
  assert.notEqual(red.fg, contrast.fg, 'active contention uses distinct red hue');
  assert(red.ratio >= 4.5, 'red text meets normal-text contrast');
  await page.locator('#detail-CTX-2 .field-block').filter({ has: page.locator('.file-list') }).screenshot({ path: path.join(evidence, 'active.png'), animations: 'disabled' });
  await open('CTX-4');
  assert.equal(await highlights('CTX-4').count(), 0, 'review details retain normal hue');
  results.push({ scenario: 'active-active, self exclusion, other viewed status', passed: true, red });
  await open('CTX-1');
  await page.getByRole('button', { name: 'Edit description for CTX-1', exact: true }).click();
  const draft = page.getByRole('textbox', { name: 'Edit description for CTX-1', exact: true });
  await draft.fill('Unsaved description');
  matches = false;
  await refresh();
  await page.waitForFunction(() => !document.querySelector('#detail-CTX-1 .context-contended-backlog'));
  assert.equal(await draft.inputValue(), 'Unsaved description', 'lock refresh preserves another editor draft');
  tasks[0].context_files = [...selectors, 'file:added.rs'];
  await refresh();
  assert.deepEqual(await page.locator('#detail-CTX-1 .context-selector').allTextContents(), tasks[0].context_files, 'viewed context updates while another field has a draft');
  tasks[0].context_files = selectors;
  await refresh();
  matches = true;
  tasks[1].status = 'review';
  tasks[2].status = 'review';
  await refresh();
  assert.equal(await highlights('CTX-1').count(), 0, 'finished active contenders stop highlighting');
  tasks[1].status = 'in-progress';
  await refresh();
  await page.waitForFunction(() => document.querySelectorAll('#detail-CTX-1 .context-contended-backlog').length === 3);
  locksUnavailable = true;
  await refresh();
  await page.locator('#detail-CTX-1 .context-contention-unknown').first().waitFor();
  assert.equal(await highlights('CTX-1').count(), 0, 'failed read clears stale highlights');
  locksUnavailable = false;
  await refresh();
  await page.waitForFunction(() => document.querySelectorAll('#detail-CTX-1 .context-contended-backlog').length === 3);
  tasks[0].status = 'in-progress';
  await refresh();
  await page.waitForFunction(() => document.querySelectorAll('#detail-CTX-1 .context-contended-active').length === 3);
  assert.equal(await draft.inputValue(), 'Unsaved description');
  tasks[0].status = 'backlog';
  await refresh();
  await page.waitForFunction(() => document.querySelectorAll('#detail-CTX-1 .context-contended-backlog').length === 3);
  results.push({ scenario: 'viewed/contender context and status refresh, unknown data, failure/recovery, preserved editor', passed: true });
  await page.locator('#detail-CTX-1 .field-editor .cancel').click();
  await page.getByRole('button', { name: 'Edit context files for CTX-1', exact: true }).click();
  assert.equal(await page.getByRole('textbox', { name: 'Edit context files for CTX-1', exact: true }).inputValue(), selectors.join('\n'), 'editing contains selectors alone');
  await refresh();
  assert.equal(await page.getByRole('textbox', { name: 'Edit context files for CTX-1', exact: true }).inputValue(), selectors.join('\n'));
  await page.locator('#detail-CTX-1 .field-editor .cancel').click();
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: 1100 });
    await drain.screenshot({ path: path.join(evidence, `readiness-${width}.png`) });
    const overflow = await page.locator('#detail-CTX-1 .file-list').evaluate(node => node.scrollWidth > node.clientWidth + 1);
    assert.equal(overflow, false, `selectors wrap at ${width}`);
  }
  await page.locator('#detail-CTX-1 .field-block').filter({ has: page.locator('.file-list') }).screenshot({ path: path.join(evidence, 'narrow.png'), animations: 'disabled' });
  for (const [name, entry, text, links] of [
    ['absent-holder', { reason: 'context_lock_conflict', conflicts: [{ requested_file: 'file:CTX-999.rs' }] }, 'Lock holder unavailable', []],
    ['tasks-and-run', { reason: 'conflict_deferred', conflicts: [{ locking_task_id: 'CTX-2', blocking_task_id: 'CTX-3' }], run_ids: ['jrun-first'] }, 'Waits on', ['CTX-2', 'CTX-3', 'jrun-first']],
    ['live-runs', { reason: 'claimed_by_live_child', run_ids: ['jrun-first', 'jrun-second'] }, 'Waits on', ['jrun-first', 'jrun-second']],
    ['dependencies', { reason: 'unmet_dependency', dependencies: [{ task_id: 'CTX-2', status: 'in-progress' }, { task_id: 'CTX-3', status: 'review' }] }, 'Depends on', ['CTX-2', 'CTX-3']],
    ['capacity', { reason: 'capacity_saturated' }, 'no free slot', []],
    ['host', { reason: 'host_os_mismatch', detail: 'requires os:macos' }, 'needs macos host', []],
    ['other', { reason: 'future_server_reason', detail: 'Admission is waiting for operator input' }, 'future server reason', []],
  ]) {
    readiness = { task_id: 'CTX-1', eligible: false, ...entry };
    await refresh();
    await page.waitForFunction(text => document.querySelector('#detail-CTX-1 .readiness-drain')?.textContent.includes(text), text);
    assert.deepEqual(await drain.locator('a').allTextContents(), links, name);
    assert(!(await drain.textContent()).includes('file:'), 'never infer blockers from paths');
    assert(!(await page.locator('#detail-CTX-1 .readiness-list').textContent()).includes('Ready: nothing'), 'wait never implies eligibility');
    for (const link of await drain.locator('a').all()) {
      const id = await link.textContent();
      await assertScoped(link, id.startsWith('jrun-') ? `#runs?run_id=${id}` : `#tasks?open=${id}`);
    }
    assert.equal(await drain.evaluate(node => node.scrollWidth > node.clientWidth + 1), false, `${name} wraps at narrow width`);
    await drain.screenshot({ path: path.join(evidence, `${name}.png`) });
  }
  results.push({ scenario: 'multiple blockers, absent holder, multiple runs, dependencies, capacity, host and unknown reasons, scoped links', passed: true });
  await page.evaluate(async () => { (await import('/static/js/common.js')).setWorkspace('ws_other'); });
  await refresh();
  await open('CTX-1');
  assert.equal(await highlights('CTX-1').count(), 0, 'same task IDs in another workspace cannot reuse old matches');
  await page.evaluate(async () => { (await import('/static/js/common.js')).setWorkspace(null); });
  await page.locator('#refresh-btn').click();
  await page.locator('[data-key="task-CTX-1"] .title').waitFor();
  await open('CTX-1');
  await page.locator('#detail-CTX-1 .context-contention-unknown').first().waitFor();
  results.push({ scenario: 'context editing, narrow wrapping, workspace switch, unknown aggregate scope', passed: true });
  assert.deepEqual(errors, [], 'no browser runtime errors');
  fs.writeFileSync(path.join(evidence, 'results.json'), JSON.stringify({ passed: true, results }, null, 2));
  process.stdout.write(JSON.stringify({ passed: true, results }) + '\n');
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
