// Orbit dashboard Analytics › Metrics (ORB-15746): implement_one duration,
// task completion and defect trends over the shared dashboard window, with
// the per-actor tables beneath.
// Pure vanilla JS, split into ES modules with no build step.
//
// One panel request reads the three endpoints together; the view state below
// (trend tab, statistic, chart or table, visible series) is local and
// re-renders from the cached payload without a fetch. Hover moves the
// tooltip in place instead of rebuilding the chart.

import {
  el as makeEl, syncNodes, fetchJson, getWindow, setWindow, persistScopeToUrl, syncWindowSelectors,
  notifyScopeChange, wireWindowSelector, requestPanel, panelCanRender, getWorkspaceRevision,
  onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime, formatClock, getClockZone,
  bindTaskLink, DASHBOARD_WINDOWS,
} from './common.js';
import { linePath, sparkline as lineSparkline, svg } from './sparkline.js';

const $ = (id) => document.getElementById(id);

// `el` from common.js, also applying the table and focus attributes it leaves out.
function el(tag, opts = {}, children = []) {
  const node = makeEl(tag, opts, children);
  for (const name of ["scope", "tabindex"]) {
    if (opts[name] != null) node.setAttribute(name, String(opts[name]));
  }
  return node;
}

// A bucket or actor with fewer runs than this is drawn hollow or dimmed: its
// average is one or two runs, not a trend.
const LOW_SAMPLE = 3;
const LOW_ACTOR_SAMPLE = 5;
const ACTORS_SHOWN = 7;

const SERIES = [
  { key: "all", label: "All runs", doneLabel: "Done (total)", tag: "" },
  { key: "low", label: "Easy", doneLabel: "Easy", tag: "low" },
  { key: "medium", label: "Medium", doneLabel: "Medium", tag: "medium" },
  { key: "hard", label: "Hard", doneLabel: "Hard", tag: "hard" },
];
// Stacked bottom to top; `other` is xhard plus unset.
const STACK = ["low", "medium", "hard", "other"];
const COMPLEXITY_TABS = ["low", "medium", "hard", "xhard", "unset"];
const STATS = [["avg", "Avg"], ["p50", "p50"], ["p95", "p95"]];
const TRENDS = [["duration", "implement_one duration"], ["completed", "Tasks completed"], ["defects", "Defects"]];
const DEFECT_GROUPS = [["lineage", "Lineage"], ["origin", "Origin"]];
// Lineage: what a bug's `regression_from` relations name as its cause.
const LINEAGE_SERIES = [
  { key: "fix_for_fix", label: "Fix-for-a-fix", tag: "bug ← bug" },
  { key: "regression", label: "Other regression", tag: "bug ← work" },
  { key: "unattributed", label: "Unattributed", tag: "no cause" },
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
const PROVIDERS = new Set(["claude", "codex", "gemini", "grok"]);

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
  },
  hover: null,
  allActors: false,
  complexity: null,
};
let lastPayload = null;
let metricsRequestScope = null;

function wireMetricsWindowSelector() {
  wireWindowSelector("metrics-window-selector", { allowAll: true });
}

function clearMetrics() {
  lastPayload = null;
  view.hover = null;
  const count = $("metrics-count");
  if (count) count.textContent = "—";
}

export function placeholdMetricsAggregate() {
  clearMetrics();
  renderPanelPlaceholder("metrics-body");
}

export function fetchAndRenderMetrics() {
  const hashQuery = new URLSearchParams(window.location.hash.split("?")[1] || "");
  view.outcome = hashQuery.get("outcome") === "all" ? "all" : "succeeded";
  wireMetricsWindowSelector();
  if (isAggregateView()) {
    placeholdMetricsAggregate();
    return Promise.resolve();
  }
  const selectedWindow = getWindow();
  const selectedOutcome = view.outcome;
  const revision = getWorkspaceRevision();
  const since = `since=${encodeURIComponent(selectedWindow)}`;
  const scope = `/api/diagnostics/implement_one?${since}&outcome=${selectedOutcome}`;
  if (metricsRequestScope !== `${revision}\0${scope}`) {
    metricsRequestScope = `${revision}\0${scope}`;
    clearMetrics();
  }
  return requestPanel("metrics-body", scope, () => Promise.all([
    fetchJson(scope),
    fetchJson(`/api/diagnostics/completions?${since}`),
    fetchJson("/api/tasks/completion-by-complexity"),
    // A host running an older Orbit has no defect trend; the other trends
    // still render and the Defects tab says why it is empty.
    fetchJson(`/api/diagnostics/defects?${since}`).catch((error) => ({
      unavailable: error.status === 404 ? "This host's Orbit does not report defects yet. Upgrade it to see this trend." : error.message,
    })),
  ]), ([durations, completions, outcome, defects]) => {
    if (getWorkspaceRevision() !== revision || getWindow() !== selectedWindow || view.outcome !== selectedOutcome) return;
    if (windowKey(durations) !== selectedWindow || windowKey(completions) !== selectedWindow || (!defects.unavailable && windowKey(defects) !== selectedWindow) || durations.outcome !== selectedOutcome) {
      console.error(`metrics payload window rejected under ${selectedWindow} selection`);
      return;
    }
    lastPayload = { windowKey: selectedWindow, durations, completions, defects, outcome: (outcome && outcome.by_complexity) || [] };
    renderMetrics();
  }, "metrics-count");
}

onWorkspaceChange(() => {
  metricsRequestScope = null;
  clearMetrics();
});

function windowKey(payload) {
  return payload && payload.window ? payload.window.key : null;
}

export function renderMetrics() {
  const body = $("metrics-body");
  if (!body) return;
  if (isAggregateView()) {
    placeholdMetricsAggregate();
    return;
  }
  if (!lastPayload || !panelCanRender("metrics-body")) return;
  const { durations, completions } = lastPayload;
  const count = $("metrics-count");
  if (count) {
    const runs = durations.current && durations.current.all ? durations.current.all.n : 0;
    count.textContent = `${runs} runs · ${completions.current.done.total} done · window ${lastPayload.windowKey}`;
  }
  syncNodes(body, [
    trendSection(),
    el("div", { class: "mx-row" }, [actorsCard(), slowestCard()]),
    el("div", { class: "mx-row" }, [complexityActorsCard(), outcomeCard()]),
  ].map((node, index) => keyed(node, `metrics-${index}`, sectionHash(index))));
}

function keyed(node, key, hash) {
  node.dataset.key = key;
  node.dataset.hash = hash;
  return node;
}

function sectionHash(index) {
  const { hover, ...state } = view;
  const p = lastPayload;
  const parts = [
    [state.outcome, state.trend, state.group, state.stat, state.mode, state.shown, p.windowKey, p.durations.buckets, p.durations.current, p.durations.previous, p.completions, p.defects],
    [state.allActors, p.durations.actors, p.durations.slowest],
    [state.complexity, p.durations.implement_one_by_complexity, p.outcome],
  ];
  return JSON.stringify(parts[index]);
}

function rerender() {
  renderMetrics();
}

// ---- formatting -----------------------------------------------------------

/// "43.9m", "1h 05m", "45s" — minutes carry one decimal so close trends stay
/// distinguishable.
function fmtMs(ms) {
  if (ms == null || !Number.isFinite(Number(ms))) return "—";
  const value = Number(ms);
  if (value < 60000) return `${Math.round(value / 1000)}s`;
  const minutes = value / 60000;
  if (minutes < 60) return `${minutes.toFixed(1)}m`;
  let hours = Math.floor(minutes / 60);
  let rest = Math.round(minutes - hours * 60);
  if (rest === 60) { hours += 1; rest = 0; }
  return `${hours}h ${String(rest).padStart(2, "0")}m`;
}

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
  return `${names[seconds] || `${seconds}s`} buckets`;
}

function perBucketUnit(seconds) {
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

function previousLabel(windowKeyValue) {
  return windowKeyValue === "all" ? "" : `prev ${windowKeyValue}`;
}

// mode "duration": higher is worse; "count": higher is better; "defects":
// a count where higher is worse.
function delta(current, previous, mode) {
  if (current == null) return null;
  if (previous == null || !previous) return { text: "new", cls: "flat" };
  const ratio = (current - previous) / previous * 100;
  const abs = Math.abs(ratio);
  const text = `${ratio >= 0 ? "▲" : "▼"} ${abs >= 10 ? Math.round(abs) : abs.toFixed(1)}%`;
  const higherIsWorse = mode === "duration" || mode === "defects";
  const worse = higherIsWorse ? ratio > 5 : ratio < -5;
  const better = higherIsWorse ? ratio < -5 : ratio > 5;
  const word = mode === "duration" ? (ratio >= 0 ? "slower" : "faster") : (ratio >= 0 ? "more" : "fewer");
  return { text: `${text} ${word}`, cls: worse ? "bad" : better ? "good" : "flat" };
}

function splitActor(name) {
  const parts = String(name || "").split(" / ");
  const provider = parts.length > 1 ? parts[0] : "";
  return { provider, model: parts.length > 1 ? parts.slice(1).join(" / ") : parts[0] };
}

function providerDot(provider, className = "mx-dot") {
  const dot = el("span", { class: className, "aria-hidden": "true" });
  dot.style.background = PROVIDERS.has(provider) ? `var(--ag-${provider})` : "var(--fg-mute)";
  return dot;
}

function actorCell(actor) {
  const who = splitActor(actor);
  return el("span", { class: "mx-actor", title: actor }, [
    providerDot(who.provider),
    el("span", { class: "mx-actor-model", text: who.model }),
    who.provider ? el("span", { class: "mx-actor-provider", text: who.provider }) : null,
  ]);
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
  rerender();
}

// The series-visibility key for the current trend; each defect grouping keeps
// its own.
function shownKey() {
  return view.trend === "defects" ? `defects-${view.group}` : view.trend;
}

function trendSection() {
  const isDuration = view.trend === "duration";
  const isDefects = view.trend === "defects";
  const model = isDuration ? durationModel() : isDefects ? defectsModel() : completedModel();
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
    segGroup("View", MODES, view.mode, (key) => { view.mode = key; view.hover = null; rerender(); }),
  ]);
  const panel = el("div", { class: "mx-trend-view", id: "metrics-trend-view", role: "tabpanel" }, [
    el("p", { class: "mx-blurb", text: model.blurb }),
    seriesCards(model.cards),
    model.empty ? emptyTrend(model.empty) : (view.mode === "table" ? trendTable(model.table) : trendChart(model.chart)),
    isDuration ? el("p", { class: "mx-notes", text: view.outcome === "succeeded"
      ? "Counted: final implement_one attempts in successful runs with a committed candidate. Failed, cancelled, no-diff and unknown outcomes are excluded."
      : "Counted: all implement_one invocations, including failed, cancelled, no-diff and unknown outcomes." }) : null,
  ]);
  return el("section", { class: "mx-card mx-trends", "aria-label": "Trends" }, [
    el("div", { class: "mx-trend-head" }, [trendTabs(), tools]),
    panel,
  ]);
}

function seriesCards(cards) {
  return el("div", { class: "mx-series", role: "group", "aria-label": "Series" }, cards.map((card) => {
    const button = el("button", {
      class: `mx-sc mx-s-${card.key}${card.on ? " on" : " off"}`,
      type: "button",
      "aria-pressed": card.on ? "true" : "false",
      title: card.on ? `Hide ${card.label}` : `Show ${card.label}`,
    }, [
      el("span", { class: "mx-sc-label" }, [
        el("span", { class: `mx-swatch${card.bar ? " bar" : ""}`, "aria-hidden": "true" }),
        card.label,
        card.tag ? el("span", { class: "mx-sc-tag", text: card.tag }) : null,
      ]),
      el("span", { class: "mx-sc-value", text: card.value }),
      el("span", { class: "mx-sc-delta" }, [
        card.delta ? el("span", { class: card.delta.cls, text: card.delta.text }) : null,
        el("span", { class: "mx-sc-sub", text: card.sub }),
      ]),
      el("span", { class: "mx-sc-foot", text: card.foot }),
    ]);
    button.addEventListener("click", () => {
      const shown = view.shown[shownKey()];
      const next = { ...shown, [card.key]: !shown[card.key] };
      // The chart always keeps one series.
      if (!Object.values(next).some(Boolean)) return;
      view.shown[shownKey()] = next;
      rerender();
    });
    return button;
  }));
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

  const blurb = `${statLabel} wall-clock time of implement_one runs that finished in each bucket, by task complexity.`;
  if (!current.all) {
    return {
      blurb,
      cards,
      empty: {
        title: `No implement_one runs finished in the last ${windowKeyValue === "all" ? "recorded history" : windowKeyValue}`,
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
    "runs bars stack easy · medium · hard · other",
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
      aria: `implement_one ${statLabel} duration by complexity, window ${windowKeyValue}. ${visible.map((series) => `${series.label} ${current[series.key] ? fmtMs(current[series.key][stat]) : "no runs"}`).join(", ")}. Use the Table view for per-bucket values.`,
      tip: (index) => {
        const bucket = buckets[index];
        const tip = { title: bucketTitle(bucket.start, seconds, index === buckets.length - 1), rows: [], foot: `${statLabel} per series · ×n = runs` };
        if (!bucket.all) {
          tip.empty = "No implement_one runs finished in this bucket.";
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
      heads: ["Runs", "All", "Easy", "Medium", "Hard"],
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

function completedModel() {
  const payload = lastPayload.completions;
  const windowKeyValue = lastPayload.windowKey;
  const buckets = payload.buckets || [];
  const seconds = payload.window.bucket_seconds;
  const shown = view.shown.completed;
  const current = payload.current;
  const previous = payload.previous;
  const prevText = previousLabel(windowKeyValue);
  const unit = perBucketUnit(seconds);

  const cards = SERIES.map((series) => {
    const cur = doneCount(current, series.key);
    const prev = previous ? doneCount(previous, series.key) : null;
    return {
      key: series.key,
      label: series.doneLabel,
      tag: series.tag,
      bar: series.key !== "all",
      on: !!shown[series.key],
      value: String(cur),
      delta: previous ? delta(cur, prev, "count") : null,
      sub: previous ? `vs ${prevText} ${prev}` : `window ${windowKeyValue}`,
      foot: series.key === "all"
        ? `${current.rejected} rejected · ${current.archived} archived`
        : `${current.done.total ? Math.round(cur / current.done.total * 100) : 0}% of done`,
    };
  });

  const blurb = "Tasks that moved to done in each bucket, by complexity.";
  if (!current.done.total && !current.rejected && !current.archived) {
    return { blurb, cards, empty: { title: `No tasks closed in the last ${windowKeyValue === "all" ? "recorded history" : windowKeyValue}`, detail: "" } };
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
        "bars stack easy · medium · hard · other; line = total",
        "time = the task's last write after it closed",
      ],
      aria: `Tasks completed per ${unit}, window ${windowKeyValue}: ${current.done.total} done (easy ${current.done.low}, medium ${current.done.medium}, hard ${current.done.hard})${previous ? `, versus ${previous.done.total} in the ${prevText}` : ""}. Use the Table view for per-bucket values.`,
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
      heads: ["Done", "Easy", "Medium", "Hard", "Rejected", "Archived"],
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

function defectsModel() {
  const payload = lastPayload.defects;
  const windowKeyValue = lastPayload.windowKey;
  if (payload.unavailable) {
    return { blurb: "Bug tasks filed in each bucket.", cards: [], empty: { title: "Defect trend unavailable", detail: payload.unavailable, noWiden: true } };
  }
  const buckets = payload.buckets || [];
  const seconds = payload.window.bucket_seconds;
  const shown = view.shown[shownKey()];
  const current = payload.current;
  const previous = payload.previous;
  const prevText = previousLabel(windowKeyValue);
  const unit = perBucketUnit(seconds);
  const series = defectSeries();
  const lineageOn = view.group === "lineage";
  const fixForFix = current.lineage.fix_for_fix || 0;

  const card = (key, label, tag, bar) => {
    const cur = defectCount(current, key);
    const prev = previous ? defectCount(previous, key) : null;
    return {
      key,
      label,
      tag,
      bar,
      on: !!shown[key],
      value: String(cur),
      delta: previous ? delta(cur, prev, "defects") : null,
      sub: previous ? `vs ${prevText} ${prev}` : `window ${windowKeyValue}`,
      foot: key === "all"
        ? `${fixForFix} fix-for-a-fix (${pct(fixForFix, current.total)}%)`
        : `${pct(cur, current.total)}% of defects`,
    };
  };
  const cards = [card("all", "Defects (total)", "", false), ...series.map((entry) => card(entry.key, entry.label, entry.tag || "", true))];

  const blurb = lineageOn
    ? "Bug tasks filed in each bucket, by what their regression_from relation names as the cause. Fix-for-a-fix: the cause was itself a bug fix."
    : "Bug tasks filed in each bucket, by the review or sweep that filed them.";
  if (!current.total) {
    return { blurb, cards, empty: { title: `No defects filed in the last ${windowKeyValue === "all" ? "recorded history" : windowKeyValue}`, detail: "" } };
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
      d: linePath(line.points.map((point) => (point ? { x: x(point.i), y: point.y } : null))),
      class: `mx-line mx-s-${line.key}`,
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
    chart.hollowNote ? el("span", { class: "mx-note-hollow" }, [el("span", { class: "mx-pt-key", "aria-hidden": "true" }), `hollow = fewer than ${LOW_SAMPLE} runs`]) : null,
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
      el("span", { class: `mx-swatch mx-s-${row.key}${row.bar ? " bar" : ""}`, "aria-hidden": "true" }),
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

function card(title, meta, children, className = "") {
  return el("section", { class: `mx-card ${className}`.trim(), "aria-label": title }, [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title", text: title }),
      meta ? el("span", { class: "mx-card-meta", text: meta }) : null,
    ]),
    ...children,
  ]);
}

function emptyNote(text) {
  return el("div", { class: "mx-empty-note", text });
}

function sparkline(trend, max, label) {
  const values = trend || [];
  const points = values.map((v, i) => (v == null ? null : { x: (i + 0.5) / values.length * 100, y: 100 - v / (max * 1.05) * 100 }));
  return lineSparkline(points, { className: "mx-spark", lineClass: "mx-spark-line", label });
}

function actorsCard() {
  const payload = lastPayload.durations;
  const actors = payload.actors || [];
  const prevText = previousLabel(lastPayload.windowKey);
  const meta = `window ${lastPayload.windowKey} · sorted by runs${prevText ? ` · Δ = avg vs ${prevText}` : ""}`;
  if (!actors.length) return card("implement_one by actor", meta, [emptyNote("No implement_one runs in this window.")], "mx-wide");
  let sparkMax = 0;
  for (const actor of actors) for (const v of actor.trend || []) if (v != null) sparkMax = Math.max(sparkMax, v);
  const rows = (view.allActors ? actors : actors.slice(0, ACTORS_SHOWN)).map((actor) => {
    const change = prevText ? delta(actor.avg, actor.previous_avg, "duration") : null;
    const stroke = providerDot(splitActor(actor.actor).provider).style.background;
    const spark = sparkline(actor.trend, sparkMax || 1, `${splitActor(actor.actor).model} average per bucket`);
    spark.style.color = stroke;
    return el("tr", { class: actor.n < LOW_ACTOR_SAMPLE ? "mx-dim" : "" }, [
      el("th", { scope: "row" }, [actorCell(actor.actor)]),
      el("td", { class: "num", text: String(actor.n) }),
      el("td", { class: "num strong", text: fmtMs(actor.avg) }),
      el("td", { class: "num", text: fmtMs(actor.p50) }),
      el("td", { class: "num", text: fmtMs(actor.p95) }),
      prevText ? el("td", { class: "num" }, [change ? el("span", { class: change.cls, text: change.text.replace(/ (slower|faster)$/, "") }) : "—"]) : null,
      el("td", { class: "mx-spark-cell" }, [spark]),
    ]);
  });
  const head = el("tr", {}, ["Actor", "Runs", "Avg", "p50", "p95", prevText ? "Δ avg" : null, "Avg trend"].filter(Boolean).map((label, i) => el("th", { class: i === 0 || label === "Avg trend" ? "" : "num", scope: "col", text: label })));
  const children = [el("div", { class: "mx-table-wrap" }, [el("table", { class: "mx-table mx-actors" }, [el("thead", {}, [head]), el("tbody", {}, rows)])])];
  if (actors.length > ACTORS_SHOWN) {
    const toggle = el("button", { class: "mx-link-btn", type: "button", text: view.allActors ? `Show top ${ACTORS_SHOWN}` : `Show all ${actors.length} actors` });
    toggle.addEventListener("click", () => { view.allActors = !view.allActors; rerender(); });
    children.push(el("div", {}, [toggle]));
  }
  return card("implement_one by actor", meta, children, "mx-wide");
}

function slowestCard() {
  const slowest = lastPayload.durations.slowest || [];
  if (!slowest.length) return card("Slowest runs", `window ${lastPayload.windowKey}`, [emptyNote("No runs in this window.")], "mx-narrow");
  const items = slowest.map((run) => {
    const who = splitActor(run.actor);
    const task = run.task_id ? bindTaskLink(el("a", { class: "mono", text: run.task_id }), run.task_id) : el("span", { class: "mx-muted", text: "no task" });
    return el("li", { class: "mx-slow" }, [
      el("span", { class: "mx-slow-dur", text: fmtMs(run.duration_ms) }),
      el("span", { class: "mx-slow-body" }, [
        task,
        el("span", { class: "mx-slow-meta", title: `${run.actor} · ${formatDateTime(run.ts)}` }, [
          providerDot(who.provider, "mx-dot small"),
          `${who.model} · ${run.complexity} · ${formatDateTime(run.ts)}`,
        ]),
      ]),
    ]);
  });
  return card("Slowest runs", `window ${lastPayload.windowKey}`, [el("ol", { class: "mx-slow-list" }, items)], "mx-narrow");
}

function complexityActorsCard() {
  const bands = lastPayload.durations.implement_one_by_complexity || [];
  const byKey = new Map(bands.map((band) => [band.complexity, band]));
  const selected = view.complexity && byKey.has(view.complexity)
    ? view.complexity
    : (["medium", "hard", "low", "xhard", "unset"].find((key) => byKey.has(key)) || "medium");
  const tabs = el("div", { class: "mx-cx-tabs", role: "group", "aria-label": "Complexity" }, COMPLEXITY_TABS.map((key) => {
    const band = byKey.get(key);
    const on = key === selected;
    const button = el("button", { class: on ? "mx-cx-tab on" : "mx-cx-tab", type: "button", "aria-pressed": on ? "true" : "false" }, [
      key,
      el("span", { class: "mx-cx-n", text: String(band ? band.n : 0) }),
    ]);
    button.disabled = !band;
    button.addEventListener("click", () => { view.complexity = key; rerender(); });
    return button;
  }));
  const band = byKey.get(selected);
  if (!band) return card("Actors within a complexity", "last 30d · bar = avg, tick = p95", [tabs, emptyNote("No implement_one runs in the last 30d.")], "mx-wide");
  const actors = [...band.actors].sort((a, b) => b.n - a.n || a.actor.localeCompare(b.actor));
  const max = Math.max(...actors.map((actor) => actor.p95), 1);
  const rows = actors.map((actor) => {
    const bar = el("div", { class: "mx-dist", "aria-hidden": "true" }, [el("div", { class: "mx-dist-avg" }), el("div", { class: "mx-dist-p95" })]);
    const color = providerDot(splitActor(actor.actor).provider).style.background;
    bar.firstChild.style.width = `${actor.avg / max * 100}%`;
    bar.firstChild.style.background = color;
    bar.lastChild.style.left = `${Math.min(actor.p95 / max * 100, 99.5)}%`;
    return el("tr", { class: actor.n < LOW_ACTOR_SAMPLE ? "mx-dim" : "" }, [
      el("th", { scope: "row" }, [actorCell(actor.actor)]),
      el("td", { class: "num", text: String(actor.n) }),
      el("td", { class: "num strong", text: fmtMs(actor.avg) }),
      el("td", { class: "num", text: fmtMs(actor.p95) }),
      el("td", { class: "mx-dist-cell" }, [bar]),
    ]);
  });
  const head = el("tr", {}, [
    el("th", { scope: "col", text: "Actor" }),
    el("th", { class: "num", scope: "col", text: "Runs" }),
    el("th", { class: "num", scope: "col", text: "Avg" }),
    el("th", { class: "num", scope: "col", text: "p95" }),
    el("th", { scope: "col" }, [el("span", { class: "visually-hidden", text: "Distribution" })]),
  ]);
  return card("Actors within a complexity", "last 30d · bar = avg, tick = p95", [
    tabs,
    el("div", { class: "mx-table-wrap" }, [el("table", { class: "mx-table" }, [el("thead", {}, [head]), el("tbody", {}, rows)])]),
  ], "mx-wide");
}

function outcomeCard() {
  const rows = lastPayload.outcome || [];
  const legend = el("div", { class: "mx-outcome-legend" }, [["done", "done"], ["rejected", "rejected"], ["archived", "archived"], ["open", "open"]].map(([key, label]) =>
    el("span", {}, [el("span", { class: `mx-swatch bar mx-o-${key}`, "aria-hidden": "true" }), label])));
  if (!rows.length) return card("Outcome by complexity", "all time", [legend, emptyNote("No tasks.")], "mx-narrow");
  const bars = rows.map((row) => {
    const counts = {};
    for (const status of row.statuses || []) counts[status.status] = status.count;
    const total = row.total || 0;
    const pct = (n) => (total ? (n || 0) / total * 100 : 0);
    const open = total - (counts.done || 0) - (counts.rejected || 0) - (counts.archived || 0);
    const label = row.complexity === "unset" ? "unset (unlabeled)" : row.complexity;
    const meter = el("div", { class: "mx-outcome-bar", role: "img", "aria-label": `${label}: ${counts.done || 0} done, ${counts.rejected || 0} rejected, ${counts.archived || 0} archived, ${open} open of ${total}` },
      ["done", "rejected", "archived"].map((key) => {
        const fill = el("div", { class: `mx-o-${key}` });
        fill.style.width = `${pct(counts[key])}%`;
        return fill;
      }));
    return el("div", { class: "mx-outcome" }, [
      el("div", { class: "mx-outcome-head" }, [
        el("span", { text: label }),
        el("span", { class: "mx-muted num", text: `${pct(counts.done).toFixed(1)}% done · n=${total}` }),
      ]),
      meter,
    ]);
  });
  return card("Outcome by complexity", "all time", [legend, el("div", { class: "mx-outcomes" }, bars)], "mx-narrow");
}
