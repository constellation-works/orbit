// Usage: node dashboard_forecast_residuals_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/forecast-residuals
// Exercise the shipped forecast renderer and CSS against scrolling residual histories.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from '../../src/tests/dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
let count = 120;
const rows = () => Array.from({ length: count }, (_, i) => ({
  forecast_at: new Date(Date.UTC(2026, 9, 1, 0, i)).toISOString(), model_version: i < 60 ? 2 : 3,
  residual: { hours: { median: i % 2 ? 0.4 : -0.3, n: 3 }, usd: { median: 0.2, n: 3 }, tokens: { median: -0.2, n: 3 }, time_to_drain_hours: i % 2 ? -0.35 : 0.5 },
  actual: { time_to_drain_hours: 4 }, forecast: { time_to_drain_hours: { p50: 3, p90: 5 } },
}));
const payload = () => ({
  backlog: { backlog: 1, in_progress: 0, blocked: 0, proposed: 0 },
  inputs: { concurrency: 5, history_deliveries: 12 },
  forecast: { tasks: 1, time_to_drain_hours: { p50: 3, p90: 5 }, usd: { p50: 10, p90: 20 }, tokens: { p50: 1000, p90: 2000 }, task_predictions: [] },
  coverage: {}, calibration: { model_version: 3, forecasts: rows() }, stored: {},
});
const html = `<!doctype html><html><head><link rel="stylesheet" href="/static/dashboard.css"></head><body><div class="panel"><div id="forecast-body" class="body forecast-body"></div></div><span id="forecast-count"></span><script type="module">import { fetchAndRenderForecast } from '/static/js/forecast.js'; window.loadForecast = fetchAndRenderForecast; await window.loadForecast();</script></body></html>`;
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost');
  if (url.pathname === '/') { res.setHeader('content-type', 'text/html'); res.end(html); return; }
  if (url.pathname === '/api/diagnostics/backlog_forecast') { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(payload())); return; }
  const asset = dashboardFile(url.pathname);
  if (!asset) { res.writeHead(404); res.end(); return; }
  res.setHeader('content-type', asset.type); res.end(asset.data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true });
  for (const width of [375, 1440]) {
    const page = await browser.newPage({ viewport: { width, height: 900 }, colorScheme: 'dark' });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.locator('.fc-res-plot').first().waitFor();
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    const charts = page.locator('.fc-res-plot');
    assert.equal(await charts.count(), 2);
    for (let i = 0; i < 2; i++) {
      const chart = charts.nth(i);
      const initial = await chart.evaluate(el => ({ left: el.scrollLeft, max: el.scrollWidth - el.clientWidth }));
      assert.ok(initial.max > 0 && Math.abs(initial.left - initial.max) <= 16, `long chart starts at newest sample: ${JSON.stringify(initial)}`);
      for (const position of ['initial', 'middle', 'start']) {
        const geometry = await chart.evaluate((el, pos) => {
          if (pos === 'middle') el.scrollLeft = (el.scrollWidth - el.clientWidth) / 2;
          if (pos === 'start') el.scrollLeft = 0;
          const plot = el.getBoundingClientRect(), zero = el.querySelector('.fc-res-zero').getBoundingClientRect();
          return { plot: { left: plot.left, right: plot.right, top: plot.top }, zero: { left: zero.left, right: zero.right, top: zero.top } };
        }, position);
        assert.ok(geometry.zero.left <= geometry.plot.left + 1 && geometry.zero.right >= geometry.plot.right - 1, `chart ${i} ${position}: baseline spans visible plot ${JSON.stringify(geometry)}`);
        assert.ok(Math.abs(geometry.zero.top - geometry.plot.top - 60) < 1, 'baseline remains at p50');
      }
      await chart.evaluate(el => { el.scrollLeft = el.scrollWidth; });
    }
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, 'no page overflow');
    await page.locator('.fc-residuals').scrollIntoViewIfNeeded();
    await page.locator('.fc-residuals').screenshot({ path: path.join(evidence, `forecast-residuals-dark-${width}.png`) });
    await page.getByRole('group', { name: 'Residual measure' }).getByRole('button', { name: 'Cost' }).click();
    assert.equal(await page.locator('.fc-res-plot').count(), 2, 'metric switch retains task and drain charts');
    assert.match(await page.locator('.fc-res-head').first().textContent(), /Task cost/);
    assert.deepEqual(errors, []);
    await page.close();
  }
  count = 2;
  const page = await browser.newPage({ viewport: { width: 375, height: 900 } });
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.locator('.fc-res-plot').first().waitFor();
  assert.equal(await page.locator('.fc-res-bar').count(), 4, 'short task and drain series render');
  count = 0;
  await page.evaluate(() => window.loadForecast());
  assert.match(await page.locator('.fc-residuals').textContent(), /No scored residuals yet/);
  await page.close();
  console.log('PASS: residual baseline geometry, metric switching, short and empty series');
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
