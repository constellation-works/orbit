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
  if (pathname === '/api/hosts') {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ hosts: [{ name: 'fixture-host', local: true }] }));
    return;
  }
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
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, hasTouch: true, reducedMotion: 'reduce' });
  await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&window=24h#tasks`);
  await page.evaluate(() => document.fonts.ready);
  const cases = [
    { name: 'four-digit', events: '6837', routines: '33 routines' },
    { name: 'five-digit', events: '68.4k', routines: '333 routines' },
    { name: 'large', events: '987.7k', routines: '1000 routines' },
    { name: 'cleared', events: '6837', routines: '' },
  ];
  for (const width of [1440, 1280, 1024, 761, 760, 390, 375]) {
    await page.setViewportSize({ width, height: 900 });
    for (const fixture of cases) {
      await page.evaluate(fixture => {
        for (const node of document.querySelectorAll('.rail .tab > .rail-count')) {
          node.textContent = fixture.events;
        }
        const routines = document.getElementById('rail-count-routine-failures');
        routines.textContent = fixture.routines;
        routines.hidden = !fixture.routines;
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
        if (['diagnostics', 'analytics'].includes(row.tab) && clipped) {
          failures.push(`${context}: Health/Analytics label is truncated`);
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
  // Drive the shipped router through repeated destination and Health subtab
  // changes; each bar must keep reusing one indicator instead of gaining a node
  // per transition. The stand-in context answers every router callback.
  await page.setViewportSize({ width: 1440, height: 900 });
  const indicators = await page.evaluate(async () => {
    const router = await import('/static/js/router.js');
    const state = { diagSubtab: 'incidents' };
    router.initRouter(new Proxy({}, {
      get: (_, name) => name === 'getDiagSubtab' ? () => state.diagSubtab
        : name === 'setDiagSubtab' ? value => { state.diagSubtab = value; }
        : name === 'getLastRuns' ? () => []
        : () => {},
    }));
    const count = () => ({
      tabs: document.querySelectorAll('#tabs > .tab-indicator').length,
      subtabs: document.querySelectorAll('#diag-subtabs > .tab-indicator').length,
      tabIndicator: Boolean(document.getElementById('tab-indicator')),
      subtabIndicator: Boolean(document.getElementById('subtab-indicator')),
    });
    const counts = [];
    for (let round = 0; round < 3; round += 1) {
      for (const route of ['tasks', 'plugins', 'jobs/history', 'analytics/metrics', 'diagnostics/incidents/errors', 'tasks']) {
        router.setActiveTab(route, { refresh: false, updateHash: false });
        counts.push({ route, ...count() });
      }
    }
    return counts;
  });
  // The Health subtab bar gains its indicator on the first Health route.
  const firstHealth = indicators.findIndex(step => step.route.startsWith('diagnostics/'));
  for (const [index, step] of indicators.entries()) {
    if (step.tabs !== 1 || !step.tabIndicator) failures.push(`${step.route}: ${step.tabs} top-level indicators, #tab-indicator ${step.tabIndicator ? 'present' : 'missing'}`);
    const expected = index >= firstHealth ? 1 : 0;
    if (step.subtabs !== expected || step.subtabIndicator !== Boolean(expected)) {
      failures.push(`${step.route}: ${step.subtabs} Health subtab indicators, expected ${expected}`);
    }
  }
  // Use fresh deep links and the public router initializer so load-time reveal,
  // focus listeners and layout observers all exercise the shipped behavior.
  const visibility = [];
  let visit = 0;
  const settle = () => page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    await Promise.all(document.getAnimations().filter(animation =>
      animation.effect.getComputedTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})));
  });
  const openRoute = async (route, width = 1440) => {
    await page.setViewportSize({ width, height: 900 });
    // A hash-only goto keeps the old document and its listeners. Vary the
    // query so every direct-load check gets a new document at both widths.
    await page.goto(`http://127.0.0.1:${server.address().port}/?workspace=ws_fixture&visit=${++visit}#${route}`);
    await page.evaluate(async () => {
      const { initHostSwitcher } = await import('/static/js/host-switch.js');
      await initHostSwitcher({ select: () => {} });
      const { renderHostResources } = await import('/static/js/host-resources.js');
      renderHostResources({
        sample_age_seconds: 1, throttle: false, reason: 'Resources available',
        cpu: { percent: 42, severity: 'normal' }, memory: { percent: 55, severity: 'normal' },
        disk: { percent: 31, severity: 'normal' }, io: { percent: 2, severity: 'normal' },
      });
      // app.js mounts this selector after reading workspaces; keep its real
      // select geometry in this otherwise data-free shell fixture.
      const workspace = document.createElement('select');
      workspace.id = 'workspace-select';
      workspace.className = 'workspace-select';
      workspace.setAttribute('aria-label', 'Workspace');
      workspace.add(new Option('Fixture workspace', 'ws_fixture'));
      document.getElementById('rail-workspace').append(workspace);
      const routines = document.getElementById('rail-count-routine-failures');
      routines.hidden = false;
      routines.textContent = '33 routines';
      const router = await import('/static/js/router.js');
      const state = { diagSubtab: 'incidents', incidentsView: 'incidents', analyticsSubtab: 'metrics',
        reliabilityView: 'pipeline', agentsRole: 'orchestrators', configSubtab: 'workflow' };
      router.initRouter(new Proxy({}, {
        get: (_, name) => {
          const key = name.slice(3, 4).toLowerCase() + name.slice(4);
          if (name === 'getLastRuns') return () => [];
          if (name.startsWith('get')) return () => state[key];
          if (name.startsWith('set')) return value => { state[key] = value; };
          return () => {};
        },
      }));
      router.initTabs();
    });
    await settle();
  };
  const checkVisible = async (selector, context, focused = false) => {
    const layout = await page.locator(selector).evaluate(node => {
      const box = node.getBoundingClientRect();
      const rail = node.closest('.rail').getBoundingClientRect();
      const foot = document.querySelector('.rail-foot').getBoundingClientRect();
      const style = getComputedStyle(node);
      const ring = node.matches(':focus-visible')
        ? Math.max(0, parseFloat(style.outlineWidth) + parseFloat(style.outlineOffset)) : 0;
      const desktop = matchMedia('(min-width: 761px)').matches;
      const probes = [0.1, 0.5, 0.9].map(fraction => {
        const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height * fraction);
        return node === hit || node.contains(hit);
      });
      const strip = node.closest('.rail-subtabs').getBoundingClientRect();
      return { top: box.top, bottom: box.bottom, left: box.left, right: box.right,
        visibleTop: Math.max(rail.top, 0), visibleBottom: Math.min(foot.top, innerHeight), ring, probes,
        desktop, focused: document.activeElement === node,
        horizontal: box.left >= Math.max(strip.left, 0) - 0.5 && box.right <= Math.min(strip.right, innerWidth) + 0.5 };
    });
    visibility.push({ context, ...layout });
    if (focused && !layout.focused) failures.push(`${context}: keyboard focus left the expected item`);
    if (!layout.probes.every(Boolean)) failures.push(`${context}: item is covered or clipped`);
    if (layout.desktop && (layout.top - layout.ring < layout.visibleTop - 0.5
        || layout.bottom + layout.ring > layout.visibleBottom + 0.5)) {
      failures.push(`${context}: item or focus ring leaves the unobscured rail: ${JSON.stringify(layout)}`);
    }
    if (!layout.desktop && !layout.horizontal) failures.push(`${context}: mobile sub-view is clipped`);
  };
  for (const route of ['config/hosts', 'config/all-keys', 'analytics/agents']) {
    for (const width of [1440, 375]) {
      await openRoute(route, width);
      await checkVisible('.rail-subtabs:not(.dimmed) .subtab.active', `${route} / ${width}px direct load`);
      if (width === 375) {
        const parent = await page.locator('.rail .tab.active').boundingBox();
        if (parent.x < 0 || parent.x + parent.width > width) failures.push(`${route}: mobile destination is clipped`);
        if (await page.evaluate(() => document.scrollingElement.scrollWidth > innerWidth)) failures.push(`${route}: mobile page overflows`);
      }
      await page.screenshot({ path: path.join(evidence, `${route.replace('/', '-')}-dark-${width}.png`) });
    }
  }
  await openRoute('config/workflow');
  const settings = page.locator('#config-subtabs .subtab');
  const names = await settings.evaluateAll(nodes => nodes.map(node => node.dataset.subtab));
  await settings.first().focus();
  for (const [index, name] of names.entries()) {
    if (index) await page.keyboard.press('Tab');
    await checkVisible(`#config-subtabs [data-subtab="${name}"]`, `Settings / Tab / ${name}`, true);
  }
  await page.screenshot({ path: path.join(evidence, 'config-all-keys-focus-dark-1440.png') });
  for (const name of names.slice(0, -1).reverse()) {
    await page.keyboard.press('Shift+Tab');
    await checkVisible(`#config-subtabs [data-subtab="${name}"]`, `Settings / Shift+Tab / ${name}`, true);
  }
  // A wrapped host verdict changes the sticky obstruction after routing.
  await openRoute('config/hosts');
  await page.locator('.host-block-verdict-text').evaluate(node => { node.textContent = 'Waiting for a fresh host resource sample'; });
  await settle();
  await checkVisible('#config-subtabs .subtab.active', 'Settings / Hosts / resized host block');
  await settings.last().focus();
  await page.locator('.host-block-verdict-text').evaluate(node => { node.textContent += ' before checking admission'; });
  await settle();
  await checkVisible('#config-subtabs [data-subtab="all-keys"]', 'Settings / focused All keys / resized host block', true);
  // The phone shell formerly consumed 225px before the main pane (61% of
  // 375x667 left for content). Exercise the rendered UI, including the fleet
  // label that caused an extra wrapped row, rather than pinning CSS text.
  const phoneChrome = [];
  const keyboard = [];
  const touch = [];
  for (const height of [667, 900]) {
    await openRoute('tasks', 375);
    await page.setViewportSize({ width: 375, height });
    await page.evaluate(async () => {
      const { renderFleetIndicator } = await import('/static/js/operations-fleet.js');
      renderFleetIndicator({ items: [{ workspace_id: 'ws_fixture', phase: 'winding_down' }] });
      const { renderProviderUsage } = await import('/static/js/provider-usage.js');
      renderProviderUsage({ provider_limits: [{ provider: 'codex', window: 'five_hour',
        used_percent: 40, threshold: 90, gating: true, gated: false, stale: false }] });
      // Notifications are optional in production; include the widest usual bar.
      document.getElementById('notify-btn').hidden = false;
    });
    await settle();
    const geometry = await page.evaluate(() => {
      const rect = selector => {
        const { x, y, width, height } = document.querySelector(selector).getBoundingClientRect();
        return { x, y, width, height };
      };
      return { viewport: { width: innerWidth, height: innerHeight },
        pane: rect('#tasks-main'), topbar: rect('.topbar'), rail: rect('.rail'),
        log: rect('#log-statusbar'), overflow: document.scrollingElement.scrollWidth > innerWidth };
    });
    phoneChrome.push(geometry);
    await page.screenshot({ path: path.join(evidence, `shell-dark-375-${height}.png`) });
    if (geometry.pane.height / height < 0.70) failures.push(`${height}px: phone content takes less than 70%: ${JSON.stringify(geometry)}`);
    if (geometry.overflow) failures.push(`${height}px: phone shell overflows horizontally`);
    if (height !== 667) continue;

    // A real touch swipe exposes the actions beyond the fleet pill. Wait
    // for native scroll completion before switching back to keyboard input.
    if (await page.locator('.topbar').evaluate(node => node.scrollWidth > node.clientWidth)) {
      await page.locator('#global-drain-state').focus();
      await settle();
      const touchCdp = await page.context().newCDPSession(page);
      // Keep the protocol domain separate from its method: the fixture hook
      // guard otherwise reads Input as an HTML tag and the method as a class.
      const touchCommand = ['Input', 'dispatchTouchEvent'].join('.');
      const beforeSwipe = await page.locator('.topbar').evaluate(node => node.scrollLeft);
      await page.locator('.topbar').evaluate(node => {
        globalThis.phoneChromeSwipe = new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error('Top bar touch scroll did not finish')), 5000);
          node.addEventListener('scrollend', () => { clearTimeout(timeout); resolve(node.scrollLeft); }, { once: true });
        });
      });
      await touchCdp.send(touchCommand, { type: 'touchStart', touchPoints: [{ x: 300, y: 25 }] });
      for (const x of [250, 200, 150, 100, 50]) {
        await touchCdp.send(touchCommand, { type: 'touchMove', touchPoints: [{ x, y: 25 }] });
      }
      await touchCdp.send(touchCommand, { type: 'touchEnd', touchPoints: [] });
      const afterSwipe = await page.evaluate(() => globalThis.phoneChromeSwipe);
      touch.push({ beforeSwipe, afterSwipe });
      if (afterSwipe <= beforeSwipe) failures.push('Touch swipe did not reveal the remaining top bar actions');
      await touchCdp.detach();
      await page.screenshot({ path: path.join(evidence, 'shell-swiped-dark-375.png') });
    }

    const targets = ['#host-select', '#workspace-select', '#global-drain-state', '#notify-btn', '#refresh-btn',
      '.provider-usage-summary', '#log-statusbar-open',
      ...await page.locator('.rail .tab').evaluateAll(nodes => nodes.map(node => `.tab[data-tab="${node.dataset.tab}"]`))];
    const seen = new Set();
    await page.locator('#skip-link').focus();
    // Use the actual Tab sequence. Focus must scroll offscreen bar actions and
    // rail destinations into view without moving the document horizontally.
    for (let step = 0; step < 160 && seen.size < targets.length; step += 1) {
      await page.keyboard.press('Tab');
      await settle();
      for (const selector of targets) {
        if (seen.has(selector) || !await page.locator(selector).evaluate(node => node === document.activeElement)) continue;
        const probe = await page.locator(selector).evaluate(node => {
          const box = node.getBoundingClientRect();
          const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
          return { left: box.left, right: box.right, top: box.top, bottom: box.bottom,
            visible: box.left >= 0 && box.right <= innerWidth && box.top >= 0 && box.bottom <= innerHeight
              && (hit === node || node.contains(hit)), focused: node.matches(':focus-visible') };
        });
        keyboard.push({ selector, ...probe });
        if (!probe.visible || !probe.focused) failures.push(`Tab focus cannot reveal ${selector}: ${JSON.stringify(probe)}`);
        seen.add(selector);
        await page.screenshot({ path: path.join(evidence, `keyboard-${seen.size}-dark-375.png`) });
      }
    }
    for (const selector of targets) if (!seen.has(selector)) failures.push(`Tab sequence never reached ${selector}`);

    // Tap every top-level destination through the real router; screenshots
    // cover the shared shell on each view, including horizontally hidden tabs.
    const destinations = await page.locator('.rail .tab').evaluateAll(nodes => nodes.map(node => node.dataset.tab));
    for (const destination of destinations) {
      const tab = page.locator(`.rail .tab[data-tab="${destination}"]`);
      await tab.scrollIntoViewIfNeeded();
      await tab.tap();
      await settle();
      if (!await tab.evaluate(node => node.classList.contains('active'))) failures.push(`Tap did not activate ${destination}`);
      await page.screenshot({ path: path.join(evidence, `shell-${destination}-dark-375.png`) });
    }
  }
  fs.writeFileSync(path.join(evidence, 'measurements.json'), `${JSON.stringify({ browser: browser.version(), measurements, indicators, visibility, phoneChrome, keyboard, touch, failures }, null, 2)}\n`);
  assert.deepEqual(failures, [], 'Rail labels and counts must remain readable, navigation must reuse one indicator per bar, and active/focused views and focus rings must stay unobscured');
  console.log(`PASS: rail label/count geometry at seven viewport widths; one indicator per bar across ${indicators.length} route changes; ${visibility.length} active/focused visibility checks including desktop sticky-footer clearance and mobile deep links; phone content >=70% at both heights; ${keyboard.length} keyboard controls and all touch destinations; screenshots: ${evidence}`);
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
