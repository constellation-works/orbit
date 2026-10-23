// Invoked by provider_auth::dashboard_top_bar_shows_provider_usage_on_every_tab
// against its isolated HTTP server. Every chip comes from the real
// /api/host/resources payload and the shipped app.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [modulePath, evidencePath, origin] = process.argv.slice(2);
const { chromium } = await import(pathToFileURL(modulePath).href);
const evidence = path.resolve(evidencePath);
fs.mkdirSync(evidence, { recursive: true });
const errors = [];
const browser = await chromium.launch({ headless: true, executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined });
try {
  const page = await browser.newPage({ viewport: { width: 1920, height: 900 } });
  page.on('pageerror', error => errors.push(String(error)));
  await page.goto(`${origin}/?workspace=ws_http_fixture#tasks`);
  await page.waitForFunction(() => document.querySelectorAll('#provider-usage-chips .provider-usage-item').length === 2, undefined, { timeout: 20000 });
  const chips = () => page.evaluate(() => [...document.querySelectorAll('#provider-usage-chips .provider-usage')]
    .filter(chip => chip.getClientRects().length)
    .map(chip => ({
      key: chip.querySelector('.k')?.textContent, value: chip.querySelector('.v')?.textContent,
      line: [...chip.querySelectorAll('.k, .v, .provider-usage-window, .provider-usage-sep, .provider-usage-flag')].map(node => node.textContent).join(' '),
      flag: chip.querySelector('.provider-usage-flag')?.textContent ?? null, className: chip.className,
      title: chip.title, label: chip.getAttribute('aria-label'),
    })));
  const wide = await chips();
  assert.deepEqual(wide.map(chip => chip.line), ['codex 100% old', 'claude 5% 5h | 26% 7d'],
    'one chip per provider with its windows together, the worst first');
  assert.equal(wide[0].flag, 'old', 'the stale codex reading says so');
  assert.match(wide[0].className, /critical stale gating-off/);
  assert.match(wide[0].title, /codex primary 100% · observed .* \(3d old · stale\) · gating off/);
  assert.match(wide[1].title, /claude five_hour 5% · observed .* \(today\) · limit 90%\nclaude seven_day 26%/);
  const layout = await page.evaluate(() => {
    const divider = getComputedStyle(document.getElementById('provider-usage-chips'), '::before');
    const host = document.getElementById('host-resource-chips').getBoundingClientRect();
    const usage = document.querySelector('#provider-usage-chips .provider-usage-item').getBoundingClientRect();
    return { divider: divider.content !== 'none' && parseFloat(divider.width) > 0, after: usage.left > host.right };
  });
  assert.deepEqual(layout, { divider: true, after: true }, 'a divider sets provider usage apart from the host readings');
  assert.ok(wide.every(chip => chip.label === chip.title), 'the accessible name is the title');
  await page.screenshot({ path: path.join(evidence, 'topbar-1920.png'), clip: { x: 0, y: 0, width: 1920, height: 64 } });

  // The bar shows the same readings on every destination.
  const routes = ['tasks', 'operations/routines', 'config/effective', 'knowledge/frictions', 'plugins'];
  for (const route of routes) {
    await page.evaluate(async route => (await import('/static/js/router.js')).setActiveTab(route), route);
    const shown = await chips();
    assert.deepEqual(shown.map(chip => chip.line), wide.map(chip => chip.line), `provider usage on #${route}`);
  }

  // Narrower bars keep one summary chip, with every reading in its title.
  for (const width of [1280, 375]) {
    await page.setViewportSize({ width, height: 900 });
    const [summary, ...rest] = await chips();
    assert.equal(rest.length, 0, `one summary chip at ${width}px`);
    assert.deepEqual([summary.key, summary.value], ['usage', '100%']);
    assert.equal(summary.title.split('\n').length, 3, 'the summary title lists every reading');
    const box = await page.evaluate(() => {
      const chip = document.querySelector('#provider-usage-chips .provider-usage-summary');
      const bar = document.querySelector('.topbar').getBoundingClientRect();
      const rect = chip.getBoundingClientRect();
      return { width: rect.width, inside: rect.right <= bar.right + 0.5, pageOverflow: document.documentElement.scrollWidth > innerWidth };
    });
    assert.ok(box.inside || width === 375, `the summary chip stays inside the bar at ${width}px`);
    assert.equal(box.pageOverflow, false, `no sideways page scroll at ${width}px`);
    await page.screenshot({ path: path.join(evidence, `topbar-${width}.png`), clip: { x: 0, y: 0, width, height: width === 375 ? 140 : 64 } });
  }
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, wide }, null, 2));
  console.log('Provider usage top bar chips passed against the real server.');
} finally {
  await browser.close();
}
