// Window summaries and breakdowns beneath the selected Metrics trend.
import { bindTaskLink, el, formatDateTime, pinnedInfoHint } from './common.js';
import { sparkline as lineSparkline } from './sparkline.js';
import { COMPLEXITY_LABELS, delta, fmtMs, previousLabel, seriesLabel, tasks } from './metrics-running.js';
import { fmtTokens } from './metrics-tokens.js';
import { fmtTurns } from './metrics-turns.js';
import { fmtLines } from './metrics-diff.js';
import { fmtAmount } from './metrics-friction.js';

const LOW_ACTOR_SAMPLE = 5;
const ACTORS_SHOWN = 7;
const COMPLEXITY_TABS = ["low", "medium", "hard", "xhard", "unset"];
const PROVIDERS = new Set(["claude", "codex", "gemini", "grok"]);

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

function card(title, meta, children, className = "", hint) {
  return el("section", { class: `mx-card ${className}`.trim(), "aria-label": title }, [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title" }, [title, hint ? pinnedInfoHint(hint) : null]),
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

function actorsCard(payload, state, rerender) {
  const actors = payload.actors || [];
  const prevText = previousLabel(payload.window.key);
  const meta = `window ${payload.window.key} · sorted by runs${prevText ? ` · Δ = avg vs ${prevText}` : ""}`;
  if (!actors.length) return card("Implementation time by actor", meta, [emptyNote("No implementation runs in this window.")], "mx-wide");
  let sparkMax = 0;
  for (const actor of actors) for (const v of actor.trend || []) if (v != null) sparkMax = Math.max(sparkMax, v);
  const rows = (state.allActors ? actors : actors.slice(0, ACTORS_SHOWN)).map((actor) => {
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
    const toggle = el("button", { class: "mx-link-btn", type: "button", text: state.allActors ? `Show top ${ACTORS_SHOWN}` : `Show all ${actors.length} actors` });
    toggle.addEventListener("click", () => { state.allActors = !state.allActors; rerender(); });
    children.push(el("div", {}, [toggle]));
  }
  return card("Implementation time by actor", meta, children, "mx-wide");
}

function slowestCard(payload) {
  const slowest = payload.slowest || [];
  if (!slowest.length) return card("Slowest runs", `window ${payload.window.key}`, [emptyNote("No runs in this window.")], "mx-narrow");
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
  return card("Slowest runs", `window ${payload.window.key}`, [el("ol", { class: "mx-slow-list" }, items)], "mx-narrow");
}

function complexityActorsCard(payload, state, rerender) {
  const bands = payload.implement_one_by_complexity || [];
  const byKey = new Map(bands.map((band) => [band.complexity, band]));
  const selected = state.complexity && byKey.has(state.complexity)
    ? state.complexity
    : (["medium", "hard", "low", "xhard", "unset"].find((key) => byKey.has(key)) || "medium");
  const tabs = el("div", { class: "mx-cx-tabs", role: "group", "aria-label": "Complexity" }, COMPLEXITY_TABS.map((key) => {
    const band = byKey.get(key);
    const on = key === selected;
    const button = el("button", { class: on ? "mx-cx-tab on" : "mx-cx-tab", type: "button", "aria-pressed": on ? "true" : "false" }, [
      key,
      el("span", { class: "mx-cx-n", text: String(band ? band.n : 0) }),
    ]);
    button.disabled = !band;
    button.addEventListener("click", () => { state.complexity = key; rerender(); });
    return button;
  }));
  const band = byKey.get(selected);
  if (!band) return card("Actors within a complexity", "last 30d · bar = avg, tick = p95", [tabs, emptyNote("No implementation runs in the last 30d.")], "mx-wide");
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

function outcomeCard(payload) {
  const empty = panelStatus("Outcome by complexity", payload, "mx-narrow");
  if (empty) return empty;
  const rows = payload.by_complexity || [];
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

function panelStatus(title, payload, className) {
  if (!payload || payload.unavailable) {
    return card(title, null, [emptyNote(payload?.unavailable || "Loading…")], className);
  }
  return null;
}

function panelTable(headers, rows) {
  const head = headers.map((text, index) => el("th", { scope: "col", class: index ? "num" : "", text }));
  const body = rows.map(([label, ...values]) => el("tr", {}, [
    el("th", { scope: "row", text: label }),
    ...values.map((value) => el("td", { class: "num", text: value == null ? "—" : String(value) })),
  ]));
  return el("table", { class: "mx-table mx-panel-table" }, [
    el("thead", {}, [el("tr", {}, head)]),
    el("tbody", {}, body),
  ]);
}

function summaryCard(title, className, data, headers, rows, empty, hint) {
  return panelStatus(title, data, className) || card(title, null, [
    rows.length ? panelTable(headers, rows) : emptyNote(empty),
  ], className, hint);
}

function comparisonHeaders(payload, label, change = false) {
  return [label, payload.previous ? "Current" : "Total",
    ...(payload.previous ? ["Prior", ...(change ? ["Change"] : [])] : [])];
}

function completedCard(payload) {
  const title = "Completed by complexity";
  const pending = panelStatus(title, payload, "mx-wide");
  if (pending) return pending;
  const { current, previous } = payload;
  const rows = ["low", "medium", "hard", "other"].map((key) => [
    COMPLEXITY_LABELS[key] || "Other complexity", current.done[key], ...(previous ? [previous.done[key]] : []),
  ]);
  rows.push(["Rejected", current.rejected, ...(previous ? [previous.rejected] : [])]);
  rows.push(["Archived", current.archived, ...(previous ? [previous.archived] : [])]);
  return summaryCard(title, "mx-wide", payload, comparisonHeaders(payload, "Tasks"),
    rows.some(([, ...counts]) => counts.some(Boolean)) ? rows : [],
    previous ? "No tasks closed in either window." : "No tasks closed in this window.",
    "Complexity rows count tasks that reached done. Other complexity includes xhard and unassessed tasks. The previous window has the same length as the current one.");
}

function defectOriginCard(payload, originLabels) {
  const title = "Defects by origin";
  const pending = panelStatus(title, payload, "mx-wide");
  if (pending) return pending;
  const { current, previous } = payload;
  const keys = new Set([...Object.keys(current.origin), ...Object.keys(previous?.origin || {})]);
  const ranked = [...keys].map((key) => [originLabels[key] || key, current.origin[key] || 0, previous?.origin[key] || 0])
    .filter(([, now, prior]) => now || prior)
    .sort(([left, now, prior], [right, next, nextPrior]) => next - now || nextPrior - prior || left.localeCompare(right));
  const rows = ranked.map(([label, now, prior]) => {
    if (!previous) return [label, now];
    const change = now - prior;
    return [label, now, prior, change > 0 ? `+${change}` : change === 0 ? "—" : `−${Math.abs(change)}`];
  });
  return summaryCard(title, "mx-wide", payload, comparisonHeaders(payload, "Origin", true), rows,
    previous ? "No defects filed in either window." : "No defects filed in this window.", "Change is the difference in defect counts from the previous window of the same length.");
}

function defectLineageCard(payload, completions, lineageSeries) {
  const title = "Defects by lineage";
  const pending = panelStatus(title, payload, "mx-narrow");
  if (pending) return pending;
  const done = completions?.current?.done.total;
  const rate = done ? (payload.current.total / done).toLocaleString(undefined, { maximumSignificantDigits: 3 }) : "—";
  const rows = lineageSeries.map(({ key, label }) => [label, payload.current.lineage[key] || 0]);
  const items = payload.current.total ? [panelTable(["Lineage", "Defects"], rows)] : [emptyNote("No defects filed in this window.")];
  if (done || payload.current.total) {
    items.push(panelTable(["Rate", "Value"], [["Defects per completed task", rate]]));
  }
  return card(title, null, items, "mx-narrow", completions?.unavailable
    ? `Completion count unavailable. ${completions.unavailable}`
    : "Defects filed divided by tasks that reached done in the same window. No rate is available when no tasks reached done.");
}

function tokenComplexityCard(payload) {
  const rows = (payload?.series || []).filter(({ total }) => total.n).map((entry) => [
    seriesLabel(entry), entry.total.n, fmtTokens(entry.total.median), fmtTokens(entry.total.mean),
  ]);
  return summaryCard("Tokens by complexity", "mx-wide", payload,
    ["Complexity", "Tasks", "Median", "Mean"], rows, "No tasks with measured tokens in this window.",
    "Median and mean normalized tokens per task within each complexity and tag cohort.");
}

function tokenCoverageCard(payload) {
  const total = payload?.total;
  const rows = total ? [
    ["Tasks counted", total.n],
    ["Uncovered model", total.excluded_uncovered],
    ["No linked invocation", total.excluded_no_invocations],
    ["Another implementing model", payload.excluded_other_model],
  ] : [];
  return summaryCard("Measurement coverage", "mx-narrow", payload, ["Result", "Tasks"],
    rows.some(([, count]) => count > 0) ? rows : [], "No matching tasks closed in this window.",
    "An uncovered model lacks normalized token data. Invocation counts follow the selected invocation scope.");
}

function turnsCoverageCard(payload) {
  const title = "Measurement coverage";
  const pending = panelStatus(title, payload, "mx-narrow");
  if (pending) return pending;
  const c = payload.coverage;
  if (!c) return card(title, null, [emptyNote("This host did not report trace coverage.")], "mx-narrow");
  const rate = (n) => n == null ? "—" : n.toLocaleString(undefined, { style: "percent", maximumFractionDigits: 1 });
  // The overall sample size sits beside the measured rate (STD-07 §R29). The
  // API's coverage cohort is already scoped to the selected provider.
  const measured = c.tasks ? `${rate(c.measured_rate)} (${c.measured} of ${tasks(c.tasks)})` : rate(c.measured_rate);
  const rows = [
    ["Measured", measured],
    ["Trace coverage", rate(c.trace_rate)],
    ["Empty or unavailable trace", c.empty_trace],
    ["Incomplete trace", c.incomplete_trace ?? "—"],
    ["No detectable edit", c.no_detectable_edit],
    ["No implement invocation", c.no_invocation],
  ];
  if (payload.provider) {
    rows.push(["Unattributed", payload.excluded_unattributed], ["Another provider", payload.excluded_other_provider]);
  }
  const hasTasks = c.tasks || payload.excluded_unattributed || payload.excluded_other_provider;
  return summaryCard(title, "mx-narrow", payload, ["Result", "Value"], hasTasks ? rows : [],
    "No matching tasks closed in this window.",
    "Measured is the share of matching tasks with a first edit recorded. Trace coverage is the share of first implementation invocations with a populated trace. A provider filter excludes tasks with no attributable invocation.");
}

function turnsProviderCard(payload) {
  const title = "Turns to first edit by provider";
  const pending = panelStatus(title, payload, "mx-wide");
  if (pending) return pending;
  if (!Array.isArray(payload.provider_summary)) return card(title, null, [emptyNote("This host did not report provider turns.")], "mx-wide");
  const providers = payload.provider_summary;
  if (!providers.some((entry) => entry.n > 0)) {
    const hasWindowMeasurements = payload.available_providers?.some((entry) => entry.coverage?.measured > 0);
    const emptyText = payload.tags?.length && hasWindowMeasurements
      ? "Provider comparison has no matching measurements among the selected tags in this window."
      : "No measured tasks in this window.";
    return card(title, null, [emptyNote(emptyText)], "mx-wide",
      "Rate is measured tasks divided by provider tasks. Compared tags select matching tasks; the provider filter does not narrow this comparison.");
  }
  const rows = providers.map((entry) => el("tr", { class: entry.thin ? "mx-dim" : "" }, [
    el("th", { scope: "row", text: entry.provider + (entry.thin ? " *" : "") }),
    ...[entry.n, fmtTurns(entry.median_turns), fmtTurns(entry.mean_turns), entry.measured_rate == null ? "—" : `${Math.round(entry.measured_rate * 100)}%`].map((value) => el("td", { class: "num", text: String(value) })),
  ]));
  const headers = ["Provider", "Measured", "Median", "Mean", "Rate"].map((label, i) => el("th", { scope: "col", class: i ? "num" : "", text: label }));
  return card(title, null, [el("div", { class: "mx-table-wrap" }, [el("table", { class: "mx-table mx-panel-table" }, [el("thead", {}, [el("tr", {}, headers)]), el("tbody", {}, rows)])]),
    el("div", { class: "mx-muted", text: `* Fewer than ${payload.min_tasks} measured tasks` })], "mx-wide",
    "Rate is measured tasks divided by provider tasks. Compared tags select matching tasks; the provider filter does not narrow this comparison.");
}

function linesByCrewCard(payload) {
  const crewRows = (payload?.crews || []).filter(({ total }) => total.n).map(({ key, total }) => {
    const { code_added, tests_added, code_deleted, tests_deleted, net } = total.sum;
    return [key === "other" ? "Other crews" : key === "unknown" ? "Unknown crew" : key,
      code_added + tests_added, code_deleted + tests_deleted, net];
  }).sort(([left, , , net], [right, , , nextNet]) => nextNet - net || left.localeCompare(right));
  const rows = crewRows.map(([label, ...lines]) => [label, ...lines.map(fmtLines)]);
  return summaryCard("Lines changed by crew", "mx-wide", payload, ["Crew", "Added", "Deleted", "Net"],
    rows, "No landed deliveries with recorded lines in this window.", "Totals include code and tests.");
}

function deliveriesCountedCard(payload) {
  const overall = payload?.overall;
  const rows = overall?.total.n ? [["Landed deliveries", overall.total.n],
    ["Thin buckets", overall.points.filter((point) => point.thin).length]] : [];
  return summaryCard("Deliveries counted", "mx-narrow", payload, ["Metric", "Count"],
    rows, "No landed deliveries with recorded lines in this window.",
    "A thin bucket contains fewer deliveries than the threshold for a solid point on the trend.");
}

function frictionFamilyCard(payload) {
  const rows = (payload?.families || []).map(({ label, unit, total, previous }) => {
    const format = (count) => `${fmtAmount(unit, count)}${unit === "tokens" ? " tokens" : ""}`;
    return [label, format(total.count), ...(previous ? [format(previous.count)] : [])];
  });
  const hasFriction = payload?.families?.some(({ total, previous }) => total.count || previous?.count);
  const hasPrevious = payload?.families?.some(({ previous }) => previous);
  return summaryCard("Friction by family", "", payload, ["Family", hasPrevious ? "Current" : "Total", ...(hasPrevious ? ["Prior"] : [])],
    hasFriction ? rows : [], "No occurrences or recorded usage for the selected families.",
    "These are window totals for the selected families. Recoveries and merge conflicts are workspace-wide when crew or complexity is selected.");
}

const durationPanelState = { allActors: false, complexity: null };

function panelPair(wide, narrow) {
  return el("div", { class: "mx-row" }, [wide, narrow]);
}

export function metricsPanelRows(trend, payload, rerender, { originLabels, lineageSeries }) {
  const { durations, completions, outcome, defects, tokens, turns, diff, friction } = payload;
  switch (trend) {
    case "duration":
      return [
        ["metrics-duration-actors", panelPair(
          panelStatus("Implementation time by actor", durations, "mx-wide") || actorsCard(durations, durationPanelState, rerender),
          panelStatus("Slowest runs", durations, "mx-narrow") || slowestCard(durations)),
        [durations?.window, durations?.actors, durations?.slowest, durations?.unavailable, durationPanelState.allActors]],
        ["metrics-duration-complexity", panelStatus("Actors within a complexity", durations)
          || complexityActorsCard(durations, durationPanelState, rerender),
        [durations?.implement_one_by_complexity, durations?.unavailable, Boolean(durations), durationPanelState.complexity]],
      ];
    case "completed":
      return [["metrics-completed", panelPair(completedCard(completions), outcomeCard(outcome)),
        [completions?.current, completions?.previous, completions?.unavailable, outcome]]];
    case "defects":
      return [["metrics-defects", panelPair(defectOriginCard(defects, originLabels), defectLineageCard(defects, completions, lineageSeries)),
        [defects?.current, defects?.previous, defects?.unavailable, completions?.current?.done.total, completions?.unavailable]]];
    case "tokens":
    case "tokens-total":
      return [["metrics-tokens", panelPair(tokenComplexityCard(tokens), tokenCoverageCard(tokens)),
        [tokens?.series?.map(({ complexity, cohort, total }) => [complexity, cohort, total]), tokens?.total, tokens?.excluded_other_model, tokens?.unavailable]]];
    case "turns":
      return [["metrics-turns", panelPair(turnsProviderCard(turns), turnsCoverageCard(turns)),
        [turns?.provider_summary, turns?.min_tasks, turns?.coverage, turns?.provider, turns?.excluded_unattributed, turns?.excluded_other_provider, turns?.unavailable, Boolean(turns)]]];
    case "diff":
      return [["metrics-diff", panelPair(linesByCrewCard(diff), deliveriesCountedCard(diff)),
        [diff?.crews?.map(({ key, total }) => [key, total]), diff?.overall?.total.n, diff?.overall?.points.map(({ thin }) => thin), diff?.unavailable]]];
    case "friction":
      return [["metrics-friction", frictionFamilyCard(friction),
        [friction?.families?.map(({ label, unit, total, previous }) => [label, unit, total.count, previous?.count]), friction?.unavailable]]];
  }
}
