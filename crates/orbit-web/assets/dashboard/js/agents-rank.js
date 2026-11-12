// Analytics › Agents › Rank. Reads GET /api/agents/rank and draws, top to
// bottom: a summary strip; the refined rank table (one row per model, grouped
// by band, with a p10–p90 whisker on one shared score axis, cost per task,
// a tier-mix bar and landed rate, and a breakdown under each row); the models
// below the evidence gate in a collapsible Not ranked section; the tasks no
// one was scored on; and one score-against-cost chart with a tier switch
// (js/agents-rank-frontier.js). Everything is derived from the payload; HTML
// uses `el()`.

import { el, syncNodes, fetchJson, requestPanel, pinnedInfoHint, formatDateTime } from './common.js';
import * as rankFormat from './agents-rank-format.js';
import { rankFrontierNode } from './agents-rank-frontier.js';

const BODY_ID = "agents-body";
const COUNT_ID = "agents-count";
const TIER_SHORT = { low: "L", medium: "M", hard: "H", xhard: "X", unassessed: "U" };
// A contribution bar fills half its track at this many points.
const CONTRIB_SCALE = 25;
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

// Reader state that survives polls: which breakdowns are open, and whether the
// reader collapsed the "Not ranked" section.
const openModels = new Set();
let pendingOpen = true;

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
  if (payload.scales_source === "window_estimate") nodes.push(bannerNode("scales", "Noise scales are estimated from this window, not a calibrated season. Treat close bands as provisional."));
  if (payload.status !== "ok") {
    nodes.push(stateNode("not-ok", "Ranking is unavailable", "The ranking did not return a board for this window."));
  } else if (!rows.length) {
    nodes.push(stateNode("empty", "No models in this window", "Widen the window to rank implementer models."));
  } else {
    const ladder = ladderModel(payload, rows);
    nodes.push(summaryNode(payload, rows, ladder));
    nodes.push(ladderNode(payload, ladder));
    if (ladder.pending.length) nodes.push(pendingNode(payload, ladder.pending));
    nodes.push(notScoredNode(payload));
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
    `Counting ${payload.execution || "managed"} tasks.`,
  ];
  if (payload.settle_lag) lines.push(`Settled boards end ${payload.settle_lag} before the requested instant.`);
  if (typeof payload.tasks_scanned === "number") lines.push(`${rankFormat.num(payload.tasks_scanned, 0)} tasks scanned.`);
  if (payload.computed_at) lines.push(`Computed ${formatDateTime(payload.computed_at)}.`);
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

// --- the table's model: bands and the shared score axis ---

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
  return { ranked, pending, bands, axis: scoreAxis(ranked) };
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

// --- summary strip ---

function summaryNode(payload, rows, ladder) {
  const measures = Array.isArray(payload.measures) ? payload.measures : [];
  const liveWeight = measures.filter((m) => m.status === "measured").reduce((sum, m) => sum + (m.weight || 0), 0);
  const totalWeight = typeof payload.weights_total === "number" ? payload.weights_total : measures.reduce((sum, m) => sum + (m.weight || 0), 0);
  const missing = measures.filter((m) => m.status !== "measured").map((m) => m.label || m.key);
  const tasksScored = ladder.ranked.reduce((sum, row) => sum + (row.units || 0), 0);
  const notScored = payload.not_scored || {};
  const unattributed = payload.unattributed || {};
  const stats = [
    stat("Ranked", String(ladder.ranked.length), `of ${rankFormat.plural(rows.length, "model")}`),
    stat("Tasks scored", rankFormat.num(tasksScored, 0), ""),
    stat("Not scored", rankFormat.num(notScored.count || 0, 0), unattributed.tasks ? `${rankFormat.num(unattributed.tasks, 0)} unattributed` : ""),
    stat("Measures live", rankFormat.num(liveWeight, 0), `of ${rankFormat.num(totalWeight, 0)} weight${missing.length ? ` · unmeasured: ${missing.join(", ")}` : ""}`),
  ];
  const node = el("section", { class: "agents-rank-summary", "aria-label": "Board summary" }, stats);
  node.dataset.key = "summary";
  node.dataset.hash = JSON.stringify(stats.map((card) => card.textContent));
  return node;
}

function stat(label, value, rest) {
  return el("div", { class: "agents-rank-stat" }, [
    el("span", { class: "agents-rank-stat-label", text: label }),
    el("span", { class: "agents-rank-stat-value" }, [value, rest ? el("span", { class: "agents-rank-stat-rest", text: ` ${rest}` }) : null]),
  ]);
}

// --- the refined rank table ---

const RANK_COLUMNS = 8;

function ladderNode(payload, ladder) {
  const axisHead = el("div", { class: "agents-rank-axis-head", "aria-hidden": "true" },
    ladder.axis.ticks.map((tick) => {
      const span = el("span", { text: String(tick), style: { left: `${ladder.axis.pos(tick)}%` } });
      if (tick === 100) span.className = "is-100";
      return span;
    }));
  const head = el("tr", {}, [
    el("th", { class: "agents-rank-chev-col", text: "" }),
    el("th", { text: "Rank" }),
    el("th", { text: "Model" }),
    el("th", { class: "agents-num", text: "Score" }),
    el("th", { class: "agents-rank-interval-col" }, [
      el("span", { text: "p10–p90 interval" }),
      axisHead,
    ]),
    el("th", { class: "agents-num", text: "Cost / task" }),
    el("th", { text: "Tasks" }),
    el("th", { class: "agents-num", text: "Landed" }),
  ]);
  const groups = ladder.bands.map((band) => bandBody(payload, band, ladder.axis));
  const table = el("table", { class: "agents-table agents-rank-ladder" }, [el("thead", {}, [head]), ...groups]);
  const wrap = el("div", { class: "agents-table-wrap agents-rank-ladder-wrap", role: "region", "aria-label": "Model ranking" }, [table]);
  wrap.tabIndex = 0;
  const empty = ladder.bands.length ? null : el("p", { class: "agents-rank-note", text: "No model has passed the evidence gate yet." });
  const node = el("section", { class: "agents-rank-board", "aria-label": "Model ranking" }, [
    empty || wrap,
    legendNode(payload),
  ]);
  node.dataset.key = "board";
  node.dataset.hash = JSON.stringify([ladder.ranked, payload.qualified, payload.measures]);
  return node;
}

function bandBody(payload, band, axis) {
  const rows = band.rows.flatMap((row, index) => ladderRow(payload, row, axis, index === 0));
  return el("tbody", { class: "agents-rank-band" }, rows);
}

function ladderRow(payload, row, axis, isBandStart) {
  const toggle = breakdownToggle(row);
  const main = el("tr", { class: `agents-rank-row${isBandStart ? " is-band-start" : ""}` }, [
    el("td", { class: "agents-rank-chev-col" }, [toggle.button]),
    el("td", { class: "agents-rank-label", text: isBandStart ? rankText(payload, row) : "" }),
    el("td", { class: "agents-rank-model-cell" }, [modelNode(row)]),
    el("td", { class: "agents-num agents-rank-score", text: rankFormat.num(row.score) }),
    el("td", { class: "agents-rank-interval-col" }, [whiskerNode(row, axis)]),
    el("td", { class: "agents-num" }, [costNode(row)]),
    el("td", {}, [tierMixNode(row)]),
    el("td", { class: "agents-num agents-rank-landed", text: landedText(row), title: landedTitle(row) }),
  ]);
  main.dataset.model = row.model || "";
  main.style.setProperty("--provider", rankFormat.providerColor(row.model));
  const detailCell = el("td", {}, [breakdownNode(payload, row)]);
  detailCell.colSpan = RANK_COLUMNS;
  const detail = el("tr", { class: "agents-rank-detail" }, [detailCell]);
  toggle.bind(detail, main);
  return [main, detail];
}

function modelNode(row) {
  const crews = Array.isArray(row.crews) ? [...row.crews].sort((a, b) => (b.tasks || 0) - (a.tasks || 0)) : [];
  const crewNames = crews.map((crew) => crew.crew).filter(Boolean).join(", ");
  const crewTitle = crews.map((crew) => `${crew.crew || "—"} (${crew.tasks ?? 0})`).join(", ");
  const singleTier = typeof row.single_tier === "string" && row.single_tier ? row.single_tier : null;
  const nameLine = el("span", { class: "agents-rank-model-line" }, [
    el("span", { class: "agents-rank-model-name", text: row.model || "—", title: row.model || "" }),
    singleTier ? el("span", { class: "agents-rank-single-tier", text: `single tier: ${singleTier}`, title: `Ranked on the ${singleTier} tier only` }) : null,
  ]);
  const body = el("span", { class: "agents-rank-model-body" }, [
    nameLine,
    crewNames ? el("span", { class: "agents-rank-crew", text: crewNames, title: crewTitle }) : null,
  ]);
  return el("span", { class: "agents-rank-model" }, [
    rankFormat.providerDot(row.model),
    body,
  ]);
}

function landedText(row) {
  return typeof row.landed_rate === "number" ? `${rankFormat.num(row.landed_rate * 100, 1)}%` : "—";
}

function landedTitle(row) {
  return `${rankFormat.num(row.landed_tasks, 0)} landed of ${row.units ?? 0}`;
}

// The chevron button toggles the breakdown; clicking the row does the same.
function breakdownToggle(row) {
  const open = openModels.has(row.model);
  const button = el("button", {
    class: "agents-rank-toggle",
    type: "button",
    "aria-expanded": open ? "true" : "false",
    title: `${row.model || "Model"}: show or hide the breakdown`,
  }, [el("span", { class: "agents-rank-chevron", "aria-hidden": "true", text: "▸" })]);
  const bind = (detail, mainRow) => {
    detail.id = breakdownId(row.model);
    detail.hidden = !open;
    button.setAttribute("aria-controls", detail.id);
    const setOpen = (next) => {
      if (next) openModels.add(row.model);
      else openModels.delete(row.model);
      detail.hidden = !next;
      button.setAttribute("aria-expanded", next ? "true" : "false");
    };
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      setOpen(!openModels.has(row.model));
    });
    if (mainRow) {
      mainRow.addEventListener("click", () => setOpen(!openModels.has(row.model)));
    }
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
  for (const tick of axis.ticks) {
    node.append(el("span", { class: tick === 100 ? "agents-rank-grid is-100" : "agents-rank-grid", style: { left: `${axis.pos(tick)}%` } }));
  }
  if (hasInterval) {
    const lo = axis.pos(interval.p10);
    const hi = axis.pos(interval.p90);
    node.append(el("span", { class: "agents-rank-whisker-line", style: { left: `${lo}%`, width: `${Math.max(0, hi - lo)}%` } }));
  }
  if (typeof row.score === "number") node.append(el("span", { class: "agents-rank-whisker-tick", style: { left: `${axis.pos(row.score)}%` } }));
  return node;
}

function costNode(row) {
  const cost = row.cost || {};
  const coverage = typeof cost.coverage === "number" ? `${rankFormat.num(cost.coverage * 100, 0)}% of tasks priced` : "coverage unknown";
  if (cost.cost_unknown || typeof cost.per_task !== "number") {
    return el("span", { class: "agents-rank-cost is-unknown", text: "cost unknown", title: coverage });
  }
  const cheaper = typeof cost.ratio === "number" && cost.ratio < 1;
  const dearer = typeof cost.ratio === "number" && cost.ratio >= 1;
  const ratio = typeof cost.ratio === "number"
    ? el("span", { class: `agents-rank-ratio${cheaper ? " is-cheaper" : dearer ? " is-dearer" : ""}`, text: `${rankFormat.num(cost.ratio, 2)}×` })
    : null;
  return el("span", {
    class: "agents-rank-cost",
    title: `${rankFormat.num(cost.ratio, 2)}× the fleet's cost per task on the same tier mix. ${coverage}.`,
  }, [el("span", { class: "agents-rank-per-task", text: rankFormat.dollars(cost.per_task) }), ratio]);
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
  const title = entries.map(([tier, count]) => `${count} ${tier}`).join(", ") || "no tasks by tier";
  const bar = el("span", { class: "agents-rank-mix", "aria-hidden": "true" },
    entries.map(([tier, count]) => el("span", { class: `agents-rank-mix-${tier}`, style: { width: `${total ? (count * 100) / total : 0}%` } })));
  return el("span", { class: "agents-rank-mixcell", title }, [
    el("span", { class: "agents-rank-units", text: String(row.units ?? 0) }),
    bar,
  ]);
}

function legendNode(payload) {
  const swatch = (tier) => el("span", { class: "agents-rank-legend-item" }, [
    el("span", { class: `agents-rank-legend-swatch agents-rank-mix-${tier}`, "aria-hidden": "true" }),
    el("span", { text: tier }),
  ]);
  const rules = payload.rules || {};
  const confidence = typeof rules.band_probability === "number" ? `${rankFormat.num(rules.band_probability * 100, 0)}%` : "80%";
  return el("div", { class: "agents-rank-legend" }, [
    el("span", { text: `Shared ranks (2=) are one band at ${confidence} confidence. Cost orders a band and never crosses it.` }),
    el("span", { class: "agents-rank-legend-tiers" }, ["low", "medium", "hard", "xhard", "unassessed"].map(swatch)),
  ]);
}

// --- not ranked and not scored ---

function gateProgress(rules, row) {
  const minTasks = typeof rules.min_tasks === "number" ? rules.min_tasks : 0;
  const minTiers = typeof rules.min_tiers === "number" ? rules.min_tiers : 0;
  const minTierTasks = typeof rules.min_tier_tasks === "number" ? rules.min_tier_tasks : 0;
  const units = row.units || 0;
  const tiers = tierEntries(row).filter(([tier, count]) => tier !== "unassessed" && count >= minTierTasks).length;
  const fractions = [];
  let reason = "";
  if (minTasks) {
    fractions.push(Math.min(1, units / minTasks));
    if (units < minTasks) reason = `${units} of ${minTasks} tasks`;
  }
  if (minTiers) {
    fractions.push(Math.min(1, tiers / minTiers));
    if (!reason && tiers < minTiers) reason = `${tiers} of ${minTiers} tiers with ${minTierTasks}+ tasks`;
  }
  const progress = fractions.length ? Math.min(...fractions) : 0;
  return { progress, text: reason || row.ineligible_reason || "not eligible" };
}

function pendingNode(payload, pending) {
  const rules = payload.rules || {};
  const gate = [];
  if (rules.min_tasks) gate.push(`${rules.min_tasks} tasks`);
  if (rules.min_tiers) gate.push(rules.min_tiers === 1 ? `1 tier with ${rules.min_tier_tasks || 0}+ tasks` : `${rules.min_tiers} tiers with ${rules.min_tier_tasks || 0}+ each`);
  const items = pending
    .map((row) => ({ row, gate: gateProgress(rules, row) }))
    .sort((a, b) => b.gate.progress - a.gate.progress || (b.row.units || 0) - (a.row.units || 0));
  const list = el("ul", { class: "agents-rank-pending-list" }, items.map(({ row, gate: progress }) => {
    const raw = typeof row.raw_score === "number" ? row.raw_score : row.score;
    return el("li", { class: "agents-rank-pending-row" }, [
      el("span", { class: "agents-rank-model" }, [
        rankFormat.providerDot(row.model),
        el("span", { class: "agents-rank-model-name", text: row.model || "—", title: row.model || "" }),
      ]),
      el("span", { class: "agents-num agents-rank-pending-score", title: "Unshrunk score; not comparable to ranked rows", text: rankFormat.num(raw) }),
      el("span", { class: "agents-num agents-rank-pending-units", text: `${row.units ?? 0} tasks` }),
      el("span", { class: "agents-rank-pending-gatecell" }, [
        el("span", { class: "agents-rank-progress", role: "img", "aria-label": `${rankFormat.num(progress.progress * 100, 0)}% of the evidence gate` }, [
          el("span", { style: { width: `${progress.progress * 100}%` } }),
        ]),
        el("span", { class: "agents-rank-pending-gate", text: progress.text, title: row.ineligible_reason || "" }),
      ]),
    ]);
  }));
  const summary = el("summary", { class: "agents-rank-notranked-summary" }, [
    el("span", { class: "agents-rank-notranked-title", text: "Not ranked" }),
    el("span", { class: "agents-rank-box-count", text: rankFormat.plural(pending.length, "model") }),
    gate.length ? el("span", { class: "agents-rank-box-meta", text: `needs ${gate.join(" across ")}` }) : null,
  ]);
  const node = el("details", { class: "agents-rank-box agents-rank-notranked", "aria-label": "Not ranked" }, [
    summary,
    list,
    el("p", { class: "agents-rank-note", text: "Scores here are unshrunk previews and carry no band." }),
  ]);
  node.open = pendingOpen;
  node.addEventListener("toggle", () => { pendingOpen = node.open; });
  node.dataset.key = "notranked";
  node.dataset.hash = JSON.stringify(pending);
  return node;
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
  const node = el("section", { class: "agents-rank-box agents-rank-unscored", "aria-label": "Not scored" }, [
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
  node.dataset.key = "unscored";
  node.dataset.hash = JSON.stringify([payload.not_scored, payload.unattributed]);
  return node;
}

// --- the breakdown under a row ---

function breakdownNode(payload, row) {
  return el("div", { class: "agents-rank-breakdown" }, [
    contributionsNode(payload, row),
    tierBreakdownNode(row),
    deliveryNode(row),
  ]);
}

function contributionsNode(payload, row) {
  const measures = Array.isArray(payload.measures) ? payload.measures : [];
  const cells = row.measures || {};
  const list = measures.map((measure) => {
    const cell = cells[measure.key] || {};
    const measured = cell.status === "measured" && typeof cell.contribution === "number";
    const value = measured ? cell.contribution : null;
    const width = measured ? Math.min(Math.abs(value), CONTRIB_SCALE) / CONTRIB_SCALE * 50 : 0;
    const bar = el("span", { class: "agents-rank-contrib-track", "aria-hidden": "true" }, [
      el("span", { class: "agents-rank-contrib-zero" }),
      measured ? el("span", {
        class: `agents-rank-contrib-fill ${value >= 0 ? "is-up" : "is-down"}`,
        style: value >= 0 ? { left: "50%", width: `${width}%` } : { right: "50%", width: `${width}%` },
      }) : null,
    ]);
    return el("div", { class: "agents-rank-contrib-row" }, [
      el("span", { class: measured ? "agents-rank-contrib-label" : "agents-rank-contrib-label is-unmeasured", text: measure.label || measure.key }),
      el("span", { class: "agents-rank-contrib-weight", text: rankFormat.num(measure.weight, 0) }),
      bar,
      el("span", {
        class: `agents-rank-contrib-value ${measured ? (value >= 0 ? "is-up" : "is-down") : "is-unmeasured"}`,
        text: measured ? `${value > 0 ? "+" : ""}${rankFormat.num(value)}` : "not yet",
        title: measured ? `${measure.label || measure.key}: actual ${rankFormat.num(cell.actual)}, expected ${rankFormat.num(cell.expected)}` : (cell.reason || measure.reason || "not measured yet"),
      }),
    ]);
  });
  const rules = payload.rules || {};
  const shrink = typeof row.raw_score === "number" && typeof row.score === "number"
    ? el("p", { class: "agents-rank-note", text: `Raw ${rankFormat.num(row.raw_score)} → shrunk ${rankFormat.num(row.score)} toward 100 (k = ${rankFormat.num(rules.shrinkage_k, 0)}).` })
    : null;
  return el("div", { class: "agents-rank-breakdown-col" }, [
    el("div", { class: "agents-rank-breakdown-head" }, [
      el("span", { class: "agents-rank-breakdown-title", text: "Score contributions" }),
      el("span", { class: "agents-rank-breakdown-sub", text: "vs. tier expectation" }),
    ]),
    ...list,
    shrink,
  ]);
}

function tierBreakdownNode(row) {
  const tiers = [...(Array.isArray(row.tiers) ? row.tiers : [])]
    .sort((a, b) => rankFormat.tierIndex(a.tier) - rankFormat.tierIndex(b.tier));
  const head = el("div", { class: "agents-rank-tier-grid agents-rank-tier-head" }, [
    el("span", { text: "Tier" }),
    el("span", { class: "agents-num", text: "Tasks" }),
    el("span", { class: "agents-num", text: "Score" }),
    el("span", { class: "agents-num", text: "$ / task" }),
  ]);
  const rows = tiers.map((point) => {
    const thin = (point.tasks || 0) < 5;
    return el("div", { class: `agents-rank-tier-grid${thin ? " is-thin" : ""}` }, [
      el("span", { class: "agents-rank-tier-name" }, [
        el("span", { class: `agents-rank-legend-swatch agents-rank-mix-${point.tier}`, "aria-hidden": "true" }),
        el("span", { text: rankFormat.tierLabel(point.tier) }),
      ]),
      el("span", { class: "agents-num", text: String(point.tasks ?? 0) }),
      el("span", { class: "agents-num", text: rankFormat.num(point.score, 1) }),
      el("span", { class: "agents-num agents-muted", text: rankFormat.dollars(point.cost_per_task) }),
    ]);
  });
  return el("div", { class: "agents-rank-breakdown-col" }, [
    el("span", { class: "agents-rank-breakdown-title", text: "By tier" }),
    head,
    ...rows,
    tiers.length ? el("p", { class: "agents-rank-note", text: "Tiers under 5 tasks are greyed: shown, not trusted." }) : el("p", { class: "agents-rank-note", text: "No tasks by tier." }),
  ]);
}

function deliveryNode(row) {
  const cost = row.cost || {};
  const crews = Array.isArray(row.crews) ? [...row.crews].sort((a, b) => (b.tasks || 0) - (a.tasks || 0)) : [];
  const crewText = crews.length ? crews.map((crew) => `${crew.crew || "—"} ${crew.tasks ?? 0}`).join(" · ") : "—";
  const fact = (label, value) => el("div", { class: "agents-rank-delivery-row" }, [
    el("span", { text: label }),
    el("span", { class: "agents-rank-delivery-value", text: value }),
  ]);
  return el("div", { class: "agents-rank-breakdown-col" }, [
    el("span", { class: "agents-rank-breakdown-title", text: "Delivery" }),
    el("div", { class: "agents-rank-delivery" }, [
      fact("Landed", `${rankFormat.num(row.landed_tasks, 0)} / ${row.units ?? 0} · ${landedText(row)}`),
      fact("Crews", crewText),
      fact("Expected $ / task", rankFormat.dollars(cost.expected_per_task)),
      fact("Cost coverage", typeof cost.coverage === "number" ? `${rankFormat.num(cost.coverage * 100, 0)}%` : "—"),
    ]),
  ]);
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
    el("p", { text: `The score starts at 100. Each measured residual, already signed so that positive is better, is divided by the noise scale and weighted; a measure's contribution is the points it adds. The result shrinks toward the other eligible models (k = ${rankFormat.num(rules.shrinkage_k, 0)}), and ${draws} the 10th–90th percentile interval. Models share a band while the probability that one is ahead of the next stays below ${band}. A model with fewer than ${typeof rules.review_min_support === "number" ? rankFormat.num(rules.review_min_support, 0) : "the minimum"} reviewed deliveries has review outcome unmeasured, so its weight leaves that model's denominator. An unqualified board shows scores without bands. Cost orders a band and never changes it. A chart point is 100 + 10 × z on that tier's tasks only, before this shrinkage.` }),
    list,
  ]);
  node.dataset.key = "how";
  node.dataset.hash = JSON.stringify([measures.map((measure) => [measure.key, measure.status, measure.weight]), rules]);
  return node;
}
