// Orbit dashboard scoreboard-domain rendering.
// Pure vanilla JS, split into ES modules with no build step.

import { el, syncNodes, getWindow, payloadHonorsWindow, wireWindowSelector, syncWindowSelectors } from './common.js';
import { navigateToDrilldown } from './audit.js';

// ORB-00337/ORB-10872: selector writes the shared dashboard window; app.js
// refresh is the single fetch path so delivery/operations and Managed
// Execution cannot drift onto different cutoffs.
function wireScoreboardWindowSelector() {
  // Selector writes shared dashboard window even in aggregate view so the
  // URL stays authoritative; app.js skips the per-workspace fetch there.
  wireWindowSelector("scoreboard-window-selector", { allowAll: true });
}

const $ = (id) => document.getElementById(id);

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

function formatScoreboardPair(agent, col) {
  const rawLeft = col.leftCompute ? col.leftCompute(agent) : readPath(agent, col.left);
  const rawRight = col.rightCompute ? col.rightCompute(agent) : readPath(agent, col.right);
  // ORB-11201: `null` means the source read failed for this window, distinct
  // from a genuinely observed zero. Coercing it away here would reintroduce
  // exactly the confusion that fix eliminated in the API payload.
  const unavailable = rawLeft === null || rawRight === null;
  const left = asScoreboardNumber(rawLeft);
  const right = asScoreboardNumber(rawRight);
  return {
    left,
    right,
    unavailable,
    text: unavailable ? "unavailable" : `${fmtScoreboardCount(left)}/${fmtScoreboardCount(right)}`,
    zero: !unavailable && left === 0 && right === 0,
    title: unavailable
      ? `${col.title}: unavailable (source failed for this window; not a measured zero)`
      : `${col.title}: ${left} / ${right}`,
  };
}

const CANONICAL_SCOREBOARD_FAMILIES = ["codex", "claude", "gemini", "grok"];

const AGENT_THEME = {
  codex: { color: "var(--ag-codex)", low: "var(--ag-codex-low)" },
  claude: { color: "var(--ag-claude)", low: "var(--ag-claude-low)" },
  gemini: { color: "var(--ag-gemini)", low: "var(--ag-gemini-low)" },
  grok: { color: "var(--ag-grok)", low: "var(--ag-grok-low)" },
};

const AGENT_CARD_KPIS = [
  { key: "tasks_created", label: "created", tone: "normal" },
  { key: "tasks_planned", label: "planned", tone: "normal" },
  { key: "tasks_completed", label: "completed", tone: "normal" },
  {
    key: "friction.reported",
    label: "friction",
    tone: "warn",
    compute: (agent) => agent?.friction?.reported ?? 0,
  },
];

const DELIVERY_SCOREBOARD_COLUMNS = [
  { key: "agent", label: "agent", num: false },
  { key: "tasks_created", label: "created", num: true },
  { key: "tasks_planned", label: "planned", num: true },
  { key: "tasks_completed", label: "completed", num: true },
];

const PR_REVIEW_COLUMNS = [
  { key: "agent", label: "agent", num: false },
  {
    key: "pr.review_comments",
    label: "pr rev",
    num: true,
    title: "pull-request review comments from the lifetime snapshot",
  },
];

const OPERATIONS_SCOREBOARD_COLUMNS = [
  { key: "agent", label: "agent", num: false },
  {
    key: "graph_calls",
    label: "graph calls",
    num: true,
    compute: (agent) => agent?.tool_calls_by_surface?.graph ?? 0,
    title: "orbit.graph.* calls",
  },
  {
    key: "task_calls",
    label: "task calls",
    num: true,
    compute: (agent) => agent?.tool_calls_by_surface?.task ?? 0,
    title: "orbit.task.* tool-call count",
  },
  {
    key: "tools",
    label: "tool fail/all",
    num: true,
    format: "pair",
    left: "failed_tool_calls",
    right: "tool_calls",
    title: "raw failed tool calls over total tool calls",
    help: "raw events",
  },
  // ORB-10871: the same failures, grouped. A repeated burst is one incident
  // with its raw event count beside it, so the left number answers "how many
  // things went wrong" and the right one "how much evidence there is".
  {
    key: "failure_incidents",
    label: "fail inc/events",
    num: true,
    format: "pair",
    left: "failure_incidents",
    right: "failure_incident_events",
    tone: "warn",
    title: "grouped failure incidents / raw failed events they collapsed",
    help: "grouped",
    // ORB-11207: names the `coverage.failure_incidents` note so an
    // unavailable source keeps this row visible instead of reading as a
    // filtered-out measured zero.
    coverageKey: "failure_incidents",
  },
  {
    key: "friction.reported",
    label: "friction",
    num: true,
    title: "append-only friction reports filed by this agent",
  },
];

function allScoreboardSections() {
  // Badges name the window so a count is never read against an unstated
  // cutoff; the Operations badge also states that its failures are grouped.
  const window = getWindow();
  return [
    { title: "Delivery", badge: `tasks created · planned · completed · window ${window}`, columns: DELIVERY_SCOREBOARD_COLUMNS },
    { title: "Review", badge: `review threads · PR comments · window ${window}`, columns: PR_REVIEW_COLUMNS },
    {
      title: "Operations",
      badge: `tool calls · raw failed events and the incidents they group into · friction · window ${window}`,
      columns: OPERATIONS_SCOREBOARD_COLUMNS,
    },
  ];
}

function readPath(obj, path) {
  let cur = obj;
  for (const part of path.split(".")) {
    if (cur == null) return undefined;
    cur = cur[part];
  }
  return cur;
}

function renderScoreboard(summary) {
  // ORB-00337: idempotent attach of the window-selector click handler
  // (guarded internally so re-renders don't double-bind).
  wireScoreboardWindowSelector();
  syncWindowSelectors();

  // ORB-10872: never paint a 24h body under an active 7d (or any other)
  // selection. The fetch path also guards this; keep the renderer honest.
  if (summary && !payloadHonorsWindow(summary, getWindow())) {
    console.error(
      `scoreboard payload window ${summary.window} rejected under ${getWindow()} selection`,
    );
    return;
  }

  const body = $("scoreboard-body");
  const narrativeHost = $("scoreboard-narrative");
  const agentStrip = $("scoreboard-agent-strip");
  const highlightsHost = $("scoreboard-highlights");
  const meta = $("scoreboard-meta");
  const insightsHost = $("scoreboard-insights");
  const insightsCount = $("scoreboard-insights-count");
  const orchestrationHost = $("scoreboard-orchestration");
  const orchestrationCount = $("scoreboard-orchestration-count");

  const agentsMap = (summary && summary.agents) || {};
  const entries = Object.entries(agentsMap);
  $("scoreboard-count").textContent = `${entries.length} agents`;

  if (entries.length === 0) {
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: "No scoreboard data yet." })
    ])]);
    if (narrativeHost) syncNodes(narrativeHost, []);
    if (agentStrip) syncNodes(agentStrip, []);
    if (highlightsHost) syncNodes(highlightsHost, [renderNotableCompletions(summary?.notable_completions)]);
    if (meta) meta.textContent = "-";
    if (insightsHost) syncNodes(insightsHost, []);
    if (insightsCount) insightsCount.textContent = "—";
    if (orchestrationHost) syncNodes(orchestrationHost, [emptyOrchestrationNode()]);
    if (orchestrationCount) orchestrationCount.textContent = "—";
    return;
  }

  const canonicalRows = canonicalScoreboardRows(agentsMap);
  if (meta) {
    const activeCanonical = canonicalRows.filter(([, agent]) => agentActivityTotal(agent) > 0).length;
    meta.textContent = `${activeCanonical} of ${canonicalRows.length} canonical active · ${entries.length} total agents`;
  }
  if (agentStrip) {
    syncNodes(agentStrip, [renderAgentStrip(canonicalRows)]);
  }
  if (highlightsHost) {
    syncNodes(highlightsHost, [renderNotableCompletions(summary?.notable_completions)]);
  }
  const matrix = buildLeaderboardMatrix(canonicalRows, allScoreboardSections(), {
    showSectionDividers: true,
    coverage: summary?.coverage,
  });
  syncNodes(body, [el("div", { class: "scoreboard-sections" }, [matrix])]);

  // Narrative — single-line "claude leads creation · ..." summary. Skipped when
  // the cycle is too small to make a confident claim (< 10 created across the
  // canonical four).
  if (narrativeHost) {
    const narrative = renderScoreboardNarrative(summary);
    syncNodes(narrativeHost, narrative ? [narrative] : []);
  }

  // Insights — rule-driven narrative cards. Panel collapses when no rule fires.
  if (insightsHost) {
    const panel = renderInsightsPanel(summary);
    syncNodes(insightsHost, panel ? [panel] : []);
    if (insightsCount) {
      const fired = panel ? panel.querySelectorAll(".ins-row").length : 0;
      insightsCount.textContent = fired === 0 ? "—" : String(fired);
    }
  }

  // This deliberately does not feed the execution-agent leaderboard above.
  // Ownership comes from canonical task orchestrators; executor family/model
  // remains an independent identity in `summary.agents`.
  if (orchestrationHost) {
    const orchestration = renderOrchestrationSummary(summary?.orchestration);
    syncNodes(orchestrationHost, [orchestration]);
    if (orchestrationCount) {
      const count = Array.isArray(summary?.orchestration?.buckets)
        ? summary.orchestration.buckets.length
        : 0;
      orchestrationCount.textContent = count === 0 ? "—" : `${count} buckets`;
    }
  }
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
  const since = orchestration.since || "all managed execution retained";
  const until = orchestration.until || orchestration.as_of || "unknown cutoff";
  const children = [
    el("div", { class: "scoreboard-orchestration-context" }, [
      el("div", { class: "scoreboard-orchestration-scope" }, [
        el("span", { class: "scope-label", text: "Managed execution only" }),
        el("span", { class: "scope-badge", text: `window ${getWindow()}` }),
        el("span", { text: "Direct interactive Codex or Claude orchestration-session overhead is excluded." }),
      ]),
      el("div", { class: "scoreboard-orchestration-window", text: `Window ${getWindow()}: ${since} ≤ invocation < ${until} (exclusive cutoff; as of ${orchestration.as_of || "unknown"}).` }),
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

function canonicalScoreboardRows(agentsMap) {
  return CANONICAL_SCOREBOARD_FAMILIES.map((family) => [family, agentsMap[family] || {}]);
}

function applyAgentTheme(node, name) {
  const theme = AGENT_THEME[name] || {};
  node.style.setProperty("--ag", theme.color || "var(--accent)");
  node.style.setProperty("--ag-low", theme.low || "rgba(110, 159, 255, 0.12)");
  return node;
}

function agentActivityTotal(agent) {
  return (
    asScoreboardNumber(agent?.tasks_created) +
    asScoreboardNumber(agent?.tasks_planned) +
    asScoreboardNumber(agent?.tasks_completed) +
    asScoreboardNumber(agent?.pr?.review_comments) +
    asScoreboardNumber(agent?.tool_calls) +
    asScoreboardNumber(agent?.failed_tool_calls) +
    asScoreboardNumber(agent?.friction?.reported)
  );
}

function agentRoleLabel(agent, rows) {
  if (agentActivityTotal(agent) === 0) return "quiet";
  const labels = [];
  const leaderIf = (label, getter) => {
    const value = asScoreboardNumber(getter(agent));
    const max = Math.max(...rows.map(([, rowAgent]) => asScoreboardNumber(getter(rowAgent))));
    if (value > 0 && value === max) labels.push(label);
  };
  leaderIf("author", (a) => a?.tasks_created);
  leaderIf("planner", (a) => a?.tasks_planned);
  leaderIf("closer", (a) => a?.tasks_completed);
  leaderIf("ops", (a) => a?.tool_calls);
  if (labels.length === 0) return "active";
  return labels.slice(0, 2).join(" / ");
}

function renderAgentStrip(rows) {
  const sorted = rows
    .map(([name, agent]) => [name, agent, agentActivityTotal(agent)])
    .sort((a, b) => b[2] - a[2]);
  const maxByKpi = new Map();
  for (const kpi of AGENT_CARD_KPIS) {
    maxByKpi.set(kpi.key, Math.max(...rows.map(([, agent]) => scoreboardColumnValue(agent, kpi))));
  }

  const strip = el("div", { class: "scoreboard-agent-strip-inner" });
  strip.dataset.key = "scoreboard-agent-strip";
  strip.dataset.hash = JSON.stringify(sorted.map(([name, agent, score]) => [
    name,
    score,
    agentRoleLabel(agent, rows),
    AGENT_CARD_KPIS.map((kpi) => [kpi.key, scoreboardColumnValue(agent, kpi)]),
  ]));

  sorted.forEach(([name, agent, score], index) => {
    const card = applyAgentTheme(el("button", {
      class: `scoreboard-agent-card ${score === 0 ? "quiet" : ""}`,
      title: `${name} - click to filter audit by role`,
    }), name);
    card.type = "button";
    card.addEventListener("click", () => navigateToDrilldown({ role: name }));

    card.appendChild(el("div", { class: "scoreboard-agent-rank", text: `#${String(index + 1).padStart(2, "0")} · activity` }));
    card.appendChild(el("div", { class: "scoreboard-agent-heading" }, [
      el("span", { class: "scoreboard-agent-name", text: name }),
      el("span", { class: `scoreboard-agent-role ${score === 0 ? "quiet" : ""}`, text: agentRoleLabel(agent, rows) }),
    ]));

    const kpis = el("div", { class: "scoreboard-agent-kpis" });
    for (const kpi of AGENT_CARD_KPIS) {
      const value = scoreboardColumnValue(agent, kpi);
      const max = maxByKpi.get(kpi.key) || 0;
      const pct = max > 0 ? Math.round((value / max) * 100) : 0;
      const valueClass = value === 0 ? "dim" : kpi.tone === "warn" ? "warn" : "";
      const fill = el("span", {
        class: `scoreboard-agent-kpi-fill ${kpi.tone === "warn" ? "warn" : ""}`,
        style: { width: `${pct}%` },
      });
      kpis.appendChild(el("div", { class: "scoreboard-agent-kpi" }, [
        el("span", { class: "k", text: kpi.label }),
        el("span", { class: "track" }, [fill]),
        el("span", { class: `v ${valueClass}`, text: value === 0 ? "—" : fmtScoreboardCount(value) }),
      ]));
    }
    card.appendChild(kpis);
    strip.appendChild(card);
  });
  return strip;
}

function buildLeaderboardMatrix(rows, sectionList, opts = {}) {
  if (!rows.length) {
    return el("div", { class: "empty-state compact", text: "No rows." });
  }

  const showSectionDividers = opts.showSectionDividers !== false;
  const table = el("table", { class: "sb2-matrix" });
  const colgroup = el("colgroup");
  colgroup.appendChild(el("col", { class: "metric" }));
  for (let i = 0; i < rows.length; i += 1) colgroup.appendChild(el("col"));
  table.appendChild(colgroup);

  const thead = el("thead");
  const headRow = el("tr");
  headRow.appendChild(el("th", { class: "col-metric", text: "metric" }));
  for (const [name, agent] of rows) {
    const th = applyAgentTheme(el("th", {
      class: "col-agent clickable",
      title: `${name} — click to filter audit by role`,
    }, [
      document.createTextNode(name),
      el("span", { class: "totals", text: `${fmtScoreboardCount(agentActivityTotal(agent))} activity` }),
    ]), name);
    th.addEventListener("click", () => navigateToDrilldown({ role: name }));
    headRow.appendChild(th);
  }
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = el("tbody");
  const columnCount = rows.length + 1;
  for (const section of sectionList) {
    const metrics = section.columns
      .filter((candidate) => candidate.key !== "agent")
      .filter((candidate) => metricHasActivity(rows, candidate, opts.coverage));
    if (showSectionDividers) {
      const badge = metrics.length === 0
        ? emptySectionBadge(section.title, opts.coverage)
        : section.badge;
      tbody.appendChild(sectionDividerRow(section.title, badge, columnCount));
    }
    for (const col of metrics) {
      const rowMax = rowMaxValue(rows, col);
      const tr = el("tr", { class: "metric" });
      tr.dataset.key = `scoreboard-${section.title}-${col.key}`;
      const metricLabel = el("td", {
        class: "m-label",
        title: col.title || col.label,
      }, [
        document.createTextNode(col.label),
        ...(col.help ? [el("span", { class: "help", text: col.help })] : []),
      ]);
      metricLabel.setAttribute("aria-label", col.title || col.label);
      tr.appendChild(metricLabel);
      for (const [name, agent] of rows) {
        const value = scoreboardColumnValue(agent, col);
        const isLeader = rowMax > 0 && value === rowMax;
        const td = col.format === "pair"
          ? pairMetricCell(name, agent, col, rowMax, isLeader)
          : metricCell(name, agent, col, rowMax, isLeader);
        td.dataset.agent = name;
        td.dataset.metric = col.key;
        td.classList.add("clickable");
        td.title = `${col.title || col.label}: click to filter audit`;
        td.addEventListener("click", () => navigateToDrilldown({
          role: name,
          metric: col.key,
          status: ["tools", "failed_tool_calls", "failure_incidents"].includes(col.key)
            ? "failure"
            : null,
        }));
        tr.appendChild(td);
      }
      tbody.appendChild(tr);
    }
  }
  table.appendChild(tbody);
  return table;
}

function metricHasActivity(rows, col, coverage) {
  // ORB-11207: an unavailable source is not the same as an observed zero —
  // keep the row visible so the operator sees the missing-coverage marker
  // instead of the row silently disappearing from the table.
  if (col.coverageKey && coverage?.[col.coverageKey]?.availability === "unavailable") {
    return true;
  }
  return rows.some(([, agent]) => scoreboardCellActivity(agent, col) > 0);
}

function scoreboardCellActivity(agent, col) {
  if (col.format === "pair") {
    const pair = formatScoreboardPair(agent, col);
    return pair.left + pair.right;
  }
  return scoreboardColumnValue(agent, col);
}

function rowMaxValue(rows, col) {
  return rows.reduce((max, [, agent]) => Math.max(max, scoreboardColumnValue(agent, col)), 0);
}

function scoreboardColumnValue(agent, col) {
  if (col.format === "pair") {
    return formatScoreboardPair(agent, col).left;
  }
  const value = col.compute ? col.compute(agent) : readPath(agent, col.key);
  return Math.max(0, asScoreboardNumber(value));
}

function metricCell(name, agent, col, rowMax, isLeader) {
  const value = asScoreboardNumber(col.compute ? col.compute(agent) : readPath(agent, col.key));
  const td = el("td", {
    class: "cell",
    title: `${col.title || col.label}: ${value}`,
  }, [scoreboardCellNode(name, value, rowMax, isLeader, { warn: col.tone === "warn" })]);
  return applyAgentTheme(td, name);
}

function pairMetricCell(name, agent, col, rowMax, isLeader) {
  const pair = formatScoreboardPair(agent, col);
  const td = el("td", {
    class: "cell",
    title: pair.title,
  }, [scoreboardCellNode(name, pair.left, rowMax, isLeader, {
    pair,
    warn: col.tone === "warn",
  })]);
  return applyAgentTheme(td, name);
}

function scoreboardCellNode(name, value, rowMax, isLeader, opts = {}) {
  const num = Math.max(0, asScoreboardNumber(value));
  const width = rowMax > 0 ? Math.max(2, Math.round((num / rowMax) * 100)) : 0;
  const fill = el("span", {
    class: `fill ${opts.warn ? "warn" : ""}`,
    style: { width: `${width}%` },
  });
  const valueClass = [
    "v",
    num === 0 ? "dim em" : "",
    isLeader ? "lead" : "",
    opts.warn ? "warn" : "",
  ].filter(Boolean).join(" ");
  return applyAgentTheme(el("span", {
    class: `sb2-cell${num === 0 ? " zero" : ""}`,
  }, [
    el("span", { class: "track" }, [fill]),
    el("span", { class: valueClass }, opts.pair
      ? pairTextNodes(opts.pair, isLeader)
      : [
          document.createTextNode(num === 0 ? "—" : fmtScoreboardCount(num)),
          ...(isLeader ? [leaderBadge()] : []),
        ]),
  ]), name);
}

function leaderBadge() {
  const mark = el("span", {
    class: "lead-mark",
    text: "▲",
    title: "Highest count in this row. Not a quality score.",
  });
  mark.setAttribute("aria-label", "Highest count in this row. Not a quality score.");
  return mark;
}

function emptySectionBadge(title, coverage) {
  if (title === "Review") {
    if (coverage?.review?.availability === "unavailable") {
      return coverage.review.detail
        || "review snapshot is not windowed; this is missing coverage, not zero activity";
    }
    return "no observed review comments in this source";
  }
  if (title === "Delivery") return "no observed task events this window";
  if (title === "Operations") {
    if (coverage?.failure_incidents?.availability === "unavailable") {
      return coverage.failure_incidents.detail
        || "failure-incident coverage is unavailable for this window; this is missing coverage, not zero activity";
    }
    return "no observed tool calls or friction this window";
  }
  return "no observed events this window";
}

function formatHighlightTime(raw) {
  if (!raw) return "completion time unknown";
  const parsed = Date.parse(raw);
  if (Number.isNaN(parsed)) return raw;
  return new Date(parsed).toISOString().replace("T", " ").replace(/\.\d+Z$/, " UTC");
}

function renderHighlightItem(item) {
  const bits = [item.priority, item.task_type, item.impact_tag].filter(Boolean);
  const excerpt = item.summary_excerpt
    ? el("p", { class: "scoreboard-highlight-excerpt", text: item.summary_excerpt })
    : el("p", {
      class: "scoreboard-highlight-excerpt missing",
      text: "No completion summary recorded.",
    });
  const time = el("time", { class: "scoreboard-highlight-time", text: formatHighlightTime(item.completed_at) });
  if (item.completed_at) time.setAttribute("datetime", item.completed_at);
  return el("li", { class: "scoreboard-highlight" }, [
    el("div", { class: "scoreboard-highlight-head" }, [
      el("span", { class: "scoreboard-highlight-id", text: item.task_id || "unknown id" }),
      el("span", { class: "scoreboard-highlight-title", text: item.title || "(untitled)" }),
    ]),
    el("div", { class: "scoreboard-highlight-meta" }, [
      el("span", { class: "scoreboard-highlight-tags", text: bits.join(" · ") || "no priority or type" }),
      time,
    ]),
    excerpt,
  ]);
}

function renderNotableCompletions(block) {
  const items = Array.isArray(block?.items) ? block.items : [];
  const label = block?.label
    || "Ordered by priority, then most recently completed. This is a reading order, not a quality score.";
  const children = [
    el("div", { class: "scoreboard-highlights-heading" }, [
      el("h3", { class: "scoreboard-highlights-title", text: "Notable completions" }),
      el("p", { class: "scoreboard-highlights-rule", text: label }),
    ]),
  ];
  if (items.length === 0) {
    children.push(el("div", { class: "empty-state compact" }, [
      el("div", { class: "text", text: "No completed tasks observed in this window." }),
    ]));
  } else {
    children.push(el("ul", { class: "scoreboard-highlights-list" }, items.map(renderHighlightItem)));
  }
  return el("section", { class: "scoreboard-highlights" }, children);
}

function emptyScoreboardNode() {
  return el("span", { class: "em", text: "—" });
}

function pairTextNodes(pair, isLeader) {
  if (pair.unavailable) {
    return [el("span", {
      class: "em unavailable",
      text: "unavailable",
      title: "source unavailable for this window; not a measured zero",
    })];
  }
  if (pair.zero) return [emptyScoreboardNode()];
  return [
    pair.left === 0 ? emptyScoreboardNode() : el("span", { class: "sb-value", text: fmtScoreboardCount(pair.left) }),
    el("span", { class: "pair", text: "/" }),
    pair.right === 0 ? emptyScoreboardNode() : el("span", { class: "sb-pair-right", text: fmtScoreboardCount(pair.right) }),
    ...(isLeader ? [leaderBadge()] : []),
  ];
}

function sectionDividerRow(title, badge, columnCount) {
  const tr = el("tr", { class: "group" });
  const td = el("td", {}, [
    document.createTextNode(title),
    badge ? el("span", { class: "badge", text: badge }) : null,
  ]);
  td.colSpan = columnCount;
  tr.appendChild(td);
  return tr;
}

// ===== Phase 1 additions: narrative + insights panel (rule-driven).
// Heuristics are intentionally simple so they can be audited at a glance and
// safely skip when the cycle is too small.

const NARRATIVE_MIN_CREATED_TOTAL = 10;

function canonicalAgentsList(summary) {
  const map = (summary && summary.agents) || {};
  return CANONICAL_SCOREBOARD_FAMILIES.map((family) => [family, map[family] || {}]);
}

function bestBy(rows, score) {
  let leader = null;
  let leaderScore = 0;
  for (const [name, agent] of rows) {
    const value = score(agent);
    if (value > leaderScore) {
      leaderScore = value;
      leader = name;
    }
  }
  return { name: leader, value: leaderScore };
}

/**
 * Render a one-line summary above the matrix. Returns the DOM node or `null`.
 *
 * Skip rule: total `tasks_created` across canonical families < 10. Quiet
 * cycles produce misleading narratives ("claude leads with 1 task"), so we
 * suppress the line entirely rather than render something dishonest.
 */
function renderScoreboardNarrative(summary) {
  const rows = canonicalAgentsList(summary);
  const totalCreated = rows.reduce(
    (sum, [, agent]) => sum + asScoreboardNumber(agent.tasks_created),
    0,
  );
  if (totalCreated < NARRATIVE_MIN_CREATED_TOTAL) return null;

  const segments = [];
  const pushSegment = (label, result) => {
    if (result && result.name && result.value > 0) {
      segments.push({ label, leader: result.name, value: result.value });
    }
  };
  pushSegment("creation", bestBy(rows, (a) => asScoreboardNumber(a.tasks_created)));
  pushSegment("planning", bestBy(rows, (a) => asScoreboardNumber(a.tasks_planned)));
  pushSegment("completion", bestBy(rows, (a) => asScoreboardNumber(a.tasks_completed)));

  if (segments.length === 0) return null;

  const children = [];
  segments.forEach((seg, idx) => {
    if (idx > 0) children.push(el("span", { class: "nar-sep", text: "·" }));
    children.push(el("span", { class: `ag-tag ${seg.leader}`, text: seg.leader }));
    children.push(document.createTextNode(" leads "));
    children.push(el("b", { text: seg.label }));
  });
  return el("div", { class: "scoreboard-narrative" }, children);
}

// ----- insight rules -----
// Each rule returns `{tone, headline, body}` or null. `body` is an array of
// DOM-nodes-or-strings so we can interleave agent pills.

function insightAgentPill(name) {
  return el("span", { class: `ag-tag ${name}`, text: name });
}

// LEADER — top agent by `tasks_created`. Suppressed when nobody has created
// anything (handled implicitly via bestBy returning value 0).
function insightLeader(rows) {
  const top = bestBy(rows, (a) => asScoreboardNumber(a.tasks_created));
  if (!top.name || top.value === 0) return null;
  return {
    tone: "win",
    headline: "leader",
    body: [
      insightAgentPill(top.name),
      ` created `,
      el("b", { text: `${top.value} tasks` }),
      ` this window — the cycle's top author.`,
    ],
  };
}

// WATCH — agent whose friction.reported is >= 3× the team average (and has
// reported at least 3 to avoid noisy small-sample callouts).
function insightWatch(rows) {
  const counts = rows.map(([, a]) => asScoreboardNumber(a?.friction?.reported));
  const total = counts.reduce((s, v) => s + v, 0);
  if (total === 0) return null;
  const avg = total / rows.length;
  let pickName = null;
  let pickValue = 0;
  for (const [name, agent] of rows) {
    const value = asScoreboardNumber(agent?.friction?.reported);
    if (value >= 3 && value >= avg * 3 && value > pickValue) {
      pickValue = value;
      pickName = name;
    }
  }
  if (!pickName) return null;
  return {
    tone: "flag",
    headline: "watch",
    body: [
      insightAgentPill(pickName),
      ` reported `,
      el("b", { text: `${pickValue} friction events` }),
      ` — ~`,
      el("b", { text: `${(pickValue / Math.max(avg, 1)).toFixed(1)}×` }),
      ` the team average. Worth a quick look.`,
    ],
  };
}

// SURPRISE — highest `tasks_completed / max(tasks_created, 1)` ratio among
// agents with `tasks_completed >= 5`. Surfaces "closers" — agents finishing
// far more than they author.
function insightSurprise(rows) {
  let pickName = null;
  let pickRatio = 0;
  let pickCompleted = 0;
  let pickCreated = 0;
  for (const [name, agent] of rows) {
    const completed = asScoreboardNumber(agent.tasks_completed);
    const created = asScoreboardNumber(agent.tasks_created);
    if (completed < 5) continue;
    const ratio = completed / Math.max(created, 1);
    if (ratio > pickRatio) {
      pickRatio = ratio;
      pickName = name;
      pickCompleted = completed;
      pickCreated = created;
    }
  }
  // Only call it a "surprise" if the closer finished meaningfully more than
  // they authored — otherwise it's just the same agent leading creation.
  if (!pickName || pickRatio < 1.5) return null;
  return {
    tone: "win",
    headline: "surprise",
    body: [
      insightAgentPill(pickName),
      ` completed `,
      el("b", { text: `${pickCompleted}` }),
      ` tasks against `,
      el("b", { text: `${pickCreated || 0}` }),
      ` created — ${pickRatio.toFixed(1)}× ratio, the cycle's closer.`,
    ],
  };
}

// COVERAGE — checks that all four canonical families have at least one
// activity signal (created, planned, completed, or any
// tool calls). Names the idle families when not.
function insightCoverage(rows) {
  const idle = rows
    .filter(([, agent]) => {
      const signal =
        asScoreboardNumber(agent.tasks_created) +
        asScoreboardNumber(agent.tasks_planned) +
        asScoreboardNumber(agent.tasks_completed) +
        asScoreboardNumber(agent.tool_calls);
      return signal === 0;
    })
    .map(([name]) => name);

  if (idle.length === 0) {
    return {
      tone: "win",
      headline: "role coverage",
      body: [`All four agent families are active this window. Healthy mix.`],
    };
  }
  const body = [];
  idle.forEach((name, idx) => {
    if (idx > 0) body.push(", ");
    body.push(insightAgentPill(name));
  });
  body.push(` ${idle.length === 1 ? "is" : "are"} idle this window — `);
  body.push(
    el("b", { text: `${idle.length} of ${rows.length}` }),
  );
  body.push(` canonical families silent.`);
  return {
    tone: idle.length >= 2 ? "flag" : "win",
    headline: "role coverage",
    body,
  };
}

/**
 * Compose the insights panel from all rules. Returns `<section
 * class="scoreboard-insights">` with one `.ins-row` per fired rule, or `null`
 * when nothing fires.
 */
function renderInsightsPanel(summary) {
  const rows = canonicalAgentsList(summary);
  const rules = [insightLeader, insightWatch, insightSurprise, insightCoverage];
  const cards = rules
    .map((rule) => rule(rows))
    .filter((card) => card !== null);
  if (cards.length === 0) return null;

  const section = el("section", { class: "scoreboard-insights" });
  for (const card of cards) {
    const row = el("div", { class: `ins-row ${card.tone}` });
    row.appendChild(el("div", { class: "hd", text: card.headline }));
    const body = el("div", { class: "body" });
    for (const part of card.body) {
      if (typeof part === "string") {
        body.appendChild(document.createTextNode(part));
      } else if (part instanceof Node) {
        body.appendChild(part);
      }
    }
    row.appendChild(body);
    section.appendChild(row);
  }
  return section;
}

export { renderScoreboard, renderScoreboardNarrative, renderInsightsPanel };
