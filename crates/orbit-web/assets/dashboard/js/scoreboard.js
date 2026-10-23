// Analytics › Cost: managed-execution cost and token usage.
// The family leaderboard, narrative, agent cards and insights panel are gone.
// The page reads GET /api/cost: { window, orchestration, explore_comparison }.
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

const COST_CHROME = ["scoreboard-orchestration"];
const COST_COUNTS = ["scoreboard-orchestration-count"];

function clearCostChrome() {
  for (const id of COST_CHROME) {
    const node = $(id);
    if (node) syncNodes(node, []);
  }
  for (const id of COST_COUNTS) {
    const node = $(id);
    if (node) node.textContent = "—";
  }
}

// Aggregate view has no per-workspace cost. Placeholder the panel so a
// previous workspace's dollars and tokens cannot stay on screen.
export function placeholdScoreboardAggregate() {
  for (const id of COST_CHROME) renderPanelPlaceholder(id);
  for (const id of COST_COUNTS) {
    const node = $(id);
    if (node) node.textContent = "—";
  }
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

function formatUsd(value) {
  const amount = typeof value === "number" && Number.isFinite(value) ? value : 0;
  return `$${amount.toFixed(4)}`;
}

function formatTokenDelta(current, previous) {
  if (!previous) return "lifetime window · no comparison baseline";
  const delta = asScoreboardNumber(current) - asScoreboardNumber(previous.normalized_token_total);
  return `${delta >= 0 ? "+" : ""}${fmtScoreboardCount(delta)} vs preceding equal window`;
}

function renderNormalizedTokenUsage(tokens, previous) {
  const usage = tokens || {};
  const total = asScoreboardNumber(usage.normalized_token_total);
  const invocations = asScoreboardNumber(usage.invocation_count);
  const tasks = asScoreboardNumber(usage.linked_task_count);
  const perInvocation = invocations === 0 ? "— (0 invocations)" : fmtScoreboardCount(Math.round(total / invocations));
  const perTask = tasks === 0 ? "— (0 linked tasks)" : fmtScoreboardCount(Math.round(total / tasks));
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
    el("span", { class: "token-buckets", text: bucketText }),
    el("div", { class: "token-ratios" }, [
      el("span", { text: `${fmtScoreboardCount(invocations)} invocations` }),
      el("span", { text: `${fmtScoreboardCount(tasks)} linked tasks` }),
      el("span", { text: `${perInvocation} tokens / invocation` }),
      el("span", { text: `${perTask} tokens / linked task` }),
    ]),
    el("span", { class: "token-coverage", text: `${fmtScoreboardCount(usage.covered_invocation_count)}/${fmtScoreboardCount(invocations)} invocations normalized · ${fmtScoreboardCount(usage.unknown_input_basis_or_model_count)} unknown model/input basis (excluded, never guessed).` }),
  ]);
}

function bucketLabel(bucket) {
  switch (bucket?.kind) {
    case "orchestrator": return `named orchestrator: ${bucket.orchestrator || "unknown"}`;
    case "shared": return "shared task ownership";
    case "unattributed": return "unattributed task ownership";
    case "missing": return "missing linked task";
    default: return "unknown ownership";
  }
}

function costPopulationLine(label, total, knownCount, invocationCount, unknownCount, kind, primary = false) {
  const known = asScoreboardNumber(knownCount);
  const totalInvocations = asScoreboardNumber(invocationCount);
  const unknown = asScoreboardNumber(unknownCount);
  const className = primary ? "scoreboard-orchestration-cost primary" : "scoreboard-orchestration-cost";
  if (known === 0) {
    return el("div", { class: className }, [
      el("span", { class: "cost-label", text: label }),
      el("strong", { class: "cost-value unknown", text: "unknown" }),
      el("span", { class: "cost-coverage", text: `0/${totalInvocations} ${kind} · ${unknown} unavailable` }),
    ]);
  }
  return el("div", { class: className }, [
    el("span", { class: "cost-label", text: label }),
    el("strong", { class: "cost-value", text: formatUsd(total) }),
    el("span", { class: "cost-coverage", text: `${known}/${totalInvocations} ${kind} · ${unknown} unavailable` }),
  ]);
}

function renderOrchestrationBucket(bucket) {
  const invocationCount = asScoreboardNumber(bucket?.invocation_count);
  const comparableCount = asScoreboardNumber(bucket?.comparable_cost_count);
  const kind = ["orchestrator", "shared", "unattributed", "missing"].includes(bucket?.kind)
    ? bucket.kind
    : "unknown";
  const costs = [
    costPopulationLine(
      "provider-reported",
      bucket?.provider_cost_usd,
      bucket?.provider_cost_count,
      invocationCount,
      bucket?.missing_provider_count,
      "reported",
      true,
    ),
    costPopulationLine(
      "derived estimate",
      bucket?.derived_cost_usd,
      bucket?.derived_cost_count,
      invocationCount,
      bucket?.unpriced_derived_count,
      "priced",
    ),
  ];
  if (comparableCount > 0) {
    costs.push(el("div", { class: "scoreboard-orchestration-cost comparable" }, [
      el("span", { class: "cost-label", text: "comparable same-invocation population" }),
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
      el("span", { class: "cost-coverage", text: `${comparableCount}/${invocationCount} invocations` }),
    ]));
  } else {
    costs.push(el("div", { class: "scoreboard-orchestration-cost comparable" }, [
      el("span", { class: "cost-label", text: "comparable" }),
      el("strong", { class: "cost-value unknown", text: "unknown" }),
      el("span", { class: "cost-coverage", text: "no shared provider/derived population · do not reconcile partial sums" }),
    ]));
  }
  return el("article", { class: `scoreboard-orchestration-bucket kind-${kind}` }, [
    el("div", { class: "scoreboard-orchestration-bucket-head" }, [
      el("span", { class: "ownership-mark" }),
      el("strong", { text: bucketLabel(bucket) }),
    ]),
    el("span", { class: "bucket-meta", text: `${invocationCount} managed invocations · ${asScoreboardNumber(bucket?.linked_task_count)} linked tasks` }),
    el("div", { class: "scoreboard-orchestration-costs" }, costs),
    Array.isArray(bucket?.models) && bucket.models.length > 0
      ? el("span", { class: "token-models", text: `Model attribution (not a cross-provider ranking): ${bucket.models.map((model) => `${model.model} ${fmtScoreboardCount(model?.tokens?.normalized_token_total)}`).join(" · ")}` })
      : el("span", { class: "token-models", text: "Model attribution unavailable for normalized coverage." }),
  ]);
}

function emptyOrchestrationNode() {
  return el("div", { class: "empty-state" }, [
    el("div", { class: "icon", text: "✧" }),
    el("div", { class: "text", text: "No managed execution cost data yet." }),
  ]);
}

function renderOrchestrationSummary(orchestration) {
  if (!orchestration || !Array.isArray(orchestration.buckets)) return emptyOrchestrationNode();
  const since = orchestration.since ? formatDateTime(orchestration.since) : "all managed execution retained";
  const until = orchestration.until || orchestration.as_of ? formatDateTime(orchestration.until || orchestration.as_of) : "unknown cutoff";
  const children = [
    el("div", { class: "scoreboard-orchestration-context" }, [
      el("div", { class: "scoreboard-orchestration-scope" }, [
        el("span", { class: "scope-label", text: "Managed execution only" }),
        el("span", { class: "scope-badge", text: `window ${getWindow()}` }),
        el("span", { text: "Direct interactive Codex or Claude orchestration-session overhead is excluded." }),
      ]),
      el("div", { class: "scoreboard-orchestration-window", text: `Window ${getWindow()}: ${since} ≤ invocation < ${until} (exclusive cutoff; as of ${orchestration.as_of ? formatDateTime(orchestration.as_of) : "unknown"}).` }),
      el("p", { class: "scoreboard-orchestration-policy", text: "Provider-first estimate policy: provider-reported values are primary; derived values remain explicitly labeled estimates with their own coverage. Only the explicitly comparable same-invocation population is safe to compare." }),
    ]),
    renderNormalizedTokenUsage(orchestration.normalized_tokens, orchestration.previous_normalized_tokens),
  ];
  if (orchestration.buckets.length === 0) {
    children.push(el("div", { class: "empty-state" }, [el("div", { class: "text", text: "No managed invocations in this bounded window." })]));
  } else {
    children.push(el("div", { class: "scoreboard-orchestration-buckets" }, orchestration.buckets.map(renderOrchestrationBucket)));
  }
  return el("section", { class: "scoreboard-orchestration-summary" }, children);
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
  const heading = el("div", { class: "scoreboard-orchestration-scope" }, [
    el("span", { class: "scope-label", text: "Read-only exploration" }),
    el("span", { text: "Implement runs that delegated explorer calls against those that did not, per implementing crew. Cost is the implementing crew's own; explorer spend is shown separately." }),
  ]);
  if (crews.length === 0) {
    return el("section", { class: "scoreboard-explore-comparison" }, [
      heading,
      el("div", { class: "empty-state" }, [el("div", { class: "text", text: "No implement runs in this window." })]),
    ]);
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
        el("td", { text: label === "with explore" ? row.crew : "" }),
        el("td", { text: label }),
        ...exploreCohortCells(cohort),
        el("td", { text: calls === 0 ? "—" : `${fmtScoreboardCount(calls)} · ${formatUsd(cohort.explore_cost_usd)}` }),
      ]));
    }
  }
  const children = [
    heading,
    el("div", { class: "scoreboard-table-wrap" }, [
      el("table", { class: "scoreboard-table" }, [el("thead", {}, [head]), el("tbody", {}, rows)]),
    ]),
  ];
  if (comparison.truncated) {
    children.push(el("p", { class: "scoreboard-orchestration-policy", text: `Read the newest ${fmtScoreboardCount(comparison.runs_scanned)} implement runs; older runs in this window are not counted.` }));
  }
  return el("section", { class: "scoreboard-explore-comparison" }, children);
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

  const orchestrationHost = $("scoreboard-orchestration");
  const orchestrationCount = $("scoreboard-orchestration-count");
  if (orchestrationHost) {
    syncNodes(orchestrationHost, [
      renderOrchestrationSummary(summary?.orchestration),
      renderExploreComparison(summary?.explore_comparison),
    ]);
  }
  if (orchestrationCount) {
    const count = Array.isArray(summary?.orchestration?.buckets)
      ? summary.orchestration.buckets.length
      : 0;
    orchestrationCount.textContent = count === 0 ? "—" : `${count} buckets`;
  }
}

export { renderScoreboard };
