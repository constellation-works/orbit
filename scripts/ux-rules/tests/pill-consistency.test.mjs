// Browser regressions for ui.pill-consistency. Fixture pages use inline CSS so
// the assertions do not depend on dashboard styling; one case loads the shipped
// dashboard CSS with the task row's triaging and repairing pills (the classes
// task-readiness.js emits) and asserts the rule measures all four. Needs the
// browser kit (PLAYWRIGHT_MODULE).
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { fileURLToPath } from "node:url";

import { check } from "../ui/pill-consistency.mjs";

const ENABLED = Boolean(process.env.PLAYWRIGHT_MODULE);
const ROOT = fileURLToPath(new URL("../../..", import.meta.url));
const CSS = ["base.css", "components.css", "tasks.css"].map((name) =>
  fileURLToPath(new URL(`../../../crates/orbit-web/assets/dashboard/css/${name}`, import.meta.url)),
);

const BASE_CSS = `
  body { font: 13px sans-serif; margin: 8px; }
  ul { list-style: none; margin: 0; padding: 0; }
  li { display: flex; gap: 8px; align-items: center; margin: 6px 0; }
  .pill { display: inline-flex; align-items: center; box-sizing: border-box; height: 22px; padding: 0 8px; font-size: 12px; font-weight: 500;
    border: 1px solid #667; border-radius: 999px; white-space: nowrap; }
  .pill.tall { height: 30px; }
  .pill.wide { padding: 0 14px; }
  .pill.big { font-size: 14px; }
  .pill.bold { font-weight: 700; }
  .pill.square { border-radius: 7px; }
  .pill.boxy { border-radius: 2px; }
  .filters { display: flex; gap: 8px; }
  .filters .chip { display: inline-flex; align-items: center; box-sizing: border-box; height: 34px; padding: 0 16px; font-size: 14px;
    font-weight: 600; border: 1px solid #667; border-radius: 999px; background: #223; color: #eee; }
`;

const evidence = { cases: [] };
let browser;

const row = (a, b) => `<li><span class="${a}" id="x-a">triaging</span><span class="${b}" id="x-b">review · repairing</span></li>`;
const list = (a, b) => `<ul id="list">${row(a, b)}${row(a, b)}${row(a, b)}</ul>`;

describe("ui.pill-consistency", { concurrency: false }, () => {
  before(async () => {
    if (!ENABLED) return;
    const mod = await import(process.env.PLAYWRIGHT_MODULE);
    browser = await (mod.default ?? mod).chromium.launch();
  });

  after(async () => {
    await browser?.close();
    const directory = path.join(ROOT, ".orbit/tmp");
    await mkdir(directory, { recursive: true });
    await writeFile(path.join(directory, "pill-consistency-evidence.json"), JSON.stringify(evidence, null, 2));
  });

  async function measure(width, markup, { css = [], style = BASE_CSS } = {}) {
    const page = await browser.newPage({ viewport: { width, height: width === 1440 ? 900 : 812 }, deviceScaleFactor: 1 });
    try {
      await page.setContent("<!doctype html><html><head><meta charset=\"utf-8\"></head><body></body></html>");
      for (const file of css) await page.addStyleTag({ path: file });
      if (style) await page.addStyleTag({ content: style });
      await page.evaluate((html) => {
        document.body.innerHTML = html;
      }, markup);
      const target = { id: `pill-fixture|dark|${width}|default`, view: "pill-fixture", width, theme: "dark", state: "default" };
      return { page, result: await check({ page, target }) };
    } catch (error) {
      await page.close();
      throw error;
    }
  }

  for (const width of [1440, 375]) {
    test(`matching pills in one list pass at ${width}`, { skip: !ENABLED }, async () => {
      const { page, result } = await measure(width, list("pill", "pill"));
      try {
        assert.equal(result.status, "pass", JSON.stringify(result.measurements));
        assert.equal(result.measurements.matched, 6);
      } finally {
        await page.close();
      }
    });

    const cases = [
      ["height", "tall"],
      ["padding-left", "wide"],
      ["padding-right", "wide"],
      ["font-size", "big"],
      ["font-weight", "bold"],
      ["border-radius", "square"],
    ];
    for (const [property, variant] of cases) {
      test(`a pill whose ${property} differs fails at ${width}`, { skip: !ENABLED }, async () => {
        const { page, result } = await measure(width, list("pill", `pill ${variant}`));
        try {
          assert.equal(result.status, "fail", JSON.stringify(result.measurements));
          const hits = result.measurements.failures.filter((item) => item.property === property);
          assert.ok(hits.length > 0, `no ${property} failure: ${JSON.stringify(result.measurements.failures)}`);
          for (const hit of hits) {
            assert.ok(hit.selector && hit.sibling_selector, "a failure names both selectors");
            assert.notEqual(hit.value, hit.sibling_value);
          }
          evidence.cases.push({ width, property, failures: hits.length, sample: hits[0] });
        } finally {
          await page.close();
        }
      });
    }

    test(`a one-pixel height difference is within tolerance at ${width}`, { skip: !ENABLED }, async () => {
      const { page, result } = await measure(width, list("pill", "pill") + `<style>#list li:first-child .pill:last-child { height: 23px; }</style>`);
      try {
        assert.equal(result.status, "pass", JSON.stringify(result.measurements));
      } finally {
        await page.close();
      }
    });

    test(`filter chips are neither grouped with nor failed against status pills at ${width}`, { skip: !ENABLED }, async () => {
      const markup = `
        <div class="filters" role="toolbar" id="filters">
          <button class="chip" aria-pressed="true">All</button>
          <button class="chip" aria-pressed="false">Review</button>
          <span class="chip" id="plain-filter">Blocked</span>
        </div>
        <div class="filters" id="named-filter-bar"><span class="chip">one</span><span class="chip">two</span></div>
        ${list("pill", "pill")}`;
      const { page, result } = await measure(width, markup);
      try {
        assert.equal(result.status, "pass", JSON.stringify(result.measurements));
        assert.equal(result.measurements.matched, 6, "only the six status pills are measured");
      } finally {
        await page.close();
      }
    });

    test(`filter chips alone are skipped, not measured, at ${width}`, { skip: !ENABLED }, async () => {
      const { page, result } = await measure(
        width,
        `<div class="filters"><button class="chip">All</button><button class="chip" style="height:40px;font-size:11px">Review</button></div>`,
      );
      try {
        assert.equal(result.status, "skipped", JSON.stringify(result.measurements));
      } finally {
        await page.close();
      }
    });

    test(`pills of different data-ux-pill kinds are not compared at ${width}`, { skip: !ENABLED }, async () => {
      const markup = `<ul>${[1, 2].map(() => `<li><span class="pill" data-ux-pill="state">alpha</span><span class="pill tall" data-ux-pill="count">bravo</span></li>`).join("")}</ul>`;
      const { page, result } = await measure(width, markup);
      try {
        assert.equal(result.status, "pass", JSON.stringify(result.measurements));
      } finally {
        await page.close();
      }
    });
  }

  test("a square-cornered box is not a pill and is never compared", { skip: !ENABLED }, async () => {
    const { page, result } = await measure(1440, list("pill", "pill boxy"));
    try {
      assert.equal(result.status, "pass", JSON.stringify(result.measurements));
      assert.equal(result.measurements.matched, 3, "only the three round pills are measured");
    } finally {
      await page.close();
    }
  });

  test("pills in one row without a repeated list are compared with each other", { skip: !ENABLED }, async () => {
    const { page, result } = await measure(1440, `<header><span class="pill">alpha</span><span class="pill tall">bravo</span></header>`);
    try {
      assert.equal(result.status, "fail", JSON.stringify(result.measurements));
      assert.ok(result.measurements.failures.some((item) => item.property === "height"));
    } finally {
      await page.close();
    }
  });

  test("shipped CSS: records the triaging and repairing pills of one task list", { skip: !ENABLED }, async () => {
    const task = (inner) => `<li class="row" style="display:flex;gap:8px">${inner}</li>`;
    const markup = `<ul class="rows">
      ${task(`<span class="title-tags"><a class="live-state-pill active-triage-chip" href="#r1">triaging</a><span class="live-state-pill delivery-attention-badge repairing" role="img">review · repairing 1/3</span></span>`)}
      ${task(`<span class="title-tags"><a class="live-state-pill active-triage-chip" href="#r2">triaging</a><span class="live-state-pill delivery-attention-badge repairing" role="img">review · repairing 2/3</span></span>`)}
    </ul>`;
    const { page, result } = await measure(1440, markup, { css: CSS, style: "" });
    try {
      evidence.cases.push({ shipped_css: true, status: result.status, failures: result.measurements.failures ?? [] });
      assert.ok(["pass", "fail"].includes(result.status), JSON.stringify(result.measurements));
      assert.equal(result.measurements.matched, 4, `expected both shipped pill kinds to be measured: ${JSON.stringify(result.measurements)}`);
    } finally {
      await page.close();
    }
  });
});
