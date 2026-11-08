// Analytics › Forecast's Accuracy tab: one calibration table, the residual
// charts and the previous model's line, with the whole-backlog scores, stored
// forecasts and method collapsed.

import { el, forecastView, forecastCard, pinnedInfoHint, forecastMoreSection, forecastTable, COMPLEXITY_LABELS, reportsReviewMode, scheduleShowsReview } from './forecast-shared.js';
import { formatDateTime } from './common.js';
import { fmtShare, formatForecastHours, formatForecastQuantileRange, formatForecastTokens, formatForecastUsd, num } from './forecast-summary.js';

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
// Stored forecasts are newest first. Keep calibration history to one bounded
// page so its column headings stay close to the rows being inspected.
const CALIBRATION_HISTORY_PAGE_SIZE = 10;
// The Tasks table's limit, which the method note describes.
const SCHEDULE_ROWS = 50;

function scheduleCaption(review) {
  const base = "In-progress tasks, then the backlog in dispatch order. Start time and order are p50 – p90 across simulated drains; work hours, cost and tokens count every attempt a task starts by the horizon; finishes is the share of runs in which the task finishes within the horizon.";
  return review ? `${base} Each task's info icon names its complexity and assumed review mode: a thin pool either keeps that mode or drops it.` : base;
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

function byComplexityTable() {
  const rows = (forecastView.payload.forecast?.by_complexity || []).map((row) => el("tr", {}, [
    el("th", { scope: "row", text: COMPLEXITY_LABELS[row.complexity] || row.complexity }),
    el("td", { class: "num", text: String(row.tasks) }),
    el("td", { class: "fc-basis", text: basisText(row.basis) }),
    el("td", { class: "num", text: formatForecastQuantileRange(row.work_hours, formatForecastHours) }),
    el("td", { class: "num", text: formatForecastQuantileRange(row.usd, formatForecastUsd) }),
    el("td", { class: "num", text: formatForecastQuantileRange(row.tokens, formatForecastTokens) }),
  ]));
  return rows.length
    ? forecastTable("Forecast by complexity", [["Complexity"], ["Tasks", true], ["Sampled from"], ["Work hours", true], ["Cost", true], ["Tokens", true]], rows, "p50 – p90 per complexity. Work hours sum every task's slot time, so they exceed the time to drain when runs overlap.")
    : el("div", { class: "mx-empty-note", text: "No backlog tasks." });
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
  const parts = [];
  for (const [label, scores] of [
    ["Adjusted", level.adjusted || level],
    ["Baseline", level.pre_regression_baseline],
  ]) {
    if (!scores) continue;
    const drain = scores.time_to_drain_hours || {};
    if (drain.n) {
      parts.push(`${label} whole-backlog totals: ${drain.n} scored, ${fmtShare(drain.under_p50)} under p50 · ${fmtShare(drain.under_p90)} under p90 on time to drain`);
    } else {
      parts.push(`${label} whole-backlog drain time: none scored yet`);
    }
  }
  if (num(level.settled)) parts.push(`${level.settled} settled${num(level.not_drained) ? `, ${level.not_drained} not drained` : ""}`);
  return el("p", { class: "fc-line fc-settlement", text: `${parts.join(". ")}.` });
}

function previousModelLine(calibration) {
  const previous = (calibration.by_model_version || [])
    .filter((row) => row.model_version !== calibration.model_version && row.task_level?.hours?.n)
    .sort((a, b) => b.model_version - a.model_version)[0];
  if (!previous) return null;
  const hits = previous.task_level.hours;
  return el("p", { class: "fc-line fc-previous-model", text: `Previous model v${previous.model_version}: ${fmtShare(hits.under_p50)} · ${fmtShare(hits.under_p90)} on task hours, ${hits.n} scored.` });
}

// A share within five points of its target reads as calibrated; anything else
// is a miss.
function hitCell(share, target) {
  const value = num(share);
  const miss = value != null && Math.abs(value - target) > 0.05;
  return el("td", { class: miss ? "num fc-miss" : "num", text: fmtShare(value) });
}

function calibrationRow(label, hits) {
  return el("tr", {}, [
    el("th", { scope: "row", text: label }),
    hitCell(hits?.under_p50, 0.5),
    hitCell(hits?.under_p90, 0.9),
    el("td", { class: "num", text: String(hits?.n ?? 0) }),
  ]);
}

function calibrationCard() {
  const calibration = forecastView.payload.calibration || {};
  const current = calibration.model_version;
  const noForecasts = !(calibration.forecasts || []).length;
  const about = current != null ? `Model v${current}, the current model. Each version is scored on its own forecasts. A well-calibrated forecast has about 50% of actuals under p50 and 90% under p90; a share more than five points off is marked.` : "A well-calibrated forecast has about 50% of actuals under p50 and 90% under p90; a share more than five points off is marked.";
  const node = forecastCard("Calibration", "Target: 50% under p50, 90% under p90.", noForecasts ? [el("div", { class: "mx-empty" }, [
    el("div", { class: "mx-empty-title", text: "No stored forecasts to score yet" }),
    el("div", { class: "mx-empty-detail", text: "Calibration appears once a stored forecast's tasks start finishing." }),
  ])] : [
    forecastTable("Calibration", [["Measure"], ["Under p50", true], ["Under p90", true], ["Scored", true]], [
      calibrationRow("Task hours", calibration.task_level?.hours),
      calibrationRow("Task cost", calibration.task_level?.usd),
      calibrationRow("Task tokens", calibration.task_level?.tokens),
      calibrationRow("Whole backlog", (calibration.forecast_level?.adjusted || calibration.forecast_level)?.time_to_drain_hours),
    ]),
    previousModelLine(calibration),
  ], "fc-calibration");
  node.querySelector(".mx-card-title")?.append(pinnedInfoHint(about));
  return node;
}

// Behind the table: the same scores by complexity and for the whole backlog.
function calibrationDetailSection() {
  const calibration = forecastView.payload.calibration || {};
  if (!(calibration.forecasts || []).length) return null;
  return forecastMoreSection("calibration", "Calibration detail", "by complexity, whole backlog", [
    populationLine(calibration),
    complexityHits(calibration),
    el("h3", { class: "fc-sub-title", text: "Adjusted whole-backlog totals" }),
    forecastCalibrationMeasures("Adjusted", calibration.forecast_level?.adjusted || calibration.forecast_level),
    el("h3", { class: "fc-sub-title", text: "Pre-regression baseline" }),
    el("p", { class: "fc-line", text: "Baseline predictions exclude unsupported regression work. They are scored separately against the same observed totals, including regression follow-ups. Missing adjusted predictions remain unscored. Whole-backlog scores settle under each model's rule, up to 90 days after the forecast." }),
    forecastCalibrationMeasures("Baseline", calibration.forecast_level?.pre_regression_baseline),
    settlementLine(calibration),
  ]);
}

function forecastCalibrationMeasures(label, level) {
  return el("div", { class: "fc-cals" }, [
    calibrationMeasure(`${label} drain time`, level?.time_to_drain_hours),
    calibrationMeasure(`${label} cost`, level?.usd),
    calibrationMeasure(`${label} tokens`, level?.tokens),
  ]);
}

const TASK_POPULATIONS = {
  first_not_started_forecast_per_task: "Each task counts once, from its first stored forecast taken before work started.",
};

function populationLine(calibration) {
  const text = TASK_POPULATIONS[calibration.task_population];
  return text ? el("p", { class: "fc-line", text }) : null;
}

// Task hit rates per complexity: under p50 · under p90 (scored).
function complexityHits(calibration) {
  const rows = (calibration.task_level_by_complexity || []).filter((row) => row.hours?.n || row.usd?.n || row.tokens?.n);
  if (!rows.length) return null;
  return forecastTable("Task calibration by complexity", [["Complexity"], ["Task hours", true], ["Task cost", true], ["Task tokens", true]], rows.map((row) => el("tr", {}, [
    el("th", { scope: "row", text: row.complexity }),
    el("td", { class: "num", text: versionHits(row.hours) }),
    el("td", { class: "num", text: versionHits(row.usd) }),
    el("td", { class: "num", text: versionHits(row.tokens) }),
  ])), "By complexity, under p50 · under p90 (scored).");
}

// ---- residuals -------------------------------------------------------------

const RESIDUAL_METRICS = [["hours", "Work hours"], ["usd", "Cost"], ["tokens", "Tokens"]];
// The chart spans at least ±ln 2 (half to double the p50), and the largest
// residual when that is further, so a calm run is not stretched to the edges.
const RESIDUAL_MIN_SPAN = Math.LN2;

// "1.8× p50" for a log residual: the actual as a multiple of the forecast's p50.
function residualRatio(value, suffix = " p50") {
  const ratio = Math.exp(value);
  return `${ratio >= 10 ? Math.round(ratio) : ratio.toFixed(ratio >= 1 ? 1 : 2)}×${suffix}`;
}

// One bar per scored forecast, oldest first, from `rows` carrying
// `{ at, version, value, detail }`. Bars above the zero line are work that
// ran over the forecast's p50 (under-predicted); bars below came in under it
// (over-predicted). Earlier model versions are muted, and a version change
// starts a labelled segment, since their quantiles are not comparable.
function residualChart(label, rows, current) {
  const span = Math.max(RESIDUAL_MIN_SPAN, ...rows.map((row) => Math.abs(row.value)));
  const bars = [];
  const labels = [];
  let version = null;
  rows.forEach((row, index) => {
    const share = Math.min(1, Math.abs(row.value) / span) * 50;
    const over = row.value >= 0;
    const bar = el("span", {
      class: `fc-res-bar ${over ? "fc-res-under" : "fc-res-over"}${row.version === current ? "" : " fc-res-old"}${row.version !== version && index ? " fc-res-split" : ""}`,
      title: `${formatDateTime(row.at)} · model v${row.version} · ${residualRatio(row.value)} (${over ? "ran over" : "came in under"} the forecast)${row.detail ? ` · ${row.detail}` : ""}`,
    }, [el("span", { class: "fc-res-fill", style: { height: `${share}%`, [over ? "bottom" : "top"]: "50%" } })]);
    bars.push(bar);
    if (row.version !== version) {
      version = row.version;
      labels.push(el("span", { class: "fc-res-version", style: { gridColumn: `${index + 1} / -1` }, text: `v${version}` }));
    }
  });
  const higher = residualRatio(span, "");
  const lower = residualRatio(-span, "");
  const newest = rows[rows.length - 1];
  const summary = `${label}: ${rows.length} scored forecast${rows.length === 1 ? "" : "s"}, newest ${residualRatio(newest.value)}. Range ${lower} to ${higher} of p50.`;
  const plot = el("div", { class: "fc-res-plot", role: "img", "aria-label": summary }, [
    el("div", { class: "fc-res-bars", style: { gridTemplateColumns: `repeat(${rows.length}, minmax(3px, 14px))` } }, [
      el("div", { class: "fc-res-zero", "aria-hidden": "true" }), ...bars, ...labels,
    ]),
  ]);
  return el("div", { class: "fc-res" }, [
    el("div", { class: "fc-res-head" }, [el("span", { text: label }), el("span", { class: "fc-res-n", text: `${rows.length} scored` })]),
    el("div", { class: "fc-res-body" }, [
      el("div", { class: "fc-res-axis", "aria-hidden": "true" }, [
        el("span", { text: `${higher} · over` }),
        el("span", { text: "p50" }),
        el("span", { text: `${lower} · under` }),
      ]),
      plot,
    ]),
  ]);
}

function residualsCard() {
  const calibration = forecastView.payload.calibration || {};
  const current = calibration.model_version;
  const metric = RESIDUAL_METRICS.find(([key]) => key === forecastView.residualMetric) || RESIDUAL_METRICS[0];
  const scored = [...(calibration.forecasts || [])].reverse();
  const taskRows = scored
    .filter((row) => num(row.residual?.[metric[0]]?.median) != null)
    .map((row) => ({ at: row.forecast_at, version: row.model_version, value: row.residual[metric[0]].median, detail: `median of ${row.residual[metric[0]].n} task${row.residual[metric[0]].n === 1 ? "" : "s"}` }));
  const drainRows = scored
    .filter((row) => num(row.residual?.time_to_drain_hours) != null)
    .map((row) => ({ at: row.forecast_at, version: row.model_version, value: row.residual.time_to_drain_hours, detail: `drain ${formatForecastHours(row.actual?.time_to_drain_hours)} vs p50 ${formatForecastHours(row.forecast?.time_to_drain_hours?.p50)}` }));
  if (!taskRows.length && !drainRows.length && forecastView.residualMetric === "hours") {
    return forecastCard("Actual vs forecast", null, [el("div", { class: "mx-empty" }, [
      el("div", { class: "mx-empty-title", text: "No scored residuals yet" }),
      el("div", { class: "mx-empty-detail", text: "A bar appears for each stored forecast once one of its tasks finishes: the median over its finished tasks of how far the actual ran from the forecast's p50. Whole-backlog drain time follows when a forecast settles, up to 90 days later." }),
    ])], "fc-residuals");
  }
  const picker = el("div", { class: "mx-seg", role: "group", "aria-label": "Residual measure" }, RESIDUAL_METRICS.map(([key, text]) => {
    const on = key === metric[0];
    const button = el("button", { class: on ? "mx-seg-btn on" : "mx-seg-btn", type: "button", text, "aria-pressed": on ? "true" : "false" });
    button.addEventListener("click", () => {
      if (on) return;
      forecastView.residualMetric = key;
      forecastView.render();
    });
    return button;
  }));
  const about = pinnedInfoHint(`One bar per stored forecast, oldest first: the median across its finished tasks of ln(actual ÷ forecast p50) for task ${metric[1].toLowerCase()}, so tasks of different size compare. Zero is a hit on the p50; +0.69 means twice the forecast, −0.69 half. Earlier model versions are muted and split off, since their quantiles differ.`);
  const charts = [
    taskRows.length ? residualChart(`Task ${metric[1].toLowerCase()}`, taskRows, current) : el("div", { class: "mx-empty-note", text: `No task ${metric[1].toLowerCase()} residuals scored yet.` }),
    drainRows.length ? residualChart("Whole-backlog time to drain", drainRows, current) : el("div", { class: "mx-empty-note", text: "No whole-backlog drain time scored yet; it settles up to 90 days after a forecast." }),
  ];
  const node = forecastCard("Actual vs forecast", null, [picker, el("div", { class: "fc-ress" }, charts), el("div", { class: "fc-res-legend", "aria-hidden": "true" }, [
    el("span", { class: "fc-res-key fc-res-under", text: "Ran over" }),
    el("span", { class: "fc-res-key fc-res-over", text: "Came in under" }),
  ])], "fc-residuals");
  node.querySelector(".mx-card-title")?.append(about);
  return node;
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

function storedSection() {
  const calibration = forecastView.payload.calibration || {};
  const stored = forecastView.payload.stored || {};
  const forecasts = calibration.forecasts || [];
  const served = forecastView.payload.served_from === "stored";
  const latest = stored.latest_at ? `latest stored ${formatDateTime(stored.latest_at)}` : "none stored yet";
  const meta = served ? `${latest} · scored when stored` : latest;
  const blurb = el("p", { class: "mx-blurb", text: `${meta}. The default forecast is stored at most once every ${stored.min_interval_minutes ?? 60} minutes. Per-task calibration can score as tasks finish; whole-forecast totals follow each model's settlement rule. The default view shows that stored forecast with the calibration scored when it was stored; Refresh scores it now.` });
  if (!forecasts.length) return forecastMoreSection("stored", "Stored forecasts", "none yet", [blurb]);
  const versions = calibration.by_model_version || [];
  const current = calibration.model_version;
  const older = versions.filter((row) => row.model_version !== current);
  const byVersion = older.length ? forecastTable("Calibration by model version", [["Model"], ["Forecasts", true], ["Task hours", true], ["Time to drain", true]], older.map((row) => el("tr", {}, [
    el("th", { scope: "row", text: `v${row.model_version}` }),
    el("td", { class: "num", text: String(row.forecasts ?? 0) }),
    el("td", { class: "num", text: versionHits(row.task_level?.hours) }),
    el("td", { class: "num", text: versionHits(row.forecast_level?.time_to_drain_hours) }),
  ])), "Earlier models, under p50 · under p90 (scored).") : null;
  const pageCount = Math.ceil(forecasts.length / CALIBRATION_HISTORY_PAGE_SIZE);
  forecastView.historyPage = Math.min(forecastView.historyPage, pageCount - 1);
  const firstRow = forecastView.historyPage * CALIBRATION_HISTORY_PAGE_SIZE;
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
  const history = forecastTable("Stored forecasts", [["Recorded"], ["Model", true], ["Done", true], ["Left out"], ["Drain p50 – p90 → actual", true], ["Cost", true], ["Tokens", true], ["State"]], rows, "Stored forecasts, newest first");
  history.classList.add("fc-history-table-wrap");
  const newer = el("button", { class: "mx-seg-btn", type: "button", text: "Newer forecasts" });
  newer.disabled = forecastView.historyPage === 0;
  newer.addEventListener("click", () => {
    forecastView.historyPage = Math.max(0, forecastView.historyPage - 1);
    forecastView.render();
  });
  const olderButton = el("button", { class: "mx-seg-btn", type: "button", text: "Older forecasts" });
  olderButton.disabled = forecastView.historyPage >= pageCount - 1;
  olderButton.addEventListener("click", () => {
    forecastView.historyPage = Math.min(pageCount - 1, forecastView.historyPage + 1);
    forecastView.render();
  });
  const pagination = pageCount > 1 ? el("nav", { class: "fc-history-pagination", "aria-label": "Stored forecast history pages" }, [
    el("span", { class: "fc-history-page-status", role: "status", "aria-live": "polite", text: `Showing ${firstRow + 1}–${lastRow} of ${forecasts.length} stored forecasts · Page ${forecastView.historyPage + 1} of ${pageCount} · newest first` }),
    el("div", { class: "mx-seg", role: "group", "aria-label": "Stored forecast history" }, [newer, olderButton]),
  ]) : null;
  return forecastMoreSection("stored", "Stored forecasts", String(forecasts.length), [
    blurb,
    byVersion,
    el("p", { class: "mx-blurb", text: `Left out across stored forecasts: ${excludedText(excluded)}.` }),
    history,
    pagination,
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

const ADMISSION_TEXT = "Each simulated drain admits work as the drain does: in dispatch order, after its dependencies finish, never beside a task whose files overlap, an editing task without context files alone, and behind surfaces reserved by higher-priority work. Each attempt first draws a recorded attempt of the same pool: one that blocked, failed or was abandoned frees its slot when that attempt did, then requeues, waits for a return drawn from how long blocked work took to come back (or never does), or stops. Blocked tasks start out waiting for that return. Review and someday tasks are left out, and so is work that depends on them; in-progress tasks count only their remaining work. Review locks, other static eligibility and arrivals' files are not modelled yet.";

function methodSection() {
  const inputs = forecastView.payload.inputs || {};
  const backlog = forecastView.payload.backlog || {};
  const meta = `${backlog.backlog ?? 0} backlog · ${backlog.in_progress ?? 0} in progress · ${backlog.blocked ?? 0} blocked · ${historyText(inputs)}`;
  const predictions = forecastView.payload.forecast?.task_predictions || [];
  return forecastMoreSection("method", "How this forecast is made", null, [
    el("p", { class: "mx-blurb" }, [
      `${meta}. Simulated ${inputs.iterations ?? 0} times on ${capacityText(inputs)}.`,
      forecastView.payload.model_version != null ? ` Model v${forecastView.payload.model_version}.` : null,
    ]),
    el("h3", { class: "fc-sub-title", text: "Sampling and admission" }),
    el("p", { class: "mx-blurb", text: samplingExplanation(inputs) }),
    el("p", { class: "mx-blurb", text: ADMISSION_TEXT }),
    forecastView.payload.model_version >= 19 ? el("p", { class: "mx-blurb", text: "Time to drain covers startable work. Future-scheduled and no-drain tasks are listed separately, with standalone work, token and cost estimates retained in spending totals. Their future regression follow-up stays outside this drain. Stored scheduled and excluded work stays outside drain-time actuals too; model versions are scored separately." }) : null,
    predictions.length ? el("p", { class: "mx-blurb", text: scheduleCaption(scheduleShowsReview(predictions.slice(0, SCHEDULE_ROWS))) }) : null,
    el("h3", { class: "fc-sub-title", text: "By complexity" }),
    byComplexityTable(),
    el("h3", { class: "fc-sub-title", text: "Calibration settlement" }),
    el("p", { class: "mx-blurb", text: CALIBRATION_SETTLEMENT }),
  ]);
}

/// The Accuracy tab.
export function accuracyPanelNodes() {
  return [
    calibrationCard(),
    residualsCard(),
    el("div", { class: "fc-more-list" }, [calibrationDetailSection(), storedSection(), methodSection()]),
  ];
}
