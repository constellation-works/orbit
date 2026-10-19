// Usage: node dashboard_orchestrators_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/orchestrators-browser
// Header boxes of the shipped Orchestrators table do not overlap at 1440 or
// 375, the Jobs badge counts runs, and the UTC/Local control defaults to UTC
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

const groups = [
  ['throughput', 'Throughput', ['owned', 'Owned'], ['landed', 'Landed'], ['weighted_landed', 'Weighted landed']],
  ['proactivity', 'Proactivity', ['landed_initiative', 'Landed initiative'], ['dropped_initiative', 'Dropped initiative'], ['net_initiative', 'Net initiative'], ['unknown_origin', 'Unknown origin']],
  ['spec_defects', 'Spec defects', ['spec_defects', 'Spec defects'], ['spec_edits', 'Edited after run start'], ['criteria_blocks', 'Criteria blocks'], ['criteria_escalations', 'Criteria escalations'], ['spec_regressions', 'Spec regressions']],
  ['rework', 'Rework', ['rework', 'Rework'], ['review_rejects', 'Review rejects'], ['pr_rework', 'Blocked or unmerged PRs'], ['repeat_runs', 'Repeat runs'], ['operator_requeues', 'Operator requeues']],
];
const components = groups.flatMap(([group, , ...members]) => members.map(([key, label]) => ({ key, group, label, definition: label })));
const zeros = Object.fromEntries(components.map(component => [component.key, 0]));
const rates = Object.fromEntries(components.map(component => [component.key, 1.2]));
const scorecard = {
  window: '24h',
  min_support: 10,
  computed_at: '2026-10-18T06:00:00Z',
  definitions: {
    groups: groups.map(([key, label]) => ({ key, label })),
    components,
    causes: [],
    complexity_weights: { low: 1, medium: 2, hard: 4, xhard: 8, unassessed: 1 },
    composite: { definition: 'Weighted landed + landed initiative − spec defects − rework.', weights: [] },
    normalization: { per_100_tasks: 'per 100 owned tasks', per_8_hours: 'per 8 active hours' },
  },
  rows: [{
    orchestrator: 'luna',
    below_support: false,
    support: { owned_tasks: 40, active_hours: 12, tasks_with_runs: 30 },
    counts: { ...zeros, review_rejects: 4 },
    per_100_tasks: rates,
    per_8_hours: rates,
    composite: { value: 3, per_100_tasks: 3.4, per_8_hours: 1.1 },
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
      case '/api/orchestrators/scorecard':
        payload = scorecard;
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
    const wrap = document.querySelector('.orch-table-wrap');
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
    const headers = [...document.querySelectorAll('.orch-table thead th')].map(th => ({
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
    const table = document.querySelector('.orch-table');
    return meta && /UTC|PDT|PST/.test(meta.textContent) && table;
  });

  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
    await page.goto(`${origin}/?workspace=one&window=24h#analytics/orchestrators`);
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
    assert.equal(layout.overflows, true, `the scorecard scrolls instead of crushing columns at ${width}px`);
    assert.deepEqual(layout.overlaps, [], `headers overlap at ${width}px: ${layout.overlaps.join('; ')} (headers: ${layout.headers.join(' | ')})`);
    assert.ok(layout.headers.some(text => text.includes('Review rejects')) && layout.headers.some(text => text.startsWith('Composite')), 'the colliding headers are on the table');
    await page.screenshot({ path: path.join(evidence, `orchestrators-dark-${width}.png`) });
    await page.locator('.orch-table-wrap').screenshot({ path: path.join(evidence, `orchestrators-table-dark-${width}.png`) });
    await page.locator('.rail').screenshot({ path: path.join(evidence, `jobs-rail-dark-${width}.png`) });
    const clockShot = width >= 1280 ? page.locator('.topbar') : page.locator('.rail-workspace');
    await clockShot.screenshot({ path: path.join(evidence, `clock-dark-${width}.png`) });

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

  // The packed top bar (long drain label, three wide chips) stays one row
  // with the clock shown from 1280px up and hidden below it.
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
      const clock = document.querySelector('.topbar .clock-zone').getBoundingClientRect();
      const railClock = document.querySelector('.rail-clock').getBoundingClientRect();
      return {
        height: topbar.height,
        refreshInside: refresh.right <= topbar.right + 0.5 && refresh.left >= topbar.left,
        clockShown: clock.width > 0,
        railClockShown: railClock.width > 0,
        expectClock: width >= 1280,
      };
    }, width);
    assert.ok(bar.height <= 64, `top bar wraps at ${width}px (${bar.height})`);
    assert.equal(bar.refreshInside, true, `Refresh leaves the bar at ${width}px`);
    assert.equal(bar.clockShown, bar.expectClock, `top-bar clock visibility at ${width}px`);
    assert.equal(bar.railClockShown, !bar.expectClock, `rail clock visibility at ${width}px`);
  }
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ ok: true, failures }, null, 2));
} finally {
  await browser.close();
  server.close();
}
if (failures.length) throw new Error(failures.join('\n'));
