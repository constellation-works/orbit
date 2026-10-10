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
const events = ['success', 'failure', 'denied'].map((status, index) => ({
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
  denials_by_tool: [{ tool: 'orbit.workflow.run.list', count: 123 }],
  denials_by_reason: [{ reason: 'a long denial reason that must fit inside the summary card', count: 123 }],
  role_split: [{ label: 'unverified', count: 98765, mcp: 12345, cli: 123, other: 1234, no_subcommand: 123 }],
  mcp_vs_cli_split: [{ label: 'mcp', count: 12345 }],
};
const policy = {
  total: 8, policy_decisions: { total: 4, sql: 3, v2: 1 }, evidence_scan_limit: 1000,
  recent_denials: Array.from({ length: 8 }, (_, index) => ({
    timestamp: events[0].timestamp, target: `fixture.denial.${index}`,
    cause: index < 4 ? 'policy' : 'context refusal', denial_kind: 'tool_policy',
  })),
};
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (url.pathname.startsWith('/api/')) {
    let payload = events;
    if (url.pathname === '/api/diagnostics/denials') {
      payload = url.searchParams.has('kind') ? { ...policy, total: 2, recent_denials: policy.recent_denials.slice(0, 2) } : policy;
      if (url.searchParams.get('agent') === 'absent') payload = { ...policy, total: 0, recent_denials: [] };
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
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
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
    for (const status of layout.statuses) {
      assert.ok(status.width > 0 && status.left >= layout.panel.left && status.right <= Math.min(layout.panel.right, width) + 1, `status visible at ${width}: ${JSON.stringify(status)}`);
    }
    for (const table of layout.tables) {
      for (const cell of table.cells) {
        if (cell.omitted) assert.ok(cell.secondary, `only secondary columns omitted: ${JSON.stringify(cell)}`);
        else {
          assert.ok(cell.left >= cell.cardLeft && cell.right <= cell.cardRight + 1, `summary cell fits ${width}: ${JSON.stringify(cell)}`);
          if (cell.column !== 'tool' && cell.column !== 'label' && cell.column !== 'reason') assert.equal(cell.clipped, false, `numeric value/header visible at ${width}: ${JSON.stringify(cell)}`);
        }
      }
    }
    measurements.push(layout);
    await page.screenshot({ path: path.join(evidence, `events-${width}.png`), fullPage: true, animations: 'disabled' });
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
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
  await page.evaluate(async () => {
    auditFixture.setAuditSubtab('policy');
    await auditFixture.fetchAndRenderPolicy(auditContext);
  });
  assert.equal(await page.locator('.policy-recent-table tbody tr').count(), 8);
  assert.match(await page.locator('.policy-count-note p').nth(0).textContent(), /^4\b.*3\b.*1\b/);
  assert.match(await page.locator('.policy-count-note p').nth(1).textContent(), /^8\b.*4\b/);
  assert.equal(await page.locator('.policy-count-note').isVisible(), true);
  await page.screenshot({ path: path.join(evidence, 'policy-1440.png'), fullPage: true, animations: 'disabled' });
  await page.evaluate(async () => {
    auditFixture.applyAuditHashQuery(new URLSearchParams('kind=fs'));
    await auditFixture.fetchAndRenderPolicy(auditContext);
  });
  assert.match(await page.locator('.policy-count-note p').nth(0).textContent(), /^4\b/);
  assert.match(await page.locator('.policy-count-note p').nth(1).textContent(), /^2\b/);
  await page.evaluate(async () => {
    auditFixture.applyAuditHashQuery(new URLSearchParams('role=absent&since=7d'));
    await auditFixture.fetchAndRenderPolicy(auditContext);
  });
  assert.equal(await page.locator('.policy-recent-table').count(), 0);
  assert.match(await page.locator('.policy-count-note p').nth(0).textContent(), /7d.*3\b.*1\b/);
  assert.match(await page.locator('.policy-count-note p').nth(1).textContent(), /^0\b/);
  assert.equal(await page.locator('.policy-count-note').isVisible(), true);
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidence, 'measurements.json'), JSON.stringify({ events: measurements, policy: { canonical: 4, evidence: 8, additional: 4 }, errors }, null, 2));
  console.log('Audit browser: visible statuses at 1440/1024/1920/375, duplicate targets, summary columns, duration buckets, policy counts/filters/windows and keyboard expansion passed');
} catch (error) {
  if (page) await page.screenshot({ path: path.join(evidence, 'failure.png'), fullPage: true }).catch(() => {});
  throw error;
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
