// Regression check against a built local site, using the host browser kit.
// node website/scripts/check-crate-diagram.mjs <playwright-module> <site-url> <evidence-dir>
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

const [playwrightModule, siteURL, evidenceDir] = process.argv.slice(2);
if (!playwrightModule || !siteURL || !evidenceDir) {
  throw new Error('Usage: check-crate-diagram.mjs <playwright-module> <site-url> <evidence-dir>');
}
const { chromium } = await import(path.resolve(playwrightModule));
const markdown = await readFile(new URL('../src/content/docs/contributing/crate-layout.md', import.meta.url), 'utf8');
const source = markdown.match(/```mermaid\n([\s\S]*?)\n```/)[1];
const origin = new URL(siteURL).origin;
const evidence = { layouts: [], externalRequests: [], errors: [] };
await mkdir(evidenceDir, { recursive: true });
const browser = await chromium.launch();
try {
  for (const theme of ['light', 'dark']) {
    for (const width of [1440, 375]) {
      const context = await browser.newContext({ viewport: { width, height: 900 } });
      await context.addInitScript((theme) => localStorage.setItem('orbit-theme-choice', theme), theme);
      await context.route('**/*', async (route) => {
        const url = new URL(route.request().url());
        if (url.origin !== origin) {
          evidence.externalRequests.push(url.href);
          await route.abort();
        } else await route.continue();
      });
      const page = await context.newPage();
      page.on('pageerror', (error) => evidence.errors.push(error.message));
      const response = await page.goto(new URL('/contributing/crate-layout/', siteURL).href);
      assert.equal(response.status(), 200, 'Built crate layout must load');
      await page.evaluate(() => document.fonts.ready);
      const graph = page.getByRole('img', { name: 'Dependency diagram', exact: true });
      assert.equal(await graph.count(), 1, 'Mermaid fence must render a static, accessible graph');
      assert.ok(await graph.isVisible(), 'Graph must be visible');
      const crateNames = await page.locator('table tbody tr td:first-child code').allTextContents();
      const labels = await graph.locator('text').allTextContents();
      assert.deepEqual([...labels].sort(), [...crateNames].sort(), 'Graph must label every crate from the responsibility table');
      const expectedEdges = [...source.matchAll(/^\s*(\w+)(?:\["[^"]*"\])?\s*-->\s*(\w+)/gm)]
        .map(([, from, to]) => [from, to]);
      const edges = await graph.locator('[marker-end]').evaluateAll((elements) =>
        elements.map((el) => [el.dataset.from, el.dataset.to]));
      assert.deepEqual(edges, expectedEdges, 'Graph must draw every dependency arrow in the authored direction');
      const fallback = page.locator('.orbit-diagram details');
      await fallback.locator('summary').click();
      assert.equal(await fallback.locator('pre code').textContent(), source, 'Text alternative must preserve the complete Mermaid source');
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth),
        'Expanded diagram source must not cause page overflow');
      await fallback.locator('summary').click();
      const viewport = page.getByRole('region', { name: 'Scrollable diagram', exact: true });
      await viewport.focus();
      await page.keyboard.press('ArrowRight');
      await page.waitForFunction(() => document.querySelector('.orbit-diagram-viewport').scrollLeft > 0);
      const layout = await viewport.evaluate((el) => ({
        clientWidth: el.clientWidth, scrollWidth: el.scrollWidth, keyboardScroll: el.scrollLeft,
        pageWidth: document.documentElement.clientWidth, pageScrollWidth: document.documentElement.scrollWidth,
        labelSize: getComputedStyle(el.querySelector('text')).fontSize,
        labelColor: getComputedStyle(el.querySelector('text')).fill,
      }));
      assert.ok(layout.pageScrollWidth <= layout.pageWidth, 'Diagram must not cause page overflow');
      assert.ok(layout.keyboardScroll > 0, 'Keyboard users must be able to explore the entire graph');
      assert.ok(parseFloat(layout.labelSize) >= 12, 'Graph labels must stay readable instead of shrinking to the viewport');
      await viewport.evaluate((el) => { el.scrollLeft = 0; el.scrollTop = 0; });
      await page.locator('#dependency-direction').evaluate((el) => window.scrollTo(0, el.getBoundingClientRect().top + scrollY - 100));
      await page.screenshot({ path: path.join(evidenceDir, `crate-layout-${theme}-${width}-after.png`) });
      if (width === 1440) {
        await page.locator('starlight-theme-select button:visible').click();
        const switchedColor = await graph.locator('text').first().evaluate((el) => getComputedStyle(el).fill);
        assert.notEqual(switchedColor, layout.labelColor, 'Existing graph must follow the live theme toggle');
      }
      evidence.layouts.push({ theme, width, labels: labels.length, arrows: await graph.locator('[marker-end]').count(), ...layout });
      await context.close();
    }
  }
  const noScript = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 375, height: 900 } });
  const staticPage = await noScript.newPage();
  await staticPage.goto(new URL('/contributing/crate-layout/', siteURL).href);
  assert.ok(await staticPage.getByRole('img', { name: 'Dependency diagram', exact: true }).isVisible(),
    'Graph must already be rendered when JavaScript is disabled');
  await staticPage.locator('.orbit-diagram summary').click();
  assert.equal(await staticPage.locator('.orbit-diagram pre code').textContent(), source,
    'Complete text alternative must remain available without JavaScript');
  evidence.noJavaScript = 'Static graph and complete source alternative passed at 375px';
  await noScript.close();
  assert.equal(evidence.externalRequests.length, 0, 'Diagram must work offline with no third-party requests');
  assert.equal(evidence.errors.length, 0, 'Built page must not raise browser errors');
  assert.notEqual(evidence.layouts[0].labelColor, evidence.layouts[2].labelColor, 'Graph text must follow the light and dark themes');
  console.log(JSON.stringify(evidence));
} finally {
  await writeFile(path.join(evidenceDir, 'crate-diagram-browser.json'), `${JSON.stringify(evidence, null, 2)}\n`);
  await browser.close();
}
