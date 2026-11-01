// Real fixture dashboard: the cramped-wrap evidence capture must frame the
// measured Recorded effort paragraph, not the manifest clip, and must put
// the page's scroll state back. Run with the browser kit and
// ORBIT_VISUAL_ORBIT set to an installed binary in a disposable root.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { loadManifest } from "../lib/manifest.mjs";
import { createUiContext, expandTargets, startFixtureServer, closeTarget } from "../lib/ui-context.mjs";
import { check, rule } from "../ui/cramped-wrap.mjs";

function pngSize(file) {
  const bytes = readFileSync(file);
  assert.equal(bytes.toString("ascii", 1, 4), "PNG");
  return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
}

test("experiment detail evidence frames the measured Recorded effort paragraph", {
  timeout: 180000,
  skip: !process.env.PLAYWRIGHT_MODULE || !process.env.ORBIT_VISUAL_ORBIT,
}, async () => {
  const root = process.cwd();
  const manifest = await loadManifest(path.join(root, "crates/orbit-web/visual/views.toml"));
  const targets = expandTargets(rule, [manifest]).filter((t) => t.view === "analytics-experiment-detail");
  const target1440 = targets.find((t) => t.width === 1440);
  const target375 = targets.find((t) => t.width === 375);
  assert.ok(target1440 && target375, "experiment detail must expand to 1440 and 375");
  const server = await startFixtureServer(target1440.appDef, target1440.viewDef.fixture, root);
  const outDir = process.env.ORBIT_UX_EVIDENCE || path.join(os.tmpdir(), "orbit-ux-experiment-detail");
  const ctx = await createUiContext({ repoRoot: root, outDir });
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
    assert.ok(failure.evidence_selector, "failure must carry a unique evidence handle");
    const framed = ctx.page.locator(failure.evidence_selector);
    assert.equal(await framed.count(), 1, "evidence handle must resolve to exactly the measured element");
    assert.match(await framed.textContent(), /^Recorded effort:/);

    // Put the page in a known scrolled state; framing must restore it.
    await ctx.page.evaluate(() => { document.querySelector(".tab-pane").scrollTop = 123; });
    const scrollBefore = await ctx.page.evaluate(() => document.querySelector(".tab-pane").scrollTop);
    const shot = await ctx.screenshotFor(rule, target1440, before);
    assert.ok(shot.endsWith(".png"), shot);
    // A framed viewport capture, not the 372px-wide manifest clip strip.
    assert.deepEqual(pngSize(path.join(outDir, shot)), { width: 1440, height: 900 });
    const scrollAfter = await ctx.page.evaluate(() => document.querySelector(".tab-pane").scrollTop);
    assert.equal(scrollAfter, scrollBefore, "capture must restore scroll state");
    assert.equal(await framed.count(), 0, "capture must remove its temporary evidence handle");
    // A repeat measurement on the restored page is unchanged.
    const repeated = await check(ctx);
    assert.equal(repeated.status, "fail");
    assert.equal(repeated.fingerprint, before.fingerprint);
    assert.equal((repeated.measurements.failures || [])[0]?.lines, 121);

    // The same text at 375 wraps in the single-column layout and passes:
    // a readable layout does not acquire a cramped-wrap finding.
    await closeTarget(ctx);
    await ctx.openTarget(target375, server.baseUrl, server.placeholders);
    const mobile = await check(ctx);
    assert.ok(
      !(mobile.measurements.failures || []).some((f) => /^Recorded effort:/.test(f.text)),
      JSON.stringify(mobile),
    );
    console.log(JSON.stringify({ before, mobile }));
  } finally {
    await closeTarget(ctx);
    await ctx.close();
    server.stop();
  }
});
