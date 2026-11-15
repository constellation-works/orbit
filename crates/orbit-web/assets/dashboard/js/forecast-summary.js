// Backlog forecast figures shared by Analytics › Forecast and the Tasks drain
// card: formatting, why an adjusted forecast is unavailable, the compact
// summary and its link. Nothing here reads the page's selected view.

import { el, scopedHashHref, relativeTime } from './common.js';

export const UNREPORTED_FORECAST_REASON = "Forecast unavailable, reason not reported";

// ---- formatting -----------------------------------------------------------

export function num(value) {
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

export function fmtRate(value) {
  const n = num(value);
  return n == null ? "—" : `${n >= 10 ? Math.round(n) : n.toFixed(1)}/day`;
}

export function fmtParallelism(value) {
  const n = num(value);
  return n == null ? "—" : `${n.toFixed(1)}×`;
}

export function fmtRank(value) {
  const n = num(value);
  return n == null ? "—" : `#${Math.round(n)}`;
}

export function fmtShare(value) {
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

export function quantilePair(quantiles) {
  return num(quantiles?.p50) != null || num(quantiles?.p90) != null;
}

export function forecastCount(value) {
  const n = num(value);
  return n == null || n < 0 ? 0 : Math.trunc(n);
}

// The if-approved forecast includes the disjoint current and proposed tasks.
// Combine the task counters used by totals and unavailable qualifications;
// history-level counters describe a shared pool and must not be added.
// A missing counter in either population stays unknown, never a measured zero.
export function proposedForecastCoverage(payload) {
  const keys = ["tasks", "uncosted", "uncovered_model", "no_history", "regression_history_missing", "risk_unestimated", "regression_history_thin"];
  return Object.fromEntries(keys.map((key) => {
    const current = payload.coverage?.[key];
    const proposed = payload.proposed_coverage?.[key];
    const known = [current, proposed].every((count) => Number.isSafeInteger(count) && count >= 0);
    return [key, known ? current + proposed : null];
  }));
}

function backlogForecastFreshness(payload) {
  if (payload.served_from === "live") return "Live";
  if (payload.served_from === "stored") {
    const age = relativeTime(payload.generated_at);
    return age ? `Stored ${age}` : "Stored time unavailable";
  }
  return "Forecast freshness unavailable";
}

// Reasons already counted on `unavailable.regression_history_gaps`. A zero
// count is not a reason to name. Order follows the forecast response.
const REGRESSION_GAP_REASON_ORDER = ["unmeasured_delivery", "footprint_unrecoverable", "origin_completion_missing"];
const REGRESSION_GAP_REASON_LABELS = {
  unmeasured_delivery: "unmeasured delivery",
  footprint_unrecoverable: "unrecoverable footprint",
  origin_completion_missing: "missing origin completion",
};

function regressionGapReasonLabel(key) {
  return REGRESSION_GAP_REASON_LABELS[key] || String(key).split("_").join(" ");
}

function taskPredictions(forecast) {
  return Array.isArray(forecast?.task_predictions) ? forecast.task_predictions : [];
}

function regressionGapReasons(forecast) {
  const gaps = forecast?.unavailable?.regression_history_gaps;
  if (!gaps || typeof gaps !== "object" || Array.isArray(gaps)) return [];
  const preferred = REGRESSION_GAP_REASON_ORDER.filter((key) => Object.prototype.hasOwnProperty.call(gaps, key));
  const rest = Object.keys(gaps).filter((key) => !REGRESSION_GAP_REASON_ORDER.includes(key));
  return [...preferred, ...rest].flatMap((key) => {
    const count = forecastCount(gaps[key]);
    return count > 0 ? [{ key, count, label: regressionGapReasonLabel(key) }] : [];
  });
}

export function regressionGapReasonPhrase(forecast) {
  return regressionGapReasons(forecast).map((reason) => `${reason.label} ${reason.count}`).join(", ");
}

export function regressionGapBecause(forecast) {
  const named = regressionGapReasonPhrase(forecast);
  return named ? `recorded pools contain gaps (${named})` : "recorded pools contain gaps";
}

// A recorded pool contains a gap when the scenario names a positive gap
// reason, or a counted task still has bundle samples and gaps in its pool.
export function recordedPoolsContainGaps(forecast) {
  if (regressionGapReasons(forecast).length) return true;
  return taskPredictions(forecast).some((row) => forecastCount(row?.regression_bundle_samples) > 0 && forecastCount(row?.regression_history_gaps) > 0);
}

// A present zero. A missing field is unknown, not an empty pool.
function regressionSampleCount(row) {
  if (!row || !Object.prototype.hasOwnProperty.call(row, "regression_bundle_samples")) return null;
  return forecastCount(row.regression_bundle_samples);
}

// History is absent only for an empty pool: no bundles, or every counted
// task at zero samples. A nonempty pool that holds a gap is not absent.
export function regressionHistoryIsAbsent(forecast) {
  if (recordedPoolsContainGaps(forecast)) return false;
  const bundles = num(forecast?.regression_rework?.support?.history_bundles);
  if (bundles === 0) return true;
  const samples = taskPredictions(forecast).map(regressionSampleCount);
  return samples.length > 0 && samples.every((count) => count === 0);
}

function withheldTaskPhrase(forecast, count) {
  const tasks = `${count} task${count === 1 ? "" : "s"}`;
  if (recordedPoolsContainGaps(forecast)) return `${tasks} withheld because recorded pools contain gaps`;
  if (regressionHistoryIsAbsent(forecast)) return `${tasks} missing regression history`;
  return `${tasks} whose regression follow-up cannot be estimated`;
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
  if (num(coverage.regression_history_missing)) parts.push(withheldTaskPhrase(forecast, forecastCount(coverage.regression_history_missing)));
  if (num(coverage.risk_unestimated)) parts.push(`${forecastCount(coverage.risk_unestimated)} tasks with unestimated risk`);
  if (num(coverage.regression_history_thin)) parts.push(`${forecastCount(coverage.regression_history_thin)} tasks with thin regression history`);
  return parts.length ? `Available evidence: ${parts.join(" · ")}.` : "";
}

// A scenario with arrivals stopped at a simulation bound. Its partial runs are
// withheld, so this names the bound rather than any figure.
function simulationCutReason(cut) {
  const detail = {
    generated_limit: `A simulated run filed ${(cut?.generated_limit ?? 10000).toLocaleString("en-US")} new tasks at the recent rate without the backlog ever emptying, so there is no drain time to report.`,
    event_limit: "A simulated run reached the limit on scheduling events before it finished, so its totals are withheld.",
  }[cut?.reason] ?? `The simulation stopped at its ${cut?.compute_budget_seconds ?? 20}-second limit before finishing every run, so partial totals are withheld.`;
  return { kind: "simulation_cut", label: "With arrivals simulation cut short", detail };
}

/// Reasons the adjusted forecast is unavailable, from `forecast.unavailable`.
/// Null when no flag is set (older payloads omit the object entirely).
export function forecastUnavailableInfo(forecast, payload = {}) {
  const unavailable = forecast?.unavailable;
  if (!unavailable || typeof unavailable !== "object") return null;
  const reasons = [];
  if (unavailable.simulation_cut) reasons.push(simulationCutReason(forecast.simulation_cut));
  if (unavailable.regression_history_missing) {
    const named = regressionGapReasonPhrase(forecast);
    let label = "Missing regression history";
    let detail = "Regression follow-up work cannot be estimated for some tasks, so the regression-adjusted forecast is unavailable.";
    if (recordedPoolsContainGaps(forecast)) {
      label = named ? `Recorded pools contain gaps (${named})` : "Recorded pools contain gaps";
      detail = `Regression follow-up cannot be estimated because ${regressionGapBecause(forecast)}, so the regression-adjusted forecast is unavailable.`;
    } else if (!regressionHistoryIsAbsent(forecast)) {
      label = "Regression follow-up unestimated";
      detail = "Regression follow-up cannot be estimated for some tasks, so the regression-adjusted forecast is unavailable.";
    }
    reasons.push({ kind: "regression_history_missing", label, detail });
  }
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

export function hasHorizonCensoringEvidence(forecast) {
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

// Scope holds are estimates of future/manual work, outside this drain's clock.
export function forecastDrainScope(source) {
  const held = (source?.task_predictions || []).filter((row) => row.drain_hold);
  const scheduled = held.filter((row) => row.drain_hold.kind === "scheduled");
  const excluded = held.filter((row) => row.drain_hold.kind === "excluded");
  const next = scheduled.map((row) => row.drain_hold.starts_at).filter(Boolean).sort()[0];
  const reasons = [...new Set(excluded.map((row) => row.drain_hold.reason === "no_drain_tag" ? "no-drain" : row.drain_hold.reason).filter(Boolean))];
  const compact = [
    scheduled.length ? `${scheduled.length} scheduled${next ? ` · starts ${next.slice(0, 10)}` : ""}` : null,
    excluded.length ? `${excluded.length} excluded${reasons.length ? ` · ${reasons.join(", ")}` : ""}` : null,
  ].filter(Boolean).join(" · ");
  const lines = held.map((row) => `${row.drain_hold.kind === "scheduled" ? "Scheduled" : "Excluded"}: ${row.title || "Untitled task"} — ${row.drain_hold.detail || row.drain_hold.reason}`);
  return { compact, lines };
}

/// Compact drain-card figures. When the adjusted forecast is unavailable this
/// still returns a summary carrying the reason (and baseline, when present),
/// so the drain card can explain itself instead of a generic line. Null only
/// for a non-object payload.
export function backlogForecastSummary(payload) {
  if (!payload || typeof payload !== "object") return null;
  const forecast = payload.forecast || {};
  const backlog = payload.backlog || {};
  const scope = forecastDrainScope(forecast);
  const counts = `${forecastCount(backlog.backlog)} backlog · ${forecastCount(backlog.in_progress)} in progress`;
  const freshness = backlogForecastFreshness(payload);
  const info = forecastUnavailableInfo(forecast, payload);
  if (info) {
    const figures = forecastBaselineFigures(forecast.pre_regression_baseline);
    return { unavailable: true, reasonKind: info.kind, reason: info.label, detail: info.detail, reasons: info.reasons, baseline: figures, baselineText: baselineText(figures), baselineBars: figures ? backlogBarQuantiles(forecast.pre_regression_baseline) : null, scope, counts, freshness, drain: null, cost: null, exactCost: null, tokens: null };
  }
  if (num(forecast.tasks) === 0) {
    return { unavailable: true, reasonKind: "empty", reason: "No backlog to forecast", detail: "No backlog or in-progress tasks in this workspace.", baseline: null, baselineText: "", scope, counts, freshness, drain: null, cost: null, exactCost: null, tokens: null };
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
      scope, counts, freshness,
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
  return { unavailable: true, reasonKind, reason, detail, baseline: figures, baselineText: baselineText(figures), baselineBars: figures ? backlogBarQuantiles(forecast.pre_regression_baseline) : null, scope, counts, freshness, drain: null, cost: null, exactCost: null, tokens: null };
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

// Share of the line the largest figure may reach, so a p90 never reads as a
// value pinned to a limit when there is no limit.
const BACKLOG_BAR_SPAN = 0.9;

// One row beside the CPU and Memory meters: label, line and figures. The
// distribution sits on a flat line: a dot at p50, with colour strongest there
// and fading outward to p90. The forecast has no p10, so the fade to the left
// of p50 mirrors the right half as a visual falloff and is not a figure. Each
// line is scaled to its own largest figure (its p90, or the tick when that is
// further), since the three metrics share no unit. A missing quantile reads
// "unknown" and draws nothing in its place: no p50 draws no row mark, and no
// p90 leaves the dot without a spread.
function backlogBarRow([key, label, , fmt], quantiles, reference, caveat) {
  const { p50, p90 } = quantiles;
  const top = Math.max(p50 ?? 0, p90 ?? 0, reference ?? 0);
  const at = (value) => (top > 0 ? Math.max(0, value) / top * BACKLOG_BAR_SPAN * 100 : 0);
  const share = (value) => `${at(value).toFixed(2)}%`;
  const figures = `${p50 == null ? "unknown" : fmt(p50)} – ${p90 == null ? "unknown" : fmt(p90)}`;
  let band = null;
  if (p50 != null && p90 != null && p90 > p50) {
    const start = Math.max(0, p50 - (p90 - p50));
    band = el("span", { class: "drain-backlog-forecast-band", style: { left: share(start), width: share(p90 - start) } });
    band.style.setProperty("--center", `${((p50 - start) / (p90 - start) * 100).toFixed(2)}%`);
  }
  const meter = el("span", { class: "drain-backlog-forecast-meter", "aria-hidden": "true" }, [
    band,
    reference != null ? el("span", { class: "drain-forecast-tick", style: { left: share(reference) } }) : null,
    p50 != null ? el("span", { class: "drain-backlog-forecast-p50", style: { left: share(p50) } }) : null,
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
/// rows (p50 marked on a flat line, shading out to p90, figures beside), with a
/// freshness label for all the snapshot's figures. Counts and exact cost are
/// in its tooltip and accessible text. When the adjusted forecast is
/// unavailable the pre-regression baseline rows are muted without a visible
/// label, or there are no rows; the reason and the baseline's caveat stay in
/// the tooltip and accessible text [ORB-20453].
export function drainBacklogForecastLink(summary) {
  const unavailable = !summary || Boolean(summary.unavailable);
  const link = el("a", { class: unavailable ? "drain-backlog-forecast unavailable" : "drain-backlog-forecast" });
  link.href = scopedHashHref("#analytics/forecast");
  if (unavailable) {
    const reason = summary?.reason || "Backlog forecast unavailable";
    const detail = [reason, summary?.detail, summary?.baselineText, summary?.counts, ...(summary?.scope?.lines || [])].filter(Boolean).join(" ");
    link.title = `${detail}\nAnalytics › Forecast`;
    if (summary) link.dataset.forecastReason = summary.reasonKind || "unknown";
    const bars = backlogBarList(summary?.baselineBars, null, "pre-regression baseline");
    if (bars) bars.classList.add("baseline");
    for (const child of [
      bars ? null : el("span", { class: "drain-section-label", text: "Backlog" }),
      bars,
      el("span", { class: "visually-hidden", text: ` ${detail}` }),
      bars ? null : el("span", { class: "drain-backlog-forecast-go", text: "›", "aria-hidden": "true" }),
    ]) if (child) link.append(child);
    if (summary?.scope?.compact) link.append(el("span", { class: "drain-forecast-meta drain-backlog-forecast-scope", text: summary.scope.compact }));
    return link;
  }
  const detail = [
    summary.freshness, summary.counts, ...(summary.scope?.lines || []),
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
    summary.scope?.compact ? el("span", { class: "drain-forecast-meta drain-backlog-forecast-scope", text: summary.scope.compact }) : null,
    summary.references ? el("span", { class: "drain-backlog-forecast-legend", text: "pre-regression baseline p50" }) : null,
    el("span", { class: "visually-hidden", text: ` p50 – p90, ${accessibleDetail}` }),
    el("span", { class: "drain-backlog-forecast-go", text: "›", "aria-hidden": "true" }),
  ].filter(Boolean));
  return link;
}
