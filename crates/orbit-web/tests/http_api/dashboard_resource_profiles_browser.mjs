// Usage: node dashboard_resource_profiles_browser.mjs /path/to/playwright/index.mjs .orbit/tmp/resources-browser
// Analytics › Resources through the shipped app, router, renderer and CSS:
// the work-class selector, complexity series ending at their horizons, the
// sparse marking, hover and keyboard tooltips, series toggles, the % of host
// toggle with admit thresholds, the empty state, the aggregate placeholder
// and a 390px phone.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const GIB = 1024 ** 3;

/// A profile whose deliveries ran `longest` minutes, `horizon` the minute
/// fewer than one in ten still ran; CPU rises to `peak` cores by minute 5.
function profile(workClass, complexity, deliveries, sparse, horizon, longest, peak, gib) {
  const series = Array.from({ length: longest + 1 }, (_, minute) => {
    const cpu = minute < 5 ? peak * (minute + 1) / 5 : peak * 0.6;
    return {
      minute,
      alive: Math.max(0.02, 1 - minute / (horizon * 1.1)),
      cpu_cores: cpu,
      cpu_stddev: cpu * 0.3,
      memory_bytes: Math.round(gib * GIB * (minute < 5 ? 0.5 + minute / 10 : 1)),
      memory_mean_bytes: gib * GIB * 0.7,
      memory_stddev_bytes: gib * GIB * 0.1,
    };
  });
  return {
    key: { workspace_id: 'ws_fixture', work_class: workClass, complexity },
    base_class: workClass === 'no-diff' ? 'no-diff' : 'change',
    delivery_count: deliveries,
    sparse,
    horizon_minutes: horizon,
    peak_cpu_cores: { minute: 4, value: peak },
    peak_memory_bytes: { minute: 5, value: gib * GIB },
    expected_core_minutes: peak * horizon,
    cpu_spread: 0.3,
    memory_spread: 0.12,
    build_like: [],
    deliveries: [],
    series,
  };
}

const profiles = [
  profile('auto-task:qa-sweep', null, 6, false, 20, 24, 3, 2),
  profile('change', 'hard', 2, true, 60, 60, 6, 6),
  profile('change', 'low', 12, false, 30, 40, 2, 2),
  profile('change', 'medium', 8, false, 45, 50, 4, 4),
  profile('no-diff', 'medium', 5, false, 15, 18, 0.8, 1),
];
const report = (workspace, list) => ({
  workspace_id: workspace,
  built_at: '2026-10-17T12:00:00Z',
  host: { cpu_count: 16, memory_total_bytes: 64 * GIB },
  admit_cpu_percent: 90,
  admit_memory_percent: 75,
  profile_window: 50,
  min_profile_deliveries: 3,
  horizon_cap_minutes: 120,
  profiles: list,
});

const requests = [];
function payloadFor(url) {
  switch (url.pathname) {
    case '/api/workspaces': return [
      { id: 'ws_fixture', name: 'fixture', status: 'active', is_default: true },
      { id: 'ws_empty', name: 'empty', status: 'active', is_default: false },
    ];
    case '/api/crews': return { default_crew: null, crews: [] };
    case '/api/workflows/auto/readiness': return { capacity: {}, tasks: [] };
    case '/api/resource-profiles': {
      const workspace = url.searchParams.get('workspace');
      requests.push(workspace);
      return workspace === 'ws_fixture' ? report(workspace, profiles) : report(workspace, []);
    }
    default: return { items: [], total: 0, limit: 100, truncated: false };
  }
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://fixture');
  if (url.pathname.startsWith('/api/')) {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify(payloadFor(url)));
    return;
  }
  const served = dashboardFile(url.pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  res.setHeader('content-type', served.type);
  res.end(served.data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const base = `http://127.0.0.1:${server.address().port}/`;
const assertions = [];
const pass = (name, detail = {}) => assertions.push({ name, ...detail });
let browser;
let page;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  page = await browser.newPage({ viewport: { width: 1440, height: 1700 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  // The fixture serves no log stream and no Metrics payload, which the
  // first route opens on; that noise is not this view's.
  page.on('console', message => {
    if (message.type() === 'error' && !/EventSource|Failed to load resource|metrics payload window rejected/.test(message.text())) errors.push(message.text());
  });
  const body = page.locator('#resources-body');
  const shot = name => page.screenshot({ path: path.join(evidence, name), fullPage: true, animations: 'disabled' });

  // Reachable from the subtab: open Metrics, then pick Resources in the rail.
  await page.goto(`${base}?workspace=ws_fixture#analytics/metrics`);
  await page.locator('#analytics-subtabs [data-subtab="resources"]').click();
  await body.locator('.mx-sc').first().waitFor();
  assert.equal(new URL(page.url()).hash.split('?')[0], '#analytics/resources');
  assert.equal(await page.locator('#analytics-resources-main').evaluate(node => getComputedStyle(node).display), 'grid');
  assert.equal(await page.locator('#analytics-metrics-main').evaluate(node => getComputedStyle(node).display), 'none');
  await page.evaluate(async () => { await document.fonts.ready; });
  pass('subtab opens the view', { hash: new URL(page.url()).hash });

  // Change overlays its complexities in order, each with its delivery count.
  const classes = await body.locator('.rp-classes button').evaluateAll(nodes => nodes.map(node => ({ text: node.textContent, pressed: node.getAttribute('aria-pressed'), disabled: node.disabled })));
  assert.deepEqual(classes.map(c => c.text), ['Change', 'No-diff', 'qa-sweep']);
  assert.equal(classes[0].pressed, 'true');
  const cards = await body.locator('.mx-sc').evaluateAll(nodes => nodes.map(node => node.textContent));
  assert.equal(cards.length, 3);
  assert.match(cards[0], /^Easylow2\.0 cores.*12 deliveries · to \+30m$/);
  assert.match(cards[2], /^Hard6\.0 cores.*2 deliveriessparse · to \+60m$/);
  assert.match(await page.locator('#resources-count').textContent(), /5 profiles · host 16 cores · 64 GiB/);
  pass('change series cards', { cards });

  // Lines: a CPU band and mean per series, memory p90 and dashed mean, alive.
  const groups = plot => body.locator(`.rp-area-${plot} .rp-lines g`);
  assert.equal(await groups('cpu').count(), 3);
  assert.equal(await body.locator('.rp-area-cpu .rp-band').count(), 3);
  assert.equal(await body.locator('.rp-area-mem .rp-mean').count(), 3);
  assert.equal(await body.locator('.rp-area-alive .rp-line').count(), 3);
  assert.equal(await body.locator('.rp-lines g.rp-sparse').count(), 3, 'the sparse hard profile is dimmed in every plot');
  assert.equal(await body.locator('.rp-end.hollow').count(), 3, 'and ends hollow');
  // Each series ends at its horizon, on a 60-minute axis set by the longest.
  const lastX = await body.locator('.rp-area-cpu g.rp-s-low .rp-line').evaluate(node => Number(node.getAttribute('d').trim().split(/[ML]/).filter(Boolean).at(-1).trim().split(' ')[0]));
  assert.equal(lastX, 500, 'easy stops at +30m of 60 although its deliveries ran to +40m');
  assert.equal(await body.locator('.rp-area-cpu .rp-end.rp-s-low').evaluate(node => node.style.left), '50%');
  assert.deepEqual(await body.locator('.rp-x-axis .mx-x-tick').allTextContents(), ['0', '10m', '20m', '30m', '40m', '50m', '1h']);
  await shot('resources-change-1440.png');
  pass('charts end at horizons', { lastX });

  // Hover past one horizon: that series says it ended, the sparse one is amber.
  const area = await body.locator('.rp-area-cpu').boundingBox();
  await page.mouse.move(area.x + area.width * (40 / 60), area.y + area.height / 2);
  const tip = body.locator('.rp-tip');
  await tip.waitFor({ state: 'visible' });
  const tipText = await tip.textContent();
  assert.match(tipText, /^\+40m since admission/);
  assert.match(tipText, /Easy×12ended \+30m/);
  assert.match(tipText, /Hard×2alive \d+%cpu 3\.6 cores ± 1\.1mem p90 6\.0 GiB · mean 4\.2 GiB/);
  assert.equal(await tip.locator('.mx-tip-n.low').count(), 1);
  assert.match(tipText, /Amber counts: sparse, fewer than 3 deliveries/);
  assert.equal(await body.locator('.rp-area .mx-cursor:not([hidden])').count(), 3, 'one cursor across the shared x-axis');
  await shot('resources-tooltip-1440.png');
  await page.mouse.move(5, 5);
  await tip.waitFor({ state: 'hidden' });
  pass('hover tooltip');

  // Keyboard: Home, arrows, Shift+arrow by ten, End, Escape.
  await body.locator('.rp-frame').focus();
  const title = () => tip.locator('.mx-tip-title').textContent();
  await page.keyboard.press('Home');
  assert.equal(await title(), '+0m since admission');
  await page.keyboard.press('ArrowRight');
  assert.equal(await title(), '+1m since admission');
  await page.keyboard.press('Shift+ArrowRight');
  assert.equal(await title(), '+11m since admission');
  await page.keyboard.press('End');
  assert.equal(await title(), '+60m since admission');
  assert.match(await body.locator('.rp-frame [role="status"]').textContent(), /^Minute 60\. /);
  await page.keyboard.press('Escape');
  await tip.waitFor({ state: 'hidden' });
  pass('keyboard tooltip');

  // Toggle series; the axis follows the visible horizons; the last one stays.
  await body.locator('.mx-sc').nth(2).click();
  assert.equal(await body.locator('.mx-sc').nth(2).getAttribute('aria-pressed'), 'false');
  assert.equal(await groups('cpu').count(), 2);
  assert.equal(await body.locator('.rp-x-axis .mx-x-tick').last().textContent(), '40m', 'medium ends the axis at +45m');
  await body.locator('.mx-sc').nth(0).click();
  await body.locator('.mx-sc').nth(1).click();
  assert.equal(await body.locator('.mx-sc').nth(1).getAttribute('aria-pressed'), 'true', 'the last series stays shown');
  await body.locator('.mx-sc').nth(0).click();
  await body.locator('.mx-sc').nth(2).click();
  assert.equal(await groups('cpu').count(), 3);
  pass('series toggles');

  // Percent of host: values and axes in %, with both admit thresholds drawn.
  await body.getByRole('button', { name: '% of host' }).click();
  assert.equal(await body.getByRole('button', { name: '% of host' }).getAttribute('aria-pressed'), 'true');
  assert.deepEqual(await body.locator('.rp-threshold-label').allTextContents(), ['admit 90% of cores', 'admit 75% of memory']);
  assert.equal(await body.locator('.mx-sc-value').first().textContent(), '13%', '2 of 16 cores');
  assert.match(await body.locator('.rp-area-cpu').locator('xpath=..').locator('.rp-y-axis').textContent(), /100%/);
  await page.mouse.move(area.x + area.width * (10 / 60), area.y + area.height / 2);
  assert.match(await tip.textContent(), /Medium×8alive \d+%cpu 15% ± 4\.5%mem p90 6\.3% · mean 4\.4%/);
  await page.mouse.move(5, 5);
  await shot('resources-percent-1440.png');
  await body.getByRole('button', { name: 'Cores · GiB' }).click();
  assert.equal(await body.locator('.rp-threshold').count(), 0);
  pass('percent of host with thresholds');

  // No-diff has one complexity; a template is one series without complexity.
  await body.getByRole('button', { name: 'No-diff', exact: true }).click();
  assert.deepEqual(await body.locator('.mx-sc .mx-sc-label').allTextContents(), ['Medium']);
  await body.getByRole('button', { name: 'qa-sweep', exact: true }).click();
  assert.deepEqual(await body.locator('.mx-sc .mx-sc-label').allTextContents(), ['qa-sweeptemplate']);
  assert.equal(await body.locator('.rp-area-cpu g.rp-s-template').count(), 1);
  await shot('resources-template-1440.png');
  await body.getByRole('button', { name: 'Change', exact: true }).click();
  const rows = await body.locator('.mx-table tbody tr').evaluateAll(nodes => nodes.map(node => ({ text: node.textContent, dim: node.classList.contains('mx-dim') })));
  assert.equal(rows.length, 3);
  assert.deepEqual(rows.map(row => row.dim), [false, false, true], 'the sparse row is dimmed');
  assert.match(rows[2].text, /Hard2 \(sparse\)60m6\.0 cores @ \+4m6\.0 GiB @ \+5m/);
  pass('work classes and table', { rows });

  // The dashboard is dark-only: an OS light preference changes nothing.
  await page.emulateMedia({ colorScheme: 'light' });
  assert.equal(await page.evaluate(() => getComputedStyle(document.body).backgroundColor), 'rgb(11, 13, 16)');
  await shot('resources-light-preference-1440.png');
  await page.emulateMedia({ colorScheme: 'dark' });
  pass('dark-only under a light preference');

  // Phone width: no sideways page scroll; plots keep most of the width.
  await page.setViewportSize({ width: 390, height: 1500 });
  const phone = await page.evaluate(() => ({
    overflow: document.documentElement.scrollWidth > window.innerWidth,
    plot: document.querySelector('#resources-body .rp-area-cpu').getBoundingClientRect().width,
    table: getComputedStyle(document.querySelector('#resources-body .mx-table-wrap')).overflowX,
  }));
  assert.equal(phone.overflow, false, `no page overflow at 390px: ${JSON.stringify(phone)}`);
  assert.ok(phone.plot > 240, `plots keep most of the phone width: ${phone.plot}`);
  await body.locator('.rp-frame').focus();
  await page.keyboard.press('End');
  await shot('resources-390.png');
  await page.keyboard.press('Escape');
  pass('phone layout', phone);
  await page.setViewportSize({ width: 1440, height: 1700 });

  // A workspace whose host has no profiles yet says so.
  await page.goto(`${base}?workspace=ws_empty#analytics/resources`);
  await body.getByText('No resource profiles on this host for this workspace yet').waitFor();
  assert.match(await page.locator('#resources-count').textContent(), /0 profiles/);
  await shot('resources-empty-1440.png');
  pass('empty state');

  // The aggregate view never merges workspaces or hosts: it asks for one.
  const before = requests.length;
  await page.goto(`${base}?workspace=all#analytics/resources`);
  await body.getByText('Select a workspace to view this panel').waitFor();
  assert.equal(requests.length, before, 'no profile read without a concrete workspace');
  await shot('resources-aggregate-1440.png');
  pass('aggregate placeholder');

  assert.ok(requests.every(workspace => workspace === 'ws_fixture' || workspace === 'ws_empty'), JSON.stringify(requests));
  assert.deepEqual(errors, [], 'no browser errors');
  fs.writeFileSync(path.join(evidence, 'resources-assertions.json'), JSON.stringify({ passed: true, assertions }, null, 2));
  console.log(`PASS: ${assertions.length} Analytics › Resources scenarios.`);
} catch (error) {
  await page?.screenshot({ path: path.join(evidence, 'failure.png'), fullPage: true });
  throw error;
} finally {
  await browser?.close();
  server.close();
}
