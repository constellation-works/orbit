// Invoked by provider_auth::dashboard_top_bar_shows_provider_usage_on_every_tab
// against its isolated HTTP server. Every chip comes from the real
// /api/host/resources payload and the shipped app; the many-provider screens
// replace only the readings in that payload, so the layout is checked against
// the readings of a busy host.
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

// The readings of a busy host: a provider with five scoped windows, one with
// a 5-hour and a 7-day window, one whose gate is off, and two more.
const busy = () => {
  const now = new Date();
  const reading = (provider, window, used, extra = {}) => ({
    provider, scope: null, window, used_percent: used, exhausted: false, resets_at: null, source: 'native',
    observed_at: now.toISOString(), age_seconds: 30, stale: false, gating: true, partial: false, threshold: 90,
    gated: false, until: new Date(now.getTime() + 6 * 3600e3).toISOString(), crews: [provider], ...extra,
  });
  const in_ = hours => new Date(now.getTime() + hours * 3600e3).toISOString();
  const pressure = (provider, value, window) => ({ provider, pressure: value, pressure_window: window });
  return {
    provider_limits: [
      reading('antigravity', 'gemini-argon-wtu-weekly', 0, { scope: 'argon', resets_at: in_(70) }),
      reading('antigravity', '3p-weekly', 0, { scope: 'claude', resets_at: in_(70) }),
      reading('antigravity', 'gemini-weekly', 5, { scope: 'flash', resets_at: in_(70) }),
      reading('antigravity', '3p-weekly', 0, { scope: 'gpt', resets_at: in_(70) }),
      reading('antigravity', 'gemini-weekly', 5.4, { scope: 'pro', resets_at: in_(70) }),
      reading('claude', 'five_hour', 13, { resets_at: in_(3.5), crews: ['opus', 'sonnet'] }),
      reading('claude', 'seven_day', 11, { resets_at: in_(52), crews: ['opus', 'sonnet'] }),
      reading('codex', 'primary', 23, { threshold: 'off', resets_at: in_(2) }),
      reading('gemini', 'primary', 87, { resets_at: in_(20) }),
      reading('grok', 'primary', 3, { resets_at: in_(20) }),
    ],
    provider_pressures: [
      pressure('antigravity', 1, null),
      pressure('claude', 1.09, { window: 'seven_day', used_percent: 11, resets_at: in_(52) }),
      pressure('codex', 0.91, { window: 'primary', used_percent: 23, resets_at: in_(2) }),
      pressure('gemini', 1.2, { window: 'primary', used_percent: 87, resets_at: in_(20) }),
      pressure('grok', 1, null),
    ],
  };
};

try {
  const context = await browser.newContext({ viewport: { width: 1920, height: 900 } });
  const page = await context.newPage();
  page.on('pageerror', error => errors.push(String(error)));
  let override = null;
  await page.route('**/api/host/resources', async route => {
    const response = await route.fetch();
    const body = await response.json();
    await route.fulfill({ response, json: override ? { ...body, ...override } : body });
  });
  const shoot = (name, width, height, x = 0) => page.screenshot({ path: path.join(evidence, name), clip: { x, y: 0, width: width - x, height } });
  const load = async (width, want) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`${origin}/?workspace=ws_http_fixture#tasks`);
    await page.waitForFunction(want, undefined, { timeout: 20000 });
  };
  const items = () => page.evaluate(() => [...document.querySelectorAll('#provider-usage-chips .provider-usage-item')].map(chip => chip.textContent));
  const chips = () => page.evaluate(() => [...document.querySelectorAll('#provider-usage-chips .provider-usage')]
    .filter(chip => chip.getClientRects().length)
    .map(chip => ({
      tag: chip.tagName, key: chip.querySelector('.k')?.textContent, value: chip.querySelector('.v')?.textContent,
      pressure: chip.querySelector('.provider-usage-pressure')?.textContent ?? null,
      flag: chip.querySelector('.provider-usage-flag')?.textContent ?? null, className: chip.className,
      hasTitle: chip.hasAttribute('title'), label: chip.getAttribute('aria-label'), // Flags are upper-cased by CSS.
      text: chip.innerText.replace(/\s+/g, ' ').trim().toLowerCase(),
      expanded: chip.getAttribute('aria-expanded'), popup: chip.getAttribute('aria-haspopup'),
    })));
  const dialog = page.locator('#provider-usage-dialog');
  const chip = key => page.locator(`#provider-usage-chips [data-usage-key="${key}"]`).locator('visible=true').first();
  const open = () => dialog.isVisible();
  const expanded = () => page.evaluate(() => document.querySelectorAll('#provider-usage-chips [aria-expanded="true"]').length);
  const settle = (ms = 450) => page.waitForTimeout(ms);
  const dialogRows = () => page.evaluate(() => [...document.querySelectorAll('#provider-usage-dialog .usage-row')].map(row => row.innerText.replace(/\s+/g, ' ').trim()));
  const away = () => page.mouse.move(900, 700, { steps: 4 });

  // ---- The real payload: two providers with the real fixture's readings.
  await load(1920, () => document.querySelectorAll('#provider-usage-chips .provider-usage-item').length === 2);
  const payload = await (await page.request.get(`${origin}/api/host/resources`)).json();
  const wide = await chips();
  const pressureOf = provider => wide.find(item => item.key === provider).pressure;
  for (const item of wide) {
    const reported = payload.provider_pressures.find(value => value.provider === item.key).pressure;
    assert.ok(Math.abs(Number(item.pressure.slice(1)) - reported) < 0.1, 'the chip shows the raw API pressure');
    assert.equal(item.tag, 'BUTTON', 'a chip is a real button');
    assert.equal(item.popup, 'dialog');
    assert.equal(item.expanded, 'false');
    assert.equal(item.hasTitle, false, 'the dialog carries the detail, so the chip has no native tooltip');
  }
  assert.deepEqual(wide.map(item => item.text), [`codex 100% ${pressureOf('codex')} no gate`, `claude 10% ${pressureOf('claude')}`],
    'one chip per provider: name, worst value and pressure, worst provider first');
  assert.match(wide[0].className, /critical gating-off/);
  assert.equal(wide[0].flag, 'no gate', 'a gating-off provider says "no gate", not "off"');
  assert.ok(!wide[0].className.includes('stale'), 'a fresh weekly window keeps the account chip fresh');
  const layout = await page.evaluate(() => {
    const divider = getComputedStyle(document.getElementById('provider-usage-chips'), '::before');
    const warn = document.getElementById('host-warn');
    const usage = document.querySelector('#provider-usage-chips .provider-usage-item').getBoundingClientRect();
    const warnShown = !!(warn && !warn.hidden && warn.getClientRects().length);
    const codex = document.querySelector('#provider-usage-chips .gating-off');
    return {
      hostChips: !!document.getElementById('host-resource-chips'),
      hostName: !!document.getElementById('host-resource-name'),
      dividerShown: divider.content !== 'none' && parseFloat(divider.width) > 0,
      warnShown,
      after: warnShown ? usage.left > warn.getBoundingClientRect().right : true,
      dashed: getComputedStyle(codex).borderStyle,
    };
  });
  assert.equal(layout.hostChips, false, 'host readings are not chips in the top bar');
  assert.equal(layout.hostName, false, 'the top bar does not name the host');
  assert.equal(layout.dividerShown, layout.warnShown, 'a divider sets provider usage apart only while the host warning chip is shown');
  assert.equal(layout.after, true, 'provider usage sits to the right of the host warning chip');
  assert.equal(layout.dashed, 'dashed', 'a gating-off provider has a dashed border');
  await shoot('topbar-closed-dark-1920.png', 900, 64);

  // The dialogs show what the old tooltip held.
  await chip('claude').click();
  await dialog.waitFor({ state: 'visible' });
  assert.equal(await dialog.getAttribute('role'), 'dialog');
  const claudeText = await dialog.innerText();
  assert.match(claudeText, /^claude\s+within limits\s+observed /i);
  assert.match(claudeText, /Weekly pressure\s+×25\.\d+/i);
  assert.match(claudeText, /Above ×1 .* below ×1 .* at ×1 .* never holds any back/s);
  assert.match(claudeText, /7-day · 10% used · resets /);
  assert.deepEqual((await dialogRows()).map(row => row.split(' ')[0]), ['7-day', '5-hour'], 'one row per window, most used first');
  assert.match((await dialogRows())[0], /^7-day 10% resets .* UTC · in \d+h gate at 90%$/i);
  assert.match(claudeText, /No crews held back/);
  await page.keyboard.press('Escape');
  await chip('codex').click();
  await dialog.waitFor({ state: 'visible' });
  const codexText = await dialog.innerText();
  assert.match(codexText, /gating off/i);
  assert.match(codexText, /no gate/i);
  assert.match(codexText, /primary\s+old\s+100%/i);
  assert.match(codexText, /secondary\s+0%/);
  assert.doesNotMatch(codexText, /gate at/, 'no row of a gating-off provider names a gate');
  await page.keyboard.press('Escape');

  // The bar shows the same readings on every destination.
  const routes = ['tasks', 'operations/routines', 'config/effective', 'knowledge/frictions', 'plugins'];
  for (const route of routes) {
    await page.evaluate(async route => (await import('/static/js/router.js')).setActiveTab(route), route);
    const shown = await chips();
    const stable = item => item.text.replace(/×[0-9.e+-]+/, '×<pressure>');
    assert.deepEqual(shown.map(stable), wide.map(stable), `provider usage on #${route}`);
  }

  // ---- A busy host, wide: five providers fold into three chips and "+2 more".
  override = busy();
  await load(1920, () => document.querySelectorAll('#provider-usage-chips .provider-usage-item').length === 4);
  const busyBar = await chips();
  assert.deepEqual(busyBar.map(item => item.key), ['gemini', 'antigravity', 'claude', '+2 more']);
  assert.deepEqual(busyBar.slice(0, 3).map(item => item.text), ['gemini 87% ×1.2', 'antigravity 5% ×1', 'claude 13% ×1.09']);
  const bar = await page.locator('.topbar').innerText();
  assert.doesNotMatch(bar, /wtu|3p-weekly|gemini-weekly|OFF/, 'no raw scoped window id or "OFF" in the bar');
  await shoot('topbar-closed-busy-dark-1920.png', 900, 64);

  // Hover opens after the intent delay and a pointer that crosses into the
  // dialog keeps it open; leaving closes it.
  assert.equal(await open(), false);
  await chip('claude').hover();
  await dialog.waitFor({ state: 'visible', timeout: 2000 });
  assert.equal(await chip('claude').getAttribute('aria-expanded'), 'true');
  assert.equal(await page.locator('.usage-pin').innerText(), 'Pin open', 'a hover preview is not pinned');
  const box = await dialog.boundingBox();
  const anchor = await chip('claude').boundingBox();
  assert.ok(box.width <= 421, `the dialog is no wider than 420px (${box.width})`);
  assert.ok(box.y >= anchor.y + anchor.height - 1, 'the dialog sits under its chip');
  assert.ok(Math.abs((box.x + box.width) - (anchor.x + anchor.width)) < 2 || box.x <= 9, 'the dialog is right-aligned to its chip');
  await page.mouse.move(box.x + box.width / 2, anchor.y + anchor.height + 2, { steps: 6 });
  await page.mouse.move(box.x + box.width / 2, box.y + 40, { steps: 6 });
  await settle(700);
  assert.equal(await open(), true, 'moving the pointer into the dialog keeps it open');
  await shoot('provider-two-window-dark-1920.png', 900, 560);
  await away();
  await dialog.waitFor({ state: 'hidden', timeout: 2000 });
  assert.equal(await expanded(), 0);
  // A pass over the chip too short to show intent opens nothing.
  await chip('claude').hover();
  await away();
  await settle(500);
  assert.equal(await open(), false, 'a quick pass does not open the dialog');

  // Click pins; the Pin button toggles; Esc, an outside click and clicking
  // the pinned chip close; one dialog is open at a time.
  await chip('claude').click();
  await away();
  await settle(500);
  assert.equal(await open(), true, 'a click keeps the dialog open after the pointer leaves');
  assert.equal(await page.locator('.usage-pin').innerText(), 'Unpin');
  await page.locator('.usage-pin').click();
  assert.equal(await page.locator('.usage-pin').innerText(), 'Pin open');
  await page.locator('.usage-pin').click();
  await chip('antigravity').click();
  assert.equal(await expanded(), 1, 'opening another chip closes the first');
  assert.equal(await chip('claude').getAttribute('aria-expanded'), 'false');
  assert.equal(await chip('antigravity').getAttribute('aria-expanded'), 'true');
  const scoped = await dialogRows();
  assert.equal(scoped.length, 5, 'one row per window');
  assert.deepEqual(scoped.map(row => row.split(' gate')[0].replace(/ resets.*/, '')), [
    'pro gemini · weekly 5%', 'flash gemini · weekly 5%', 'argon gemini · wtu · weekly 0%', 'claude 3p · weekly 0%', 'gpt 3p · weekly 0%',
  ], 'scope-named rows with the window as sublabel, most used first');
  const many = await dialog.boundingBox();
  assert.ok(many.width <= 421, 'the five-window dialog is no wider than 420px');
  assert.ok(many.y + many.height <= 900, 'the five-window dialog fits the viewport');
  await shoot('provider-many-windows-dark-1920.png', 900, 700);
  await page.keyboard.press('Escape');
  assert.equal(await open(), false, 'Esc closes the dialog');
  await chip('claude').click();
  await page.mouse.click(1200, 700);
  assert.equal(await open(), false, 'an outside click closes the dialog');
  await chip('claude').click();
  assert.equal(await open(), true);
  await chip('claude').click();
  assert.equal(await open(), false, 'clicking the pinned chip closes the dialog');
  // Keyboard focus opens a preview, and Tab reaches the Pin button.
  await page.evaluate(() => document.activeElement.blur());
  await page.mouse.move(900, 700);
  await chip('claude').focus();
  assert.equal(await open(), true, 'focus opens the dialog');
  await page.keyboard.press('Tab');
  assert.equal(await page.evaluate(() => document.activeElement.className), 'usage-pin', 'Tab from an open chip reaches Pin');
  await page.keyboard.press('Enter');
  await page.keyboard.press('Escape');
  assert.equal(await open(), false);
  assert.equal(await page.evaluate(() => document.activeElement.dataset.usageKey), 'claude', 'Esc returns focus to the chip');
  // The "+2 more" chip lists every provider, worst first, with the worst window.
  await chip('*').click();
  const everyone = await dialogRows();
  assert.deepEqual(everyone.map(row => row.split(' ')[0]), ['gemini', 'antigravity', 'claude', 'codex', 'grok'], 'every provider, not only the hidden ones, worst first');
  assert.match(everyone[1], /^antigravity 5% pro · gemini · weekly/);
  assert.match(everyone[2], /^claude 13% 5-hour/);
  assert.equal(everyone.length, 5);
  await shoot('all-providers-dark-1920.png', 900, 560);
  await page.keyboard.press('Escape');
  const noTitle = await page.evaluate(() => document.querySelectorAll('#provider-usage-chips [title]').length);
  assert.equal(noTitle, 0, 'no chip keeps a native tooltip');

  // ---- Narrower bars keep one summary chip that opens the all-providers dialog.
  for (const width of [1440, 1280, 375]) {
    await load(width, () => document.querySelectorAll('#provider-usage-chips .provider-usage-summary').length === 1);
    const [summary, ...rest] = await chips();
    assert.equal(rest.length, 0, `one summary chip at ${width}px`);
    assert.equal(summary.key, 'gemini');
    assert.equal(summary.value, '87%');
    if (width === 375) await page.locator('#provider-usage-chips .provider-usage-summary').scrollIntoViewIfNeeded();
    const geometry = await page.evaluate(() => {
      const chip = document.querySelector('#provider-usage-chips .provider-usage-summary');
      const bar = document.querySelector('.topbar').getBoundingClientRect();
      const rect = chip.getBoundingClientRect();
      return { inside: rect.right <= bar.right + 0.5, pageOverflow: document.documentElement.scrollWidth > innerWidth };
    });
    assert.ok(geometry.inside, `the summary chip stays inside the bar at ${width}px`);
    assert.equal(geometry.pageOverflow, false, `no sideways page scroll at ${width}px`);
    const height = width === 375 ? 140 : 64;
    await shoot(`summary-closed-dark-${width}.png`, width, height);
    await chip('*').click();
    await dialog.waitFor({ state: 'visible' });
    const placed = await dialog.boundingBox();
    assert.ok(placed.x >= 0 && placed.x + placed.width <= width + 0.5, `the dialog stays inside the viewport at ${width}px`);
    assert.ok(placed.width <= 421);
    assert.equal((await dialogRows()).length, 5, 'the summary chip opens the all-providers dialog');
    assert.equal(await page.locator('.usage-dialog').count(), 1);
    await shoot(`summary-all-providers-dark-${width}.png`, width, Math.min(900, Math.round(placed.y + placed.height + 16)));
    await page.keyboard.press('Escape');
  }

  // ---- Resizing across 1900px with the all-providers dialog open: the open
  // state follows the chip that is now on screen, and an unchanged poll keeps it there.
  const allChips = () => page.evaluate(() => [...document.querySelectorAll('#provider-usage-chips [data-usage-key="*"]')]
    .map(node => ({ summary: node.classList.contains('provider-usage-summary'), visible: node.getClientRects().length > 0, expanded: node.getAttribute('aria-expanded') })));
  const assertOpenOnVisible = async step => {
    const state = await allChips();
    assert.equal(state.length, 2, `the summary and +N more chips both exist ${step}`);
    for (const node of state) assert.equal(node.expanded, node.visible ? 'true' : 'false', `aria-expanded is on the visible chip only ${step}`);
  };
  const unchangedPoll = async () => {
    // The host poll runs every five seconds; the payload is the same override.
    await page.waitForResponse(response => response.url().endsWith('/api/host/resources'), { timeout: 15000 });
    await settle(250);
  };

  // Open from +N more at 1920px, then narrow past the breakpoint.
  await load(1920, () => document.querySelectorAll('#provider-usage-chips .provider-usage-item').length === 4);
  await chip('*').click();
  assert.equal(await open(), true);
  await page.setViewportSize({ width: 1440, height: 900 });
  await settle(300);
  await shoot('provider-resize-narrow-dark-1440.png', 1440, 560);
  assert.equal(await open(), true, 'the dialog stays open across the breakpoint');
  await assertOpenOnVisible('after narrowing');
  const narrowed = await dialog.boundingBox();
  const summaryBox = await chip('*').boundingBox();
  assert.ok(narrowed.y >= summaryBox.y + summaryBox.height - 1, 'the dialog sits under the summary chip after narrowing');
  await unchangedPoll();
  assert.equal(await open(), true);
  await assertOpenOnVisible('after an unchanged poll');
  await page.keyboard.press('Escape');

  // Open from the summary chip below 1900px, then widen past the breakpoint.
  await load(1440, () => document.querySelectorAll('#provider-usage-chips .provider-usage-summary').length === 1);
  await chip('*').click();
  assert.equal(await open(), true);
  await page.setViewportSize({ width: 1920, height: 900 });
  await settle(300);
  await shoot('provider-resize-wide-dark-1920.png', 1920, 560);
  assert.equal(await open(), true, 'the dialog stays open across the breakpoint');
  await assertOpenOnVisible('after widening');
  await unchangedPoll();
  assert.equal(await open(), true);
  await assertOpenOnVisible('after an unchanged poll');
  await page.keyboard.press('Escape');
  assert.equal(await open(), false);

  // Touch: a tap toggles.
  const touch = await browser.newContext({ viewport: { width: 375, height: 800 }, hasTouch: true, isMobile: true });
  const phone = await touch.newPage();
  phone.on('pageerror', error => errors.push(String(error)));
  await phone.route('**/api/host/resources', async route => {
    const response = await route.fetch();
    await route.fulfill({ response, json: { ...(await response.json()), ...override } });
  });
  await phone.goto(`${origin}/?workspace=ws_http_fixture#tasks`);
  await phone.waitForFunction(() => document.querySelectorAll('#provider-usage-chips .provider-usage-summary').length === 1);
  const summaryChip = phone.locator('#provider-usage-chips .provider-usage-summary').locator('visible=true').first();
  await summaryChip.scrollIntoViewIfNeeded();
  await summaryChip.tap();
  await phone.locator('#provider-usage-dialog').waitFor({ state: 'visible' });
  assert.equal(await summaryChip.getAttribute('aria-expanded'), 'true', 'a tap opens the dialog');
  await summaryChip.tap();
  await phone.locator('#provider-usage-dialog').waitFor({ state: 'hidden' });
  assert.equal(await summaryChip.getAttribute('aria-expanded'), 'false', 'a second tap closes it');
  await summaryChip.tap();
  await phone.locator('#provider-usage-dialog').waitFor({ state: 'visible' });
  await phone.tap('main, .main-col', { position: { x: 20, y: 400 } });
  await phone.locator('#provider-usage-dialog').waitFor({ state: 'hidden' });
  await touch.close();

  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ passed: true, wide, busyBar }, null, 2));
  console.log('Provider usage top bar chips and dialogs passed against the real server.');
} finally {
  await browser.close();
}
