// Orbit dashboard diagnostics-domain (metrics + errors tables + implement_one side card).
// Pure vanilla JS, split into ES modules with no build step.
//
// lastDiagnostics and activeDiagSubtab live in app.js (mutated by the fetch
// closures in activeRefreshJobs, which are kept in app.js per scope). They are
// exposed read-only via the diagnosticsContext() factory in app.js, passed as
// the argument to render entry points. This mirrors the taskContext() /
// auditContext() pattern. Simpler getter approach wins here.
//
// Uses `el`, `syncNodes` from `./common.js` (re-defines $ locally, as other
// extracted modules do).
//
// Cross helpers (fmtRelative, fmtDuration, truncate, setActiveTab,
// navigateToRun) are provided via ctx. The row click uses setActiveTab
// (preserving the ?step= query for run-detail pre-expansion) rather than
// navigateToRun to ensure identical behavior to before the split.
//
// Main-table and side-card requests render independently so a side-card
// completion cannot replace the main panel's loading or failure feedback.

import { panelCanRender, resetPanel, el, syncNodes, getWindow } from './common.js';
import { navigateToDrilldown } from './audit.js';

const $ = (id) => document.getElementById(id);

// ORB-10871: which incidents the operator has expanded. Module-scoped (like
// audit.js's expandedAuditIds) so a refresh tick does not collapse the row
// someone is reading.
const expandedIncidents = new Set();

function hasCtx(ctx, key) {
  return ctx && typeof ctx[key] === "function";
}

function fmtRelativeValue(ctx, v) {
  return hasCtx(ctx, "fmtRelative") ? ctx.fmtRelative(v) : (v || "-");
}

function fmtDurationValue(ctx, v) {
  return hasCtx(ctx, "fmtDuration") ? ctx.fmtDuration(v) : (v == null ? "-" : String(v));
}

function truncateValue(ctx, s, n = 220) {
  return hasCtx(ctx, "truncate") ? ctx.truncate(s, n) : String(s || "").slice(0, n);
}

// `actor_identity` is the persisted ActorIdentity enum: a flat label string,
// or the tagged `{"human": "..."}` / `{"agent": {"model": "..."}}` shape used
// for the labels a flat string cannot represent. Both render as one label.
function actorIdentityLabel(v) {
  if (v == null) return "";
  if (typeof v !== "object") return String(v);
  if (typeof v.human === "string") return v.human;
  const agent = v.agent || {};
  return agent.model || agent.name || "";
}

function getDiagMetricsColumns(ctx) {
  return [
    { key: "ts", label: "time", num: false, render: (v) => fmtRelativeValue(ctx, v) },
    { key: "step", label: "step", num: false },
    {
      key: "actor_identity",
      label: "actor",
      num: false,
      render: (v) => actorIdentityLabel(v) || "-",
    },
    {
      key: "token_usage",
      label: "tokens",
      num: true,
      render: (v) => (v == null ? "-" : String(v)),
    },
    { key: "tool_invocations", label: "tools", num: true },
    {
      key: "step_duration_ms",
      label: "duration",
      num: true,
      render: (v) => fmtDurationValue(ctx, v),
    },
    { key: "retry_count", label: "retries", num: true },
  ];
}

function errorRunLabel(row) {
  if (row.job_run) return row.job_run;
  if (row.affiliation === "unaffiliated") return "unaffiliated";
  return "-";
}

function getDiagErrorsColumns(ctx) {
  return [
    { key: "ts", label: "time", num: false, render: (v) => fmtRelativeValue(ctx, v) },
    { key: "source", label: "source", num: false },
    {
      key: "job_run",
      label: "run",
      num: false,
      render: (_v, row) => errorRunLabel(row),
    },
    { key: "provider", label: "provider", num: false, render: (v) => v || "-" },
    { key: "step", label: "step", num: false, render: (v) => v || "-" },
    {
      key: "message",
      label: "message",
      num: false,
      cellClass: "stderr",
      render: (v, row, td) => {
        const full = v || "";
        td.title = row.target ? `${row.target}: ${full}` : full;
        return truncateValue(ctx, full, 220);
      },
    },
  ];
}

function renderDiagnosticsTable(rows, columns, ctx, emptyText) {
  const body = $("diag-body");
  
  if (!rows || rows.length === 0) {
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: emptyText || "No entries this month." })
    ])]);
    return;
  }
  
  let table = body.querySelector("table.scoreboard-table");
  let tbody;
  const tableSig = columns.map(c => c.key).join("-");
  if (!table || table.dataset.sig !== tableSig) {
    table = el("table", { class: "scoreboard-table" });
    table.dataset.sig = tableSig;
    const thead = el("thead");
    const headRow = el("tr");
    for (const col of columns) {
      headRow.appendChild(el("th", { class: col.num ? "num" : "", text: col.label }));
    }
    thead.appendChild(headRow);
    table.appendChild(thead);
    tbody = el("tbody");
    table.appendChild(tbody);
    syncNodes(body, [table]);
  } else {
    tbody = table.querySelector("tbody");
  }

  const frag = document.createDocumentFragment();
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i];
    const tr = el("tr");
    for (const col of columns) {
      const baseClass =
        (col.num ? "num" : "") + (col.cellClass ? ` ${col.cellClass}` : "");
      const td = el("td", { class: baseClass });
      const v = row[col.key];
      const text = col.render ? col.render(v, row, td) : v == null ? "" : String(v);
      td.textContent = text;
      tr.appendChild(td);
    }
    tr.dataset.key = `diag-${row.ts || ''}-${row.job_run || row.affiliation || ''}-${row.step || i}-${row.command || actorIdentityLabel(row.actor_identity) || ''}`;
    tr.dataset.hash = JSON.stringify(row);
    if (row.affiliation === "unaffiliated") {
      tr.classList.add("unaffiliated");
    }
    if (row.job_run) {
      tr.classList.add("clickable");
      tr.title = "Open owning run";
      tr.addEventListener("click", () => {
        const stepQuery = row.step_index == null ? "" : `?step=${encodeURIComponent(row.step_index)}`;
        if (hasCtx(ctx, "setActiveTab")) {
          ctx.setActiveTab(`runs/${encodeURIComponent(row.job_run)}${stepQuery}`);
        }
      });
    }
    frag.appendChild(tr);
  }
  
  syncNodes(tbody, Array.from(frag.children));
}

// ===== ORB-10871: grouped failure incidents.
//
// The Errors view above lists raw failed events — that stays, because it is the
// forensic record. This view answers the different question "how many distinct
// problems happened", and every number it renders states what it is out of and
// which window it was measured over. Nothing is hidden: each incident expands
// to the exact audit rows it collapsed, and links out to the raw Audit view.

function asCount(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function eventCountLabel(value) {
  const count = asCount(value);
  return `${count} event${count === 1 ? "" : "s"}`;
}

const INCIDENT_CLASS_ORDER = ["unexpected", "expected", "denied", "diagnostic"];

function incidentSummaryNode(payload) {
  const incidents = asCount(payload.incident_count);
  const failed = asCount(payload.raw_failed_events);
  const total = asCount(payload.total_events);
  const runs = asCount(payload.affected_run_count);
  const window = payload.window || getWindow();
  const categories = payload.failure_categories || {};
  const unexpected = categories.unexpected || {};
  const unexpectedIncidents = asCount(unexpected.incidents);
  const unexpectedEvents = asCount(unexpected.raw_events);
  const unexpectedRuns = asCount(unexpected.affected_runs);
  const lifecycleEvents = asCount(payload.lifecycle_diagnostic_events);
  const lifecycleIncidents = asCount(payload.lifecycle_diagnostic_incidents);
  const lifecycleRuns = asCount(payload.lifecycle_diagnostic_affected_run_count);
  const lifecycleLabel = payload.lifecycle_diagnostic_label || "lifecycle diagnostics";

  const head = el("div", { class: "incident-summary-head" }, [
    el("strong", { class: "incident-summary-headline", text: `${unexpectedIncidents} unexpected incidents` }),
    el("span", {
      class: "incident-summary-denominator",
      text: `${unexpectedEvents} unexpected raw events · ${unexpectedRuns} affected runs; ${incidents} incidents / ${failed} failed events / ${runs} affected runs across ${total} audited events`,
    }),
    el("span", { class: "incident-summary-window", text: `window ${window}` }),
  ]);

  const byClass = payload.incidents_by_class || {};
  const eventsByClass = payload.raw_events_by_class || {};
  const labels = payload.class_labels || {};
  const chips = el("div", { class: "incident-class-chips" });
  for (const key of INCIDENT_CLASS_ORDER) {
    const count = asCount(byClass[key]);
    const events = asCount(eventsByClass[key]);
    const category = categories[key] || {};
    const categoryRuns = asCount(category.affected_runs);
    if (count === 0 && events === 0) continue;
    chips.appendChild(el("span", {
      class: `incident-class-chip ${key}`,
      title: `${labels[key] || key}: ${count} incidents from ${events} raw events affecting ${categoryRuns} runs (window ${window})`,
      text: `${labels[key] || key}: ${count} incidents · ${events} raw · ${categoryRuns} runs`,
    }));
  }

  const children = [head];
  if (chips.childNodes.length > 0) children.push(chips);
  if (lifecycleEvents > 0 || lifecycleIncidents > 0) {
    children.push(el("div", {
      class: "incident-lifecycle-note",
      title: `${lifecycleLabel} are failure-only event surfaces and are excluded from callable-tool denominators and rates`,
      text: `${lifecycleLabel}: ${lifecycleIncidents} incidents · ${lifecycleEvents} raw events · ${lifecycleRuns} affected runs (excluded from tool rates)`,
    }));
  }
  if (payload.truncated) {
    children.push(el("p", {
      class: "incident-truncation-note",
      text: "Scan limit reached — older failed events in this window were not grouped. Narrow the window for a complete count.",
    }));
  }
  return el("section", { class: "incident-summary" }, children);
}

function incidentEvidenceTable(events, ctx) {
  const table = el("table", { class: "incident-evidence" });
  const thead = el("thead");
  const headRow = el("tr");
  for (const label of ["event", "execution", "time", "status", "actor", "surface", "tool", "run", "task", "message"]) {
    headRow.appendChild(el("th", { text: label }));
  }
  thead.appendChild(headRow);
  table.appendChild(thead);
  const tbody = el("tbody");
  for (const event of events) {
    const tr = el("tr");
    tr.appendChild(el("td", { class: "mono", text: event.id == null ? "-" : `#${event.id}` }));
    tr.appendChild(el("td", { class: "mono", text: event.execution_id || "-" }));
    tr.appendChild(el("td", { text: ctx.fmtAbsTime ? ctx.fmtAbsTime(event.ts) : (event.ts || "-") }));
    tr.appendChild(el("td", { text: event.status || "-" }));
    tr.appendChild(el("td", { text: event.actor || "-" }));
    tr.appendChild(el("td", { class: "mono", text: event.surface || "-" }));
    tr.appendChild(el("td", { class: "mono", text: event.tool || "-" }));
    tr.appendChild(el("td", { class: "mono", text: event.run_id || "-" }));
    tr.appendChild(el("td", { class: "mono", text: event.task_id || "-" }));
    const message = el("td", { class: "stderr", text: truncateValue(ctx, event.message || "", 160) });
    message.title = event.message || "";
    tr.appendChild(message);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  return table;
}

function incidentDetailNode(incident, ctx) {
  const detail = el("div", { class: "incident-detail" });

  const facts = el("dl", { class: "incident-facts" });
  const fact = (label, value) => {
    facts.appendChild(el("dt", { text: label }));
    facts.appendChild(el("dd", { class: "mono", text: value }));
  };
  fact("grouping signature", incident.signature || "-");
  fact("classification", incident.class_label || incident.class || "-");
  fact("actor", incident.actor || "-");
  fact("surface", incident.surface || "-");
  if (incident.activity_id) fact("step", incident.activity_id);
  fact("first seen", ctx.fmtAbsTime ? ctx.fmtAbsTime(incident.first_ts) : incident.first_ts || "-");
  fact("last seen", ctx.fmtAbsTime ? ctx.fmtAbsTime(incident.last_ts) : incident.last_ts || "-");
  fact(
    "raw events",
    `${asCount(incident.event_count)} (${asCount(incident.root_event_count)} root · ${asCount(incident.propagated_event_count)} propagated)`,
  );
  const runIds = Array.isArray(incident.run_ids) ? incident.run_ids : [];
  const taskIds = Array.isArray(incident.task_ids) ? incident.task_ids : [];
  fact("runs", runIds.length ? runIds.join(" · ") : "none recorded");
  fact("tasks", taskIds.length ? taskIds.join(" · ") : "none recorded");
  detail.appendChild(facts);

  const propagation = Array.isArray(incident.propagation) ? incident.propagation : [];
  if (propagation.length > 0) {
    const chain = el("div", { class: "incident-propagation" });
    chain.appendChild(el("div", {
      class: "incident-section-title",
      text: `Propagation from this root (${propagation.length} downstream failures, not independent root causes)`,
    }));
    for (const link of propagation) {
      chain.appendChild(el("div", { class: "incident-propagation-link" }, [
        el("span", { class: "chain-mark", text: "↳" }),
        el("span", { class: "chain-surface mono", text: link.surface || "-" }),
        el("span", { class: "chain-count", text: eventCountLabel(link.event_count) }),
        el("span", { class: "chain-message", text: truncateValue(ctx, link.message || link.signature || "", 140) }),
      ]));
    }
    detail.appendChild(chain);
  }

  const allEvents = Array.isArray(incident.events) && incident.events.length
    ? incident.events
    : [
        ...(Array.isArray(incident.sample_events) ? incident.sample_events : []),
        ...((Array.isArray(incident.propagation) ? incident.propagation : [])
          .flatMap((link) => Array.isArray(link.sample_events) ? link.sample_events : [])),
      ];
  if (allEvents.length > 0) {
    detail.appendChild(el("div", {
      class: "incident-section-title",
      text: `Underlying audit events (${allEvents.length} of ${asCount(incident.event_count)} shown)`,
    }));
    detail.appendChild(incidentEvidenceTable(allEvents, ctx));
  }

  const actions = el("div", { class: "incident-actions" });
  const rawButton = el("button", {
    class: "chip",
    text: "Open raw audit events",
    title: "Every underlying event stays in the raw Audit view",
  });
  rawButton.type = "button";
  rawButton.addEventListener("click", () => navigateToDrilldown({
    role: incident.actor || null,
    tool: incident.surface || null,
    status: incident.class === "denied" ? "denied" : "failure",
  }));
  actions.appendChild(rawButton);
  if (runIds.length > 0 && hasCtx(ctx, "setActiveTab")) {
    const runButton = el("button", { class: "chip", text: `Open run ${runIds[0]}` });
    runButton.type = "button";
    runButton.addEventListener("click", () => ctx.setActiveTab(`runs/${encodeURIComponent(runIds[0])}`));
    actions.appendChild(runButton);
  }
  detail.appendChild(actions);

  return detail;
}

function incidentRowNode(incident, ctx) {
  const key = incident.incident_id || incident.signature || "";
  const expanded = expandedIncidents.has(key);
  const article = el("article", {
    class: `incident-row ${incident.class || "unexpected"} ${expanded ? "open" : ""}`,
  });
  article.dataset.key = `incident-${key}`;
  // Expansion is part of the row's identity: without it a keyed diff would
  // reuse the collapsed node and swallow the click that opened it.
  article.dataset.hash = JSON.stringify([incident.event_count, incident.last_ts, expanded]);

  const header = el("button", {
    class: "incident-head",
    title: "Show the exact audit events behind this incident",
  }, [
    el("span", { class: "incident-caret", text: expanded ? "▾" : "▸" }),
    el("span", { class: `incident-class ${incident.class || "unexpected"}`, text: incident.class_label || incident.class || "failure" }),
    el("span", { class: "incident-surface mono", text: incident.surface || "-" }),
    el("span", { class: "incident-actor", text: incident.actor || "unknown actor" }),
    el("span", {
      class: "incident-count",
      title: `${asCount(incident.event_count)} raw audit events collapsed into this incident`,
      text: eventCountLabel(incident.event_count),
    }),
    el("span", { class: "incident-when", text: fmtRelativeValue(ctx, incident.last_ts) }),
  ]);
  header.type = "button";
  header.setAttribute("aria-expanded", expanded ? "true" : "false");
  header.addEventListener("click", () => {
    if (expandedIncidents.has(key)) expandedIncidents.delete(key);
    else expandedIncidents.add(key);
    renderDiagnostics(ctx);
  });
  article.appendChild(header);
  article.appendChild(el("div", {
    class: "incident-message",
    text: truncateValue(ctx, incident.message || incident.signature || "", 220),
  }));
  if (expanded) article.appendChild(incidentDetailNode(incident, ctx));
  return article;
}

function renderIncidents(payload, ctx) {
  const body = $("diag-body");
  const incidents = Array.isArray(payload && payload.incidents) ? payload.incidents : [];
  const summary = incidentSummaryNode(payload || {});
  if (incidents.length === 0) {
    syncNodes(body, [summary, el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: "No failure incidents in this window." }),
    ])]);
    return;
  }
  const list = el("div", { class: "incident-list" });
  for (const incident of incidents) list.appendChild(incidentRowNode(incident, ctx));
  syncNodes(body, [summary, list]);
}

function renderDiagnostics(ctx = {}) {
  const sub = ctx.getActiveDiagSubtab ? ctx.getActiveDiagSubtab() : "metrics";
  const last = ctx.getLastDiagnostics ? ctx.getLastDiagnostics() : { metrics: [], errors: [], incidents: null, implement_one: [], implement_one_by_complexity: [], completion_by_complexity: [] };

  if (!panelCanRender("diag-body")) return;
  if (ctx.getLastDiagnostics && last[sub] == null) {
    resetPanel("diag-body", "diag-count");
    return;
  }

  if (sub === "incidents") {
    const payload = last.incidents || {};
    // Both counts in the header: grouped incidents, and the raw failed events
    // they were derived from. Neither is inferable from the other.
    $("diag-count").textContent =
      `${asCount(payload.failure_categories && payload.failure_categories.unexpected && payload.failure_categories.unexpected.incidents)} unexpected / ${asCount(payload.incident_count)} all incidents / ${asCount(payload.raw_failed_events)} failed events`;
    renderIncidents(payload, ctx);
    return;
  }

  const rows = last[sub] || [];
  const count = $("diag-count");
  if (sub === "errors") {
    count.textContent = `${rows.length} error events this month`;
    count.title = `Step and event failures for the current month, capped at the diag URL parameter (default 50). Distinct from header Failed runs (${getWindow()} Failed, Timeout, and Interrupted job runs) and Recent Runs' failed filter (durable Failed job runs, no window).`;
  } else {
    count.textContent = `${rows.length} metric entries this month`;
    count.title = "Invocation metrics for the current month.";
  }
  const columns =
    sub === "metrics"
      ? getDiagMetricsColumns(ctx)
      : getDiagErrorsColumns(ctx);
  renderDiagnosticsTable(
    rows,
    columns,
    ctx,
    sub === "errors"
      ? "No error events this month (step/event failures, not job-run states)."
      : "No metric entries this month.",
  );
}

// ORB-11655: keyed cards, so the 30 s refresh replaces only what moved.
// Emptying this scroll box instead dropped the operator's scroll position on
// every tick.
export function renderDiagnosticsSideCard(last, ctx) {
  const container = $("diag-implement-one-body");
  if (!container) return;
  syncNodes(container, [
    completionByComplexityCard(last.completion_by_complexity || []),
    ...implementOneCards(last.implement_one_by_complexity || [], last.implement_one || [], ctx),
  ]);
}

function keyed(node, key, source) {
  node.dataset.key = key;
  node.dataset.hash = JSON.stringify(source);
  return node;
}

function metricsCard(title, rows, cols) {
  const card = el("div", { class: "audit-summary-card" });
  card.appendChild(el("div", { class: "card-title", text: title }));
  const body = el("div", { class: "card-body" });
  
  const table = el("table", { class: "summary-table" });
  const thead = el("thead");
  const tr = el("tr");
  for (const c of cols) tr.appendChild(el("th", { class: c.num ? "num" : "", text: c.label }));
  thead.appendChild(tr);
  table.appendChild(thead);

  const tbody = el("tbody");
  for (const item of rows) {
    const row = el("tr");
    for (const c of cols) {
      const val = c.format ? c.format(item[c.key]) : item[c.key];
      row.appendChild(el("td", { class: c.num ? "num" : "", text: val }));
    }
    tbody.appendChild(row);
  }
  table.appendChild(tbody);
  body.appendChild(table);
  card.appendChild(body);
  return card;
}

function formatCountRate(count, total) {
  const n = Number(count) || 0;
  const d = Number(total) || 0;
  if (d <= 0) return `${n} / ${d}`;
  return `${n} / ${d} (${((n / d) * 100).toFixed(1)}%)`;
}

function complexityLabel(value) {
  return value === "unset" ? "unset (unlabeled)" : (value || "unset (unlabeled)");
}

function completionByComplexityCard(rows) {
  if (!rows.length) {
    const card = el("div", { class: "audit-summary-card" });
    card.appendChild(el("div", { class: "card-title", text: "Task completion by complexity" }));
    const body = el("div", { class: "card-body" });
    body.appendChild(el("div", { class: "empty", text: "No tasks." }));
    card.appendChild(body);
    return keyed(card, "completion-by-complexity", []);
  }
  const statusCols = [
    { key: "complexity", label: "complexity" },
    { key: "total", label: "n", num: true },
    { key: "done", label: "done", num: true },
    { key: "rejected", label: "rejected", num: true },
    { key: "archived", label: "archived", num: true },
  ];
  const tableRows = rows.map((bucket) => {
    const byStatus = {};
    for (const status of bucket.statuses || []) {
      byStatus[status.status] = status;
    }
    const total = bucket.total || 0;
    const cell = (name) => formatCountRate((byStatus[name] || {}).count || 0, total);
    return {
      complexity: complexityLabel(bucket.complexity),
      total,
      done: cell("done"),
      rejected: cell("rejected"),
      archived: cell("archived"),
    };
  });
  return keyed(
    metricsCard("Task completion by complexity", tableRows, statusCols),
    "completion-by-complexity",
    tableRows,
  );
}

function implementOneCards(byComplexity, fallbackRows, ctx = {}) {
  const durCols = [
    { key: "actor", label: "actor" },
    { key: "n", label: "n", num: true },
    { key: "avg", label: "avg", num: true, format: (v) => fmtDurationValue(ctx, v) },
    { key: "p50", label: "p50", num: true, format: (v) => fmtDurationValue(ctx, v) },
    { key: "p95", label: "p95", num: true, format: (v) => fmtDurationValue(ctx, v) }
  ];
  const bands = Array.isArray(byComplexity) ? byComplexity.filter((band) => (band.actors || []).length) : [];
  if (bands.length) {
    return bands.map((band) => {
      const label = complexityLabel(band.complexity);
      return keyed(
        metricsCard(
          `Average implement_one duration by actor (30d) · ${label} · n=${band.n || 0}`,
          band.actors,
          durCols,
        ),
        `implement-one:${band.complexity}`,
        [band.n || 0, band.actors],
      );
    });
  }
  if (!fallbackRows.length) {
    const empty = el("div", { class: "empty", text: "No implement_one runs in last 30d." });
    return [keyed(empty, "implement-one", [])];
  }
  return [keyed(
    metricsCard("Average implement_one duration by actor (30d)", fallbackRows, durCols),
    "implement-one",
    fallbackRows,
  )];
}

export { renderDiagnostics };
