import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

import { check, rule } from "../ui/contrast.mjs";

const target = {
  id: "analytics-metrics|dark|1440|default",
  view: "analytics-metrics",
  width: 1440,
  theme: "dark",
  state: "default",
};

// Browser-boundary coverage: the in-page measurement decides which text is an
// inactive control, and only real Chromium resolves :disabled (including
// fieldset-disabled controls and the first-legend exception). When no browser
// kit is available the cases skip rather than pass silently.
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

// #999 on white is about 2.85:1, below the 4.5:1 body-text threshold.
const LOW = "color:#999;background:#fff";

test("contrast exempts a native disabled control and its text descendants, but still fails enabled controls and informational text", async (t) => {
  const result = await checkInBrowser(
    t,
    `<button id="tab" disabled style="${LOW}">low<span id="count">0</span></button>` +
      `<button id="live" style="${LOW}">Live</button>` +
      `<button id="aria" aria-disabled="true" style="${LOW}">Aria</button>` +
      `<p id="note" style="${LOW}">Informational note</p>`,
  );
  if (result === null) return;
  assert.equal(result.rule, rule.id);
  assert.equal(result.status, "fail");
  assert.deepEqual(result.measurements.failures.map((failure) => failure.selector).sort(), ["#aria", "#live", "#note"]);
  assert.equal(result.measurements.disabled_exempt, 2, "the disabled button's own text and its span descendant are exempt");
});

test("contrast passes when every low-contrast text sits inside a native disabled control", async (t) => {
  const result = await checkInBrowser(
    t,
    `<button disabled style="${LOW}">low<span>0</span></button>`,
  );
  if (result === null) return;
  assert.equal(result.status, "pass", "exempt text is still matched, so the verdict is not skipped");
  assert.equal(result.measurements.disabled_exempt, 2);
});

test("contrast treats controls under a disabled fieldset as disabled, except inside its first legend", async (t) => {
  const result = await checkInBrowser(
    t,
    `<fieldset id="off" disabled style="background:#fff;border:0">` +
      `<legend><button id="in-legend" style="${LOW}">Legend</button></legend>` +
      `<button id="inside" style="${LOW}">Save</button>` +
      `<p id="hint" style="${LOW}">Hint</p>` +
      `</fieldset>`,
  );
  if (result === null) return;
  assert.equal(result.status, "fail");
  assert.deepEqual(result.measurements.failures.map((failure) => failure.selector).sort(), ["#hint", "#in-legend"]);
  assert.equal(result.measurements.disabled_exempt, 1, "only the button inside the fieldset body is exempt");
});
