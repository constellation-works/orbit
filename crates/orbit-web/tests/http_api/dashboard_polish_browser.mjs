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
    ...[
      ['haiku', ['operation.review_crew']],
      ['opus', ['workflow.final_recovery_crews']],
      ['sol', ['workflow.final_recovery_crews']],
      ['grok', ['auto-task skill-validation']],
    ].map(([name, referenced_by]) => ({ name, provider: 'codex', model: 'long-fixture-model-name-that-must-wrap-on-a-phone', effort: 'high', tags: ['implementation', 'fallback'], source: 'global', referenced_by })),
  ],
};
const initializer = `
  import { initRouter, initTabs } from '/js/router.js';
  import { initConfig, getConfigSubtab, setConfigSubtab } from '/js/config.js';
  import * as audit from '/js/audit.js';
  import * as codebase from '/js/codebase.js';
  let tab = 'tasks', diag = 'incidents', jobs = 'live', operations = 'routines', knowledge = 'frictions';
  let incidentsView = 'incidents', analytics = 'metrics', reliabilityView = 'pipeline';
  initConfig();
  initRouter({
    getTab: () => tab, setTab: value => { tab = value; },
    getJobsSubtab: () => jobs, setJobsSubtab: value => { jobs = value; },
    getDiagSubtab: () => diag, setDiagSubtab: value => { diag = value; },
    getIncidentsView: () => incidentsView, setIncidentsView: value => { incidentsView = value; },
    getAnalyticsSubtab: () => analytics, setAnalyticsSubtab: value => { analytics = value; },
    getReliabilityView: () => reliabilityView, setReliabilityView: value => { reliabilityView = value; },
    getOperationsSubtab: () => operations, setOperationsSubtab: value => { operations = value; },
    getKnowledgeSubtab: () => knowledge, setKnowledgeSubtab: value => { knowledge = value; },
    getConfigSubtab, setConfigSubtab,
    getLastRuns: () => [], renderDiagnostics: () => {}, refreshDashboard: () => {},
    applyAuditHashQuery: audit.applyAuditHashQuery, buildAuditHash: audit.buildAuditHash,
    syncAuditControls: audit.syncAuditControls,
    applyCodebaseHashQuery: codebase.applyCodebaseHashQuery, buildCodebaseHash: codebase.buildCodebaseHash,
  });
  audit.buildAuditChips({});
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
const measurements = { navigation: [], observe: [], crews: [], tools: [] };
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 375, height: 812 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  const settle = async () => {
    await page.waitForFunction(() => window.polishReady);
    await page.evaluate(async () => {
      await document.fonts.ready;
      await new Promise(requestAnimationFrame);
      await Promise.all(document.getAnimations()
        .filter(animation => animation.effect.getTiming().iterations !== Infinity)
        .map(animation => animation.finished.catch(() => {})));
    });
  };
  await page.goto(`${origin}#config/crews`);
  await settle();
  const routes = await page.evaluate(() => [
    ...['config', 'operations'].flatMap(section => [...document.querySelectorAll(`#${section}-subtabs .subtab`)].map(button => `${section}/${button.dataset.subtab}`)),
    ...[...document.querySelectorAll('#diag-subtabs .subtab')].map(button => `diagnostics/${button.dataset.subtab}`),
    ...[...document.querySelectorAll('#incidents-views .subtab')].filter(button => button.dataset.view !== 'incidents')
      .map(button => `diagnostics/incidents/${button.dataset.view}`),
    ...[...document.querySelectorAll('#analytics-subtabs .subtab')].map(button => `analytics/${button.dataset.subtab}`),
    'analytics/reliability/tools',
    'jobs/history',
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
  for (const route of ['config/hosts', 'analytics/cost', 'jobs/catalogue']) {
    await page.evaluate(route => { document.getElementById('tabs').scrollLeft = 0; location.hash = route; }, route);
    await page.waitForFunction(route => location.hash.slice(1).split('?')[0] === route && document.querySelector('.tab.active').dataset.tab === route.split('/')[0], route);
    await settle();
    await navigation(route, 'hashchange');
  }
  // Walk every Observe destination, rail view and secondary view by clicking,
  // as an operator does. Each step shows exactly one view's <main>, marks its
  // rail entry, rail view and secondary tab, and names it in the hash; a
  // destination or view reopens on the view it was left on.
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(`${origin}#tasks`);
  // A fresh page, so every destination starts on its default view.
  await page.reload();
  await settle();
  assert.equal(await page.locator('.rail .tab[data-tab="audit"]').count(), 0, 'the rail has no Audit destination');
  assert.deepEqual(await page.locator('.rail-group').filter({ hasText: 'Observe' }).locator('.tab, .subtab').evaluateAll(nodes =>
    nodes.map(node => node.dataset.tab || `${node.closest('nav').id}:${node.dataset.subtab}`)),
  ['diagnostics', 'diag-subtabs:incidents', 'diag-subtabs:doctor', 'analytics',
    'analytics-subtabs:metrics', 'analytics-subtabs:reliability', 'analytics-subtabs:codebase', 'analytics-subtabs:resources',
    'analytics-subtabs:cost'],
  'Observe holds Health (Incidents, Doctor) and Analytics (Metrics, Reliability, Codebase, Resources, Cost)');
  const mains = ['diagnostics-main', 'diagnostics-events-main', 'diagnostics-doctor-main', 'analytics-metrics-main',
    'analytics-reliability-main', 'analytics-tools-main', 'analytics-codebase-main', 'analytics-resources-main', 'analytics-cost-main'];
  const walk = [
    { click: '.rail .tab[data-tab="diagnostics"]', tab: 'diagnostics', subtab: 'incidents', views: 'incidents-views', view: 'incidents', main: 'diagnostics-main', hash: /^#diagnostics\/incidents\?window=24h$/ },
    { click: '#incidents-views [data-view="events"]', tab: 'diagnostics', subtab: 'incidents', views: 'incidents-views', view: 'events', main: 'diagnostics-events-main', hash: /^#diagnostics\/incidents\/events\?since=24h&status=non_success$/ },
    { click: '#audit-filter .chip[data-status="denied"]', tab: 'diagnostics', subtab: 'incidents', views: 'incidents-views', view: 'events', main: 'diagnostics-events-main', hash: /^#diagnostics\/incidents\/events\?since=24h&status=denied$/ },
    { click: '#incidents-views [data-view="errors"]', tab: 'diagnostics', subtab: 'incidents', views: 'incidents-views', view: 'errors', main: 'diagnostics-main', hash: /^#diagnostics\/incidents\/errors\?window=24h$/ },
    { click: '#incidents-views [data-view="events"]', tab: 'diagnostics', subtab: 'incidents', views: 'incidents-views', view: 'events', main: 'diagnostics-events-main', hash: /status=denied$/, note: 'Events keeps its filter' },
    { click: '#incidents-views [data-view="incidents"]', tab: 'diagnostics', subtab: 'incidents', views: 'incidents-views', view: 'incidents', main: 'diagnostics-main', hash: /^#diagnostics\/incidents\?window=24h$/ },
    { click: '#diag-subtabs [data-subtab="doctor"]', tab: 'diagnostics', subtab: 'doctor', main: 'diagnostics-doctor-main', hash: /^#diagnostics\/doctor\?window=24h$/ },
    { click: '.rail .tab[data-tab="analytics"]', tab: 'analytics', subtab: 'metrics', main: 'analytics-metrics-main', hash: /^#analytics\/metrics\?window=24h&outcome=succeeded$/ },
    { click: '#analytics-subtabs [data-subtab="reliability"]', tab: 'analytics', subtab: 'reliability', views: 'reliability-views', view: 'pipeline', main: 'analytics-reliability-main', hash: /^#analytics\/reliability\?window=24h$/ },
    { click: '#reliability-views [data-view="tools"]', tab: 'analytics', subtab: 'reliability', views: 'reliability-views', view: 'tools', main: 'analytics-tools-main', hash: /^#analytics\/reliability\/tools\?window=24h$/ },
    { click: '#analytics-subtabs [data-subtab="codebase"]', tab: 'analytics', subtab: 'codebase', main: 'analytics-codebase-main', hash: /^#analytics\/codebase$/ },
    { click: '#analytics-subtabs [data-subtab="resources"]', tab: 'analytics', subtab: 'resources', main: 'analytics-resources-main', hash: /^#analytics\/resources\?window=24h$/ },
    { click: '#analytics-subtabs [data-subtab="cost"]', tab: 'analytics', subtab: 'cost', main: 'analytics-cost-main', hash: /^#analytics\/cost\?window=24h$/ },
    { click: '#analytics-subtabs [data-subtab="reliability"]', tab: 'analytics', subtab: 'reliability', views: 'reliability-views', view: 'tools', main: 'analytics-tools-main', hash: /^#analytics\/reliability\/tools\?/, note: 'Reliability reopens on Tools' },
    { click: '#reliability-views [data-view="pipeline"]', tab: 'analytics', subtab: 'reliability', views: 'reliability-views', view: 'pipeline', main: 'analytics-reliability-main', hash: /^#analytics\/reliability\?window=24h$/ },
    { click: '.rail .tab[data-tab="diagnostics"]', tab: 'diagnostics', subtab: 'doctor', main: 'diagnostics-doctor-main', hash: /^#diagnostics\/doctor\?/, note: 'Health reopens on Doctor' },
    { click: '.rail .tab[data-tab="analytics"]', tab: 'analytics', subtab: 'reliability', views: 'reliability-views', view: 'pipeline', main: 'analytics-reliability-main', hash: /^#analytics\/reliability\?/, note: 'Analytics reopens on Reliability' },
  ];
  for (const [index, step] of walk.entries()) {
    const before = await page.evaluate(() => location.hash);
    await page.locator(step.click).click();
    await page.waitForFunction(before => location.hash !== before, before).catch(() => {});
    await settle();
    const state = await page.evaluate(mains => ({
      hash: location.hash,
      pane: document.querySelector('.tab-pane.active')?.dataset.tab,
      tab: document.querySelector('.rail .tab.active')?.dataset.tab,
      subtab: document.querySelector('.rail-subtabs:not(.dimmed) .subtab.active')?.dataset.subtab,
      bars: Object.fromEntries(['incidents-views', 'reliability-views'].map(id => {
        const bar = document.getElementById(id);
        return [id, bar.checkVisibility() ? bar.querySelector('.subtab.active')?.dataset.view : null];
      })),
      shown: mains.filter(id => document.getElementById(id).checkVisibility()),
      overflow: document.scrollingElement.scrollWidth > innerWidth,
    }), mains);
    const context = `${step.click}${step.note ? ` (${step.note})` : ''}: ${JSON.stringify(state)}`;
    assert.match(state.hash, step.hash, context);
    assert.equal(state.pane, step.tab, context);
    assert.equal(state.tab, step.tab, context);
    assert.equal(state.subtab, step.subtab, context);
    assert.deepEqual(state.bars, { 'incidents-views': null, 'reliability-views': null, ...(step.views ? { [step.views]: step.view } : {}) }, context);
    assert.deepEqual(state.shown, [step.main], context);
    assert.equal(state.overflow, false, context);
    measurements.observe.push({ step: step.click, ...state });
    await page.screenshot({ path: path.join(evidence, `observe-${String(index + 1).padStart(2, '0')}-${state.hash.slice(1).split('?')[0].replaceAll('/', '-')}.png`) });
  }
  // A direct load or Back lands on the same view as the click that made it.
  await page.goBack();
  await page.waitForFunction(() => location.hash.startsWith('#diagnostics/doctor'));
  assert.equal(await page.evaluate(() => document.getElementById('diagnostics-doctor-main').checkVisibility()), true, 'Back restores Health › Doctor');
  for (const [route, main] of [['diagnostics/incidents/events?status=all', 'diagnostics-events-main'], ['analytics/reliability/tools', 'analytics-tools-main']]) {
    await page.goto(`${origin}#${route}`);
    await page.reload();
    await settle();
    assert.equal(await page.evaluate(main => document.getElementById(main).checkVisibility(), main), true, `#${route} loads its view`);
    if (route.startsWith('diagnostics/incidents/events')) {
      assert.equal((await page.locator('#audit-filter .chip.active').evaluateAll(nodes => nodes.map(node => node.dataset.status))).join(), 'all',
        'status=all opens Events with every status');
    }
  }

  await page.goto(`${origin}#config/crews`);
  await settle();
  for (const width of [390, 768]) {
    await page.setViewportSize({ width, height: 1000 });
    for (const authorized of [true, false]) {
      configPayload.config_set.authorized = authorized;
      await page.evaluate(async () => { const config = await import('/js/config.js'); await config.fetchAndRenderConfig(); });
      const layout = await page.evaluate(() => {
        const head = document.querySelector('.config-crew-head');
        const headings = [...head.children].map(cell => cell.textContent);
        return {
          headings, headerDisplay: getComputedStyle(head).display,
          overflow: document.scrollingElement.scrollWidth > innerWidth,
          rows: [...document.querySelectorAll('.config-crew-row')].map(row => ({
            key: row.dataset.key,
            uses: [...row.querySelectorAll('.config-crew-use')].map(node => node.textContent),
            fields: [...row.querySelector('.config-crew-cells').children].map(cell => {
              const label = cell.firstElementChild;
              const value = cell.lastElementChild;
              const labelRect = label.getBoundingClientRect();
              const valueRect = value.getBoundingClientRect();
              return { label: label.textContent, display: getComputedStyle(label).display, labelWidth: labelRect.width, labelRight: labelRect.right, valueLeft: valueRect.left };
            }),
          })),
        };
      });
      assert.equal(layout.headerDisplay, 'none', 'stacked crew rows use inline labels');
      assert.equal(layout.overflow, false, `crew cards fit at ${width}`);
      assert.equal(layout.rows.length, configPayload.crews.length);
      for (const row of layout.rows) {
        assert.deepEqual(row.fields.map(field => field.label), layout.headings, `${row.key}: all fields have their matching heading first in the DOM`);
        assert.ok(row.fields.every(field => field.display !== 'none' && field.labelWidth > 0 && field.labelRight <= field.valueLeft), `${row.key}: every inline label is visible before its value at ${width}`);
        const crew = configPayload.crews.find(crew => row.key === `crews.${crew.name}`);
        if (crew.referenced_by.length) assert.deepEqual(row.uses, crew.referenced_by, `${row.key}: every server usage reference is displayed once`);
      }
      measurements.crews.push({ width, authorized, ...layout });
      if (authorized) await page.screenshot({ path: path.join(evidence, `crews-${width}.png`), fullPage: true });
    }
  }
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
        const cells = [...document.querySelector('[data-key="crews.empty"] .config-crew-cells').children].map(cell => cell.lastElementChild);
        return {
          text: { left: text.left, right: text.right }, cell: { left: cell.left, right: cell.right }, cardRight: card.right,
          empty: [1, 2, 3, 4, 5, 7].map(index => cells[index].textContent), action: cells[8].textContent,
          columns: [...head.children].map(node => node.getBoundingClientRect().left),
          rowColumns: [...document.querySelector('[data-key="crews.primary"] .config-crew-cells').children].map(node => node.getBoundingClientRect().left),
          labelDisplays: [...document.querySelectorAll('.config-crew-label')].map(node => getComputedStyle(node).display),
          overflow: document.scrollingElement.scrollWidth > innerWidth,
        };
      });
      assert.ok(layout.text.left >= layout.cell.left && layout.text.right <= layout.cell.right + 1 && layout.cell.right <= layout.cardRight,
        `Actions header fits at ${width}: ${JSON.stringify(layout)}`);
      assert.equal(new Set(layout.empty).size, 1, 'all missing crew values share a placeholder');
      assert.equal([...layout.empty[0]].length, 1, 'empty crew values use one glyph');
      if (!authorized) assert.equal(layout.action, layout.empty[0], 'read-only Actions uses the same placeholder');
      assert.deepEqual(layout.columns, layout.rowColumns, 'crew headers stay aligned with row columns');
      assert.ok(layout.labelDisplays.every(display => display === 'none'), 'desktop crews keep their labels in the header');
      assert.equal(layout.overflow, false);
      measurements.crews.push({ width, authorized, ...layout });
    }
    await page.screenshot({ path: path.join(evidence, `crews-${width}.png`), fullPage: true });
  }
  const toolNames = ['orbit.task.artifact.put', 'orbit.task.artifact.get', 'orbit.task.artifact.list'];
  await page.evaluate(async tools => {
    const { setActiveTab } = await import('/js/router.js');
    const { renderAuditSummary } = await import('/js/audit.js');
    setActiveTab('analytics/reliability/tools', { refresh: false });
    renderAuditSummary({ window: '24h', failure_rate_by_tool: tools.map(tool => ({ tool, rate: 0.2, failures: 2, successes: 8, total: 10 })) });
  }, toolNames);
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: 1000 });
    const names = await page.locator('.tool-name').evaluateAll(nodes => nodes.map(node => ({ text: node.textContent, title: node.title, clipped: node.scrollWidth > node.clientWidth })));
    assert.deepEqual(names.map(node => node.title), toolNames, 'shared-prefix tools retain distinct full-name tooltips');
    measurements.tools.push({ width, names });
    await page.screenshot({ path: path.join(evidence, `tools-${width}.png`), fullPage: true });
  }
  assert.deepEqual(errors, [], 'the fixture reports no browser errors');
  fs.writeFileSync(path.join(evidence, 'measurements.json'), JSON.stringify(measurements, null, 2));
  console.log(`PASS: ${routes.length} phone deep links on load/reload and three hash changes; ${walk.length} clicks through every Observe view; inline crew labels and usage at 390/768; crew headers/placeholders at 1280/1440/1920; full Tools names at 1440/375.`);
} finally {
  await browser?.close();
  server.close();
}
