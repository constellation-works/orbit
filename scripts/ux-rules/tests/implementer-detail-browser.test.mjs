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

test("implementer detail stats stay readable at desktop and phone widths", {
  timeout: 180000,
  skip: !process.env.PLAYWRIGHT_MODULE || !process.env.ORBIT_VISUAL_ORBIT,
}, async () => {
  const root = process.cwd();
  const manifest = await loadManifest(path.join(root, "crates/orbit-web/visual/views.toml"));
  const targets = expandTargets(rule, [manifest]).filter((t) => t.view === "analytics-implementer-detail");
  const target1440 = targets.find((target) => target.width === 1440);
  const target375 = targets.find((target) => target.width === 375);
  assert.ok(target1440 && target375);
  const server = await startFixtureServer(target1440.appDef, target1440.viewDef.fixture, root);
  const outDir = process.env.ORBIT_UX_EVIDENCE || path.join(root, ".orbit/tmp/ux-implementer-detail");
  const ctx = await createUiContext({ repoRoot: root, outDir });
  try {
    const results = [];
    for (const target of [target1440, target375]) {
      await closeTarget(ctx);
      await ctx.openTarget(target, server.baseUrl, server.placeholders);
      const stats = ctx.page.locator("#agents-detail .agents-stats");
      const text = await stats.textContent();
      assert.match(text, /Cost coverage \(counted attempts\)/);
      assert.match(text, /Cost per landed task \(USD\)/);
      assert.match(text, /Cost missing-run attempts/);
      assert.match(text, /18 measured · 4 unpriced · 2 missing run of 24 counted attempts/);
      assert.match(text, /Tasks by tiermedium: 24/);

      // Reapply the original production declaration to capture and verify the
      // broken layout on the same candidate, view, data and viewport.
      const baselineStyle = await ctx.page.addStyleTag({ content: `
        .agents-stats { grid-template-columns: minmax(0, 1fr) auto !important; }
        .agents-stats dt, .agents-stats dd { min-width: auto !important; overflow-wrap: normal !important; }
      ` });
      const before = await check(ctx);
      const beforeFingerprint = `ui.cramped-wrap|analytics-implementer-detail|dark|${target.width}|default`;
      assert.equal(before.status, "fail", JSON.stringify(before));
      assert.equal(before.fingerprint, beforeFingerprint, JSON.stringify(before));
      assert.ok((before.measurements.failures || []).some((failure) => failure.collapsed), JSON.stringify(before));
      await ctx.page.locator("[data-ux-evidence-target]").evaluateAll((nodes) => nodes.forEach((node) => node.removeAttribute("data-ux-evidence-target")));
      await stats.scrollIntoViewIfNeeded();
      const beforePath = path.join(outDir, `analytics-implementer-detail-dark-${target.width}-before.png`);
      await ctx.page.screenshot({ path: beforePath });
      await baselineStyle.evaluate((style) => style.remove());

      const after = await check(ctx);
      assert.equal(after.status, "pass", JSON.stringify(after));
      assert.equal(after.fingerprint, beforeFingerprint, JSON.stringify({ before, after }));
      const geometry = await stats.evaluate((list) => {
        const children = [...list.children];
        const columns = getComputedStyle(list).gridTemplateColumns.split(" ").map(Number.parseFloat);
        const pairs = [];
        for (let index = 0; index < children.length; index += 2) {
          const label = children[index].getBoundingClientRect();
          const value = children[index + 1].getBoundingClientRect();
          pairs.push({ labelWidth: label.width, valueWidth: value.width, separated: label.right <= value.left });
        }
        return { columns, pairs };
      });
      assert.equal(geometry.columns.length, 2, JSON.stringify(geometry));
      assert.ok(geometry.columns.every((width) => width > 0), JSON.stringify(geometry));
      assert.ok(geometry.pairs.every((pair) => pair.labelWidth > 0 && pair.valueWidth > 0 && pair.separated), JSON.stringify(geometry));

      const afterPath = path.join(outDir, `analytics-implementer-detail-dark-${target.width}-after.png`);
      await stats.scrollIntoViewIfNeeded();
      await ctx.page.screenshot({ path: afterPath });
      assert.deepEqual(pngSize(beforePath), { width: target.width, height: target.width === 1440 ? 900 : 812 });
      assert.deepEqual(pngSize(afterPath), { width: target.width, height: target.width === 1440 ? 900 : 812 });

      // An ordinary label that wraps to two lines remains acceptable.
      await stats.evaluate((list) => {
        const label = document.createElement("dt");
        label.textContent = "Ordinary two line label";
        label.style.maxWidth = "95px";
        const value = document.createElement("dd");
        value.textContent = "normal value";
        list.append(label, value);
      });
      const ordinary = stats.locator("dt").last();
      const lineCount = await ordinary.evaluate((element) => {
        const range = document.createRange();
        range.selectNodeContents(element);
        return new Set([...range.getClientRects()].map((rect) => Math.round(rect.top))).size;
      });
      assert.equal(lineCount, 2, "negative control really wraps to two lines");
      const ordinaryResult = await check(ctx);
      assert.ok(!(ordinaryResult.measurements.failures || []).some((failure) => failure.text === "Ordinary two line label"), JSON.stringify(ordinaryResult));
      results.push({ before, after, geometry, ordinary: ordinaryResult, captures: [beforePath, afterPath] });
    }
    console.log(JSON.stringify(results));
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

test("experiment detail Recorded effort passes and evidence machinery is covered", {
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
    // ORB-20986: Recorded effort spans the full crew row, so the retained
    // 2015-char fixture passes the rule with every recorded detail intact.
    const before = await check(ctx);
    assert.equal(before.fingerprint, "ui.cramped-wrap|analytics-experiment-detail|dark|1440|default");
    assert.ok(!(before.measurements.failures || []).some((f) => /^Recorded effort:/.test(f.text)), JSON.stringify(before));
    const efforts = ctx.page.locator(".ex-crew-rows span.mx-muted", { hasText: "Recorded effort:" });
    assert.match(await efforts.first().textContent(), /^Recorded effort: unknown crew · model-00/);
    assert.match(await efforts.first().textContent(), /model-39/);
    assert.equal((await efforts.nth(1).textContent()).trim(), "Recorded effort: unknown");
    assert.equal(await handles(), 0, "a passing result stamps no handles");

    // The evidence machinery below needs a failing result: stage one with a
    // synthetic narrow row, then exercise framing, scroll restore, capture
    // failure diagnostics and the 20-row cap against it.
    await ctx.page.evaluate(() => {
      const span = document.createElement("span");
      span.id = "qa-cramped";
      span.style.cssText = "display:block;width:40px";
      span.textContent = "Synthetic cramped prose row with enough words to wrap narrowly across many lines of tiny text";
      document.querySelector(".tab-pane.active").append(span);
    });
    const failing = await check(ctx);
    assert.equal(failing.status, "fail", JSON.stringify(failing));
    assert.equal(failing.fingerprint, before.fingerprint);
    const failure = (failing.measurements.failures || []).find((f) => f.text.startsWith("Synthetic cramped"));
    assert.ok(failure, JSON.stringify(failing));
    assert.ok(failure.evidence_selector);
    assert.equal(await handles(), failing.measurements.failures.length, "only kept failures are stamped");
    assert.ok(failing.measurements.failures.length <= 20);
    const framed = ctx.page.locator(failure.evidence_selector);
    assert.equal(await framed.count(), 1);
    assert.match(await framed.textContent(), /^Synthetic cramped/);

    // Nested scroller starting at zero, plus a nonzero page-pane offset.
    await ctx.page.evaluate(() => {
      const box = document.createElement("div");
      box.id = "qa-scroll";
      box.style.cssText = "position:fixed;top:100px;left:100px;width:300px;height:200px;overflow:auto;z-index:99999;background:white";
      box.innerHTML = '<div style="height:600px"></div><span id="qa-target">Measurable target</span><div style="height:200px"></div>';
      document.body.append(box);
      document.querySelector(".tab-pane.active").scrollTop = 123;
    });
    const shot = await ctx.screenshotFor(rule, target1440, failing);
    assert.deepEqual(pngSize(path.join(outDir, shot)), { width: 1440, height: 900 });
    assert.equal(await handles(), 0, "capture removes every temporary handle");
    assert.equal(await ctx.page.evaluate(() => document.querySelector(".tab-pane.active").scrollTop), 123, "nonzero scroll restored");
    const repeated = await check(ctx);
    assert.equal(repeated.fingerprint, failing.fingerprint);
    assert.ok((repeated.measurements.failures || []).some((f) => f.text.startsWith("Synthetic cramped")));
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
