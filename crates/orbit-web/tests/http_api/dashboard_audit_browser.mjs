// Usage: node dashboard_audit_browser.mjs /absolute/path/to/playwright/index.mjs .orbit/tmp/audit-browser
// Runs the shipped renderer, markup and CSS against disposable HTTP fixtures.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
let events = ['success', 'failure', 'denied'].map((status, index) => ({
  id: index + 1, timestamp: '2026-10-07T09:32:04Z', status,
  role: index ? 'codex' : 'unverified', command: 'tool', subcommand: 'run-mcp',
  tool_name: 'orbit.workflow.run.list', target_id: 'orbit.workflow.run.list',
  duration_ms: 1240, exit_code: index ? 1 : 0,
  execution_id: `fixture-${index}`, arguments_json: '{"limit":50}',
}));
events.push({ ...events[0], id: 4, tool_name: null, command: 'audit', subcommand: 'list', target_id: 'workspace' });
events.push({ ...events[1], id: 5, target_id: 'a-distinct-target-with-a-very-long-name', tool_name: 'orbit.task.show' });
const summary = {
  window: '24h', tool_call_failure_rate: { failed: 12345, total: 98765, rate: 0.125, denied: 123 },
  tool_call_failures_by_tool: [{ tool: 'orbit.workflow.run.list', failed: 12345, total: 98765, rate: 0.125, unexpected: 1234, denied: 123 }],
  failure_categories: { unexpected: { incidents: 1234, raw_events: 12345, affected_runs: 123 } },
  duration_by_tool: [
    { tool: 'unknown', count: 2207, avg: 564000, p95: 700000 },
    { tool: 'orbit.workflow.run.list', count: 12345, avg: 1240, p95: 5000 },
  ],
  mcp_vs_cli_split: [{ label: 'mcp', count: 12345 }],
};
const rate = (numerator, denominator) => ({ rate: numerator / denominator, numerator, denominator });
// Three paths sit in two or more of the four path rankings; the lock ranking
// is quiet and change coupling is empty.
const analytics = {
  window: '90d', rollup: 'file', min_support: 15, touched: 'declared_footprint', tasks_scanned: 5000,
  computed_at: '2026-10-07T09:32:04Z', cache_ttl_seconds: 300, row_limit: 25,
  regressions: { below_support: 3, population: 400, rows: [{
    path: 'crates/orbit-core/src/application/task/mod.rs', ...rate(12, 30),
    regressed_tasks: [{ task_id: 'ORB-1', title: 'Fixture regression', crew: 'opus', regression_tasks: ['ORB-2'] }],
    by_crew: [{ crew: 'opus', ...rate(12, 30) }],
  }, {
    path: 'crates/orbit-core/src/runtime/task/a-long-module-name/with/nested/paths/mod.rs', ...rate(4, 20),
    regressed_tasks: [], by_crew: [],
  }] },
  lock_contention: { below_support: 0, reservations: 2557, denials: 0, wait_time_recorded: false, rows: [] },
  footprint_misses: { below_support: 0, population: 400, rows: [
    { path: 'docs/README.md', ...rate(6, 18), widenings: 7 },
    { path: 'crates/orbit-core/src/application/task/mod.rs', ...rate(5, 30), widenings: 5 },
    ...Array.from({ length: 23 }, (_, index) => ({ path: `crates/orbit-fixture/src/module_${index}.rs`, ...rate(3, 20), widenings: 3 })),
  ] },
  change_coupling: { below_support: 13051, reservations: 2300, excluded_large: 250, large_footprint_cap: 12, rows: [] },
  blocked: { below_support: 0, causes: [{ cause: 'auth', kind: 'infrastructure', tasks: 3 }, { cause: 'unclassified', kind: 'unclassified', tasks: 2 }, { cause: 'run_failure', kind: 'attributed', tasks: 5 }], rows: [
    { path: 'src/blocked.rs', ...rate(5, 16), unclassified: 2 },
    { path: 'crates/orbit-core/src/application/task/mod.rs', ...rate(3, 30), unclassified: 1 },
  ] },
  rejected: { below_support: 0, rejected_with_run: 2, excluded_without_run: 6, rows: [
    { path: 'src/rejected.rs', ...rate(2, 15) },
    { path: 'docs/README.md', ...rate(1, 18) },
    { path: 'src/blocked.rs', ...rate(1, 16) },
  ] },
};
const analyticsRequests = [];
// When set, /api/audit pages through it like the real endpoint: newest first,
// `before` keyset cursor, `x-audit-next-before` while a full page may have more.
let pagedEvents = null;
// When set, the newest page (no `before`) answers with these rows and cursor
// even though it is short: a capped scan that stopped before the window ended.
let cappedHead = null;
const auditRequests = [];
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (url.pathname === '/api/audit' && pagedEvents) {
    auditRequests.push(url.searchParams);
    const limit = Number(url.searchParams.get('limit'));
    const before = url.searchParams.get('before');
    const rows = pagedEvents
      .filter(event => before === null || event.id < Number(before))
      .filter(event => !url.searchParams.has('status') || event.status === url.searchParams.get('status'))
      .filter(event => url.searchParams.get('hide_unverified') !== 'true' || !(event.role === 'unverified' && event.status === 'success'))
      .slice(0, limit);
    const capped = before === null && cappedHead;
    const cursor = capped ? cappedHead.cursor : rows.length >= limit ? String(rows.at(-1).id) : null;
    res.setHeader('content-type', 'application/json');
    if (cursor) res.setHeader('x-audit-next-before', cursor);
    res.end(JSON.stringify(capped ? cappedHead.rows : rows));
    return;
  }
  if (url.pathname.startsWith('/api/')) {
    let payload = events;
    if (url.pathname === '/api/codebase/analytics') {
      analyticsRequests.push(url.searchParams);
      payload = analytics;
    }
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify(payload));
    return;
  }
  const served = dashboardFile(url.pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (url.pathname === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '');
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
let page;
const measurements = [];
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&window=24h#audit`);
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(async (summary) => {
    for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.dataset.tab === 'audit');
    const audit = await import('/js/audit.js');
    globalThis.auditFixture = audit;
    globalThis.auditContext = { fmtTimestamp: () => '09:32:04', fmtDuration: value => `${(value / 1000).toFixed(1)}s` };
    audit.setAuditSubtab('events');
    await audit.fetchAndRenderAudit(auditContext);
    audit.renderAuditSummary(summary, auditContext);
  }, summary);
  await page.waitForLoadState('networkidle');
  // Return to desktop to check that the table layout restores after mobile cards.
  for (const width of [1440, 1024, 1920, 375, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.evaluate(async () => {
      await new Promise(requestAnimationFrame);
      await document.fonts.ready;
      await new Promise(requestAnimationFrame);
    });
    const layout = await page.evaluate(() => {
      const body = document.getElementById('audit-body');
      body.scrollLeft = 0;
      const box = body.getBoundingClientRect();
      return {
        width: innerWidth, panel: { left: box.left, right: box.right }, scrollLeft: body.scrollLeft,
        cardKeys: [...document.querySelectorAll('#audit-summary-body .audit-summary-card')].map(card => card.dataset.key),
        eventsPanelBottom: document.getElementById('audit-pane').getBoundingClientRect().bottom,
        summaryPanelBottom: document.getElementById('audit-summary-panel').getBoundingClientRect().bottom,
        statuses: [...body.querySelectorAll('.c-status')].map(node => {
          const rect = node.getBoundingClientRect();
          return { text: node.textContent, left: rect.left, right: rect.right, width: rect.width };
        }),
        tables: [...document.querySelectorAll('#audit-summary-body .summary-table')].map(table => {
          const card = table.closest('.audit-summary-card');
          const rect = card.getBoundingClientRect();
          return {
            key: card.dataset.key, width: rect.width, scroll: table.parentElement.scrollWidth,
            cells: [...table.querySelectorAll('th,td')].map(cell => {
              const box = cell.getBoundingClientRect();
              return { column: cell.dataset.column, omitted: getComputedStyle(cell).display === 'none', secondary: cell.classList.contains('summary-secondary'), left: box.left, right: box.right, cardLeft: rect.left, cardRight: rect.right, clipped: cell.scrollWidth > cell.clientWidth };
            }),
          };
        }),
      };
    });
    assert.equal(layout.scrollLeft, 0);
    assert.equal(layout.statuses.length, events.length);
    assert.ok(!layout.cardKeys.includes('denials') && !layout.cardKeys.includes('role-split'),
      `the Denials and Role split cards are gone at ${width}: ${layout.cardKeys}`);
    assert.ok(layout.cardKeys.includes('mcp-vs-cli'), `MCP vs CLI stays at ${width}: ${layout.cardKeys}`);
    if (width >= 1280) {
      assert.ok(Math.abs(layout.eventsPanelBottom - layout.summaryPanelBottom) <= 2,
        `events and summary panels align at ${width}: ${layout.eventsPanelBottom} vs ${layout.summaryPanelBottom}`);
    }
    for (const status of layout.statuses) {
      assert.ok(status.width > 0 && status.left >= layout.panel.left && status.right <= Math.min(layout.panel.right, width) + 1, `status visible at ${width}: ${JSON.stringify(status)}`);
    }
    for (const table of layout.tables) {
      for (const cell of table.cells) {
        if (cell.omitted) assert.ok(cell.secondary, `only secondary columns omitted: ${JSON.stringify(cell)}`);
        else {
          assert.ok(cell.left >= cell.cardLeft && cell.right <= cell.cardRight + 1, `summary cell fits ${width}: ${JSON.stringify(cell)}`);
          if (cell.column !== 'tool' && cell.column !== 'label') assert.equal(cell.clipped, false, `numeric value/header visible at ${width}: ${JSON.stringify(cell)}`);
        }
      }
    }
    measurements.push(layout);
    await page.screenshot({ path: path.join(evidence, `events-${width}.png`), fullPage: true, animations: 'disabled' });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  const shortEventCount = events.length;
  events = [...events, ...Array.from({ length: 60 }, (_, index) => ({
    ...events[index % shortEventCount], id: index + shortEventCount + 1,
    execution_id: `long-list-${index}`,
  }))];
  await page.evaluate(async () => {
    await auditFixture.fetchAndRenderAudit(auditContext);
    await new Promise(requestAnimationFrame);
  });
  const longListLayout = await page.evaluate(() => {
    const body = document.getElementById('audit-body');
    return {
      eventsBottom: document.getElementById('audit-pane').getBoundingClientRect().bottom,
      summaryBottom: document.getElementById('audit-summary-panel').getBoundingClientRect().bottom,
      bodyClientHeight: body.clientHeight,
      bodyScrollHeight: body.scrollHeight,
    };
  });
  assert.ok(longListLayout.bodyScrollHeight > longListLayout.bodyClientHeight,
    'extra audit events scroll inside the events panel');
  assert.ok(Math.abs(longListLayout.eventsBottom - longListLayout.summaryBottom) <= 2,
    `a long events list does not grow the panel past the summary: ${JSON.stringify(longListLayout)}`);
  const rows = page.locator('.audit-row');
  assert.equal(await rows.nth(0).locator('.c-command').textContent(), events[0].tool_name);
  assert.equal(await rows.nth(0).locator('.c-target').textContent(), '');
  assert.equal(await rows.nth(3).locator('.c-command').textContent(), 'audit list');
  assert.equal(await rows.nth(4).locator('.c-target').textContent(), events[4].target_id);
  assert.match(await page.locator('.audit-table th').nth(2).getAttribute('title'), /unverified/);
  await rows.nth(0).focus();
  await page.keyboard.press('Enter');
  assert.equal(await page.locator('#audit-detail-1 td').getAttribute('colspan'), '7');
  await page.keyboard.press('Enter');
  assert.equal(await page.locator('#audit-detail-1').count(), 0);
  assert.equal(await page.locator('[data-key="duration-by-tool"] tbody tr').count(), 1);
  assert.equal(await page.locator('[data-key="duration-by-tool"] tbody tr td').first().textContent(), events[0].tool_name);
  // Paging: the header separates rows shown from events in the window, older
  // pages append, and the filters (and their URL) ride along on every page.
  pagedEvents = Array.from({ length: 120 }, (_, index) => ({
    ...events[1], id: 120 - index, execution_id: `paged-${120 - index}`,
    status: index % 4 === 0 ? 'failure' : 'success',
    role: index % 3 === 0 ? 'unverified' : 'codex',
  }));
  const countText = () => page.locator('#audit-count').textContent();
  await page.evaluate(async (summary) => {
    auditFixture.applyAuditHashQuery(new URLSearchParams(''));
    await auditFixture.fetchAndRenderAudit(auditContext);
    auditFixture.renderAuditSummary({ ...summary, events: 120 }, auditContext);
  }, summary);
  assert.equal(await page.locator('.audit-row').count(), 50);
  assert.equal(await countText(), '50 of 120 in 24h');
  await page.getByRole('button', { name: 'Load older events' }).click();
  await page.waitForFunction(() => document.querySelectorAll('.audit-row').length === 100);
  assert.equal(await countText(), '100 of 120 in 24h');
  assert.equal(await page.locator('.audit-row').last().getAttribute('title'), 'event 21', 'older pages continue where the last ended');
  // A refresh re-fetches the newest page but keeps the older rows on screen.
  pagedEvents.unshift({ ...pagedEvents[0], id: 121, execution_id: 'paged-121' });
  await page.evaluate(() => auditFixture.fetchAndRenderAudit(auditContext));
  assert.equal(await page.locator('.audit-row').count(), 101, 'refresh keeps loaded pages and adds the new event');
  assert.equal(new Set(await page.locator('.audit-row').evaluateAll(rows => rows.map(row => row.title))).size, 101, 'no row is listed twice');
  await page.getByRole('button', { name: 'Load older events' }).click();
  await page.waitForFunction(() => document.querySelectorAll('.audit-row').length === 121);
  assert.equal(await page.getByRole('button', { name: 'Load older events' }).count(), 0, 'no button once the window is exhausted');
  assert.match(await page.locator('.audit-more-end').textContent(), /No older events/);
  assert.equal(await countText(), '121 of 121 in 24h', 'the shown count never exceeds the window count');
  auditRequests.length = 0;
  await page.evaluate(async () => {
    auditFixture.applyAuditHashQuery(new URLSearchParams('status=success&hide_unverified=1'));
    auditFixture.buildAuditChips(auditContext);
    await auditFixture.fetchAndRenderAudit(auditContext);
  });
  assert.equal(await page.evaluate(() => auditFixture.buildAuditHash()), '#audit?since=24h&status=success&hide_unverified=1', 'the hide choice is part of the URL');
  assert.equal(auditRequests.at(-1).get('hide_unverified'), 'true');
  assert.equal(await page.locator('.chip[data-toggle="hide_unverified"]').getAttribute('aria-pressed'), 'true');
  assert.equal(await page.locator('.audit-row .c-role', { hasText: 'Unconfirmed caller' }).count(), 0, 'unconfirmed successes are hidden');
  assert.match(await countText(), /^50 shown · 120 in 24h$/);
  await page.getByRole('button', { name: 'Load older events' }).click();
  await page.waitForFunction(() => document.querySelectorAll('.audit-row').length > 50);
  const olderRequest = auditRequests.at(-1);
  assert.ok(olderRequest.has('before'), 'paging sends the cursor');
  assert.equal(olderRequest.get('status'), 'success', 'paging keeps the status filter');
  assert.equal(olderRequest.get('hide_unverified'), 'true', 'paging keeps the probe filter');
  assert.equal(await page.locator('.audit-row .audit-status:not(.success)').count(), 0, 'every page honours the status filter');
  // A short capped head whose oldest match is newer than the loaded rows leaves
  // a gap. The old tail must not be spliced on, and Load older must resume from
  // the head's own cursor so the match inside the gap (event 9000) is reached.
  const gapRow = (id) => ({ ...events[1], id, execution_id: `gap-${id}`, status: 'success', role: 'codex' });
  pagedEvents = [9000, ...Array.from({ length: 100 }, (_, index) => 100 - index)].map(gapRow);
  const rowTitles = () => page.locator('.audit-row').evaluateAll(rows => rows.map(row => row.title));
  await page.evaluate(async () => {
    auditFixture.applyAuditHashQuery(new URLSearchParams('status=success'));
    await auditFixture.fetchAndRenderAudit(auditContext);
  });
  await page.getByRole('button', { name: 'Load older events' }).click();
  await page.waitForFunction(() => document.querySelectorAll('.audit-row').length === 100);
  cappedHead = { rows: [20000, 19000].map(gapRow), cursor: '10001' };
  auditRequests.length = 0;
  await page.evaluate(() => auditFixture.fetchAndRenderAudit(auditContext));
  cappedHead = null;
  assert.deepEqual(await rowTitles(), ['event 20000', 'event 19000'], 'a short capped head drops the disconnected older rows');
  await page.getByRole('button', { name: 'Load older events' }).click();
  await page.waitForFunction(() => document.querySelector('.audit-row[title="event 9000"]') !== null);
  assert.equal(auditRequests.at(-1).get('before'), '10001', 'Load older resumes from the head cursor');
  assert.equal((await rowTitles())[2], 'event 9000', 'the gap match follows the new head');
  pagedEvents = null;
  await page.evaluate(async () => {
    const codebase = await import('/js/codebase.js');
    globalThis.codebaseFixture = codebase;
    // app.js is stripped here, so wire the controls the way it does.
    globalThis.codebaseHashes = [];
    codebase.wireCodebaseControls(() => { globalThis.codebaseHashes.push(auditFixture.buildAuditHash()); });
    auditFixture.setActiveAuditSubtabFromButton('codebase');
    await codebase.fetchAndRenderCodebase();
  });
  const refreshCodebase = () => page.evaluate(() => codebaseFixture.fetchAndRenderCodebase());
  assert.equal(await page.locator('#audit-title').textContent(), 'Codebase');
  assert.equal(await page.locator('#audit-body').isVisible(), false, 'Codebase hides the Events listing');
  assert.equal(await page.locator('#audit-codebase-controls').isVisible(), true);
  assert.equal(await page.locator('#audit-summary-panel').isVisible(), false, 'Codebase hides the audit summary');
  assert.equal(analyticsRequests.at(-1).get('min_support'), '15', 'minimum support defaults to 15');
  const widths = () => page.evaluate(() => {
    const main = document.getElementById('audit-main');
    const style = getComputedStyle(main);
    return {
      main: main.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight),
      pane: document.getElementById('audit-pane').getBoundingClientRect().width,
    };
  });
  const fullWidth = await widths();
  assert.ok(fullWidth.pane >= fullWidth.main - 2, `Codebase takes the full width: ${JSON.stringify(fullWidth)}`);
  await page.evaluate(() => auditFixture.setActiveAuditSubtabFromButton('events'));
  assert.equal(await page.locator('#audit-summary-panel').isVisible(), true, 'Events keeps the audit summary');
  const eventsWidth = await widths();
  assert.ok(eventsWidth.pane < eventsWidth.main * 0.75, `Events shares the width with the summary: ${JSON.stringify(eventsWidth)}`);
  await page.evaluate(() => auditFixture.setActiveAuditSubtabFromButton('codebase'));

  // Seven tiles are toggle buttons; the overlap list is open by default and
  // holds exactly the paths in two or more of the four path rankings.
  const tiles = page.locator('#audit-codebase-body .codebase-tile');
  assert.equal(await tiles.count(), 7);
  assert.deepEqual(await tiles.evaluateAll(nodes => nodes.map(node => [node.tagName, node.getAttribute('aria-pressed')])),
    [['BUTTON', 'true'], ...Array.from({ length: 6 }, () => ['BUTTON', 'false'])]);
  const rowPaths = () => page.locator('#codebase-list .codebase-row .codebase-path').evaluateAll(nodes => nodes.map(node => node.textContent));
  assert.deepEqual(await rowPaths(), ['crates/orbit-core/src/application/task/mod.rs', 'docs/README.md', 'src/blocked.rs']);
  const selectedPath = () => page.locator('#codebase-list .codebase-row[aria-current="true"] .codebase-path').textContent();
  assert.equal(await selectedPath(), 'crates/orbit-core/src/application/task/mod.rs', 'the first row is selected');
  assert.equal(await page.locator('#codebase-detail .codebase-task').count(), 1);
  await page.locator('#codebase-list .codebase-row').nth(1).click();
  assert.equal(await selectedPath(), 'docs/README.md');
  assert.match(await page.locator('#codebase-detail [data-ranking="regressions"] .codebase-standing-rank').textContent(), /^not in the top 25$/);

  // A poll with the same payload keeps the tile, the row and the scroll.
  await page.locator('#audit-codebase-body .codebase-tile[data-tile="footprint_misses"]').click();
  assert.equal(await page.locator('#audit-codebase-body .codebase-tile[data-tile="footprint_misses"]').getAttribute('aria-pressed'), 'true');
  const scroller = page.locator('.tab-pane[data-tab="audit"]');
  await page.setViewportSize({ width: 1440, height: 600 });
  const scrolled = await scroller.evaluate(node => { node.scrollTop = 200; return node.scrollTop; });
  assert.ok(scrolled > 0, 'the fixture page is tall enough to scroll');
  await refreshCodebase();
  assert.equal(await scroller.evaluate(node => node.scrollTop), scrolled, 'a poll keeps the scroll position');
  assert.equal(await page.locator('#audit-codebase-body .codebase-tile[data-tile="footprint_misses"]').getAttribute('aria-pressed'), 'true', 'a poll keeps the tile');
  assert.equal(await selectedPath(), 'docs/README.md', 'a poll keeps the row');
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.screenshot({ path: path.join(evidence, 'codebase-1440.png'), fullPage: true, animations: 'disabled' });

  // The segments and the empty-coupling action rewrite the hash and the request.
  await page.locator('#codebase-rollup .codebase-seg[data-value="crate"]').click();
  assert.equal(await page.locator('#codebase-rollup .codebase-seg[data-value="crate"]').getAttribute('aria-pressed'), 'true');
  assert.deepEqual(await page.evaluate(() => globalThis.codebaseHashes), ['#audit/codebase?rollup=crate']);
  await refreshCodebase();
  assert.equal(analyticsRequests.at(-1).get('rollup'), 'crate', 'the rollup segment changes the request');
  await page.locator('#audit-codebase-body .codebase-tile[data-tile="change_coupling"]').click();
  await page.getByRole('button', { name: 'Group by directory' }).click();
  assert.equal(await page.evaluate(() => globalThis.codebaseHashes.at(-1)), '#audit/codebase?rollup=dir');
  await refreshCodebase();
  assert.equal(analyticsRequests.at(-1).get('rollup'), 'dir');

  // 375 px: no horizontal page scroll, every tile and row inside the body,
  // the detail stacked under the list, and readable text.
  await page.locator('#audit-codebase-body .codebase-tile[data-tile="overlap"]').click();
  for (const width of [375, 1280]) {
    await page.setViewportSize({ width, height: 900 });
    await page.evaluate(() => new Promise(requestAnimationFrame));
    const fit = await page.evaluate(() => {
      const body = document.getElementById('audit-codebase-body').getBoundingClientRect();
      const pane = document.querySelector('.tab-pane[data-tab="audit"]');
      const outside = [...document.querySelectorAll('#audit-codebase-body .codebase-tile, #audit-codebase-body .codebase-row, #audit-codebase-body .codebase-metric, #audit-codebase-controls .codebase-seg')]
        .map(node => node.getBoundingClientRect())
        .filter(box => box.left < body.left - 1 || box.right > body.right + 1);
      const list = document.getElementById('codebase-list').getBoundingClientRect();
      const detail = document.getElementById('codebase-detail').getBoundingClientRect();
      return {
        page: document.documentElement.scrollWidth - innerWidth,
        pane: pane.scrollWidth - pane.clientWidth,
        outside: outside.length,
        stacked: detail.top >= list.bottom - 1,
      };
    });
    assert.equal(fit.page, 0, `no horizontal page scroll at ${width}: ${JSON.stringify(fit)}`);
    assert.equal(fit.pane, 0, `no horizontal pane scroll at ${width}: ${JSON.stringify(fit)}`);
    assert.equal(fit.outside, 0, `tiles, rows and segments fit at ${width}: ${JSON.stringify(fit)}`);
    assert.equal(fit.stacked, width < 1280, `the detail stacks under the list only on a narrow screen at ${width}`);
    await page.screenshot({ path: path.join(evidence, `codebase-${width}.png`), fullPage: true, animations: 'disabled' });
  }
  // Text contrast against the composited background, through the alpha
  // washes of selected rows and hints.
  const lowContrast = await page.evaluate(() => {
    const parse = value => (value.match(/[\d.]+/g) || []).map(Number);
    const blend = (top, under) => {
      const alpha = top[3] ?? 1;
      return [0, 1, 2].map(index => top[index] * alpha + under[index] * (1 - alpha));
    };
    const background = node => {
      const layers = [];
      for (let at = node; at; at = at.parentElement) {
        const color = parse(getComputedStyle(at).backgroundColor);
        if (color.length >= 3 && (color[3] ?? 1) > 0) layers.push(color);
        if (color.length >= 3 && (color[3] ?? 1) === 1) break;
      }
      return layers.reverse().reduce((under, layer) => blend(layer, under), [0, 0, 0]);
    };
    const luminance = rgb => {
      const [r, g, b] = rgb.map(value => {
        const channel = value / 255;
        return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
      });
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const failures = [];
    for (const node of document.querySelectorAll('#audit-codebase-controls *, #audit-codebase-body *')) {
      const own = [...node.childNodes].some(child => child.nodeType === 3 && child.textContent.trim());
      if (!own || !node.getClientRects().length) continue;
      const style = getComputedStyle(node);
      if (style.visibility === 'hidden' || style.display === 'none' || node.closest('details:not([open]) > :not(summary)')) continue;
      const bg = background(node);
      const fg = blend(parse(style.color), bg);
      const [light, dark] = [luminance(fg), luminance(bg)].sort((a, b) => b - a);
      const ratio = (light + 0.05) / (dark + 0.05);
      if (ratio < 4.5) failures.push({ text: node.textContent.trim().slice(0, 40), class: node.className, ratio: Number(ratio.toFixed(2)) });
    }
    return failures;
  });
  assert.deepEqual(lowContrast, [], 'codebase text meets 4.5:1');
  await page.setViewportSize({ width: 1440, height: 900 });
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidence, 'measurements.json'), JSON.stringify({ events: measurements, codebase: { fullWidth, eventsWidth }, errors }, null, 2));
  console.log('Audit browser: visible statuses at 1440/1024/1920/375, duplicate targets, summary columns, duration buckets, keyboard expansion, and the full-width codebase view (tiles, overlap list, detail, poll stability, hash, 375 px and contrast) passed');
} catch (error) {
  if (page) await page.screenshot({ path: path.join(evidence, 'failure.png'), fullPage: true }).catch(() => {});
  throw error;
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
