// Analytics › Agents › Rank. Reads GET /api/agents/rank and draws, top to
// bottom: summary cards; the band ladder (one group per shared rank, each row
// with an interval whisker on one shared score axis, its per-measure
// contributions, cost against the fleet, tier mix and units, and a Breakdown
// under it); the models below the evidence gate with their progress toward
// it; the tasks no one was scored on; and one score-against-cost chart with a
// tier switch (js/agents-rank-frontier.js). Everything is derived from the
// payload; HTML uses `el()`.

import { el, syncNodes, fetchJson, requestPanel, pinnedInfoHint } from './common.js';
import * as rankFormat from './agents-rank-format.js';
import { rankFrontierNode } from './agents-rank-frontier.js';

const BODY_ID = "agents-body";
const COUNT_ID = "agents-count";
const TIER_SHORT = { low: "L", medium: "M", hard: "H", xhard: "X", unassessed: "U" };
// The cost bar fills at 2^±1.5, about 0.35× and 2.8× the fleet.
const COST_LOG_SPAN = 1.5;
// A contribution smaller than this reads as neutral.
const NEUTRAL_POINTS = 0.5;
const MEASURE_SHORT = { escaped_regressions: "Regr.", review_outcome: "Review", crew_rework: "Rework" };
const NOT_SCORED_REASONS = {
  infrastructure: "rework caused by infrastructure",
  no_cause: "rework with no recorded cause",
  spec: "problems caused by the task's spec",
  unattributed: "no attribution",
  no_run: "no implementation run to charge",
  no_review: "no settled review to charge",
  no_diff_expected: "no diff expected",
  merge_reintroduced: "regressions a merge reintroduced",
  contract_drift: "contract-drift regressions",
  test_not_selected: "regressions no selected test covered",
  platform_only: "platform-only regressions",
  nondeterministic: "nondeterministic regressions",
  unclassified: "unclassified regressions",
};

// Reader state that survives polls: which breakdowns are open.
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
    const ladder = ladderModel(payload, rows);
    nodes.push(summaryNode(payload, rows, ladder));
    nodes.push(ladderNode(payload, ladder));
    nodes.push(asideNode(payload, rows));
    nodes.push(rankFrontierNode(payload, rows));
  }
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
  if (typeof payload.tasks_scanned === "number") lines.push(`${rankFormat.num(payload.tasks_scanned, 0)} tasks scanned.`);
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

// --- the ladder's model: bands, their notes and the shared score axis ---

function ladderModel(payload, rows) {
  const ranked = rows.filter((row) => row.eligible);
  const pending = rows.filter((row) => !row.eligible);
  const bands = [];
  if (payload.qualified) {
    for (const row of ranked) {
      const label = row.rank_label == null ? "" : String(row.rank_label);
      const last = bands[bands.length - 1];
      if (last && last.label === label) last.rows.push(row);
      else bands.push({ label, rows: [row] });
    }
  } else if (ranked.length) {
    bands.push({ label: "", rows: [...ranked].sort((a, b) => (b.score ?? -Infinity) - (a.score ?? -Infinity)) });
  }
  const pairs = payload.paired_probabilities || {};
  bands.forEach((band, index) => {
    band.name = payload.qualified ? `Band ${index + 1}` : "Scores, no bands";
    band.note = inBandNote(band.rows, pairs);
    const next = bands[index + 1];
    band.edge = next ? minAcross(band.rows, next.rows, pairs) : null;
    band.nextName = next ? `band ${index + 2}` : "";
  });
  return { ranked, pending, bands, axis: scoreAxis(ranked) };
}

function probability(pairs, a, b) {
  const row = pairs[a];
  return row && typeof row[b] === "number" ? row[b] : null;
}

function inBandNote(rows, pairs) {
  if (rows.length < 2) return "";
  const ordered = [...rows].sort((a, b) => (b.score ?? 0) - (a.score ?? 0));
  const parts = [];
  for (let i = 0; i < ordered.length; i += 1) {
    for (let j = i + 1; j < ordered.length; j += 1) {
      const p = probability(pairs, ordered[i].model, ordered[j].model);
      if (p != null) parts.push(`${ordered[i].model} vs ${ordered[j].model} ${rankFormat.num(p, 2)}`);
    }
  }
  if (!parts.length) return "";
  const shown = parts.slice(0, 6);
  if (parts.length > shown.length) shown.push(`${parts.length - shown.length} more pairs`);
  return shown.join(" · ");
}

function minAcross(upper, lower, pairs) {
  let min = null;
  for (const a of upper) {
    for (const b of lower) {
      const p = probability(pairs, a.model, b.model);
      if (p != null && (min == null || p < min)) min = p;
    }
  }
  return min;
}

function scoreAxis(rows) {
  const values = [];
  for (const row of rows) {
    if (typeof row.score === "number") values.push(row.score);
    const interval = row.interval || {};
    if (typeof interval.p10 === "number") values.push(interval.p10);
    if (typeof interval.p90 === "number") values.push(interval.p90);
  }
  if (!values.length) values.push(90, 110);
  const span = Math.max(...values) - Math.min(...values);
  const step = span > 100 ? 20 : span > 50 ? 10 : 5;
  let lo = Math.floor(Math.min(...values) / step) * step;
  let hi = Math.ceil(Math.max(...values) / step) * step;
  if (lo === hi) { lo -= step; hi += step; }
  const ticks = [];
  for (let v = lo; v <= hi + 1e-9; v += step) ticks.push(v);
  return { lo, hi, ticks, pos: (v) => Math.max(0, Math.min(100, ((v - lo) / (hi - lo)) * 100)) };
}

// --- summary cards ---

function summaryNode(payload, rows, ladder) {
  const cards = [leaderCard(payload, ladder), valueCard(ladder), rankedCard(payload, rows, ladder), coverageCard(payload)];
  const node = el("section", { class: "agents-rank-summary", "aria-label": "Board summary" }, cards);
  node.dataset.key = "summary";
  node.dataset.hash = JSON.stringify(cards.map((card) => card.textContent));
  return node;
}

function card(label, value, valueRest, sub) {
  return el("div", { class: "agents-rank-card" }, [
    el("span", { class: "agents-rank-card-label", text: label }),
    el("span", { class: "agents-rank-card-value" }, [value, valueRest ? el("span", { class: "agents-rank-card-rest", text: ` ${valueRest}` }) : null]),
    el("span", { class: "agents-rank-card-sub", text: sub }),
  ]);
}

function leaderCard(payload, ladder) {
  const top = ladder.bands[0];
  if (!top) return card("Leader", "—", "", "No model has passed the evidence gate.");
  if (!payload.qualified) {
    return card("Leader", top.rows[0].model || "—", "", "Highest score; this board has no bands.");
  }
  const names = top.rows.map((row) => row.model).join(", ");
  const edge = top.edge == null ? "" : ` · ${rankFormat.num(top.edge, 2)} ahead of band 2`;
  const sub = top.rows.length === 1 ? `Alone in band 1${edge}` : `${top.rows.length} share band 1${edge}`;
  return card("Leader", names, "", sub);
}

function valueCard(ladder) {
  const priced = ladder.ranked.filter((row) => row.cost && !row.cost.cost_unknown && typeof row.cost.ratio === "number");
  if (!priced.length) return card("Best value", "—", "", "No ranked model has a known cost.");
  const best = priced.reduce((a, b) => (b.cost.ratio < a.cost.ratio ? b : a));
  const rank = best.rank_label ? `Rank ${best.rank_label}` : "Unbanded";
  return card("Best value", best.model, "", `${rank} at ${rankFormat.num(best.cost.ratio, 2)}× fleet cost`);
}

function rankedCard(payload, rows, ladder) {
  const bands = payload.qualified ? ` · ${rankFormat.plural(ladder.bands.length, "band")}` : "";
  const below = ladder.pending.length;
  return card("Ranked", String(ladder.ranked.length), `of ${rankFormat.plural(rows.length, "model")}${bands}`,
    below ? `${below} below the evidence gate` : "Every model passed the evidence gate");
}

function coverageCard(payload) {
  const measures = Array.isArray(payload.measures) ? payload.measures : [];
  const measured = measures.filter((m) => m.status === "measured").reduce((sum, m) => sum + (m.weight || 0), 0);
  const total = typeof payload.weights_total === "number" ? payload.weights_total : measures.reduce((sum, m) => sum + (m.weight || 0), 0);
  const missing = measures.filter((m) => m.status !== "measured").map((m) => m.label || m.key);
  return card("Score coverage", rankFormat.num(measured, 0), `of ${rankFormat.num(total, 0)} weight measured`,
    missing.length ? `Unmeasured: ${missing.join(", ")}` : "Every measure is measured");
}

// --- the band ladder ---

function ladderNode(payload, ladder) {
  const measures = (Array.isArray(payload.measures) ? payload.measures : []).filter((m) => m.status === "measured");
  const columns = 7 + measures.length;
  const axisHead = el("div", { class: "agents-rank-axis-head", "aria-hidden": "true" },
    ladder.axis.ticks.map((tick) => el("span", { text: String(tick), style: { left: `${ladder.axis.pos(tick)}%` } })));
  const head = el("tr", {}, [
    el("th", { text: "Rank" }),
    el("th", { text: "Model" }),
    el("th", { class: "agents-num", text: "Score" }),
    el("th", { class: "agents-rank-interval-col" }, [
      el("span", { text: "Score, 10th–90th percentile" }),
      axisHead,
    ]),
    ...measures.map((m) => el("th", { class: "agents-num", text: MEASURE_SHORT[m.key] || m.label || m.key, title: `${m.label || m.key} contribution, points added to 100` })),
    el("th", { text: "Cost vs fleet", title: "List-price cost per task divided by the fleet's on the same tier mix, on a log scale centred at 1.0×" }),
    el("th", { text: "Tier mix" }),
    el("th", { class: "agents-num", text: "Units" }),
  ]);
  const groups = ladder.bands.map((band) => bandBody(payload, band, measures, ladder.axis, columns));
  const table = el("table", { class: "agents-table agents-rank-ladder" }, [el("thead", {}, [head]), ...groups]);
  const wrap = el("div", { class: "agents-table-wrap agents-rank-ladder-wrap", role: "region", "aria-label": "Model ranking" }, [table]);
  wrap.tabIndex = 0;
  const empty = ladder.bands.length ? null : el("p", { class: "agents-rank-note", text: "No model has passed the evidence gate yet." });
  const node = el("section", { class: "agents-rank-board", "aria-label": "Model ranking" }, [
    empty || wrap,
    legendNode(),
  ]);
  node.dataset.key = "board";
  node.dataset.hash = JSON.stringify([ladder.ranked, payload.paired_probabilities, payload.qualified, payload.measures]);
  return node;
}

function bandBody(payload, band, measures, axis, columns) {
  const headCell = el("th", { class: "agents-rank-band-head" }, [
    el("span", { class: "agents-rank-band-name", text: band.name }),
    band.note ? el("span", { class: "agents-rank-band-note", text: band.note, title: "Probability the first model's score is above the second's. Close to 0.5 is a tie." }) : null,
  ]);
  headCell.colSpan = columns;
  headCell.setAttribute("scope", "rowgroup");
  const rows = [el("tr", { class: "agents-rank-band-row" }, [headCell])];
  for (const row of band.rows) rows.push(...ladderRow(payload, row, measures, axis, columns));
  if (band.edge != null) {
    const edge = el("td", { class: "agents-rank-edge" }, [
      el("span", { class: "agents-rank-edge-rule", "aria-hidden": "true" }),
      el("span", { text: `P(ahead of ${band.nextName}) ≥ ${rankFormat.num(band.edge, 2)}` }),
    ]);
    edge.colSpan = columns;
    rows.push(el("tr", { class: "agents-rank-edge-row" }, [edge]));
  }
  return el("tbody", { class: "agents-rank-band" }, rows);
}

function ladderRow(payload, row, measures, axis, columns) {
  const toggle = breakdownToggle(row, "agents-rank-model-button");
  const main = el("tr", { class: "agents-rank-row" }, [
    el("td", { class: "agents-rank-label", text: rankText(payload, row) }),
    el("td", { class: "agents-rank-model" }, [rankFormat.providerDot(row.model), toggle.button]),
    el("td", { class: "agents-num agents-rank-score", text: rankFormat.num(row.score) }),
    el("td", { class: "agents-rank-interval-col" }, [whiskerNode(row, axis)]),
    ...measures.map((m) => contributionCell(row, m)),
    el("td", {}, [costNode(row)]),
    el("td", {}, [tierMixNode(row)]),
    el("td", { class: "agents-num agents-muted", text: row.units == null ? "—" : String(row.units) }),
  ]);
  main.dataset.model = row.model || "";
  main.style.setProperty("--provider", rankFormat.providerColor(row.model));
  const detailCell = el("td", {}, [breakdownNode(payload, row)]);
  detailCell.colSpan = columns;
  const detail = el("tr", { class: "agents-rank-detail" }, [detailCell]);
  toggle.bind(detail);
  return [main, detail];
}

// The model name opens its Breakdown under the row (a table row or a block).
function breakdownToggle(row, className) {
  const open = openModels.has(row.model);
  const button = el("button", {
    class: className,
    type: "button",
    "aria-expanded": open ? "true" : "false",
    title: `${row.model || "Model"}: show or hide the breakdown`,
  }, [
    el("span", { class: "agents-rank-model-name", text: row.model || "—" }),
    el("span", { class: "agents-rank-chevron", "aria-hidden": "true", text: "▸" }),
  ]);
  const bind = (detail) => {
    detail.id = breakdownId(row.model);
    detail.hidden = !open;
    button.setAttribute("aria-controls", detail.id);
    button.addEventListener("click", () => {
      const next = !openModels.has(row.model);
      if (next) openModels.add(row.model);
      else openModels.delete(row.model);
      detail.hidden = !next;
      button.setAttribute("aria-expanded", next ? "true" : "false");
    });
  };
  return { button, bind };
}

function breakdownId(model) {
  return `agents-rank-breakdown-${String(model || "model").replace(/[^A-Za-z0-9_-]/g, "-")}`;
}

function rankText(payload, row) {
  if (!payload.qualified || row.rank_label == null || row.rank_label === "") return "—";
  return String(row.rank_label);
}

function whiskerNode(row, axis) {
  const interval = row.interval || {};
  const hasInterval = typeof interval.p10 === "number" && typeof interval.p90 === "number";
  const label = hasInterval
    ? `${row.model} score ${rankFormat.num(row.score)}, interval ${rankFormat.num(interval.p10, 1)} to ${rankFormat.num(interval.p90, 1)}`
    : `${row.model} score ${rankFormat.num(row.score)}, no interval`;
  const node = el("div", { class: "agents-rank-whisker", role: "img", "aria-label": label, title: label });
  for (const tick of axis.ticks) node.append(el("span", { class: "agents-rank-grid", style: { left: `${axis.pos(tick)}%` } }));
  if (hasInterval) {
    const lo = axis.pos(interval.p10);
    const hi = axis.pos(interval.p90);
    node.append(
      el("span", { class: "agents-rank-whisker-line", style: { left: `${lo}%`, width: `${Math.max(0, hi - lo)}%` } }),
      el("span", { class: "agents-rank-whisker-cap", style: { left: `${lo}%` } }),
      el("span", { class: "agents-rank-whisker-cap", style: { left: `${hi}%` } }),
    );
    const flip = hi > 72;
    const text = el("span", {
      class: flip ? "agents-rank-whisker-text is-left" : "agents-rank-whisker-text",
      text: `${rankFormat.num(interval.p10, 1)}–${rankFormat.num(interval.p90, 1)}`,
      style: { left: `${flip ? lo : hi}%` },
    });
    node.append(text);
  }
  if (typeof row.score === "number") node.append(el("span", { class: "agents-rank-whisker-dot", style: { left: `${axis.pos(row.score)}%` } }));
  return node;
}

function contributionCell(row, measure) {
  const cell = (row.measures || {})[measure.key] || {};
  if (cell.status !== "measured" || typeof cell.contribution !== "number") {
    return el("td", { class: "agents-num agents-rank-contrib", text: "—", title: cell.reason || "unmeasured" });
  }
  const value = cell.contribution;
  const tone = Math.abs(value) < NEUTRAL_POINTS ? "is-flat" : value > 0 ? "is-up" : "is-down";
  return el("td", {
    class: `agents-num agents-rank-contrib ${tone}`,
    text: signed(value),
    title: `${measure.label || measure.key}: actual ${rankFormat.num(cell.actual)}, expected ${rankFormat.num(cell.expected)} over ${cell.units ?? "—"} ${cell.unit || ""}`.trim(),
  });
}

function costNode(row) {
  const cost = row.cost || {};
  const coverage = typeof cost.coverage === "number" ? `${rankFormat.num(cost.coverage * 100, 0)}% of tasks priced` : "coverage unknown";
  if (cost.cost_unknown || typeof cost.ratio !== "number" || cost.ratio <= 0) {
    return el("span", { class: "agents-rank-cost is-unknown", text: cost.cost_unknown ? "cost unknown" : "—", title: coverage });
  }
  const scaled = Math.max(-1, Math.min(1, Math.log2(cost.ratio) / COST_LOG_SPAN));
  const half = Math.abs(scaled) * 50;
  const cheaper = cost.ratio < 1;
  const bar = el("span", { class: "agents-rank-costbar", "aria-hidden": "true" }, [
    el("span", { class: "agents-rank-costbar-mid" }),
    el("span", { class: "agents-rank-costbar-fill", style: { left: `${cheaper ? 50 - half : 50}%`, width: `${half}%` } }),
  ]);
  return el("span", {
    class: `agents-rank-cost ${cheaper ? "is-cheaper" : "is-dearer"}`,
    title: `${rankFormat.num(cost.ratio, 2)}× the fleet's cost per task on the same tier mix. ${coverage}.`,
  }, [bar, el("span", { class: "agents-rank-cost-text", text: `${rankFormat.num(cost.ratio, 2)}×` })]);
}

function tierEntries(row) {
  const byTier = row.tasks_by_tier || {};
  return Object.entries(byTier)
    .filter(([, count]) => typeof count === "number" && count > 0)
    .sort(([a], [b]) => rankFormat.tierIndex(a) - rankFormat.tierIndex(b));
}

function tierMixNode(row) {
  const entries = tierEntries(row);
  const total = entries.reduce((sum, [, count]) => sum + count, 0);
  const text = entries.map(([tier, count]) => `${count} ${TIER_SHORT[tier] || tier}`).join(" · ");
  const title = entries.map(([tier, count]) => `${count} ${tier}`).join(", ") || "no tasks by tier";
  const bar = el("span", { class: "agents-rank-mix", "aria-hidden": "true" },
    entries.map(([tier, count]) => el("span", { class: `agents-rank-mix-${tier}`, style: { width: `${total ? (count * 100) / total : 0}%` } })));
  return el("span", { class: "agents-rank-mixcell", title }, [bar, el("span", { class: "agents-rank-mix-text", text: text || "—" })]);
}

function legendNode() {
  const swatch = (tier) => el("span", { class: "agents-rank-legend-item" }, [
    el("span", { class: `agents-rank-legend-swatch agents-rank-mix-${tier}`, "aria-hidden": "true" }),
    el("span", { text: tier }),
  ]);
  return el("div", { class: "agents-rank-legend" }, [
    el("span", {}, [
      "Contribution is the points each measure adds to 100. ",
      el("span", { class: "agents-rank-contrib is-up", text: "Blue" }), " helps, ",
      el("span", { class: "agents-rank-contrib is-down", text: "amber" }), " hurts, — is unmeasured. ",
      "Inside a band, rows are ordered by cost and the order is not a ranking.",
    ]),
    ...["low", "medium", "hard", "xhard", "unassessed"].map(swatch),
  ]);
}

// --- not yet ranked and not scored ---

function asideNode(payload, rows) {
  const pending = rows.filter((row) => !row.eligible);
  const node = el("div", { class: "agents-rank-aside" }, [
    pending.length ? pendingNode(payload, pending) : null,
    notScoredNode(payload),
  ]);
  node.dataset.key = "aside";
  node.dataset.hash = JSON.stringify([pending, payload.rules, payload.not_scored, payload.unattributed]);
  return node;
}

function gateProgress(rules, row) {
  const minTasks = typeof rules.min_tasks === "number" ? rules.min_tasks : 0;
  const minTiers = typeof rules.min_tiers === "number" ? rules.min_tiers : 0;
  const minTierTasks = typeof rules.min_tier_tasks === "number" ? rules.min_tier_tasks : 0;
  const units = row.units || 0;
  const tiers = tierEntries(row).filter(([tier, count]) => tier !== "unassessed" && count >= minTierTasks).length;
  const parts = [];
  const fractions = [];
  if (minTasks) {
    fractions.push(Math.min(1, units / minTasks));
    if (units < minTasks) parts.push(`${units} / ${minTasks} tasks`);
  }
  if (minTiers) {
    fractions.push(Math.min(1, tiers / minTiers));
    if (tiers < minTiers) parts.push(`${tiers} / ${rankFormat.plural(minTiers, "tier")}`);
  }
  const progress = fractions.length ? Math.min(...fractions) : 0;
  const text = parts.length ? parts.join(" · ") : (row.ineligible_reason || "not eligible");
  return { progress, text };
}

function pendingNode(payload, pending) {
  const rules = payload.rules || {};
  const gate = [];
  if (rules.min_tasks) gate.push(`${rules.min_tasks} implemented tasks`);
  if (rules.min_tiers) gate.push(`${rankFormat.plural(rules.min_tiers, "tier")} with ${rules.min_tier_tasks || 0}+ tasks each`);
  const items = pending
    .map((row) => ({ row, gate: gateProgress(rules, row) }))
    .sort((a, b) => b.gate.progress - a.gate.progress || (b.row.units || 0) - (a.row.units || 0));
  const list = el("ul", { class: "agents-rank-pending-list" }, items.map(({ row, gate: progress }) => {
    const toggle = breakdownToggle(row, "agents-rank-model-button is-muted");
    const raw = typeof row.raw_score === "number" ? row.raw_score : row.score;
    const line = el("div", { class: "agents-rank-pending-row" }, [
      el("span", { class: "agents-rank-model" }, [rankFormat.providerDot(row.model), toggle.button]),
      el("span", { class: "agents-rank-progress", role: "img", "aria-label": `${rankFormat.num(progress.progress * 100, 0)}% of the evidence gate` }, [
        el("span", { style: { width: `${progress.progress * 100}%` } }),
      ]),
      el("span", { class: "agents-rank-pending-gate", text: progress.text, title: row.ineligible_reason || "" }),
      el("span", { class: "agents-num agents-rank-pending-score", title: "Unshrunk score; not comparable to ranked rows" }, [
        rankFormat.num(raw), el("span", { class: "agents-rank-raw", text: " raw" }),
      ]),
    ]);
    const detail = el("div", { class: "agents-rank-detail" }, [breakdownNode(payload, row)]);
    toggle.bind(detail);
    return el("li", {}, [line, detail]);
  }));
  return el("section", { class: "agents-rank-box agents-rank-pending", "aria-label": "Not yet ranked" }, [
    el("div", { class: "agents-rank-box-head" }, [
      el("h3", {}, ["Not yet ranked ", el("span", { class: "agents-rank-box-count", text: String(pending.length) })]),
      gate.length ? el("span", { class: "agents-rank-box-meta", text: `Gate: ${gate.join(" and ")}` }) : null,
    ]),
    list,
  ]);
}

function notScoredNode(payload) {
  const notScored = payload.not_scored || {};
  const unattributed = payload.unattributed || {};
  const reasons = Object.entries(notScored.reasons || {}).filter(([, count]) => count > 0).sort((a, b) => b[1] - a[1]);
  const max = reasons.length ? reasons[0][1] : 0;
  const crews = Array.isArray(unattributed.crews) ? [...unattributed.crews].sort((a, b) => (b.tasks || 0) - (a.tasks || 0)) : [];
  const crewText = crews.map((crew) => `${crew.crew || "—"} ${crew.tasks ?? 0}`).join(", ");
  const list = reasons.length
    ? el("ul", { class: "agents-rank-reasons" }, reasons.map(([reason, count]) => el("li", { title: NOT_SCORED_REASONS[reason] || reason.replace(/_/g, " ") }, [
      el("span", { class: "agents-rank-reason-key", text: reason.replace(/_/g, " ") }),
      el("span", { class: "agents-rank-reason-bar", "aria-hidden": "true" }, [el("span", { style: { width: `${max ? (count * 100) / max : 0}%` } })]),
      el("span", { class: "agents-num", text: rankFormat.num(count, 0) }),
    ])))
    : el("p", { class: "agents-rank-note", text: "Every problem in this window was charged to a model." });
  return el("section", { class: "agents-rank-box agents-rank-unscored", "aria-label": "Not scored" }, [
    el("div", { class: "agents-rank-box-head" }, [
      el("h3", {}, ["Not scored ", el("span", { class: "agents-rank-box-count", text: rankFormat.plural(notScored.count || 0, "problem") })]),
      el("span", {
        class: "agents-rank-box-meta",
        text: `${rankFormat.num(unattributed.tasks || 0, 0)} unattributed tasks`,
        title: crewText ? `Tasks with no model to charge, by crew: ${crewText}` : "",
      }),
    ]),
    list,
  ]);
}

// --- the breakdown under a row ---

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
      el("td", { class: "agents-num", text: unmarked ? rankFormat.num(cell.actual) : "—" }),
      el("td", { class: "agents-num", text: unmarked ? rankFormat.num(cell.expected) : "—" }),
      el("td", { class: "agents-num", text: unmarked ? rankFormat.num(cell.residual) : "—" }),
      el("td", { class: "agents-num", text: unmarked ? rankFormat.num(cell.contribution) : "—" }),
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
      el("td", { class: "agents-num", text: rankFormat.num((item.input_tokens || 0) + (item.output_tokens || 0), 0), title: `${item.input_tokens || 0} in, ${item.output_tokens || 0} out` }),
    ]))),
  ]);
}

function shownNode(payload, row) {
  const totals = effortTotals(row);
  const cost = row.cost || {};
  const lines = [
    `Cost per task: ${typeof cost.per_task === "number" ? rankFormat.num(cost.per_task, 4) : "—"}${cost.cost_unknown ? " (cost unknown)" : ""}. Coverage ${typeof cost.coverage === "number" ? rankFormat.num(cost.coverage * 100, 0) + "%" : "—"}.`,
    totals.empty
      ? "Tokens and run time: — (no completed implementation invocations)."
      : `Tokens: ${rankFormat.num(totals.input + totals.output, 0)} (${rankFormat.num(totals.input, 0)} in, ${rankFormat.num(totals.output, 0)} out). Run time: ${formatDuration(totals.duration)}, the sum of completed implementation invocations, not queue or CI.`,
    `Landed rate: ${typeof row.landed_rate === "number" ? rankFormat.num(row.landed_rate * 100, 1) + "%" : "—"} (${rankFormat.num(row.landed_tasks, 2)} landed of ${row.units ?? 0}).`,
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
    ? `${rankFormat.num(regressions.actual * 1000 / changed)} escaped regressions`
    : "escaped regressions unmeasured";
  const cost = row.cost || {};
  let costText = "cost unknown";
  if (typeof cost.per_task === "number" && row.units) {
    const priced = (cost.coverage || 0) * row.units;
    const perThousand = cost.per_task * priced * 1000 / changed;
    costText = `${rankFormat.num(perThousand, 4)} USD${cost.cost_unknown ? " (cost unknown)" : ""}`;
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

function howNode(payload) {
  const measures = Array.isArray(payload.measures) ? payload.measures : [];
  const list = el("ul", { class: "agents-rank-how-list" });
  for (const measure of measures) {
    const weight = typeof measure.weight === "number" ? ` weight ${measure.weight}` : "";
    const status = measure.status === "measured" ? "" : `, ${measure.status || "unmeasured"}`;
    list.append(el("li", { text: `${measure.label || measure.key}${weight}${status}. ${measure.definition || ""}`.trim() }));
  }
  const rules = payload.rules || {};
  const draws = typeof rules.draws === "number" ? `${rankFormat.num(rules.draws, 0)} draws give` : "Draws give";
  const band = typeof rules.band_probability === "number" ? rankFormat.num(rules.band_probability, 2) : "the band threshold";
  const node = el("details", { class: "agents-how agents-disclosure" }, [
    el("summary", { text: "How the score works" }),
    el("p", { text: `The score starts at 100. Each measured residual, already signed so that positive is better, is divided by the noise scale and weighted; a measure's contribution is the points it adds. The result shrinks toward the other eligible models (k = ${rankFormat.num(rules.shrinkage_k, 0)}), and ${draws} the 10th–90th percentile interval. Models share a band while the probability that one is ahead of the next stays below ${band}. An unqualified board shows scores without bands. Cost orders a band and never changes it. A chart point is 100 + 10 × z on that tier's tasks only, before this shrinkage.` }),
    list,
  ]);
  node.dataset.key = "how";
  node.dataset.hash = JSON.stringify([measures.map((measure) => [measure.key, measure.status, measure.weight]), rules]);
  return node;
}

// --- small helpers ---

function signed(value) {
  if (Math.abs(value) < 0.05) return "0.0";
  return `${value > 0 ? "+" : "−"}${Math.abs(value).toFixed(1)}`;
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
