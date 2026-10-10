// Verify the built page and Markdown transform through a real browser.
// node website/scripts/check-changelog.mjs <playwright-module> <site-url> <evidence-dir>
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { markdownToHtml } from 'satteri';
import { changelogLinks, changelogReleases } from '../plugins/changelog.mjs';

const [playwrightModule, siteURL, evidenceDir] = process.argv.slice(2);
if (!playwrightModule || !siteURL || !evidenceDir) {
  throw new Error('Usage: check-changelog.mjs <playwright-module> <site-url> <evidence-dir>');
}
const { chromium } = await import(path.resolve(playwrightModule));
await mkdir(evidenceDir, { recursive: true });
const source = await readFile(new URL('../../CHANGELOG.md', import.meta.url), 'utf8');
const versions = [...source.matchAll(/^## (\d+\.\d+\.\d+)/gm)].map((match) => match[1]);
const references = [...source.matchAll(/\[([A-Z][A-Z0-9]{1,15}-\d{1,8})\]/g)].map((match) => match[1]);
const dates = JSON.parse(await readFile(new URL('../src/data/release-dates.json', import.meta.url), 'utf8'));
const pageURL = new URL('/changelog/', siteURL).href;
const browser = await chromium.launch();
const evidence = { viewports: [], releases: versions.length, references: references.length };
try {
  const originalPage = await browser.newPage();
  const original = markdownToHtml(source, { features: { smartPunctuation: true } });
  await originalPage.setContent(original.html);
  const authoredText = await originalPage.evaluate(() => {
    for (const heading of document.querySelectorAll('h2')) {
      heading.textContent = heading.textContent.match(/^\d+\.\d+\.\d+/)?.[0] ?? heading.textContent;
    }
    return document.body.textContent.replace(/\[[A-Z][A-Z0-9]{1,15}-\d{1,8}\]/g, 'Pull request').replace(/\s+/g, ' ').trim();
  });
  await originalPage.close();
  for (const width of [1440, 375]) {
    for (const colorScheme of ['dark', 'light']) {
      const page = await browser.newPage({ viewport: { width, height: 900 }, colorScheme });
      // Orbit defaults to dark regardless of the OS; its saved choice selects light.
      await page.addInitScript((theme) => localStorage.setItem('orbit-theme-choice', theme), colorScheme);
      await page.goto(pageURL);
      await page.evaluate(async () => {
        await document.fonts.load('16px "Geist Variable"');
        await document.fonts.load('16px "Geist Mono Variable"');
        await document.fonts.ready;
      });
      assert.equal(await page.locator('html').getAttribute('data-theme'), colorScheme, 'Capture the chosen site theme');
      const result = await page.evaluate(() => {
        const root = document.querySelector('.orbit-changelog');
        const originalContent = root.cloneNode(true);
        for (const heading of originalContent.querySelectorAll('h2')) {
          heading.textContent = heading.textContent.match(/^\d+\.\d+\.\d+/)?.[0] ?? heading.textContent;
        }
        return {
          height: document.documentElement.scrollHeight,
          width: document.documentElement.scrollWidth,
          text: root.textContent,
          authoredText: originalContent.textContent.replace(/\s+/g, ' ').trim(),
          releases: [...root.querySelectorAll('.orbit-release')].map((release) => ({
            id: release.querySelector('h2').id,
            date: release.querySelector('time')?.dateTime,
            open: release.open,
          })),
          links: [...root.querySelectorAll('a[href*="/pulls?q="]')].map((link) => ({
            href: link.href, text: link.textContent,
          })),
        };
      });
      assert.equal(result.width, width, 'The changelog must not overflow horizontally');
      assert.equal(result.authoredText, authoredText, 'Folding releases must preserve all authored notes');
      if (width === 1440) assert.ok(result.height < 15000, `Page height is ${result.height}px`);
      assert.equal(result.releases.length, versions.length, 'Every release remains available');
      assert.ok(result.releases.some((release) => !release.open), 'Older notes must be folded');
      assert.ok(result.releases[0].open, 'The newest release must be expanded');
      assert.deepEqual(result.releases.map((release) => release.id), versions.map((version) => version.replaceAll('.', '')));
      assert.deepEqual(result.releases.map((release) => release.date), versions.map((version) => {
        const heading = source.split('\n').find((line) => line.startsWith(`## ${version}`));
        return heading.match(/\d{4}-\d{2}-\d{2}/)?.[0] ?? dates[version];
      }));
      assert.equal(result.links.length, references.length);
      assert.deepEqual(result.links.map(({ href }) => new URL(href).searchParams.get('q')),
        references.map((id) => `is:pr is:merged "${id}"`));
      assert.ok(result.links.every(({ text }) => !/[\[\]]|[A-Z]+-\d+/.test(text)), 'Public labels must not display internal IDs');
      assert.ok(!/\[[A-Z]+-\d+\]/.test(result.text), 'Bracketed references must become links');
      assert.equal(await page.locator('.orbit-version').textContent(), `v${versions[0]}`, 'The global release badge must remain version-only');
      evidence.viewports.push({ width, colorScheme, height: result.height });
      await page.screenshot({ path: path.join(evidenceDir, `changelog-${width}-${colorScheme}.png`), fullPage: true });
      await page.screenshot({ path: path.join(evidenceDir, `changelog-${width}-${colorScheme}-viewport.png`) });

      const older = page.locator('.orbit-release:not([open])').first();
      const olderId = await older.locator('h2').getAttribute('id');
      const summary = older.locator('summary');
      await summary.focus();
      await page.keyboard.press('Enter');
      assert.equal(await page.locator(`.orbit-release:has([id="${olderId}"])`).evaluate((el) => el.open), true, 'Keyboard activation must expand notes');
      await page.keyboard.press('Space');
      assert.equal(await page.locator(`.orbit-release:has([id="${olderId}"])`).evaluate((el) => el.open), false, 'Keyboard activation must fold notes');

      // Every existing version anchor must reveal its notes after navigation.
      for (const version of versions) {
        const id = version.replaceAll('.', '');
        await page.evaluate((id) => { location.hash = id; }, id);
        await page.waitForFunction((id) => document.getElementById(id)?.closest('details')?.open, id);
      }
      const oldestId = versions.at(-1).replaceAll('.', '');
      await page.goto(`${pageURL}#${oldestId}`);
      await page.waitForFunction((id) => document.getElementById(id)?.closest('details')?.open, oldestId);
      await page.goto(pageURL);
      const subsectionId = await page.locator('.orbit-release:not([open]) h3').last().getAttribute('id');
      await page.goto(`${pageURL}#${subsectionId}`);
      await page.waitForFunction((id) => document.getElementById(id)?.closest('details')?.open, subsectionId);
      assert.ok(await page.locator(`[id="${subsectionId}"]`).isVisible(), 'Direct subsection anchors must reveal hidden bodies');
      await page.close();
    }
  }

  const noJS = await browser.newPage({ javaScriptEnabled: false });
  await noJS.goto(pageURL);
  const disclosure = noJS.locator('.orbit-release:not([open])').first();
  const disclosureId = await disclosure.locator('h2').getAttribute('id');
  await disclosure.locator('summary').click();
  assert.equal(await noJS.locator(`.orbit-release:has([id="${disclosureId}"])`).evaluate((el) => el.open), true, 'Disclosure controls must work without JavaScript');
  await noJS.close();

  const options = {
    fileURL: new URL('../../CHANGELOG.md', import.meta.url),
    hastPlugins: [changelogLinks, changelogReleases],
  };
  const fixture = '# Changelog\n\n## 9.8.7 — 2026-10-07\n\n### Highlights\n\n- A change ([TASK-1234], [TASK-5678]).\n\n`[TASK-1111]` and [existing link](https://example.com).';
  const rendered = await markdownToHtml(fixture, options);
  const fixturePage = await browser.newPage();
  await fixturePage.setContent(rendered.html);
  assert.equal(await fixturePage.locator('h2').getAttribute('id'), '987', 'Future dated headings must keep their version anchors');
  assert.equal(await fixturePage.locator('time').getAttribute('datetime'), '2026-10-07');
  assert.equal(await fixturePage.locator('a[href*="/pulls?q="]').count(), 2, 'Multiple references in one text node must become separate links');
  assert.equal(await fixturePage.locator('code').textContent(), '[TASK-1111]', 'Code examples must remain literal');
  assert.equal(await fixturePage.locator('a[href="https://example.com"]').count(), 1, 'Existing links must remain intact');
  const untouched = await markdownToHtml(fixture, { ...options, fileURL: new URL('../src/content/docs/example.md', import.meta.url) });
  await fixturePage.setContent(untouched.html);
  assert.equal(await fixturePage.locator('details, time, a[href*="/pulls?q="]').count(), 0, 'Other Markdown pages must remain unaffected');
  await assert.rejects(async () => markdownToHtml('## 9.8.6\n\nNo release date.', options), /needs an ISO date/);
  await assert.rejects(async () => markdownToHtml('## 9.8.6 — 2026-02-30', options), /needs an ISO date/);
  await assert.rejects(async () => markdownToHtml('## 9.8.6 — 2026-13-01', options), /needs an ISO date/);
  await fixturePage.close();
  evidence.checks = 'Loaded web fonts, explicit site themes, preserved notes, links, dates, stable version/subsection anchors, keyboard/no-JS disclosures, future dated headings, transform scoping and release badge passed.';
  await writeFile(path.join(evidenceDir, 'changelog-browser.json'), JSON.stringify(evidence, null, 2) + '\n');
  console.log(JSON.stringify(evidence));
} finally {
  await browser.close();
}
