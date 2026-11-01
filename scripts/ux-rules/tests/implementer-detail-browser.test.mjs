// Real fixture dashboard and actual geometry detector; run with the browser kit
// and ORBIT_VISUAL_ORBIT set to an installed binary in a disposable root.
import assert from "node:assert/strict";
import test from "node:test";
import { existsSync, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { loadManifest } from "../lib/manifest.mjs";
import { attachFailureScreenshot, createUiContext, expandTargets, startFixtureServer, closeTarget } from "../lib/ui-context.mjs";
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
    assert.equal(before.fingerprint, "ui.cramped-wrap|analytics-implementer-detail|dark|1440|default", JSON.stringify(before));
    for (const label of ["Cost coverage (counted attempts)", "Cost per landed task (USD)", "Cost missing-run attempts"]) {
      assert.ok(failures.some((f) => f.selector === "dt" && f.text === label), JSON.stringify(before));
    }
    assert.ok(!failures.some((f) => /Invisible zero width|Clipped zero width|Offscreen clipped zero width/.test(f.text)), JSON.stringify(before));
    await stats.scrollIntoViewIfNeeded();
    await ctx.screenshotFor(rule, target);
    // Give the same dashboard data a readable, bounded two-column layout.
    await ctx.page.addStyleTag({ content: "#agents-detail .agents-stats { grid-template-columns: minmax(160px, 1fr) minmax(0, 1fr) !important; } #agents-detail .agents-stats dd { overflow-wrap: anywhere; }" });
    const after = await check(ctx);
    assert.equal(after.fingerprint, before.fingerprint, JSON.stringify({ before, after }));
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

function pngSize(file) {
  const bytes = readFileSync(file);
  assert.equal(bytes.toString("ascii", 1, 4), "PNG");
  return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
}

test("experiment detail evidence frames Recorded effort and cleans up scroll and handles", {
  timeout: 240000,
  skip: !process.env.PLAYWRIGHT_MODULE || !process.env.ORBIT_VISUAL_ORBIT,
}, async () => {
  const root = process.cwd();
  const manifest = await loadManifest(path.join(root, "crates/orbit-web/visual/views.toml"));
  const targets = expandTargets(rule, [manifest]).filter((t) => t.view === "analytics-experiment-detail");
  const target1440 = targets.find((t) => t.width === 1440);
  const target375 = targets.find((t) => t.width === 375);
  assert.ok(target1440 && target375);
  const server = await startFixtureServer(target1440.appDef, target1440.viewDef.fixture, root);
  const outDir = process.env.ORBIT_UX_EVIDENCE || path.join(os.tmpdir(), "orbit-ux-experiment-detail");
  const ctx = await createUiContext({ repoRoot: root, outDir });
  const handles = () => ctx.page.locator("[data-ux-evidence-target]").count();
  try {
    await ctx.openTarget(target1440, server.baseUrl, server.placeholders);
    const before = await check(ctx);
    assert.equal(before.status, "fail", JSON.stringify(before));
    assert.equal(before.fingerprint, "ui.cramped-wrap|analytics-experiment-detail|dark|1440|default");
    const failure = (before.measurements.failures || [])[0];
    assert.ok(failure, JSON.stringify(before));
    assert.equal(failure.selector, "span.mx-muted");
    assert.equal(failure.chars, 2015);
    assert.equal(failure.lines, 121);
    assert.equal(failure.width_px, 140);
    assert.match(failure.text, /^Recorded effort:/);
    assert.ok(failure.evidence_selector);
    assert.equal(await handles(), before.measurements.failures.length, "only kept failures are stamped");
    assert.ok(before.measurements.failures.length <= 20);
    const framed = ctx.page.locator(failure.evidence_selector);
    assert.equal(await framed.count(), 1);
    assert.match(await framed.textContent(), /^Recorded effort:/);

    // Nested scroller starting at zero, plus a nonzero page-pane offset.
    await ctx.page.evaluate(() => {
      const box = document.createElement("div");
      box.id = "qa-scroll";
      box.style.cssText = "position:fixed;top:100px;left:100px;width:300px;height:200px;overflow:auto;z-index:99999;background:white";
      box.innerHTML = '<div style="height:600px"></div><span id="qa-target">Measurable target</span><div style="height:200px"></div>';
      document.body.append(box);
      document.querySelector(".tab-pane.active").scrollTop = 123;
    });
    const shot = await ctx.screenshotFor(rule, target1440, before);
    assert.deepEqual(pngSize(path.join(outDir, shot)), { width: 1440, height: 900 });
    assert.equal(await handles(), 0, "capture removes every temporary handle");
    assert.equal(await ctx.page.evaluate(() => document.querySelector(".tab-pane.active").scrollTop), 123, "nonzero scroll restored");
    const repeated = await check(ctx);
    assert.equal(repeated.fingerprint, before.fingerprint);
    assert.equal((repeated.measurements.failures || [])[0]?.lines, 121);
    assert.equal(await handles(), repeated.measurements.failures.length);
    await ctx.screenshotFor(rule, target1440, repeated);
    assert.equal(await handles(), 0);

    // Initially-zero nested scroll stays zero; nonzero nested scroll is kept.
    const qaShot = { measurements: { evidence_selector: "#qa-target" } };
    await ctx.screenshotFor(rule, target1440, qaShot);
    assert.equal(await ctx.page.locator("#qa-scroll").evaluate((e) => e.scrollTop), 0, "zero nested scroll restored to zero");
    await ctx.page.locator("#qa-scroll").evaluate((e) => { e.scrollTop = 77; });
    await ctx.screenshotFor(rule, target1440, qaShot);
    assert.equal(await ctx.page.locator("#qa-scroll").evaluate((e) => e.scrollTop), 77, "nonzero nested scroll restored");

    // Targeted capture failure: no misleading clip PNG is returned or
    // attached; an explicit diagnostic is retained and state is restored.
    await ctx.page.evaluate(() => { document.querySelector(".tab-pane.active").scrollTop = 55; document.querySelector("#qa-scroll").scrollTop = 0; });
    const stamped = await check(ctx);
    const fingerprintBefore = stamped.fingerprint;
    const pngDestination = path.join(outDir, "evidence", rule.id, "analytics-experiment-detail-dark-1440-default.png");
    const originalShot = ctx.page.screenshot.bind(ctx.page);
    ctx.page.screenshot = async () => { throw new Error("forced capture failure"); };
    let attached = "unset";
    try {
      attached = await attachFailureScreenshot(ctx, rule, target1440, stamped);
    } finally {
      ctx.page.screenshot = originalShot;
    }
    assert.equal(attached, "", "failed targeted capture returns no PNG");
    assert.ok(!stamped.evidence.some((entry) => entry.endsWith(".png")), JSON.stringify(stamped.evidence));
    assert.ok(stamped.evidence.some((entry) => entry.endsWith(".txt")), JSON.stringify(stamped.evidence));
    assert.equal(stamped.measurements.evidence_capture?.status, "failed");
    assert.equal(stamped.measurements.evidence_capture?.kind, "evidence_capture_failed");
    assert.match(stamped.measurements.evidence_capture?.reason ?? "", /targeted viewport capture failed.*forced capture failure/);
    assert.equal(stamped.status, "fail", "underlying finding is preserved");
    assert.equal(stamped.fingerprint, fingerprintBefore, "fingerprint is preserved");
    assert.equal(existsSync(pngDestination), false, "no stale or fallback clip remains at the destination");
    assert.equal(await handles(), 0, "failed capture removes every handle");
    assert.equal(await ctx.page.evaluate(() => document.querySelector(".tab-pane.active").scrollTop), 55, "failed capture restores nonzero scroll");
    assert.equal(await ctx.page.locator("#qa-scroll").evaluate((e) => e.scrollTop), 0, "failed capture restores initially-zero nested scroll");
    // Same failure with a nonzero nested offset keeps that offset.
    await ctx.page.locator("#qa-scroll").evaluate((e) => { e.scrollTop = 77; });
    const stamped2 = await check(ctx);
    ctx.page.screenshot = async () => { throw new Error("forced capture failure"); };
    try {
      assert.equal(await attachFailureScreenshot(ctx, rule, target1440, stamped2), "");
    } finally {
      ctx.page.screenshot = originalShot;
    }
    assert.ok(!stamped2.evidence.some((entry) => entry.endsWith(".png")), JSON.stringify(stamped2.evidence));
    assert.equal(stamped2.measurements.evidence_capture?.status, "failed");
    assert.equal(await ctx.page.locator("#qa-scroll").evaluate((e) => e.scrollTop), 77, "failed capture restores nonzero nested scroll");
    assert.equal(await handles(), 0);
    // Unresolved explicit target is likewise honest, never a clip fallback.
    await assert.rejects(
      ctx.screenshotFor(rule, target1440, { measurements: { evidence_selector: '[data-ux-evidence-target="missing"]' } }),
      /evidence capture failed.*did not resolve to exactly one element.*no screenshot attached/,
    );
    assert.equal(await handles(), 0);
    // A genuinely untargeted result keeps the whole-view capture.
    const untargeted = await ctx.screenshotFor(rule, target1440, { measurements: {} });
    assert.ok(untargeted.endsWith(".png"), untargeted);
    assert.equal(existsSync(path.join(outDir, untargeted)), true);

    // Beyond the 20-row cap: no handle exists for a dropped row.
    await ctx.page.evaluate(() => {
      const host = document.createElement("div");
      host.id = "qa-many";
      for (let i = 0; i < 25; i += 1) {
        const span = document.createElement("span");
        span.style.cssText = "display:block;width:40px";
        span.textContent = `Synthetic cramped prose row number ${i} with enough words to wrap narrowly`;
        host.append(span);
      }
      document.querySelector(".tab-pane.active").append(host);
    });
    const many = await check(ctx);
    assert.equal(many.measurements.failures.length, 20, JSON.stringify(many.measurements.failures?.length));
    assert.equal(await handles(), 20, "rows beyond the cap carry no handle");
    await ctx.screenshotFor(rule, target1440, many);
    assert.equal(await handles(), 0);

    await closeTarget(ctx);
    await ctx.openTarget(target375, server.baseUrl, server.placeholders);
    const mobile = await check(ctx);
    assert.ok(!(mobile.measurements.failures || []).some((f) => /^Recorded effort:/.test(f.text)), JSON.stringify(mobile));
    console.log(JSON.stringify({ before, mobile }));
  } finally {
    await closeTarget(ctx);
    await ctx.close();
    server.stop();
  }
});
