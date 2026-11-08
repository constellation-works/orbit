// Analytics › Agents › Rank. Reads GET /api/agents/rank and draws the band
// table, each row's measure breakdown, and one score-against-cost chart per
// qualifying tier. The tier score is the server's unsmoothed 100+10×z for
// that tier only; this page decides the frontier.

import { el, syncNodes, fetchJson, requestPanel, pinnedInfoHint } from './common.js';

const BODY_ID = "agents-body";
const COUNT_ID = "agents-count";
const SVG_NS = "http://www.w3.org/2000/svg";
const TIER_ORDER = ["low", "medium", "hard", "xhard", "unassessed"];
const MIN_MODELS = 3;
const MIN_TASKS = 10;
const COLUMNS = 8;

const openModels = new Set();

export function fetchAndRenderRank({ window: selectedWindow, execution, live, onCount }) {
  const body = document.getElementById(BODY_ID);
  if (body) body.classList.add("agents-body-rank");
  const path = `/api/agents/rank?window=${encodeURIComponent(selectedWindow)}&execution=${encodeURIComponent(execution)}${live ? "&live=true" : ""}`;
  return requestPanel(BODY_ID, path, () => fetchJson(path), (payload) => {
    if (!payloadHonorsRank(payload, selectedWindow, execution, live)) {
      console.error("agent rank payload rejected", payload && payload.requested_window, payload && payload.execution, payload && payload.live);
      renderRejected();
      return;
    }
    renderRank(payload, onCount);
  }, COUNT_ID, (payload) => (payload && (payload.provisional || payload.live) ? "Live view, provisional." : "Settled."));
}

function payloadHonorsRank(payload, selectedWindow, execution, live) {
  return Boolean(payload)
    && payload.requested_window === selectedWindow
    && (payload.execution ?? "managed") === execution
    && Boolean(payload.live) === Boolean(live);
}

function renderRejected() {
  const body = document.getElementById(BODY_ID);
  if (!body) return;
  syncNodes(body, [stateNode("stale", "This ranking is out of date", "The answer did not match the window on screen. Reload the page to retry.")]);
}

function renderRank(payload, onCount) {
  const body = document.getElementById(BODY_ID);
  if (!body) return;
  body.classList.add("agents-body-rank");
  const rows = Array.isArray(payload.rows) ? payload.rows : [];
  const title = document.getElementById("agents-title");
  if (title) title.textContent = "Rank";
  const count = document.getElementById(COUNT_ID);
  if (count) count.textContent = payload.status === "ok" ? `${rows.length} models` : "—";
  if (onCount) {
    const text = payload.qualified ? `${payload.eligible_models || 0} eligible` : "unqualified";
    const hint = payload.qualified
      ? `${payload.eligible_models || 0} eligible models`
      : "Fewer than 3 eligible models, so this board has no bands";
    onCount(text, hint);
  }
  const meta = document.getElementById("agents-meta");
  if (meta) {
    meta.textContent = "";
    meta.append(document.createTextNode(metaText(payload)), " ", pinnedInfoHint(metaHint(payload)));
  }
  const nodes = [];
  if (payload.provisional || payload.live) nodes.push(bannerNode("provisional", "Live view, provisional. Escaped regressions have not had time to arrive. This board is not a settled result."));
  if (!payload.qualified) nodes.push(bannerNode("unqualified", "This board is not qualified (fewer than 3 eligible models), so it shows scores without bands or ranks."));
  if (payload.scales_source === "window_estimate") nodes.push(bannerNode("scales", "Noise scales are estimated from this window. They are not a calibrated season."));
  if (payload.status !== "ok") {
    nodes.push(stateNode("not-ok", "Ranking is unavailable", "The ranking did not return a board for this window."));
  } else if (!rows.length) {
    nodes.push(stateNode("empty", "No models in this window", "Widen the window to rank implementer models."));
  } else {
    nodes.push(boardNode(payload, rows));
  }
  nodes.push(...chartNodes(rows));
  nodes.push(howNode(payload));
  syncNodes(body, nodes);
}

function metaText(payload) {
  const requested = payload.requested_window || "";
  const served = payload.window || "";
  const windowLabel = requested && served && requested !== served ? `${requested} served as ${served}` : (requested || served);
  const scales = payload.scales_source === "window_estimate"
    ? "scales estimated"
    : (payload.season && payload.season.season != null ? `season ${payload.season.season}` : "no season");
  const eligible = payload.qualified ? `${payload.eligible_models || 0} eligible` : "unqualified";
  return `${windowLabel} · ${payload.execution || "managed"} · ${scales} · ${eligible}`;
}

function metaHint(payload) {
  const lines = [
    "Score starts at 100. A higher score is better. Cost orders a band and is never part of the score.",
    `Counting ${payload.execution || "managed"} tasks. The execution split below still counts every mode.`,
  ];
  if (payload.settle_lag) lines.push(`Settled boards end ${payload.settle_lag} before the requested instant.`);
  if (payload.computed_at) lines.push(`Computed ${payload.computed_at}.`);
  return lines.join("\n");
}

function bannerNode(key, text) {
  const node = el("p", { class: `agents-rank-banner agents-rank-banner-${key}`, role: "note", text });
  node.dataset.key = `banner-${key}`;
  node.dataset.hash = text;
  return node;
}

function stateNode(key, heading, text) {
  const node = el("section", { class: `agents-state agents-state-${key}`, role: "status" }, [
    el("h3", { text: heading }),
    el("p", { text }),
  ]);
  node.dataset.key = "state";
  node.dataset.hash = JSON.stringify([key, heading, text]);
  return node;
}

function boardNode(payload, rows) {
  const head = el("tr", {}, [
    el("th", { text: "" }),
    el("th", { text: "Rank" }),
    el("th", { text: "Model" }),
    el("th", { class: "agents-num", text: "Score" }),
    el("th", { class: "agents-num", text: "Interval", title: "10th to 90th percentile of the shrunk score" }),
    el("th", { class: "agents-num", text: "Cost ratio", title: "List-price cost per task divided by the fleet's on the same tier mix" }),
    el("th", { class: "agents-num", text: "Units" }),
    el("th", { text: "Eligibility" }),
  ]);
  const bodyRows = rows.flatMap((row) => rowPair(payload, row));
  const table = el("table", { class: "agents-table" }, [
    el("thead", {}, [head]),
    el("tbody", {}, bodyRows),
  ]);
  const wrap = el("div", { class: "agents-table-wrap", role: "region", "aria-label": "Model ranking" }, [table]);
  wrap.tabIndex = 0;
  const note = payload.qualified
    ? "Shared ranks such as 1= are one band. Cost orders the band and never crosses it."
    : "Scores are shown. This board has no bands or ranks.";
  const node = el("section", { class: "agents-ranking", "aria-label": "Model ranking" }, [
    el("p", { class: "agents-ranking-note", text: note }),
    wrap,
  ]);
  node.dataset.key = "board";
  node.dataset.hash = JSON.stringify(rows.map(rowSignature));
  return node;
}

function rowSignature(row) {
  return [row.model, row.rank_label, row.score, row.eligible, row.cost && row.cost.ratio, row.cost && row.cost.cost_unknown, row.units];
}

function rowPair(payload, row) {
  const open = openModels.has(row.model);
  const button = el("button", {
    class: "agents-rank-expand",
    type: "button",
    text: open ? "Hide" : "Breakdown",
    "aria-expanded": open ? "true" : "false",
  });
  button.setAttribute("aria-controls", breakdownId(row.model));
  const main = el("tr", {}, [
    el("td", {}, [button]),
    el("td", { class: "agents-num", text: rankText(payload, row) }),
    el("td", { text: row.model || "—" }),
    el("td", { class: "agents-num", text: num(row.score) }),
    el("td", { class: "agents-num", text: intervalText(row) }),
    el("td", { class: "agents-num", text: costText(row), title: costTitle(row) }),
    el("td", { class: "agents-num", text: row.units == null ? "—" : String(row.units) }),
    el("td", { class: row.eligible ? "" : "agents-muted", text: row.eligible ? "eligible" : (row.ineligible_reason || "ineligible") }),
  ]);
  main.dataset.model = row.model || "";
  const detailCell = el("td", {}, [breakdownNode(payload, row)]);
  detailCell.colSpan = COLUMNS;
  const detail = el("tr", { class: "agents-rank-detail" }, [detailCell]);
  detail.id = breakdownId(row.model);
  detail.hidden = !open;
  button.addEventListener("click", () => {
    const next = !openModels.has(row.model);
    if (next) openModels.add(row.model);
    else openModels.delete(row.model);
    detail.hidden = !next;
    button.textContent = next ? "Hide" : "Breakdown";
    button.setAttribute("aria-expanded", next ? "true" : "false");
  });
  return [main, detail];
}

function breakdownId(model) {
  return `agents-rank-breakdown-${String(model || "model").replace(/[^A-Za-z0-9_-]/g, "-")}`;
}

function rankText(payload, row) {
  if (!payload.qualified || row.rank_label == null || row.rank_label === "") return "—";
  return String(row.rank_label);
}

function intervalText(row) {
  const interval = row.interval;
  if (!interval || typeof interval.p10 !== "number" || typeof interval.p90 !== "number") return "—";
  return `${num(interval.p10)}–${num(interval.p90)}`;
}

function costText(row) {
  const cost = row.cost || {};
  if (cost.cost_unknown) return "cost unknown";
  if (typeof cost.ratio !== "number") return "—";
  return `${num(cost.ratio, 2)}×`;
}

function costTitle(row) {
  const cost = row.cost || {};
  const coverage = typeof cost.coverage === "number" ? `${num(cost.coverage * 100, 0)}% of tasks priced` : "coverage unknown";
  if (cost.cost_unknown) return `Cost unknown. ${coverage}.`;
  return coverage;
}

function breakdownNode(payload, row) {
  return el("div", { class: "agents-rank-breakdown" }, [
    el("h3", { text: `${row.model || "Model"} breakdown` }),
    measureTable(payload, row),
    crewsNode(row),
    effortNode(row),
    shownNode(payload, row),
  ]);
}

function measureTable(payload, row) {
  const measures = Array.isArray(payload.measures) ? payload.measures : [];
  const cells = row.measures || {};
  const body = measures.map((measure) => {
    const cell = cells[measure.key] || {};
    const unmarked = cell.status === "measured";
    return el("tr", {}, [
      el("td", { text: measure.label || measure.key }),
      el("td", { text: cell.status || "—", title: cell.reason || "" }),
      el("td", { class: "agents-num", text: unmarked ? num(cell.actual) : "—" }),
      el("td", { class: "agents-num", text: unmarked ? num(cell.expected) : "—" }),
      el("td", { class: "agents-num", text: unmarked ? num(cell.residual) : "—" }),
      el("td", { class: "agents-num", text: unmarked ? num(cell.contribution) : "—" }),
      el("td", { text: unmarked ? `${cell.units ?? "—"} ${cell.unit || measure.units || ""}`.trim() : (cell.reason || "unmeasured") }),
    ]);
  });
  return el("table", { class: "agents-table agents-rank-measures" }, [
    el("thead", {}, [el("tr", {}, [
      el("th", { text: "Measure" }),
      el("th", { text: "Status" }),
      el("th", { class: "agents-num", text: "Actual" }),
      el("th", { class: "agents-num", text: "Expected" }),
      el("th", { class: "agents-num", text: "Residual" }),
      el("th", { class: "agents-num", text: "Contribution" }),
      el("th", { text: "Units" }),
    ])]),
    el("tbody", {}, body.length ? body : [el("tr", {}, [emptyCell("No measures on this board.", 7)])]),
  ]);
}

function emptyCell(text, span) {
  const cell = el("td", { text });
  cell.colSpan = span;
  return cell;
}

function crewsNode(row) {
  const crews = Array.isArray(row.crews) ? row.crews : [];
  const text = crews.length
    ? crews.map((crew) => `${crew.crew || "—"} (${crew.tasks ?? 0})`).join(", ")
    : "No crew split.";
  return el("p", { class: "agents-rank-note", text: `Crews: ${text}` });
}

function effortNode(row) {
  const split = Array.isArray(row.effort_split) ? row.effort_split : [];
  if (!split.length) return el("p", { class: "agents-rank-note", text: "No completed implementation invocations in this window." });
  return el("table", { class: "agents-table agents-rank-measures" }, [
    el("caption", { text: "Effort split" }),
    el("thead", {}, [el("tr", {}, [
      el("th", { text: "Crew" }),
      el("th", { text: "Effort" }),
      el("th", { class: "agents-num", text: "Invocations" }),
      el("th", { class: "agents-num", text: "Tasks" }),
      el("th", { class: "agents-num", text: "Run time" }),
      el("th", { class: "agents-num", text: "Tokens" }),
    ])]),
    el("tbody", {}, split.map((item) => el("tr", {}, [
      el("td", { text: item.crew || "—" }),
      el("td", { text: item.reasoning_effort == null ? "provider default" : String(item.reasoning_effort) }),
      el("td", { class: "agents-num", text: String(item.invocations ?? 0) }),
      el("td", { class: "agents-num", text: String(item.tasks ?? 0) }),
      el("td", { class: "agents-num", text: formatDuration(item.duration_ms || 0), title: "Sum of completed implementation invocations, not queue or CI time" }),
      el("td", { class: "agents-num", text: num((item.input_tokens || 0) + (item.output_tokens || 0), 0), title: `${item.input_tokens || 0} in, ${item.output_tokens || 0} out` }),
    ]))),
  ]);
}

function shownNode(payload, row) {
  const totals = effortTotals(row);
  const cost = row.cost || {};
  const lines = [
    `Cost per task: ${typeof cost.per_task === "number" ? num(cost.per_task, 4) : "—"}${cost.cost_unknown ? " (cost unknown)" : ""}. Coverage ${typeof cost.coverage === "number" ? num(cost.coverage * 100, 0) + "%" : "—"}.`,
    totals.empty
      ? "Tokens and run time: — (no completed implementation invocations)."
      : `Tokens: ${num(totals.input + totals.output, 0)} (${num(totals.input, 0)} in, ${num(totals.output, 0)} out). Run time: ${formatDuration(totals.duration)}, the sum of completed implementation invocations, not queue or CI.`,
    `Landed rate: ${typeof row.landed_rate === "number" ? num(row.landed_rate * 100, 1) + "%" : "—"} (${num(row.landed_tasks, 2)} landed of ${row.units ?? 0}).`,
    `Per 1,000 landed lines: ${lineRates(row, totals)}.`,
    executionText(row.execution),
    boardStats(payload),
  ];
  return el("div", { class: "agents-rank-shown" }, [
    el("h4", { text: "Shown, not scored" }),
    ...lines.map((text) => el("p", { class: "agents-rank-note", text })),
  ]);
}

function effortTotals(row) {
  const split = Array.isArray(row.effort_split) ? row.effort_split : [];
  const totals = { input: 0, output: 0, duration: 0, empty: split.length === 0 };
  for (const item of split) {
    totals.input += item.input_tokens || 0;
    totals.output += item.output_tokens || 0;
    totals.duration += item.duration_ms || 0;
  }
  return totals;
}

function lineRates(row, totals) {
  const changed = row.lines && typeof row.lines.changed === "number" ? row.lines.changed : 0;
  if (!changed) return "no landed diff size";
  const regressions = row.measures && row.measures.escaped_regressions;
  const regressionText = regressions && regressions.status === "measured" && typeof regressions.actual === "number"
    ? `${num(regressions.actual * 1000 / changed)} escaped regressions`
    : "escaped regressions unmeasured";
  const cost = row.cost || {};
  let costText = "cost unknown";
  if (typeof cost.per_task === "number" && row.units) {
    const priced = (cost.coverage || 0) * row.units;
    const perThousand = cost.per_task * priced * 1000 / changed;
    costText = `${num(perThousand, 4)} USD${cost.cost_unknown ? " (cost unknown)" : ""}`;
  }
  const timeText = totals.empty ? "run time unavailable" : `${formatDuration(totals.duration * 1000 / changed)} run time`;
  return `${regressionText}, ${costText}, ${timeText}`;
}

function executionText(execution) {
  const counts = execution || {};
  return `Execution split: ${counts.managed_tasks || 0} managed, ${counts.interactive_tasks || 0} interactive, ${counts.unclassified_tasks || 0} unclassified.`;
}

function boardStats(payload) {
  const unattributed = payload.unattributed || {};
  const notScored = payload.not_scored || {};
  const reasons = Object.entries(notScored.reasons || {})
    .filter(([, count]) => count)
    .map(([reason, count]) => `${count} ${String(reason).replace(/_/g, " ")}`);
  const reasonText = reasons.length ? ` (${reasons.join(", ")})` : "";
  return `Board: ${unattributed.tasks || 0} unattributed tasks. Not-scored problems: ${notScored.count || 0}${reasonText}.`;
}

function chartNodes(rows) {
  const tiers = qualifyingTiers(rows);
  if (!tiers.length) {
    const note = el("p", {
      class: "agents-rank-note",
      role: "note",
      text: "No tier has at least 3 models with at least 10 tasks, so there is no score-against-cost chart.",
    });
    note.dataset.key = "charts-empty";
    note.dataset.hash = "none";
    return [note];
  }
  return tiers.map(({ tier, points }) => chartNode(tier, points));
}

function qualifyingTiers(rows) {
  const byTier = new Map();
  for (const row of rows) {
    const points = Array.isArray(row.tiers) ? row.tiers : [];
    for (const point of points) {
      if (!point || (point.tasks || 0) < MIN_TASKS) continue;
      const list = byTier.get(point.tier) || [];
      list.push({ model: row.model, ...point });
      byTier.set(point.tier, list);
    }
  }
  const tiers = [];
  for (const [tier, points] of byTier) {
    if (points.length >= MIN_MODELS) tiers.push({ tier, points });
  }
  tiers.sort((a, b) => tierIndex(a.tier) - tierIndex(b.tier) || String(a.tier).localeCompare(String(b.tier)));
  return tiers;
}

function tierIndex(tier) {
  const index = TIER_ORDER.indexOf(tier);
  return index < 0 ? TIER_ORDER.length : index;
}

function chartNode(tier, points) {
  const split = splitFrontier(points);
  const label = tierLabel(tier);
  const section = el("section", { class: "agents-rank-chart", "aria-label": `${label} score against cost` }, [
    el("div", { class: "agents-rank-chart-head" }, [
      el("h3", { text: `${label}: score against cost per task` }),
      pinnedInfoHint("Each point is that tier's score, 100 + 10 × z on the tier's tasks only, before the board's shrinkage, against recorded cost per task on the tier. A frontier model is one no other plotted model beats on both a higher score and a lower cost. Models with unknown cost are counted toward the tier and are not plotted."),
    ]),
  ]);
  section.dataset.tier = tier || "";
  if (!split.plot.length) {
    section.append(el("p", {
      class: "agents-rank-note",
      text: `${points.length} models have at least 10 tasks on ${label}, but none has both a score and a known cost to plot.`,
    }));
  } else {
    section.append(scatterPlot(split), legendNode(split), captionNode(split));
  }
  section.dataset.key = `chart-${tier}`;
  section.dataset.hash = JSON.stringify(points.map((point) => [point.model, point.score, point.cost_per_task, point.cost_unknown, point.tasks]));
  return section;
}

function splitFrontier(points) {
  const plot = points.filter((point) => typeof point.score === "number"
    && !point.cost_unknown
    && typeof point.cost_per_task === "number");
  const front = plot.filter((point) => !plot.some((other) => other !== point
    && other.score > point.score
    && other.cost_per_task < point.cost_per_task));
  const plotted = new Set(plot);
  const omitted = points.filter((point) => !plotted.has(point));
  return { plot, front, omitted };
}

function scatterPlot(split) {
  const width = 640;
  const height = 320;
  const left = 52;
  const right = 16;
  const top = 16;
  const bottom = 36;
  const costs = split.plot.map((point) => point.cost_per_task);
  const scores = split.plot.map((point) => point.score);
  let x0 = Math.min(...costs);
  let x1 = Math.max(...costs);
  let y0 = Math.min(...scores);
  let y1 = Math.max(...scores);
  if (x0 === x1) { x0 -= 1; x1 += 1; }
  if (y0 === y1) { y0 -= 1; y1 += 1; }
  const xPad = (x1 - x0) * 0.08;
  const yPad = (y1 - y0) * 0.08;
  x0 -= xPad;
  x1 += xPad;
  y0 -= yPad;
  y1 += yPad;
  const xOf = (cost) => left + ((cost - x0) / (x1 - x0)) * (width - left - right);
  const yOf = (score) => top + ((y1 - score) / (y1 - y0)) * (height - top - bottom);
  const names = split.front.map((point) => point.model).join(", ");
  const svg = svgEl("svg", {
    class: "agents-rank-plot",
    viewBox: `0 0 ${width} ${height}`,
    role: "img",
    "aria-label": `Score against cost per task. Frontier: ${names || "none"}.`,
  });
  svg.append(
    svgEl("line", { class: "agents-rank-axis", x1: left, y1: top, x2: left, y2: height - bottom }),
    svgEl("line", { class: "agents-rank-axis", x1: left, y1: height - bottom, x2: width - right, y2: height - bottom }),
  );
  const xLabel = svgEl("text", { class: "agents-rank-axis-label", x: (left + width - right) / 2, y: height - 8, "text-anchor": "middle" });
  xLabel.textContent = "cost per task";
  const yLabel = svgEl("text", {
    class: "agents-rank-axis-label",
    x: 14,
    y: (top + height - bottom) / 2,
    "text-anchor": "middle",
    transform: `rotate(-90 14 ${(top + height - bottom) / 2})`,
  });
  yLabel.textContent = "score";
  svg.append(xLabel, yLabel);
  const sorted = [...split.front].sort((a, b) => a.cost_per_task - b.cost_per_task || String(a.model).localeCompare(String(b.model)));
  if (sorted.length >= 2) {
    svg.append(svgEl("polyline", {
      class: "agents-rank-front",
      points: sorted.map((point) => `${xOf(point.cost_per_task).toFixed(1)},${yOf(point.score).toFixed(1)}`).join(" "),
    }));
  }
  const frontier = new Set(split.front.map((point) => point.model));
  for (const point of split.plot) {
    const marked = frontier.has(point.model);
    const dot = svgEl("circle", {
      class: marked ? "agents-rank-dot is-frontier" : "agents-rank-dot",
      cx: xOf(point.cost_per_task).toFixed(1),
      cy: yOf(point.score).toFixed(1),
      r: marked ? 6 : 4.5,
    });
    dot.setAttribute("data-frontier", marked ? "true" : "false");
    dot.setAttribute("data-model", point.model || "");
    const title = svgEl("title", {});
    title.textContent = `${point.model}: score ${num(point.score)}, cost ${num(point.cost_per_task, 4)}${marked ? ", frontier" : ""}`;
    dot.append(title);
    svg.append(dot);
  }
  return svg;
}

function legendNode(split) {
  const frontier = new Set(split.front.map((point) => point.model));
  const list = el("ul", { class: "agents-rank-legend" });
  const ordered = [...split.plot].sort((a, b) => String(a.model).localeCompare(String(b.model)));
  for (const point of ordered) {
    const marked = frontier.has(point.model);
    const mark = el("span", { class: "agents-rank-frontier-label", text: marked ? "frontier" : "" });
    if (!marked) mark.hidden = true;
    list.append(el("li", { class: marked ? "is-frontier" : "" }, [
      el("span", { class: marked ? "agents-rank-swatch is-frontier" : "agents-rank-swatch", "aria-hidden": "true" }),
      el("span", { text: point.model || "—" }),
      mark,
    ]));
  }
  return list;
}

function captionNode(split) {
  const names = split.front.map((point) => point.model).join(", ") || "none";
  const omitted = split.omitted.length
    ? ` ${split.omitted.length === 1 ? "1 model is" : `${split.omitted.length} models are`} not plotted (${split.omitted.map((point) => point.model).join(", ")}).`
    : "";
  return el("p", { class: "agents-rank-note", text: `Frontier: ${names}.${omitted}` });
}

function howNode(payload) {
  const measures = Array.isArray(payload.measures) ? payload.measures : [];
  const list = el("ul", { class: "agents-rank-how-list" });
  for (const measure of measures) {
    const weight = typeof measure.weight === "number" ? ` weight ${measure.weight}` : "";
    const status = measure.status === "measured" ? "" : `, ${measure.status || "unmeasured"}`;
    list.append(el("li", { text: `${measure.label || measure.key}${weight}${status}. ${measure.definition || ""}`.trim() }));
  }
  const node = el("details", { class: "agents-how agents-disclosure" }, [
    el("summary", { text: "How the score works" }),
    el("p", { text: "The score starts at 100. Each measured residual, already signed so that positive is better, is divided by the noise scale and weighted. The result shrinks toward the other eligible models. Bands are shared ranks such as 1=. An unqualified board shows scores without bands. Cost orders a band and never changes it. A chart point is 100 + 10 × z on that tier's tasks only, before this shrinkage." }),
    list,
  ]);
  node.dataset.key = "how";
  node.dataset.hash = JSON.stringify(measures.map((measure) => [measure.key, measure.status, measure.weight]));
  return node;
}

function tierLabel(tier) {
  if (!tier) return "Tier";
  if (tier === "xhard") return "Xhard";
  return tier.charAt(0).toUpperCase() + tier.slice(1);
}

function num(value, digits = 2) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  return value.toLocaleString(undefined, { maximumFractionDigits: digits });
}

function formatDuration(ms) {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms <= 0) return "0s";
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

function svgEl(name, attrs) {
  const node = document.createElementNS(SVG_NS, name);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value != null) node.setAttribute(key, String(value));
  }
  return node;
}
