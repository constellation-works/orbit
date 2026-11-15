// Real-browser checks for ui.panel-body-padding. Each case loads the shipped
// dashboard stylesheet, so the clipped live region is the production rule
// (components.css `.panel-placeholder.quiet`), not a copy of it. Needs the
// browser kit: source its env.sh so PLAYWRIGHT_MODULE is set.
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { check } from "../ui/panel-body-padding.mjs";

const COMPONENTS_CSS = fileURLToPath(
  new URL("../../../crates/orbit-web/assets/dashboard/css/components.css", import.meta.url),
);
const ENABLED = Boolean(process.env.PLAYWRIGHT_MODULE);
const target = {
  id: "panel-body-fixture|dark|1166|default",
  view: "panel-body-fixture",
  width: 1166,
  theme: "dark",
  state: "default",
};

// Same markup common.js panelMessage() writes for a settled panel's live note.
const QUIET_NOTE =
  '<div class="panel-placeholder quiet" data-panel-status="true" role="status" aria-live="polite">Updated.</div>';

let browser;
before(async () => {
  if (!ENABLED) return;
  const mod = await import(process.env.PLAYWRIGHT_MODULE);
  browser = await (mod.default ?? mod).chromium.launch();
});
after(async () => {
  await browser?.close();
});

async function measure(markup) {
  const page = await browser.newPage({ viewport: { width: 1166, height: 812 } });
  try {
    await page.setContent(
      `<!doctype html><html><head><meta charset="utf-8"><style>body{margin:0}p{margin:0}</style></head><body>${markup}</body></html>`,
    );
    await page.addStyleTag({ path: COMPONENTS_CSS });
    return await check({ page, target });
  } finally {
    await page.close();
  }
}

const only = (result) => result.measurements.panels[0];

test("a visible child after a clipped live region measures its own inset", { skip: !ENABLED }, async () => {
  const result = await measure(`<main id="main">
    <section class="panel" id="agents-panel"><div class="body" id="agents-body" style="padding:0">
      ${QUIET_NOTE}
      <section class="strip" style="padding:0 12px"><p>Agents</p></section>
    </div></section>
  </main>`);
  assert.equal(result.status, "pass", JSON.stringify(result));
  assert.equal(only(result).content_selector, "p", JSON.stringify(result));
  assert.equal(only(result).effective_inset_px, 12, JSON.stringify(result));
});

test("a zero-inset visible child still fails when a clipped live region precedes it", { skip: !ENABLED }, async () => {
  const result = await measure(`<main id="main">
    <section class="panel" id="agents-panel"><div class="body" id="agents-body" style="padding:0">
      ${QUIET_NOTE}
      <section class="strip" style="padding:0"><p>Agents</p></section>
    </div></section>
  </main>`);
  assert.equal(result.status, "fail", JSON.stringify(result));
  const [failure] = result.measurements.failures;
  assert.equal(failure.content_selector, "p", JSON.stringify(result));
  assert.equal(failure.effective_inset_px, 0, JSON.stringify(result));
  assert.equal(failure.reason, "content touches the panel border (zero inset)", JSON.stringify(result));
});

test("an ordinary painted status message is still measured as content", { skip: !ENABLED }, async () => {
  const inset = await measure(`<main id="main">
    <section class="panel" id="agents-panel"><div class="body" id="agents-body" style="padding:0">
      ${QUIET_NOTE}
      <div class="panel-placeholder" data-panel-status="true" role="status">Loading…</div>
    </div></section>
  </main>`);
  assert.equal(inset.status, "pass", JSON.stringify(inset));
  assert.equal(only(inset).content_selector, "div.panel-placeholder", JSON.stringify(inset));
  assert.equal(only(inset).effective_inset_px, 16, JSON.stringify(inset));

  const zero = await measure(`<main id="main">
    <section class="panel" id="agents-panel"><div class="body" id="agents-body" style="padding:0">
      ${QUIET_NOTE}
      <div class="panel-placeholder" data-panel-status="true" role="status" style="padding:0">Loading…</div>
    </div></section>
  </main>`);
  assert.equal(zero.status, "fail", JSON.stringify(zero));
  assert.equal(zero.measurements.failures[0].content_selector, "div.panel-placeholder", JSON.stringify(zero));
});

test("an inset that differs from sibling panels still fails", { skip: !ENABLED }, async () => {
  const result = await measure(`<main id="main">
    <section class="panel" id="panel-a"><div class="body" style="padding:0 12px"><p>A</p></div></section>
    <section class="panel" id="panel-b"><div class="body" style="padding:0 2px"><p>B</p></div></section>
    <section class="panel" id="panel-c"><div class="body" style="padding:0 12px"><p>C</p></div></section>
  </main>`);
  assert.equal(result.status, "fail", JSON.stringify(result));
  const failures = result.measurements.failures;
  assert.equal(failures.length, 1, JSON.stringify(result));
  assert.equal(failures[0].panel, "#panel-b", JSON.stringify(result));
  assert.equal(failures[0].reason, "inset differs from sibling panels", JSON.stringify(result));
  assert.equal(failures[0].sibling_inset_px, 12, JSON.stringify(result));
});
