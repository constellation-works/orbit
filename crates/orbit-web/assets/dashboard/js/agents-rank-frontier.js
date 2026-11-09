// Analytics › Agents › Rank: score against cost, one tier at a time. Each
// point is a ranked model's unsmoothed tier score (100 + 10 × z on that tier's
// tasks) against its list-price cost per task there, on a log cost axis; this
// module decides the frontier. A tier switch picks the tier and keeps the
// reader's choice across polls. HTML uses `el()`, the chart inline SVG.

import { el, pinnedInfoHint } from './common.js';
import * as rankFormat from './agents-rank-format.js';

const SVG_NS = "http://www.w3.org/2000/svg";
const MIN_MODELS = 3;
const MIN_TASKS = 10;

// The tier on screen, kept across polls while it still qualifies.
let frontierTier = null;

// Every qualifying tier with its points: at least MIN_MODELS ranked models of
// at least MIN_TASKS tasks. A point whose tier cost covers less than
// `rules.min_cost_coverage` of its tasks (`cost_unknown`) is drawn hollow and
// never joins the frontier; one without a score or cost is listed as not plotted.
function tierCharts(payload, rows) {
  const candidates = rows.filter((row) => row.eligible);
  const tiers = new Set();
  for (const row of rows) for (const point of Array.isArray(row.tiers) ? row.tiers : []) if (point && point.tier) tiers.add(point.tier);
  const charts = [];
  for (const tier of tiers) {
    const counted = [];
    const skipped = [];
    for (const row of rows) {
      const point = (Array.isArray(row.tiers) ? row.tiers : []).find((entry) => entry && entry.tier === tier);
      if (!point) continue;
      if (!candidates.includes(row)) { skipped.push({ model: row.model, why: "not ranked yet" }); continue; }
      if ((point.tasks || 0) < MIN_TASKS) { skipped.push({ model: row.model, why: `${rankFormat.plural(point.tasks || 0, "task")}, under ${MIN_TASKS}` }); continue; }
      counted.push({ row, point });
    }
    if (counted.length < MIN_MODELS) continue;
    const plot = [];
    for (const { row, point } of counted) {
      if (typeof point.score !== "number") skipped.push({ model: row.model, why: "no tier score" });
      else if (typeof point.cost_per_task !== "number") skipped.push({ model: row.model, why: "no cost recorded" });
      else plot.push({ model: row.model, ...point, underpriced: Boolean(point.cost_unknown) });
    }
    const priced = plot.filter((point) => !point.underpriced);
    const front = priced.filter((point) => !priced.some((other) => other !== point
      && other.score > point.score
      && other.cost_per_task < point.cost_per_task));
    for (const point of plot) point.frontier = front.includes(point);
    charts.push({ tier, plot, front, skipped, tasks: plot.reduce((sum, point) => sum + (point.tasks || 0), 0) });
  }
  charts.sort((a, b) => rankFormat.tierIndex(a.tier) - rankFormat.tierIndex(b.tier) || String(a.tier).localeCompare(String(b.tier)));
  return charts;
}

/// The chart section for the rank board, or a note when no tier qualifies.
export function rankFrontierNode(payload, rows) {
  const charts = tierCharts(payload, rows);
  const minCoverage = typeof (payload.rules || {}).min_cost_coverage === "number" ? payload.rules.min_cost_coverage : 0.8;
  if (!charts.length) {
    const note = el("p", {
      class: "agents-rank-note",
      role: "note",
      text: `No tier has at least ${MIN_MODELS} ranked models with at least ${MIN_TASKS} tasks, so there is no score-against-cost chart.`,
    });
    note.dataset.key = "frontier";
    note.dataset.hash = "none";
    return note;
  }
  if (!charts.some((chart) => chart.tier === frontierTier)) {
    frontierTier = charts.reduce((best, chart) => (chart.tasks > best.tasks ? chart : best)).tier;
  }
  const build = () => {
    const chart = charts.find((entry) => entry.tier === frontierTier) || charts[0];
    const switcher = el("div", { class: "agents-rank-tiers", role: "group", "aria-label": "Tier" }, charts.map((entry) => {
      const button = el("button", {
        class: entry === chart ? "agents-rank-tier on" : "agents-rank-tier",
        type: "button",
        "aria-pressed": entry === chart ? "true" : "false",
      }, [rankFormat.tierLabel(entry.tier), el("span", { class: "agents-rank-tier-count", text: ` ${entry.plot.length}` })]);
      button.dataset.tier = entry.tier;
      button.addEventListener("click", () => {
        if (frontierTier === entry.tier) return;
        frontierTier = entry.tier;
        const next = build();
        section.replaceWith(next);
        section = next;
        const focus = next.querySelector(`.agents-rank-tier[data-tier="${CSS.escape(entry.tier)}"]`);
        if (focus) focus.focus();
      });
      return button;
    }));
    const node = el("section", { class: "agents-rank-frontier", "aria-label": "Score against cost" }, [
      el("div", { class: "agents-rank-frontier-head" }, [
        el("div", { class: "agents-rank-frontier-title" }, [
          el("h3", { text: "Score against cost, by tier" }),
          pinnedInfoHint("Each point is one ranked model's score on this tier's tasks only, 100 + 10 × z before the board's shrinkage, against its recorded list-price cost per task on the tier. The frontier joins models no other priced model beats on both a higher score and a lower cost. A tier is shown when at least 3 ranked models have 10 or more tasks on it."),
        ]),
        switcher,
      ]),
      el("div", { class: "agents-rank-frontier-body" }, [
        chartCard(chart, minCoverage),
        tierTable(chart),
      ]),
    ]);
    node.dataset.key = "frontier";
    node.dataset.hash = JSON.stringify([frontierTier, charts.map((entry) => [entry.tier, entry.plot, entry.skipped])]);
    return node;
  };
  let section = build();
  return section;
}

function chartCard(chart, minCoverage) {
  const label = rankFormat.tierLabel(chart.tier);
  const body = chart.plot.length
    ? el("div", { class: "agents-rank-plot-wrap", role: "region", "aria-label": `${label} chart` }, [scatterPlot(chart)])
    : el("p", { class: "agents-rank-note", text: `No ranked model on ${label} has both a tier score and a recorded cost to plot.` });
  if (chart.plot.length) body.tabIndex = 0;
  return el("div", { class: "agents-rank-box agents-rank-chart" }, [
    el("div", { class: "agents-rank-box-head" }, [
      el("h4", { text: `${label} tier · ${rankFormat.plural(chart.plot.length, "model")} plotted` }),
      el("span", { class: "agents-rank-box-meta", text: "Cost axis is logarithmic · $ per task at list price" }),
    ]),
    body,
    el("div", { class: "agents-rank-legend" }, [
      el("span", { class: "agents-rank-legend-item" }, [el("span", { class: "agents-rank-key is-frontier", "aria-hidden": "true" }), "on the frontier"]),
      el("span", { class: "agents-rank-legend-item" }, [el("span", { class: "agents-rank-key", "aria-hidden": "true" }), "dominated"]),
      el("span", { class: "agents-rank-legend-item" }, [el("span", { class: "agents-rank-key is-underpriced", "aria-hidden": "true" }), `under ${rankFormat.num(minCoverage * 100, 0)}% of tasks priced, not on the frontier`]),
    ]),
  ]);
}

const LOG_TICKS = [0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 25, 50, 100, 250];

function scatterPlot(chart) {
  const width = 760;
  const height = 460;
  const left = 52;
  const right = 24;
  const top = 20;
  const bottom = 44;
  const plotW = width - left - right;
  const plotH = height - top - bottom;
  const costs = chart.plot.map((point) => point.cost_per_task).filter((cost) => cost > 0);
  let c0 = costs.length ? Math.min(...costs) / 1.6 : 0.1;
  let c1 = costs.length ? Math.max(...costs) * 1.6 : 10;
  if (c0 <= 0) c0 = 0.01;
  const lx0 = Math.log10(c0);
  const lx1 = Math.log10(c1);
  const xOf = (cost) => left + ((Math.log10(Math.max(cost, c0)) - lx0) / (lx1 - lx0)) * plotW;
  const scores = [...chart.plot.map((point) => point.score), 100];
  const rawSpan = Math.max(...scores) - Math.min(...scores) || 10;
  const step = [5, 10, 20, 25, 50, 100].find((candidate) => rawSpan / candidate <= 6) || 100;
  const y0 = Math.floor((Math.min(...scores) - rawSpan * 0.06) / step) * step;
  const y1 = Math.ceil((Math.max(...scores) + rawSpan * 0.06) / step) * step;
  const yOf = (score) => top + ((y1 - score) / (y1 - y0)) * plotH;
  const sortedFront = [...chart.front].sort((a, b) => a.cost_per_task - b.cost_per_task);
  const svg = svgEl("svg", {
    class: "agents-rank-plot",
    viewBox: `0 0 ${width} ${height}`,
    role: "img",
    "aria-label": `${rankFormat.tierLabel(chart.tier)} tier score against cost per task. Frontier: ${sortedFront.map((point) => point.model).join(", ") || "none"}.`,
  });
  for (let v = y0; v <= y1 + 1e-9; v += step) {
    const y = yOf(v);
    svg.append(svgEl("line", { class: "agents-rank-gridline", x1: left, y1: y, x2: width - right, y2: y }));
    svg.append(svgText(String(v), { class: "agents-rank-tick", x: left - 8, y: y + 4, "text-anchor": "end" }));
  }
  for (const tick of LOG_TICKS) {
    if (tick < c0 || tick > c1) continue;
    const x = xOf(tick);
    svg.append(svgEl("line", { class: "agents-rank-gridline", x1: x, y1: top, x2: x, y2: height - bottom }));
    svg.append(svgText(dollars(tick), { class: "agents-rank-tick", x, y: height - bottom + 16, "text-anchor": "middle" }));
  }
  svg.append(
    svgEl("line", { class: "agents-rank-axisline", x1: left, y1: top, x2: left, y2: height - bottom }),
    svgEl("line", { class: "agents-rank-axisline", x1: left, y1: height - bottom, x2: width - right, y2: height - bottom }),
    svgText("cost per task", { class: "agents-rank-axis-title", x: left + plotW / 2, y: height - 8, "text-anchor": "middle" }),
    svgText("tier score", { class: "agents-rank-axis-title", x: 14, y: top + plotH / 2, "text-anchor": "middle", transform: `rotate(-90 14 ${top + plotH / 2})` }),
    svgText("↖ HIGHER SCORE, LOWER COST", { class: "agents-rank-corner", x: left + 10, y: top + 14 }),
  );
  if (100 >= y0 && 100 <= y1) {
    svg.append(
      svgEl("line", { class: "agents-rank-fleet", x1: left, y1: yOf(100), x2: width - right, y2: yOf(100) }),
      svgText("fleet = 100", { class: "agents-rank-tick", x: width - right - 6, y: yOf(100) - 6, "text-anchor": "end" }),
    );
  }
  if (sortedFront.length >= 2) {
    svg.append(svgEl("polyline", {
      class: "agents-rank-front",
      points: sortedFront.map((point) => `${xOf(point.cost_per_task).toFixed(1)},${yOf(point.score).toFixed(1)}`).join(" "),
    }));
  }
  const labels = placeLabels(chart.plot.map((point) => ({ point, x: xOf(point.cost_per_task), y: yOf(point.score) })), left + plotW * 0.68, top, height - bottom);
  for (const { point, x, y, ly, flip } of labels) {
    const group = svgEl("g", {
      class: `agents-rank-point${point.frontier ? " is-frontier" : ""}${point.underpriced ? " is-underpriced" : ""}`,
      "data-model": point.model || "",
      "data-frontier": point.frontier ? "true" : "false",
    });
    const title = svgEl("title", {});
    title.textContent = `${point.model}: tier score ${rankFormat.num(point.score, 1)}, ${dollars(point.cost_per_task, true)} per task, ${rankFormat.plural(point.tasks || 0, "task")}, ${rankFormat.num((point.cost_coverage || 0) * 100, 0)}% priced${point.frontier ? ", frontier" : ""}`;
    group.append(title, svgEl("circle", { class: "agents-rank-mark", cx: x.toFixed(1), cy: y.toFixed(1), r: point.frontier ? 7 : 5.5 }));
    const tx = flip ? x - 12 : x + 12;
    const anchor = flip ? "end" : "start";
    if (Math.abs(ly - y) > 4) group.append(svgEl("line", { class: "agents-rank-leader", x1: x, y1: y, x2: tx - (flip ? -4 : 4), y2: ly - 4 }));
    group.append(
      svgText(point.model || "—", { class: "agents-rank-point-name", x: tx, y: ly - 3, "text-anchor": anchor }),
      svgText(`${rankFormat.num(point.score, 1)} · ${dollars(point.cost_per_task, true)} · ${point.tasks || 0} tasks${point.underpriced ? ` · ${rankFormat.num((point.cost_coverage || 0) * 100, 0)}% priced` : ""}`,
        { class: "agents-rank-point-caption", x: tx, y: ly + 11, "text-anchor": anchor }),
    );
    svg.append(group);
  }
  return svg;
}

// Labels sit right of their point, or left past `flipAt`, and are pushed
// apart vertically so two never overlap on the same side.
function placeLabels(items, flipAt, top, bottom) {
  const gap = 30;
  const placed = items.map((item) => ({ ...item, flip: item.x > flipAt, ly: item.y }));
  for (const side of [false, true]) {
    const group = placed.filter((item) => item.flip === side).sort((a, b) => a.y - b.y);
    for (let i = 1; i < group.length; i += 1) {
      const prev = group[i - 1];
      if (Math.abs(group[i].x - prev.x) < 200 && group[i].ly - prev.ly < gap) group[i].ly = prev.ly + gap;
    }
    const overflow = group.length ? group[group.length - 1].ly - (bottom - 14) : 0;
    if (overflow > 0) for (const item of group) item.ly = Math.max(top + 10, item.ly - overflow);
  }
  return placed;
}

function tierTable(chart) {
  const label = rankFormat.tierLabel(chart.tier);
  const ordered = [...chart.plot].sort((a, b) => b.score - a.score || String(a.model).localeCompare(String(b.model)));
  const rows = ordered.map((point) => {
    const name = el("span", { class: "agents-rank-model" }, [
      rankFormat.providerDot(point.model),
      el("span", { class: "agents-rank-model-name", text: point.model || "—", title: point.model || "" }),
      point.frontier ? el("span", { class: "agents-rank-badge", text: "FRONTIER" }) : null,
    ]);
    const tr = el("tr", { class: point.underpriced ? "is-underpriced" : "" }, [
      el("td", {}, [name]),
      el("td", { class: "agents-num agents-rank-score", text: rankFormat.num(point.score, 1) }),
      el("td", {
        class: "agents-num agents-muted",
        text: `${dollars(point.cost_per_task, true)}${point.underpriced ? "*" : ""}`,
        title: `${rankFormat.num((point.cost_coverage || 0) * 100, 0)}% of this tier's tasks priced`,
      }),
      el("td", { class: "agents-num agents-muted", text: String(point.tasks || 0) }),
    ]);
    tr.dataset.model = point.model || "";
    return tr;
  });
  const skipped = groupSkipped(chart.skipped);
  return el("aside", { class: "agents-rank-box agents-rank-tier-table", "aria-label": `${label} tier table` }, [
    el("h4", { text: `${label} tier, by tier score` }),
    el("div", { class: "agents-table-wrap" }, [
      el("table", { class: "agents-table" }, [
        el("thead", {}, [el("tr", {}, [
          el("th", { text: "Model" }),
          el("th", { class: "agents-num", text: "Score" }),
          el("th", { class: "agents-num", text: "$/task" }),
          el("th", { class: "agents-num", text: "Tasks" }),
        ])]),
        el("tbody", {}, rows),
      ]),
    ]),
    chart.plot.some((point) => point.underpriced) ? el("p", { class: "agents-rank-note", text: "* Too few tasks priced to place on the frontier." }) : null,
    skipped.length ? el("div", { class: "agents-rank-skipped" }, [
      el("h5", { text: "Not plotted" }),
      el("ul", {}, skipped.map((entry) => el("li", {}, [entry.models.join(", "), el("span", { class: "agents-muted", text: ` · ${entry.why}` })]))),
    ]) : null,
    el("p", { class: "agents-rank-note", text: "Tier score is 100 + 10 × z on this tier alone, unsmoothed. It can disagree with the overall rank." }),
  ]);
}

function groupSkipped(skipped) {
  const byWhy = new Map();
  for (const entry of skipped) {
    const list = byWhy.get(entry.why) || [];
    list.push(entry.model || "—");
    byWhy.set(entry.why, list);
  }
  return [...byWhy].map(([why, models]) => ({ why, models }));
}

function dollars(value, precise = false) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  if (precise) return `$${value.toFixed(2)}`;
  return `$${value < 1 ? value.toFixed(value < 0.1 ? 3 : 2).replace(/0+$/, "") : rankFormat.num(value, 1)}`;
}

function svgEl(name, attrs) {
  const node = document.createElementNS(SVG_NS, name);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value != null) node.setAttribute(key, String(value));
  }
  return node;
}

function svgText(text, attrs) {
  const node = svgEl("text", attrs);
  node.textContent = text;
  return node;
}
