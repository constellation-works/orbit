// Run with the host Playwright kit and a checkout-local evidence directory.
// ORB-17935: flat Context files rows must ellipsize inside a 560px pane, and
// a grouped contended basename must ellipsize (not hard-clip) in a 375px
// pane on a wide viewport. ORB-17947: a flat basename wider than its link must
// stay inside the link and ellipsize at 560, 720 and 375px panes.
// Exercises shipped CSS/JS with disposable API data.
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
const longBasename = 'a_very_long_filename_that_exceeds_the_width_of_the_context_files_row_and_should_show_an_ellipsis.rs';
const longSelector = `file:crates/orbit-core/src/${longBasename}`;
const symbolSelector = 'symbol:crates/orbit-core/src/adapter/engine_host/actions/sandbox/provider_state.rs#a_very_long_symbol_name_that_exceeds_the_width_of_the_context_files_row_and_should_show_an_ellipsis:function';
const tasks = [
  { id: 'CTX-1', status: 'backlog', context_files: [selector, `file:unrelated/${'long-directory/'.repeat(10)}other.rs`] },
  { id: 'CTX-2', status: 'in-progress', context_files: [selector] },
  { id: 'CTX-3', status: 'backlog', context_files: [longSelector] },
  { id: 'CTX-4', status: 'backlog', context_files: [symbolSelector] },
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

  const checkRowCopy = async (rowLocator, description, { expectPill = false, expectCompact = true } = {}) => {
    await rowLocator.hover();
    const data = await rowLocator.evaluate(node => {
      const copy = node.querySelector('.cf-copy');
      const link = node.querySelector('.context-selector');
      const base = node.querySelector('.selector-base');
      const pill = node.querySelector('.cf-pill');
      const copyBox = copy.getBoundingClientRect();
      const linkBox = link.getBoundingClientRect();
      const baseBox = base.getBoundingClientRect();
      const pillBox = pill ? pill.getBoundingClientRect() : null;
      const copyOpacity = getComputedStyle(copy).opacity;
      const rowBox = node.getBoundingClientRect();
      return {
        copyBox,
        linkBox,
        baseBox,
        pillBox,
        copyOpacity,
        rowHeight: rowBox.height,
      };
    });
    assert.equal(data.copyOpacity, '1', `${description}: copy button visible on hover`);
    assert(data.copyBox.left >= data.linkBox.right - 0.5,
      `${description}: copy button (left ${data.copyBox.left}) does not overlap link (right ${data.linkBox.right})`);
    assert(data.copyBox.left >= data.baseBox.right - 0.5,
      `${description}: copy button (left ${data.copyBox.left}) does not overlap basename (right ${data.baseBox.right})`);
    if (expectPill) {
      assert(data.pillBox, `${description}: contention pill exists`);
      assert(data.copyBox.left >= data.pillBox.right - 0.5,
        `${description}: copy button (left ${data.copyBox.left}) does not overlap contention pill (right ${data.pillBox.right})`);
    }
    if (expectCompact) {
      assert(data.rowHeight <= 28, `${description}: compact row height (${data.rowHeight}px <= 28px)`);
    }
    return data;
  };

  const checkRowCopyFocused = async (rowLocator, description, { expectPill = false } = {}) => {
    const copyBtn = rowLocator.locator('.cf-copy');
    await copyBtn.focus();
    const data = await rowLocator.evaluate(node => {
      const copy = node.querySelector('.cf-copy');
      const link = node.querySelector('.context-selector');
      const base = node.querySelector('.selector-base');
      const pill = node.querySelector('.cf-pill');
      const copyBox = copy.getBoundingClientRect();
      const linkBox = link.getBoundingClientRect();
      const baseBox = base.getBoundingClientRect();
      const pillBox = pill ? pill.getBoundingClientRect() : null;
      const copyOpacity = getComputedStyle(copy).opacity;
      return {
        copyBox,
        linkBox,
        baseBox,
        pillBox,
        copyOpacity,
        isFocused: document.activeElement === copy,
      };
    });
    assert(data.isFocused, `${description}: copy button is focused`);
    assert.equal(data.copyOpacity, '1', `${description}: copy button visible on focus`);
    assert(data.copyBox.left >= data.linkBox.right - 0.5,
      `${description}: focused copy button (left ${data.copyBox.left}) does not overlap link (right ${data.linkBox.right})`);
    assert(data.copyBox.left >= data.baseBox.right - 0.5,
      `${description}: focused copy button (left ${data.copyBox.left}) does not overlap basename (right ${data.baseBox.right})`);
    if (expectPill) {
      assert(data.pillBox, `${description}: contention pill exists`);
      assert(data.copyBox.left >= data.pillBox.right - 0.5,
        `${description}: focused copy button (left ${data.copyBox.left}) does not overlap contention pill (right ${data.pillBox.right})`);
    }
    return data;
  };

  const checkKeyboardTab = async (rowLocator, description, expectedSelector) => {
    await rowLocator.locator('.context-selector').focus();
    await page.keyboard.press('Tab');
    const activeInfo = await page.evaluate(() => {
      const el = document.activeElement;
      return {
        isCopy: el && el.classList.contains('cf-copy'),
        ariaLabel: el ? el.getAttribute('aria-label') : null,
        opacity: el ? getComputedStyle(el).opacity : null,
      };
    });
    assert(activeInfo.isCopy, `${description}: Tab reached copy button`);
    assert.equal(activeInfo.opacity, '1', `${description}: copy button visible when focused via Tab`);
    assert.equal(activeInfo.ariaLabel, `Copy ${expectedSelector}`, `${description}: copy button has selector aria-label`);
  };
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
  await checkRowCopy(page.locator('#detail-CTX-1 .cf-row').first(), 'flat contended 560px', { expectPill: true });
  await checkRowCopyFocused(page.locator('#detail-CTX-1 .cf-row').first(), 'flat contended 560px focused', { expectPill: true });
  await checkKeyboardTab(page.locator('#detail-CTX-1 .cf-row').first(), 'flat contended 560px', selector);
  await page.locator('#detail-CTX-1 .cf-row').first().hover();
  await block().screenshot({ path: path.join(evidence, 'context-files-flat-dark-560.png'), animations: 'disabled' });
  await page.locator('#detail-CTX-1').getByRole('button', { name: 'By folder', exact: true }).click();
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
  await checkRowCopy(page.locator('#detail-CTX-1 .cf-row').first(), 'grouped contended 375px', { expectPill: true, expectCompact: false });
  await page.locator('#detail-CTX-1 .cf-row').first().hover();
  await block().screenshot({ path: path.join(evidence, 'context-files-grouped-dark-375.png'), animations: 'disabled' });
  await page.locator('#detail-CTX-1').getByRole('button', { name: 'Flat paths', exact: true }).click();
  await page.locator('#detail-CTX-1 .file-list.cf-flat').waitFor();
  await setPaneWidth(375);
  await checkRowCopy(page.locator('#detail-CTX-1 .cf-row').first(), 'flat contended 375px', { expectPill: true, expectCompact: false });
  await page.locator('#detail-CTX-1 .cf-row').first().hover();
  await block().screenshot({ path: path.join(evidence, 'context-files-flat-dark-375.png'), animations: 'disabled' });
  await setPaneWidth(560);
  await page.locator('#detail-CTX-1').getByRole('button', { name: 'By folder', exact: true }).click();
  await page.locator('#detail-CTX-1 .file-list.cf-grouped').waitFor();
  await checkRowCopy(page.locator('#detail-CTX-1 .cf-row').first(), 'grouped contended 560px', { expectPill: true });
  await checkRowCopyFocused(page.locator('#detail-CTX-1 .cf-row').first(), 'grouped contended 560px focused', { expectPill: true });
  await checkKeyboardTab(page.locator('#detail-CTX-1 .cf-row').first(), 'grouped contended 560px', selector);
  await page.locator('#detail-CTX-1 .cf-row').first().hover();
  await block().screenshot({ path: path.join(evidence, 'context-files-grouped-dark-560.png'), animations: 'disabled' });
  await page.locator('[data-key="task-CTX-3"] .title').click();
  await page.locator('#detail-CTX-3 [role="tab"][data-tab="context_files"]').click();
  await page.locator('#detail-CTX-3 .file-list').waitFor();
  await page.waitForFunction(() => document.querySelectorAll('#detail-CTX-3 .context-contended-backlog').length >= 1);
  const longBlock = () => page.locator('#detail-CTX-3 .field-block').filter({ has: page.locator('.file-list') });
  const longRow = async ({ view, cls, width }) => {
    await setPaneWidth(width);
    await page.locator('#detail-CTX-3').getByRole('button', { name: view, exact: true }).click();
    await page.locator(`#detail-CTX-3 .file-list.${cls}`).waitFor();
    return page.locator('#detail-CTX-3 .file-list').evaluate(node => {
      const pane = node.closest('.task-detail-pane');
      const link = node.querySelector('.context-selector');
      const base = node.querySelector('.selector-base');
      const linkBox = link.getBoundingClientRect();
      const baseBox = base.getBoundingClientRect();
      return { paneClient: pane.clientWidth, paneScroll: pane.scrollWidth, linkClient: link.clientWidth, linkScroll: link.scrollWidth, baseText: base.textContent, baseClient: base.clientWidth, baseScroll: base.scrollWidth, baseInsideLink: baseBox.left >= linkBox.left - 1 && baseBox.right <= linkBox.right + 1, textOverflow: getComputedStyle(base).textOverflow };
    });
  };
  const longFlat = [];
  for (const width of [560, 720, 375]) {
    const row = await longRow({ view: 'Flat paths', cls: 'cf-flat', width });
    assert(row.paneScroll <= row.paneClient + 1, `long basename pane does not overflow at ${width}px (scroll ${row.paneScroll} vs client ${row.paneClient})`);
    assert.equal(row.baseText, longBasename);
    assert(row.linkScroll <= row.linkClient + 1, `long basename stays inside its link at ${width}px (link scroll ${row.linkScroll} vs client ${row.linkClient})`);
    assert(row.baseInsideLink, `long basename box stays inside its link at ${width}px`);
    assert(row.baseScroll > row.baseClient + 1 && row.textOverflow === 'ellipsis', `long basename ellipsizes instead of hard-clipping at ${width}px (client ${row.baseClient} vs scroll ${row.baseScroll}, text-overflow ${row.textOverflow})`);
    await checkRowCopy(page.locator('#detail-CTX-3 .cf-row').first(), `long flat basename at ${width}px`, { expectPill: false, expectCompact: width > 480 });
    await checkRowCopyFocused(page.locator('#detail-CTX-3 .cf-row').first(), `long flat basename at ${width}px focused`, { expectPill: false });
    if (width === 560) {
      await checkKeyboardTab(page.locator('#detail-CTX-3 .cf-row').first(), `long flat basename at ${width}px`, longSelector);
    }
    await page.locator('#detail-CTX-3 .cf-row').first().hover();
    await longBlock().screenshot({ path: path.join(evidence, `context-files-long-flat-dark-${width}.png`), animations: 'disabled' });
    longFlat.push({ width, ...row });
  }
  const longGrouped = await longRow({ view: 'By folder', cls: 'cf-grouped', width: 375 });
  assert(longGrouped.paneScroll <= longGrouped.paneClient + 1, `long basename grouped pane does not overflow at 375px (scroll ${longGrouped.paneScroll} vs client ${longGrouped.paneClient})`);
  assert.equal(longGrouped.baseText, longBasename);
  assert(longGrouped.baseInsideLink, 'long grouped basename stays inside its link at 375px');
  assert.equal(longGrouped.textOverflow, 'ellipsis', 'long grouped basename ellipsizes instead of hard-clipping');
  await checkRowCopy(page.locator('#detail-CTX-3 .cf-row').first(), 'long grouped basename at 375px', { expectPill: false, expectCompact: false });
  await checkRowCopyFocused(page.locator('#detail-CTX-3 .cf-row').first(), 'long grouped basename at 375px focused', { expectPill: false });
  await page.locator('#detail-CTX-3 .cf-row').first().hover();
  await longBlock().screenshot({ path: path.join(evidence, 'context-files-long-grouped-dark-375.png'), animations: 'disabled' });
  const longGrouped560 = await longRow({ view: 'By folder', cls: 'cf-grouped', width: 560 });
  await checkRowCopy(page.locator('#detail-CTX-3 .cf-row').first(), 'long grouped basename at 560px', { expectPill: false, expectCompact: true });
  await checkRowCopyFocused(page.locator('#detail-CTX-3 .cf-row').first(), 'long grouped basename at 560px focused', { expectPill: false });
  await page.locator('[data-key="task-CTX-4"] .title').click();
  await page.locator('#detail-CTX-4 [role="tab"][data-tab="context_files"]').click();
  await page.locator('#detail-CTX-4 .file-list').waitFor();
  await page.waitForFunction(() => document.querySelectorAll('#detail-CTX-4 .context-contended-backlog').length >= 1);
  await setPaneWidth(560);
  await page.locator('#detail-CTX-4').getByRole('button', { name: 'Flat paths', exact: true }).click();
  await page.locator('#detail-CTX-4 .file-list.cf-flat').waitFor();
  const symbol = await page.locator('#detail-CTX-4 .file-list').evaluate(node => {
    const pane = node.closest('.task-detail-pane');
    const link = node.querySelector('.context-selector');
    const kind = node.querySelector('.selector-kind');
    const base = node.querySelector('.selector-base');
    const linkBox = link.getBoundingClientRect();
    const baseBox = base.getBoundingClientRect();
    return { paneClient: pane.clientWidth, paneScroll: pane.scrollWidth, kindText: kind?.textContent, linkClient: link.clientWidth, linkScroll: link.scrollWidth, baseClient: base.clientWidth, baseScroll: base.scrollWidth, baseInsideLink: baseBox.left >= linkBox.left - 1 && baseBox.right <= linkBox.right + 1, textOverflow: getComputedStyle(base).textOverflow };
  });
  assert(symbol.paneScroll <= symbol.paneClient + 1, `symbol row pane does not overflow at 560px (scroll ${symbol.paneScroll} vs client ${symbol.paneClient})`);
  assert.equal(symbol.kindText, 'symbol:');
  assert(symbol.linkScroll <= symbol.linkClient + 1, `symbol row stays inside its link at 560px (link scroll ${symbol.linkScroll} vs client ${symbol.linkClient})`);
  assert(symbol.baseInsideLink, 'symbol basename stays inside its link at 560px');
  assert(symbol.baseScroll > symbol.baseClient + 1 && symbol.textOverflow === 'ellipsis', `symbol basename ellipsizes instead of hard-clipping at 560px (client ${symbol.baseClient} vs scroll ${symbol.baseScroll}, text-overflow ${symbol.textOverflow})`);
  await page.locator('#detail-CTX-4 .field-block').filter({ has: page.locator('.file-list') }).screenshot({ path: path.join(evidence, 'context-files-symbol-flat-dark-560.png'), animations: 'disabled' });
  assert.deepEqual(errors, [], 'no browser runtime errors');
  const results = [
    { scenario: 'flat ellipsis at 560px pane, grouped basename at 375px pane', passed: true, flat, grouped },
    { scenario: 'long flat basename ellipsizes inside its link at 560, 720 and 375px panes; grouped at 375px', passed: true, longFlat, longGrouped },
    { scenario: 'long kind-prefixed flat symbol ellipsizes inside its link at 560px pane', passed: true, symbol },
  ];
  fs.writeFileSync(path.join(evidence, 'results.json'), JSON.stringify({ passed: true, results }, null, 2));
  process.stdout.write(JSON.stringify({ passed: true, results }) + '\n');
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
