// Browser regressions for ui.aligned-starts. The production fixtures load the
// shipped dashboard stylesheets, so the Doctor, task-chip and Incidents
// measurements are the real rules rather than a copy. Needs the browser kit
// (PLAYWRIGHT_MODULE from its env.sh).
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { after, before, describe, test } from "node:test";
import { fileURLToPath } from "node:url";

import { check } from "../ui/aligned-starts.mjs";

const DASHBOARD_CSS = ["base.css", "shell.css", "components.css", "health.css", "task-detail.css", "tasks.css"].map(
  (name) => fileURLToPath(new URL(`../../../crates/orbit-web/assets/dashboard/css/${name}`, import.meta.url)),
);
const ENABLED = Boolean(process.env.PLAYWRIGHT_MODULE);

const DOCTOR = `
<section id="doctor-375" style="width:343px">
  <div class="doctor-remediation">
    <span class="doctor-remediation-label">Fix</span>
    <span class="doctor-remediation-text">Run <span class="doctor-command"><code>orbit workspace repair --apply --root /home/daniel/workspace/constellation</code><button class="doctor-copy" type="button">copy</button></span></span>
  </div>
</section>`;

const CHIPS = `
<div class="task-levers" id="task-chips" style="width:900px">
  <span class="status-cell">
    <select class="task-status-select mono" aria-label="Update status"><option>in-progress</option></select>
  </span>
  <div class="complexity-cell">
    <select class="task-complexity-select mono" aria-label="Update complexity"><option>medium</option></select>
  </div>
  <span class="crew-cell task-chip-crew">
    <span class="task-chip task-crew-control"><label>Crew</label><select class="task-crew-select mono" aria-label="Update crew"><option>grok</option></select></span>
  </span>
  <span class="task-chip task-chip-host" title="Runs on">
    <span class="visually-hidden">Runs on: </span>
    <span class="value"><span class="task-os-readout">Linux</span><span class="info-hint" tabindex="0"><span class="info-hint-text">any host from OS tags</span></span></span>
  </span>
  <span class="task-risk-hint">
    <button class="task-chip task-chip-risk" type="button"><span>Risk</span><span class="risk-value">12%</span></button>
    <span class="task-risk-tooltip" role="tooltip">Fitted on size</span>
  </span>
  <div class="task-levers-tags">
    <div class="field-editor-cell">
      <div class="field-editor">
        <div class="detail-tag-row"><span class="chip">ux-rules</span><span class="chip">ui</span></div>
        <button class="field-edit" type="button">edit</button>
      </div>
    </div>
  </div>
</div>`;

// Title width forces the count onto the next line while the toolbar columns
// stay on one row, which is the incidents false measurement.
const INCIDENTS = `
<div class="incident-toolbar" id="incidents-toolbar" style="width:1100px">
  <h3 class="incident-list-title" style="max-width:112px">Unexpected<span class="incident-list-count"> 12 signatures of 20 in 4 causes</span></h3>
  <div class="incident-search-wrap">
    <label class="visually-hidden" for="incident-search">Filter incidents</label>
    <input class="incident-search" id="incident-search" type="search" placeholder="Filter by message" aria-label="Filter incidents">
  </div>
  <div class="incident-windows incident-modes" role="group">
    <button class="incident-seg" type="button">Cause</button>
    <button class="incident-seg" type="button">Signature</button>
  </div>
</div>`;

let browser;
const geometry = {};

// The three production fixtures share one geometry record, so they run in order.
describe("ui.aligned-starts", { concurrency: false }, () => {
before(async () => {
  if (!ENABLED) return;
  const mod = await import(process.env.PLAYWRIGHT_MODULE);
  browser = await (mod.default ?? mod).chromium.launch();
});

after(async () => {
  await browser?.close();
});

function targetFor(view, width) {
  return { id: `${view}|dark|${width}|default`, view, width, theme: "dark", state: "default" };
}

async function measure(width, markup, { css = [], prepare } = {}) {
  const page = await browser.newPage({ viewport: { width, height: 812 } });
  try {
    await page.setContent(`<!doctype html><html><head><meta charset="utf-8"></head><body>${markup}</body></html>`);
    for (const path of css) await page.addStyleTag({ path });
    if (prepare) await prepare(page);
    const result = await check({ page, target: targetFor("aligned-starts-fixture", width) });
    return { page, result };
  } catch (error) {
    await page.close();
    throw error;
  }
}

function anchorsOf(result) {
  return (result.measurements.comparisons ?? []).flatMap((row) => row.anchors ?? []);
}

function rowOf(result, parentIncludes) {
  return (result.measurements.comparisons ?? []).find((row) => String(row.parent).includes(parentIncludes));
}

test("doctor own text before the wrapped command is not a false start", { skip: !ENABLED }, async () => {
  const { page, result } = await measure(375, DOCTOR, { css: DASHBOARD_CSS });
  try {
    const commandTop = await page.evaluate(() => Math.round(document.querySelector(".doctor-command").getBoundingClientRect().top));
    const row = rowOf(result, "doctor-remediation");
    assert.ok(row, JSON.stringify(result.measurements));
    const fix = row.anchors.find((anchor) => anchor.text === "Fix");
    const run = row.anchors.find((anchor) => anchor.text.startsWith("Run"));
    assert.ok(fix, JSON.stringify(row));
    assert.ok(run, JSON.stringify(row));
    assert.equal(fix.kind, "text");
    assert.equal(run.kind, "text");
    assert.ok(Math.abs(fix.anchor_top_px - run.anchor_top_px) <= 8, JSON.stringify(row));
    assert.ok(commandTop - fix.anchor_top_px > 8, JSON.stringify({ commandTop, fix }));
    assert.equal(row.anchors.some((anchor) => anchor.selector.includes("doctor-command")), false, JSON.stringify(row));
    assert.equal(result.status, "pass", JSON.stringify(result.measurements.failures));
    geometry.doctor375 = { status: result.status, command_top_px: commandTop, row };
  } finally {
    await page.close();
  }
});

test("task chips compare visible text and controls, not the hidden label", { skip: !ENABLED }, async () => {
  const { page, result } = await measure(1440, CHIPS, { css: DASHBOARD_CSS });
  try {
    const legacy = await page.evaluate(() => {
      const visible = (el) => {
        const style = getComputedStyle(el);
        const rect = el.getBoundingClientRect();
        return style.display !== "none" && style.visibility !== "hidden" && rect.width > 0 && rect.height > 0;
      };
      const row = document.querySelector("#task-chips");
      return [...row.children].filter(visible).map((child) => {
        const first = [...child.children].find(visible) ?? child;
        const rect = first.getBoundingClientRect();
        return {
          column: child.className,
          selector: first.className || first.tagName,
          top_px: Math.round(rect.top),
          text: first.textContent.trim().replace(/\s+/g, " ").slice(0, 40),
        };
      });
    });
    const linux = anchorsOf(result).find((anchor) => anchor.text === "Linux");
    const host = legacy.find((item) => item.column.includes("task-chip-host"));
    const legacySpread = Math.max(...legacy.map((item) => item.top_px)) - Math.min(...legacy.map((item) => item.top_px));
    assert.ok(linux, JSON.stringify(result.measurements));
    assert.equal(anchorsOf(result).some((anchor) => anchor.text.startsWith("Runs on")), false, JSON.stringify(anchorsOf(result)));
    assert.match(host.text, /Runs on/);
    assert.ok(legacySpread > 8, JSON.stringify(legacy));
    assert.equal(result.status, "pass", JSON.stringify(result.measurements.failures));
    geometry.taskChips1440 = {
      status: result.status,
      legacy_first_element: legacy,
      legacy_spread_px: legacySpread,
      anchors: anchorsOf(result),
      incomparable: result.measurements.incomparable,
    };
  } finally {
    await page.close();
  }
});

test("incidents toolbar does not measure the wrapped count or the hidden label", { skip: !ENABLED }, async () => {
  const { page, result } = await measure(1440, INCIDENTS, { css: DASHBOARD_CSS });
  try {
    const probe = await page.evaluate(() => {
      const title = document.querySelector(".incident-list-title");
      const range = document.createRange();
      range.setStart(title.firstChild, 0);
      range.setEnd(title.firstChild, title.firstChild.textContent.length);
      const button = document.querySelector(".incident-seg");
      const buttonRange = document.createRange();
      buttonRange.selectNodeContents(button);
      return {
        own_top_px: Math.round(range.getClientRects()[0].top),
        count_top_px: Math.round(document.querySelector(".incident-list-count").getBoundingClientRect().top),
        button_text_top_px: Math.round(buttonRange.getClientRects()[0].top),
        label_top_px: Math.round(document.querySelector(".incident-search-wrap .visually-hidden").getBoundingClientRect().top),
      };
    });
    const row = rowOf(result, "incidents-toolbar");
    assert.ok(row, JSON.stringify(result.measurements));
    assert.equal(row.anchors.some((anchor) => anchor.text.includes("signatures")), false, JSON.stringify(row));
    assert.equal(row.anchors.some((anchor) => anchor.text === "Unexpected"), false, JSON.stringify(row));
    assert.equal(row.anchors.some((anchor) => anchor.text === "Filter incidents"), false, JSON.stringify(row));
    assert.ok(
      result.measurements.incomparable.some(
        (item) => item.text === "Unexpected" && item.top_px === probe.own_top_px && item.reason === "no comparable peer",
      ),
      JSON.stringify(result.measurements.incomparable),
    );
    assert.ok(probe.count_top_px - probe.own_top_px > 8, JSON.stringify(probe));
    assert.ok(probe.count_top_px - probe.label_top_px > 8, JSON.stringify(probe));
    const controls = row.anchors.filter((anchor) => anchor.kind === "control");
    assert.ok(controls.length >= 2, JSON.stringify(row));
    const tops = controls.map((anchor) => anchor.anchor_top_px);
    assert.ok(Math.max(...tops) - Math.min(...tops) <= 8, JSON.stringify(row));
    assert.equal(result.status, "pass", JSON.stringify(result.measurements.failures));
    geometry.incidentsToolbar1440 = { status: result.status, probe, row, incomparable: result.measurements.incomparable };
    await mkdir(fileURLToPath(new URL("../../../.orbit/tmp/", import.meta.url)), { recursive: true });
    await writeFile(
      fileURLToPath(new URL("../../../.orbit/tmp/aligned-starts-geometry.json", import.meta.url)),
      JSON.stringify(geometry, null, 2),
    );
  } finally {
    await page.close();
  }
});

test("own text before a nested inline and a multiline first line stay aligned", { skip: !ENABLED }, async () => {
  const { page, result } = await measure(800, `
    <style>
      body { margin: 0; font: 16px/1.4 sans-serif; }
      .row { display: flex; align-items: flex-start; gap: 16px; width: 360px; }
      .narrow { width: 70px; }
      #run { width: 3ch; }
    </style>
    <div class="row" id="own-text">
      <div id="fix">Fix</div>
      <div class="narrow" id="run">Run <em id="command">the long command</em></div>
    </div>
    <div class="row" id="multiline">
      <div id="start">Start</div>
      <div class="narrow" id="multi">One two three four</div>
    </div>`);
  try {
    assert.equal(result.status, "pass", JSON.stringify(result.measurements.failures));
    const own = rowOf(result, "own-text");
    const fix = own.anchors.find((anchor) => anchor.selector === "#fix");
    const run = own.anchors.find((anchor) => anchor.selector === "#run");
    assert.equal(run.text, "Run");
    assert.ok(Math.abs(fix.anchor_top_px - run.anchor_top_px) <= 8, JSON.stringify(own));
    const commandLines = await page.evaluate(() => {
      const node = document.querySelector("#command").firstChild;
      const range = document.createRange();
      range.selectNodeContents(node);
      return [...range.getClientRects()].map((rect) => Math.round(rect.top));
    });
    assert.ok(commandLines.length >= 1, JSON.stringify(commandLines));
    assert.ok(Math.min(...commandLines) - run.anchor_top_px > 8, JSON.stringify({ commandLines, run }));
    const lines = await page.evaluate(() => {
      const node = document.querySelector("#multi").firstChild;
      const range = document.createRange();
      range.selectNodeContents(node);
      return [...range.getClientRects()].map((rect) => Math.round(rect.top));
    });
    assert.ok(lines.length >= 2, JSON.stringify(lines));
    const multi = anchorsOf(result).find((anchor) => anchor.selector === "#multi");
    assert.equal(multi.anchor_top_px, Math.min(...lines), JSON.stringify({ multi, lines }));
    assert.ok(Math.max(...lines) - multi.anchor_top_px > 8, JSON.stringify(lines));
  } finally {
    await page.close();
  }
});

test("nested wrappers and aligned controls share a visible start", { skip: !ENABLED }, async () => {
  const { page, result } = await measure(800, `
    <style>body { margin: 0; font: 16px/1.2 sans-serif; } .row { display: flex; align-items: center; gap: 12px; }</style>
    <div class="row" id="wrapped">
      <div><div><span id="inner">Same</span></div></div>
      <div><span id="direct">Same</span></div>
    </div>
    <div class="row" id="controls">
      <button id="save" style="height:28px">Save</button>
      <button id="edit" style="height:28px">Edit</button>
      <select id="pick" style="height:28px" aria-label="Edit"><option>Edit</option></select>
    </div>`);
  try {
    assert.equal(result.status, "pass", JSON.stringify(result.measurements.failures));
    const wrapped = rowOf(result, "wrapped");
    const inner = wrapped.anchors.find((anchor) => anchor.selector === "#inner");
    const direct = wrapped.anchors.find((anchor) => anchor.selector === "#direct");
    assert.ok(inner && direct, JSON.stringify(wrapped));
    assert.ok(Math.abs(inner.anchor_top_px - direct.anchor_top_px) <= 8, JSON.stringify(wrapped));
    const controls = rowOf(result, "controls").anchors;
    assert.equal(controls.length, 3, JSON.stringify(controls));
    assert.ok(controls.every((anchor) => anchor.kind === "control"), JSON.stringify(controls));
    const tops = controls.map((anchor) => anchor.anchor_top_px);
    assert.ok(Math.max(...tops) - Math.min(...tops) <= 8, JSON.stringify(controls));
    assert.equal(page.url() !== "", true);
  } finally {
    await page.close();
  }
});

test("a displaced text start names its anchor, peer and delta", { skip: !ENABLED }, async () => {
  const { page, result } = await measure(800, `
    <style>body { margin: 0; font: 16px/1.2 sans-serif; } .row { display: flex; align-items: flex-start; gap: 12px; }</style>
    <div class="row" id="displaced">
      <div><span id="early">Early</span></div>
      <div style="padding-top:24px"><span id="late">Late</span></div>
    </div>`);
  try {
    assert.equal(result.status, "fail", JSON.stringify(result.measurements));
    const failure = result.measurements.failures.find((item) => item.anchor_selector === "#late");
    assert.ok(failure, JSON.stringify(result.measurements.failures));
    assert.equal(failure.peer_selector, "#early");
    assert.equal(failure.text, "Late");
    assert.equal(failure.peer_text, "Early");
    assert.equal(failure.kind, "text");
    assert.ok(failure.delta_px > 8 && failure.delta_px < 40, JSON.stringify(failure));
    assert.equal(failure.evidence_selector, '[data-ux-evidence-target="ui.aligned-starts-0"]');
    const stamped = await page.locator(failure.evidence_selector).count();
    assert.equal(stamped, 1);
    assert.equal(result.fingerprint, "ui.aligned-starts|aligned-starts-fixture|dark|800|default");
  } finally {
    await page.close();
  }
});

const MULTILINE_FIELDS = `
<style>
  body { margin: 0; font: 16px/20px sans-serif; }
  .row { display: flex; align-items: flex-start; gap: 12px; }
  button, textarea, input, select {
    font: 16px/20px sans-serif;
    box-sizing: border-box;
    margin: 0;
    padding: 2px;
    border: 1px solid #000;
  }
</style>`;

const SCROLLED_VALUE = "One\nTwo\nThree\nFour\nFive\nSix\nSeven";

test("a tall textarea uses its first line, and a displaced control still fails", { skip: !ENABLED }, async () => {
  const aligned = await measure(800, `${MULTILINE_FIELDS}
    <div class="row" id="tall">
      <textarea id="area" style="height:110px;width:120px;resize:none">Same</textarea>
      <textarea id="taller" style="height:180px;width:120px;resize:none">Same</textarea>
      <button id="button" type="button" style="height:30px">Same</button>
      <select id="list" size="4" style="height:110px"><option>Same</option><option>Two</option><option>Three</option><option>Four</option></select>
      <select id="multi" multiple style="height:110px"><option>Same</option><option>Two</option><option>Three</option><option>Four</option></select>
    </div>
    <div class="row" id="scrolled-pair">
      <textarea id="scrolled-a" style="height:80px;width:120px;resize:none">${SCROLLED_VALUE}</textarea>
      <textarea id="scrolled-b" style="height:80px;width:120px;resize:none">${SCROLLED_VALUE}</textarea>
    </div>
    <div class="row" id="unmeasured">
      <textarea id="zero" style="height:80px;width:120px;font-size:0;line-height:0;resize:none">Same</textarea>
      <button id="zero-button" type="button" style="height:30px">Same</button>
    </div>`, {
    prepare: async (page) => {
      await page.evaluate(() => {
        for (const id of ["#scrolled-a", "#scrolled-b"]) document.querySelector(id).scrollTop = 25;
      });
    },
  });
  try {
    const geometry = await aligned.page.evaluate(() => {
      const contentTop = (el) => {
        const style = getComputedStyle(el);
        const rect = el.getBoundingClientRect();
        return rect.top + (parseFloat(style.borderTopWidth) || 0) + (parseFloat(style.paddingTop) || 0);
      };
      const centered = (el) => {
        const style = getComputedStyle(el);
        const rect = el.getBoundingClientRect();
        const borderTop = parseFloat(style.borderTopWidth) || 0;
        const borderBottom = parseFloat(style.borderBottomWidth) || 0;
        const padTop = parseFloat(style.paddingTop) || 0;
        const padBottom = parseFloat(style.paddingBottom) || 0;
        const top = rect.top + borderTop + padTop;
        const contentHeight = rect.height - borderTop - borderBottom - padTop - padBottom;
        const fontSize = parseFloat(style.fontSize) || 16;
        let line = fontSize * 1.2;
        if (style.lineHeight.endsWith("px")) line = parseFloat(style.lineHeight);
        return top + Math.max(0, (contentHeight - line) / 2);
      };
      const area = document.querySelector("#area");
      const scrolled = document.querySelector("#scrolled-a");
      const list = document.querySelector("#list");
      return {
        areaContentTop: contentTop(area),
        areaCentered: centered(area),
        tallerContentTop: contentTop(document.querySelector("#taller")),
        optionTop: list.querySelector("option").getBoundingClientRect().top,
        multiOptionTop: document.querySelector("#multi option").getBoundingClientRect().top,
        listCentered: centered(list),
        scrolledTop: contentTop(scrolled) - scrolled.scrollTop,
        scrolledContentTop: contentTop(scrolled),
        scrollTop: scrolled.scrollTop,
      };
    });
    assert.equal(aligned.result.status, "pass", JSON.stringify(aligned.result.measurements.failures));
    const tall = rowOf(aligned.result, "tall").anchors;
    const area = tall.find((anchor) => anchor.selector === "#area");
    const taller = tall.find((anchor) => anchor.selector === "#taller");
    const button = tall.find((anchor) => anchor.selector === "#button");
    const list = tall.find((anchor) => anchor.selector === "#list");
    const multi = tall.find((anchor) => anchor.selector === "#multi");
    assert.ok(area && taller && button && list && multi, JSON.stringify(tall));
    assert.equal(area.anchor_top_px, taller.anchor_top_px, JSON.stringify(tall));
    assert.ok(Math.abs(area.anchor_top_px - Math.round(geometry.areaContentTop)) <= 1, JSON.stringify({ area, geometry }));
    assert.ok(Math.abs(area.anchor_top_px - Math.round(geometry.tallerContentTop)) <= 1, JSON.stringify({ area, geometry }));
    assert.ok(geometry.areaCentered - area.anchor_top_px > 20, JSON.stringify({ area, geometry }));
    assert.ok(Math.abs(button.anchor_top_px - area.anchor_top_px) <= 8, JSON.stringify(tall));
    assert.ok(Math.abs(list.anchor_top_px - Math.round(geometry.optionTop)) <= 1, JSON.stringify({ list, geometry }));
    assert.ok(Math.abs(multi.anchor_top_px - Math.round(geometry.multiOptionTop)) <= 1, JSON.stringify({ multi, geometry }));
    assert.ok(geometry.listCentered - list.anchor_top_px > 20, JSON.stringify({ list, geometry }));
    const scrolled = rowOf(aligned.result, "scrolled-pair").anchors;
    assert.equal(scrolled.length, 2, JSON.stringify(scrolled));
    assert.ok(scrolled.every((anchor) => anchor.anchor_top_px === Math.round(geometry.scrolledTop)), JSON.stringify({ scrolled, geometry }));
    assert.ok(geometry.scrolledContentTop - geometry.scrolledTop >= 20, JSON.stringify(geometry));
    assert.equal(geometry.scrollTop, 25);
    const zero = aligned.result.measurements.incomparable.find((item) => item.selector === "#zero" || item.column_selector === "#zero");
    assert.ok(zero, JSON.stringify(aligned.result.measurements.incomparable));
    assert.equal(zero.reason, "content geometry unavailable");
  } finally {
    await aligned.page.close();
  }

  const shifted = await measure(800, `${MULTILINE_FIELDS}
    <div class="row" id="scroll-shift">
      <textarea id="still" style="height:80px;width:120px;resize:none">${SCROLLED_VALUE}</textarea>
      <textarea id="moved" style="height:80px;width:120px;resize:none">${SCROLLED_VALUE}</textarea>
    </div>`, {
    prepare: async (page) => {
      await page.evaluate(() => {
        document.querySelector("#moved").scrollTop = 25;
      });
    },
  });
  try {
    assert.equal(shifted.result.status, "fail", JSON.stringify(shifted.result.measurements));
    const failure = shifted.result.measurements.failures.find((item) => item.anchor_selector === "#still");
    assert.ok(failure, JSON.stringify(shifted.result.measurements.failures));
    assert.equal(failure.peer_selector, "#moved");
    assert.equal(failure.kind, "control");
    assert.ok(failure.delta_px > 8, JSON.stringify(failure));
  } finally {
    await shifted.page.close();
  }

  const displaced = await measure(800, `${MULTILINE_FIELDS}
    <div class="row" id="pushed">
      <div><textarea id="early-area" style="height:110px;width:120px;resize:none">Same</textarea></div>
      <div style="padding-top:24px"><textarea id="late-area" style="height:110px;width:120px;resize:none">Same</textarea></div>
    </div>`);
  try {
    assert.equal(displaced.result.status, "fail", JSON.stringify(displaced.result.measurements));
    const failure = displaced.result.measurements.failures.find((item) => item.anchor_selector === "#late-area");
    assert.ok(failure, JSON.stringify(displaced.result.measurements.failures));
    assert.equal(failure.peer_selector, "#early-area");
    assert.equal(failure.kind, "control");
    assert.ok(failure.delta_px > 8, JSON.stringify(failure));
  } finally {
    await displaced.page.close();
  }

  const single = await measure(800, `${MULTILINE_FIELDS}
    <div class="row" id="single">
      <input id="field" style="height:48px;width:120px" value="Same">
      <select id="closed" style="height:48px"><option>Same</option></select>
      <button id="single-button" type="button" style="height:48px">Same</button>
    </div>`);
  try {
    const tops = await single.page.evaluate(() => {
      const contentTop = (el) => {
        const style = getComputedStyle(el);
        const rect = el.getBoundingClientRect();
        return rect.top + (parseFloat(style.borderTopWidth) || 0) + (parseFloat(style.paddingTop) || 0);
      };
      return {
        field: contentTop(document.querySelector("#field")),
        closed: contentTop(document.querySelector("#closed")),
      };
    });
    assert.equal(single.result.status, "pass", JSON.stringify(single.result.measurements.failures));
    const row = rowOf(single.result, "single").anchors;
    const field = row.find((anchor) => anchor.selector === "#field");
    const closed = row.find((anchor) => anchor.selector === "#closed");
    const button = row.find((anchor) => anchor.selector === "#single-button");
    assert.ok(field && closed && button, JSON.stringify(row));
    assert.ok(field.anchor_top_px - tops.field > 8, JSON.stringify({ field, tops }));
    assert.ok(closed.anchor_top_px - tops.closed > 8, JSON.stringify({ closed, tops }));
    assert.ok(Math.abs(field.anchor_top_px - button.anchor_top_px) <= 8, JSON.stringify(row));
    assert.ok(Math.abs(closed.anchor_top_px - button.anchor_top_px) <= 8, JSON.stringify(row));
  } finally {
    await single.page.close();
  }
});

test("a displaced control start is still detected", { skip: !ENABLED }, async () => {
  const { page, result } = await measure(800, `
    <style>body { margin: 0; font: 16px/1.2 sans-serif; } .row { display: flex; align-items: flex-start; gap: 12px; }</style>
    <div class="row">
      <div><button id="early-control">Early</button></div>
      <div style="padding-top:24px"><button id="late-control">Late</button></div>
    </div>`);
  try {
    assert.equal(result.status, "fail", JSON.stringify(result.measurements));
    const failure = result.measurements.failures.find((item) => item.anchor_selector === "#late-control");
    assert.ok(failure, JSON.stringify(result.measurements.failures));
    assert.equal(failure.peer_selector, "#early-control");
    assert.equal(failure.kind, "control");
    assert.ok(failure.delta_px > 8, JSON.stringify(failure));
    assert.equal(page.url() !== "", true);
  } finally {
    await page.close();
  }
});

test("incomparable anchors are not a pass", { skip: !ENABLED }, async () => {
  const empty = await measure(800, `
    <style>
      body { margin: 0; font: 16px/1.2 sans-serif; }
      .row { display: flex; align-items: flex-start; gap: 12px; }
      .visually-hidden { position: absolute; width: 1px; height: 1px; margin: -1px; overflow: hidden; clip-path: inset(50%); }
    </style>
    <div class="row" id="empty-row">
      <div id="blank" style="min-width:40px; min-height:20px"></div>
      <div id="assist" style="min-width:40px; min-height:20px"><span class="visually-hidden">Hidden</span></div>
    </div>`);
  try {
    assert.equal(empty.result.status, "skipped", JSON.stringify(empty.result.measurements));
    assert.equal(empty.result.measurements.matched, 0);
    assert.equal(empty.result.measurements.reason, "no comparable content starts");
    assert.ok(empty.result.measurements.incomparable.length >= 2, JSON.stringify(empty.result.measurements.incomparable));
    assert.ok(empty.result.measurements.incomparable.every((item) => item.reason === "no comparable rendered content"));
  } finally {
    await empty.page.close();
  }

  const mixed = await measure(800, `
    <style>body { margin: 0; font: 16px/1.2 sans-serif; } .row { display: flex; align-items: flex-start; gap: 12px; }</style>
    <div class="row" id="mixed">
      <div id="left">Left</div>
      <div id="right">Right</div>
      <div id="empty" style="min-width:40px; min-height:20px"></div>
    </div>
    <div class="row" id="unlike">
      <div id="label">Label</div>
      <button id="go">Go</button>
    </div>`);
  try {
    assert.equal(mixed.result.status, "pass", JSON.stringify(mixed.result.measurements.failures));
    const texts = rowOf(mixed.result, "mixed").anchors;
    assert.equal(texts.length, 2, JSON.stringify(texts));
    assert.equal(mixed.result.measurements.matched, 2, JSON.stringify(mixed.result.measurements));
    assert.ok(
      mixed.result.measurements.incomparable.some((item) => item.column_selector === "#empty" && item.reason === "no comparable rendered content"),
      JSON.stringify(mixed.result.measurements.incomparable),
    );
    assert.ok(
      mixed.result.measurements.incomparable.some((item) => item.selector === "#label" && item.reason === "no comparable peer"),
      JSON.stringify(mixed.result.measurements.incomparable),
    );
    assert.ok(
      mixed.result.measurements.incomparable.some((item) => item.selector === "#go" && item.reason === "no comparable peer"),
      JSON.stringify(mixed.result.measurements.incomparable),
    );
  } finally {
    await mixed.page.close();
  }

  const unavailable = await measure(800, `
    <style>body { margin: 0; font: 16px/1.2 sans-serif; } .row { display: flex; align-items: flex-start; gap: 12px; }</style>
    <div class="row" id="missing-geometry">
      <div id="gone" style="font-size:0; line-height:0; width:40px; height:20px">Gone</div>
      <div id="here">Here</div>
    </div>`);
  try {
    assert.notEqual(unavailable.result.status, "pass", JSON.stringify(unavailable.result.measurements));
    assert.equal(unavailable.result.measurements.matched, 0);
    const gone = unavailable.result.measurements.incomparable.find((item) => item.column_selector === "#gone" || item.selector === "#gone");
    assert.ok(gone, JSON.stringify(unavailable.result.measurements.incomparable));
    assert.equal(gone.reason, "content geometry unavailable");
  } finally {
    await unavailable.page.close();
  }
});
});
