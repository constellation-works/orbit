import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import { check, MIN_FONT_PX } from "../ui/undersized-text.mjs";

const target = {
  id: "dashboard|dark|1440|default",
  view: "dashboard",
  width: 1440,
  theme: "dark",
  state: "default",
};

function context(measured) {
  return {
    page: { evaluate: async () => measured },
    target,
  };
}

test("undersized-text fails when content text drops below 12px", async () => {
  const result = await check(
    context({
      matched: 3,
      failures: [
        {
          selector: "p.meta",
          font_size_px: 10,
          overline: false,
          text: "Meta copy readers must read",
        },
      ],
    }),
  );
  assert.equal(result.status, "fail");
  assert.equal(MIN_FONT_PX, 11);
});

test("undersized-text passes when content meets the floor", async () => {
  const result = await check(context({ matched: 3, failures: [] }));
  assert.equal(result.status, "pass");
});

test("undersized-text skips when no content text matched", async () => {
  const result = await check(context({ matched: 0, failures: [] }));
  assert.equal(result.status, "skipped");
});

// Browser boundary: the real measurement and overline predicate run in
// Chromium. Skipped where the pinned browser kit is not installed.
async function launchBrowser() {
  const modulePath =
    process.env.PLAYWRIGHT_MODULE ||
    path.join(process.env.HOME ?? "", ".local/chromium-deps/kit/node_modules/playwright/index.mjs");
  try {
    const mod = await import(modulePath);
    return await (mod.default ?? mod).chromium.launch();
  } catch {
    return null;
  }
}

async function measureHtml(browser, html) {
  const page = await browser.newPage();
  try {
    await page.setContent(html);
    return await check({ page, target });
  } finally {
    await page.close();
  }
}

test("undersized-text separates ordinary small prose from an uppercase tracked overline", async (t) => {
  const browser = await launchBrowser();
  if (!browser) {
    t.skip("browser kit not installed");
    return;
  }
  try {
    const prose = await measureHtml(
      browser,
      '<p style="font-size:8px;letter-spacing:1px">This paragraph is too small to read</p>',
    );
    assert.equal(prose.status, "fail", "tracked mixed-case prose is not an overline");

    const shouting = await measureHtml(
      browser,
      '<p style="font-size:8px;text-transform:uppercase">This paragraph is too small to read</p>',
    );
    assert.equal(shouting.status, "fail", "untracked uppercase prose is not an overline");

    const overline = await measureHtml(
      browser,
      '<p style="font-size:8px;text-transform:uppercase;letter-spacing:1px">Column head label</p>',
    );
    assert.equal(overline.status, "skipped", "uppercase tracked overline stays exempt");

    const mixed = await measureHtml(
      browser,
      '<p style="font-size:8px;text-transform:uppercase;letter-spacing:1px">Column head label</p>' +
        '<p style="font-size:14px">Readable body copy here</p>',
    );
    assert.equal(mixed.status, "pass", "normal-size prose passes beside an overline");
  } finally {
    await browser.close();
  }
});
