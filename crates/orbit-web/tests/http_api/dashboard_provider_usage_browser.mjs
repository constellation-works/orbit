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
  const payload = await (await page.request.get(`${origin}/api/host/resources`)).json();
  const wide = await chips();
  const multiplier = provider => wide.find(chip => chip.key === provider).value;
  for (const chip of wide) {
    const reported = payload.provider_pressures.find(item => item.provider === chip.key).pressure;
    assert.ok(Math.abs(Number(chip.value.slice(1)) - reported) < 0.1, 'the chip shows the raw API pressure');
  }
  assert.deepEqual(wide.map(chip => chip.line), [`codex ${multiplier('codex')} 100% old | 0% secondary off`, `claude ${multiplier('claude')} 5% 5h | 10% 7d`],
    'one chip per provider with its windows together, the worst first');
  assert.equal(wide[0].flag, 'old', 'the stale codex reading says so');
  assert.match(wide[0].className, /critical gating-off/);
  assert.ok(!wide[0].className.includes('stale'), 'a fresh weekly window keeps the account chip fresh');
  assert.match(wide[0].title, /codex primary 100% · observed .* \(3d old · stale\) · gating off/);
  assert.match(wide[1].title, /claude ×25\.\d+ \(seven_day 10%, resets /);
  assert.match(wide[1].title, /claude five_hour 5% · observed .* \(today\) · limit 90%\nclaude seven_day 10%/);
  const layout = await page.evaluate(() => {
    const divider = getComputedStyle(document.getElementById('provider-usage-chips'), '::before');
    const warn = document.getElementById('host-warn');
    const usage = document.querySelector('#provider-usage-chips .provider-usage-item').getBoundingClientRect();
    const warnShown = !!(warn && !warn.hidden && warn.getClientRects().length);
    const dividerShown = divider.content !== 'none' && parseFloat(divider.width) > 0;
    return {
      hostChips: !!document.getElementById('host-resource-chips'),
      hostName: !!document.getElementById('host-resource-name'),
      dividerShown,
      warnShown,
      after: warnShown ? usage.left > warn.getBoundingClientRect().right : true,
    };
  });
  assert.equal(layout.hostChips, false, 'host readings are not chips in the top bar');
  assert.equal(layout.hostName, false, 'the top bar does not name the host');
  assert.equal(layout.dividerShown, layout.warnShown, 'a divider sets provider usage apart only while the host warning chip is shown');
  assert.equal(layout.after, true, 'provider usage sits to the right of the host warning chip');
  assert.ok(wide.every(chip => chip.label === chip.title), 'the accessible name is the title');
  await page.screenshot({ path: path.join(evidence, 'topbar-1920.png'), clip: { x: 0, y: 0, width: 1920, height: 64 } });

  // The bar shows the same readings on every destination.
  const routes = ['tasks', 'operations/routines', 'config/effective', 'knowledge/frictions', 'plugins'];
  for (const route of routes) {
    await page.evaluate(async route => (await import('/static/js/router.js')).setActiveTab(route), route);
    const shown = await chips();
    const stableLine = chip => chip.line.replace(/×[0-9.e+-]+/, '×<pressure>');
    assert.deepEqual(shown.map(stableLine), wide.map(stableLine), `provider usage on #${route}`);
  }

  // Narrower bars keep one summary chip, with every reading in its title.
  for (const width of [1280, 375]) {
    await page.setViewportSize({ width, height: 900 });
    const [summary, ...rest] = await chips();
    assert.equal(rest.length, 0, `one summary chip at ${width}px`);
    assert.deepEqual([summary.key, summary.value], ['codex', multiplier('codex')]);
    assert.equal(summary.title.split('\n').length, 6, 'the summary title lists every reading');
    if (width === 375) await page.locator('#provider-usage-chips .provider-usage-summary').scrollIntoViewIfNeeded();
    const box = await page.evaluate(() => {
      const chip = document.querySelector('#provider-usage-chips .provider-usage-summary');
      const bar = document.querySelector('.topbar').getBoundingClientRect();
      const rect = chip.getBoundingClientRect();
      return { width: rect.width, inside: rect.right <= bar.right + 0.5, pageOverflow: document.documentElement.scrollWidth > innerWidth };
    });
    assert.ok(box.inside, `the summary chip stays inside the bar at ${width}px`);
    assert.equal(box.pageOverflow, false, `no sideways page scroll at ${width}px`);
    await page.screenshot({ path: path.join(evidence, `topbar-${width}.png`), clip: { x: 0, y: 0, width, height: width === 375 ? 140 : 64 } });
  }
  if (process.env.ORBIT_PROVIDER_USAGE_BEFORE_MODULE) {
    const source = fs.readFileSync(process.env.ORBIT_PROVIDER_USAGE_BEFORE_MODULE, 'utf8');
    await page.route('**/static/js/host-resources.js*', route => route.fulfill({ status: 200, contentType: 'text/javascript', body: source }));
    for (const width of [1920, 375]) {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`${origin}/?workspace=ws_http_fixture#tasks`);
      await page.waitForFunction(() => document.querySelectorAll('#provider-usage-chips .provider-usage-item').length === 2);
      if (width === 375) await page.locator('#provider-usage-chips .provider-usage-summary').scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(evidence, `topbar-before-${width}.png`), clip: { x: 0, y: 0, width, height: width === 375 ? 140 : 64 } });
    }
  }
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, wide }, null, 2));
  console.log('Provider usage top bar chips passed against the real server.');
} finally {
  await browser.close();
}
