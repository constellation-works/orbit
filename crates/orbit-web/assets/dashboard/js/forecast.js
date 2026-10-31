// Orbit dashboard Analytics › Forecast: how long the dispatchable backlog
// takes to drain and what it costs, as p50 and p90 resampled from this
// workspace's delivery history, beside the recent arrival rate the forecast
// assumes away, and how earlier stored forecasts scored against what their
// tasks actually did. Read from /api/diagnostics/backlog_forecast. The Tasks
// drain card reuses this default read (30 days, no arrivals) as a compact summary.
// Pure vanilla JS, split into ES modules with no build step.
//
// The forecast is per workspace; the aggregate view shows the usual
// placeholder. The history select and arrivals toggle refetch: only the default
// view (30 days reaching back to at least the newest 50 completions, no
// arrivals, the live drain ceiling) is the one stored and scored, and is read
// from the newest stored snapshot when one is fresh; Refresh recomputes it
// live and keeps that result until the workspace or forecast options change.
// History is
// counted in tasks, `Last 200` or `Last 500`, or in days; a month without
// deliveries leaves the default and the counted views with history.

import {
  el as makeEl, syncNodes, fetchJson, requestPanel, panelCanRender, getWorkspaceRevision,
  onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime, infoHint,
  scopedHashHref, bindTaskLink, relativeTime, statusPill, detailsPanel,
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
const UNREPORTED_FORECAST_REASON = "Forecast unavailable, reason not reported";
// History selection: the default lookback (30 days, at least the newest 50
// completions), the newest N completions whatever their age, or 90 days.
const HISTORIES = [
  ["default", "30d · ≥50", "The last 30 days, reaching back to at least the newest 50 completed tasks: the stored forecast"],
  ["tasks:200", "Last 200", "The newest 200 completed tasks, whatever their age"],
  ["tasks:500", "Last 500", "The newest 500 completed tasks, whatever their age"],
  ["days:90", "90d", "The last 90 days, reaching back to at least the newest 50 completed tasks"],
];
const ARRIVALS = [["off", "No arrivals"], ["on", "With arrivals"]];
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
  ["scheduled", "Scheduled time", "Waiting until the task’s earliest scheduled start; admission can start it later."],
  ["dependency", "Dependencies", "Waiting for an earlier task in this forecast to finish."],
  ["lock", "File locks", "Its files overlap a running task's locked files."],
  ["reservation", "Surface reservations", "Higher-priority lock-blocked work reserved an overlapping surface."],
  ["footprint", "Unknown footprint", "An editing task without context files runs alone, or waits for the tree to be free."],
  ["capacity", "Capacity", "Ready and unblocked, but every slot is taken."],
  ["return", "Returns", "Blocked, or blocked by an attempt, and waiting for a sampled return to the queue."],
];
// The schedule table lists this many tasks in dispatch order.
const SCHEDULE_ROWS = 50;
// Stored forecasts are newest first. Keep calibration history to one bounded
// page so its column headings stay close to the rows being inspected.
const CALIBRATION_HISTORY_PAGE_SIZE = 10;

const view = { history: "default", arrivals: "off" };
let lastPayload = null;
let requestScope = null;
let liveRefresh = null;
let calibrationHistoryPage = 0;

function clearForecast() {
  lastPayload = null;
  liveRefresh = null;
  calibrationHistoryPage = 0;
  const count = $(COUNT);
  if (count) count.textContent = "—";
}

// The default history and arrivals off omit the query, matching the stored
// forecast. `history` is `tasks:<n>` or `days:<n>`; `refresh` computes live.
export function backlogForecastPath({ history = "default", arrivals = "off", refresh = false } = {}) {
  const params = new URLSearchParams();
  const [kind, count] = String(history).split(":");
  if (kind === "tasks" && count) params.set("history_tasks", count);
  if (kind === "days" && count && count !== "30") params.set("lookback_days", count);
  if (arrivals === "on") params.set("arrivals", "on");
  if (refresh) params.set("refresh", "1");
  const query = params.toString();
  return query ? `${ENDPOINT}?${query}` : ENDPOINT;
}

function endpoint() {
  return backlogForecastPath({ history: view.history, arrivals: view.arrivals });
}

export function fetchBacklogForecast(options) {
  return fetchJson(backlogForecastPath(options));
}

export function placeholdForecastAggregate() {
  clearForecast();
  renderPanelPlaceholder(BODY);
}

// `refresh` recomputes live; the panel scope stays the view's path, so the
// current forecast stays up while it runs. Default polls share an explicit
// Refresh's promise, including after it settles, until its view changes.
export function fetchAndRenderForecast({ refresh = false } = {}) {
  if (isAggregateView()) {
    placeholdForecastAggregate();
    return Promise.resolve();
  }
  const revision = getWorkspaceRevision();
  const path = endpoint();
  const scope = `${revision}:${path}`;
  if (requestScope !== scope) {
    requestScope = scope;
    clearForecast();
  }
  if (!refresh && liveRefresh) return liveRefresh;
  const request = refresh ? backlogForecastPath({ ...view, refresh: true }) : path;
  const pending = requestPanel(BODY, path, () => fetchJson(request), (payload) => {
    if (getWorkspaceRevision() !== revision) return;
    lastPayload = payload;
    renderForecast();
  }, COUNT);
  if (!refresh) return pending;
  const retained = pending.catch((error) => {
    if (liveRefresh === retained) liveRefresh = null;
    throw error;
  });
  liveRefresh = retained;
  return retained;
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
  const nodes = [headerBar(), answerCard(), tasksCard(), gapsCard(), calibrationCard(), detailSections()];
  const root = el("div", { class: "fc-view" }, nodes);
  root.dataset.key = "forecast-view";
  root.dataset.hash = JSON.stringify([view, lastPayload, calibrationHistoryPage]);
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

function backlogForecastFreshness(payload) {
  if (payload.served_from === "live") return "Live";
  if (payload.served_from === "stored") {
    const age = relativeTime(payload.generated_at);
    return age ? `Stored ${age}` : "Stored time unavailable";
  }
  return "Forecast freshness unavailable";
}

function supportText(forecast, payload) {
  const support = forecast?.regression_rework?.support;
  const coverage = payload?.coverage || {};
  const parts = [];
  if (support && typeof support === "object") {
    if (num(support.missing_fix_history) != null) parts.push(`${forecastCount(support.missing_fix_history)} with missing fix history`);
    if (num(support.history_fixes) != null) parts.push(`${forecastCount(support.history_fixes)} recorded regression fixes`);
    if (num(support.history_bundles) != null) parts.push(`${forecastCount(support.history_bundles)} regression bundles`);
    if (support.history_thin) parts.push("regression history is thin");
  }
  if (num(coverage.regression_history_missing)) parts.push(`${forecastCount(coverage.regression_history_missing)} tasks missing regression history`);
  if (num(coverage.risk_unestimated)) parts.push(`${forecastCount(coverage.risk_unestimated)} tasks with unestimated risk`);
  if (num(coverage.regression_history_thin)) parts.push(`${forecastCount(coverage.regression_history_thin)} tasks with thin regression history`);
  return parts.length ? `Available evidence: ${parts.join(" · ")}.` : "";
}

/// Reasons the adjusted forecast is unavailable, from `forecast.unavailable`.
/// Null when no flag is set (older payloads omit the object entirely).
export function forecastUnavailableInfo(forecast, payload = {}) {
  const unavailable = forecast?.unavailable;
  if (!unavailable || typeof unavailable !== "object") return null;
  const reasons = [];
  if (unavailable.regression_history_missing) reasons.push({ kind: "regression_history_missing", label: "Missing regression history", detail: "Regression follow-up work cannot be estimated for some tasks, so the regression-adjusted forecast is unavailable." });
  if (unavailable.risk_unestimated) reasons.push({ kind: "risk_unestimated", label: "Risk not estimated", detail: "Regression risk is not estimated for some tasks, so the adjusted forecast is unavailable." });
  if (unavailable.arrival_conflict_unknown) reasons.push({ kind: "arrival_conflict_unknown", label: "Arrival conflicts unknown", detail: "Whether arriving tasks would conflict with this backlog is unknown, so the adjusted forecast is unavailable." });
  if (!reasons.length) return null;
  const support = supportText(forecast, payload);
  return { kind: reasons[0].kind, reasons, label: reasons.map((r) => r.label).join(" · "), detail: [reasons.map((r) => r.detail).join(" "), support].filter(Boolean).join(" "), support };
}

function quantileText(quantiles, fmt) {
  const p50 = num(quantiles?.p50);
  const p90 = num(quantiles?.p90);
  if (p50 == null && p90 == null) return null;
  return `p50 ${p50 == null ? "unknown" : fmt(p50)} · p90 ${p90 == null ? "unknown" : fmt(p90)}`;
}

function hasHorizonCensoringEvidence(forecast) {
  const drainProbability = num(forecast?.drain_probability);
  const completionProbability = num(forecast?.completion_probability);
  const unfinished = num(forecast?.not_expected_to_drain);
  return (drainProbability != null && drainProbability < 1)
    || (completionProbability != null && completionProbability < 1)
    || (unfinished != null && unfinished > 0);
}

/// Formatted pre-regression baseline figures; each missing percentile stays
/// unknown and no metric is invented. Null when the baseline has no figures.
export function forecastBaselineFigures(baseline) {
  if (!baseline || typeof baseline !== "object") return null;
  const figures = {
    time: quantileText(baseline.time_to_drain_hours, formatForecastHours),
    cost: quantileText(baseline.usd, formatForecastUsd),
    tokens: quantileText(baseline.tokens, formatForecastTokens),
  };
  return figures.time || figures.cost || figures.tokens ? figures : null;
}

// The baseline's figures as one sentence ("Time to drain p50 … · Tokens …."),
// for the drain card's tooltip when the adjusted forecast is unavailable
// [ORB-20453]; "" when there are none.
function baselineFigureText(figures) {
  if (!figures) return "";
  const parts = [figures.time && `Time to drain ${figures.time}`, figures.cost && `Cost ${figures.cost}`, figures.tokens && `Tokens ${figures.tokens}`].filter(Boolean);
  return parts.length ? `${parts.join(" · ")}.` : "";
}

function baselineText(figures) {
  const text = baselineFigureText(figures);
  return text ? `Pre-regression baseline (excludes unsupported regression follow-up work): ${text}` : "";
}

export function backlogForecastFailureSummary() {
  return { unavailable: true, reasonKind: "transport", reason: "Couldn't load the backlog forecast", detail: "The forecast could not be loaded. Check the connection and try again." };
}

/// Compact drain-card figures. When the adjusted forecast is unavailable this
/// still returns a summary carrying the reason (and baseline, when present),
/// so the drain card can explain itself instead of a generic line. Null only
/// for a non-object payload.
export function backlogForecastSummary(payload) {
  if (!payload || typeof payload !== "object") return null;
  const forecast = payload.forecast || {};
  const backlog = payload.backlog || {};
  const counts = `${forecastCount(backlog.backlog)} backlog · ${forecastCount(backlog.in_progress)} in progress`;
  const freshness = backlogForecastFreshness(payload);
  const info = forecastUnavailableInfo(forecast, payload);
  if (info) {
    const figures = forecastBaselineFigures(forecast.pre_regression_baseline);
    return { unavailable: true, reasonKind: info.kind, reason: info.label, detail: info.detail, reasons: info.reasons, baseline: figures, baselineText: baselineText(figures), baselineBars: figures ? backlogBarQuantiles(forecast.pre_regression_baseline) : null, counts, freshness, drain: null, cost: null, exactCost: null, tokens: null };
  }
  if (num(forecast.tasks) === 0) {
    return { unavailable: true, reasonKind: "empty", reason: "No backlog to forecast", detail: "No backlog or in-progress tasks in this workspace.", baseline: null, baselineText: "", counts, freshness, drain: null, cost: null, exactCost: null, tokens: null };
  }
  const hours = forecast.time_to_drain_hours;
  if (quantilePair(hours)) {
    const costed = quantilePair(forecast.usd);
    return {
      drain: formatForecastQuantileRange(hours, formatForecastHours),
      cost: costed ? formatForecastQuantileRange(forecast.usd, wholeUsd) : null,
      exactCost: costed ? formatForecastQuantileRange(forecast.usd, formatForecastUsd) : null,
      tokens: quantilePair(forecast.tokens) ? formatForecastQuantileRange(forecast.tokens, formatForecastTokens) : null,
      bars: backlogBarQuantiles(forecast),
      references: backlogBarReferences(forecast.pre_regression_baseline),
      counts, freshness,
    };
  }
  // No adjusted quantiles and no unavailable flags: classify from evidence.
  const tasks = num(forecast.tasks);
  const history = num(payload.inputs?.history_deliveries);
  const coverage = payload.coverage || {};
  let reasonKind = "unknown";
  let reason = UNREPORTED_FORECAST_REASON;
  let detail = "The forecast did not report why it is unavailable.";
  if (tasks === 0) { reasonKind = "empty"; reason = "No backlog to forecast"; detail = "No backlog or in-progress tasks in this workspace."; }
  else if (history === 0 || (tasks != null && tasks > 0 && num(coverage.no_history) != null && num(coverage.no_history) >= tasks)) { reasonKind = "no_history"; reason = "No delivery history to sample"; detail = "There is no delivery history to sample for this backlog."; }
  else if (forecast.schedulable_subset) { reasonKind = "subset"; reason = "Not every task can be scheduled"; detail = "Some tasks depend on work the drain cannot start, so there is no full total."; }
  else if (tasks != null && tasks > 0 && hasHorizonCensoringEvidence(forecast)) { reasonKind = "horizon"; reason = "Beyond the 90-day horizon"; detail = "The simulated backlog did not drain within the 90-day horizon."; }
  const figures = forecastBaselineFigures(forecast.pre_regression_baseline);
  return { unavailable: true, reasonKind, reason, detail, baseline: figures, baselineText: baselineText(figures), baselineBars: figures ? backlogBarQuantiles(forecast.pre_regression_baseline) : null, counts, freshness, drain: null, cost: null, exactCost: null, tokens: null };
}

// The backlog card's bar rows: key, label, forecast field and figure format.
// Cost is whole dollars so the row fits; the tooltip keeps the cents.
const BACKLOG_BAR_METRICS = [
  ["drain", "Time to drain", "time_to_drain_hours", formatForecastHours],
  ["cost", "Cost", "usd", wholeUsd],
  ["tokens", "Tokens", "tokens", formatForecastTokens],
];

// Each metric's p50 and p90 as numbers (null when unknown). A metric with
// neither quantile is null, so its row is left out rather than drawn empty.
function backlogBarQuantiles(source) {
  const bars = {};
  for (const [key, , field] of BACKLOG_BAR_METRICS) {
    bars[key] = quantilePair(source?.[field]) ? { p50: num(source[field].p50), p90: num(source[field].p90) } : null;
  }
  return bars;
}

// The reference tick per metric: the pre-regression baseline's p50, which
// sits on the same quantile as the solid fill, so the gap between the fill's
// end and the tick is the regression follow-up work the adjusted forecast
// adds. Core sends a baseline only beside an unavailable adjusted forecast
// today, so a ready summary normally has no ticks. Null when there is none.
function backlogBarReferences(baseline) {
  if (!baseline || typeof baseline !== "object") return null;
  const references = {};
  for (const [key, , field] of BACKLOG_BAR_METRICS) references[key] = num(baseline[field]?.p50);
  return Object.values(references).some((value) => value != null) ? references : null;
}

// Share of the track the largest figure may fill, so a p90 never reads as a
// bar pinned to a limit when there is no limit.
const BACKLOG_BAR_SPAN = 0.9;

// One row in the CPU and Memory meter's language: label, bar and figures.
// The solid fill is p50 and the lighter extension runs from p50 to p90. Each
// bar is scaled to its own largest figure (its p90, or the tick when that is
// further), since the three metrics share no unit. A missing quantile reads
// "unknown" and draws nothing in its place.
function backlogBarRow([key, label, , fmt], quantiles, reference, caveat) {
  const { p50, p90 } = quantiles;
  const top = Math.max(p50 ?? 0, p90 ?? 0, reference ?? 0);
  const share = (value) => `${(top > 0 ? Math.max(0, value) / top * BACKLOG_BAR_SPAN * 100 : 0).toFixed(2)}%`;
  const figures = `${p50 == null ? "unknown" : fmt(p50)} – ${p90 == null ? "unknown" : fmt(p90)}`;
  const meter = el("span", { class: "drain-forecast-meter drain-backlog-forecast-meter", "aria-hidden": "true" }, [
    p50 != null && p90 != null && p90 > p50
      ? el("span", { class: "drain-backlog-forecast-spread", style: { left: share(p50), width: share(p90 - p50) } })
      : null,
    p50 != null ? el("span", { class: "drain-forecast-fill", style: { width: share(p50) } }) : null,
    reference != null ? el("span", { class: "drain-forecast-tick", style: { left: share(reference) } }) : null,
  ].filter(Boolean));
  const spoken = [`${label} p50 ${p50 == null ? "unknown" : fmt(p50)}, p90 ${p90 == null ? "unknown" : fmt(p90)}`,
    reference != null ? `pre-regression baseline p50 ${fmt(reference)}` : "", caveat].filter(Boolean).join(", ");
  const row = el("div", { class: "drain-forecast-peak drain-backlog-forecast-row" }, [
    el("dt", { text: label }),
    el("dd", { class: "drain-forecast-bar" }, [meter]),
    el("dd", { class: "drain-forecast-figure" }, [
      el("span", { class: "drain-forecast-figure-main mono", "aria-hidden": "true", text: figures }),
      el("span", { class: "visually-hidden", text: spoken }),
    ]),
  ]);
  row.dataset.metric = key;
  return row;
}

// The rows for every metric with a known quantile, or null when none has one.
function backlogBarList(bars, references, caveat) {
  if (!bars) return null;
  const rows = BACKLOG_BAR_METRICS
    .filter(([key]) => bars[key])
    .map((metric) => backlogBarRow(metric, bars[metric[0]], references?.[metric[0]] ?? null, caveat));
  if (!rows.length) return null;
  return el("dl", { class: "drain-forecast-peaks drain-backlog-forecast-bars" }, rows);
}

// Whole dollars, so the dock line fits; the tooltip keeps the cents.
function wholeUsd(value) {
  const n = num(value);
  return n == null ? "—" : `$${Math.round(n).toLocaleString()}`;
}

/// Compact link to Analytics › Forecast: time to drain, cost and tokens as
/// bar rows (p50 fill, p50 – p90 extension, figures beside), with a
/// freshness label for all the snapshot's figures. Counts and exact cost are
/// in its tooltip and accessible text. When the adjusted forecast is
/// unavailable the rows draw the pre-regression baseline, muted and labelled
/// as such, or there are no rows; the reason and the baseline's caveat stay
/// in the tooltip and accessible text [ORB-20453].
export function drainBacklogForecastLink(summary) {
  const unavailable = !summary || Boolean(summary.unavailable);
  const link = el("a", { class: unavailable ? "drain-backlog-forecast unavailable" : "drain-backlog-forecast" });
  link.href = scopedHashHref("#analytics/forecast");
  if (unavailable) {
    const reason = summary?.reason || "Backlog forecast unavailable";
    const detail = [reason, summary?.detail, summary?.baselineText, summary?.counts].filter(Boolean).join(" ");
    link.title = `${detail}\nAnalytics › Forecast`;
    if (summary) link.dataset.forecastReason = summary.reasonKind || "unknown";
    const bars = backlogBarList(summary?.baselineBars, null, "pre-regression baseline");
    if (bars) bars.classList.add("baseline");
    for (const child of [
      el("span", { class: "drain-section-label", text: "Backlog" }),
      bars ? el("span", { class: "drain-forecast-meta drain-backlog-forecast-basis", text: "pre-regression baseline" }) : null,
      bars,
      el("span", { class: "visually-hidden", text: ` ${detail}` }),
      el("span", { class: "drain-backlog-forecast-go", text: "›", "aria-hidden": "true" }),
    ]) if (child) link.append(child);
    return link;
  }
  const detail = [
    summary.freshness, summary.counts,
    summary.drain && `Time to drain ${summary.drain}`, summary.cost && `Cost ${summary.cost}`,
    summary.tokens && `Tokens ${summary.tokens}`, summary.exactCost && `Exact cost ${summary.exactCost}`,
  ].filter(Boolean).join(" · ");
  const accessibleDetail = [summary.counts, summary.exactCost && `Exact cost ${summary.exactCost}`].filter(Boolean).join(" · ");
  link.title = `p50 – p90 · ${detail}\nAnalytics › Forecast`;
  const bars = backlogBarList(summary.bars, summary.references, "");
  link.append(...[
    el("span", { class: "drain-section-label", text: "Backlog" }),
    el("span", { class: "drain-forecast-meta drain-backlog-forecast-freshness", text: summary.freshness }),
    bars,
    summary.references ? el("span", { class: "drain-backlog-forecast-legend", text: "pre-regression baseline p50" }) : null,
    el("span", { class: "visually-hidden", text: ` p50 – p90, ${accessibleDetail}` }),
    el("span", { class: "drain-backlog-forecast-go", text: "›", "aria-hidden": "true" }),
  ].filter(Boolean));
  return link;
}

// ---- page -----------------------------------------------------------------
//
// Top to bottom: the header, the answer (time to drain, if approved, cost),
// one tasks table, the data gaps that are not zero, calibration, then the
// collapsed detail: stored forecasts, every wait gate, and how the forecast
// is made with the full coverage checklist.

function seg(label, options, current, onPick) {
  return el("div", { class: "mx-seg mx-seg-wrap", role: "group", "aria-label": label }, options.map(([key, text, title]) => {
    const on = key === current;
    const button = el("button", { class: on ? "mx-seg-btn on" : "mx-seg-btn", type: "button", text, title: title || null, "aria-pressed": on ? "true" : "false" });
    button.addEventListener("click", () => {
      if (on) return;
      onPick(key);
      refetch();
    });
    return button;
  }));
}

function historyControl() {
  const select = el("select", { class: "mx-select", "aria-label": "Delivery history" }, HISTORIES.map(([key, text, title]) => {
    const option = el("option", { text, title });
    option.value = key;
    option.selected = key === view.history;
    return option;
  }));
  select.addEventListener("change", () => {
    if (select.value === view.history) return;
    view.history = select.value;
    refetch();
  });
  return el("label", { class: "fc-history" }, [el("span", { class: "fc-control-label", text: "History" }), select]);
}

// Where this forecast came from and how old it is. The absolute time is its
// tooltip; Refresh recomputes it live.
function sourceLabel() {
  const stored = lastPayload.served_from === "stored";
  const age = relativeTime(lastPayload.generated_at);
  const text = stored ? `Stored ${age ?? "earlier"}` : "Live";
  const title = lastPayload.generated_at ? `Computed ${formatDateTime(lastPayload.generated_at)}` : null;
  return el("span", { class: "fc-source", text, title });
}

function refreshButton() {
  const button = el("button", { class: "mx-seg-btn", type: "button", text: "Refresh", title: "Recompute this forecast live now", "aria-label": "Refresh forecast" });
  button.addEventListener("click", () => {
    button.disabled = true;
    fetchAndRenderForecast({ refresh: true })
      .catch((error) => console.error("Failed to refresh backlog forecast", error))
      .finally(() => { button.disabled = false; });
  });
  return el("div", { class: "mx-seg", role: "group", "aria-label": "Forecast refresh" }, [button]);
}

// The panel head already reads "N tasks · N slots"; this line continues it.
function headerBar() {
  const meta = el("p", { class: "fc-meta" }, [
    sourceLabel(),
    lastPayload.model_version != null ? ` · model v${lastPayload.model_version}` : null,
  ]);
  return el("div", { class: "fc-header" }, [
    meta,
    el("div", { class: "fc-tools" }, [
      historyControl(),
      seg("Arrivals", ARRIVALS, view.arrivals, (key) => { view.arrivals = key; }),
      refreshButton(),
    ]),
  ]);
}

function card(title, meta, children, className = "", id = null) {
  const node = el("section", { id, class: `mx-card ${className}`.trim(), "aria-label": title }, [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title", text: title }),
      meta ? el("span", { class: "mx-card-meta", text: meta }) : null,
    ]),
    ...children,
  ]);
  return node;
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

// ---- answer ---------------------------------------------------------------

// One read-out: p50 large with p90 beside it. `unknown` names a percentile
// the response left out; adjusted figures use "—", the baseline "unknown".
function figure(label, quantiles, fmt, foot, { className = "fc-kpi", unknown = "—" } = {}) {
  const value = (q) => (num(quantiles?.[q]) == null ? unknown : fmt(quantiles[q]));
  const feet = (Array.isArray(foot) ? foot : [foot]).filter(Boolean);
  return el("div", { class: `mx-sc fc-fig ${className}`, role: "group", "aria-label": label }, [
    el("span", { class: "mx-sc-label", text: label }),
    el("span", { class: "fc-kpi-pair" }, [
      el("span", { class: "fc-q" }, [el("span", { class: "mx-sc-tag", text: "p50" }), el("span", { class: "mx-sc-value", text: value("p50") })]),
      el("span", { class: "fc-q fc-p90" }, [el("span", { class: "mx-sc-tag", text: "p90" }), el("span", { class: "fc-p90-value", text: value("p90") })]),
    ]),
    ...feet.map((text) => el("span", { class: "mx-sc-foot", text })),
  ]);
}

// Unforecastable dependencies withhold every full total, whatever the history.
const SUBSET_FOOT = "not every task can be scheduled";

function metricFoot(quantiles, metric) {
  const info = forecastUnavailableInfo(lastPayload.forecast, lastPayload);
  if (info) return info.label;
  if (quantiles?.p90 != null) return metric === "tokens" ? "normalized" : null;
  if (lastPayload.forecast?.schedulable_subset) return SUBSET_FOOT;
  const coverage = lastPayload.coverage || {};
  if (metric === "tokens" && num(coverage.uncovered_model)) return "some tasks have no priced history";
  if (metric === "usd" && num(coverage.uncosted)) return "some tasks have no costed history";
  const missing = lastPayload.arrivals?.coverage?.[`missing_${metric}`];
  if (missing) return metric === "tokens" ? "some history is missing token telemetry" : "some history is missing cost telemetry";
  if (lastPayload.inputs?.arrivals && missing) return "incomplete history or arrival telemetry";
  return metric === "tokens" ? "not enough priced history" : "not enough costed history";
}

function drainFoot(quantiles) {
  const info = forecastUnavailableInfo(lastPayload.forecast, lastPayload);
  if (info) return info.label;
  if (quantiles?.p90 != null) return null;
  if (lastPayload.forecast?.schedulable_subset) return SUBSET_FOOT;
  if (quantiles?.p50 != null) return hasHorizonCensoringEvidence(lastPayload.forecast) ? "p90 beyond the 90-day horizon" : UNREPORTED_FORECAST_REASON;
  if (lastPayload.coverage?.no_history) return "some tasks have no history";
  return hasHorizonCensoringEvidence(lastPayload.forecast) ? "beyond the 90-day horizon" : UNREPORTED_FORECAST_REASON;
}

function costedFoot() {
  const tasks = forecastCount(lastPayload.coverage?.tasks ?? lastPayload.forecast?.tasks);
  const costed = Math.max(0, tasks - forecastCount(lastPayload.coverage?.uncosted));
  return `${costed} of ${tasks} task${tasks === 1 ? "" : "s"} costed`;
}

// "+0.4h at p50" between two drain times on the same basis.
function drainDelta(next, base) {
  const after = num(next?.p50);
  const before = num(base?.p50);
  if (after == null || before == null) return null;
  const diff = after - before;
  if (Math.abs(diff) < 1 / 60) return "no change at p50";
  return `${diff > 0 ? "+" : "−"}${formatForecastHours(Math.abs(diff))} at p50`;
}

// The if-approved scenario's own unavailable evidence. Proposed coverage is
// a separate checklist, so the main backlog's coverage is not this reason.
function proposedForecastUnavailable() {
  const approved = lastPayload.if_approved;
  if (!approved || !forecastCount(lastPayload.backlog?.proposed)) return null;
  const proposedCoverage = lastPayload.proposed_coverage;
  const coverage = proposedCoverage && typeof proposedCoverage === "object" ? proposedCoverage : {};
  return forecastUnavailableInfo(approved, { coverage });
}

// If the proposed tasks were approved: the adjusted drain time, or the
// baseline when that is unavailable. The page's one warning, from whichever
// scenario is unavailable, covers why; this figure only says which it shows.
function approvedFigure() {
  const approved = lastPayload.if_approved;
  const proposed = forecastCount(lastPayload.backlog?.proposed);
  if (!approved || !proposed) return null;
  const forecast = lastPayload.forecast || {};
  const label = "If proposed work is approved";
  const noun = `${proposed} proposed task${proposed === 1 ? "" : "s"}`;
  const rest = (scenario) => `cost ${formatForecastQuantileRange(scenario?.usd, formatForecastUsd)} · tokens ${formatForecastQuantileRange(scenario?.tokens, formatForecastTokens)}`;
  if (proposedForecastUnavailable()) {
    const baseline = approved.pre_regression_baseline;
    if (!forecastBaselineFigures(baseline)) return figure(label, null, formatForecastHours, [`${noun} · no baseline`], { className: "fc-approved-fig", unknown: "unknown" });
    const delta = drainDelta(baseline.time_to_drain_hours, forecast.pre_regression_baseline?.time_to_drain_hours);
    return figure(label, baseline.time_to_drain_hours, formatForecastHours, [[noun, "baseline", delta].filter(Boolean).join(" · "), rest(baseline)], { className: "fc-approved-fig", unknown: "unknown" });
  }
  const delta = drainDelta(approved.time_to_drain_hours, forecast.time_to_drain_hours);
  return figure(label, approved.time_to_drain_hours, formatForecastHours, [[noun, delta].filter(Boolean).join(" · "), rest(approved)], { className: "fc-approved-fig" });
}

function focusGaps() {
  const target = $("fc-gaps");
  target?.scrollIntoView?.({ block: "start", behavior: "smooth" });
  target?.focus?.({ preventScroll: true });
}

function unavailableWarning(text, kind) {
  const link = el("button", { class: "fc-link", type: "button", text: "See data gaps" });
  link.addEventListener("click", focusGaps);
  const node = el("p", { class: "fc-warning fc-unavailable", role: "note" }, [text, " ", link]);
  node.dataset.forecastReason = kind;
  return node;
}

// The single warning when a regression-adjusted forecast is unavailable.
// Either scenario can supply it; the reasons and evidence are in Data gaps.
function warningLine(info, hasBaseline) {
  const missing = forecastCount(lastPayload.coverage?.regression_history_missing);
  const lead = hasBaseline ? "Baseline only — " : "No forecast — ";
  const text = info.kind === "regression_history_missing" && missing
    ? `${lead}regression follow-up work isn't included because ${missing} task${missing === 1 ? " has" : "s have"} no regression history.`
    : `${lead}the regression-adjusted forecast is unavailable: ${info.label}.`;
  return unavailableWarning(text, info.kind);
}

// The same one warning when only the proposed scenario is unavailable, so
// the main forecast stays the adjusted figures and the proposed baseline is
// still qualified.
function proposedWarning(info, hasBaseline) {
  const reason = `If approved, the adjusted forecast is unavailable: ${info.label}.`;
  const limit = hasBaseline
    ? " The baseline shown for proposed work excludes unsupported regression follow-up work and is not the regression-adjusted forecast."
    : "";
  return unavailableWarning(`${reason}${limit}`, info.kind);
}

function arrivalsLine() {
  const arrivals = lastPayload.arrivals || {};
  const perDay = arrivals.per_day || {};
  const parts = [["auto-tasks", perDay.auto_task], ["sweeps", perDay.sweep], ["filings", perDay.filing]]
    .map(([label, rate]) => `${label} ${fmtRate(rate)}`).join(" · ");
  const assumed = lastPayload.inputs?.arrivals
    ? "Includes new tasks arriving at the recent rate."
    : "Assumes no new tasks arrive.";
  return el("p", { class: "fc-line fc-arrivals" }, [
    `${assumed} Last ${arrivals.window_days ?? 7} days: ${fmtRate(perDay.total)} filed (${parts}), ${fmtRate(arrivals.done_per_day)} done.`,
  ]);
}

// Unforecastable dependencies withhold the full totals; the schedulable
// subset is what the simulation could still drain.
function subsetNote(forecast) {
  const subset = forecast.schedulable_subset;
  if (!subset) return null;
  const left = (subset.of ?? 0) - (subset.tasks ?? 0);
  return el("p", { class: "fc-line fc-subset", role: "note" }, [
    el("strong", { text: `No full total: ${left} task${left === 1 ? "" : "s"} depend${left === 1 ? "s" : ""} on work the drain cannot start. ` }),
    `The other ${subset.tasks} of ${subset.of}: drain ${formatForecastQuantileRange(subset.time_to_drain_hours, formatForecastHours)}, cost ${formatForecastQuantileRange(subset.usd, formatForecastUsd)}, tokens ${formatForecastQuantileRange(subset.tokens, formatForecastTokens)} (p50 – p90).`,
  ]);
}

function drainOdds(forecast) {
  const horizon = formatForecastHours(forecast.horizon_hours ?? 2160);
  const drainProbability = num(forecast.drain_probability);
  const completionProbability = num(forecast.completion_probability);
  const parts = [];
  if (drainProbability != null) parts.push(`Drains within ${horizon} in ${fmtShare(drainProbability)} of runs`);
  if (completionProbability != null) parts.push(`the initial tasks all finish in ${fmtShare(completionProbability)}`);
  const unfinished = num(forecast.not_expected_to_drain);
  if (unfinished) parts.push(`${unfinished.toFixed(2)} tasks unfinished on average`);
  if (!parts.length) return null;
  const text = `${parts.join("; ")}.`;
  return forecast.simulation_truncated ? `${text} Some runs hit the event limit.` : text;
}

function answerCard() {
  const inputs = lastPayload.inputs || {};
  const forecast = lastPayload.forecast || {};
  const children = [];
  if (!forecast.tasks) {
    children.push(el("div", { class: "mx-empty" }, [
      el("div", { class: "mx-empty-title", text: "Nothing to forecast" }),
      el("div", { class: "mx-empty-detail", text: "No backlog or in-progress tasks in this workspace." }),
    ]));
  } else if (!inputs.history_deliveries) {
    children.push(el("div", { class: "mx-empty" }, [
      el("div", { class: "mx-empty-title", text: "No delivery history to sample" }),
      el("div", { class: "mx-empty-detail", text: inputs.history_tasks != null
        ? `None of the newest ${inputs.history_tasks} completed tasks has recorded agent runs.`
        : `No task finished with recorded agent runs in the last ${inputs.lookback_days} days or among the newest ${inputs.min_history_tasks ?? 50} completions. Widen the history.` }),
    ]));
  } else {
    const info = forecastUnavailableInfo(forecast, lastPayload);
    if (info) {
      const baseline = forecast.pre_regression_baseline;
      const hasBaseline = Boolean(forecastBaselineFigures(baseline));
      const unknown = { className: "fc-baseline-fig", unknown: "unknown" };
      const figures = el("div", { class: hasBaseline ? "fc-figures fc-baseline" : "fc-figures" }, [
        hasBaseline ? figure("Time to drain", baseline.time_to_drain_hours, formatForecastHours, "pre-regression baseline", unknown) : null,
        approvedFigure(),
        hasBaseline ? figure("Cost", baseline.usd, formatForecastUsd, costedFoot(), unknown) : null,
      ]);
      if (hasBaseline) figures.dataset.forecastBaseline = "pre-regression";
      if (figures.children.length) children.push(figures);
      children.push(warningLine(info, hasBaseline));
      if (hasBaseline) children.push(el("div", { class: "fc-minor-row" }, [figure("Tokens", baseline.tokens, formatForecastTokens, "pre-regression baseline", { ...unknown, className: "fc-baseline-fig fc-minor" })]));
    } else {
      children.push(el("div", { class: "fc-figures" }, [
        figure("Time to drain", forecast.time_to_drain_hours, formatForecastHours, drainFoot(forecast.time_to_drain_hours) || "current backlog"),
        approvedFigure(),
        figure("Cost", forecast.usd, formatForecastUsd, [metricFoot(forecast.usd, "usd"), costedFoot()]),
      ]));
      children.push(el("div", { class: "fc-minor-row" }, [
        figure("Tokens", forecast.tokens, formatForecastTokens, metricFoot(forecast.tokens, "tokens"), { className: "fc-kpi fc-minor" }),
        figure("Effective parallelism", forecast.effective_parallelism, fmtParallelism, `of ${inputs.concurrency ?? "?"} slots, averaged over the drain`, { className: "fc-kpi fc-minor" }),
        figure("Critical path", forecast.critical_path_hours, formatForecastHours, "longest dependency chain", { className: "fc-kpi fc-minor" }),
      ]));
      const approvedInfo = proposedForecastUnavailable();
      if (approvedInfo) {
        const hasApprovedBaseline = Boolean(forecastBaselineFigures(lastPayload.if_approved?.pre_regression_baseline));
        children.push(proposedWarning(approvedInfo, hasApprovedBaseline));
      }
      const odds = drainOdds(forecast);
      if (odds) children.push(el("p", { class: "fc-line fc-odds" }, [odds]));
    }
    children.push(subsetNote(forecast));
  }
  children.push(arrivalsLine());
  return el("section", { class: "mx-card fc-answer", "aria-label": "Backlog forecast" }, children);
}

// ---- tasks ----------------------------------------------------------------

function taskCell(row, review) {
  const link = el("a", { class: "fc-task", text: row.title || "Untitled task" });
  bindTaskLink(link, row.id);
  const reviewText = review ? reviewAssumptionText(row) : null;
  const sub = [row.complexity ? COMPLEXITY_LABELS[row.complexity] || row.complexity : null, reviewText && reviewText !== "—" ? reviewText : null].filter(Boolean).join(" · ");
  return el("th", { scope: "row" }, [link, sub ? el("span", { class: "fc-sub", text: sub }) : null]);
}

function startsCell(row) {
  if (row.unforecastable) return el("td", { class: "fc-unforecastable", text: UNFORECASTABLE_LABELS[row.unforecastable] || row.unforecastable });
  if (row.status === "in-progress") return el("td", { class: "num", text: "running" });
  return el("td", { class: "num" }, [
    formatForecastQuantileRange(row.predicted_start_hours, formatForecastHours),
    el("span", { class: "fc-sub", text: `order ${formatForecastQuantileRange(row.predicted_start_rank, fmtRank)}` }),
  ]);
}

function costCell(row) {
  const tokens = quantilePair(row.tokens) ? `${formatForecastQuantileRange(row.tokens, formatForecastTokens)} tokens` : null;
  return el("td", { class: "num" }, [
    formatForecastQuantileRange(row.usd, formatForecastUsd),
    tokens ? el("span", { class: "fc-sub", text: tokens }) : null,
  ]);
}

// Only the gates that held work back; the full table is in the details.
function waitsLine() {
  const waits = lastPayload.forecast?.wait_hours;
  if (!waits) return null;
  const active = WAITS.filter(([key]) => num(waits[key]?.p50) > 0 || num(waits[key]?.p90) > 0);
  const text = active.length
    ? `Waiting: ${active.map(([key, label]) => `${label.toLowerCase()} ${formatForecastQuantileRange(waits[key], formatForecastHours)}`).join("; ")} (task-hours, p50 – p90).`
    : "Nothing waits: no gate held work back in the simulated drains.";
  return el("p", { class: "fc-line fc-waits-line", text });
}

function tasksCard() {
  const predictions = lastPayload.forecast?.task_predictions || [];
  if (!predictions.length) return null;
  const shown = predictions.slice(0, SCHEDULE_ROWS);
  const review = scheduleShowsReview(shown);
  const rows = shown.map((row) => el("tr", {}, [
    taskCell(row, review),
    el("td", { class: "fc-state" }, [statusPill(row.status)]),
    startsCell(row),
    el("td", { class: "num", text: formatForecastQuantileRange(row.work_hours, formatForecastHours) }),
    costCell(row),
    el("td", { class: "num", text: row.unforecastable ? "—" : fmtShare(row.completion_probability) }),
  ]));
  const count = predictions.length > shown.length ? `first ${shown.length} of ${predictions.length}` : `${predictions.length} task${predictions.length === 1 ? "" : "s"}`;
  const head = [["Task"], ["Status"], ["Starts in", true], ["Work hours", true], ["Cost", true], ["Finishes", true]];
  return card("Tasks", `${count} · dispatch order · p50 – p90`, [
    table("Forecast tasks", head, rows),
    waitsLine(),
  ], "fc-tasks");
}

function scheduleCaption(review) {
  const base = "In-progress tasks, then the backlog in dispatch order. Start time and order are p50 – p90 across simulated drains; work hours, cost and tokens count every attempt a task starts by the horizon; finishes is the share of runs in which the task finishes within the horizon.";
  return review ? `${base} The line under each task names its complexity and assumed review mode: a thin pool either keeps that mode or drops it.` : base;
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

function byComplexityTable() {
  const rows = (lastPayload.forecast?.by_complexity || []).map((row) => el("tr", {}, [
    el("th", { scope: "row", text: COMPLEXITY_LABELS[row.complexity] || row.complexity }),
    el("td", { class: "num", text: String(row.tasks) }),
    el("td", { class: "fc-basis", text: basisText(row.basis) }),
    el("td", { class: "num", text: formatForecastQuantileRange(row.work_hours, formatForecastHours) }),
    el("td", { class: "num", text: formatForecastQuantileRange(row.usd, formatForecastUsd) }),
    el("td", { class: "num", text: formatForecastQuantileRange(row.tokens, formatForecastTokens) }),
  ]));
  return rows.length
    ? table("Forecast by complexity", [["Complexity"], ["Tasks", true], ["Sampled from"], ["Work hours", true], ["Cost", true], ["Tokens", true]], rows, "p50 – p90 per complexity. Work hours sum every task's slot time, so they exceed the time to drain when runs overlap.")
    : el("div", { class: "mx-empty-note", text: "No backlog tasks." });
}

// ---- data gaps ------------------------------------------------------------

// Every coverage check as [key, label, count, definition]: the gaps card
// shows the non-zero ones, the method section all of them.
function coverageItems() {
  const coverage = lastPayload.coverage || {};
  const arrivals = lastPayload.arrivals?.coverage || {};
  const min = lastPayload.inputs?.min_cell_samples ?? 5;
  const thinHint = reportsReviewMode()
    ? `Fewer than ${min} deliveries of the same complexity, crew pool and review mode. The wider pool may keep the review mode (any crew of that complexity, then any delivery of that mode) or drop it. Review mode dropped counts only the tasks that lost review conditioning.`
    : `Fewer than ${min} deliveries of the same complexity and crew pool: sampled from a wider pool.`;
  // Without a complexity the first pool that keeps the review mode is every
  // delivery of that mode; an older response predates review conditioning.
  const unassessedHint = reportsReviewMode()
    ? `No complexity yet: sampled from every delivery of the task's review mode when there are at least ${min}, otherwise from every delivery, dropping the review mode.`
    : "No complexity yet: sampled from every delivery.";
  const reviewItems = reportsReviewMode() ? [
    ["review_mode_fallbacks", "Review mode dropped", coverage.review_mode_fallbacks, `The sample no longer conditions on the task's review mode. The mode-preserving pools had fewer than ${min} deliveries, including when the lookback has no history. A wider pool that still keeps the review mode is counted under thin-cell fallbacks instead.`],
    ["review_mode_unknown", "Unknown review assumption", coverage.review_mode_unknown, "The task's review mode could not be resolved, so it is not treated as a measured review policy. An unknown assumption uses the mode-pooled fallback."],
    ["history_review_mode_unknown", "History with unknown review", coverage.history_review_mode_unknown, "Deliveries in the lookback whose review evidence is missing or contradictory. They are not a measured review mode."],
  ] : [];
  const regressionItems = ["regression_history_missing", "risk_unestimated", "regression_history_thin"].some((key) => Object.prototype.hasOwnProperty.call(coverage, key)) ? [
    ["regression_history_missing", "Missing regression history", coverage.regression_history_missing, "Tasks whose regression follow-up cannot be estimated from recorded fix history. This is why the adjusted forecast may be unavailable; the pre-regression baseline excludes that follow-up work."],
    ["risk_unestimated", "Risk not estimated", coverage.risk_unestimated, "Tasks whose regression risk has no estimate."],
    ["regression_history_thin", "Thin regression history", coverage.regression_history_thin, "Tasks with some regression history, but fewer bundles than the minimum sample."],
  ] : [];
  return [
    ...regressionItems,
    ["unassessed", "Unassessed", coverage.unassessed, unassessedHint],
    ["thin_cell_fallbacks", "Thin-cell fallbacks", coverage.thin_cell_fallbacks, thinHint],
    ...reviewItems,
    ["uncovered_model", "Uncovered model", coverage.uncovered_model, "No sampled delivery has every run priced, so tokens are not forecast for these tasks."],
    ["uncosted", "Uncosted", coverage.uncosted, "No sampled delivery has every run's provider cost, so cost is not forecast for these tasks."],
    ["no_history", "No history", coverage.no_history, "Nothing to sample at all."],
    ["dependency_unforecastable", "Unforecastable dependency", coverage.dependency_unforecastable, "Depends on work the drain will not start (in review, blocked, someday, unapproved, elsewhere or missing), sits behind such work, or is on a dependency cycle. Left out of the full totals."],
    ["unverifiable_dependencies", "Unverifiable dependencies", coverage.unverifiable_dependencies, "Dependencies this workspace cannot verify. The drain treats them as satisfied, and so does the forecast."],
    ["review_lock_overlaps", "Review lock overlaps", coverage.review_lock_overlaps, "Overlaps files a task in review still locks. The forecast does not model when review releases them."],
    ["blocked", "Blocked", coverage.blocked, "Blocked now: each waits for a return drawn from how long blocked work took to come back, given how long it has waited."],
    ["outcome_fallbacks", "Attempt-outcome fallbacks", coverage.outcome_fallbacks, `Fewer than ${min} recorded attempts of the same complexity, crew pool and review mode: outcomes sampled from a wider pool.`],
    ["outcome_no_history", "No attempt history", coverage.outcome_no_history, "No recorded attempt to sample: every first attempt is assumed to deliver."],
    ["return_unsupported", "No return history", coverage.return_unsupported, "Blocked, with no recorded wait to draw a return from. Left out of the full totals."],
    ["return_tail_unsupported", "Unknown return tail", coverage.return_tail_unsupported, "Recorded waits run out before 90 days: a draw past the last one counts as no return."],
    ["phase_age_unknown", "Unknown elapsed time", coverage.phase_age_unknown, "Running or blocked, but the history does not say since when: treated as just started."],
    ["arrivals_missing_tokens", "Arrival history: missing tokens", arrivals.missing_tokens, "Deliveries missing normalized tokens in the pool used by recent arrivals."],
    ["arrivals_missing_usd", "Arrival history: missing cost", arrivals.missing_usd, "Deliveries missing provider cost in the pool used by recent arrivals."],
  ];
}

// Why an adjusted forecast is unavailable, with its evidence: the detail
// behind the answer card's one warning line. Either scenario can supply it.
function unavailableNote() {
  const forecast = lastPayload.forecast || {};
  const info = forecast.tasks ? forecastUnavailableInfo(forecast, lastPayload) : null;
  const approvedInfo = proposedForecastUnavailable();
  if (!info && !approvedInfo) return null;
  const parts = [];
  if (info) {
    parts.push(info.detail);
    if (forecastBaselineFigures(forecast.pre_regression_baseline)) {
      parts.push("The pre-regression baseline shown instead excludes unsupported regression follow-up work and is not the regression-adjusted forecast; a percentile shown as unknown was not estimated.");
    }
    if (approvedInfo) {
      const also = `If approved, the adjusted forecast is also unavailable: ${approvedInfo.label}.`;
      const approvedBaseline = forecastBaselineFigures(lastPayload.if_approved?.pre_regression_baseline);
      parts.push(approvedBaseline ? `${also} Its baseline excludes unsupported regression follow-up work.` : also);
    }
  } else {
    parts.push(`If approved, the adjusted forecast is unavailable: ${approvedInfo.label}. ${approvedInfo.detail}`);
    if (forecastBaselineFigures(lastPayload.if_approved?.pre_regression_baseline)) {
      parts.push("The baseline shown for proposed work excludes unsupported regression follow-up work and is not the regression-adjusted forecast; a percentile shown as unknown was not estimated.");
    }
  }
  const node = el("p", { class: "fc-line fc-gap-note" }, [parts.join(" ")]);
  node.dataset.forecastReason = (info || approvedInfo).kind;
  return node;
}

function gapsClearNote() {
  const forecast = lastPayload.forecast || {};
  const mainUnavailable = Boolean(forecast.tasks && forecastUnavailableInfo(forecast, lastPayload));
  if (!mainUnavailable && proposedForecastUnavailable()) {
    return "Current backlog coverage checks are clear. The proposed-work forecast is unavailable for a separate reason.";
  }
  return "No data gaps: every coverage check is clear.";
}

function gapsCard() {
  const items = coverageItems();
  const open = items.filter(([, , value]) => num(value) > 0);
  const clear = items.length - open.length;
  const meta = open.length ? `${clear} other check${clear === 1 ? "" : "s"} clear` : `all ${items.length} checks clear`;
  const tiles = open.length
    ? el("div", { class: "fc-gaps" }, open.map(([key, label, value, hint]) => el("div", { class: key === "regression_history_missing" ? "fc-gap fc-gap-key" : "fc-gap", title: hint }, [
      el("span", { class: "fc-gap-label", text: label }),
      el("span", { class: "fc-gap-count", text: String(value) }),
    ])))
    : el("div", { class: "mx-empty-note", text: gapsClearNote() });
  const node = card("Data gaps", meta, [tiles, unavailableNote()], "fc-gaps-card", "fc-gaps");
  node.tabIndex = -1;
  return node;
}

function coverageChecklist() {
  const coverage = lastPayload.coverage || {};
  const backlog = lastPayload.backlog || {};
  const arrivals = lastPayload.arrivals?.coverage || {};
  const list = el("dl", { class: "fc-coverage" }, coverageItems().map(([, label, value, hint]) => el("div", { class: "fc-cov" }, [
    el("dt", {}, [label]),
    el("dd", { class: value ? "fc-cov-count fc-warn" : "fc-cov-count", text: String(value ?? 0) }),
    el("dd", { class: "fc-cov-def", text: hint }),
  ])));
  const left = `${backlog.review ?? 0} in review are not forecast.`;
  const arrivalNote = `Recent arrivals sample all ${arrivals.deliveries ?? 0} history deliveries for both the backlog and if-approved totals. A sampled arrival missing tokens or cost makes that iteration's total unavailable for that metric; duration remains independent. Missing totals sort last, so p50 or p90 can show —. These gaps do not affect the no-arrivals forecast.`;
  return [
    el("h3", { class: "fc-sub-title", text: `Coverage checklist · ${coverage.tasks ?? 0} initial tasks forecast` }),
    list,
    el("p", { class: "mx-blurb", text: arrivalNote }),
    el("p", { class: "mx-blurb", text: left }),
  ];
}

// ---- calibration -----------------------------------------------------------

const CALIBRATION_SETTLEMENT = "Removed or re-scoped tasks, and done tasks with no agent runs after the forecast, are left out and counted. Rejected and archived tasks are also excluded under model versions before 5; from version 5 they count as abandoned, with their post-forecast cost and token usage retained in actual totals. Before model version 9, a forecast is scored as a whole once no tasks are pending, at least one task is done or abandoned, and none was left out. From model version 9 onward, whole-forecast scoring stays provisional until 90 days after the forecast, even if its initial tasks finish earlier, so regression follow-ups observed during that window can be counted. At the cutoff, the cohort settles regardless of pending tasks and its whole actual is scored if no tasks were left out; unfinished work has no time-to-drain actual. Per-task hits can accrue before then. A settled cohort with an abandoned task is scored as not drained and has no time-to-drain actual.";

// A share of actuals under a percentile against its target: within five
// points reads as calibrated, anything else as a miss.
function calibrationBar(label, share, target) {
  const value = num(share);
  const miss = value != null && Math.abs(value - target) > 0.05;
  const width = value == null ? 0 : Math.max(0, Math.min(1, value)) * 100;
  const text = value == null ? "—" : fmtShare(value);
  return el("div", { class: miss ? "fc-cal-row fc-miss" : "fc-cal-row" }, [
    el("span", { class: "fc-cal-label", text: label }),
    el("span", { class: "fc-cal-track", role: "img", "aria-label": `${label}: ${text}, target ${Math.round(target * 100)}%` }, [
      el("span", { class: "fc-cal-fill", style: { width: `${width}%` } }),
      el("span", { class: "fc-cal-target", style: { left: `${target * 100}%` } }),
    ]),
    el("span", { class: "fc-cal-value", text }),
  ]);
}

function calibrationMeasure(label, hits) {
  return el("div", { class: "fc-cal", role: "group", "aria-label": label }, [
    el("div", { class: "fc-cal-head" }, [
      el("span", { text: label }),
      el("span", { class: "fc-cal-n", text: `${hits?.n ?? 0} scored` }),
    ]),
    calibrationBar("p50", hits?.under_p50, 0.5),
    calibrationBar("p90", hits?.under_p90, 0.9),
  ]);
}

function settlementLine(calibration) {
  const level = calibration.forecast_level || {};
  const drain = level.time_to_drain_hours || {};
  const parts = [];
  if (drain.n) {
    parts.push(`Whole-backlog totals: ${drain.n} scored, ${fmtShare(drain.under_p50)} under p50 · ${fmtShare(drain.under_p90)} under p90 on time to drain`);
  } else {
    parts.push("Whole-backlog totals (drain time, cost, tokens) settle under each model's rule, up to 90 days after the forecast — none scored yet");
  }
  if (num(level.settled)) parts.push(`${level.settled} settled${num(level.not_drained) ? `, ${level.not_drained} not drained` : ""}`);
  const current = calibration.model_version;
  const previous = (calibration.by_model_version || [])
    .filter((row) => row.model_version !== current && row.task_level?.hours?.n)
    .sort((a, b) => b.model_version - a.model_version)[0];
  if (previous) {
    const hits = previous.task_level.hours;
    parts.push(`v${previous.model_version} scored ${fmtShare(hits.under_p50)} · ${fmtShare(hits.under_p90)} on task hours (${hits.n})`);
  }
  return el("p", { class: "fc-line fc-settlement", text: `${parts.join(". ")}.` });
}

function calibrationCard() {
  const calibration = lastPayload.calibration || {};
  const current = calibration.model_version;
  const meta = `${current != null ? `model v${current} · ` : ""}target 50% under p50, 90% under p90`;
  if (!(calibration.forecasts || []).length) {
    return card("Calibration", meta, [el("div", { class: "mx-empty" }, [
      el("div", { class: "mx-empty-title", text: "No stored forecasts to score yet" }),
      el("div", { class: "mx-empty-detail", text: "Calibration appears once a stored forecast's tasks start finishing." }),
    ])]);
  }
  const taskLevel = calibration.task_level || {};
  return card("Calibration", meta, [
    el("div", { class: "fc-cals" }, [
      calibrationMeasure("Task hours", taskLevel.hours),
      calibrationMeasure("Task cost", taskLevel.usd),
      calibrationMeasure("Task tokens", taskLevel.tokens),
    ]),
    settlementLine(calibration),
  ], "fc-calibration");
}

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

// ---- collapsed detail -----------------------------------------------------

function moreSection(key, title, meta, children) {
  const panel = detailsPanel(`forecast:${key}`, { class: "fc-more" });
  panel.append(el("summary", { class: "fc-more-summary" }, [
    el("span", { class: "fc-more-title", text: title }),
    meta ? el("span", { class: "fc-more-meta", text: meta }) : null,
  ]));
  panel.append(el("div", { class: "fc-more-body" }, children));
  return panel;
}

function storedSection() {
  const calibration = lastPayload.calibration || {};
  const stored = lastPayload.stored || {};
  const forecasts = calibration.forecasts || [];
  const served = lastPayload.served_from === "stored";
  const latest = stored.latest_at ? `latest stored ${formatDateTime(stored.latest_at)}` : "none stored yet";
  const meta = served ? `${latest} · scored when stored` : latest;
  const blurb = el("p", { class: "mx-blurb", text: `${meta}. The default forecast is stored at most once every ${stored.min_interval_minutes ?? 60} minutes. Per-task calibration can score as tasks finish; whole-forecast totals follow each model's settlement rule. The default view shows that stored forecast with the calibration scored when it was stored; Refresh scores it now. A well-calibrated forecast has about 50% of actuals under p50 and 90% under p90.` });
  if (!forecasts.length) return moreSection("stored", "Stored forecasts", "none yet", [blurb]);
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
  const pageCount = Math.ceil(forecasts.length / CALIBRATION_HISTORY_PAGE_SIZE);
  calibrationHistoryPage = Math.min(calibrationHistoryPage, pageCount - 1);
  const firstRow = calibrationHistoryPage * CALIBRATION_HISTORY_PAGE_SIZE;
  const lastRow = Math.min(firstRow + CALIBRATION_HISTORY_PAGE_SIZE, forecasts.length);
  const rows = forecasts.slice(firstRow, lastRow).map((row) => el("tr", {}, [
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
  const history = table("Stored forecasts", [["Recorded"], ["Model", true], ["Done", true], ["Left out"], ["Drain p50 – p90 → actual", true], ["Cost", true], ["Tokens", true], ["State"]], rows, "Stored forecasts, newest first");
  history.classList.add("fc-history-table-wrap");
  const newer = el("button", { class: "mx-seg-btn", type: "button", text: "Newer forecasts" });
  newer.disabled = calibrationHistoryPage === 0;
  newer.addEventListener("click", () => {
    calibrationHistoryPage = Math.max(0, calibrationHistoryPage - 1);
    renderForecast();
  });
  const olderButton = el("button", { class: "mx-seg-btn", type: "button", text: "Older forecasts" });
  olderButton.disabled = calibrationHistoryPage >= pageCount - 1;
  olderButton.addEventListener("click", () => {
    calibrationHistoryPage = Math.min(pageCount - 1, calibrationHistoryPage + 1);
    renderForecast();
  });
  const pagination = pageCount > 1 ? el("nav", { class: "fc-history-pagination", "aria-label": "Stored forecast history pages" }, [
    el("span", { class: "fc-history-page-status", role: "status", "aria-live": "polite", text: `Showing ${firstRow + 1}–${lastRow} of ${forecasts.length} stored forecasts · Page ${calibrationHistoryPage + 1} of ${pageCount} · newest first` }),
    el("div", { class: "mx-seg", role: "group", "aria-label": "Stored forecast history" }, [newer, olderButton]),
  ]) : null;
  return moreSection("stored", "Stored forecasts", String(forecasts.length), [
    blurb,
    summary,
    byVersion,
    el("p", { class: "mx-blurb", text: `Left out across stored forecasts: ${excludedText(excluded)}.` }),
    history,
    pagination,
  ]);
}

function waitsSection() {
  const waits = lastPayload.forecast?.wait_hours;
  if (!waits || !lastPayload.forecast?.tasks) return null;
  const rows = WAITS.map(([key, label, hint]) => el("tr", {}, [
    el("th", { scope: "row" }, [label, " ", infoHint(hint)]),
    el("td", { class: "num", text: formatForecastQuantileRange(waits[key], formatForecastHours) }),
  ]));
  const caption = "Summed over every waiting task, so hours add up across tasks and exceed the time to drain. A task counts under the first gate it fails.";
  const head = [["Held back by"], ["Waiting", true]];
  return moreSection("waits", "Wait breakdown by gate", "task-hours, p50 – p90", [
    el("div", { class: "fc-waits" }, [
      el("p", { class: "mx-blurb fc-waits-caption", text: caption }),
      el("div", { class: "fc-waits-grid" }, [
        table("First four waiting reasons", head, rows.slice(0, 4)),
        table("Last three waiting reasons", head, rows.slice(4)),
      ]),
    ]),
  ]);
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

// The sample the forecast drew from: n deliveries, and how far back they reach.
function historyText(inputs) {
  const n = `n=${inputs.history_deliveries ?? 0} deliveries`;
  const since = inputs.lookback_start ? ` since ${formatDateTime(inputs.lookback_start)}` : "";
  if (inputs.history_tasks != null) return `${n}${since} (the newest ${inputs.history_tasks} completions)`;
  return `${n}${since} (${inputs.lookback_days ?? "?"} days, at least the newest ${inputs.min_history_tasks ?? 50} completions)`;
}

const ADMISSION_TEXT = "Each simulated drain admits work as the drain does: in dispatch order, after its dependencies finish, never beside a task whose files overlap, an editing task without context files alone, and behind surfaces reserved by higher-priority work. Each attempt first draws a recorded attempt of the same pool: one that blocked, failed or was abandoned frees its slot when that attempt did, then requeues, waits for a return drawn from how long blocked work took to come back (or never does), or stops. Blocked tasks start out waiting for that return. Review and someday tasks are left out, and so is work that depends on them; in-progress tasks count only their remaining work. Review locks, static eligibility and arrivals' files are not modelled yet.";

function methodSection() {
  const inputs = lastPayload.inputs || {};
  const backlog = lastPayload.backlog || {};
  const meta = `${backlog.backlog ?? 0} backlog · ${backlog.in_progress ?? 0} in progress · ${backlog.blocked ?? 0} blocked · ${historyText(inputs)}`;
  const predictions = lastPayload.forecast?.task_predictions || [];
  return moreSection("method", "How this forecast is made", null, [
    el("p", { class: "mx-blurb" }, [
      `${meta}. Simulated ${inputs.iterations ?? 0} times on ${capacityText(inputs)}.`,
      lastPayload.model_version != null ? ` Model v${lastPayload.model_version}.` : null,
    ]),
    el("h3", { class: "fc-sub-title", text: "Sampling and admission" }),
    el("p", { class: "mx-blurb", text: samplingExplanation(inputs) }),
    el("p", { class: "mx-blurb", text: ADMISSION_TEXT }),
    predictions.length ? el("p", { class: "mx-blurb", text: scheduleCaption(scheduleShowsReview(predictions.slice(0, SCHEDULE_ROWS))) }) : null,
    el("h3", { class: "fc-sub-title", text: "By complexity" }),
    byComplexityTable(),
    ...coverageChecklist(),
    el("h3", { class: "fc-sub-title", text: "Calibration settlement" }),
    el("p", { class: "mx-blurb", text: CALIBRATION_SETTLEMENT }),
  ]);
}

function detailSections() {
  return el("div", { class: "fc-more-list" }, [storedSection(), waitsSection(), methodSection()]);
}
