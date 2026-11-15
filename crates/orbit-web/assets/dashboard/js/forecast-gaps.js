// Analytics › Forecast's Data gaps tab: one sentence on where the figures stand,
// the coverage checks that are not zero, grouped into this backlog and its
// history, and every check with its definition behind Details.

import {
  el, forecastView, forecastCard, pinnedInfoHint, forecastMoreSection, reportsReviewMode,
} from './forecast-shared.js';
import {
  forecastBaselineFigures, forecastCount, forecastUnavailableInfo, num, recordedPoolsContainGaps,
  regressionGapReasonPhrase, regressionHistoryIsAbsent,
} from './forecast-summary.js';
import * as forecastSummary from './forecast-summary.js';

// Checks about delivery history; every other check counts tasks in this backlog.
const HISTORY_CHECK_KEYS = new Set([
  "history_review_mode_unknown", "review_checkpoint_unreadable", "arrivals_missing_tokens", "arrivals_missing_usd",
]);

// The if-approved totals and their qualifications cover current plus proposed
// tasks, even when all proposed additions have measured history.
function proposedForecastUnavailable() {
  const approved = forecastView.payload.if_approved;
  if (!approved || !forecastCount(forecastView.payload.backlog?.proposed)) return null;
  const coverage = forecastSummary.proposedForecastCoverage(forecastView.payload);
  return forecastUnavailableInfo(approved, { coverage });
}

// Every coverage check as [key, label, count, definition]: the groups show the
// non-zero ones, the Details checklist all of them.
function coverageItems() {
  const coverage = forecastView.payload.coverage || {};
  const arrivals = forecastView.payload.arrivals?.coverage || {};
  const min = forecastView.payload.inputs?.min_cell_samples ?? 5;
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
    ["history_review_mode_unknown", "History with unknown review", coverage.history_review_mode_unknown, "Deliveries in the lookback whose review evidence is missing or contradictory. They are not a measured review mode. A checkpoint that could not be read is counted separately and is not treated as missing."],
  ] : [];
  const unreadableItems = Object.prototype.hasOwnProperty.call(coverage, "review_checkpoint_unreadable") ? [
    ["review_checkpoint_unreadable", "Unreadable review checkpoint", coverage.review_checkpoint_unreadable, "Historical runs whose review checkpoint could not be read. Their review evidence is Unknown. The forecast keeps the deliveries it could read and does not treat the gap as no review, a verified exemption, a completed review, or zero cost."],
  ] : [];
  const poolsHaveGaps = recordedPoolsContainGaps(forecastView.payload.forecast);
  const historyAbsent = regressionHistoryIsAbsent(forecastView.payload.forecast);
  const namedGaps = regressionGapReasonPhrase(forecastView.payload.forecast);
  const historyLabel = poolsHaveGaps
    ? "Recorded pools contain gaps"
    : historyAbsent ? "Missing regression history" : "Regression follow-up unestimated";
  const historyHint = poolsHaveGaps
    ? `Regression follow-up cannot be estimated because ${namedGaps ? `recorded pools contain gaps (${namedGaps})` : "recorded pools contain gaps"}. The pre-regression baseline excludes that follow-up work.`
    : historyAbsent
      ? "Tasks whose pool has no regression history, so regression follow-up cannot be estimated. The pre-regression baseline excludes that follow-up work."
      : "Tasks whose regression follow-up cannot be estimated from recorded fix history. This is why the adjusted forecast may be unavailable; the pre-regression baseline excludes that follow-up work.";
  const regressionItems = ["regression_history_missing", "risk_unestimated", "regression_history_thin"].some((key) => Object.prototype.hasOwnProperty.call(coverage, key)) ? [
    ["regression_history_missing", historyLabel, coverage.regression_history_missing, historyHint],
    ["risk_unestimated", "Risk not estimated", coverage.risk_unestimated, "Tasks whose regression risk has no estimate."],
    ["regression_history_thin", "Thin regression history", coverage.regression_history_thin, "Tasks with some regression history, but fewer bundles than the minimum sample."],
  ] : [];
  return [
    ...regressionItems,
    ["unassessed", "Unassessed", coverage.unassessed, unassessedHint],
    ["thin_cell_fallbacks", "Thin-cell fallbacks", coverage.thin_cell_fallbacks, thinHint],
    ...reviewItems,
    ...unreadableItems,
    ["uncovered_model", "Uncovered model", coverage.uncovered_model, "No sampled delivery has every run priced, so tokens are not forecast for these tasks."],
    ["uncosted", "Uncosted", coverage.uncosted, "No sampled delivery has every run's provider cost, so cost is not forecast for these tasks."],
    ...(["thin_tokens", "thin_cost"].some((key) => Object.prototype.hasOwnProperty.call(coverage, key)) ? [
      ["thin_tokens", "Thin token support", coverage.thin_tokens, `Some but fewer than ${forecastView.payload.inputs?.min_metric_samples ?? min} measured token samples in the sampled pool: the token range rests on very few measured deliveries (one sample gives the same p50 and p90). Counted separately from the time-sample count.`],
      ["thin_cost", "Thin cost support", coverage.thin_cost, `Some but fewer than ${forecastView.payload.inputs?.min_metric_samples ?? min} measured provider-cost samples in the sampled pool: the cost range rests on very few measured deliveries (one sample gives the same p50 and p90). Counted separately from the time-sample count.`],
    ] : []),
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
// behind the Forecast tab's one status line. Either scenario can supply it.
function unavailableNote() {
  const forecast = forecastView.payload.forecast || {};
  const info = forecast.tasks ? forecastUnavailableInfo(forecast, forecastView.payload) : null;
  const approvedInfo = proposedForecastUnavailable();
  if (!info && !approvedInfo) return null;
  const parts = [];
  if (info) {
    parts.push(info.detail);
    if (forecastBaselineFigures(forecast.pre_regression_baseline)) {
      parts.push("The pre-regression baseline shown instead excludes unsupported regression follow-up work and is not the regression-adjusted forecast; a percentile shown as unknown was not estimated.");
    }
    if (approvedInfo) {
      const also = [`If approved, the adjusted forecast is also unavailable: ${approvedInfo.label}.`, approvedInfo.support].filter(Boolean).join(" ");
      const approvedBaseline = forecastBaselineFigures(forecastView.payload.if_approved?.pre_regression_baseline);
      parts.push(approvedBaseline ? `${also} Its baseline excludes unsupported regression follow-up work.` : also);
    }
  } else {
    parts.push(`If approved, the adjusted forecast is unavailable: ${approvedInfo.label}. ${approvedInfo.detail}`);
    if (forecastBaselineFigures(forecastView.payload.if_approved?.pre_regression_baseline)) {
      parts.push("The baseline shown for proposed work excludes unsupported regression follow-up work and is not the regression-adjusted forecast; a percentile shown as unknown was not estimated.");
    }
  }
  const node = el("p", { class: "fc-line fc-gap-note" }, [parts.join(" ")]);
  node.dataset.forecastReason = (info || approvedInfo).kind;
  return node;
}

function gapsClearNote() {
  const forecast = forecastView.payload.forecast || {};
  const mainUnavailable = Boolean(forecast.tasks && forecastUnavailableInfo(forecast, forecastView.payload));
  if (!mainUnavailable && proposedForecastUnavailable()) {
    return "Current backlog coverage checks are clear. The proposed-work forecast is unavailable for a separate reason.";
  }
  return "No data gaps: every coverage check is clear.";
}

function partialHistoryNote() {
  const coverage = forecastView.payload.coverage || {};
  const listed = Array.isArray(forecastView.payload.inputs?.review_evidence_gaps)
    ? forecastView.payload.inputs.review_evidence_gaps
    : [];
  const counted = Object.prototype.hasOwnProperty.call(coverage, "review_checkpoint_unreadable")
    ? num(coverage.review_checkpoint_unreadable)
    : 0;
  if (!listed.length && !(counted > 0)) return null;
  const count = listed.length || counted;
  const noun = count === 1 ? "checkpoint" : "checkpoints";
  const lines = [
    `Partial review history. ${count} historical ${noun} could not be read, so that review evidence is Unknown. The forecast still uses the delivery history that could be read, and does not treat the gap as no review, a verified exemption, a completed review, or zero cost.`,
  ];
  for (const gap of listed) {
    const run = gap && gap.run_id ? gap.run_id : "unknown run";
    const kind = gap && gap.coverage_gap ? gap.coverage_gap : "review_checkpoint_unreadable";
    const reason = gap && gap.reason ? gap.reason : "unreadable checkpoint";
    lines.push(`${run} (${kind}): ${reason}`);
  }
  return el("p", { class: "fc-line fc-gap-note fc-partial-history" }, [lines.join(" ")]);
}

function openCoverageItems() {
  return coverageItems().filter(([, , value]) => num(value) > 0);
}

/// How many coverage checks are not zero: the Data gaps tab's badge.
export function openCoverageCount() {
  return openCoverageItems().length;
}

/// The Data gaps tab's header line.
export function otherChecksClearText() {
  const total = coverageItems().length;
  const open = openCoverageCount();
  const clear = total - open;
  return open ? `${clear} other check${clear === 1 ? "" : "s"} clear` : `All ${total} checks clear`;
}

// One sentence: where the figures stand and why, from the live reasons.
function gapsSentence() {
  const forecast = forecastView.payload.forecast || {};
  const info = forecast.tasks ? forecastUnavailableInfo(forecast, forecastView.payload) : null;
  if (info) {
    return forecastBaselineFigures(forecast.pre_regression_baseline)
      ? `${info.label}, so totals show the baseline only.`
      : `${info.label}, so there is no forecast.`;
  }
  const open = openCoverageCount();
  const approved = proposedForecastUnavailable();
  if (approved) return `The current backlog's totals stand. With proposed work approved: ${approved.label}.`;
  if (!open) return gapsClearNote();
  return `The totals stand, with ${open} coverage check${open === 1 ? "" : "s"} reporting gaps.`;
}

// One card of open gaps; a group with none renders nothing.
function gapGroup(title, items) {
  if (!items.length) return null;
  const rows = [...items]
    .sort((a, b) => num(b[2]) - num(a[2]))
    .map(([key, label, value, hint]) => el("div", { class: key === "regression_history_missing" ? "fc-gap fc-gap-key" : "fc-gap" }, [
      el("dt", {}, [label, pinnedInfoHint(hint)]),
      el("dd", { class: "fc-gap-count", text: String(value) }),
    ]));
  return forecastCard(title, null, [el("dl", { class: "fc-gaps" }, rows)], "fc-gap-group");
}

/// The Data gaps tab.
export function dataGapsPanelNodes() {
  const open = openCoverageItems();
  return [
    el("p", { class: "fc-gaps-sentence", text: gapsSentence() }),
    open.length ? el("div", { class: "fc-gap-groups" }, [
      gapGroup("This backlog", open.filter(([key]) => !HISTORY_CHECK_KEYS.has(key))),
      gapGroup("History", open.filter(([key]) => HISTORY_CHECK_KEYS.has(key))),
    ]) : null,
    el("div", { class: "fc-more-list" }, [
      forecastMoreSection("gaps-details", "Details", null, [unavailableNote(), partialHistoryNote(), ...coverageChecklist()]),
    ]),
  ];
}

function coverageChecklist() {
  const coverage = forecastView.payload.coverage || {};
  const backlog = forecastView.payload.backlog || {};
  const arrivals = forecastView.payload.arrivals?.coverage || {};
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
