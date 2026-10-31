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
// crew never recolours another.
const SERIES_SLOTS = 8;
const SVG_NS = "http://www.w3.org/2000/svg";
// A phone draws on a narrower canvas so tick labels keep a readable size.
const NARROW = "(max-width: 640px)";

let complexity = "";
// `null` until the reader picks: then the crews they chose.
let picked = null;
let curves = null;
let curvesScope = null;
// The latest render's redraw, so crossing the phone breakpoint redraws the
// chart on its canvas.
let redraw = null;
// The phone query, or null where the host has no `matchMedia`.
const narrowQuery = typeof window.matchMedia === "function" ? window.matchMedia(NARROW) : null;
if (narrowQuery) narrowQuery.addEventListener("change", () => { if (redraw) redraw(); });

function isNarrow() {
  return Boolean(narrowQuery && narrowQuery.matches);
}

function curvesPath(selectedWindow, execution) {
  const tier = complexity ? `&complexity=${encodeURIComponent(complexity)}` : "";
  return `/api/agents/implementers/size-curves?window=${encodeURIComponent(selectedWindow)}${tier}&execution=${encodeURIComponent(execution || "managed")}`;
}

/// Read the curves for `selectedWindow` and `execution` under the current
/// filter. A response for a window, filter or workspace no longer shown is
/// dropped.
function loadCurves(selectedWindow, execution) {
  const path = curvesPath(selectedWindow, execution);
  const scope = `${getWorkspaceRevision()}\0${path}`;
  if (curvesScope !== scope) {
    curvesScope = scope;
    curves = null;
  }
  return fetchJson(path).then((payload) => {
    if (curvesScope !== scope) return;
    curves = payload && payload.requested_window === selectedWindow && Array.isArray(payload.series) ? payload : null;
  }, () => {
    if (curvesScope === scope) curves = { failed: true };
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

function compactLines(value) {
  if (value >= 1000) return `${Number((value / 1000).toFixed(value >= 10000 ? 0 : 1))}k`;
  return String(value);
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
    const item = el("li", {}, [el("label", { class: `agents-curves-pick${drawn ? "" : " is-undrawn"}`, title: each.models.length ? `Models: ${each.models.join(", ")}` : "No model recorded" }, [
      box,
      el("span", { class: `agents-curves-swatch agents-curve-s${slot(each.crew)}`, "aria-hidden": "true" }),
      el("span", { class: "agents-curves-crew", text: each.crew }),
      el("span", { class: "agents-curves-n", text: `${each.regressed}/${each.tasks}` }),
    ])]);
    item.dataset.crew = each.crew;
    list.append(item);
  }
  return list;
}

// The chart: one band, line and marker set per drawn series over the
// buckets any drawn series reaches. A gap in a series' drawn buckets breaks
// its line and band rather than bridging missing evidence.
function chartNode(drawnSeries) {
  const narrow = isNarrow();
  const WIDTH = narrow ? 360 : 640;
  const HEIGHT = narrow ? 240 : 260;
  const PAD = { left: 40, right: 10, top: 12, bottom: 40 };
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
  const plot = svg("svg", { viewBox: `0 0 ${WIDTH} ${HEIGHT}`, class: "agents-curves-plot", role: "img",
    "aria-label": `Regression rate by lines changed for ${drawnSeries.map((each) => each.crew).join(", ")}; the bucket table below lists every value` });
  for (let i = 0; i * step <= top + 1e-9; i += 1) {
    const rate = i * step;
    plot.append(svg("line", { x1: PAD.left, x2: WIDTH - PAD.right, y1: y(rate), y2: y(rate), class: "agents-curves-grid" }));
    plot.append(svg("text", { x: PAD.left - 6, y: y(rate), "text-anchor": "end", "dominant-baseline": "central", class: "agents-curves-tick" }, percent(rate)));
  }
  const labelEvery = Math.ceil((last - first + 1) / (narrow ? 6 : 10));
  const reference = drawnSeries.flatMap((each) => each.buckets);
  for (let index = first; index <= last; index += 1) {
    if ((index - first) % labelEvery !== 0) continue;
    const bucket = reference.find((each) => each.bucket === index) || { lines_from: 2 ** index - 1 };
    plot.append(svg("text", { x: x(index), y: HEIGHT - PAD.bottom + 16, "text-anchor": "middle", class: "agents-curves-tick" }, compactLines(bucket.lines_from)));
  }
  plot.append(svg("text", { x: PAD.left + plotWidth / 2, y: HEIGHT - 6, "text-anchor": "middle", class: "agents-curves-axis" }, "Lines changed (bucket start, doubling)"));
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
    for (const run of runs) {
      const upper = run.map((bucket) => `${x(bucket.bucket)},${y(bucket.high)}`);
      const lower = run.slice().reverse().map((bucket) => `${x(bucket.bucket)},${y(bucket.low)}`);
      if (run.length > 1) {
        group.append(svg("polygon", { points: [...upper, ...lower].join(" "), class: "agents-curves-band" }));
        group.append(svg("polyline", { points: run.map((bucket) => `${x(bucket.bucket)},${y(bucket.rate)}`).join(" "), class: "agents-curves-line" }));
      } else {
        const [bucket] = run;
        group.append(svg("line", { x1: x(bucket.bucket), x2: x(bucket.bucket), y1: y(bucket.low), y2: y(bucket.high), class: "agents-curves-whisker" }));
      }
    }
    for (const bucket of each.buckets.filter((entry) => entry.drawn)) {
      const marker = svg("circle", { cx: x(bucket.bucket), cy: y(bucket.rate), r: 3.5, class: `agents-curves-marker${bucket.sparse ? " is-sparse" : ""}` });
      marker.append(svg("title", {}, `${each.crew}, ${rangeText(bucket)}: ${bucket.regressed} of ${bucket.tasks} regressed, ${percent(bucket.rate)} (95% ${percent(bucket.low)}–${percent(bucket.high)})${bucket.sparse ? `; fewer than ${curves.sparse_outcomes} regressed tasks` : ""}`));
      group.append(marker);
    }
    plot.append(group);
  }
  return el("div", { class: "agents-curves-chart" }, [plot]);
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
  return `${tier}${plural(counted, "landed task")} with ${curves.follow_up_days} days of follow-up`
    + ` · ${immature} landed too recently and ${noSize} with no recorded diff size left out`;
}

function render({ scorecard, rerender }) {
  const selectedWindow = scorecard.requested_window;
  redraw = rerender;
  const nodes = [
    el("p", { class: "agents-muted", text: "Share of landed tasks with a regression_from link observed within 7 days of completion, by lines changed, one curve per crew reference as dispatched. Shaded bands are Wilson 95% intervals. Shown, not scored." }),
    controlsNode(selectedWindow, scorecard.execution, rerender),
  ];
  if (!curves) {
    nodes.push(el("p", { class: "agents-muted", role: "status", text: "Loading size curves…" }));
    return nodes;
  }
  if (curves.failed) {
    nodes.push(el("p", { class: "agents-error", role: "status", text: "Size curves unavailable. Refresh to retry." }));
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
  nodes.push(el("p", { class: "agents-muted", text: `Buckets with fewer than ${curves.min_bucket_tasks} tasks are not drawn. Hollow markers have fewer than ${curves.sparse_outcomes} regressed tasks: their rate rests on very few outcomes. Crews with ${curves.min_series_tasks} or more tasks are drawn by default; the count beside each crew is regressed/landed.` }));
  if (chosen.length) nodes.push(tableNode(chosen));
  return nodes;
}

// The panel node is rebuilt when the curves or the reader's choices change.
function hash() {
  return [complexity, picked ? [...picked].sort() : null, curves, isNarrow()];
}

// Read on every scorecard fetch, so a poll refreshes the curves too.
function fetchFor(selectedWindow, execution) {
  return loadCurves(selectedWindow, execution);
}

export const implementersPanel = {
  title: "Regression rate by change size",
  render,
  hash,
  fetch: fetchFor,
  how: [
    "Regression rate by change size: the landed tasks whose final implementation run started in the window, counted once each under that run's crew reference as dispatched (opus@medium and opus@high are separate curves), with a recorded landed diff size and 7 days since completion. A task counts as regressed when any regression_from link against it, whatever its nature or escape, was observed (its regressing task created) by 7 days after completion. Buckets are floor(log2(1 + lines changed)); each point shows the share regressed with its Wilson 95% interval. The complexity filter narrows the cohort to one tier; across all complexities the curves mix task tiers, so they confound task mix with size.",
  ],
};
