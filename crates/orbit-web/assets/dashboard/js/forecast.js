// Orbit dashboard Analytics › Forecast: how long the dispatchable backlog
// takes to drain and what it costs, as p50 and p90 resampled from this
// workspace's delivery history, beside the recent arrival rate the forecast
// assumes away, and how earlier stored forecasts scored against what their
// tasks actually did. Read from /api/diagnostics/backlog_forecast. The Tasks
// drain card reuses this default read (30 days, no arrivals) as a compact summary.
// Pure vanilla JS, split into ES modules with no build step.
//
// The forecast is per workspace; the aggregate view shows the usual
// placeholder. The lookback and arrivals toggles refetch: only the default
// view (30 days, no arrivals, the live drain ceiling) is the one stored and
// scored.

import {
  el as makeEl, syncNodes, fetchJson, requestPanel, panelCanRender, getWorkspaceRevision,
  onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime, infoHint,
  scopedHashHref, bindTaskLink,
} from './common.js';

const $ = (id) => document.getElementById(id);

// `el` from common.js, also applying the table and focus attributes it leaves out.
function el(tag, opts = {}, children = []) {
  const node = makeEl(tag, opts, children);
  for (const name of ["scope", "tabindex", "colspan"]) {
    if (opts[name] != null) node.setAttribute(name, String(opts[name]));
  }
  return node;
}

const BODY = "forecast-body";
const COUNT = "forecast-count";
const ENDPOINT = "/api/diagnostics/backlog_forecast";
const LOOKBACKS = [[14, "14d"], [30, "30d"], [90, "90d"]];
const ARRIVALS = [["off", "No arrivals"], ["on", "Recent arrivals"]];
const COMPLEXITY_LABELS = {
  low: "Low", medium: "Medium", hard: "Hard", xhard: "Xhard", unassessed: "Unassessed",
};
// An older forecast omits review-mode coverage. Its sampling table keeps these
// labels. A response that reports the new fields uses REVIEW_BASIS_LABELS, so
// a mode-preserving widening never reads the same as dropping the review mode.
const LEGACY_BASIS_LABELS = { cell: "cell", complexity: "complexity", workspace: "workspace", none: "no history" };
const REVIEW_BASIS_LABELS = {
  cell: "same complexity, crew and review mode",
  review_complexity: "review mode kept, crew dropped",
  review_workspace: "review mode kept, complexity dropped",
  mode_pooled_cell: "review mode dropped, same complexity and crew",
  complexity: "review mode dropped, same complexity",
  workspace: "review mode dropped, all deliveries",
  none: "no history",
};
const REVIEW_MODE_LABELS = {
  none: "No review", before_pr: "Before PR", before_landing: "Before landing",
  mixed: "Mixed review", unknown: "Unknown assumption",
};
const REVIEW_COVERAGE_KEYS = ["review_mode_fallbacks", "review_mode_unknown", "history_review_mode_unknown"];
// Why a task has no simulated start: its dependency is work the drain will
// not start on its own, or sits behind such work or a cycle.
const UNFORECASTABLE_LABELS = {
  review: "waits on work in review", blocked: "waits on blocked work", someday: "waits on someday work",
  proposed: "waits on unapproved work", rejected: "waits on rejected work", archived: "waits on archived work",
  external_work: "waits on another workspace", missing: "waits on a missing task", cycle: "dependency cycle",
  upstream: "behind unforecastable work", return_unsupported: "blocked, with no return history",
};
const WAITS = [
  ["dependency", "Dependencies", "Waiting for an earlier task in this forecast to finish."],
  ["lock", "File locks", "Its files overlap a running task's locked files."],
  ["reservation", "Surface reservations", "Higher-priority lock-blocked work reserved an overlapping surface."],
  ["footprint", "Unknown footprint", "An editing task without context files runs alone, or waits for the tree to be free."],
  ["capacity", "Capacity", "Ready and unblocked, but every slot is taken."],
  ["return", "Returns", "Blocked, or blocked by an attempt, and waiting for a sampled return to the queue."],
];
// The schedule table lists this many tasks in dispatch order.
const SCHEDULE_ROWS = 50;

const view = { lookback: 30, arrivals: "off" };
let lastPayload = null;
let requestScope = null;

function clearForecast() {
  lastPayload = null;
  const count = $(COUNT);
  if (count) count.textContent = "—";
}

// Default lookback and arrivals off omit the query, matching the stored forecast.
export function backlogForecastPath({ lookback = 30, arrivals = "off" } = {}) {
  const params = new URLSearchParams();
  if (lookback !== 30) params.set("lookback_days", String(lookback));
  if (arrivals === "on") params.set("arrivals", "on");
  const query = params.toString();
  return query ? `${ENDPOINT}?${query}` : ENDPOINT;
}

function endpoint() {
  return backlogForecastPath({ lookback: view.lookback, arrivals: view.arrivals });
}

export function fetchBacklogForecast(options) {
  return fetchJson(backlogForecastPath(options));
}

export function placeholdForecastAggregate() {
  clearForecast();
  renderPanelPlaceholder(BODY);
}

export function fetchAndRenderForecast() {
  if (isAggregateView()) {
    placeholdForecastAggregate();
    return Promise.resolve();
  }
  const revision = getWorkspaceRevision();
  if (requestScope !== `${revision}`) {
    requestScope = `${revision}`;
    clearForecast();
  }
  const path = endpoint();
  return requestPanel(BODY, path, () => fetchJson(path), (payload) => {
    if (getWorkspaceRevision() !== revision) return;
    lastPayload = payload;
    renderForecast();
  }, COUNT);
}

onWorkspaceChange(() => {
  requestScope = null;
  clearForecast();
});

export function renderForecast() {
  const body = $(BODY);
  if (!body) return;
  if (isAggregateView()) {
    placeholdForecastAggregate();
    return;
  }
  if (!lastPayload || !panelCanRender(BODY)) return;
  const forecast = lastPayload.forecast || {};
  const count = $(COUNT);
  if (count) count.textContent = `${forecast.tasks ?? 0} task${forecast.tasks === 1 ? "" : "s"} · ${lastPayload.inputs?.concurrency ?? "?"} slots`;
  const nodes = [forecastCard(), waitsCard(), scheduleCard(), byComplexityCard(), coverageCard(), calibrationCard()];
  const root = el("div", { class: "fc-view" }, nodes);
  root.dataset.key = "forecast-view";
  root.dataset.hash = JSON.stringify([view, lastPayload]);
  syncNodes(body, [root]);
}

function refetch() {
  fetchAndRenderForecast().catch((error) => console.error("Failed to fetch backlog forecast", error));
}

// ---- formatting -----------------------------------------------------------

function num(value) {
  const n = Number(value);
  return value == null || !Number.isFinite(n) ? null : n;
}

export function formatForecastHours(value) {
  const n = num(value);
  if (n == null) return "—";
  if (n < 1) return `${Math.round(n * 60)}m`;
  if (n < 48) return `${n >= 10 ? Math.round(n) : n.toFixed(1)}h`;
  const days = n / 24;
  return `${days >= 10 ? Math.round(days) : days.toFixed(1)}d`;
}

export function formatForecastTokens(value) {
  const n = num(value);
  if (n == null) return "—";
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(n >= 1e4 ? 0 : 1)}k`;
  return `${Math.round(n)}`;
}

export function formatForecastUsd(value) {
  const n = num(value);
  if (n == null) return "—";
  return n >= 100 ? `$${Math.round(n).toLocaleString()}` : `$${n.toFixed(2)}`;
}

function fmtRate(value) {
  const n = num(value);
  return n == null ? "—" : `${n >= 10 ? Math.round(n) : n.toFixed(1)}/day`;
}

function fmtParallelism(value) {
  const n = num(value);
  return n == null ? "—" : `${n.toFixed(1)}×`;
}

function fmtRank(value) {
  const n = num(value);
  return n == null ? "—" : `#${Math.round(n)}`;
}

function fmtShare(value) {
  const n = num(value);
  return n == null ? "—" : `${Math.round(n * 100)}%`;
}

/// "p50 – p90" for one quantile pair; never a bare point estimate.
export function formatForecastQuantileRange(quantiles, fmt) {
  const p50 = quantiles?.p50;
  const p90 = quantiles?.p90;
  if (p50 == null && p90 == null) return "—";
  return `${fmt(p50)} – ${fmt(p90)}`;
}

function quantilePair(quantiles) {
  return num(quantiles?.p50) != null || num(quantiles?.p90) != null;
}

function forecastCount(value) {
  const n = num(value);
  return n == null || n < 0 ? 0 : Math.trunc(n);
}

/// Compact drain-card figures. Null when time to drain has no quantile.
/// Tokens and cost are null when that pair has none, so a missing cost is
/// left out rather than shown as a dash.
export function backlogForecastSummary(payload) {
  if (!payload || typeof payload !== "object") return null;
  const forecast = payload.forecast || {};
  const hours = forecast.time_to_drain_hours;
  if (!quantilePair(hours)) return null;
  const backlog = payload.backlog || {};
  const costed = quantilePair(forecast.usd);
  return {
    drain: formatForecastQuantileRange(hours, formatForecastHours),
    cost: costed ? formatForecastQuantileRange(forecast.usd, wholeUsd) : null,
    exactCost: costed ? formatForecastQuantileRange(forecast.usd, formatForecastUsd) : null,
    tokens: quantilePair(forecast.tokens) ? formatForecastQuantileRange(forecast.tokens, formatForecastTokens) : null,
    counts: `${forecastCount(backlog.backlog)} backlog · ${forecastCount(backlog.in_progress)} in progress`,
  };
}

// Whole dollars, so the dock line fits; the tooltip keeps the cents.
function wholeUsd(value) {
  const n = num(value);
  return n == null ? "—" : `$${Math.round(n).toLocaleString()}`;
}

/// One line linking to Analytics › Forecast: time to drain and cost as
/// p50 – p90. The counts, tokens and exact cost are in its tooltip and
/// accessible text, so the crowded dock spends one row on it. A null summary is the muted
/// unavailable line.
export function drainBacklogForecastLink(summary) {
  const unavailable = !summary;
  const link = el("a", { class: unavailable ? "drain-backlog-forecast unavailable" : "drain-backlog-forecast" });
  link.href = scopedHashHref("#analytics/forecast");
  if (unavailable) {
    link.title = "Analytics › Forecast";
    link.textContent = "Backlog forecast unavailable";
    return link;
  }
  const figures = [summary.drain, summary.cost].filter(Boolean).join(" · ");
  const detail = [
    summary.counts, summary.tokens && `Tokens ${summary.tokens}`, summary.exactCost && `Cost ${summary.exactCost}`,
  ].filter(Boolean).join(" · ");
  link.title = `p50 – p90 · ${detail}\nAnalytics › Forecast`;
  link.append(
    el("span", { class: "drain-section-label", text: "Backlog" }),
    el("span", { class: "drain-backlog-forecast-figures", text: figures }),
    el("span", { class: "visually-hidden", text: ` p50 – p90, ${detail}` }),
    el("span", { class: "drain-backlog-forecast-go", text: "›", "aria-hidden": "true" }),
  );
  return link;
}

// ---- forecast -------------------------------------------------------------

function seg(label, options, current, onPick) {
  return el("div", { class: "mx-seg", role: "group", "aria-label": label }, options.map(([key, text]) => {
    const on = key === current;
    const button = el("button", { class: on ? "mx-seg-btn on" : "mx-seg-btn", type: "button", text, "aria-pressed": on ? "true" : "false" });
    button.addEventListener("click", () => {
      if (on) return;
      onPick(key);
      refetch();
    });
    return button;
  }));
}

function kpi(label, quantiles, fmt, foot) {
  const value = (q) => (quantiles?.[q] == null ? "—" : fmt(quantiles[q]));
  return el("div", { class: "mx-sc fc-kpi", role: "group", "aria-label": label }, [
    el("span", { class: "mx-sc-label", text: label }),
    el("span", { class: "fc-kpi-pair" }, [
      el("span", { class: "fc-q" }, [el("span", { class: "mx-sc-tag", text: "p50" }), el("span", { class: "mx-sc-value", text: value("p50") })]),
      el("span", { class: "fc-q" }, [el("span", { class: "mx-sc-tag", text: "p90" }), el("span", { class: "mx-sc-value fc-p90", text: value("p90") })]),
    ]),
    foot ? el("span", { class: "mx-sc-foot", text: foot }) : null,
  ]);
}

// Unforecastable dependencies withhold every full total, whatever the history.
const SUBSET_FOOT = "not every task can be scheduled";

function metricFoot(quantiles, metric) {
  if (quantiles?.p90 != null) return metric === "tokens" ? "normalized" : null;
  if (lastPayload.forecast?.schedulable_subset) return SUBSET_FOOT;
  const missing = lastPayload.arrivals?.coverage?.[`missing_${metric}`];
  if (lastPayload.inputs?.arrivals && missing) return "incomplete history or arrival telemetry";
  return metric === "tokens" ? "not enough priced history" : "not enough costed history";
}

function drainFoot(quantiles) {
  if (quantiles?.p90 != null) return null;
  if (lastPayload.forecast?.schedulable_subset) return SUBSET_FOOT;
  if (quantiles?.p50 != null) return "p90 beyond the 90-day horizon";
  return lastPayload.coverage?.no_history ? "some tasks have no history" : "beyond the 90-day horizon";
}

function capacityText(inputs) {
  const source = {
    requested: "requested",
    run_control: "the running drain's adjusted ceiling",
    run_input: "the running drain's ceiling",
    pull_run_control: "the pull drain's adjusted ceiling",
    pull_run_input: "the pull drain's ceiling",
    default: "the default ceiling; no drain is running",
  }[inputs.concurrency_source] || inputs.concurrency_source;
  return `${inputs.concurrency} leaf run${inputs.concurrency === 1 ? "" : "s"} at once (${source})`;
}

function arrivalsLine() {
  const arrivals = lastPayload.arrivals || {};
  const perDay = arrivals.per_day || {};
  const parts = [["auto-tasks", perDay.auto_task], ["sweeps", perDay.sweep], ["filings", perDay.filing]]
    .map(([label, rate]) => `${label} ${fmtRate(rate)}`).join(" · ");
  const assumed = lastPayload.inputs?.arrivals
    ? "Includes new tasks arriving at the recent rate."
    : "Assumes no new tasks arrive.";
  return el("div", { class: "fc-assume" }, [
    el("span", { class: "fc-assume-head", text: assumed }),
    el("span", { class: "fc-assume-rate" }, [
      `Last ${arrivals.window_days ?? 7} days: ${fmtRate(perDay.total)} filed (${parts}), ${fmtRate(arrivals.done_per_day)} done.`,
    ]),
  ]);
}

function forecastCard() {
  const inputs = lastPayload.inputs || {};
  const forecast = lastPayload.forecast || {};
  const backlog = lastPayload.backlog || {};
  const tools = el("div", { class: "mx-trend-tools" }, [
    seg("History lookback", LOOKBACKS, view.lookback, (key) => { view.lookback = key; }),
    seg("Arrivals", ARRIVALS, view.arrivals, (key) => { view.arrivals = key; }),
  ]);
  const meta = `${backlog.backlog ?? 0} backlog · ${backlog.in_progress ?? 0} in progress · ${backlog.blocked ?? 0} blocked · ${inputs.history_deliveries ?? 0} deliveries in ${inputs.lookback_days ?? "?"} days`;
  const children = [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title", text: "Time to drain and cost" }),
      tools,
    ]),
    el("p", { class: "mx-blurb" }, [
      `${meta}. Simulated ${inputs.iterations ?? 0} times on ${capacityText(inputs)}. `,
      infoHint(`${samplingExplanation(inputs)} Each simulated drain admits work as the drain does: in dispatch order, after its dependencies finish, never beside a task whose files overlap, an editing task without context files alone, and behind surfaces reserved by higher-priority work. Each attempt first draws a recorded attempt of the same pool: one that blocked, failed or was abandoned frees its slot when that attempt did, then requeues, waits for a return drawn from how long blocked work took to come back (or never does), or stops. Blocked tasks start out waiting for that return. Review and someday tasks are left out, and so is work that depends on them; in-progress tasks count only their remaining work. Review locks, static eligibility and arrivals' files are not modelled yet.`),
      lastPayload.model_version != null ? el("span", { class: "fc-model mx-muted", text: ` Model v${lastPayload.model_version}.` }) : null,
    ]),
  ];
  if (!forecast.tasks) {
    children.push(el("div", { class: "mx-empty" }, [
      el("div", { class: "mx-empty-title", text: "Nothing to forecast" }),
      el("div", { class: "mx-empty-detail", text: "No backlog or in-progress tasks in this workspace." }),
    ]));
  } else if (!inputs.history_deliveries) {
    children.push(el("div", { class: "mx-empty" }, [
      el("div", { class: "mx-empty-title", text: "No delivery history to sample" }),
      el("div", { class: "mx-empty-detail", text: `No task finished with recorded agent runs in the last ${inputs.lookback_days} days. Widen the lookback.` }),
    ]));
  } else {
    children.push(el("div", { class: "mx-series fc-kpis" }, [
      kpi("Time to drain", forecast.time_to_drain_hours, formatForecastHours, drainFoot(forecast.time_to_drain_hours)),
      kpi("Cost", forecast.usd, formatForecastUsd, metricFoot(forecast.usd, "usd")),
      kpi("Tokens", forecast.tokens, formatForecastTokens, metricFoot(forecast.tokens, "tokens")),
      kpi("Effective parallelism", forecast.effective_parallelism, fmtParallelism, `of ${inputs.concurrency ?? "?"} slots, averaged over the drain`),
      kpi("Critical path", forecast.critical_path_hours, formatForecastHours, "longest dependency chain"),
    ]));
    const subset = subsetNote(forecast);
    if (subset) children.push(subset);
    children.push(el("p", { class: "fc-approved fc-odds" }, [drainOdds(forecast)]));
  }
  children.push(arrivalsLine());
  const approved = lastPayload.if_approved;
  if (approved && backlog.proposed) {
    children.push(el("p", { class: "fc-approved" }, [
      `If the ${backlog.proposed} proposed task${backlog.proposed === 1 ? " is" : "s are"} approved: drain ${formatForecastQuantileRange(approved.time_to_drain_hours, formatForecastHours)}, cost ${formatForecastQuantileRange(approved.usd, formatForecastUsd)}, tokens ${formatForecastQuantileRange(approved.tokens, formatForecastTokens)} (p50 – p90).`,
    ]));
  }
  return el("section", { class: "mx-card", "aria-label": "Backlog forecast" }, children);
}

// Unforecastable dependencies withhold the full totals; the schedulable
// subset is what the simulation could still drain.
function subsetNote(forecast) {
  const subset = forecast.schedulable_subset;
  if (!subset) return null;
  const left = (subset.of ?? 0) - (subset.tasks ?? 0);
  return el("div", { class: "fc-assume fc-subset", role: "note" }, [
    el("span", { class: "fc-assume-head", text: `No full total: ${left} task${left === 1 ? "" : "s"} depend${left === 1 ? "s" : ""} on work the drain cannot start.` }),
    el("span", { class: "fc-assume-rate", text: `The other ${subset.tasks} of ${subset.of}: drain ${formatForecastQuantileRange(subset.time_to_drain_hours, formatForecastHours)}, cost ${formatForecastQuantileRange(subset.usd, formatForecastUsd)}, tokens ${formatForecastQuantileRange(subset.tokens, formatForecastTokens)} (p50 – p90).` }),
  ]);
}

function drainOdds(forecast) {
  const horizon = formatForecastHours(forecast.horizon_hours ?? 2160);
  const parts = [`Drains within ${horizon} in ${fmtShare(forecast.drain_probability)} of runs`, `the initial tasks all finish in ${fmtShare(forecast.completion_probability)}`];
  const unfinished = num(forecast.not_expected_to_drain);
  if (unfinished) parts.push(`${unfinished} task${unfinished === 1 ? "" : "s"} unfinished on average`);
  const text = `${parts.join("; ")}.`;
  return forecast.simulation_truncated ? `${text} Some runs hit the event limit.` : text;
}

function card(title, meta, children, className = "") {
  return el("section", { class: `mx-card ${className}`.trim(), "aria-label": title }, [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title", text: title }),
      meta ? el("span", { class: "mx-card-meta", text: meta }) : null,
    ]),
    ...children,
  ]);
}

function table(label, head, rows, caption) {
  return el("div", { class: "mx-table-wrap", tabindex: "0", role: "region", "aria-label": label }, [
    el("table", { class: "mx-table fc-table" }, [
      caption ? el("caption", { text: caption }) : null,
      el("thead", {}, [el("tr", {}, head.map(([text, numeric]) => el("th", { scope: "col", class: numeric ? "num" : null, text })))]),
      el("tbody", {}, rows),
    ]),
  ]);
}

function waitsCard() {
  const waits = lastPayload.forecast?.wait_hours;
  if (!waits || !lastPayload.forecast?.tasks) return null;
  const rows = WAITS.map(([key, label, hint]) => el("tr", {}, [
    el("th", { scope: "row" }, [label, " ", infoHint(hint)]),
    el("td", { class: "num", text: formatForecastQuantileRange(waits[key], formatForecastHours) }),
  ]));
  return card("Why work waits", "task-hours, p50 – p90", [
    table("Waiting by reason", [["Held back by"], ["Waiting", true]], rows, "Summed over every waiting task, so hours add up across tasks and exceed the time to drain. A task counts under the first gate it fails."),
  ]);
}

function taskCell(row) {
  const link = el("a", { class: "fc-task", text: row.title || "Untitled task" });
  bindTaskLink(link, row.id);
  return el("th", { scope: "row" }, [link]);
}

function scheduleCard() {
  const predictions = lastPayload.forecast?.task_predictions || [];
  if (!predictions.length) return null;
  const shown = predictions.slice(0, SCHEDULE_ROWS);
  const review = scheduleShowsReview(shown);
  const rows = shown.map((row) => el("tr", {}, [
    taskCell(row),
    el("td", { class: "fc-state", text: row.status }),
    review ? el("td", { class: "fc-review", text: reviewAssumptionText(row) }) : null,
    row.unforecastable
      ? el("td", { class: "fc-unforecastable", colspan: "3", text: UNFORECASTABLE_LABELS[row.unforecastable] || row.unforecastable })
      : el("td", { class: "num", text: row.status === "in-progress" ? "running" : formatForecastQuantileRange(row.predicted_start_hours, formatForecastHours) }),
    row.unforecastable ? null : el("td", { class: "num", text: row.status === "in-progress" ? "—" : formatForecastQuantileRange(row.predicted_start_rank, fmtRank) }),
    row.unforecastable ? null : el("td", { class: "num", text: fmtShare(row.completion_probability) }),
  ]));
  const meta = predictions.length > shown.length ? `first ${shown.length} of ${predictions.length}` : `${predictions.length} task${predictions.length === 1 ? "" : "s"}`;
  const head = [["Task"], ["Status"], ["Starts in", true], ["Start order", true], ["Finishes", true]];
  if (review) head.splice(2, 0, ["Review"]);
  const caption = review
    ? "In-progress tasks, then the backlog in dispatch order. Start time and order are p50 – p90 across simulated drains; finishes is the share of runs in which the task finishes within the horizon. Review is the assumed review mode: a thin pool either keeps that mode or drops it."
    : "In-progress tasks, then the backlog in dispatch order. Start time and order are p50 – p90 across simulated drains; finishes is the share of runs in which the task finishes within the horizon.";
  return card("Predicted schedule", meta, [
    table("Predicted schedule", head, rows, caption),
  ]);
}

// Review-mode coverage is present on current forecasts and absent on older
// ones. Per-task `review_mode` is the same contract, so either is enough.
function reportsReviewMode(payload = lastPayload) {
  const coverage = payload?.coverage;
  if (coverage && REVIEW_COVERAGE_KEYS.some((key) => Object.prototype.hasOwnProperty.call(coverage, key))) {
    return true;
  }
  return (payload?.forecast?.task_predictions || []).some((row) => row && (row.review_mode != null || row.review_mode_fallback != null));
}

function basisLabel(key) {
  const labels = reportsReviewMode() ? REVIEW_BASIS_LABELS : LEGACY_BASIS_LABELS;
  return labels[key] || key;
}

// Shipped order: keep the review mode while widening, and drop it only after
// the mode-preserving pools are thin. Complexity and workspace after that do
// not wait for another full cell.
function samplingExplanation(inputs) {
  if (!reportsReviewMode()) {
    return "Each task draws a completed delivery's wall time, tokens and cost from tasks of the same complexity and crew pool, falling back to its complexity, then to all deliveries, when there are too few.";
  }
  const min = inputs?.min_cell_samples ?? 5;
  return `Each task draws a completed delivery's wall time, tokens and cost from deliveries of the same complexity, crew pool and review mode. Below ${min} deliveries it keeps the review mode, first widening to that complexity at any crew, then to every delivery of that mode. Only after that does it drop the review mode: the same complexity and crew pool once that pool also has ${min} deliveries, then that complexity, then every delivery.`;
}

function basisText(basis) {
  return Object.entries(basis || {})
    .map(([key, count]) => `${basisLabel(key)} ${count}`)
    .join(" · ");
}

function scheduleShowsReview(predictions) {
  return predictions.some((row) => row && (row.review_mode != null || row.review_mode_fallback != null));
}

function reviewAssumptionText(row) {
  if (row.review_mode == null && row.review_mode_fallback == null) return "—";
  if (row.review_mode === "unknown") return "Unknown assumption";
  const mode = REVIEW_MODE_LABELS[row.review_mode] || row.review_mode || "—";
  if (row.review_mode_fallback) return `${mode} · mode dropped`;
  if (row.basis === "review_complexity" || row.basis === "review_workspace") return `${mode} · mode kept`;
  return mode;
}

function byComplexityCard() {
  const rows = (lastPayload.forecast?.by_complexity || []).map((row) => el("tr", {}, [
    el("th", { scope: "row", text: COMPLEXITY_LABELS[row.complexity] || row.complexity }),
    el("td", { class: "num", text: String(row.tasks) }),
    el("td", { class: "fc-basis", text: basisText(row.basis) }),
    el("td", { class: "num", text: formatForecastQuantileRange(row.work_hours, formatForecastHours) }),
    el("td", { class: "num", text: formatForecastQuantileRange(row.usd, formatForecastUsd) }),
    el("td", { class: "num", text: formatForecastQuantileRange(row.tokens, formatForecastTokens) }),
  ]));
  const content = rows.length
    ? table("Forecast by complexity", [["Complexity"], ["Tasks", true], ["Sampled from"], ["Work hours", true], ["Cost", true], ["Tokens", true]], rows, "p50 – p90 per complexity. Work hours sum every task's slot time, so they exceed the time to drain when runs overlap.")
    : el("div", { class: "mx-empty-note", text: "No backlog tasks." });
  return card("By complexity", null, [content]);
}

function coverageCard() {
  const coverage = lastPayload.coverage || {};
  const backlog = lastPayload.backlog || {};
  const arrivals = lastPayload.arrivals?.coverage || {};
  const min = lastPayload.inputs?.min_cell_samples ?? 5;
  const thinHint = reportsReviewMode()
    ? `Fewer than ${min} deliveries of the same complexity, crew pool and review mode. The wider pool may keep the review mode (any crew of that complexity, then any delivery of that mode) or drop it. Review mode dropped counts only the tasks that lost review conditioning.`
    : `Fewer than ${min} deliveries of the same complexity and crew pool: sampled from a wider pool.`;
  const reviewItems = reportsReviewMode() ? [
    ["Review mode dropped", coverage.review_mode_fallbacks, `The sample no longer conditions on the task's review mode. The mode-preserving pools had fewer than ${min} deliveries, including when the lookback has no history. A wider pool that still keeps the review mode is counted under thin-cell fallbacks instead.`],
    ["Unknown review assumption", coverage.review_mode_unknown, "The task's review mode could not be resolved, so it is not treated as a measured review policy. An unknown assumption uses the mode-pooled fallback."],
    ["History with unknown review", coverage.history_review_mode_unknown, "Deliveries in the lookback whose review evidence is missing or contradictory. They are not a measured review mode."],
  ] : [];
  const items = [
    ["Unassessed", coverage.unassessed, "No complexity yet: sampled from every delivery."],
    ["Thin-cell fallbacks", coverage.thin_cell_fallbacks, thinHint],
    ...reviewItems,
    ["Uncovered model", coverage.uncovered_model, "No sampled delivery has every run priced, so tokens are not forecast for these tasks."],
    ["Uncosted", coverage.uncosted, "No sampled delivery has every run's provider cost, so cost is not forecast for these tasks."],
    ["No history", coverage.no_history, "Nothing to sample at all."],
    ["Unforecastable dependency", coverage.dependency_unforecastable, "Depends on work the drain will not start (in review, blocked, someday, unapproved, elsewhere or missing), sits behind such work, or is on a dependency cycle. Left out of the full totals."],
    ["Unverifiable dependencies", coverage.unverifiable_dependencies, "Dependencies this workspace cannot verify. The drain treats them as satisfied, and so does the forecast."],
    ["Review lock overlaps", coverage.review_lock_overlaps, "Overlaps files a task in review still locks. The forecast does not model when review releases them."],
    ["Blocked", coverage.blocked, "Blocked now: each waits for a return drawn from how long blocked work took to come back, given how long it has waited."],
    ["Attempt-outcome fallbacks", coverage.outcome_fallbacks, `Fewer than ${lastPayload.inputs?.min_cell_samples ?? 5} recorded attempts of the same complexity, crew pool and review mode: outcomes sampled from a wider pool.`],
    ["No attempt history", coverage.outcome_no_history, "No recorded attempt to sample: every first attempt is assumed to deliver."],
    ["No return history", coverage.return_unsupported, "Blocked, with no recorded wait to draw a return from. Left out of the full totals."],
    ["Unknown return tail", coverage.return_tail_unsupported, "Recorded waits run out before 90 days: a draw past the last one counts as no return."],
    ["Unknown elapsed time", coverage.phase_age_unknown, "Running or blocked, but the history does not say since when: treated as just started."],
    ["Arrival history: missing tokens", arrivals.missing_tokens, "Deliveries missing normalized tokens in the pool used by recent arrivals."],
    ["Arrival history: missing cost", arrivals.missing_usd, "Deliveries missing provider cost in the pool used by recent arrivals."],
  ];
  const list = el("dl", { class: "fc-coverage" }, items.map(([label, value, hint]) => el("div", { class: "fc-cov" }, [
    el("dt", {}, [label, " ", infoHint(hint)]),
    el("dd", { class: value ? "fc-warn" : null, text: String(value ?? 0) }),
  ])));
  const left = `${backlog.review ?? 0} in review are not forecast.`;
  const arrivalNote = `Recent arrivals sample all ${arrivals.deliveries ?? 0} history deliveries for both the backlog and if-approved totals. A sampled arrival missing tokens or cost makes that iteration's total unavailable for that metric; duration remains independent. Missing totals sort last, so p50 or p90 can show —. These gaps do not affect the no-arrivals forecast.`;
  return card("Coverage", `${coverage.tasks ?? 0} initial tasks forecast`, [list, el("p", { class: "mx-blurb", text: arrivalNote }), el("p", { class: "mx-blurb", text: left })]);
}

// ---- calibration -----------------------------------------------------------

function hitRow(label, hits, target) {
  return el("tr", {}, [
    el("th", { scope: "row", text: label }),
    el("td", { class: "num", text: String(hits?.n ?? 0) }),
    el("td", { class: "num", text: fmtShare(hits?.under_p50) }),
    el("td", { class: "num", text: fmtShare(hits?.under_p90) }),
    el("td", { class: "fc-target mx-muted", text: target }),
  ]);
}

function excludedText(excluded) {
  const parts = [["rejected", excluded?.rejected], ["re-scoped", excluded?.rescoped], ["removed", excluded?.missing], ["no runs", excluded?.no_delivery]]
    .filter(([, count]) => count)
    .map(([label, count]) => `${count} ${label}`);
  return parts.length ? parts.join(" · ") : "—";
}

function versionHits(hits) {
  return hits?.n ? `${fmtShare(hits.under_p50)} · ${fmtShare(hits.under_p90)} (${hits.n})` : "—";
}

function versus(forecast, actual, fmt) {
  return `${formatForecastQuantileRange(forecast, fmt)} → ${actual == null ? "—" : fmt(actual)}`;
}

function calibrationCard() {
  const calibration = lastPayload.calibration || {};
  const stored = lastPayload.stored || {};
  const forecasts = calibration.forecasts || [];
  const meta = stored.latest_at ? `latest stored ${formatDateTime(stored.latest_at)}` : "none stored yet";
  const blurb = el("p", { class: "mx-blurb" }, [
    `The default forecast is stored at most once every ${stored.min_interval_minutes ?? 60} minutes while this view is open, then scored as its tasks finish. A well-calibrated forecast has about 50% of actuals under p50 and 90% under p90. `,
    infoHint("Tasks since rejected, archived, removed or re-scoped to another complexity, and done tasks with no agent runs after the forecast, are left out and counted. A forecast is scored as a whole only once all its tasks are done and none was left out."),
  ]);
  if (!forecasts.length) {
    return card("Calibration", meta, [blurb, el("div", { class: "mx-empty" }, [
      el("div", { class: "mx-empty-title", text: "No stored forecasts to score yet" }),
      el("div", { class: "mx-empty-detail", text: "Calibration appears once a stored forecast's tasks start finishing." }),
    ])]);
  }
  const versions = calibration.by_model_version || [];
  const current = calibration.model_version;
  const taskLevel = calibration.task_level || {};
  const forecastLevel = calibration.forecast_level || {};
  const summary = table("Calibration summary", [["Measure"], ["Scored", true], ["Under p50", true], ["Under p90", true], ["Target"]], [
    hitRow("Task hours", taskLevel.hours, "50% · 90%"),
    hitRow("Task cost", taskLevel.usd, "50% · 90%"),
    hitRow("Task tokens", taskLevel.tokens, "50% · 90%"),
    hitRow("Time to drain", forecastLevel.time_to_drain_hours, "50% · 90%"),
    hitRow("Backlog cost", forecastLevel.usd, "50% · 90%"),
    hitRow("Backlog tokens", forecastLevel.tokens, "50% · 90%"),
  ], current != null ? `Model v${current}, the current model. Each version is scored on its own forecasts.` : null);
  const older = versions.filter((row) => row.model_version !== current);
  const byVersion = older.length ? table("Calibration by model version", [["Model"], ["Forecasts", true], ["Task hours", true], ["Time to drain", true]], older.map((row) => el("tr", {}, [
    el("th", { scope: "row", text: `v${row.model_version}` }),
    el("td", { class: "num", text: String(row.forecasts ?? 0) }),
    el("td", { class: "num", text: versionHits(row.task_level?.hours) }),
    el("td", { class: "num", text: versionHits(row.forecast_level?.time_to_drain_hours) }),
  ])), "Earlier models, under p50 · under p90 (scored).") : null;
  const rows = forecasts.map((row) => el("tr", {}, [
    el("th", { scope: "row", text: formatDateTime(row.forecast_at) }),
    el("td", { class: "num", text: row.model_version != null ? `v${row.model_version}` : "—" }),
    el("td", { class: "num", text: `${row.done}/${row.tasks}` }),
    el("td", { text: excludedText(row.excluded) }),
    el("td", { class: "num", text: versus(row.forecast?.time_to_drain_hours, row.actual?.time_to_drain_hours, formatForecastHours) }),
    el("td", { class: "num", text: versus(row.forecast?.usd, row.actual?.usd, formatForecastUsd) }),
    el("td", { class: "num", text: versus(row.forecast?.tokens, row.actual?.tokens, formatForecastTokens) }),
    el("td", { class: "fc-state", text: row.scored_whole ? "scored" : row.settled ? "settled, not scored" : `${row.pending} pending` }),
  ]));
  const excluded = calibration.excluded || {};
  const history = table("Stored forecasts", [["Recorded"], ["Model", true], ["Done", true], ["Left out"], ["Drain p50 – p90 → actual", true], ["Cost", true], ["Tokens", true], ["State"]], rows);
  return card("Calibration", meta, [
    blurb,
    summary,
    byVersion,
    el("p", { class: "mx-blurb", text: `Left out across stored forecasts: ${excludedText(excluded)}.` }),
    history,
  ]);
}
