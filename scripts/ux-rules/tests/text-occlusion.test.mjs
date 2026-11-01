import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

import { check, rule } from "../ui/text-occlusion.mjs";

const target = {
  id: "analytics-implementers|dark|1440|default",
  view: "analytics-implementers",
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

test("text-occlusion fails when a value paints over its label", async () => {
  const result = await check(
    context({
      matched: 2,
      failures: [
        {
          selector: "dt.cost",
          covered_by: "dd.cost-value",
          point_x_px: 120,
          point_y_px: 300,
          text: "Cost coverage",
        },
      ],
    }),
  );
  assert.equal(result.status, "fail");
  assert.equal(result.rule, rule.id);
  assert.match(result.fingerprint, /ui\.text-occlusion\|analytics-implementers/);
});

test("text-occlusion passes when nothing is covered", async () => {
  const result = await check(context({ matched: 4, failures: [] }));
  assert.equal(result.status, "pass");
});

test("text-occlusion skips when no text matched", async () => {
  const result = await check(context({ matched: 0, failures: [] }));
  assert.equal(result.status, "skipped");
});

// Browser-boundary coverage: the stubbed contexts above never run the
// in-page measurement, so they cannot see paint-vs-hit-test mistakes.
// These cases run check() against real Chromium geometry (the browser kit
// named by PLAYWRIGHT_MODULE, sourced from the kit's env.sh). When no
// browser kit is available the cases skip rather than pass silently.
async function checkInBrowser(t, bodyHtml) {
  const modulePath =
    process.env.PLAYWRIGHT_MODULE ||
    path.join(process.env.HOME ?? "", ".local/chromium-deps/kit/node_modules/playwright/index.mjs");
  let chromium;
  try {
    const loaded = await import(pathToFileURL(modulePath).href);
    chromium = (loaded.default ?? loaded).chromium;
    if (!chromium) throw new Error("playwright module does not export chromium");
  } catch (error) {
    t.skip(`browser kit unavailable at ${modulePath}: ${error.message}`);
    return null;
  }
  let browser;
  try {
    browser = await chromium.launch({ headless: true });
  } catch (error) {
    t.skip(`Chromium launch failed (source the browser kit env.sh): ${error.message}`);
    return null;
  }
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 400 } });
    await page.setContent(
      `<!doctype html><html><head><style>body{font:16px/1.5 Arial,sans-serif;margin:0}</style></head><body>${bodyHtml}</body></html>`,
    );
    await page.evaluate(() => document.fonts?.ready);
    return await check({ page, target });
  } finally {
    await browser.close().catch(() => {});
  }
}

test("text-occlusion passes when an overlapping element's glyphs paint elsewhere", async (t) => {
  const result = await checkInBrowser(
    t,
    `<div style="position:relative;width:600px;height:100px">` +
      `<span id="label" style="position:absolute;top:0;left:0">Readable label</span>` +
      `<div id="value" style="position:absolute;top:0;left:0;width:600px;text-align:right">Value on the far right</div>` +
      `</div>`,
  );
  if (result === null) return;
  assert.equal(result.status, "pass");
});

test("text-occlusion fails under an opaque pointer-events:none overlay", async (t) => {
  const result = await checkInBrowser(
    t,
    `<div style="position:relative;width:600px;height:100px">` +
      `<span id="label" style="position:absolute;top:0;left:0">Readable label</span>` +
      `<div id="overlay" style="position:absolute;top:0;left:0;width:200px;height:30px;background:#fff;pointer-events:none"></div>` +
      `</div>`,
  );
  if (result === null) return;
  assert.equal(result.status, "fail");
  assert.equal(result.measurements.failures[0].selector, "#label");
  assert.equal(result.measurements.failures[0].covered_by, "#overlay");
});

test("text-occlusion passes when a sticky bar scrolls clear of the text", async (t) => {
  const result = await checkInBrowser(
    t,
    `<div id="scroller" style="height:180px;overflow:auto">` +
      `<div style="height:150px">Top matter for the column.</div>` +
      `<p id="label">Description field text</p>` +
      `<div style="height:240px">Later matter so this column can scroll.</div>` +
      `<div id="bar" style="position:sticky;bottom:0;height:48px;background:#fff">Approve</div>` +
      `</div>`,
  );
  if (result === null) return;
  assert.equal(result.status, "pass");
});

test("text-occlusion fails when a fixed overlay cannot be scrolled clear", async (t) => {
  const result = await checkInBrowser(
    t,
    `<p id="label">Permanent label</p>` +
      `<div id="overlay" style="position:fixed;top:0;left:0;width:240px;height:36px;background:#fff"></div>`,
  );
  if (result === null) return;
  assert.equal(result.status, "fail");
  assert.equal(result.measurements.failures[0].selector, "#label");
  assert.equal(result.measurements.failures[0].covered_by, "#overlay");
});

test("text-occlusion still fails when another text run paints over the label", async (t) => {
  const result = await checkInBrowser(
    t,
    `<div style="position:relative;width:600px;height:100px">` +
      `<span id="label" style="position:absolute;top:0;left:0">Readable label</span>` +
      `<div id="value" style="position:absolute;top:0;left:0">Value over label</div>` +
      `</div>`,
  );
  if (result === null) return;
  assert.equal(result.status, "fail");
  assert.equal(result.measurements.failures[0].selector, "#label");
  assert.equal(result.measurements.failures[0].covered_by, "#value");
});
