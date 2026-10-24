// Usage: node dashboard_orchestrators_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/orchestrators-browser
// Header boxes of the shipped Analytics › Agents › Orchestrators ranking do not overlap at
// 1440 or 375, the ranking scrolls in its own box at 375, the Jobs badge counts
// runs, and the UTC/Local control defaults to UTC
// and survives a reload.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });

const terms = [
  ['weighted_landed', 'Weighted landed', 1, 'credit'],
  ['landed_initiative', 'Landed initiative', 1, 'credit'],
  ['spec_defects', 'Spec defects', -1, 'problem'],
  ['spec_rework', 'Spec-caused rework', -1, 'problem'],
].map(([key, label, weight, group]) => ({ key, label, weight, group, definition: label }));
const counts = { weighted_landed: 52, landed_initiative: 4, spec_defects: 3, spec_rework: 1 };
const scorecard = {
  role: 'orchestrators',
  window: '24h',
  requested_window: '24h',
  since: '2026-10-17T06:00:00Z',
  min_support: 10,
  computed_at: '2026-10-18T06:00:00Z',
  cache_ttl_seconds: 300,
  tasks_scanned: 40,
  status: 'ok',
  unit: 'owned tasks',
  fleet_average: 130,
  terms,
  rows: [{
    crew: 'luna',
    model: 'model unknown',
    units: 40,
    below_support: false,
    rank: 1,
    counts,
    per_100: Object.fromEntries(Object.entries(counts).map(([key, count]) => [key, (count * 100) / 40])),
    raw: 130,
    score: 130,
    runs_without_model: 0,
    provisional: null,
    stats: [],
  }],
};

const server = http.createServer((req, res) => {
  const served = dashboardFile(new URL(req.url, 'http://fixture').pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  res.setHeader('content-type', served.type);
  res.end(served.data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

let jobsTotal = 34;
const browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
const failures = [];
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Los_Angeles' });
  page.on('pageerror', error => failures.push(`pageerror: ${error.message}`));
  await page.route('**/api/**', async route => {
    const url = new URL(route.request().url());
    if (url.pathname === '/api/log/stream') {
      await route.fulfill({ status: 204, body: '' });
      return;
    }
    let payload = [];
    switch (url.pathname) {
      case '/api/workspaces':
        payload = [{ id: 'one', name: 'orbit', status: 'active', is_default: true }];
        break;
      case '/api/job-runs':
      case '/api/job-runs/all':
        payload = { items: [], total: url.searchParams.get('state') === 'active' ? jobsTotal : 0 };
        break;
      case '/api/agents/orchestrators/scorecard':
        payload = scorecard;
        break;
      case '/api/agents/implementers/scorecard':
      case '/api/agents/reviewers/scorecard':
      case '/api/agents/pilots/scorecard':
        payload = { ...scorecard, role: url.pathname.split('/')[3], status: 'not_computed', fleet_average: null, rows: [] };
        break;
      case '/api/agents/orchestrators/detail':
        payload = { ...scorecard, row: scorecard.rows[0], terms: terms.map(term => ({ ...term, count: 0, per_100: 0, contribution: 0, tasks: [] })) };
        break;
      case '/api/agents/attribution':
        payload = { window: '24h', requested_window: '24h', total: 0, owners: { orchestrators: 0, implementers: 0, reviewers: 0, pilots: 0, not_scored: { count: 0, reasons: {} } }, charged: {} };
        break;
      case '/api/audit/summary':
        payload = { events: 1, failed_runs: 0, denials: 0, active_long_runs: 0, window: '24h' };
        break;
      case '/api/workflows/auto/readiness':
        payload = { capacity: { drain_phase: 'idle' }, tasks: [] };
        break;
      case '/api/log':
        payload = { events: [], offset: 0 };
        break;
      default:
        break;
    }
    await route.fulfill({ json: payload });
  });

  const origin = `http://127.0.0.1:${server.address().port}`;
  const headerLayout = () => page.evaluate(() => {
    const wrap = document.querySelector('.agents-table-wrap');
    const intersects = (a, b) => Math.min(a.right, b.right) - Math.max(a.left, b.left) > 0.5
      && Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) > 0.5;
    const textBox = th => {
      const range = document.createRange();
      range.selectNodeContents(th);
      const rects = [...range.getClientRects()].filter(rect => rect.width > 0 && rect.height > 0);
      if (!rects.length) return null;
      return {
        left: Math.min(...rects.map(rect => rect.left)),
        right: Math.max(...rects.map(rect => rect.right)),
        top: Math.min(...rects.map(rect => rect.top)),
        bottom: Math.max(...rects.map(rect => rect.bottom)),
      };
    };
    const headers = [...document.querySelectorAll('.agents-table thead th')].map(th => ({
      text: th.textContent.replace(/\s+/g, ' ').trim(),
      textBox: textBox(th),
      cell: th.getBoundingClientRect().toJSON(),
    }));
    const overlaps = [];
    for (let i = 0; i < headers.length; i += 1) {
      for (let j = i + 1; j < headers.length; j += 1) {
        if (headers[i].textBox && headers[j].textBox && intersects(headers[i].textBox, headers[j].textBox)) {
          overlaps.push(`${headers[i].text} ∩ ${headers[j].text}`);
        }
      }
      const header = headers[i];
      if (header.textBox && (
        header.textBox.left < header.cell.left - 1 || header.textBox.right > header.cell.right + 1
        || header.textBox.top < header.cell.top - 1 || header.textBox.bottom > header.cell.bottom + 1
      )) overlaps.push(`${header.text} escapes its header`);
    }
    return {
      overlaps,
      scrollLeft: wrap ? wrap.scrollLeft : null,
      overflows: wrap ? wrap.scrollWidth > wrap.clientWidth + 1 : false,
      headers: headers.map(header => header.text),
    };
  });

  const settled = () => page.waitForFunction(() => {
    const meta = document.getElementById('meta-text');
    const table = document.querySelector('.agents-table');
    return meta && /UTC|PDT|PST/.test(meta.textContent) && table;
  });

  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
    await page.goto(`${origin}/?workspace=one&window=24h#analytics/agents/orchestrators`);
    await settled();
    await page.evaluate(() => document.fonts.ready);
    const jobs = await page.locator('#rail-count-jobs').textContent();
    assert.equal(jobs, '34 runs', `Jobs badge names its unit at ${width}px`);
    const clock = await page.locator('#meta-text').textContent();
    assert.match(clock, /UTC/, `the clock defaults to UTC at ${width}px: ${clock}`);
    assert.equal(await page.locator('.clock-zone-btn[data-zone="utc"]').evaluateAll(
      nodes => nodes.some(node => node.getClientRects().length && node.getAttribute('aria-pressed') === 'true'),
    ), true, `the visible UTC control is pressed at ${width}px`);
    const layout = await headerLayout();
    assert.equal(layout.scrollLeft, 0);
    if (width === 375) assert.equal(layout.overflows, true, `the ranking scrolls instead of crushing columns at ${width}px`);
    assert.deepEqual(layout.overlaps, [], `headers overlap at ${width}px: ${layout.overlaps.join('; ')} (headers: ${layout.headers.join(' | ')})`);
    assert.ok(layout.headers.some(text => text.includes('Spec-caused rework')) && layout.headers.some(text => text.startsWith('Score')), 'the last term and the score are on the table');
    await page.screenshot({ path: path.join(evidence, `agents-orchestrators-chrome-dark-${width}.png`) });
    await page.locator('.agents-table-wrap').screenshot({ path: path.join(evidence, `agents-orchestrators-table-dark-${width}.png`) });
    await page.locator('.rail').screenshot({ path: path.join(evidence, `jobs-rail-dark-${width}.png`) });
    await page.locator('.rail-foot').screenshot({ path: path.join(evidence, `clock-dark-${width}.png`) });

    await page.getByRole('button', { name: 'Local' }).click();
    await page.waitForFunction(() => /PDT|PST/.test(document.getElementById('meta-text').textContent));
    await page.reload();
    await settled();
    const kept = await page.locator('#meta-text').textContent();
    assert.match(kept, /PDT|PST/, `Local survives a reload at ${width}px: ${kept}`);
    await page.getByRole('button', { name: 'UTC' }).click();
    await page.waitForFunction(() => /\d{2}:\d{2}:\d{2} UTC/.test(document.getElementById('meta-text').textContent));
    await page.evaluate(() => localStorage.removeItem('orbit.dashboard.clock'));
  }

  jobsTotal = 1;
  await page.locator('#refresh-btn').click();
  await page.waitForFunction(() => document.getElementById('rail-count-jobs').textContent === '1 run');
  jobsTotal = 18000;
  await page.locator('#refresh-btn').click();
  await page.waitForFunction(() => document.getElementById('rail-count-jobs').textContent === '18.0k runs');

  // The packed top bar (long drain label, three wide chips) stays one row, and
  // the refresh line and its clock stay in view at the foot of the rail.
  await page.evaluate(async () => {
    const { renderHostResources } = await import('/static/js/host-resources.js');
    const drain = document.getElementById('global-drain-state');
    drain.hidden = false;
    drain.querySelector('.global-drain-label').textContent = 'Per-workspace drain status';
    renderHostResources({
      cpu: { percent: 1234, severity: 'critical' }, memory: { percent: 100, severity: 'critical' },
      disk: { path: '/workspace', percent: 100, severity: 'critical' }, sample_age_seconds: 1, max_age_seconds: 15,
      stale: false, throttle: true, pressures: [{ resource: 'cpu' }], reason: 'cpu load high', thresholds: { enabled: true },
    });
  });
  for (const width of [1024, 1280, 1440, 1600]) {
    await page.setViewportSize({ width, height: 900 });
    const bar = await page.evaluate(width => {
      const topbar = document.querySelector('.topbar').getBoundingClientRect();
      const refresh = document.getElementById('refresh-btn').getBoundingClientRect();
      const rail = document.querySelector('.rail').getBoundingClientRect();
      const inRail = node => {
        const box = node.getBoundingClientRect();
        return box.width > 0 && box.left >= rail.left && box.right <= rail.right + 0.5 && box.bottom <= rail.bottom + 0.5;
      };
      return {
        height: topbar.height,
        refreshInside: refresh.right <= topbar.right + 0.5 && refresh.left >= topbar.left,
        clocks: document.querySelectorAll('.clock-zone').length,
        clockInRail: inRail(document.querySelector('.rail-foot .clock-zone')),
        connInRail: inRail(document.getElementById('meta-text')),
      };
    }, width);
    assert.ok(bar.height <= 64, `top bar wraps at ${width}px (${bar.height})`);
    assert.equal(bar.refreshInside, true, `Refresh leaves the bar at ${width}px`);
    assert.equal(bar.clocks, 1, `one UTC/Local control at ${width}px`);
    assert.equal(bar.clockInRail, true, `the rail foot shows the clock at ${width}px`);
    assert.equal(bar.connInRail, true, `the rail foot shows the refresh line at ${width}px`);
  }
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ ok: true, failures }, null, 2));
} finally {
  await browser.close();
  server.close();
}
if (failures.length) throw new Error(failures.join('\n'));
