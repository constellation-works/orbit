// Real fixture dashboard and actual geometry detector; run with the browser kit
// and ORBIT_VISUAL_ORBIT set to an installed binary in a disposable root.
import assert from "node:assert/strict";
import test from "node:test";
import os from "node:os";
import path from "node:path";
import { loadManifest } from "../lib/manifest.mjs";
import { createUiContext, expandTargets, startFixtureServer, closeTarget } from "../lib/ui-context.mjs";
import { check, rule } from "../ui/cramped-wrap.mjs";

test("implementer detail collapsed labels are detected and readable labels pass", {
  timeout: 180000,
  skip: !process.env.PLAYWRIGHT_MODULE || !process.env.ORBIT_VISUAL_ORBIT,
}, async () => {
  const root = process.cwd();
  const manifest = await loadManifest(path.join(root, "crates/orbit-web/visual/views.toml"));
  const target = expandTargets(rule, [manifest]).find((t) => t.view === "analytics-implementer-detail" && t.width === 1440);
  assert.ok(target);
  const server = await startFixtureServer(target.appDef, target.viewDef.fixture, root);
  const ctx = await createUiContext({ repoRoot: root, outDir: process.env.ORBIT_UX_EVIDENCE || path.join(os.tmpdir(), "orbit-ux-implementer-detail") });
  try {
    await ctx.openTarget(target, server.baseUrl, server.placeholders);
    const stats = ctx.page.locator("#agents-detail .agents-stats");
    assert.match(await stats.textContent(), /Cost coverage \(counted attempts\)/);
    assert.match(await stats.textContent(), /Cost per landed task \(USD\)/);
    assert.match(await stats.textContent(), /Cost missing-run attempts/);
    assert.match(await stats.textContent(), /18 measured · 4 unpriced · 2 missing run/);
    const collapsedLabels = await stats.locator("dt").evaluateAll((nodes) => nodes.filter((node) => node.getBoundingClientRect().width === 0).map((node) => node.textContent));
    assert.ok(collapsedLabels.includes("Cost coverage (counted attempts)"));
    assert.ok(collapsedLabels.includes("Cost per landed task (USD)"));
    assert.ok(collapsedLabels.includes("Cost missing-run attempts"));
    await ctx.page.evaluate(() => {
      const parent = document.querySelector("#agents-detail .agents-stats");
      for (const [text, style] of [["Invisible zero width label", "width:0;opacity:0"], ["Clipped zero width label", "width:0;overflow:hidden"]]) {
        const node = document.createElement("dt");
        node.textContent = text;
        node.style.cssText = style;
        parent.append(node);
      }
      const clip = document.createElement("div");
      clip.style.cssText = "width:100px;height:40px;overflow:hidden;position:relative";
      const offclip = document.createElement("span");
      offclip.style.cssText = "display:block;width:0;position:relative;left:200px";
      offclip.textContent = "Offscreen clipped zero width label";
      clip.append(offclip);
      parent.append(clip);
    });
    const before = await check(ctx);
    const failures = before.measurements.failures || [];
    assert.equal(before.status, "fail", JSON.stringify(before));
    for (const label of ["Cost coverage (counted attempts)", "Cost per landed task (USD)", "Cost missing-run attempts"]) {
      assert.ok(failures.some((f) => f.selector === "dt" && f.text === label), JSON.stringify(before));
    }
    assert.ok(!failures.some((f) => /Invisible zero width|Clipped zero width|Offscreen clipped zero width/.test(f.text)), JSON.stringify(before));
    await stats.scrollIntoViewIfNeeded();
    await ctx.screenshotFor(rule, target);
    // Give the same dashboard data a readable, bounded two-column layout.
    await ctx.page.addStyleTag({ content: "#agents-detail .agents-stats { grid-template-columns: minmax(160px, 1fr) minmax(0, 1fr) !important; } #agents-detail .agents-stats dd { overflow-wrap: anywhere; }" });
    const after = await check(ctx);
    assert.ok(!(after.measurements.failures || []).some((f) => /Cost/.test(f.text)), JSON.stringify(after));
    await stats.scrollIntoViewIfNeeded();
    await stats.screenshot({ path: path.join(ctx.outDir, "implementer-readable.png") });
    // A regular two-line label is not treated as cramped.
    await ctx.page.evaluate(() => {
      const dt = document.querySelector("#agents-detail .agents-stats dt");
      dt.textContent = "Ordinary two line label";
      dt.style.width = "95px";
    });
    const lineCount = await ctx.page.locator("#agents-detail .agents-stats dt").first().evaluate((el) => {
      const range = document.createRange();
      range.selectNodeContents(el);
      return new Set([...range.getClientRects()].map((rect) => Math.round(rect.top))).size;
    });
    assert.equal(lineCount, 2, "negative control really wraps to two lines");
    const normal = await check(ctx);
    assert.ok(!(normal.measurements.failures || []).some((f) => f.text === "Ordinary two line label"));
    console.log(JSON.stringify({ before, after, normal }));
  } finally {
    await closeTarget(ctx);
    await ctx.close();
    server.stop();
  }
});
