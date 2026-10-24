// Analytics › Cost: managed-execution cost and token usage.
// The family leaderboard, narrative, agent cards and insights panel are gone.
// The page reads GET /api/cost: { window, orchestration, explore_comparison }.
// A header band (title, window, scope, range) sits above two tabs: Spend &
// tokens renders into #scoreboard-orchestration, Read-only exploration into
// #scoreboard-exploration. router.js owns which tab is shown; both bodies
// render on every response so a tab switch never refetches.
// Pure vanilla JS, split into ES modules with no build step.

import { el, syncNodes, fetchJson, getWindow, payloadHonorsWindow, wireWindowSelector, syncWindowSelectors, requestPanel, getWorkspaceRevision, onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime } from './common.js';

// The selector writes the shared dashboard window. app.js refresh is the
// single fetch path so Cost cannot drift onto a different cutoff.
function wireScoreboardWindowSelector() {
  // Selector writes the shared dashboard window even in aggregate view so the
  // URL stays authoritative; app.js skips the per-workspace fetch there.
  wireWindowSelector("scoreboard-window-selector", { allowAll: true });
}

const $ = (id) => document.getElementById(id);

const COST_CHROME = ["scoreboard-orchestration", "scoreboard-exploration"];
const COST_COUNTS = ["scoreboard-orchestration-count", "scoreboard-explore-count"];

function setWindowRange(text, title = "") {
  const node = $("scoreboard-window-range");
  if (!node) return;
  node.textContent = text;
  node.title = title;
}

function clearCostChrome() {
  for (const id of COST_CHROME) {
    const node = $(id);
    if (node) syncNodes(node, []);
  }
  for (const id of COST_COUNTS) {
    const node = $(id);
    if (node) node.textContent = "—";
  }
  setWindowRange("");
}

// Aggregate view has no per-workspace cost. Placeholder the panel so a
// previous workspace's dollars and tokens cannot stay on screen.
export function placeholdScoreboardAggregate() {
  for (const id of COST_CHROME) renderPanelPlaceholder(id);
  for (const id of COST_COUNTS) {
    const node = $(id);
    if (node) node.textContent = "—";
  }
  setWindowRange("");
}

let scoreboardRequestScope = null;

export function fetchAndRenderScoreboard() {
  if (isAggregateView()) {
    placeholdScoreboardAggregate();
    return Promise.resolve();
  }
  const selectedWindow = getWindow();
  const revision = getWorkspaceRevision();
  const path = `/api/cost?window=${encodeURIComponent(selectedWindow)}`;
  const scope = `${revision}\0${path}`;
  if (scoreboardRequestScope !== scope) {
    scoreboardRequestScope = scope;
    clearCostChrome();
  }
  return requestPanel("scoreboard-orchestration", path, () => fetchJson(path), (summary) => {
    if (getWorkspaceRevision() !== revision || getWindow() !== selectedWindow) return;
    if (!payloadHonorsWindow(summary, selectedWindow)) {
      console.error(
        `cost payload window ${summary && summary.window} rejected under ${selectedWindow} selection`,
      );
      return;
    }
    renderScoreboard(summary);
  }, "scoreboard-orchestration-count");
}

onWorkspaceChange(() => {
  scoreboardRequestScope = null;
  clearCostChrome();
});

const compactCountFormatter = new Intl.NumberFormat("en-US", {
  notation: "compact",
  maximumFractionDigits: 1,
});

function asScoreboardNumber(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function fmtScoreboardCount(value) {
  const num = asScoreboardNumber(value);
  return Math.abs(num) < 1000 ? String(num) : compactCountFormatter.format(num);
}

const usdFormatter = new Intl.NumberFormat("en-US", {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});
const wholeFormatter = new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 });

// Cents with thousands separators; a non-zero amount that rounds to $0.00
// reads "<$0.01" so it is not mistaken for no spend.
function formatUsd(value) {
  const amount = typeof value === "number" && Number.isFinite(value) ? value : 0;
  const sign = amount < 0 ? "-" : "";
  const magnitude = Math.abs(amount);
  if (magnitude > 0 && usdFormatter.format(magnitude) === "0.00") return `${sign}<$0.01`;
  return `${sign}$${usdFormatter.format(magnitude)}`;
}

// A population count in full, with thousands separators ("6,237").
function fmtScoreboardWhole(value) {
  return wholeFormatter.format(asScoreboardNumber(value));
}

function formatTokenDelta(current, previous) {
  if (!previous) return "lifetime window · no comparison baseline";
  const delta = asScoreboardNumber(current) - asScoreboardNumber(previous.normalized_token_total);
  return `${delta >= 0 ? "+" : ""}${fmtScoreboardCount(delta)} vs preceding ${getWindow()}`;
}

function pluralize(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function tokenStat(value, label) {
  return el("div", { class: "token-stat" }, [
    el("strong", { text: value }),
    el("span", { text: label }),
  ]);
}

function renderNormalizedTokenUsage(tokens, previous) {
  const usage = tokens || {};
  const total = asScoreboardNumber(usage.normalized_token_total);
  const invocations = asScoreboardNumber(usage.invocation_count);
  const tasks = asScoreboardNumber(usage.linked_task_count);
  const perInvocation = invocations === 0 ? "—" : fmtScoreboardCount(Math.round(total / invocations));
  const perTask = tasks === 0 ? "—" : fmtScoreboardCount(Math.round(total / tasks));
  const bucketText = [
    `uncached input ${fmtScoreboardCount(usage.uncached_input_tokens)}`,
    `cache read ${fmtScoreboardCount(usage.cache_read_tokens)}`,
    `5m cache write ${fmtScoreboardCount(usage.cache_create_tokens)}`,
    `1h cache write ${fmtScoreboardCount(usage.cache_create_1h_tokens)}`,
    `output ${fmtScoreboardCount(usage.output_tokens)}`,
  ].join(" · ");
  return el("section", { class: "scoreboard-token-usage" }, [
    el("div", { class: "scoreboard-token-usage-head" }, [
      el("span", { class: "token-label", text: "Normalized token usage" }),
      el("strong", { class: "token-total", text: fmtScoreboardCount(total) }),
      el("span", { class: "token-delta", text: formatTokenDelta(total, previous) }),
    ]),
    el("div", { class: "token-ratios" }, [
      tokenStat(fmtScoreboardCount(invocations), "invocations"),
      tokenStat(fmtScoreboardCount(tasks), "linked tasks"),
      tokenStat(perInvocation, invocations === 0 ? "tokens / invocation (0 invocations)" : "tokens / invocation"),
      tokenStat(perTask, tasks === 0 ? "tokens / linked task (0 linked tasks)" : "tokens / linked task"),
    ]),
    el("span", { class: "token-buckets", text: bucketText }),
    el("span", { class: "token-coverage", text: `${fmtScoreboardCount(usage.covered_invocation_count)} of ${fmtScoreboardCount(invocations)} invocations normalized · ${fmtScoreboardCount(usage.unknown_input_basis_or_model_count)} with unknown model or input basis are excluded, never guessed.` }),
  ]);
}

function bucketLabel(bucket) {
  switch (bucket?.kind) {
    case "orchestrator": return `Orchestrator: ${bucket.orchestrator || "unknown"}`;
    case "shared": return "Shared task ownership";
    case "unattributed": return "Unattributed task ownership";
    case "missing": return "Missing linked task";
    default: return "Unknown ownership";
  }
}

// One provider-reported or derived figure: its dollars (or "unknown") and the
// share of invocations it covers.
function costPopulationLine(label, total, knownCount, invocationCount, unknownCount, kind, primary = false) {
  const known = asScoreboardNumber(knownCount);
  const totalInvocations = asScoreboardNumber(invocationCount);
  const unknown = asScoreboardNumber(unknownCount);
  const className = primary ? "scoreboard-orchestration-cost primary" : "scoreboard-orchestration-cost";
  const coverage = `${fmtScoreboardWhole(known)} of ${fmtScoreboardWhole(totalInvocations)} ${kind}` +
    (known > 0 && unknown > 0 ? ` · ${fmtScoreboardWhole(unknown)} unavailable` : "");
  return el("div", { class: className }, [
    el("span", { class: "cost-label", text: label }),
    el("strong", { class: known === 0 ? "cost-value unknown" : "cost-value", text: known === 0 ? "unknown" : formatUsd(total) }),
    el("span", { class: "cost-coverage", text: coverage }),
  ]);
}

function renderOrchestrationBucket(bucket) {
  const invocationCount = asScoreboardNumber(bucket?.invocation_count);
  const comparableCount = asScoreboardNumber(bucket?.comparable_cost_count);
  const kind = ["orchestrator", "shared", "unattributed", "missing"].includes(bucket?.kind)
    ? bucket.kind
    : "unknown";
  const comparison = comparableCount > 0
    ? [
      el("span", { class: "cost-comparison" }, [
        el("span", {}, [
          el("span", { class: "k", text: "provider" }),
          el("strong", { text: formatUsd(bucket?.comparable_provider_cost_usd) }),
        ]),
        el("span", {}, [
          el("span", { class: "k", text: "derived" }),
          el("strong", { text: formatUsd(bucket?.comparable_derived_cost_usd) }),
        ]),
        el("span", {}, [
          el("span", { class: "k", text: "delta" }),
          el("strong", { text: formatUsd(bucket?.comparable_cost_delta_usd) }),
        ]),
      ]),
      el("span", { class: "cost-coverage", text: `${fmtScoreboardWhole(comparableCount)} of ${fmtScoreboardWhole(invocationCount)} invocations` }),
    ]
    : [el("span", { class: "cost-coverage", text: "No shared population: don't reconcile the partial sums." })];
  const models = Array.isArray(bucket?.models) && bucket.models.length > 0
    ? bucket.models.map((model) => `${model.model} ${fmtScoreboardCount(model?.tokens?.normalized_token_total)}`).join(" · ")
    : "Model attribution unavailable for normalized coverage.";
  return el("article", { class: `scoreboard-orchestration-bucket kind-${kind}` }, [
    el("div", { class: "scoreboard-orchestration-bucket-head" }, [
      el("span", { class: "ownership-mark" }),
      el("strong", { text: bucketLabel(bucket) }),
      el("span", { class: "bucket-meta", text: `${fmtScoreboardWhole(invocationCount)} managed invocations · ${fmtScoreboardWhole(bucket?.linked_task_count)} linked tasks` }),
    ]),
    el("div", { class: "scoreboard-orchestration-costs" }, [
      costPopulationLine(
        "Provider-reported",
        bucket?.provider_cost_usd,
        bucket?.provider_cost_count,
        invocationCount,
        bucket?.missing_provider_count,
        "reported",
        true,
      ),
      costPopulationLine(
        "Derived estimate",
        bucket?.derived_cost_usd,
        bucket?.derived_cost_count,
        invocationCount,
        bucket?.unpriced_derived_count,
        "priced",
      ),
    ]),
    el("div", { class: "scoreboard-orchestration-cost comparable" }, [
      el("span", { class: "cost-label", text: "Same-invocation comparison" }),
      ...comparison,
    ]),
    el("div", { class: "token-models" }, [
      el("span", { class: "token-models-label", text: "Model tokens · not a cross-provider ranking" }),
      el("span", { text: models }),
    ]),
  ]);
}

function emptyOrchestrationNode() {
  return el("div", { class: "empty-state" }, [
    el("div", { class: "icon", text: "✧" }),
    el("div", { class: "text", text: "No managed execution cost data yet." }),
  ]);
}

// The one-line window range for the header band, plus the exclusive-cutoff
// reading for its tooltip. Both ends share the clock's zone, shown once.
function paintWindowRange(orchestration) {
  if (!orchestration) {
    setWindowRange("");
    return;
  }
  const until = orchestration.until || orchestration.as_of;
  const from = orchestration.since ? formatDateTime(orchestration.since) : "all managed execution retained";
  const to = until ? formatDateTime(until) : "unknown cutoff";
  const zone = to.slice(to.lastIndexOf(" ") + 1);
  const start = from.endsWith(` ${zone}`) ? from.slice(0, -zone.length - 1) : from;
  setWindowRange(
    `${start} → ${to}`,
    `Window ${getWindow()}: ${from} ≤ invocation < ${to} (exclusive cutoff; as of ${orchestration.as_of ? formatDateTime(orchestration.as_of) : "unknown"}).`,
  );
}

function renderOrchestrationSummary(orchestration) {
  if (!orchestration || !Array.isArray(orchestration.buckets)) return [emptyOrchestrationNode()];
  const children = [
    el("p", { class: "scoreboard-orchestration-policy", text: "Provider-reported values come first. Derived values are labelled estimates with their own coverage. Compare the two only on the same-invocation population." }),
    renderNormalizedTokenUsage(orchestration.normalized_tokens, orchestration.previous_normalized_tokens),
  ];
  if (orchestration.buckets.length === 0) {
    children.push(el("div", { class: "empty-state" }, [el("div", { class: "text", text: "No managed invocations in this bounded window." })]));
  } else {
    children.push(el("div", { class: "scoreboard-orchestration-buckets" }, orchestration.buckets.map(renderOrchestrationBucket)));
  }
  return children;
}

function finiteOrNull(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function formatDurationMs(value) {
  const ms = finiteOrNull(value);
  if (ms === null) return "—";
  const minutes = ms / 60000;
  return minutes >= 1 ? `${minutes.toFixed(1)} min` : `${Math.round(ms / 1000)} s`;
}

function exploreCohortCells(cohort) {
  const runs = asScoreboardNumber(cohort?.runs);
  if (runs === 0) return [el("td", { text: "0" }), el("td", { text: "—" }), el("td", { text: "—" }), el("td", { text: "—" })];
  const priced = asScoreboardNumber(cohort.priced_runs);
  const known = asScoreboardNumber(cohort.footprint_known_runs);
  const rate = finiteOrNull(cohort.footprint_miss_rate);
  return [
    el("td", { text: fmtScoreboardCount(runs) }),
    el("td", { text: formatDurationMs(cohort.mean_duration_ms) }),
    el("td", { text: cohort.mean_cost_usd == null ? "—" : `${formatUsd(cohort.mean_cost_usd)} (n=${priced})` }),
    el("td", { text: rate === null ? "—" : `${Math.round(rate * 100)}% (${fmtScoreboardCount(cohort.footprint_miss_runs)}/${known})` }),
  ];
}

// Implement runs with and without read-only explorer calls, per implementing
// crew. Each cohort shows its sample count so a thin cohort reads as such.
function renderExploreComparison(comparison) {
  const crews = Array.isArray(comparison?.crews) ? comparison.crews : [];
  const description = el("p", { class: "scoreboard-explore-description", text: "Implement runs that delegated explorer calls against those that did not, per implementing crew. Cost is the implementing crew's own; explorer spend is shown separately." });
  if (crews.length === 0) {
    return [
      description,
      el("div", { class: "empty-state" }, [el("div", { class: "text", text: "No implement runs in this window." })]),
    ];
  }
  const head = el("tr", {}, [
    el("th", { text: "crew" }),
    el("th", { text: "cohort" }),
    el("th", { text: "runs" }),
    el("th", { text: "mean duration" }),
    el("th", { text: "mean cost" }),
    el("th", { text: "footprint misses" }),
    el("th", { text: "explorer calls · cost" }),
  ]);
  const rows = [];
  for (const row of crews) {
    for (const [label, cohort] of [["with explore", row.with_explore], ["without", row.without_explore]]) {
      const calls = asScoreboardNumber(cohort?.explore_calls);
      rows.push(el("tr", {}, [
        el("td", { class: "crew", text: label === "with explore" ? row.crew : "" }),
        el("td", { text: label }),
        ...exploreCohortCells(cohort),
        el("td", { text: calls === 0 ? "—" : `${fmtScoreboardCount(calls)} · ${formatUsd(cohort.explore_cost_usd)}` }),
      ]));
    }
  }
  const children = [
    description,
    el("div", { class: "scoreboard-table-wrap scoreboard-explore-table" }, [
      el("table", { class: "scoreboard-table" }, [el("thead", {}, [head]), el("tbody", {}, rows)]),
    ]),
  ];
  if (comparison.truncated) {
    children.push(el("p", { class: "scoreboard-explore-footnote", text: `Reads the newest ${fmtScoreboardCount(comparison.runs_scanned)} implement runs; older runs in this window are not counted.` }));
  }
  return children;
}

function renderScoreboard(summary) {
  // Idempotent attach of the window-selector click handler (guarded
  // internally so re-renders don't double-bind).
  wireScoreboardWindowSelector();
  syncWindowSelectors();

  // Never paint a 24h body under an active 7d (or any other) selection.
  // The fetch path also guards this; keep the renderer honest.
  if (summary && !payloadHonorsWindow(summary, getWindow())) {
    console.error(
      `cost payload window ${summary.window} rejected under ${getWindow()} selection`,
    );
    return;
  }

  paintWindowRange(summary?.orchestration);
  const orchestrationHost = $("scoreboard-orchestration");
  if (orchestrationHost) syncNodes(orchestrationHost, renderOrchestrationSummary(summary?.orchestration));
  const explorationHost = $("scoreboard-exploration");
  if (explorationHost) syncNodes(explorationHost, renderExploreComparison(summary?.explore_comparison));
  const orchestrationCount = $("scoreboard-orchestration-count");
  if (orchestrationCount) {
    const count = Array.isArray(summary?.orchestration?.buckets)
      ? summary.orchestration.buckets.length
      : 0;
    orchestrationCount.textContent = count === 0 ? "—" : pluralize(count, "bucket");
  }
  const exploreCount = $("scoreboard-explore-count");
  if (exploreCount) {
    const count = Array.isArray(summary?.explore_comparison?.crews)
      ? summary.explore_comparison.crews.length
      : 0;
    exploreCount.textContent = count === 0 ? "—" : pluralize(count, "crew");
  }
}

export { renderScoreboard };
