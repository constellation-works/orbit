// Orbit dashboard Analytics › Forecast: how long the dispatchable backlog
// takes to drain and what it costs, as p50 and p90 resampled from this
// workspace's delivery history, beside the recent arrival rate the forecast
// assumes away, and how earlier stored forecasts scored against what their
// tasks actually did. Read from /api/diagnostics/backlog_forecast.
// Pure vanilla JS, split into ES modules with no build step.
//
// The forecast is per workspace; the aggregate view shows the usual
// placeholder. The lookback and arrivals toggles refetch: only the default
// view (30 days, no arrivals, the live drain ceiling) is the one stored and
// scored.

import {
  el as makeEl, syncNodes, fetchJson, requestPanel, panelCanRender, getWorkspaceRevision,
  onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime, infoHint,
} from './common.js';

const $ = (id) => document.getElementById(id);

// `el` from common.js, also applying the table and focus attributes it leaves out.
function el(tag, opts = {}, children = []) {
  const node = makeEl(tag, opts, children);
  for (const name of ["scope", "tabindex"]) {
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
const BASIS_LABELS = { cell: "cell", complexity: "complexity", workspace: "workspace", none: "no history" };

const view = { lookback: 30, arrivals: "off" };
let lastPayload = null;
let requestScope = null;

function clearForecast() {
  lastPayload = null;
  const count = $(COUNT);
  if (count) count.textContent = "—";
}

function endpoint() {
  const params = new URLSearchParams();
  if (view.lookback !== 30) params.set("lookback_days", String(view.lookback));
  if (view.arrivals === "on") params.set("arrivals", "on");
  const query = params.toString();
  return query ? `${ENDPOINT}?${query}` : ENDPOINT;
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
  const nodes = [forecastCard(), byComplexityCard(), coverageCard(), calibrationCard()];
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

function fmtHours(value) {
  const n = num(value);
  if (n == null) return "—";
  if (n < 1) return `${Math.round(n * 60)}m`;
  if (n < 48) return `${n >= 10 ? Math.round(n) : n.toFixed(1)}h`;
  const days = n / 24;
  return `${days >= 10 ? Math.round(days) : days.toFixed(1)}d`;
}

function fmtTokens(value) {
  const n = num(value);
  if (n == null) return "—";
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(n >= 1e4 ? 0 : 1)}k`;
  return `${Math.round(n)}`;
}

function fmtUsd(value) {
  const n = num(value);
  if (n == null) return "—";
  return n >= 100 ? `$${Math.round(n).toLocaleString()}` : `$${n.toFixed(2)}`;
}

function fmtRate(value) {
  const n = num(value);
  return n == null ? "—" : `${n >= 10 ? Math.round(n) : n.toFixed(1)}/day`;
}

function fmtShare(value) {
  const n = num(value);
  return n == null ? "—" : `${Math.round(n * 100)}%`;
}

/// "p50 – p90" for one quantile pair; never a bare point estimate.
function range(quantiles, fmt) {
  const p50 = quantiles?.p50;
  const p90 = quantiles?.p90;
  if (p50 == null && p90 == null) return "—";
  return `${fmt(p50)} – ${fmt(p90)}`;
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

function drainFoot(quantiles) {
  if (quantiles?.p90 != null) return null;
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
  const meta = `${backlog.backlog ?? 0} backlog · ${backlog.in_progress ?? 0} in progress · ${inputs.history_deliveries ?? 0} deliveries in ${inputs.lookback_days ?? "?"} days`;
  const children = [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title", text: "Time to drain and cost" }),
      tools,
    ]),
    el("p", { class: "mx-blurb" }, [
      `${meta}. Simulated ${inputs.iterations ?? 0} times on ${capacityText(inputs)}. `,
      infoHint("Each task draws a completed delivery's wall time, tokens and cost from tasks of the same complexity and crew pool, falling back to its complexity, then to all deliveries, when there are too few. Review, blocked and someday tasks are left out; in-progress tasks count only their remaining work."),
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
      kpi("Time to drain", forecast.time_to_drain_hours, fmtHours, drainFoot(forecast.time_to_drain_hours)),
      kpi("Cost", forecast.usd, fmtUsd, forecast.usd?.p50 == null ? "not enough costed history" : null),
      kpi("Tokens", forecast.tokens, fmtTokens, forecast.tokens?.p50 == null ? "not enough priced history" : "normalized"),
    ]));
  }
  children.push(arrivalsLine());
  const approved = lastPayload.if_approved;
  if (approved && backlog.proposed) {
    children.push(el("p", { class: "fc-approved" }, [
      `If the ${backlog.proposed} proposed task${backlog.proposed === 1 ? " is" : "s are"} approved: drain ${range(approved.time_to_drain_hours, fmtHours)}, cost ${range(approved.usd, fmtUsd)}, tokens ${range(approved.tokens, fmtTokens)} (p50 – p90).`,
    ]));
  }
  return el("section", { class: "mx-card", "aria-label": "Backlog forecast" }, children);
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

function basisText(basis) {
  return Object.entries(basis || {})
    .map(([key, count]) => `${BASIS_LABELS[key] || key} ${count}`)
    .join(" · ");
}

function byComplexityCard() {
  const rows = (lastPayload.forecast?.by_complexity || []).map((row) => el("tr", {}, [
    el("th", { scope: "row", text: COMPLEXITY_LABELS[row.complexity] || row.complexity }),
    el("td", { class: "num", text: String(row.tasks) }),
    el("td", { class: "fc-basis", text: basisText(row.basis) }),
    el("td", { class: "num", text: range(row.work_hours, fmtHours) }),
    el("td", { class: "num", text: range(row.usd, fmtUsd) }),
    el("td", { class: "num", text: range(row.tokens, fmtTokens) }),
  ]));
  const content = rows.length
    ? table("Forecast by complexity", [["Complexity"], ["Tasks", true], ["Sampled from"], ["Work hours", true], ["Cost", true], ["Tokens", true]], rows, "p50 – p90 per complexity. Work hours sum every task's slot time, so they exceed the time to drain when runs overlap.")
    : el("div", { class: "mx-empty-note", text: "No backlog tasks." });
  return card("By complexity", null, [content]);
}

function coverageCard() {
  const coverage = lastPayload.coverage || {};
  const backlog = lastPayload.backlog || {};
  const items = [
    ["Unassessed", coverage.unassessed, "No complexity yet: sampled from every delivery."],
    ["Thin-cell fallbacks", coverage.thin_cell_fallbacks, `Fewer than ${lastPayload.inputs?.min_cell_samples ?? 5} deliveries of the same complexity and crew pool: sampled from a wider pool.`],
    ["Uncovered model", coverage.uncovered_model, "No sampled delivery has every run priced, so tokens are not forecast for these tasks."],
    ["Uncosted", coverage.uncosted, "No sampled delivery has every run's provider cost, so cost is not forecast for these tasks."],
    ["No history", coverage.no_history, "Nothing to sample at all."],
  ];
  const list = el("dl", { class: "fc-coverage" }, items.map(([label, value, hint]) => el("div", { class: "fc-cov" }, [
    el("dt", {}, [label, " ", infoHint(hint)]),
    el("dd", { class: value ? "fc-warn" : null, text: String(value ?? 0) }),
  ])));
  const left = `${backlog.review ?? 0} in review and ${backlog.blocked ?? 0} blocked are not forecast.`;
  return card("Coverage", `${coverage.tasks ?? 0} tasks forecast`, [list, el("p", { class: "mx-blurb", text: left })]);
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

function versus(forecast, actual, fmt) {
  return `${range(forecast, fmt)} → ${actual == null ? "—" : fmt(actual)}`;
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
  const taskLevel = calibration.task_level || {};
  const forecastLevel = calibration.forecast_level || {};
  const summary = table("Calibration summary", [["Measure"], ["Scored", true], ["Under p50", true], ["Under p90", true], ["Target"]], [
    hitRow("Task hours", taskLevel.hours, "50% · 90%"),
    hitRow("Task cost", taskLevel.usd, "50% · 90%"),
    hitRow("Task tokens", taskLevel.tokens, "50% · 90%"),
    hitRow("Time to drain", forecastLevel.time_to_drain_hours, "50% · 90%"),
    hitRow("Backlog cost", forecastLevel.usd, "50% · 90%"),
    hitRow("Backlog tokens", forecastLevel.tokens, "50% · 90%"),
  ]);
  const rows = forecasts.map((row) => el("tr", {}, [
    el("th", { scope: "row", text: formatDateTime(row.forecast_at) }),
    el("td", { class: "num", text: `${row.done}/${row.tasks}` }),
    el("td", { text: excludedText(row.excluded) }),
    el("td", { class: "num", text: versus(row.forecast?.time_to_drain_hours, row.actual?.time_to_drain_hours, fmtHours) }),
    el("td", { class: "num", text: versus(row.forecast?.usd, row.actual?.usd, fmtUsd) }),
    el("td", { class: "num", text: versus(row.forecast?.tokens, row.actual?.tokens, fmtTokens) }),
    el("td", { class: "fc-state", text: row.scored_whole ? "scored" : row.settled ? "settled, not scored" : `${row.pending} pending` }),
  ]));
  const excluded = calibration.excluded || {};
  const history = table("Stored forecasts", [["Recorded"], ["Done", true], ["Left out"], ["Drain p50 – p90 → actual", true], ["Cost", true], ["Tokens", true], ["State"]], rows);
  return card("Calibration", meta, [
    blurb,
    summary,
    el("p", { class: "mx-blurb", text: `Left out across stored forecasts: ${excludedText(excluded)}.` }),
    history,
  ]);
}
