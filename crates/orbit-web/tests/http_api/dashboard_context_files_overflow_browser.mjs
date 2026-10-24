// Run with the host Playwright kit and a checkout-local evidence directory.
// ORB-17935: flat Context files rows must ellipsize inside a 560px pane, and
// a grouped contended basename must ellipsize (not hard-clip) in a 375px
// pane on a wide viewport. Exercises shipped CSS/JS with disposable API data.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const selector = 'file:crates/orbit-core/src/adapter/engine_host/actions/sandbox/provider_state.rs';
const tasks = [
  { id: 'CTX-1', status: 'backlog', context_files: [selector, `file:unrelated/${'long-directory/'.repeat(10)}other.rs`] },
  { id: 'CTX-2', status: 'in-progress', context_files: [selector] },
].map(task => ({ ...task, title: `Context fixture ${task.id}`, priority: 'medium', complexity: 'low',
  description: 'Disposable browser scenario', plan: 'Inspect contexts', comments: [], history: [], artifacts: [], tags: [], status_transitions: [] }));
const list = items => ({ items, total: items.length, limit: 100, truncated: false });
function locks() {
  const active = tasks.filter(task => task.status === 'in-progress');
  const context_contention = {};
  for (const task of tasks) {
    context_contention[task.id] = { status: task.status, entries: task.context_files.map(sel => ({
      selector: sel, known: true,
      contenders: active.filter(holder => holder.id !== task.id).map(holder => ({ id: holder.id, status: holder.status, selector: holder.context_files[0] })),
    })) };
  }
  return { by_task: active, by_reservation: [], locked_files: ['file:crates/orbit-core/src/adapter/engine_host/actions/sandbox/provider_state.rs'], total_locked: 1, total_tasks: 1, total_reservations: 0, context_contention };
}
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  url.pathname = url.pathname.replace(/^\/api\/on\/[^/]+\//, '/api/');
  if (url.pathname.startsWith('/api/')) {
    res.setHeader('content-type', 'application/json');
    let payload = list([]);
    if (url.pathname === '/api/workspaces') payload = [{ id: 'ws_fixture', name: 'fixture', status: 'active', is_default: true }];
    if (url.pathname === '/api/hosts') payload = { generation: 1, hosts: [{ name: 'serving-host', machine_id: 'hm_serving', local: true, reachable: true }], forward_writes: { authorized: true, reason: null } };
    if (url.pathname === '/api/crews') payload = { crews: [], default_crew: null };
    if (url.pathname === '/api/tasks') payload = list(tasks);
    if (url.pathname === '/api/tasks/all') payload = list(tasks.map(task => ({ ...task, workspace_id: 'ws_fixture', workspace_name: 'fixture' })));
    const task = tasks.find(task => url.pathname === `/api/tasks/${task.id}`);
    if (task) payload = task;
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
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&window=24h#tasks`);
  await page.locator('[data-key="task-CTX-1"] .title').click();
  await page.locator('#detail-CTX-1 [role="tab"][data-tab="context_files"]').click();
  await page.locator('#detail-CTX-1 .file-list').waitFor();
  await page.waitForFunction(() => document.querySelectorAll('#detail-CTX-1 .context-contended-backlog').length >= 1);
  const setPaneWidth = async width => {
    await page.evaluate(w => {
      const main = document.querySelector('main.tasks-layout');
      main.style.setProperty('grid-template-columns', `minmax(0, 1fr) 12px ${w}px`, 'important');
    }, width);
    await page.waitForFunction(w => Math.abs(document.querySelector('.task-detail-pane').getBoundingClientRect().width - w) <= 1, width);
  };
  const block = () => page.locator('#detail-CTX-1 .field-block').filter({ has: page.locator('.file-list') });
  await setPaneWidth(560);
  await page.locator('#detail-CTX-1').getByRole('button', { name: 'Flat paths', exact: true }).click();
  await page.locator('#detail-CTX-1 .file-list.cf-flat').waitFor();
  const flat = await page.locator('#detail-CTX-1 .file-list').evaluate(node => {
    const pane = node.closest('.task-detail-pane');
    const dim = node.querySelector('.selector-dim');
    const base = node.querySelector('.selector-base');
    const clipped = [dim, base].filter(el => el && el.scrollWidth > el.clientWidth + 1);
    return { paneClient: pane.clientWidth, paneScroll: pane.scrollWidth, listClient: node.clientWidth, listScroll: node.scrollWidth, clippedEllipsis: clipped.length > 0 && clipped.every(el => getComputedStyle(el).textOverflow === 'ellipsis'), baseText: base?.textContent, baseClient: base?.clientWidth, baseScroll: base?.scrollWidth, dimClient: dim?.clientWidth, dimScroll: dim?.scrollWidth, dimHeight: dim?.clientHeight };
  });
  assert(flat.paneScroll <= flat.paneClient + 1, `flat pane does not overflow at 560px (scroll ${flat.paneScroll} vs client ${flat.paneClient})`);
  assert(flat.listScroll <= flat.listClient + 1, 'flat list does not overflow at 560px');
  assert.equal(flat.baseText, 'provider_state.rs');
  assert(flat.clippedEllipsis, 'flat overflowing directory or basename ellipsizes at 560px');
  assert(flat.baseClient >= flat.baseScroll - 1, `flat basename stays whole at 560px (client ${flat.baseClient} vs scroll ${flat.baseScroll})`);
  assert(flat.dimScroll > flat.dimClient + 1 && flat.dimHeight <= 20, `flat directory ellipsizes on one line at 560px (client ${flat.dimClient} vs scroll ${flat.dimScroll}, height ${flat.dimHeight})`);
  await block().screenshot({ path: path.join(evidence, 'context-files-flat-dark-560.png'), animations: 'disabled' });
  await page.locator('#detail-CTX-1').getByRole('button', { name: 'By crate', exact: true }).click();
  await page.locator('#detail-CTX-1 .file-list.cf-grouped').waitFor();
  await setPaneWidth(375);
  const grouped = await page.locator('#detail-CTX-1 .file-list').evaluate(node => {
    const pane = node.closest('.task-detail-pane');
    const base = node.querySelector('.selector-base');
    return { paneClient: pane.clientWidth, paneScroll: pane.scrollWidth, baseText: base?.textContent, baseClient: base?.clientWidth, baseScroll: base?.scrollWidth, textOverflow: base ? getComputedStyle(base).textOverflow : '' };
  });
  assert(grouped.paneScroll <= grouped.paneClient + 1, `grouped pane does not overflow at 375px (scroll ${grouped.paneScroll} vs client ${grouped.paneClient})`);
  assert.equal(grouped.baseText, 'provider_state.rs');
  assert.equal(grouped.textOverflow, 'ellipsis', 'grouped basename ellipsizes instead of hard-clipping');
  assert(grouped.baseClient >= 80, `grouped basename stays identifiable at 375px (client ${grouped.baseClient})`);
  await block().screenshot({ path: path.join(evidence, 'context-files-grouped-dark-375.png'), animations: 'disabled' });
  await page.locator('#detail-CTX-1').getByRole('button', { name: 'Flat paths', exact: true }).click();
  await page.locator('#detail-CTX-1 .file-list.cf-flat').waitFor();
  await block().screenshot({ path: path.join(evidence, 'context-files-flat-dark-375.png'), animations: 'disabled' });
  await setPaneWidth(560);
  await page.locator('#detail-CTX-1').getByRole('button', { name: 'By crate', exact: true }).click();
  await page.locator('#detail-CTX-1 .file-list.cf-grouped').waitFor();
  await block().screenshot({ path: path.join(evidence, 'context-files-grouped-dark-560.png'), animations: 'disabled' });
  assert.deepEqual(errors, [], 'no browser runtime errors');
  const results = [{ scenario: 'flat ellipsis at 560px pane, grouped basename at 375px pane', passed: true, flat, grouped }];
  fs.writeFileSync(path.join(evidence, 'results.json'), JSON.stringify({ passed: true, results }, null, 2));
  process.stdout.write(JSON.stringify({ passed: true, results }) + '\n');
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
