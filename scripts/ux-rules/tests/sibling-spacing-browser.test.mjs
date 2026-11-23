// Browser regressions for ui.sibling-spacing using the dashboard component
// stylesheet. Needs the browser kit (PLAYWRIGHT_MODULE).
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { fileURLToPath } from "node:url";

import { loadManifest } from "../lib/manifest.mjs";
import {
  closeTarget,
  createUiContext,
  expandTargets,
  startFixtureServer,
} from "../lib/ui-context.mjs";
import { check, rule } from "../ui/sibling-spacing.mjs";

const ENABLED = Boolean(process.env.PLAYWRIGHT_MODULE);
const ROOT = fileURLToPath(new URL("../../..", import.meta.url));
const CSS = ["base.css", "components.css"].map((name) =>
  fileURLToPath(new URL(`../../../crates/orbit-web/assets/dashboard/css/${name}`, import.meta.url)),
);

const evidence = { anomalies: [], states: [], ambiguous: [], views: [] };

function targetFor(view, width) {
  return { id: `${view}|dark|${width}|default`, view, width, theme: "dark", state: "default" };
}

function coverageNamed(result, name) {
  return (result.measurements.coverage ?? []).filter((row) => row.coverage === name);
}

function failureNamed(result, selector, property) {
  return (result.measurements.failures ?? []).find((item) => item.selector === selector && item.property === property);
}

let browser;

describe("ui.sibling-spacing", { concurrency: false }, () => {
  before(async () => {
    if (!ENABLED) return;
    const mod = await import(process.env.PLAYWRIGHT_MODULE);
    browser = await (mod.default ?? mod).chromium.launch();
  });

  after(async () => {
    await browser?.close();
    const directory = path.join(ROOT, ".orbit/tmp");
    await mkdir(directory, { recursive: true });
    await writeFile(path.join(directory, "sibling-spacing-evidence.json"), JSON.stringify(evidence, null, 2));
  });

  async function measure(width, markup, { css = [] } = {}) {
    const page = await browser.newPage({ viewport: { width, height: width === 1440 ? 900 : 812 }, deviceScaleFactor: 1 });
    try {
      await page.setContent("<!doctype html><html><head><meta charset=\"utf-8\"></head><body></body></html>");
      for (const file of css) await page.addStyleTag({ path: file });
      await page.evaluate((html) => {
        document.body.innerHTML = html;
      }, markup);
      const result = await check({ page, target: targetFor("sibling-spacing-fixture", width) });
      return { page, result };
    } catch (error) {
      await page.close();
      throw error;
    }
  }

  for (const width of [1440, 375]) {
    test(`padding, font-size and font-weight anomalies still fail inside one kind at ${width}`, { skip: !ENABLED }, async () => {
      const { page, result } = await measure(width, `
        <div id="pad-peers">
          <span class="pill" data-state="running" id="pad-a">running</span>
          <span class="pill" data-state="pending" id="pad-b">paused</span>
          <span class="pill" data-state="success" id="pad-c" style="padding: 4px 20px">concluded</span>
        </div>
        <div id="size-peers">
          <span class="chip" id="size-a">a</span>
          <span class="chip" id="size-b">b</span>
          <span class="chip" id="size-c" style="font-size: 18px">c</span>
        </div>
        <div id="weight-peers">
          <span class="pill" data-state="running" id="weight-a">a</span>
          <span class="pill" data-state="running" id="weight-b">b</span>
          <span class="pill" data-state="running" id="weight-c" style="font-weight: 700">c</span>
        </div>`, { css: CSS });
      try {
        assert.equal(result.status, "fail");
        for (const expected of [
          { selector: "#pad-c", property: "padding", kind: "pill", parent: "#pad-peers" },
          { selector: "#size-c", property: "font_size", kind: "chip", parent: "#size-peers" },
          { selector: "#weight-c", property: "font_weight", kind: "pill", parent: "#weight-peers" },
        ]) {
          const failure = failureNamed(result, expected.selector, expected.property);
          assert.ok(failure, JSON.stringify(result.measurements.failures));
          assert.equal(failure.kind, expected.kind);
          assert.equal(failure.parent, expected.parent);
          assert.equal(typeof failure.value, "string");
          assert.equal(typeof failure.sibling_value, "string");
          assert.notEqual(failure.value, failure.sibling_value);
          assert.equal(
            (result.measurements.failures ?? []).some((item) => item.selector === expected.selector.replace("-c", "-a") && item.property === expected.property),
            false,
          );
        }
        evidence.anomalies.push({
          width,
          failures: result.measurements.failures,
          status: result.status,
        });
      } finally {
        await page.close();
      }
    });
  }

  test("selected and active states are exempt; a false value and a lookalike class are not", { skip: !ENABLED }, async () => {
    const { page, result } = await measure(1440, `
      <div id="states">
        <span class="chip" id="state-a">a</span>
        <span class="chip" id="state-b">b</span>
        <span class="chip" id="state-c">c</span>
        <span class="chip active" id="state-active" style="font-weight: 700">active</span>
        <span class="chip selected" id="state-selected" style="font-weight: 700">selected</span>
        <span class="chip" id="state-aria" aria-selected="true" style="font-weight: 700">aria</span>
        <span class="chip" id="state-pressed" aria-pressed="true" style="font-weight: 700">pressed</span>
        <span class="chip" id="state-current" aria-current="false" style="font-weight: 700">current</span>
      </div>`);
    try {
      assert.equal(result.status, "pass", JSON.stringify(result.measurements.failures));
      const exempt = coverageNamed(result, "state-exempt").find((row) => row.parent === "#states");
      assert.ok(exempt, JSON.stringify(result.measurements.coverage));
      const states = Object.fromEntries(exempt.members.map((member) => [member.selector, member.state]));
      assert.equal(states["#state-active"], "active");
      assert.equal(states["#state-selected"], "selected");
      assert.equal(states["#state-aria"], "aria-selected");
      assert.equal(states["#state-pressed"], "aria-pressed");
      assert.equal(states["#state-current"], "aria-current");
      assert.equal(states["#state-a"], undefined);
      const compared = coverageNamed(result, "compared").find((row) => row.parent === "#states");
      assert.equal(compared.count, 3);
      evidence.states.push({ exempt: exempt.members, compared, status: result.status });
    } finally {
      await page.close();
    }

    const negative = await measure(1440, `
      <div id="false-aria-peers">
        <span class="chip" id="fa-a">a</span>
        <span class="chip" id="fa-b">b</span>
        <span class="chip" id="false-aria" aria-selected="false" style="font-weight: 700">aria</span>
      </div>
      <div id="false-pressed-peers">
        <span class="chip" id="fp-a">a</span>
        <span class="chip" id="fp-b">b</span>
        <span class="chip" id="false-pressed" aria-pressed="false" style="font-weight: 700">pressed</span>
      </div>`);
    try {
      const aria = failureNamed(negative.result, "#false-aria", "font_weight");
      const pressed = failureNamed(negative.result, "#false-pressed", "font_weight");
      assert.ok(aria, JSON.stringify(negative.result.measurements.failures));
      assert.ok(pressed, JSON.stringify(negative.result.measurements.failures));
      assert.equal(aria.value, "700");
      assert.notEqual(aria.sibling_value, "700");
      assert.equal(pressed.property, "font_weight");
      assert.equal(coverageNamed(negative.result, "state-exempt").length, 0);
    } finally {
      await negative.page.close();
    }
  });

  test("an undocumented modifier is ambiguous and does not suppress other groups", { skip: !ENABLED }, async () => {
    const { page, result } = await measure(375, `
      <div id="ambiguous-badges">
        <span class="badge" id="amb-status">running</span>
        <span class="badge promo" id="amb-promo" style="font-weight: 700">promo</span>
        <span class="badge draft" id="amb-draft" style="font-weight: 400">draft</span>
      </div>
      <div id="still-compared">
        <span class="chip" id="still-a">a</span>
        <span class="chip" id="still-b">b</span>
        <span class="chip" id="still-c">c</span>
      </div>
      <div id="lookalikes">
        <span class="chip" id="look-a">a</span>
        <span class="chip" id="look-b">b</span>
        <span class="chip active-triage-chip" id="look-triage" style="font-weight: 700">triage</span>
        <span class="chip on" id="look-on" style="font-weight: 700">on</span>
      </div>`);
    try {
      assert.equal(result.status, "pass", JSON.stringify(result.measurements.failures));
      const ambiguous = coverageNamed(result, "ambiguous");
      const badges = ambiguous.find((row) => row.parent === "#ambiguous-badges");
      const lookalikes = ambiguous.find((row) => row.parent === "#lookalikes");
      assert.ok(badges, JSON.stringify(result.measurements.coverage));
      assert.ok(lookalikes, JSON.stringify(result.measurements.coverage));
      assert.deepEqual(badges.members.map((member) => member.selector).sort(), ["#amb-draft", "#amb-promo", "#amb-status"]);
      assert.equal(lookalikes.members.some((member) => member.selector === "#look-triage"), true);
      assert.equal(lookalikes.members.some((member) => member.selector === "#look-on"), true);
      assert.equal(coverageNamed(result, "state-exempt").some((row) => row.parent === "#lookalikes"), false);
      const compared = coverageNamed(result, "compared").find((row) => row.parent === "#still-compared");
      assert.equal(compared.kind, "chip");
      assert.equal(compared.count, 3);
      assert.equal(result.fingerprint, "ui.sibling-spacing|sibling-spacing-fixture|dark|375|default");
      evidence.ambiguous.push({
        status: result.status,
        coverage: result.measurements.coverage,
        fingerprint: result.fingerprint,
      });
    } finally {
      await page.close();
    }
  });

  test("experiment detail status pill and mode badge pass sibling spacing at 1440 and 375", {
    timeout: 300000,
    skip: !ENABLED,
  }, async () => {
    const manifest = await loadManifest(path.join(ROOT, "crates/orbit-web/visual/views.toml"));
    const targets = expandTargets(rule, [manifest]).filter((target) =>
      target.view === "analytics-experiment-detail" && (target.width === 1440 || target.width === 375));
    assert.equal(targets.length, 2);
    const server = await startFixtureServer(targets[0].appDef, targets[0].viewDef.fixture, ROOT);
    const ctx = await createUiContext({
      repoRoot: ROOT,
      outDir: path.join(ROOT, ".orbit/tmp/sibling-spacing-view"),
    });
    try {
      for (const target of targets) {
        await closeTarget(ctx);
        await ctx.openTarget(target, server.baseUrl, server.placeholders);
        const rendered = await ctx.page.evaluate(() => {
          const root = document.querySelector(".ex-head .ex-badges") ?? document.querySelector(".ex-badges");
          if (!root) return { missing: true, badges: [] };
          return {
            missing: false,
            parentClass: root.className,
            badges: [...root.children].map((el) => {
              const style = getComputedStyle(el);
              return {
                className: el.className,
                dataState: el.dataset.state ?? null,
                text: el.textContent.trim(),
                fontWeight: style.fontWeight,
                fontSize: style.fontSize,
                padding: style.padding,
              };
            }),
          };
        });
        const result = await check(ctx);
        assert.equal(rendered.missing, false, JSON.stringify(rendered));
        // Status renders as the shared .pill and mode as the shared .badge. They
        // are different kinds, so the sibling rule compares neither against the
        // other. The visual fixture is a running experiment with no verdict badge.
        assert.deepEqual(rendered.badges.map((badge) => badge.className), ["pill", "badge"], JSON.stringify(rendered));
        assert.deepEqual(rendered.badges.map((badge) => badge.text), ["running", "shadow"], JSON.stringify(rendered));
        assert.equal(rendered.badges[0].dataState, "running", JSON.stringify(rendered));
        assert.equal(result.status, "pass", JSON.stringify(result.measurements.failures));
        evidence.views.push({
          width: target.width,
          status: result.status,
          fingerprint: result.fingerprint,
          rendered,
          failureCount: (result.measurements.failures ?? []).length,
          failures: result.measurements.failures ?? [],
        });
      }
    } finally {
      await closeTarget(ctx);
      await ctx.close();
      server.stop();
    }
  });
});
