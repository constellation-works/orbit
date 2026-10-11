// Usage: node dashboard_rail_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/rail-browser
// Exercise the shipped rail markup and styles with representative rendered counts.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { dashboardFile } from './dashboard_static.mjs';

const { chromium } = await import(pathToFileURL(path.resolve(process.argv[2])).href);
const evidence = path.resolve(process.argv[3]);
fs.mkdirSync(evidence, { recursive: true });
const server = http.createServer((req, res) => {
  const pathname = new URL(req.url, 'http://fixture').pathname;
  const served = dashboardFile(pathname);
  if (!served) { res.writeHead(404); res.end(); return; }
  const data = pathname === '/'
    ? served.data.toString().replace(/<script[^>]*src="[^"]*app.js"[^>]*><\/script>/g, '')
    : served.data;
  res.setHeader('content-type', served.type);
  res.end(data);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
const measurements = [];
const failures = [];
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&window=24h#tasks`);
  await page.evaluate(() => document.fonts.ready);
  const cases = [
    { name: 'four-digit', events: '6837', denials: '33 denied', routines: '33 routines' },
    { name: 'five-digit', events: '68.4k', denials: '333 denied', routines: '333 routines' },
    { name: 'large', events: '987.7k', denials: '12.3k denied', routines: '1000 routines' },
    { name: 'cleared', events: '6837', denials: '', routines: '' },
  ];
  for (const width of [1440, 1280, 1024, 761, 760, 390, 375]) {
    await page.setViewportSize({ width, height: 900 });
    for (const fixture of cases) {
      await page.evaluate(fixture => {
        for (const node of document.querySelectorAll('.rail .tab > .rail-count')) {
          node.textContent = fixture.events;
        }
        for (const [id, text] of [
          ['rail-count-audit-denials', fixture.denials],
          ['rail-count-routine-failures', fixture.routines],
        ]) {
          const node = document.getElementById(id);
          node.textContent = text;
          node.hidden = !text;
        }
      }, fixture);
      await page.evaluate(() => new Promise(requestAnimationFrame));
      const layout = await page.locator('.rail .tab').evaluateAll(nodes => {
        const box = node => {
          const { left, right, top, bottom, width, height } = node.getBoundingClientRect();
          return { left, right, top, bottom, width, height };
        };
        return nodes.map(node => {
          const label = node.querySelector('.rail-label');
          const range = document.createRange();
          range.selectNodeContents(label);
          const style = getComputedStyle(label);
          return {
            tab: node.dataset.tab, button: box(node), label: box(label), text: box(range),
            overflow: style.overflowX, ellipsis: style.textOverflow,
            counts: [...node.querySelectorAll('.rail-count')].filter(count => !count.hidden && count.textContent)
              .map(count => ({ text: count.textContent, ...box(count) })),
          };
        });
      });
      const intersects = (a, b) => Math.min(a.right, b.right) - Math.max(a.left, b.left) > 0.5
        && Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) > 0.5;
      for (const row of layout) {
        const context = `${width}px / ${fixture.name} / ${row.tab}`;
        if (row.label.width < 12) failures.push(`${context}: label has no readable space`);
        const clipped = row.text.width > row.label.width + 0.5;
        if (clipped && (row.overflow === 'visible' || row.ellipsis !== 'ellipsis')) {
          failures.push(`${context}: text spills beyond its label without an ellipsis`);
        }
        if (['audit', 'diagnostics'].includes(row.tab) && clipped) {
          failures.push(`${context}: Audit/Health label is truncated`);
        }
        const visibleText = clipped && row.overflow !== 'visible'
          ? { ...row.text, left: Math.max(row.text.left, row.label.left), right: Math.min(row.text.right, row.label.right) }
          : row.text;
        for (let index = 0; index < row.counts.length; index += 1) {
          const count = row.counts[index];
          if (intersects(row.label, count) || intersects(visibleText, count)) failures.push(`${context}: label overlaps ${count.text}`);
          if (count.left < row.button.left || count.right > row.button.right + 0.5
              || count.top < row.button.top || count.bottom > row.button.bottom + 0.5) {
            failures.push(`${context}: count leaves its button`);
          }
          if (row.counts.slice(index + 1).some(other => intersects(count, other))) {
            failures.push(`${context}: counts overlap each other`);
          }
        }
      }
      measurements.push({ width, case: fixture.name, layout });
      if (width > 760) {
        await page.locator('.rail').screenshot({ path: path.join(evidence, `rail-${width}-${fixture.name}.png`) });
      } else {
        // Bring every destination into view, including entries beyond the horizontal scroll.
        for (const row of layout) {
          const tab = page.locator(`.rail .tab[data-tab="${row.tab}"]`);
          await tab.scrollIntoViewIfNeeded();
          await page.locator('.rail').screenshot({ path: path.join(evidence, `rail-${width}-${fixture.name}-${row.tab}.png`) });
        }
      }
      if (await page.evaluate(() => document.scrollingElement.scrollWidth > innerWidth)) {
        failures.push(`${width}px / ${fixture.name}: page overflows horizontally`);
      }
    }
  }
  fs.writeFileSync(path.join(evidence, 'measurements.json'), `${JSON.stringify({ browser: browser.version(), measurements, failures }, null, 2)}\n`);
  assert.deepEqual(failures, [], 'Rail labels and counts must remain readable, separate and inside their buttons');
  console.log(`PASS: rail label/count geometry at seven viewport widths, both two-count entries, large and cleared counts; screenshots: ${evidence}`);
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
