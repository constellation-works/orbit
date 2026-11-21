// Orbit dashboard Analytics › Metrics (ORB-15746): implement_one duration,
// task completion, defect, tokens-per-task, turns-to-first-edit,
// lines-changed and delivery-friction trends over the shared dashboard
// window, with breakdowns for the selected trend beneath. The tokens,
// turns-to-first-edit, lines-changed and delivery-friction trends keep
// their filters and models in metrics-tokens.js, metrics-turns.js,
// metrics-diff.js and metrics-friction.js. The count trends (Tasks completed,
// Defects, Tokens and Lines changed) also offer Per bucket, Cumulative and Per
// day views, derived in metrics-running.js.
// Pure vanilla JS, split into ES modules with no build step.
//
// The active tab owns its trend and breakdown requests. Polls refresh that
// tab, and filter changes refresh only the endpoint they affect.
// Statistic, chart/table and series changes re-render locally. Hover moves
// the tooltip in place.

import {
  el, syncNodes, fetchJson, getWindow, setWindow, persistScopeToUrl, syncWindowSelectors,
  notifyScopeChange, wireWindowSelector, requestPanel, getWorkspaceRevision,
  onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime, formatClock, getClockZone,
  DASHBOARD_WINDOWS, pinnedInfoHint,
} from './common.js';
import { linePath, svg } from './sparkline.js';
import { fetchTokens, isCurrentTokensPayload, tokenSpendModel, tokensModel, tokensQuery, tokensTools } from './metrics-tokens.js';
import { TERMINAL_TIME_NOTE, availableCountViews, countViewHash, countViewsFromQuery, delta, fmtMs, isCountTrend, previousLabel, resolveCountView, runningModel, undatedNote } from './metrics-running.js';
import { fetchTurns, isCurrentTurnsPayload, turnsModel, turnsQuery, turnsTools, turnsView } from './metrics-turns.js';
import { diffModel, diffQuery, diffTools, diffView, fetchDiff, isCurrentDiffPayload } from './metrics-diff.js';
import { fetchFriction, frictionModel, frictionQuery, frictionTools, frictionView, isCurrentFrictionPayload } from './metrics-friction.js';
import { metricsPanelRows } from './metrics-panels.js';

const $ = (id) => document.getElementById(id);

// A bucket or actor with fewer runs than this is drawn hollow or dimmed: its
// average is one or two runs, not a trend.
const LOW_SAMPLE = 3;

const SERIES = [
  { key: "all", label: "All runs", doneLabel: "Done (total)", tag: "" },
  { key: "low", label: "Low", doneLabel: "Low", tag: "low" },
  { key: "medium", label: "Medium", doneLabel: "Medium", tag: "medium" },
  { key: "hard", label: "Hard", doneLabel: "Hard", tag: "hard" },
];
// Stacked bottom to top; `other` is xhard plus unset.
const STACK = ["low", "medium", "hard", "other"];
const STATS = [["avg", "Avg"], ["p50", "p50"], ["p95", "p95"]];
// The filtered trends that refetch on their own: each one's fenced fetch and
// currency test, keyed by its field in `lastPayload`.
const TREND_SOURCES = {
  tokens: { fetch: fetchTokens, isCurrent: isCurrentTokensPayload },
  turns: { fetch: fetchTurns, isCurrent: isCurrentTurnsPayload },
  diff: { fetch: fetchDiff, isCurrent: isCurrentDiffPayload },
  friction: { fetch: fetchFriction, isCurrent: isCurrentFrictionPayload },
};

const TRENDS = [["duration", "Implementation time"], ["completed", "Tasks completed"], ["defects", "Defects"], ["tokens", "Tokens per task"], ["tokens-total", "Tokens"], ["turns", "Turns to first edit"], ["diff", "Lines changed"], ["friction", "Delivery friction"]];
const DEFECT_GROUPS = [["lineage", "Lineage"], ["origin", "Origin"]];
// Lineage: what a bug's `regression_from` relations name as its cause.
const LINEAGE_SERIES = [
  { key: "fix_for_fix", label: "Fix-for-a-fix" },
  { key: "regression", label: "Other regression" },
  { key: "unattributed", label: "Unattributed" },
];
// Origin: the finder that filed the bug. The chart keeps seven named finders
// and folds the rest into Other; the table lists every origin the API sends.
const ORIGIN_LABELS = {
  "code-review": "Code review",
  "full-code-review": "Full code review",
  "delivery-code-review": "Delivery review",
  "qa-sweep": "QA sweep",
  "qa-full-sweep": "Full QA sweep",
  "security-review": "Security review",
  "ci-failure-sweep": "CI failure sweep",
  "scanning-sweep": "Scanning sweeps",
  "review-reconciliation": "Review reconciliation",
  "friction-curation": "Friction curation",
  other: "Untagged",
};
const ORIGIN_CHART = ["code-review", "full-code-review", "delivery-code-review", "qa-sweep", "qa-full-sweep", "security-review", "ci-failure-sweep"];
const MODES = [["chart", "Chart"], ["table", "Table"]];
const OUTCOMES = [["succeeded", "Succeeded"], ["all", "All"]];

const view = {
  outcome: "succeeded",
  trend: "duration",
  group: "lineage",
  stat: "avg",
  mode: "chart",
  shown: {
    duration: { all: true, low: true, medium: true, hard: true },
    completed: { all: true, low: true, medium: true, hard: true },
    "defects-lineage": { all: true, fix_for_fix: true, regression: true, unattributed: true },
    "defects-origin": Object.fromEntries(["all", ...ORIGIN_CHART, "other"].map((key) => [key, true])),
    // Series the API sends vary with the tag filter; absent keys are shown.
    tokens: {},
    "tokens-total": {},
    turns: {},
    // Lines changed: line kinds, or crews (which vary with the window).
    "diff-lines": {},
    "diff-crew": {},
    // Delivery friction: the families the picker fetched.
    friction: {},
  },
  // The Per bucket | Cumulative | Per day view each count trend was last given,
  // by trend; the hash query remembers it.
  countViews: {},
  hover: null,
};
let lastPayload = null;
let metricsRequestScope = null;

function wireMetricsWindowSelector() {
  wireWindowSelector("metrics-window-selector", { allowAll: true });
}

function clearMetrics() {
  lastPayload = null;
  view.hover = null;
}

export function placeholdMetricsAggregate() {
  clearMetrics();
  renderPanelPlaceholder("metrics-body");
}

async function fetchMetric(path, selectedWindow, notYet) {
  try {
    const payload = await fetchJson(path);
    if (!payload || (selectedWindow && windowKey(payload) !== selectedWindow)) {
      return { unavailable: "The host did not return metrics for the selected window." };
    }
    return payload;
  } catch (error) {
    return { unavailable: error.status === 404 && notYet ? notYet : error.message };
  }
}

function metricRequests(trend, selectedWindow, outcome) {
  const since = `since=${encodeURIComponent(selectedWindow)}`;
  const completions = () => fetchMetric(`/api/diagnostics/completions?${since}`, selectedWindow);
  switch (trend) {
    case "duration":
      return { durations: fetchMetric(`/api/diagnostics/implement_one?${since}&outcome=${outcome}`, selectedWindow)
        .then((payload) => payload.unavailable || payload.outcome === outcome
          ? payload : { unavailable: "The host did not return metrics for the selected run outcome." }) };
    case "completed":
      return { completions: completions(), outcome: fetchMetric("/api/tasks/completion-by-complexity") };
    case "defects":
      return { defects: fetchMetric(`/api/diagnostics/defects?${since}`, selectedWindow,
        "This host's Orbit does not report defects yet. Upgrade it to see this trend."), completions: completions() };
    default:
      return { [trend]: TREND_SOURCES[trend].fetch(selectedWindow) };
  }
}

export function fetchAndRenderMetrics() {
  const hashQuery = new URLSearchParams(window.location.hash.split("?")[1] || "");
  view.outcome = hashQuery.get("outcome") === "all" ? "all" : "succeeded";
  view.countViews = countViewsFromQuery(hashQuery);
  wireMetricsWindowSelector();
  if (isAggregateView()) {
    placeholdMetricsAggregate();
    return Promise.resolve();
  }
  const selectedWindow = getWindow();
  const selectedTrend = view.trend;
  const trend = activeTrendSource();
  const outcome = view.outcome;
  const scope = JSON.stringify([getWorkspaceRevision(), selectedWindow, outcome]);
  if (metricsRequestScope !== scope) {
    metricsRequestScope = scope;
    clearMetrics();
  }
  return requestPanel("metrics-body", scope, async () => {
    const requests = Object.entries(metricRequests(trend, selectedWindow, outcome));
    return Promise.all(requests.map(async ([key, request]) => [key, await request]));
  }, (entries) => {
    if (selectedTrend !== view.trend) return;
    lastPayload ??= { windowKey: selectedWindow };
    for (const [key, payload] of entries) {
      if (!TREND_SOURCES[key] || TREND_SOURCES[key].isCurrent(payload, selectedWindow)) {
        lastPayload[key] = checkedTrendWindow(payload, selectedWindow, key);
      }
    }
    renderMetrics();
  });
}

onWorkspaceChange(() => {
  metricsRequestScope = null;
  clearMetrics();
});

function windowKey(payload) {
  return payload && payload.window ? payload.window.key : null;
}

function checkedTrendWindow(payload, selectedWindow, key) {
  if (!TREND_SOURCES[key] || payload.unavailable || windowKey(payload) === selectedWindow) return payload;
  return { unavailable: "The host did not return metrics for the selected window." };
}

export function renderMetrics() {
  const body = $("metrics-body");
  if (!body) return;
  if (isAggregateView()) {
    placeholdMetricsAggregate();
    return;
  }
  if (!lastPayload) return;
  const p = lastPayload;
  const { hover, ...trendState } = view;
  syncNodes(body, [
    keyed(trendSection(), "metrics-trend", [
      trendState, p.windowKey, p.durations?.buckets, p.durations?.current, p.durations?.previous, p.durations?.unavailable,
      p.completions, p.defects, p.tokens, tokensQuery(p.windowKey), p.turns, turnsQuery(p.windowKey), turnsView,
      p.diff, diffQuery(p.windowKey), diffView, p.friction, frictionQuery(p.windowKey), frictionView,
    ]),
    ...metricsPanelRows(view.trend, p, rerender, { originLabels: ORIGIN_LABELS, lineageSeries: LINEAGE_SERIES }).map(([key, node, data]) => keyed(node, key, data)),
  ]);
}

function keyed(node, key, data) {
  node.dataset.key = key;
  node.dataset.hash = JSON.stringify(data, (key, value) => key === "trendFence" ? undefined : value);
  return node;
}

function rerender() {
  renderMetrics();
}

// ---- formatting -----------------------------------------------------------

function axisLabel(minutes) {
  if (minutes === 0) return "0";
  if (minutes < 60) return `${minutes}m`;
  if (minutes % 60 === 0) return `${minutes / 60}h`;
  return `${Math.floor(minutes / 60)}h${minutes % 60}`;
}

const MINUTE_SCALES = [[30, 10], [45, 15], [60, 15], [90, 30], [120, 30], [150, 30], [180, 60], [240, 60], [300, 60]];
const COUNT_SCALES = [[5, 1], [10, 2], [15, 5], [20, 5], [25, 5], [30, 10], [40, 10], [50, 10], [60, 20], [80, 20], [100, 20], [120, 40], [160, 40], [200, 50], [250, 50], [300, 100], [400, 100]];

function niceScale(value, ladder, coarse) {
  for (const [max, step] of ladder) if (value <= max) return { max, step };
  return { max: Math.ceil(value / coarse) * coarse, step: coarse };
}

function bucketWidthLabel(seconds) {
  const names = { 300: "5-minute", 3600: "1-hour", 21600: "6-hour", 86400: "1-day (UTC)", 604800: "1-week (UTC, from Monday)" };
  if (seconds > 604800 && seconds % 604800 === 0) return `${seconds / 604800}-week (UTC, from Monday) buckets`;
  return `${names[seconds] || `${seconds}s`} buckets`;
}

function perBucketUnit(seconds) {
  if (seconds > 604800 && seconds % 604800 === 0) return `${seconds / 604800} weeks`;
  return { 300: "5 min", 3600: "hour", 21600: "6h", 86400: "day", 604800: "week" }[seconds] || "bucket";
}

// Buckets of 6h and wider start on UTC boundaries, so they are named by UTC
// date. Narrower ones follow the dashboard clock (UTC unless Local is
// selected) and omit the zone abbreviation; the axis has no room for it.
function utcDate(value) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" });
}

function tickLabel(start, seconds) {
  return seconds < 21600 ? formatClock(start, { seconds: false, zone: false }) : utcDate(start);
}

/// Which buckets carry an x-axis label: round clock times where the width
/// allows, else every few buckets counted back from the newest.
function tickParts(start) {
  const date = new Date(start);
  return getClockZone() === "utc"
    ? { minutes: date.getUTCMinutes(), hours: date.getUTCHours() }
    : { minutes: date.getMinutes(), hours: date.getHours() };
}

function isTick(start, seconds, index, count) {
  const parts = tickParts(start);
  if (seconds === 300) return parts.minutes % 10 === 0;
  if (seconds === 3600) return parts.hours % 4 === 0;
  if (seconds === 21600) return new Date(start).getUTCHours() === 0;
  const every = seconds === 86400 ? 5 : Math.max(1, Math.ceil(count / 7));
  return (count - 1 - index) % every === 0;
}

function bucketTitle(start, seconds, last) {
  const end = new Date(new Date(start).getTime() + seconds * 1000);
  let range;
  if (seconds === 86400) range = `${utcDate(start)} (UTC day)`;
  else if (seconds > 86400) range = `${utcDate(start)}–${utcDate(end.getTime() - 1)} (UTC)`;
  else range = `${formatDateTime(start)}–${formatClock(end, { seconds: false, zone: false })}`;
  return last ? `${range} · in progress` : range;
}

// ---- trend section --------------------------------------------------------

function segGroup(label, items, current, onPick) {
  return el("div", { class: "mx-seg", role: "group", "aria-label": label }, items.map(([key, text]) => {
    const button = el("button", { class: key === current ? "mx-seg-btn on" : "mx-seg-btn", type: "button", text, "aria-pressed": key === current ? "true" : "false" });
    button.addEventListener("click", () => onPick(key));
    return button;
  }));
}

function trendTabs() {
  const list = el("div", { class: "mx-trend-tabs", role: "tablist", "aria-label": "Trend" });
  const tabs = TRENDS.map(([key, text]) => {
    const on = key === view.trend;
    const tab = el("button", { class: on ? "mx-ttab on" : "mx-ttab", type: "button", role: "tab", text, "aria-selected": on ? "true" : "false", "aria-controls": "metrics-trend-view" });
    tab.dataset.trend = key;
    tab.tabIndex = on ? 0 : -1;
    tab.addEventListener("click", () => pickTrend(key));
    return tab;
  });
  list.addEventListener("keydown", (event) => {
    if (event.key !== "ArrowRight" && event.key !== "ArrowLeft") return;
    event.preventDefault();
    const index = TRENDS.findIndex(([key]) => key === view.trend);
    const next = TRENDS[(index + (event.key === "ArrowRight" ? 1 : TRENDS.length - 1)) % TRENDS.length][0];
    pickTrend(next);
    const focus = $("metrics-body") && $("metrics-body").querySelector(`.mx-ttab[data-trend="${next}"]`);
    if (focus) focus.focus();
  });
  for (const tab of tabs) list.appendChild(tab);
  return list;
}

function pickTrend(key) {
  if (view.trend === key) return;
  view.trend = key;
  view.hover = null;
  if (lastPayload) {
    lastPayload = { windowKey: lastPayload.windowKey };
    rerender();
  }
  fetchAndRenderMetrics();
}

function activeTrendSource() {
  return view.trend === "tokens-total" ? "tokens" : view.trend;
}

// The series-visibility key for the current trend; each defect grouping keeps
// its own.
function shownKey() {
  if (view.trend === "diff") return `diff-${diffView.group}`;
  return view.trend === "defects" ? `defects-${view.group}` : view.trend;
}

// A filtered trend's refetch after a filter change: `null` clears the chart to
// loading until the refetch answers and `undefined` only redraws the controls.
// A reply whose fence (visit, window, filter query, generation) is stale is
// dropped, unavailable and error replies included. The turns measure, diff
// grouping and friction measure toggles do not come through here; they redraw
// from their view state.
function applyTrend(key, payload) {
  if (!lastPayload || key !== activeTrendSource()) return;
  if (payload === null) {
    lastPayload[key] = null;
    view.hover = null;
  } else if (payload !== undefined) {
    if (!TREND_SOURCES[key].isCurrent(payload, lastPayload.windowKey)) return;
    lastPayload[key] = checkedTrendWindow(payload, lastPayload.windowKey, key);
    view.hover = null;
  }
  rerender();
}

// The payload whose bucket width decides which count views a trend offers.
function countPayload() {
  return { completed: lastPayload.completions, defects: lastPayload.defects, "tokens-total": lastPayload.tokens, diff: lastPayload.diff }[view.trend];
}

// The count view on screen for the selected trend, or null for a per-run
// trend: the one the hash remembers if the trend can draw it, else the
// window's default.
function activeCountView() {
  if (!isCountTrend(view.trend)) return null;
  const payload = countPayload();
  const seconds = payload && !payload.unavailable && payload.window ? payload.window.bucket_seconds : null;
  return { seconds, mode: resolveCountView(view.countViews[view.trend], lastPayload.windowKey, seconds) };
}

// A view change redraws locally and rewrites the address in place: it is not
// a route, so nothing refetches.
function pickCountView(mode) {
  view.countViews[view.trend] = mode;
  view.hover = null;
  window.history.replaceState(null, "", countViewHash(window.location.hash, view.trend, mode));
  rerender();
}

function trendSection() {
  const isDuration = view.trend === "duration";
  const isDefects = view.trend === "defects";
  const isTokens = view.trend === "tokens";
  const isTokenSpend = view.trend === "tokens-total";
  const count = activeCountView();
  const isTurns = view.trend === "turns";
  const isDiff = view.trend === "diff";
  const isFriction = view.trend === "friction";
  const buildModel = () => isDuration ? durationModel()
    : isDefects ? defectsModel(count.mode)
      : isTokens ? tokensModel(lastPayload.tokens, { windowKey: lastPayload.windowKey, shown: view.shown.tokens, bucketTitle, bucketWidthLabel })
        : isTokenSpend ? tokenSpendModel(lastPayload.tokens, { windowKey: lastPayload.windowKey, shown: view.shown["tokens-total"], countView: count.mode, bucketTitle, bucketWidthLabel })
        : isTurns ? turnsModel(lastPayload.turns, { windowKey: lastPayload.windowKey, shown: view.shown.turns, bucketTitle, bucketWidthLabel })
          : isDiff ? diffModel(lastPayload.diff, { windowKey: lastPayload.windowKey, shown: view.shown[shownKey()], countView: count.mode, bucketTitle, bucketWidthLabel })
            : isFriction ? frictionModel(lastPayload.friction, { windowKey: lastPayload.windowKey, shown: view.shown.friction, bucketTitle, bucketWidthLabel })
              : completedModel(count && count.mode);
  let model = buildModel();
  // The chart always keeps one series. A switch of count view, grouping or
  // family can leave every remaining card hidden (each card's guard sees only
  // the cards on screen), so show them all again rather than draw nothing.
  const toggles = (model.cards || []).flatMap((card) => card.grouped ? card.entries : card.static ? [] : [card]);
  if (toggles.length && !toggles.some((card) => card.on)) {
    view.shown[shownKey()] = { ...view.shown[shownKey()], ...Object.fromEntries(toggles.map((card) => [card.key, true])) };
    model = buildModel();
  }
  const tools = el("div", { class: "mx-trend-tools" }, [
    isDuration ? segGroup("Run outcome", OUTCOMES, view.outcome, (key) => {
      if (key === view.outcome) return;
      const query = new URLSearchParams(window.location.hash.split("?")[1] || "");
      query.set("outcome", key);
      view.hover = null;
      window.location.hash = `#analytics/metrics?${query}`;
    }) : null,
    isDuration ? segGroup("Statistic", STATS, view.stat, (key) => { view.stat = key; rerender(); }) : null,
    isDefects ? segGroup("Group by", DEFECT_GROUPS, view.group, (key) => { view.group = key; view.hover = null; rerender(); }) : null,
    ...(isTokens || isTokenSpend ? tokensTools({ segGroup, windowKey: lastPayload.windowKey, payload: lastPayload.tokens, onChange: (payload) => applyTrend("tokens", payload) }) : []),
    ...(isTurns ? turnsTools({ segGroup, windowKey: lastPayload.windowKey, payload: lastPayload.turns, rerender: () => { view.hover = null; rerender(); }, onChange: (payload) => applyTrend("turns", payload) }) : []),
    ...(isDiff ? diffTools({ segGroup, countView: count.mode, windowKey: lastPayload.windowKey, rerender: () => { view.hover = null; rerender(); }, onChange: (payload) => applyTrend("diff", payload) }) : []),
    ...(isFriction ? frictionTools({ segGroup, windowKey: lastPayload.windowKey, payload: lastPayload.friction, rerender: () => { view.hover = null; rerender(); }, onChange: (payload) => applyTrend("friction", payload) }) : []),
    count ? segGroup("Count view", availableCountViews(count.seconds), count.mode, pickCountView) : null,
    segGroup("View", MODES, view.mode, (key) => { view.mode = key; view.hover = null; rerender(); }),
    // What the trend counts sits behind an info icon, not in the working view.
    pinnedInfoHint([
      model.blurb,
      isDuration ? (view.outcome === "succeeded"
        ? "Counted: final implementation attempts in successful runs with a committed candidate. Failed, cancelled, no-diff and unknown outcomes are excluded."
        : "Counted: all implementation attempts, including failed, cancelled, no-diff and unknown outcomes.") : null,
    ].filter(Boolean).join("\n")),
  ]);
  const panel = el("div", { class: "mx-trend-view", id: "metrics-trend-view", role: "tabpanel" }, [
    seriesCards(model.cards),
    model.empty ? emptyTrend(model.empty) : (view.mode === "table" ? trendTable(model.table) : trendChart(model.chart)),
  ]);
  return el("section", { class: "mx-card mx-trends", "aria-label": "Trends" }, [
    el("div", { class: "mx-trend-head" }, [trendTabs(), tools]),
    panel,
  ]);
}

function groupedSeriesCard(card) {
  const rows = card.entries.map((entry) => {
    const button = el("button", {
      class: `mx-series-row mx-s-${entry.swatch || entry.key}${entry.on ? " on" : " off"}${entry.dashed ? " dashed" : ""}`,
      type: "button",
      "aria-pressed": entry.on ? "true" : "false",
      "aria-description": entry.on ? `Hide ${entry.label}` : `Show ${entry.label}`,
    }, [
      el("span", { class: "mx-series-row-head" }, [
        el("span", { class: `mx-swatch${entry.dashed ? " dashed" : ""}`, "aria-hidden": "true" }),
        el("span", { text: entry.label }),
      ]),
      el("span", { class: "mx-series-row-value", text: entry.value }),
      el("span", { class: "mx-series-row-sub", text: entry.sub }),
      el("span", { class: "mx-series-row-foot", text: entry.foot }),
    ]);
    button.addEventListener("click", () => {
      const shown = view.shown[shownKey()];
      if (!card.entries.some((other) => (other.key === entry.key ? !entry.on : other.on))) return;
      view.shown[shownKey()] = { ...shown, [entry.key]: !entry.on };
      rerender();
    });
    return button;
  });
  return el("div", { class: "mx-sc mx-sc-static mx-sc-breakdown", role: "group", "aria-label": card.label }, [
    el("span", { class: "mx-sc-label", text: card.label }),
    el("div", { class: "mx-sc-breakdown-list" }, rows),
  ]);
}

function seriesCards(cards) {
  return el("div", { class: "mx-series", role: "group", "aria-label": "Series" }, cards.map((card) => {
    if (card.grouped) return groupedSeriesCard(card);
    // A figure derived from the series, not a series: no toggle.
    if (card.static) {
      return el("div", { class: "mx-sc mx-sc-static", role: "group", "aria-label": card.label }, [
        el("span", { class: "mx-sc-label", text: card.label }),
        el("span", { class: "mx-sc-value", text: card.value }),
        el("span", { class: "mx-sc-delta" }, [el("span", { class: "mx-sc-sub", text: card.sub })]),
        el("span", { class: "mx-sc-foot", text: card.foot }),
      ]);
    }
    const button = el("button", {
      class: `mx-sc mx-s-${card.swatch || card.key}${card.on ? " on" : " off"}`,
      type: "button",
      "aria-pressed": card.on ? "true" : "false",
      "aria-description": card.on ? `Hide ${card.label}` : `Show ${card.label}`,
    }, [
      el("span", { class: "mx-sc-label" }, [
        el("span", { class: `mx-swatch${card.bar ? " bar" : ""}${card.dashed ? " dashed" : ""}`, "aria-hidden": "true" }),
        card.label,
        card.tag && card.tag !== card.label.toLowerCase() ? el("span", { class: "mx-sc-tag", text: card.tag }) : null,
      ]),
      card.value != null ? el("span", { class: "mx-sc-value", text: card.value }) : null,
      card.delta || card.sub ? el("span", { class: "mx-sc-delta" }, [
        card.delta ? el("span", { class: card.delta.cls, text: card.delta.text }) : null,
        card.sub ? el("span", { class: "mx-sc-sub", text: card.sub }) : null,
      ]) : null,
      card.foot ? el("span", { class: "mx-sc-foot", text: card.foot }) : null,
    ]);
    button.addEventListener("click", () => {
      const shown = view.shown[shownKey()];
      const next = { ...shown, [card.key]: !card.on };
      // The chart always keeps one series.
      if (!cards.some((other) => (other.key === card.key ? !card.on : other.on))) return;
      view.shown[shownKey()] = next;
      rerender();
    });
    return button;
  }));
}

function pendingTrend(title, blurb, payload) {
  return {
    blurb, cards: [], empty: {
      title: payload ? `${title} unavailable` : `Loading ${title.toLowerCase()}…`,
      detail: payload?.unavailable,
      noWiden: true,
    },
  };
}

function emptyTrend({ title, detail, noWiden }) {
  const box = el("div", { class: "mx-empty" }, [
    el("div", { class: "mx-empty-title", text: title }),
    detail ? el("div", { class: "mx-empty-detail", text: detail }) : null,
  ]);
  const index = DASHBOARD_WINDOWS.indexOf(lastPayload.windowKey);
  const wider = index >= 0 && !noWiden ? DASHBOARD_WINDOWS[index + 1] : null;
  if (wider) {
    const widen = el("button", { class: "mx-link-btn", type: "button", text: `Widen to ${wider}` });
    widen.addEventListener("click", () => {
      setWindow(wider);
      persistScopeToUrl();
      syncWindowSelectors();
      notifyScopeChange();
    });
    box.appendChild(widen);
  }
  return box;
}

function durationModel() {
  const payload = lastPayload.durations;
  if (!payload || payload.unavailable) return pendingTrend("Implementation time", "Wall-clock time for implementation runs.", payload);
  const windowKeyValue = lastPayload.windowKey;
  const buckets = payload.buckets || [];
  const seconds = payload.window.bucket_seconds;
  const stat = view.stat;
  const statLabel = STATS.find(([key]) => key === stat)[1];
  const shown = view.shown.duration;
  const prevText = previousLabel(windowKeyValue);
  const current = payload.current || {};
  const previous = payload.previous;

  const cards = SERIES.map((series) => {
    const cur = current[series.key];
    const prev = previous ? previous[series.key] : null;
    return {
      key: series.key,
      label: series.label,
      tag: series.tag,
      on: !!shown[series.key],
      value: cur ? fmtMs(cur[stat]) : "—",
      delta: previous ? delta(cur ? cur[stat] : null, prev ? prev[stat] : null, "duration") : null,
      sub: previous ? `vs ${prevText} ${prev ? fmtMs(prev[stat]) : "—"}` : `window ${windowKeyValue}`,
      foot: `${cur ? cur.n : 0} runs`,
    };
  });

  const blurb = `${statLabel} wall-clock time of implementation runs that finished in each bucket, by task complexity.`;
  if (!current.all) {
    return {
      blurb,
      cards,
      empty: {
        title: `No implementation runs finished in the last ${windowKeyValue === "all" ? "recorded history" : windowKeyValue}`,
        detail: payload.truncated ? "The read hit its row cap." : "",
      },
    };
  }

  const visible = SERIES.filter((series) => shown[series.key]);
  let maxMinutes = 0;
  for (const bucket of buckets) {
    for (const series of visible) {
      const cell = bucket[series.key];
      if (cell) maxMinutes = Math.max(maxMinutes, cell[stat] / 60000);
    }
  }
  const prevAll = previous && previous.all ? previous.all[stat] : null;
  const showPrev = shown.all && prevAll != null;
  if (showPrev) maxMinutes = Math.max(maxMinutes, prevAll / 60000);
  const scale = niceScale(Math.max(maxMinutes, 1) * 1.08, MINUTE_SCALES, 60);
  const y = (ms) => 100 - Math.min(ms / 60000, scale.max) / scale.max * 100;

  const yTicks = [];
  for (let v = 0; v <= scale.max; v += scale.step) yTicks.push({ top: 100 - v / scale.max * 100, label: axisLabel(v) });

  let volumeMax = 0;
  for (const bucket of buckets) volumeMax = Math.max(volumeMax, bucket.all ? bucket.all.n : 0);
  const otherRuns = (current.xhard ? current.xhard.n : 0) + (current.unset ? current.unset.n : 0);

  const notes = [
    `${bucketWidthLabel(seconds)}, aligned to the clock`,
    "gap = no runs finished",
    "runs bars stack low · medium · hard · other",
  ];
  if (otherRuns) notes.push(`xhard + unset (${otherRuns} runs) count toward All only`);
  if (payload.truncated) notes.push("read hit its row cap: oldest runs missing");

  return {
    blurb,
    cards,
    chart: {
      kind: "duration",
      buckets,
      seconds,
      yTicks,
      lines: visible.map((series) => ({
        key: series.key,
        points: buckets.map((bucket, i) => (bucket[series.key] ? { i, y: y(bucket[series.key][stat]), hollow: bucket[series.key].n < LOW_SAMPLE } : null)),
      })),
      prev: showPrev ? { top: y(prevAll), label: `All · ${prevText} ${statLabel.toLowerCase()} ${fmtMs(prevAll)}` } : null,
      volume: { max: volumeMax, stacks: buckets.map((bucket) => STACK.map((key) => (key === "other" ? bucket.other_runs || 0 : (bucket[key] ? bucket[key].n : 0)))) },
      notes,
      hollowNote: true,
      aria: `Implementation ${statLabel.toLowerCase()} time by complexity, window ${windowKeyValue}. ${visible.map((series) => `${series.label} ${current[series.key] ? fmtMs(current[series.key][stat]) : "no runs"}`).join(", ")}. Use the Table view for per-bucket values.`,
      tip: (index) => {
        const bucket = buckets[index];
        const tip = { title: bucketTitle(bucket.start, seconds, index === buckets.length - 1), rows: [], foot: `${statLabel} per series · ×n = runs` };
        if (!bucket.all) {
          tip.empty = "No implementation runs finished in this bucket.";
          return tip;
        }
        for (const series of SERIES) {
          const cell = bucket[series.key];
          if (!cell) continue;
          tip.rows.push({ key: series.key, name: series.label, n: `×${cell.n}`, low: cell.n < LOW_SAMPLE, value: fmtMs(cell[stat]), strong: series.key === "all" });
        }
        if (tip.rows.some((row) => row.low)) tip.warn = `Amber counts: fewer than ${LOW_SAMPLE} runs.`;
        if (bucket.other_runs) tip.extra = `xhard / unset: ${bucket.other_runs} runs (in All only)`;
        return tip;
      },
    },
    table: {
      caption: `${statLabel} per bucket, runs in parentheses · ${bucketWidthLabel(seconds)} · newest first`,
      heads: ["Runs", "All", "Low", "Medium", "Hard"],
      rows: buckets.map((bucket, i) => ({
        title: bucketTitle(bucket.start, seconds, i === buckets.length - 1),
        cells: [String(bucket.all ? bucket.all.n : 0), ...SERIES.map((series) => (bucket[series.key] ? `${fmtMs(bucket[series.key][stat])} (${bucket[series.key].n})` : null))],
      })).reverse(),
    },
  };
}

function doneCount(counts, key) {
  return key === "all" ? counts.done.total : counts.done[key];
}

function completedModel(countView) {
  const payload = lastPayload.completions;
  if (!payload || payload.unavailable) return pendingTrend("Tasks completed", "Tasks that moved to done in each bucket, by complexity.", payload);
  const windowKeyValue = lastPayload.windowKey;
  const buckets = payload.buckets || [];
  const seconds = payload.window.bucket_seconds;
  const shown = view.shown.completed;
  const current = payload.current;
  const previous = payload.previous;
  const prevText = previousLabel(windowKeyValue);
  const unit = perBucketUnit(seconds);

  const cards = SERIES.map((series) => {
    const isTotal = series.key === "all";
    const cur = doneCount(current, series.key);
    const prev = previous ? doneCount(previous, series.key) : null;
    return {
      key: series.key,
      label: series.doneLabel,
      tag: series.tag,
      bar: !isTotal,
      on: !!shown[series.key],
      value: isTotal ? String(cur) : null,
      delta: isTotal && previous ? delta(cur, prev, "count") : null,
      sub: isTotal && previous ? `vs ${prevText} ${prev}` : null,
      foot: !isTotal && current.done.total ? `${Math.round(cur / current.done.total * 100)}% of done` : null,
    };
  });

  const blurb = "Tasks that moved to done in each bucket, by complexity.";
  const undated = undatedNote(payload.undated && payload.undated.done.total);
  if (!current.done.total && !current.rejected && !current.archived) {
    return { blurb, cards, empty: { title: `No tasks closed in the last ${windowKeyValue === "all" ? "recorded history" : windowKeyValue}`, detail: undated || "" } };
  }

  if (countView !== "bucket") {
    const lead = payload.lead_buckets || [];
    return {
      blurb,
      cards,
      ...runningModel({
        mode: countView,
        kind: "completed",
        windowKey: windowKeyValue,
        seconds,
        starts: buckets.map((bucket) => bucket.start),
        series: SERIES.map((series) => ({
          key: series.key,
          swatch: series.key,
          label: series.doneLabel,
          strong: series.key === "all",
          on: !!shown[series.key],
          values: buckets.map((bucket) => doneCount(bucket, series.key)),
          lead: lead.map((bucket) => doneCount(bucket, series.key)),
        })),
        fmt: String,
        noun: "tasks",
        subject: "Tasks moved to done",
        notes: [TERMINAL_TIME_NOTE, "xhard and unset count toward the total only", undated].filter(Boolean),
        bucketTitle,
        bucketWidthLabel,
      }),
    };
  }

  const stackValue = (bucket) => STACK.reduce((sum, key) => sum + (key === "other" ? (shown.all ? bucket.done.other : 0) : (shown[key] ? bucket.done[key] : 0)), 0);
  let max = 0;
  for (const bucket of buckets) max = Math.max(max, shown.all ? bucket.done.total : stackValue(bucket));
  const prevPer = previous && buckets.length ? previous.done.total / buckets.length : null;
  const showPrev = shown.all && prevPer != null && previous.done.total > 0;
  if (showPrev) max = Math.max(max, prevPer);
  const scale = niceScale(Math.max(max, 1) * 1.08, COUNT_SCALES, 100);
  const y = (n) => 100 - Math.min(n, scale.max) / scale.max * 100;
  const yTicks = [];
  for (let v = 0; v <= scale.max; v += scale.step) yTicks.push({ top: 100 - v / scale.max * 100, label: String(v) });

  return {
    blurb,
    cards,
    chart: {
      kind: "completed",
      buckets,
      seconds,
      yTicks,
      bars: buckets.map((bucket) => STACK.map((key) => {
        const n = key === "other" ? (shown.all ? bucket.done.other : 0) : (shown[key] ? bucket.done[key] : 0);
        return n / scale.max * 100;
      })),
      lines: shown.all ? [{ key: "all", points: buckets.map((bucket, i) => ({ i, y: y(bucket.done.total), hollow: false })) }] : [],
      prev: showPrev ? { top: y(prevPer), label: `${prevText} · ${prevPer.toFixed(1)} per ${unit}` } : null,
      notes: [
        `${bucketWidthLabel(seconds)}, aligned to the clock`,
        "bars stack low · medium · hard · other; line = total",
        TERMINAL_TIME_NOTE,
        undated,
      ].filter(Boolean),
      aria: `Tasks completed per ${unit}, window ${windowKeyValue}: ${current.done.total} done (low ${current.done.low}, medium ${current.done.medium}, hard ${current.done.hard})${previous ? `, versus ${previous.done.total} in the ${prevText}` : ""}. Use the Table view for per-bucket values.`,
      tip: (index) => {
        const bucket = buckets[index];
        const tip = { title: bucketTitle(bucket.start, seconds, index === buckets.length - 1), rows: [], foot: `Tasks that moved to done in this ${unit}`, extra: `rejected ${bucket.rejected} · archived ${bucket.archived}` };
        if (!bucket.done.total) {
          tip.empty = "No tasks reached done in this bucket.";
          return tip;
        }
        for (const series of SERIES) {
          tip.rows.push({ key: series.key, name: series.doneLabel, bar: series.key !== "all", value: String(doneCount(bucket, series.key)), strong: series.key === "all" });
        }
        if (bucket.done.other) tip.rows.push({ key: "other", name: "xhard / unset", bar: true, value: String(bucket.done.other) });
        return tip;
      },
    },
    table: {
      caption: `Tasks moved to done per bucket · ${bucketWidthLabel(seconds)} · newest first`,
      heads: ["Done", "Low", "Medium", "Hard", "Rejected", "Archived"],
      rows: buckets.map((bucket, i) => ({
        title: bucketTitle(bucket.start, seconds, i === buckets.length - 1),
        cells: [bucket.done.total, bucket.done.low, bucket.done.medium, bucket.done.hard, bucket.rejected, bucket.archived].map(String),
      })).reverse(),
    },
  };
}

// Origin counts with the unnamed finders folded into `other`, as the chart
// draws them.
function chartOrigins(counts) {
  const out = { other: 0 };
  for (const [key, n] of Object.entries(counts.origin || {})) {
    if (ORIGIN_CHART.includes(key)) out[key] = n;
    else out.other += n;
  }
  return out;
}

function defectSeries() {
  if (view.group === "lineage") return LINEAGE_SERIES;
  return [...ORIGIN_CHART, "other"].map((key) => ({ key, label: key === "other" ? "Other" : ORIGIN_LABELS[key] }));
}

function defectCount(counts, key) {
  if (key === "all") return counts.total;
  return view.group === "lineage" ? (counts.lineage[key] || 0) : (chartOrigins(counts)[key] || 0);
}

function pct(part, whole) {
  return whole ? Math.round(part / whole * 100) : 0;
}

function defectsModel(countView) {
  const payload = lastPayload.defects;
  if (!payload || payload.unavailable) return pendingTrend("Defects", "Bug tasks filed in each bucket.", payload);
  const windowKeyValue = lastPayload.windowKey;
  const buckets = payload.buckets || [];
  const seconds = payload.window.bucket_seconds;
  const shown = view.shown[shownKey()];
  const current = payload.current;
  const previous = payload.previous;
  const prevText = previousLabel(windowKeyValue);
  const unit = perBucketUnit(seconds);
  const series = defectSeries();
  const lineageOn = view.group === "lineage";

  const card = (key, label, bar) => {
    const cur = defectCount(current, key);
    const prev = previous ? defectCount(previous, key) : null;
    return {
      key,
      label,
      bar,
      on: !!shown[key],
      value: key === "all" ? String(cur) : null,
      delta: key === "all" && previous ? delta(cur, prev, "defects") : null,
      sub: key === "all" && previous ? `vs ${prevText} ${prev}` : null,
      foot: key !== "all" && current.total ? `${pct(cur, current.total)}% of defects` : null,
    };
  };
  const cards = [card("all", "Defects (total)", false), ...series.map((entry) => card(entry.key, entry.label, true))];

  const blurb = lineageOn
    ? "Bug tasks filed in each bucket, by what their regression_from relation names as the cause. Fix-for-a-fix names a bug fix, other regression names another type of task, and unattributed defects have no cause recorded."
    : "Bug tasks filed in each bucket, by the review or sweep that filed them.";
  if (!current.total) {
    return { blurb, cards, empty: { title: `No defects filed in the last ${windowKeyValue === "all" ? "recorded history" : windowKeyValue}`, detail: "" } };
  }

  if (countView !== "bucket") {
    const lead = payload.lead_buckets || [];
    return {
      blurb,
      cards,
      ...runningModel({
        mode: countView,
        kind: "defects",
        windowKey: windowKeyValue,
        seconds,
        starts: buckets.map((bucket) => bucket.start),
        series: [{ key: "all", label: "Defects (total)" }, ...series].map((entry) => ({
          key: entry.key,
          swatch: entry.key,
          label: entry.label,
          strong: entry.key === "all",
          on: !!shown[entry.key],
          values: buckets.map((bucket) => defectCount(bucket, entry.key)),
          lead: lead.map((bucket) => defectCount(bucket, entry.key)),
        })),
        fmt: String,
        noun: "defects",
        subject: "Bug tasks filed",
        notes: ["time = when the bug task was filed"],
        bucketTitle,
        bucketWidthLabel,
      }),
    };
  }

  const keys = series.map((entry) => entry.key);
  const stackValue = (bucket) => keys.reduce((sum, key) => sum + (shown[key] ? defectCount(bucket, key) : 0), 0);
  let max = 0;
  for (const bucket of buckets) max = Math.max(max, shown.all ? bucket.total : stackValue(bucket));
  const prevPer = previous && buckets.length ? previous.total / buckets.length : null;
  const showPrev = shown.all && prevPer != null && previous.total > 0;
  if (showPrev) max = Math.max(max, prevPer);
  const scale = niceScale(Math.max(max, 1) * 1.08, COUNT_SCALES, 100);
  const y = (n) => 100 - Math.min(n, scale.max) / scale.max * 100;
  const yTicks = [];
  for (let v = 0; v <= scale.max; v += scale.step) yTicks.push({ top: 100 - v / scale.max * 100, label: String(v) });

  const tableKeys = lineageOn ? keys : (payload.origins || Object.keys(current.origin));
  const tableLabel = (key) => (lineageOn ? LINEAGE_SERIES.find((entry) => entry.key === key).label : ORIGIN_LABELS[key] || key);
  const tableCount = (bucket, key) => (lineageOn ? bucket.lineage[key] : bucket.origin[key]) || 0;
  const attributed = current.total - (current.lineage.unattributed || 0);

  return {
    blurb,
    cards,
    chart: {
      kind: "defects",
      buckets,
      seconds,
      yTicks,
      stackKeys: keys,
      bars: buckets.map((bucket) => keys.map((key) => (shown[key] ? defectCount(bucket, key) : 0) / scale.max * 100)),
      lines: shown.all ? [{ key: "all", points: buckets.map((bucket, i) => ({ i, y: y(bucket.total), hollow: false })) }] : [],
      prev: showPrev ? { top: y(prevPer), label: `${prevText} · ${prevPer.toFixed(1)} per ${unit}` } : null,
      notes: [
        `${bucketWidthLabel(seconds)}, aligned to the clock`,
        `bars stack ${series.map((entry) => entry.label.toLowerCase()).join(" · ")}; line = total`,
        "time = when the bug task was filed",
        lineageOn
          ? `${attributed} of ${current.total} name a cause (${pct(attributed, current.total)}%)`
          : "Other = scanning sweeps, review reconciliation, friction curation and untagged",
      ],
      aria: `Defects filed per ${unit}, window ${windowKeyValue}: ${current.total} total, ${fixForFix} fix-for-a-fix${previous ? `, versus ${previous.total} in the ${prevText}` : ""}. Grouped by ${view.group}: ${series.map((entry) => `${entry.label} ${defectCount(current, entry.key)}`).join(", ")}. Use the Table view for per-bucket values.`,
      tip: (index) => {
        const bucket = buckets[index];
        const tip = { title: bucketTitle(bucket.start, seconds, index === buckets.length - 1), rows: [], foot: `Bug tasks filed in this ${unit}` };
        if (!bucket.total) {
          tip.empty = "No defects filed in this bucket.";
          return tip;
        }
        tip.rows.push({ key: "all", name: "Defects (total)", value: String(bucket.total), strong: true });
        for (const entry of series) {
          const n = defectCount(bucket, entry.key);
          if (n) tip.rows.push({ key: entry.key, name: entry.label, bar: true, value: String(n) });
        }
        if (!lineageOn && bucket.lineage.fix_for_fix) tip.extra = `fix-for-a-fix ${bucket.lineage.fix_for_fix}`;
        return tip;
      },
    },
    table: {
      caption: `Bug tasks filed per bucket by ${view.group} · ${bucketWidthLabel(seconds)} · newest first`,
      heads: ["Total", ...tableKeys.map(tableLabel)],
      rows: buckets.map((bucket, i) => ({
        title: bucketTitle(bucket.start, seconds, i === buckets.length - 1),
        cells: [bucket.total, ...tableKeys.map((key) => tableCount(bucket, key))].map(String),
      })).reverse(),
    },
  };
}

function trendTable(table) {
  const head = el("tr", {}, [
    el("th", { text: "Bucket", scope: "col" }),
    ...table.heads.map((label) => el("th", { class: "num", text: label, scope: "col" })),
  ]);
  const rows = table.rows.map((row) => el("tr", {}, [
    el("th", { text: row.title, scope: "row" }),
    ...row.cells.map((cell) => el("td", { class: cell == null || cell === "0" ? "num mx-muted" : "num", text: cell == null ? "—" : cell })),
  ]));
  return el("div", { class: "mx-table-wrap", tabindex: "0" }, [
    el("table", { class: "mx-table" }, [
      el("caption", { text: table.caption }),
      el("thead", {}, [head]),
      el("tbody", {}, rows),
    ]),
  ]);
}

function trendChart(chart) {
  const count = chart.buckets.length;
  const x = (i) => (i + 0.5) / count * 100;

  const yAxis = el("div", { class: "mx-y-axis", "aria-hidden": "true" }, chart.yTicks.map((tick) => {
    const label = el("div", { class: "mx-y-tick", text: tick.label });
    label.style.top = `${tick.top}%`;
    return label;
  }));

  const plot = el("div", { class: "mx-plot", role: "img", "aria-label": chart.aria });
  for (const tick of chart.yTicks) {
    const grid = el("div", { class: "mx-grid" });
    grid.style.top = `${tick.top}%`;
    plot.appendChild(grid);
  }
  if (chart.bars) {
    plot.appendChild(el("div", { class: "mx-bars", "aria-hidden": "true" }, chart.bars.map((stack) => stackColumn(stack, "mx-bar-col", chart.stackKeys))));
  }
  if (chart.prev) {
    const line = el("div", { class: "mx-prev" });
    line.style.top = `${chart.prev.top}%`;
    const label = el("div", { class: "mx-prev-label", text: chart.prev.label });
    label.style.top = `${chart.prev.top}%`;
    plot.append(line, label);
  }
  const lines = svg("svg", { viewBox: "0 0 1000 1000", preserveAspectRatio: "none", class: "mx-lines", "aria-hidden": "true" });
  // `all` draws last, over the complexity lines.
  for (const line of [...chart.lines].sort((a, b) => (a.key === "all") - (b.key === "all"))) {
    lines.appendChild(svg("path", {
      // A `joined` line spans buckets without a value instead of breaking.
      d: linePath((line.joined ? line.points.filter(Boolean) : line.points).map((point) => (point ? { x: x(point.i), y: point.y } : null))),
      class: `mx-line mx-s-${line.key}${line.cls ? ` ${line.cls}` : ""}`,
      "vector-effect": "non-scaling-stroke",
    }));
  }
  plot.appendChild(lines);
  const cursor = el("div", { class: "mx-cursor", "aria-hidden": "true" });
  cursor.hidden = true;
  plot.appendChild(cursor);
  const dots = [];
  for (const line of chart.lines) {
    for (const point of line.points) {
      if (!point) continue;
      const dot = el("div", { class: `mx-pt mx-s-${line.key}${point.hollow ? " hollow" : ""}`, "aria-hidden": "true" });
      dot.style.left = `${x(point.i)}%`;
      dot.style.top = `${point.y}%`;
      dot.dataset.bucket = String(point.i);
      dots.push(dot);
      plot.appendChild(dot);
    }
  }
  const tip = el("div", { class: "mx-tip", "aria-hidden": "true" });
  tip.hidden = true;
  plot.appendChild(tip);

  const columns = [];
  let volume = null;
  if (chart.volume) {
    volume = el("div", { class: "mx-volume", "aria-hidden": "true" }, chart.volume.stacks.map((stack) => {
      const column = stackColumn(stack.map((n) => (chart.volume.max ? n / chart.volume.max * 100 : 0)), "mx-vol-col");
      columns.push(column);
      return column;
    }));
  }
  const barColumns = chart.bars ? Array.from(plot.querySelectorAll(".mx-bar-col")) : [];

  const xAxis = el("div", { class: "mx-x-axis", "aria-hidden": "true" });
  chart.buckets.forEach((bucket, i) => {
    if (!isTick(bucket.start, chart.seconds, i, count)) return;
    const label = el("div", { class: "mx-x-tick", text: tickLabel(bucket.start, chart.seconds) });
    label.style.left = `${x(i)}%`;
    xAxis.appendChild(label);
  });

  const hover = el("div", { class: "mx-hover" });
  const show = (index) => {
    view.hover = index;
    if (index == null || index < 0 || index >= count) {
      tip.hidden = true;
      cursor.hidden = true;
      for (const node of [...dots, ...columns, ...barColumns]) node.classList.remove("on");
      return;
    }
    cursor.hidden = false;
    cursor.style.left = `${x(index)}%`;
    for (const dot of dots) dot.classList.toggle("on", dot.dataset.bucket === String(index));
    for (const [i, column] of [...columns.entries(), ...barColumns.entries()]) column.classList.toggle("on", i === index);
    fillTip(tip, chart.tip(index));
    tip.hidden = false;
    tip.style.left = `${x(index)}%`;
    tip.classList.toggle("left", x(index) > 62);
  };
  for (let i = 0; i < count; i++) {
    const column = el("div", { class: "mx-hover-col" });
    column.addEventListener("mouseenter", () => show(i));
    hover.appendChild(column);
  }
  hover.addEventListener("mouseleave", () => show(null));

  const frame = el("div", { class: "mx-chart-frame", tabindex: "0", "aria-label": "Trend chart: arrow keys step through buckets" }, [plot, volume, xAxis, hover]);
  frame.addEventListener("keydown", (event) => {
    if (event.key === "Escape") { show(null); return; }
    if (event.key !== "ArrowRight" && event.key !== "ArrowLeft") return;
    event.preventDefault();
    const from = view.hover == null ? count : view.hover;
    show(Math.max(0, Math.min(count - 1, from + (event.key === "ArrowRight" ? 1 : -1))));
  });
  frame.addEventListener("blur", () => show(null));
  if (view.hover != null && view.hover < count) queueMicrotask(() => show(view.hover));

  const legend = el("div", { class: "mx-notes" }, [
    chart.hollowNote || chart.hollowText ? el("span", { class: "mx-note-hollow" }, [el("span", { class: "mx-pt-key", "aria-hidden": "true" }), chart.hollowText || `hollow = fewer than ${LOW_SAMPLE} runs`]) : null,
    ...chart.notes.map((text) => el("span", { text })),
  ]);
  const yWrap = el("div", { class: `mx-y-wrap${chart.volume ? " with-volume" : ""}` }, [
    yAxis,
    chart.volume ? el("div", { class: "mx-vol-label", "aria-hidden": "true" }, [el("span", { text: "runs" }), el("span", { class: "mx-vol-max", text: String(chart.volume.max) })]) : null,
  ]);
  return el("div", { class: `mx-chart mx-chart-${chart.kind}` }, [
    el("div", { class: "mx-chart-body" }, [yWrap, frame]),
    legend,
  ]);
}

function stackColumn(heights, className, keys = STACK) {
  const column = el("div", { class: className });
  // Rendered top to bottom inside a bottom-aligned column.
  for (let i = heights.length - 1; i >= 0; i--) {
    const segment = el("div", { class: `mx-seg-fill mx-s-${keys[i]}` });
    segment.style.height = `${heights[i]}%`;
    column.appendChild(segment);
  }
  return column;
}

function fillTip(tip, model) {
  const rows = (model.rows || []).map((row) => el("div", { class: `mx-tip-row${row.strong ? " strong" : ""}` }, [
    el("span", { class: "mx-tip-name" }, [
      el("span", { class: `mx-swatch mx-s-${row.key}${row.bar ? " bar" : ""}${row.dashed ? " dashed" : ""}`, "aria-hidden": "true" }),
      row.name,
      row.n ? el("span", { class: row.low ? "mx-tip-n low" : "mx-tip-n", text: row.n }) : null,
    ]),
    el("span", { class: "mx-tip-value", text: row.value }),
  ]));
  tip.replaceChildren(
    el("div", { class: "mx-tip-title", text: model.title }),
    ...(model.empty ? [el("div", { class: "mx-tip-empty", text: model.empty })] : rows),
    model.extra ? el("div", { class: "mx-tip-extra", text: model.extra }) : "",
    model.warn ? el("div", { class: "mx-tip-warn", text: model.warn }) : "",
    el("div", { class: "mx-tip-foot", text: model.foot }),
  );
}

// ---- detail cards ---------------------------------------------------------
