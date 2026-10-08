// Usage: node dashboard_polish_browser.mjs /absolute/path/to/playwright/index.mjs .orbit/tmp/polish-browser
// Exercise the shipped router, renderers and CSS with disposable HTTP data.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const configPayload = {
  config_set: { authorized: true },
  sections: [{ kind: 'crews', title: 'Crews', blurb: 'Named providers', keys: [] }],
  crews: [
    { name: 'primary', provider: 'codex', model: 'example-model', effort: 'high', tags: ['implementation'], source: 'workspace', referenced_by: ['workflow.default_crew'] },
    { name: 'empty', provider: null, model: null, effort: '', tags: [], source: 'global', referenced_by: [] },
  ],
};
const initializer = `
  import { initRouter, initTabs } from '/js/router.js';
  import { initConfig, getConfigSubtab, setConfigSubtab } from '/js/config.js';
  import * as audit from '/js/audit.js';
  let tab = 'tasks', diag = 'runs', operations = 'routines', knowledge = 'frictions';
  initConfig();
  initRouter({
    getTab: () => tab, setTab: value => { tab = value; },
    getDiagSubtab: () => diag, setDiagSubtab: value => { diag = value; },
    getOperationsSubtab: () => operations, setOperationsSubtab: value => { operations = value; },
    getKnowledgeSubtab: () => knowledge, setKnowledgeSubtab: value => { knowledge = value; },
    getConfigSubtab, setConfigSubtab,
    getLastRuns: () => [], renderDiagnostics: () => {}, refreshDashboard: () => {},
    getActiveAuditSubtab: audit.getActiveAuditSubtab, setAuditSubtab: audit.setAuditSubtab,
    applyAuditHashQuery: audit.applyAuditHashQuery, buildAuditHash: audit.buildAuditHash,
    syncAuditControls: audit.syncAuditControls,
  });
  initTabs();
  window.polishReady = true;
`;
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (url.pathname === '/api/config/effective') {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify(configPayload));
    return;
  }
  const served = url.pathname === '/fixture.mjs'
    ? { data: initializer, type: 'text/javascript' }
    : dashboardFile(url.pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  let data = served.data;
  if (url.pathname === '/') data = data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '<script type="module" src="/fixture.mjs"></script>');
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}/?workspace=ws_fixture`;
const measurements = { navigation: [], crews: [], audit: [] };
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 375, height: 812 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  const settle = async () => {
    await page.waitForFunction(() => window.polishReady);
    await page.evaluate(async () => { await document.fonts.ready; await new Promise(requestAnimationFrame); });
  };
  await page.goto(`${origin}#config/crews`);
  await settle();
  const routes = await page.evaluate(() => [
    ...['config', 'operations'].flatMap(section => [...document.querySelectorAll(`#${section}-subtabs .subtab`)].map(button => `${section}/${button.dataset.subtab}`)),
    ...[...document.querySelectorAll('#diag-subtabs .subtab')].map(button => `diagnostics/${button.dataset.subtab}`),
    'diagnostics/runs',
  ]);
  const navigation = async (route, visit) => {
    const layout = await page.evaluate(() => {
      const rect = node => { const box = node.getBoundingClientRect(); return { left: box.left, right: box.right, width: box.width }; };
      const row = document.getElementById('tabs');
      const tab = row.querySelector('.tab.active');
      const subtab = row.querySelector('.rail-subtabs:not(.dimmed) .subtab.active');
      return {
        row: rect(row), tab: rect(tab), subtab: subtab && rect(subtab), views: subtab && rect(subtab.parentElement),
        selected: tab.dataset.tab, hash: location.hash, focus: document.activeElement.tagName,
        pageWidth: document.scrollingElement.scrollWidth,
      };
    });
    for (const [name, box] of Object.entries({ tab: layout.tab, subtab: layout.subtab })) {
      if (!box) continue;
      assert.ok(box.width > 0 && box.left >= layout.row.left - 1 && box.right <= layout.row.right + 1,
        `${route} ${visit}: active ${name} fits the phone nav: ${JSON.stringify(layout)}`);
    }
    if (layout.subtab) assert.ok(layout.subtab.left >= layout.views.left - 1 && layout.subtab.right <= layout.views.right + 1,
      `${route} ${visit}: active view fits its scroller: ${JSON.stringify(layout)}`);
    assert.equal(layout.pageWidth, 375, `${route} ${visit}: no page overflow`);
    assert.equal(layout.focus, 'BODY', 'route scrolling preserves focus on a direct load');
    measurements.navigation.push({ route, visit, ...layout });
  };
  for (const route of routes) {
    await page.goto(`${origin}#${route}`);
    await settle();
    await navigation(route, 'load');
    await page.reload();
    await settle();
    await navigation(route, 'reload');
  }
  // Changing the hash on an existing page must also reveal the current view.
  for (const route of ['config/hosts', 'diagnostics/scoreboard', 'operations/jobs']) {
    await page.evaluate(route => { document.getElementById('tabs').scrollLeft = 0; location.hash = route; }, route);
    await page.waitForFunction(route => location.hash.slice(1).split('?')[0] === route && document.querySelector('.tab.active').dataset.tab === route.split('/')[0], route);
    await settle();
    await navigation(route, 'hashchange');
  }
  await page.goto(`${origin}#config/crews`);
  await settle();
  for (const width of [1280, 1440, 1920]) {
    await page.setViewportSize({ width, height: 1000 });
    for (const authorized of [true, false]) {
      configPayload.config_set.authorized = authorized;
      await page.evaluate(async () => { const config = await import('/js/config.js'); await config.fetchAndRenderConfig(); });
      const layout = await page.evaluate(() => {
        const head = document.querySelector('.config-crew-head');
        const range = document.createRange();
        range.selectNodeContents(head.lastElementChild);
        const text = range.getBoundingClientRect();
        const cell = head.lastElementChild.getBoundingClientRect();
        const card = head.closest('.panel').getBoundingClientRect();
        const cells = [...document.querySelector('[data-key="crews.empty"] .config-crew-cells').children];
        return {
          text: { left: text.left, right: text.right }, cell: { left: cell.left, right: cell.right }, cardRight: card.right,
          empty: [1, 2, 3, 4, 6].map(index => cells[index].textContent), action: cells[7].textContent,
          columns: [...head.children].map(node => node.getBoundingClientRect().left),
          rowColumns: [...document.querySelector('[data-key="crews.primary"] .config-crew-cells').children].map(node => node.getBoundingClientRect().left),
          overflow: document.scrollingElement.scrollWidth > innerWidth,
        };
      });
      assert.ok(layout.text.left >= layout.cell.left && layout.text.right <= layout.cell.right + 1 && layout.cell.right <= layout.cardRight,
        `Actions header fits at ${width}: ${JSON.stringify(layout)}`);
      assert.equal(new Set(layout.empty).size, 1, 'all missing crew values share a placeholder');
      assert.equal([...layout.empty[0]].length, 1, 'empty crew values use one glyph');
      if (!authorized) assert.equal(layout.action, layout.empty[0], 'read-only Actions uses the same placeholder');
      assert.deepEqual(layout.columns, layout.rowColumns, 'crew headers stay aligned with row columns');
      assert.equal(layout.overflow, false);
      measurements.crews.push({ width, authorized, ...layout });
    }
    await page.screenshot({ path: path.join(evidence, `crews-${width}.png`), fullPage: true });
  }
  const toolNames = ['orbit.task.artifact.put', 'orbit.task.artifact.get', 'orbit.task.artifact.list'];
  await page.evaluate(async tools => {
    const { setActiveTab } = await import('/js/router.js');
    const { renderAuditSummary } = await import('/js/audit.js');
    setActiveTab('audit', { refresh: false });
    renderAuditSummary({ window: '24h', failure_rate_by_tool: tools.map(tool => ({ tool, rate: 0.2, failures: 2, successes: 8, total: 10 })) });
  }, toolNames);
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: 1000 });
    const names = await page.locator('.tool-name').evaluateAll(nodes => nodes.map(node => ({ text: node.textContent, title: node.title, clipped: node.scrollWidth > node.clientWidth })));
    assert.deepEqual(names.map(node => node.title), toolNames, 'shared-prefix tools retain distinct full-name tooltips');
    measurements.audit.push({ width, names });
    await page.screenshot({ path: path.join(evidence, `audit-${width}.png`), fullPage: true });
  }
  assert.deepEqual(errors, [], 'the fixture reports no browser errors');
  fs.writeFileSync(path.join(evidence, 'measurements.json'), JSON.stringify(measurements, null, 2));
  console.log(`PASS: ${routes.length} phone deep links on load/reload and three hash changes; crew headers/placeholders at 1280/1440/1920; full Audit names at 1440/375.`);
} finally {
  await browser?.close();
  server.close();
}
