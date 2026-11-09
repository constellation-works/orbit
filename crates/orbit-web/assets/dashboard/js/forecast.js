// Orbit dashboard Analytics › Forecast: how long the dispatchable backlog
// takes to drain and what it costs, as p50 and p90 resampled from this
// workspace's delivery history. Read from /api/diagnostics/backlog_forecast.
// The Tasks drain card reuses this default read (30 days, no arrivals) as a
// compact summary. Pure vanilla JS, split into ES modules with no build step:
// this file owns fetching, the tabs and the Forecast tab; forecast-accuracy.js
// the Accuracy tab, forecast-gaps.js the Data gaps tab and forecast-summary.js
// the formatting and drain-card summary.
//
// The view switches between this Backlog forecast and the Resource forecast
// (resource-forecast.js). Each reads only when it is first shown and on its
// own refresh, so switching never refetches the other.
//
// The forecast is per workspace; the aggregate view shows the usual
// placeholder. The history select and the With new arrivals scenario refetch:
// only the default view (30 days reaching back to at least the newest 50
// completions, no arrivals, the live drain ceiling) is the one stored and
// scored, and is read from the newest stored snapshot when one is fresh;
// Refresh recomputes it live and keeps that result until the workspace or
// forecast options change. The With N proposed scenario reads `if_approved`
// from the same response. History is counted in tasks, `Last 200` or
// `Last 500`, or in days; a month without deliveries leaves the default and
// the counted views with history.

import {
  syncNodes, fetchJson, requestPanel, panelCanRender, getWorkspaceRevision,
  onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime,
  bindTaskLink, relativeTime, statusPill,
} from './common.js';
import {
  el, forecastView as view, forecastCard, forecastTable, forecastMoreSection, pinnedInfoHint,
  COMPLEXITY_LABELS, scheduleShowsReview,
} from './forecast-shared.js';
import {
  UNREPORTED_FORECAST_REASON, forecastUnavailableInfo, forecastBaselineFigures, formatForecastHours,
  formatForecastQuantileRange, formatForecastTokens, formatForecastUsd, fmtParallelism, fmtRank, fmtRate,
  fmtShare, forecastCount, hasHorizonCensoringEvidence, num, quantilePair, recordedPoolsContainGaps,
  regressionGapBecause, regressionHistoryIsAbsent, forecastDrainScope,
} from './forecast-summary.js';
import { dataGapsPanelNodes, openCoverageCount, otherChecksClearText } from './forecast-gaps.js';
import { accuracyPanelNodes } from './forecast-accuracy.js';
import * as forecastSummary from './forecast-summary.js';
import {
  fetchAndRenderResourceForecast, onResourceForecastTabPick, placeholdResourceForecastAggregate,
  resourceForecastLoaded, resourceForecastRoute, setResourceForecastTab,
} from './resource-forecast.js';

export {
  backlogForecastFailureSummary, backlogForecastSummary, drainBacklogForecastLink, forecastBaselineFigures,
  forecastUnavailableInfo, formatForecastHours, formatForecastQuantileRange, formatForecastTokens, formatForecastUsd,
} from './forecast-summary.js';

const $ = (id) => document.getElementById(id);

const BODY = "forecast-body";
const COUNT = "forecast-count";
const ENDPOINT = "/api/diagnostics/backlog_forecast";
// History selection: the default lookback (30 days, at least the newest 50
// completions), the newest N completions whatever their age, or 90 days.
const HISTORIES = [
  ["default", "30d · ≥50", "The last 30 days, reaching back to at least the newest 50 completed tasks: the stored forecast"],
  ["tasks:200", "Last 200", "The newest 200 completed tasks, whatever their age"],
  ["tasks:500", "Last 500", "The newest 500 completed tasks, whatever their age"],
  ["days:90", "90d", "The last 90 days, reaching back to at least the newest 50 completed tasks"],
];
const FORECAST_TABS = [["forecast", "Forecast"], ["accuracy", "Accuracy"], ["data-gaps", "Data gaps"]];
const DEFAULT_FORECAST_TAB = FORECAST_TABS[0][0];
const REVIEW_MODE_LABELS = {
  none: "No review", before_pr: "Before PR", before_landing: "Before landing",
  mixed: "Mixed review", unknown: "Unknown assumption",
};
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

let requestScope = null;
let liveRefresh = null;
let tabPickListener = null;
// The switch's label while a scenario with no payload yet loads; null until a
// read succeeds in the workspace, so an unknown count never reads as zero.
let proposedKnown = null;

function clearForecast() {
  view.payload = null;
  liveRefresh = null;
  view.historyPage = 0;
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

function pathOptions() {
  return { history: view.history, arrivals: view.scenario === "arrivals" ? "on" : "off" };
}

function endpoint() {
  return backlogForecastPath(pathOptions());
}

export function fetchBacklogForecast(options) {
  return fetchJson(backlogForecastPath(options));
}

export function placeholdForecastAggregate() {
  clearForecast();
  renderPanelPlaceholder(BODY);
  placeholdResourceForecastAggregate();
}

// ---- backlog / resource switch ----------------------------------------------

const FORECAST_VIEWS = ["backlog", "resources"];
let forecastViewShown = "backlog";

/// Read the forecast view on screen: the Backlog or the Resource forecast.
export function fetchForecastView() {
  return forecastViewShown === "resources" ? fetchAndRenderResourceForecast() : fetchAndRenderForecast();
}

function applyForecastView() {
  const resources = forecastViewShown === "resources";
  const backlogPanel = $("forecast-panel");
  const resourcePanel = $("resource-forecast-panel");
  if (backlogPanel) backlogPanel.hidden = resources;
  if (resourcePanel) resourcePanel.hidden = !resources;
  for (const button of document.querySelectorAll("#forecast-switch [data-forecast-view]")) {
    const on = button.dataset.forecastView === forecastViewShown;
    button.classList.toggle("on", on);
    button.setAttribute("aria-selected", on ? "true" : "false");
    button.tabIndex = on ? 0 : -1;
  }
}

// A pick shows the other view, reading it only if it has not been read in
// this workspace, and keeps the address in step without routing.
function pickForecastView(name) {
  if (!FORECAST_VIEWS.includes(name) || name === forecastViewShown) return;
  forecastViewShown = name;
  applyForecastView();
  tabPickListener?.();
  if (isAggregateView()) return;
  if (name === "resources" && !resourceForecastLoaded()) {
    fetchAndRenderResourceForecast().catch((error) => console.error("Failed to fetch the resource forecast", error));
  } else if (name === "backlog" && !view.payload) {
    refetch();
  }
}

function bindForecastSwitch() {
  const strip = $("forecast-switch");
  if (!strip || strip.dataset.bound) return;
  strip.dataset.bound = "true";
  for (const button of strip.querySelectorAll("[data-forecast-view]")) {
    button.addEventListener("click", () => pickForecastView(button.dataset.forecastView));
  }
  strip.addEventListener("keydown", (event) => {
    if (!["ArrowRight", "ArrowLeft", "Home", "End"].includes(event.key)) return;
    const index = FORECAST_VIEWS.indexOf(forecastViewShown);
    const next = event.key === "Home" ? 0 : event.key === "End" ? FORECAST_VIEWS.length - 1
      : (index + (event.key === "ArrowRight" ? 1 : -1) + FORECAST_VIEWS.length) % FORECAST_VIEWS.length;
    event.preventDefault();
    pickForecastView(FORECAST_VIEWS[next]);
    strip.querySelector(`[data-forecast-view="${FORECAST_VIEWS[next]}"]`)?.focus();
  });
  applyForecastView();
}

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", bindForecastSwitch);
else bindForecastSwitch();

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
  const viewChanged = requestScope !== scope;
  if (viewChanged) {
    requestScope = scope;
    clearForecast();
  }
  if (!refresh && liveRefresh) return liveRefresh;
  const request = refresh ? backlogForecastPath({ ...pathOptions(), refresh: true }) : path;
  const pending = requestPanel(BODY, path, () => fetchJson(request), (payload) => {
    if (getWorkspaceRevision() !== revision) return;
    view.payload = payload;
    renderForecast();
  }, COUNT).catch((error) => {
    // A route-missing 404 is a permanent capability message, so it should not
    // leave controls for a forecast endpoint this host does not provide. A poll
    // or Refresh after a good read keeps its figures: the note already says they
    // are stale, and clearing them would leave that note over an empty panel.
    if (error.routeMissing && !view.payload && getWorkspaceRevision() === revision && requestScope === scope) {
      syncNodes($(BODY), []);
    }
    throw error;
  });
  // Any view can load slowly or fail, so keep its recovery controls available.
  if (viewChanged) renderViewControls();
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
  proposedKnown = null;
  clearForecast();
});

export function renderForecast() {
  const body = $(BODY);
  if (!body) return;
  if (isAggregateView()) {
    placeholdForecastAggregate();
    return;
  }
  if (!view.payload || !panelCanRender(BODY)) return;
  proposedKnown = forecastCount(view.payload.backlog?.proposed);
  const scenario = selectedScenario();
  const count = $(COUNT);
  if (count) count.textContent = `${scenario.source?.tasks ?? 0} task${scenario.source?.tasks === 1 ? "" : "s"} · ${view.payload.inputs?.concurrency ?? "?"} slots`;
  const root = el("div", { class: "fc-view" }, [
    forecastHeader(),
    accuracyHeader(),
    dataGapsHeader(),
    forecastTabStrip(),
    forecastTabPanel("forecast", forecastPanelNodes(scenario)),
    forecastTabPanel("accuracy", accuracyPanelNodes()),
    forecastTabPanel("data-gaps", dataGapsPanelNodes()),
  ]);
  root.dataset.key = "forecast-view";
  root.dataset.hash = JSON.stringify([view.history, view.scenario, view.payload, view.historyPage, view.residualMetric]);
  syncNodes(body, [root]);
  applyForecastTab();
}
view.render = renderForecast;

function refetch() {
  fetchAndRenderForecast().catch((error) => console.error("Failed to fetch backlog forecast", error));
}

// ---- tabs -----------------------------------------------------------------
//
// All three tabs stay rendered and a switch only shows another, so it never
// refetches and an open section or focused control survives it.

/// The route of the tab on screen, without the leading `#`.
export function forecastTabRoute() {
  if (forecastViewShown === "resources") return resourceForecastRoute();
  return view.tab === DEFAULT_FORECAST_TAB ? "analytics/forecast" : `analytics/forecast/${view.tab}`;
}

/// Show `tab` (an unknown or missing one is the Forecast tab). `resources`
/// shows the Resource forecast instead, on its tab `subtab`.
export function setForecastTab(tab, subtab) {
  if (tab === "resources") {
    forecastViewShown = "resources";
    setResourceForecastTab(subtab);
  } else {
    forecastViewShown = "backlog";
    view.tab = FORECAST_TABS.some(([key]) => key === tab) ? tab : DEFAULT_FORECAST_TAB;
    applyForecastTab();
  }
  applyForecastView();
}

/// `listener` runs when the operator picks a tab or view, so the router can
/// keep the address in step without routing, which would refetch.
export function onForecastTabPick(listener) {
  tabPickListener = listener;
  onResourceForecastTabPick(listener);
}

function pickForecastTab(tab) {
  setForecastTab(tab);
  tabPickListener?.();
}

function applyForecastTab() {
  const body = $(BODY);
  if (!body) return;
  for (const button of body.querySelectorAll(".fc-tab")) {
    const on = button.dataset.fcTab === view.tab;
    button.classList.toggle("active", on);
    button.setAttribute("aria-selected", on ? "true" : "false");
    button.tabIndex = on ? 0 : -1;
  }
  for (const node of body.querySelectorAll(".fc-tabbed")) node.hidden = node.dataset.fcTab !== view.tab;
  // A long residual series scrolls inside its plot; start at the newest
  // forecast, which needs the plot on screen to measure.
  for (const plot of body.querySelectorAll(".fc-res-plot")) {
    if (plot.dataset.fcScrolled || !plot.scrollWidth) continue;
    plot.scrollLeft = plot.scrollWidth;
    plot.dataset.fcScrolled = "true";
  }
}

function forecastTabStrip() {
  const open = openCoverageCount();
  const strip = el("div", { class: "band-tabs fc-tabs", role: "tablist", "aria-label": "Forecast sections" }, FORECAST_TABS.map(([key, text]) => {
    const button = el("button", {
      id: `fc-tab-${key}`, class: "band-tab fc-tab", type: "button", role: "tab", "aria-controls": `fc-panel-${key}`,
    }, [text, key === "data-gaps" && open ? el("span", { class: "band-tab-count fc-tab-count", text: String(open) }) : null]);
    button.dataset.fcTab = key;
    button.addEventListener("click", () => pickForecastTab(key));
    return button;
  }));
  strip.addEventListener("keydown", (event) => {
    const keys = FORECAST_TABS.map(([key]) => key);
    const index = keys.indexOf(event.target?.dataset?.fcTab);
    if (index < 0) return;
    let next = null;
    if (event.key === "ArrowRight") next = (index + 1) % keys.length;
    else if (event.key === "ArrowLeft") next = (index - 1 + keys.length) % keys.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = keys.length - 1;
    if (next == null) return;
    event.preventDefault();
    pickForecastTab(keys[next]);
    $(`fc-tab-${keys[next]}`)?.focus();
  });
  return strip;
}

function forecastTabPanel(key, children) {
  const panel = el("div", { id: `fc-panel-${key}`, class: "fc-panel fc-tabbed", role: "tabpanel", "aria-labelledby": `fc-tab-${key}` }, children);
  panel.dataset.fcTab = key;
  return panel;
}

// ---- header ---------------------------------------------------------------

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
  const stored = view.payload.served_from === "stored";
  const age = relativeTime(view.payload.generated_at);
  const text = stored ? `Stored ${age ?? "earlier"}` : "Live";
  const title = view.payload.generated_at ? `Computed ${formatDateTime(view.payload.generated_at)}` : null;
  return el("span", { class: "fc-source", text, title });
}

function refreshButton() {
  const button = el("button", { class: "fc-refresh", type: "button", title: "Recompute this forecast live now", "aria-label": "Refresh forecast" }, [
    el("span", { class: "visually-hidden", text: "Refresh" }),
  ]);
  button.addEventListener("click", () => {
    button.disabled = true;
    fetchAndRenderForecast({ refresh: true })
      .catch((error) => console.error("Failed to refresh backlog forecast", error))
      .finally(() => { button.disabled = false; });
  });
  return button;
}

function tabbedHeader(tab, children) {
  const header = el("div", { class: "fc-header fc-tabbed" }, children);
  header.dataset.fcTab = tab;
  return header;
}

// The panel head already reads "N tasks · N slots"; this line continues it.
function forecastHeader() {
  return tabbedHeader("forecast", [
    el("p", { class: "fc-meta" }, [
      sourceLabel(),
      view.payload.model_version != null ? ` · model v${view.payload.model_version}` : null,
    ]),
    el("div", { class: "fc-tools" }, [historyControl(), refreshButton()]),
  ]);
}

function accuracyHeader() {
  const version = view.payload.calibration?.model_version ?? view.payload.model_version;
  return tabbedHeader("accuracy", [el("p", { class: "fc-meta", text: version != null ? `Model v${version}` : "" })]);
}

function dataGapsHeader() {
  return tabbedHeader("data-gaps", [el("p", { class: "fc-meta", text: otherChecksClearText() })]);
}

// While a picked view loads or fails only its controls can render: the status
// note above them says which, and these are the way back.
function renderViewControls() {
  const body = $(BODY);
  if (!body) return;
  const root = el("div", { class: "fc-view" }, [
    el("div", { class: "fc-header" }, [el("div", { class: "fc-tools" }, [historyControl(), refreshButton()])]),
    el("div", { class: "fc-scenario-row" }, [scenarioSwitch()]),
  ]);
  root.dataset.key = "forecast-view";
  // The scenario buttons close over their pressed state, so a pick or a newly
  // learned count must replace the node rather than reuse the old one.
  root.dataset.hash = JSON.stringify(["controls", view.history, view.scenario, proposedKnown]);
  syncNodes(body, [root]);
}

// ---- scenario -------------------------------------------------------------

function proposedScenarioReady(payload = view.payload) {
  return Boolean(payload?.if_approved && forecastCount(payload.backlog?.proposed));
}

// The figures on screen. A selected proposed scenario the response cannot back
// (the workspace changed, or nothing is proposed now) reads as the current one.
function selectedScenario() {
  const payload = view.payload;
  if (view.scenario === "proposed" && proposedScenarioReady()) {
    const coverage = forecastSummary.proposedForecastCoverage(payload);
    return { key: "proposed", source: payload.if_approved, coverage, info: forecastUnavailableInfo(payload.if_approved, { coverage }) };
  }
  const source = payload.forecast || {};
  return { key: view.scenario === "arrivals" ? "arrivals" : "current", source, coverage: payload.coverage || {}, info: forecastUnavailableInfo(source, payload) };
}

function pickScenario(key) {
  const before = endpoint();
  view.scenario = key;
  if (endpoint() !== before) refetch();
  // Same read: with no forecast loaded there is nothing for renderForecast to
  // draw, so redraw the controls to show the pick.
  else if (view.payload) renderForecast();
  else renderViewControls();
}

function scenarioSwitch() {
  const payload = view.payload;
  const proposed = payload ? forecastCount(payload.backlog?.proposed) : proposedKnown;
  const countKnown = proposed != null;
  // The proposed scenario lives in the no-arrivals read, so any loaded response
  // or a remembered count is enough to offer it.
  const offerable = payload ? proposedScenarioReady() : countKnown && proposed > 0;
  let proposedTitle = "The proposed-task count is unknown until a forecast loads";
  if (countKnown) {
    proposedTitle = offerable
      ? `If the ${proposed} proposed task${proposed === 1 ? " is" : "s are"} approved`
      : "No proposed-work forecast: nothing is proposed in this workspace";
  }
  const on = view.scenario === "proposed" && !offerable ? "current" : view.scenario;
  const choices = [
    ["current", "Current backlog", "The backlog and in-progress tasks, with no new tasks arriving"],
    ["proposed", countKnown ? `With ${proposed} proposed` : "With proposed", proposedTitle],
    ["arrivals", "With new arrivals", "New tasks keep arriving at the recent rate"],
  ];
  return el("div", { class: "mx-seg mx-seg-wrap fc-scenario", role: "group", "aria-label": "Scenario" }, choices.map(([key, text, title]) => {
    const pressed = key === on;
    const button = el("button", { class: pressed ? "mx-seg-btn on" : "mx-seg-btn", type: "button", text, title, "aria-pressed": pressed ? "true" : "false" });
    if (key === "proposed" && !offerable) button.disabled = true;
    button.addEventListener("click", () => {
      if (!pressed) pickScenario(key);
    });
    return button;
  }));
}

function arrivalsText() {
  const arrivals = view.payload.arrivals || {};
  const perDay = arrivals.per_day || {};
  const parts = [["auto-tasks", perDay.auto_task], ["sweeps", perDay.sweep], ["filings", perDay.filing]]
    .map(([label, rate]) => `${label} ${fmtRate(rate)}`).join(" · ");
  const assumed = view.payload.inputs?.arrivals
    ? "Includes new tasks arriving at the recent rate."
    : "Assumes no new tasks arrive.";
  return `${assumed} Last ${arrivals.window_days ?? 7} days: ${fmtRate(perDay.total)} filed (${parts}), ${fmtRate(arrivals.done_per_day)} done.`;
}

// ---- totals ---------------------------------------------------------------

// Unforecastable dependencies withhold every full total, whatever the history.
const SUBSET_REASON = "Not every task can be scheduled.";

function metricReason(scenario, quantiles, metric) {
  if (quantiles?.p90 != null) return metric === "tokens" ? "Normalized tokens." : null;
  if (scenario.source?.schedulable_subset) return SUBSET_REASON;
  const coverage = scenario.coverage;
  if (metric === "tokens" && num(coverage.uncovered_model)) return "Some tasks have no priced history.";
  if (metric === "usd" && num(coverage.uncosted)) return "Some tasks have no costed history.";
  if (view.payload.arrivals?.coverage?.[`missing_${metric}`]) return metric === "tokens" ? "Some history is missing token telemetry." : "Some history is missing cost telemetry.";
  return metric === "tokens" ? "Not enough priced history." : "Not enough costed history.";
}

function drainReason(scenario, quantiles) {
  if (quantiles?.p90 != null) return null;
  const { source } = scenario;
  if (source?.schedulable_subset) return SUBSET_REASON;
  if (quantiles?.p50 != null) return hasHorizonCensoringEvidence(source) ? "p90 beyond the 90-day horizon." : `${UNREPORTED_FORECAST_REASON}.`;
  if (scenario.coverage.no_history) return "Some tasks have no history.";
  return hasHorizonCensoringEvidence(source) ? "Beyond the 90-day horizon." : `${UNREPORTED_FORECAST_REASON}.`;
}

function costedLine(scenario) {
  const { tasks, uncosted } = scenario.coverage;
  if (![tasks, uncosted].every((count) => Number.isSafeInteger(count) && count >= 0)
    || tasks !== scenario.source?.tasks || uncosted > tasks) return "Cost coverage unavailable.";
  const costed = tasks - uncosted;
  return `${costed} of ${tasks} task${tasks === 1 ? "" : "s"} costed.`;
}

// "+0.4h at p50" between two drain times on the same basis.
function drainDelta(next, base) {
  const after = num(next?.p50);
  const before = num(base?.p50);
  if (after == null || before == null) return null;
  const diff = after - before;
  if (Math.abs(diff) < 1 / 60) return "No change at p50 against the current backlog.";
  return `${diff > 0 ? "+" : "−"}${formatForecastHours(Math.abs(diff))} at p50 against the current backlog.`;
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
  return forecast.simulation_truncated ? `${text} Some runs hit a simulation limit.` : text;
}

// The With new arrivals read still carries the if-approved scenario; the switch
// shows one scenario at a time, so its figures stay here.
function arrivalsApprovedLine(scenario) {
  const approved = view.payload.if_approved;
  if (scenario.key !== "arrivals" || !proposedScenarioReady()) return null;
  const coverage = forecastSummary.proposedForecastCoverage(view.payload);
  const figures = forecastUnavailableInfo(approved, { coverage }) ? approved.pre_regression_baseline : approved;
  if (!quantilePair(figures?.time_to_drain_hours) && !quantilePair(figures?.usd) && !quantilePair(figures?.tokens)) return null;
  const basis = figures === approved ? "" : " (pre-regression baseline)";
  return `If proposed work is also approved${basis}: drain ${formatForecastQuantileRange(figures.time_to_drain_hours, formatForecastHours)}, cost ${formatForecastQuantileRange(figures.usd, formatForecastUsd)}, tokens ${formatForecastQuantileRange(figures.tokens, formatForecastTokens)}.`;
}

// One p50 – p90 range; a percentile the response left out reads "unknown".
function rangeText(quantiles, fmt) {
  const p50 = num(quantiles?.p50);
  const p90 = num(quantiles?.p90);
  if (p50 == null && p90 == null) return null;
  return `${p50 == null ? "unknown" : fmt(p50)} – ${p90 == null ? "unknown" : fmt(p90)}`;
}

function totalTile(label, value, lines, className) {
  return el("div", { class: `mx-sc fc-fig fc-total ${className}`, role: "group", "aria-label": label }, [
    el("span", { class: "mx-sc-label" }, [label, pinnedInfoHint(lines.filter(Boolean).join("\n"))]),
    el("span", { class: value.known ? "fc-total-value" : "fc-total-value fc-total-unknown", text: value.text }),
  ]);
}

function tileValue(quantiles, fmt, missing) {
  const text = rangeText(quantiles, fmt);
  return { text: text ?? missing, known: text != null };
}

function adjustedTiles(scenario) {
  const { source } = scenario;
  const inputs = view.payload.inputs || {};
  const parallelism = rangeText(source.effective_parallelism, fmtParallelism);
  const criticalPath = rangeText(source.critical_path_hours, formatForecastHours);
  return [
    totalTile("Time to drain", tileValue(source.time_to_drain_hours, formatForecastHours, "Unknown"), [
      drainReason(scenario, source.time_to_drain_hours),
      scenario.key === "proposed" ? drainDelta(source.time_to_drain_hours, view.payload.forecast?.time_to_drain_hours) : null,
      drainOdds(source),
      parallelism ? `Effective parallelism ${parallelism} of ${inputs.concurrency ?? "?"} slots, averaged over the drain.` : null,
      criticalPath ? `Critical path ${criticalPath}: the longest dependency chain.` : null,
      arrivalsApprovedLine(scenario),
    ], "fc-kpi"),
    totalTile("Cost", tileValue(source.usd, formatForecastUsd, "Unknown"), [metricReason(scenario, source.usd, "usd"), costedLine(scenario)], "fc-kpi"),
    totalTile("Tokens", tileValue(source.tokens, formatForecastTokens, "Unknown"), [metricReason(scenario, source.tokens, "tokens")], "fc-kpi"),
  ];
}

// The regression-adjusted forecast is withheld. With a baseline each tile
// shows it, labelled; a percentile it lacks stays unknown, never zero.
function withheldTiles(scenario) {
  const { source, info } = scenario;
  const baseline = source.pre_regression_baseline;
  if (!forecastBaselineFigures(baseline)) {
    return ["Time to drain", "Cost", "Tokens"].map((label) => totalTile(label, { text: "No forecast", known: false }, [info.detail], "fc-none-fig"));
  }
  const limit = "Pre-regression baseline: excludes unsupported regression follow-up work.";
  const tile = (label, quantiles, fmt, extra) => {
    const value = tileValue(quantiles, fmt, "Unknown");
    return totalTile(label, value, [limit, value.known ? null : `Not estimated: ${info.label}.`, ...extra], "fc-baseline-fig");
  };
  return [
    tile("Time to drain", baseline.time_to_drain_hours, formatForecastHours, [
      scenario.key === "proposed" ? drainDelta(baseline.time_to_drain_hours, view.payload.forecast?.pre_regression_baseline?.time_to_drain_hours) : null,
      arrivalsApprovedLine(scenario),
    ]),
    tile("Cost", baseline.usd, formatForecastUsd, [costedLine(scenario)]),
    tile("Tokens", baseline.tokens, formatForecastTokens, []),
  ];
}

// ---- status lines ---------------------------------------------------------

function unavailableWarning(text, kind) {
  const link = el("button", { class: "fc-link", type: "button", text: "Why", "aria-label": "Why: see Data gaps" });
  link.addEventListener("click", () => pickForecastTab("data-gaps"));
  const node = el("p", { class: "fc-warning fc-unavailable", role: "note" }, [text, " ", link]);
  node.dataset.forecastReason = kind;
  return node;
}

// The single status line when the current or with-arrivals forecast is
// unavailable; the reasons and evidence are in Data gaps.
function warningLine(scenario, hasBaseline) {
  const { info, source, coverage } = scenario;
  const missing = forecastCount(coverage.regression_history_missing);
  const lead = hasBaseline ? "Baseline only — " : "No forecast — ";
  const tasks = `${missing} task${missing === 1 ? "" : "s"}`;
  let text = `${lead}the regression-adjusted forecast is unavailable: ${info.label}.`;
  if (info.kind === "simulation_cut") {
    text = `${lead}${info.reasons[0].detail} Choose Current backlog for that forecast.`;
  } else if (info.kind === "regression_history_missing" && recordedPoolsContainGaps(source)) {
    text = `${lead}regression follow-up cannot be estimated because ${regressionGapBecause(source)}.`;
  } else if (info.kind === "regression_history_missing" && missing && regressionHistoryIsAbsent(source)) {
    text = `${lead}regression follow-up work isn't included because ${tasks} ${missing === 1 ? "has" : "have"} no regression history.`;
  } else if (info.kind === "regression_history_missing" && missing) {
    text = `${lead}regression follow-up cannot be estimated for ${tasks}.`;
  }
  return unavailableWarning(text, info.kind);
}

function proposedWarning(info, hasBaseline) {
  const reason = `If approved, the adjusted forecast is unavailable: ${info.label}.`;
  const limit = hasBaseline
    ? " The baseline shown for proposed work excludes unsupported regression follow-up work and is not the regression-adjusted forecast."
    : "";
  return unavailableWarning(`${reason}${limit}`, info.kind);
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

function emptyState(title, detail) {
  return el("div", { class: "mx-empty" }, [
    el("div", { class: "mx-empty-title", text: title }),
    el("div", { class: "mx-empty-detail", text: detail }),
  ]);
}

function totalsNodes(scenario) {
  const { source, info } = scenario;
  const inputs = view.payload.inputs || {};
  if (!(source.tasks ?? view.payload.forecast?.tasks)) return [emptyState("Nothing to forecast", "No backlog or in-progress tasks in this workspace.")];
  if (inputs.history_deliveries === 0) {
    return [emptyState("No delivery history to sample", inputs.history_tasks != null
      ? `None of the newest ${inputs.history_tasks} completed tasks has recorded agent runs.`
      : `No task finished with recorded agent runs in the last ${inputs.lookback_days} days or among the newest ${inputs.min_history_tasks ?? 50} completions. Widen the history.`)];
  }
  if (!info) {
    return [el("section", { class: "fc-figures", "aria-label": "Totals" }, adjustedTiles(scenario)), subsetNote(source)];
  }
  const hasBaseline = Boolean(forecastBaselineFigures(source.pre_regression_baseline));
  const figures = el("section", { class: hasBaseline ? "fc-figures fc-baseline" : "fc-figures", "aria-label": "Totals" }, withheldTiles(scenario));
  if (hasBaseline) figures.dataset.forecastBaseline = "pre-regression";
  return [
    figures,
    scenario.key === "proposed" ? proposedWarning(info, hasBaseline) : warningLine(scenario, hasBaseline),
    subsetNote(source),
  ];
}

// ---- tasks ----------------------------------------------------------------

/// Whether a task's cost or token range rests on too few measured deliveries.
export function forecastMetricThin(row, metric) {
  if (!row) return false;
  const flag = metric === "tokens" ? row.thin_tokens : row.thin_cost;
  if (typeof flag === "boolean") return flag;
  const count = num(metric === "tokens" ? row.token_samples : row.cost_samples);
  if (count == null || count === 0) return false;
  const min = num(view.payload?.inputs?.min_metric_samples) ?? num(view.payload?.inputs?.min_cell_samples) ?? 5;
  return count < min;
}

function metricSupportLine(row) {
  const parts = [];
  if (row.cost_samples != null) {
    const count = forecastCount(row.cost_samples);
    parts.push(`${count} cost sample${count === 1 ? "" : "s"}`);
  }
  if (row.token_samples != null) {
    const count = forecastCount(row.token_samples);
    parts.push(`${count} token sample${count === 1 ? "" : "s"}`);
  }
  if (!parts.length) return null;
  const thin = [];
  if (forecastMetricThin(row, "usd")) thin.push("cost");
  if (forecastMetricThin(row, "tokens")) thin.push("tokens");
  return thin.length ? `thin ${thin.join(" & ")} · ${parts.join(" · ")}` : parts.join(" · ");
}

function reviewAssumptionText(row) {
  if (row.review_mode == null && row.review_mode_fallback == null) return "—";
  if (row.review_mode === "unknown") return "Unknown assumption";
  const mode = REVIEW_MODE_LABELS[row.review_mode] || row.review_mode || "—";
  if (row.review_mode_fallback) return `${mode} · mode dropped`;
  if (row.basis === "review_complexity" || row.basis === "review_workspace") return `${mode} · mode kept`;
  return mode;
}

// What each one-line row keeps behind its info icon.
function taskEvidenceLines(row, review) {
  const reviewText = review ? reviewAssumptionText(row) : null;
  const sub = [row.complexity ? COMPLEXITY_LABELS[row.complexity] || row.complexity : null, reviewText && reviewText !== "—" ? reviewText : null].filter(Boolean).join(" · ");
  const order = !row.unforecastable && row.status !== "in-progress" && quantilePair(row.predicted_start_rank)
    ? `Start order ${formatForecastQuantileRange(row.predicted_start_rank, fmtRank)}`
    : null;
  return [
    sub,
    order,
    row.drain_hold?.detail,
    quantilePair(row.tokens) ? `${formatForecastQuantileRange(row.tokens, formatForecastTokens)} tokens` : null,
    metricSupportLine(row),
  ].filter(Boolean);
}

function taskCell(row, review) {
  const link = el("a", { class: "fc-task", text: row.title || "Untitled task", title: row.title || null });
  bindTaskLink(link, row.id);
  const lines = taskEvidenceLines(row, review);
  return el("th", { scope: "row" }, [
    el("span", { class: "fc-task-line" }, [link, lines.length ? pinnedInfoHint(lines.join("\n")) : null]),
  ]);
}

function startsCell(row) {
  const hold = row.drain_hold;
  if (hold) return el("td", { class: "fc-unforecastable", title: hold.detail, text: hold.kind === "scheduled" ? `starts ${hold.starts_at?.slice(0, 10) || "later"}` : `Excluded · ${hold.reason === "no_drain_tag" ? "no-drain" : hold.reason || "drain policy"}` });
  if (row.unforecastable) return el("td", { class: "fc-unforecastable", text: UNFORECASTABLE_LABELS[row.unforecastable] || row.unforecastable });
  if (row.status === "in-progress") return el("td", { class: "num", text: "running" });
  return el("td", { class: "num", text: formatForecastQuantileRange(row.predicted_start_hours, formatForecastHours) });
}

function costCell(row) {
  const thin = forecastMetricThin(row, "usd") || forecastMetricThin(row, "tokens");
  return el("td", { class: "num" }, [
    formatForecastQuantileRange(row.usd, formatForecastUsd),
    thin ? el("span", { class: "fc-thin", text: "thin" }) : null,
  ]);
}

function finishesCell(row) {
  if (row.unforecastable || row.drain_hold) return el("td", { class: "num", text: "—" });
  const share = num(row.completion_probability);
  return el("td", { class: share != null && share < 1 ? "num fc-finish-short" : "num", text: fmtShare(share) });
}

function tasksCard(scenario) {
  const predictions = Array.isArray(scenario.source?.task_predictions) ? scenario.source.task_predictions : [];
  if (!predictions.length) return null;
  const shown = predictions.slice(0, SCHEDULE_ROWS);
  const review = scheduleShowsReview(shown);
  const rows = shown.map((row) => el("tr", {}, [
    taskCell(row, review),
    el("td", { class: "fc-state" }, [statusPill(row.status)]),
    startsCell(row),
    el("td", { class: "num", text: formatForecastQuantileRange(row.work_hours, formatForecastHours) }),
    costCell(row),
    finishesCell(row),
  ]));
  const count = predictions.length > shown.length ? `First ${shown.length} of ${predictions.length}` : `${predictions.length} task${predictions.length === 1 ? "" : "s"}`;
  const head = [["Task"], ["Status"], ["Starts in", true], ["Work hours", true], ["Cost", true], ["Finishes", true]];
  return forecastCard("Tasks", `${count} in dispatch order. Ranges are p50 – p90.`, [forecastTable("Forecast tasks", head, rows)], "fc-tasks");
}

// ---- waits ----------------------------------------------------------------

function waitsSection(scenario) {
  const waits = scenario.source?.wait_hours;
  if (!waits || !scenario.source?.tasks) return null;
  const items = WAITS.map(([key, label, hint]) => el("div", { class: "fc-wait" }, [
    el("dt", {}, [label, pinnedInfoHint(hint)]),
    el("dd", { class: "fc-wait-value", text: formatForecastQuantileRange(waits[key], formatForecastHours) }),
  ]));
  return forecastMoreSection("waits", "Wait breakdown", "task-hours, p50 – p90", [
    el("p", { class: "mx-blurb", text: "Summed over every waiting task, so hours add up across tasks and exceed the time to drain. A task counts under the first gate it fails." }),
    el("dl", { class: "fc-waits" }, items),
  ]);
}

// ---- forecast tab ---------------------------------------------------------

function forecastPanelNodes(scenario) {
  const scope = forecastDrainScope(scenario.source);
  return [
    el("div", { class: "fc-scenario-row" }, [scenarioSwitch(), pinnedInfoHint(arrivalsText())]),
    ...totalsNodes(scenario),
    scope.lines.length ? forecastCard("Outside this drain", pinnedInfoHint("Time to drain covers startable work. These tasks retain their own work, token and cost estimates; their regression follow-up is outside this drain."), scope.lines.map((line) => el("p", { class: "mx-blurb", text: line }))) : null,
    tasksCard(scenario),
    el("div", { class: "fc-more-list" }, [waitsSection(scenario)]),
  ];
}
