// Follow the Project overview cards on a built local site and verify their pages.
// node website/scripts/check-project.mjs <playwright-module> <site-url> <evidence-dir>
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';

const [playwrightModule, siteURL, evidenceDir] = process.argv.slice(2);
if (!playwrightModule || !siteURL || !evidenceDir) {
  throw new Error('Usage: check-project.mjs <playwright-module> <site-url> <evidence-dir>');
}

const { chromium } = await import(path.resolve(playwrightModule));
await mkdir(evidenceDir, { recursive: true });

const baseURL = new URL(siteURL);
const projectURL = new URL('/project/', baseURL);
const cards = [
  { text: 'Contributing', route: '/contributing/', heading: 'Contributing' },
  { text: 'Privacy', route: '/privacy/', heading: 'Privacy Policy' },
];
const evidence = { projectURL: projectURL.href, cards: [] };
const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  const projectResponse = await page.goto(projectURL.href);
  assert.ok(projectResponse && projectResponse.status() < 400, 'Built Project page must load');
  assert.equal(await page.locator('main h1').textContent(), 'Project');

  for (const card of cards) {
    await page.goto(projectURL.href);
    const expectedURL = new URL(card.route, baseURL).href;
    let navigationResponse;
    const captureNavigationResponse = (response) => {
      if (response.url() === expectedURL && response.request().isNavigationRequest()) {
        navigationResponse = response;
      }
    };
    page.on('response', captureNavigationResponse);
    const cardLink = page.locator('main a.orbit-card').filter({ hasText: card.text });
    assert.equal(await cardLink.count(), 1, `${card.text} card must be present once`);
    await cardLink.click();
    page.off('response', captureNavigationResponse);
    const destination = new URL(page.url());
    const heading = (await page.locator('main h1').textContent())?.trim() ?? null;
    const result = {
      card: card.heading,
      status: navigationResponse?.status() ?? null,
      url: destination.href,
      heading,
    };
    evidence.cards.push(result);
    assert.equal(destination.origin, baseURL.origin, `${card.heading} must stay on the built site`);
    assert.equal(destination.pathname, card.route, `${card.heading} must use its root route`);
    assert.ok(navigationResponse && navigationResponse.status() < 400,
      `${card.heading} destination must load`);
    assert.equal(heading, card.heading, `${card.heading} destination content must be rendered`);
  }

  await writeFile(path.join(evidenceDir, 'project-links-browser.json'), `${JSON.stringify(evidence, null, 2)}\n`);
  console.log(JSON.stringify(evidence));
} finally {
  await browser.close();
}
