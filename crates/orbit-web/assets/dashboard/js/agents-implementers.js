// Analytics › Agents › Implementers: the role panel, escaped-regression rate
// against landed change size. Reads GET /api/agents/implementers/size-curves
// for the page's window and the panel's complexity filter; the server slices
// the same cached scan as the scorecard and computes every rate and Wilson
// interval. One series per crew reference as dispatched, so `opus@medium`
// and `opus@high` are separate curves. The crew picker starts on the series
// the server marks `default_selected` (enough tasks in the window) and keeps
// the reader's choice across polls. Buckets under the minimum sample are not
// drawn; sparse-outcome buckets get hollow markers. Display only: nothing
// here enters a score. Nodes use `el()`, the chart inline SVG.

import { el, fetchJson, getWorkspaceRevision } from './common.js';

const TIER_LABELS = { low: "Low", medium: "Medium", hard: "Hard", xhard: "X-hard", unassessed: "Unassessed" };
// Series colours by position in the window's series list, so toggling a
// crew never recolours another. Twenty slots cover the window's series
// without reuse; CSS defines .agents-curve-s0..s19 to match.
const SERIES_SLOTS = 20;
const SVG_NS = "http://www.w3.org/2000/svg";

let complexity = "";
// `null` until the reader picks: then the crews they chose.
let picked = null;
let curves = null;
let curvesScope = null;
let curvesRequest = 0;
function curvesPath(selectedWindow, execution) {
  const tier = complexity ? `&complexity=${encodeURIComponent(complexity)}` : "";
  return `/api/agents/implementers/size-curves?window=${encodeURIComponent(selectedWindow)}${tier}&execution=${encodeURIComponent(execution || "managed")}`;
}

/// Read the curves for `selectedWindow` and `execution` under the current
/// filter. A response for a window, filter or workspace no longer shown is
/// dropped, as is a superseded read of the same scope.
function loadCurves(selectedWindow, execution) {
  const request = ++curvesRequest;
  const path = curvesPath(selectedWindow, execution);
  const scope = `${getWorkspaceRevision()}\0${path}`;
  if (curvesScope !== scope) {
    curvesScope = scope;
    curves = null;
  }
  return fetchJson(path).then((payload) => {
    if (request !== curvesRequest || curvesScope !== scope) return;
    curves = payload && payload.requested_window === selectedWindow && Array.isArray(payload.series) ? payload : null;
  }, () => {
    // A failed poll keeps curves this scope already showed; only a scope with
    // nothing loaded reports the read as unavailable.
    if (request === curvesRequest && curvesScope === scope && (!curves || curves.failed)) curves = { failed: true };
  });
}

function svg(tag, attrs = {}, text) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [name, value] of Object.entries(attrs)) node.setAttribute(name, String(value));
  if (text != null) node.textContent = text;
  return node;
}

function percent(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  const points = value * 100;
  return `${points > 0 && points < 10 ? points.toFixed(1) : points.toFixed(0)}%`;
}

// The smallest round step that splits `top` into at most six intervals.
function tickStep(top) {
  return [0.01, 0.02, 0.05, 0.1, 0.2, 0.25].find((step) => top / step <= 6) || 0.25;
}

// One format for every x tick: the plain bucket-start count, never a mix
// of plain and abbreviated labels (511 beside 2k).
function compactLines(value) {
  return String(value);
}

function markerRadius(bucket) {
  const tasks = typeof bucket.tasks === "number" ? bucket.tasks : 0;
  return 2.4 + Math.min(2.6, Math.log10(tasks + 1) * 1.3);
}

// Rows of `text` whose rendered width stays within `maxWidth`. A space breaks
// first; a crew reference with no space breaks mid-token so the bubble can
// stay inside the plot (STD-07 §R4, §R32). `measure` is the rendered width.
function wrapTipLine(text, maxWidth, measure) {
  if (!text) return [];
  if (!(maxWidth > 0) || measure(text) <= maxWidth) return [text];
  const lines = [];
  let rest = text;
  while (rest) {
    if (measure(rest) <= maxWidth) {
      lines.push(rest);
      break;
    }
    let best = 1;
    let lo = 1;
    let hi = rest.length - 1;
    while (lo <= hi) {
      const mid = Math.floor((lo + hi) / 2);
      if (measure(rest.slice(0, mid)) <= maxWidth) {
        best = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }
    const space = rest.lastIndexOf(" ", best);
    const cut = space > 0 ? space : best;
    lines.push(rest.slice(0, cut).trimEnd());
    const next = rest.slice(cut).trimStart();
    if (next === rest) break;
    rest = next;
  }
  return lines;
}

function pointLabel(crew, bucket) {
  return `${crew}, ${rangeText(bucket)}: ${bucket.regressed} of ${bucket.tasks} tasks regressed (n=${bucket.tasks}), ${percent(bucket.rate)} (95% ${percent(bucket.low)}–${percent(bucket.high)})${bucket.sparse ? `; fewer than ${curves.sparse_outcomes} regressed tasks` : ""}`;
}

function rangeText(bucket) {
  return bucket.lines_from === bucket.lines_to
    ? `${bucket.lines_from} lines`
    : `${bucket.lines_from}–${bucket.lines_to} lines`;
}

function plural(n, one, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}

function series() {
  return curves && Array.isArray(curves.series) ? curves.series : [];
}

function selectedCrews() {
  const all = series();
  if (picked) return new Set(all.filter((each) => picked.has(each.crew)).map((each) => each.crew));
  return new Set(all.filter((each) => each.default_selected).map((each) => each.crew));
}

function slot(crew) {
  return series().findIndex((each) => each.crew === crew) % SERIES_SLOTS;
}

function toggle(crew, on, rerender) {
  const next = selectedCrews();
  if (on) next.add(crew);
  else next.delete(crew);
  picked = next;
  rerender();
}

function controlsNode(selectedWindow, execution, rerender) {
  const select = el("select", { id: "agents-curves-tier", "aria-label": "Complexity" });
  const counts = new Map((curves && Array.isArray(curves.complexities) ? curves.complexities : []).map((entry) => [entry.tier, entry.tasks]));
  const total = [...counts.values()].reduce((sum, n) => sum + n, 0);
  for (const [value, text] of [["", "All complexities"], ...Object.entries(TIER_LABELS)]) {
    const n = value ? counts.get(value) : total;
    const option = el("option", { text: curves && !curves.failed ? `${text} (${n ?? 0})` : text });
    option.value = value;
    select.append(option);
  }
  select.value = complexity;
  select.addEventListener("change", () => {
    complexity = select.value;
    loadCurves(selectedWindow, execution).then(rerender);
    rerender();
  });
  const label = el("label", { text: "Complexity" });
  label.htmlFor = "agents-curves-tier";
  return el("div", { class: "agents-curves-control" }, [label, select]);
}

function pickerNode(rerender) {
  const chosen = selectedCrews();
  const list = el("ul", { class: "agents-curves-picker", "aria-label": "Crews drawn" });
  for (const each of series()) {
    const box = el("input", { type: "checkbox" });
    box.checked = chosen.has(each.crew);
    box.addEventListener("change", () => toggle(each.crew, box.checked, rerender));
    const drawn = each.buckets.some((bucket) => bucket.drawn);
    const counts = `${each.regressed} regressed of ${each.tasks} landed tasks`;
    const label = el("label", { class: `agents-curves-pick${drawn ? "" : " is-undrawn"}`, title: `${each.models.length ? `Models: ${each.models.join(", ")}` : "No model recorded"} · ${counts}` }, [
      box,
      el("span", { class: `agents-curves-swatch agents-curve-s${slot(each.crew)}`, "aria-hidden": "true" }),
      el("span", { class: "agents-curves-crew", text: each.crew }),
      el("span", { class: "agents-curves-n", text: `${each.regressed}/${each.tasks}`, title: "Regressed / landed tasks" }),
    ]);
    // The checkbox carries the count's meaning: aria-label is not allowed on the
    // generic span or the label element, so assistive tech would drop it there.
    box.setAttribute("aria-label", `${each.crew}: ${counts}`);
    const item = el("li", {}, [label]);
    item.dataset.crew = each.crew;
    list.append(item);
  }
  return el("div", { class: "agents-curves-picker-wrap" }, [
    el("p", { class: "agents-curves-picker-note", text: "Crew counts are regressed / landed tasks. Hover or focus a line to see its interval." }),
    list,
  ]);
}

// The chart: one line and marker set per drawn series over the buckets any
// drawn series reaches. A gap breaks the line rather than bridging missing
// evidence. Drawn 1:1 in CSS px at the wrapper's real width and redrawn
// when that width changes (STD-07 §R35, as the frontier chart), so tick and
// axis text stay at the dashboard type scale at every panel width. Wilson
// bands show only for the hovered/focused series; a single-bucket series
// shows the same interval as a capped whisker under its marker.
function chartNode(drawnSeries) {
  const wrap = el("div", { class: "agents-curves-chart", role: "region", "aria-label": "Regression rate by lines changed" });
  let drawnWidth = 0;
  const draw = () => {
    const width = Math.floor(wrap.clientWidth);
    if (!width || width === drawnWidth) return;
    drawnWidth = width;
    wrap.replaceChildren(buildPlot(drawnSeries, width));
  };
  draw();
  if (typeof ResizeObserver === "function") {
    const observer = new ResizeObserver(() => {
      if (!wrap.isConnected) { observer.disconnect(); return; }
      if (wrap.clientWidth > 0) draw();
    });
    observer.observe(wrap);
  }
  requestAnimationFrame(() => { if (wrap.isConnected && wrap.clientWidth > 0) draw(); });
  return wrap;
}

function buildPlot(drawnSeries, WIDTH) {
  const HEIGHT = WIDTH < 500 ? 248 : 288;
  const PAD = { left: 44, right: 12, top: 12, bottom: 42 };
  const indexes = drawnSeries.flatMap((each) => each.buckets.filter((bucket) => bucket.drawn).map((bucket) => bucket.bucket));
  const first = Math.min(...indexes);
  const last = Math.max(...indexes);
  const highest = Math.max(0.05, ...drawnSeries.flatMap((each) => each.buckets.filter((bucket) => bucket.drawn).map((bucket) => bucket.high)));
  const step = tickStep(highest);
  const top = Math.min(1, Math.ceil(highest / step - 1e-9) * step);
  const plotWidth = WIDTH - PAD.left - PAD.right;
  const plotHeight = HEIGHT - PAD.top - PAD.bottom;
  const slotWidth = plotWidth / Math.max(1, last - first + 1);
  const x = (index) => PAD.left + (index - first + 0.5) * slotWidth;
  const y = (rate) => PAD.top + plotHeight * (1 - rate / top);
  const plot = svg("svg", { width: WIDTH, height: HEIGHT, class: "agents-curves-plot", role: "img",
    "aria-label": `Regression rate by lines changed for ${drawnSeries.map((each) => each.crew).join(", ")}; the bucket table below lists every value` });
  const setActive = (crew) => {
    for (const group of plot.querySelectorAll(".agents-curves-series")) group.classList.toggle("is-active", group.dataset.crew === crew);
  };
  for (let i = 0; i * step <= top + 1e-9; i += 1) {
    const rate = i * step;
    plot.append(svg("line", { x1: PAD.left, x2: WIDTH - PAD.right, y1: y(rate), y2: y(rate), class: "agents-curves-grid" }));
    plot.append(svg("text", { x: PAD.left - 6, y: y(rate), "text-anchor": "end", "dominant-baseline": "central", class: "agents-curves-tick" }, percent(rate)));
  }
  const labelEvery = Math.ceil((last - first + 1) / (WIDTH < 640 ? 6 : 10));
  const reference = drawnSeries.flatMap((each) => each.buckets);
  for (let index = first; index <= last; index += 1) {
    if ((index - first) % labelEvery !== 0) continue;
    const bucket = reference.find((each) => each.bucket === index) || { lines_from: 2 ** index - 1 };
    plot.append(svg("text", { x: x(index), y: HEIGHT - PAD.bottom + 16, "text-anchor": "middle", class: "agents-curves-tick" }, compactLines(bucket.lines_from)));
  }
  plot.append(svg("text", { x: PAD.left + plotWidth / 2, y: HEIGHT - 6, "text-anchor": "middle", class: "agents-curves-axis" }, "Lines changed (bucket start, doubling)"));
  // Coincident points (same bucket and rate) are dodged on a small ring so
  // every marker stays individually visible and focusable.
  const coincident = new Map();
  for (const each of drawnSeries) for (const bucket of each.buckets.filter((entry) => entry.drawn)) {
    const key = `${bucket.bucket}:${bucket.rate}`;
    coincident.set(key, [...(coincident.get(key) || []), `${each.crew}:${bucket.bucket}`]);
  }
  const seen = new Map();
  const dodge = (crew, bucket) => {
    const key = `${bucket.bucket}:${bucket.rate}`;
    const group = coincident.get(key) || [];
    if (group.length < 2) return { dx: 0, dy: 0 };
    const index = seen.get(key) || 0;
    seen.set(key, index + 1);
    const stepX = Math.min(9, Math.max(4.5, (slotWidth * 0.82) / group.length));
    return { dx: (index - (group.length - 1) / 2) * stepX, dy: group.length > 2 ? ((index % 2) * 2 - 1) * 3 : 0 };
  };
  // The hover and focus tooltip: one bubble drawn over the plot, so a
  // keyboard reader sees a point's count and interval, not only a focus ring.
  // The bubble is measured from the rendered 12px tip text and wrapped to the
  // plot, then clamped inside it. An unwrapped long crew line is wider than a
  // phone plot, and the SVG clips that overflow with no scroll (STD-07 §R4).
  const tip = svg("g", { class: "agents-curves-tip", visibility: "hidden", "aria-hidden": "true", "pointer-events": "none" });
  const showTip = (lines, cx, cy) => {
    const probe = svg("text", { class: "agents-curves-tip-text", visibility: "hidden" });
    plot.append(probe);
    const measure = (text) => {
      probe.textContent = text;
      try {
        const width = probe.getComputedTextLength();
        return Number.isFinite(width) && width > 0 ? width : text.length * 7.2;
      } catch {
        return text.length * 7.2;
      }
    };
    const maxBox = Math.max(8, WIDTH - 8);
    const maxInner = Math.max(1, maxBox - 17);
    let wrapped = [];
    let textW = 0;
    try {
      wrapped = lines.flatMap((line) => wrapTipLine(line, maxInner, measure));
      if (!wrapped.length) wrapped = [""];
      textW = Math.max(...wrapped.map((line) => measure(line)));
    } finally {
      probe.remove();
    }
    const boxW = Math.min(maxBox, Math.ceil(textW) + 16);
    const boxH = wrapped.length * 15 + 10;
    let bx = cx + 12;
    if (bx + boxW > WIDTH - 4) bx = cx - 12 - boxW;
    bx = Math.max(4, Math.min(bx, Math.max(4, WIDTH - 4 - boxW)));
    const by = Math.min(Math.max(4, HEIGHT - 4 - boxH), Math.max(4, cy - boxH / 2));
    tip.replaceChildren(
      svg("rect", { class: "agents-curves-tip-box", x: bx, y: by, width: boxW, height: boxH, rx: 4 }),
      ...wrapped.map((line, i) => svg("text", { class: "agents-curves-tip-text", x: bx + 8, y: by + 15 + i * 15 }, line)),
    );
    tip.setAttribute("visibility", "visible");
  };
  const hideTip = () => tip.setAttribute("visibility", "hidden");
  for (const each of drawnSeries) {
    const css = `agents-curve-s${slot(each.crew)}`;
    const runs = [];
    let current = [];
    for (const bucket of each.buckets) {
      if (!bucket.drawn) continue;
      if (current.length && bucket.bucket !== current[current.length - 1].bucket + 1) {
        runs.push(current);
        current = [];
      }
      current.push(bucket);
    }
    if (current.length) runs.push(current);
    const group = svg("g", { class: `agents-curves-series ${css}` });
    group.dataset.crew = each.crew;
    group.addEventListener("mouseenter", () => setActive(each.crew));
    group.addEventListener("mouseleave", () => setActive(null));
    group.addEventListener("focusin", () => setActive(each.crew));
    group.addEventListener("focusout", () => { if (!group.contains(document.activeElement)) setActive(null); });
    for (const run of runs) {
      const upper = run.map((bucket) => `${x(bucket.bucket)},${y(bucket.high)}`);
      const lower = run.slice().reverse().map((bucket) => `${x(bucket.bucket)},${y(bucket.low)}`);
      if (run.length > 1) {
        group.append(svg("polygon", { points: [...upper, ...lower].join(" "), class: "agents-curves-band" }));
        group.append(svg("polyline", { points: run.map((bucket) => `${x(bucket.bucket)},${y(bucket.rate)}`).join(" "), class: "agents-curves-line" }));
      } else {
        const [bucket] = run;
        const cx = x(bucket.bucket);
        group.append(svg("rect", { x: cx - 7, y: y(bucket.high), width: 14, height: Math.max(1, y(bucket.low) - y(bucket.high)), class: "agents-curves-band" }));
        group.append(svg("line", { x1: cx, x2: cx, y1: y(bucket.low), y2: y(bucket.high), class: "agents-curves-whisker" }));
        group.append(svg("line", { x1: cx - 5, x2: cx + 5, y1: y(bucket.high), y2: y(bucket.high), class: "agents-curves-whisker" }));
        group.append(svg("line", { x1: cx - 5, x2: cx + 5, y1: y(bucket.low), y2: y(bucket.low), class: "agents-curves-whisker" }));
      }
    }
    for (const bucket of each.buckets.filter((entry) => entry.drawn)) {
      const { dx, dy } = dodge(each.crew, bucket);
      const label = pointLabel(each.crew, bucket);
      const cx = x(bucket.bucket) + dx;
      const cy = y(bucket.rate) + dy;
      const marker = svg("circle", { cx, cy, r: markerRadius(bucket), class: `agents-curves-marker${bucket.sparse ? " is-sparse" : ""}`, tabindex: 0, role: "img", "aria-label": label });
      const lines = [
        `${each.crew}, ${rangeText(bucket)}`,
        `${bucket.regressed} of ${bucket.tasks} tasks regressed (n=${bucket.tasks})`,
        `${percent(bucket.rate)} (95% ${percent(bucket.low)}–${percent(bucket.high)})`,
        ...(bucket.sparse ? [`fewer than ${curves.sparse_outcomes} regressed tasks`] : []),
      ];
      marker.addEventListener("mouseenter", () => showTip(lines, cx, cy));
      marker.addEventListener("focus", () => showTip(lines, cx, cy));
      marker.addEventListener("mouseleave", hideTip);
      marker.addEventListener("blur", hideTip);
      group.append(marker);
    }
    plot.append(group);
  }
  plot.append(tip);
  return plot;
}

function tableNode(chosen) {
  const header = (text, scope, className = "") => {
    const cell = el("th", { class: className, text });
    cell.setAttribute("scope", scope);
    return cell;
  };
  const rows = chosen.flatMap((each) => each.buckets.map((bucket) => {
    const tr = el("tr", { class: bucket.drawn ? "" : "agents-row-below" }, [
      header(each.crew, "row"),
      el("td", { text: rangeText(bucket) }),
      el("td", { class: "agents-num", text: String(bucket.tasks) }),
      el("td", { class: "agents-num", text: String(bucket.regressed) }),
      el("td", { class: "agents-num", text: bucket.drawn ? percent(bucket.rate) : "—" }),
      el("td", { class: "agents-num", text: bucket.drawn ? `${percent(bucket.low)}–${percent(bucket.high)}` : "—" }),
      el("td", { text: !bucket.drawn ? `under ${curves.min_bucket_tasks} tasks, not drawn` : bucket.sparse ? "sparse outcomes" : "" }),
    ]);
    tr.dataset.crew = each.crew;
    tr.dataset.bucket = String(bucket.bucket);
    return tr;
  }));
  const table = el("table", { class: "agents-curves-table" }, [
    el("thead", {}, [el("tr", {}, ["Crew", "Lines changed", "Tasks", "Regressed", "Rate", "Wilson 95%", "Note"].map((text) => header(text, "col")))]),
    el("tbody", {}, rows),
  ]);
  const wrap = el("div", { class: "agents-curves-table-wrap", role: "region", "aria-label": "Regression rate by change size, every bucket" }, [table]);
  wrap.tabIndex = 0;
  return el("details", { class: "agents-curves-data" }, [el("summary", { text: "Bucket table" }), wrap]);
}

function cohortText() {
  const counted = series().reduce((sum, each) => sum + each.tasks, 0);
  const { immature = 0, no_size: noSize = 0 } = curves.excluded || {};
  const tier = complexity ? `${TIER_LABELS[complexity] || complexity} · ` : "";
  return `${tier}${plural(counted, "landed task")} counted · excluding ${plural(immature, "task")} too recent and ${plural(noSize, "task")} with no recorded diff size`;
}

function render({ scorecard, rerender }) {
  const selectedWindow = scorecard.requested_window;
  const nodes = [
    controlsNode(selectedWindow, scorecard.execution, rerender),
  ];
  if (!curves) {
    nodes.push(el("p", { class: "agents-muted", role: "status", text: "Loading size curves…" }));
    return nodes;
  }
  if (curves.failed) {
    nodes.push(el("p", { class: "agents-error", role: "status", text: "Size curves unavailable. Reload the page to retry." }));
    return nodes;
  }
  nodes.push(el("p", { class: "agents-muted agents-curves-cohort", text: cohortText() }));
  if (!series().length) {
    nodes.push(el("p", { class: "agents-curves-empty", role: "status", text: `No landed task in this window has ${curves.follow_up_days} days of follow-up and a recorded diff size${complexity ? " at this complexity" : ""}. Widen the window${complexity ? " or choose another complexity" : ""}.` }));
    return nodes;
  }
  nodes.push(pickerNode(rerender));
  const chosenSet = selectedCrews();
  const chosen = series().filter((each) => chosenSet.has(each.crew));
  const drawn = chosen.filter((each) => each.buckets.some((bucket) => bucket.drawn));
  if (drawn.length) {
    nodes.push(chartNode(drawn));
  } else {
    nodes.push(el("p", { class: "agents-curves-empty", role: "status", text: chosen.length
      ? `No chosen crew has a bucket with ${curves.min_bucket_tasks} or more tasks yet.`
      : "Choose a crew above to draw its curve." }));
  }
  if (chosen.length) nodes.push(tableNode(chosen));
  return nodes;
}

// What the panel's figures mean, with the sampling rules once the curves name them.
function panelHint() {
  const lines = ["Share of landed tasks with a regression_from link observed within 7 days of completion, by lines changed, one curve per crew reference as dispatched. The Wilson 95% interval shows for the hovered or focused series; each point's count and interval are on hover and keyboard focus. Shown, not scored."];
  if (curves && !curves.failed) {
    lines.push(`Counts tasks with ${curves.follow_up_days} days of follow-up: too recent and no recorded diff size are excluded from the counted total.`);
    lines.push(`Buckets with fewer than ${curves.min_bucket_tasks} tasks are not drawn. Hollow markers have fewer than ${curves.sparse_outcomes} regressed tasks: their rate rests on very few outcomes. Marker size grows with its bucket's task count. Crews with ${curves.min_series_tasks} or more tasks are drawn by default; the count beside each crew is regressed/landed tasks.`);
  }
  return lines.join("\n");
}

// The panel node is rebuilt when the curves or the reader's choices change.
function hash() {
  return [complexity, picked ? [...picked].sort() : null, curves];
}

// Read on every scorecard fetch, so a poll refreshes the curves too.
function fetchFor(selectedWindow, execution) {
  return loadCurves(selectedWindow, execution);
}

export const implementersPanel = {
  title: "Regression rate by change size",
  hint: panelHint,
  render,
  hash,
  fetch: fetchFor,
  how: [
    "Regression rate by change size: the landed tasks whose final implementation run started in the window, counted once each under that run's crew reference as dispatched (opus@medium and opus@high are separate curves), with a recorded landed diff size and 7 days since completion. A task counts as regressed when any regression_from link against it, whatever its nature or escape, was observed (its regressing task created) by 7 days after completion. Buckets are floor(log2(1 + lines changed)); each point shows the share regressed with its Wilson 95% interval. The complexity filter narrows the cohort to one tier; across all complexities the curves mix task tiers, so they confound task mix with size.",
  ],
};
