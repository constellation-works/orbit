// Optional installed-native evidence for ORB-21111. The fixture launcher owns
// a disposable HOME/root/workspace; no candidate Rust build or live store.
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, statfs, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { loadManifest } from "../lib/manifest.mjs";
import { attachFailureScreenshot, closeTarget, createUiContext, expandTargets, startFixtureServer } from "../lib/ui-context.mjs";
import { expandUiObservations } from "../lib/ui-observations.mjs";
import { check as checkText, rule as textRule } from "../ui/undersized-text.mjs";
import { check as checkConsole, rule as consoleRule } from "../ui/console-clean.mjs";

const enabled = Boolean(process.env.PLAYWRIGHT_MODULE && process.env.UX_GROUPING_NATIVE_ORBIT);

test("installed native fixture retains grouped element PNGs and proven shared API cause across native views", { skip: !enabled, timeout: 180000 }, async () => {
  const disk = await statfs(os.tmpdir());
  assert.ok(1 - disk.bavail / disk.blocks < 0.8, "disk gate creates no fixture at 80% capacity");
  const base = process.env.UX_GROUPING_NATIVE_OUT ?? os.tmpdir();
  await mkdir(base, { recursive: true });
  const out = await mkdtemp(path.join(base, "ux-native-grouping-"));
  const manifest = await loadManifest("crates/orbit-web/visual/views.toml");
  const targets = expandTargets(textRule, [manifest]).filter((target) => target.state === "default" && target.view === "health-doctor");
  const previousScratch = process.env.ORBIT_SCRATCH_DIR;
  const previousOrbit = process.env.ORBIT_VISUAL_ORBIT;
  process.env.ORBIT_SCRATCH_DIR = out;
  process.env.ORBIT_VISUAL_ORBIT = process.env.UX_GROUPING_NATIVE_ORBIT;
  let server;
  let ctx;
  try {
    server = await startFixtureServer(targets[0].appDef, "busy-workspace", process.cwd());
    ctx = await createUiContext({ outDir: out, repoRoot: process.cwd() });
    const results = [];
    for (const target of targets) {
      await closeTarget(ctx);
      await ctx.openTarget(target, server.baseUrl, server.placeholders);
      await ctx.page.addStyleTag({ content: "#doctor-refresh{font-size:9px!important}" });
      const aggregate = await checkText(ctx);
      const member = expandUiObservations(aggregate).find((result) => result.status === "fail" && result.measurements.observation?.selector === "#doctor-refresh");
      assert.ok(member, JSON.stringify(aggregate));
      await attachFailureScreenshot(ctx, textRule, target, member);
      assert.equal(member.evidence.length, 1);
      results.push({ ...member, surface: "ui" });
    }
    assert.equal(results[0].defect.key, results[1].defect.key);
    const consoleTargets = expandTargets(consoleRule, [manifest]).filter((target) => target.state === "default" && target.width === 1440 && ["health-doctor", "analytics-metrics"].includes(target.view));
    const byPage = [];
    for (const original of consoleTargets) {
      const target = { ...original, state: "error", id: original.id.replace(/default$/, "error") };
      await closeTarget(ctx);
      await ctx.openTarget(target, server.baseUrl, server.placeholders);
      const members = expandUiObservations(await checkConsole(ctx)).filter((result) => result.status === "fail" && result.measurements.observation?.status === 500 && result.measurements.observation?.source);
      byPage.push({ target, members });
    }
    const shared = byPage[0].members.find((member) => byPage[1].members.some((other) => other.defect.key === member.defect.key));
    assert.ok(shared, JSON.stringify(byPage.map(({ target, members }) => ({ view: target.view, members: members.map((member) => member.measurements.observation) }))));
    for (const { target, members } of byPage) {
      // Reopen with the same deliberate error-state fixture before capture.
      await closeTarget(ctx);
      await ctx.openTarget(target, server.baseUrl, server.placeholders);
      const member = members.find((candidate) => candidate.defect.key === shared.defect.key);
      await attachFailureScreenshot(ctx, consoleRule, target, member);
      assert.equal(member.evidence.length, 1);
      results.push({ ...member, surface: "ui" });
    }
    const report = { schema_version: 1, surface: "ui", commit: ctx.commit, kit: ctx.kit,
      target: { mode: "fixture", native_backend: process.env.UX_GROUPING_NATIVE_ORBIT,
        fault_injections: ["9px font on native #doctor-refresh", "error-state route returns 500 for native API requests"] },
      results, summary: { pass: 0, fail: results.length, error: 0, skipped: 0, observations: results.length, defect_groups: 2 } };
    await writeFile(path.join(out, "ux-rules.json"), `${JSON.stringify(report, null, 2)}\n`);
    for (const result of results) for (const evidence of result.evidence) assert.ok((await readFile(path.join(out, evidence))).length > 0);
    console.log(`ORB-21111 installed-native browser report: ${path.join(out, "ux-rules.json")}`);
  } finally {
    await closeTarget(ctx ?? {});
    await ctx?.close();
    server?.stop();
    if (previousScratch === undefined) delete process.env.ORBIT_SCRATCH_DIR;
    else process.env.ORBIT_SCRATCH_DIR = previousScratch;
    if (previousOrbit === undefined) delete process.env.ORBIT_VISUAL_ORBIT;
    else process.env.ORBIT_VISUAL_ORBIT = previousOrbit;
  }
});
