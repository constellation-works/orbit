// Real fixture sections plus injected cards. The band exemption is geometric:
// these controls are not an id allowlist. Needs the browser kit
// (PLAYWRIGHT_MODULE from its env.sh) and `orbit` on PATH for the fixture backend.
import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";

import { loadManifest } from "../lib/manifest.mjs";
import {
  closeTarget,
  createUiContext,
  expandTargets,
  startFixtureServer,
} from "../lib/ui-context.mjs";
import { check, rule } from "../ui/nested-card.mjs";

const ENABLED = Boolean(process.env.PLAYWRIGHT_MODULE);

const NAMED = {
  "analytics-implementers": ["#agents-controls"],
  "health-doctor": ["#doctor-controls"],
  "automation-routines": ["#clock-body", "#routines-body"],
};

// Other default-state views whose only nested-card hits, before the band
// exemption, were the same full-bleed unframed shape.
const SAME_SHAPE = ["jobs-live", "analytics-metrics", "settings"];

// The floated child keeps position: static but is out of normal flow, so it
// must fail even though it is flush, zero-margin and filled (ORB-21076).
const POSITIVE_MARKUP = `
<section class="panel" id="ux-nested-positive" style="display:flow-root;width:360px;margin:16px;border:1px solid #2c333b;background:#121519">
  <div id="ux-band" style="display:block;background:#0b0d10;margin:0;border-bottom:1px solid #2c333b;padding:8px 12px">Band</div>
  <div id="ux-negative" style="display:block;box-sizing:border-box;width:100%;margin:-8px 0 0 0;background:#243044;padding:8px">Negative margin card</div>
  <div id="ux-filled" style="display:block;background:#243044;margin:12px;padding:8px">Filled card</div>
  <div id="ux-bordered" style="display:block;border:1px solid #8aa;margin:12px;padding:8px">Bordered card</div>
  <div id="ux-fullwidth" style="display:block;box-sizing:border-box;width:100%;margin:0;background:#243044;border:1px solid #8aa;border-radius:8px;box-shadow:0 1px 2px rgba(0,0,0,.4);padding:8px">Full width card</div>
  <div id="ux-separated" style="display:block;width:100%;margin:8px 0;background:#243044;padding:8px">Separated full-width card</div>
  <div id="ux-group" role="group" style="display:block;background:#243044;margin:12px;padding:8px">Group</div>
  <div id="ux-floated" style="float:left;width:100%;height:30px;margin:0;background:#243044">Floated full-width band</div>
</section>`;

function failuresOf(result) {
  return result.measurements.failures ?? [];
}

function selectorsOf(result) {
  return failuresOf(result).map((item) => item.selector);
}

test("panel bands are not nested cards and real cards still are", { timeout: 240000, skip: !ENABLED }, async () => {
  const root = process.cwd();
  const manifest = await loadManifest(path.join(root, "crates/orbit-web/visual/views.toml"));
  const wanted = new Set([...Object.keys(NAMED), ...SAME_SHAPE]);
  const targets = expandTargets(rule, [manifest]).filter(
    (target) => target.state === "default" && wanted.has(target.view) && (target.width === 1440 || target.width === 375),
  );
  assert.equal(targets.length, wanted.size * 2);
  const server = await startFixtureServer(targets[0].appDef, "busy-workspace", root);
  const ctx = await createUiContext({
    repoRoot: root,
    outDir: path.join(root, ".orbit/tmp/nested-card-browser"),
  });
  try {
    for (const target of targets) {
      await closeTarget(ctx);
      await ctx.openTarget(target, server.baseUrl, server.placeholders);
      const result = await check(ctx);
      const fingerprint = `ui.nested-card|${target.view}|dark|${target.width}|default`;
      assert.equal(result.fingerprint, fingerprint, JSON.stringify(result));
      assert.equal(result.status, "pass", JSON.stringify(selectorsOf(result)));
      for (const selector of NAMED[target.view] ?? []) {
        assert.equal(selectorsOf(result).includes(selector), false, selector);
      }
    }

    for (const width of [1440, 375]) {
      const target = targets.find((item) => item.view === "health-doctor" && item.width === width);
      await closeTarget(ctx);
      await ctx.openTarget(target, server.baseUrl, server.placeholders);
      await ctx.page.evaluate((markup) => {
        const panes = [...document.querySelectorAll(".tab-pane")].filter((pane) => {
          const style = getComputedStyle(pane);
          const rect = pane.getBoundingClientRect();
          return style.display !== "none" && style.visibility !== "hidden" && rect.width > 0 && rect.height > 0;
        });
        const host = document.createElement("div");
        host.innerHTML = markup;
        panes[panes.length - 1].appendChild(host.firstElementChild);
      }, POSITIVE_MARKUP);
      const result = await check(ctx);
      const failed = new Set(selectorsOf(result));
      assert.equal(failed.has("#doctor-controls"), false, JSON.stringify([...failed]));
      assert.equal(failed.has("#ux-band"), false, JSON.stringify([...failed]));
      assert.equal(failed.has("#ux-group"), false, JSON.stringify([...failed]));
      for (const selector of ["#ux-filled", "#ux-bordered", "#ux-fullwidth", "#ux-separated", "#ux-negative", "#ux-floated"]) {
        assert.equal(failed.has(selector), true, JSON.stringify([...failed]));
      }
      const floated = failuresOf(result).find((item) => item.selector === "#ux-floated");
      assert.equal(floated.band, false);
      assert.equal(floated.filled, true);
      assert.equal(floated.bordered, false);
      const fullWidth = failuresOf(result).find((item) => item.selector === "#ux-fullwidth");
      assert.equal(fullWidth.band, false);
      assert.equal(fullWidth.filled, true);
      assert.equal(fullWidth.bordered, true);
      const filled = failuresOf(result).find((item) => item.selector === "#ux-filled");
      assert.equal(filled.filled, true);
      assert.equal(filled.bordered, false);
      const bordered = failuresOf(result).find((item) => item.selector === "#ux-bordered");
      assert.equal(bordered.bordered, true);
      assert.equal(result.fingerprint, `ui.nested-card|health-doctor|dark|${width}|default`);
    }
  } finally {
    await closeTarget(ctx);
    await ctx.close();
    server.stop();
  }
});
