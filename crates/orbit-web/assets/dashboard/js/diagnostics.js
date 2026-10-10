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

import { incidentClassLabel, auditActorLabel, panelCanRender, resetPanel, el, syncNodes, getWindow, getHost, getWorkspace, formatDateTime, listItems, runHref } from './common.js';
import { navigateToDrilldown } from './audit.js';

const $ = (id) => document.getElementById(id);

// ORB-10871: which incidents the operator has expanded. Module-scoped (like
// audit.js's expandedAuditIds) so a refresh tick does not collapse the row
// someone is reading. Recurrence rows use their own set so opening a run
// does not collapse the group, and a refresh keeps both.
const expandedIncidents = new Set();
const expandedRecurrences = new Set();
let incidentClass = "unexpected";
export function getIncidentClass() { return incidentClass; }
let agentDiagnosticsOpen = false;

function shortenWorktreePaths(message) {
  return String(message || "").replace(/(?:\/[\w.@~+-]+)+\/\.orbit\/state\/worktrees\/[^\s/:"'<>]+\/?/g, "[worktree]/");
}

function recoverableAgentDiagnostic(row) {
  if (row.source !== "agent-stderr") return false;
  const lines = String(row.message || "").split("\n").filter(Boolean);
  // Codex logs patch verification under `codex_core::tools::router` (the tool
  // name only appears in the message) and model refresh timeouts under
  // `codex_models_manager::manager`.
  const target = row.target || "";
  const patchTarget = /tools::router|apply_patch/i.test(target);
  const modelTarget = /models?_manager/i.test(target);
  return lines.length > 0 && lines.every(message =>
    (patchTarget && /apply_patch verification failed|Failed to find expected lines/i.test(message))
    || (modelTarget && /request timed out/i.test(message)));
}

function hasCtx(ctx, key) {
  return ctx && typeof ctx[key] === "function";
}

// A relative age names its absolute instant in the cell's title.
function relativeCell(ctx, v, td) {
  if (td && v) td.title = formatDateTime(v);
  return fmtRelativeValue(ctx, v);
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
    { key: "ts", label: "time", num: false, render: (v, _row, td) => relativeCell(ctx, v, td) },
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
      render: (v) => (v == null ? "-" : Number(v).toLocaleString("en-US")),
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

function dashboardHref(hash, workspaceId = getWorkspace()) {
  const url = new URL(window.location.href);
  if (getHost()) url.searchParams.set("host", getHost());
  if (workspaceId) url.searchParams.set("workspace", workspaceId);
  url.hash = hash;
  return url.href;
}

function dashboardLink(label, hash, workspaceId = getWorkspace()) {
  const link = el("a", { class: "mono", text: label, title: `Open ${label}` });
  link.href = dashboardHref(hash, workspaceId);
  return link;
}

function runDashboardLink(runId, workspaceId) {
  const link = el("a", { class: "mono", text: runId, title: `Open ${runId}` });
  link.href = runHref(runId, { workspace: workspaceId });
  return link;
}

function errorMessageDisclosure(value, row, ctx, td) {
  const full = value || "";
  const shortened = shortenWorktreePaths(full);
  td.title = row.target ? `${row.target}: ${full}` : full;
  const details = el("details", { class: "error-message" });
  const summary = el("summary");
  summary.appendChild(el("span", {
    class: "error-message-preview",
    text: truncateValue(ctx, shortened, 220),
  }));
  details.appendChild(summary);
  details.appendChild(el("div", { class: "error-message-full", text: full }));
  return details;
}

function errorColumnHasData(rows, key) {
  if (key === "recovered") return rows.some(row => row.recovered === true);
  return rows.some(row => {
    const value = row[key];
    return value != null && value !== false && String(value).trim() !== "" && String(value).trim() !== "-";
  });
}

function loadMoreErrorsLink(payload, ctx) {
  if (!errorsCoverageLabel(payload)) return null;
  const url = new URL(window.location.href);
  const configured = Number.parseInt(url.searchParams.get("diag") || "50", 10);
  const current = Number.isInteger(configured) && configured > 0 ? configured : 50;
  const next = Math.min(current + 50, 200);
  if (next <= current) return null;
  url.searchParams.set("diag", String(next));
  const link = el("a", {
    class: "diagnostics-load-more",
    text: "Load more errors",
    title: `Load up to ${next} error events`,
  });
  link.href = url.href;
  link.addEventListener("click", event => {
    event.stopPropagation();
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0) return;
    if (!hasCtx(ctx, "loadMoreErrors")) return;
    event.preventDefault();
    ctx.loadMoreErrors(next);
  });
  return link;
}

function getDiagErrorsColumns(ctx) {
  return [
    { key: "ts", label: "time", num: false, render: (v, _row, td) => relativeCell(ctx, v, td) },
    { key: "source", label: "source", num: false },
    {
      key: "job_run",
      label: "run",
      num: false,
      render: (_v, row) => {
        const runId = row.job_run;
        if (!runId) return errorRunLabel(row);
        const workspaceId = row.workspace_id || getWorkspace();
        const link = runDashboardLink(runId, workspaceId);
        link.addEventListener("click", event => {
          event.stopPropagation();
          if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0) return;
          event.preventDefault();
          if (hasCtx(ctx, "navigateToRun")) ctx.navigateToRun(runId, workspaceId);
        });
        return link;
      },
    },
    { key: "provider", label: "provider", num: false, render: (v) => v || "-" },
    { key: "step", label: "step", num: false, render: (v) => v || "-" },
    { key: "target", label: "target", num: false, render: (v) => v || "-" },
    { key: "recovered", label: "recovery", num: false, render: (v) => v ? "recovered — run succeeded" : "-" },
    {
      key: "message",
      label: "message",
      num: false,
      cellClass: "stderr",
      render: (v, row, td) => errorMessageDisclosure(v, row, ctx, td),
    },
  ];
}

function renderDiagnosticsTable(rows, columns, ctx, emptyText, { cards = false, body = $("diag-body") } = {}) {
  
  if (!rows || rows.length === 0) {
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: emptyText || "No entries in this window." })
    ])]);
    return;
  }
  
  let table = body.querySelector("table.scoreboard-table");
  let tbody;
  const tableSig = `${cards ? "cards:" : ""}${columns.map(c => c.key).join("-")}`;
  if (!table || table.dataset.sig !== tableSig) {
    // `cards` tables restack each row as a card on narrow screens (health.css).
    table = el("table", { class: `scoreboard-table${cards ? " card-table diag-table" : ""}` });
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
        (col.num ? "num" : "") + (col.cellClass ? ` ${col.cellClass}` : "") + ` c-${col.key}`;
      const td = el("td", { class: baseClass });
      const v = row[col.key];
      const rendered = col.render ? col.render(v, row, td) : v == null ? "" : String(v);
      if (rendered && typeof rendered === "object" && rendered.nodeType) td.appendChild(rendered);
      else td.textContent = rendered;
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
// Stored incidents stay one per run. The list rolls identical signatures up
// across runs so a routine that fails on every fire is one recurrence.

function asCount(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function eventCountLabel(value) {
  const count = asCount(value);
  return `${count} event${count === 1 ? "" : "s"}`;
}

const INCIDENT_CLASS_ORDER = ["unexpected", "expected", "denied", "diagnostic"];

function incidentSummaryNode(payload, ctx) {
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
  const chips = el("div", { class: "incident-class-chips", role: "group", "aria-label": "Incident class" });
  for (const key of ["all", ...INCIDENT_CLASS_ORDER]) {
    const count = asCount(byClass[key]);
    const events = asCount(eventsByClass[key]);
    const category = categories[key] || {};
    const categoryRuns = asCount(category.affected_runs);
    const chip = el("button", {
      class: `incident-class-chip ${key}`,
      type: "button",
      "aria-pressed": incidentClass === key ? "true" : "false",
      title: `${labels[key] || key}: ${key === "all" ? incidents : count} incidents (window ${window})`,
      text: key === "all" ? `All: ${incidents}` : `${incidentClassLabel(key, labels[key])}: ${count} incidents · ${events} raw · ${categoryRuns} runs`,
    });
    chip.dataset.class = key;
    chip.addEventListener("click", () => {
      if (incidentClass === key) return;
      incidentClass = key;
      renderIncidents(payload, ctx);
      if (hasCtx(ctx, "refreshDiagnostics")) ctx.refreshDiagnostics();
    });
    chips.appendChild(chip);
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
    const runCell = el("td", { class: "mono" });
    if (event.run_id) {
      const workspaceId = event.workspace_id || getWorkspace();
      const link = runDashboardLink(event.run_id, workspaceId);
      link.addEventListener("click", click => {
        click.stopPropagation();
        if (click.metaKey || click.ctrlKey || click.shiftKey || click.altKey || click.button !== 0) return;
        click.preventDefault();
        if (hasCtx(ctx, "navigateToRun")) ctx.navigateToRun(event.run_id, workspaceId);
      });
      runCell.appendChild(link);
    } else {
      runCell.textContent = "-";
    }
    tr.appendChild(runCell);
    const taskCell = el("td", { class: "mono" });
    if (event.task_id) {
      taskCell.appendChild(dashboardLink(
        event.task_id,
        `tasks?status=all&q=${encodeURIComponent(event.task_id)}`,
        event.workspace_id || getWorkspace(),
      ));
    } else {
      taskCell.textContent = "-";
    }
    tr.appendChild(taskCell);
    const message = el("td", { class: "stderr", text: truncateValue(ctx, event.message || "", 160) });
    message.title = event.message || "";
    tr.appendChild(message);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  return table;
}

// The grouping signature is an internal key (`unexpected|role=…|msg=…`), so a
// row with no recorded message says so rather than showing it; the signature
// stays in the expanded details.
function incidentMessageText(row) {
  const message = typeof row.message === "string" ? row.message.trim() : "";
  return message || `${row.surface || "unknown surface"} failed; no message recorded`;
}

function incidentDetailNode(incident, ctx) {
  const detail = el("div", { class: "incident-detail" });

  const facts = el("dl", { class: "incident-facts" });
  const fact = (label, value, title = "") => {
    facts.appendChild(el("dt", { text: label }));
    facts.appendChild(el("dd", { class: "mono", text: value, title }));
  };
  fact("grouping signature", incident.signature || "-");
  fact("classification", incidentClassLabel(incident.class, incident.class_label), incident.class_label || incident.class);
  fact("actor", auditActorLabel(incident.actor), incident.actor);
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
        el("span", { class: "chain-message", text: truncateValue(ctx, incidentMessageText(link), 140) }),
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
  rawButton.addEventListener("click", () => {
    const eventIds = Array.isArray(incident.events)
      ? incident.events
        .map(event => event.id)
        .filter(id => Number.isSafeInteger(id) && id > 0)
      : [];
    const completeEventIds = eventIds.length > 0 && eventIds.length === asCount(incident.event_count)
      ? eventIds
      : null;
    navigateToDrilldown({
      role: completeEventIds ? null : (incident.actor || null),
      tool: completeEventIds || incident.has_tool_identity === false ? null : (incident.surface || null),
      eventIds: completeEventIds,
    });
  });
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

function incidentRunIds(incident) {
  if (!Array.isArray(incident.run_ids)) return [];
  return incident.run_ids
    .map(id => (id == null ? "" : String(id).trim()))
    .filter(Boolean);
}

function incidentRowNode(incident, ctx, options = {}) {
  const nested = options.nested === true;
  const key = incident.incident_id || incident.signature || "";
  const expanded = expandedIncidents.has(key);
  const article = el("article", {
    class: `incident-row ${incident.class || "unexpected"} ${expanded ? "open" : ""}`,
  });
  article.dataset.key = `incident-${key}`;
  // Expansion is part of the row's identity: without it a keyed diff would
  // reuse the collapsed node and swallow the click that opened it.
  article.dataset.hash = JSON.stringify([incident.event_count, incident.last_ts, expanded, nested]);

  const runIds = incidentRunIds(incident);
  const header = el("button", {
    class: "incident-head",
    title: nested
      ? `Incident ${key || "-"} · show the audit events for this run`
      : "Show the exact audit events behind this incident",
  }, [
    el("span", { class: "incident-caret", text: expanded ? "▾" : "▸" }),
    el("span", { class: `incident-class ${incident.class || "unexpected"}`, text: incidentClassLabel(incident.class, incident.class_label), title: incident.class_label || incident.class }),
    el("span", { class: "incident-surface mono", text: incident.surface || "-" }),
    nested
      ? el("span", { class: "incident-run mono", text: runIds.length ? runIds.join(" · ") : "no run recorded" })
      : null,
    el("span", { class: "incident-actor", text: incident.actor ? auditActorLabel(incident.actor) : "unknown actor", title: incident.actor }),
    el("span", {
      class: "incident-count",
      title: `${asCount(incident.event_count)} raw audit events collapsed into this incident`,
      text: eventCountLabel(incident.event_count),
    }),
    el("span", { class: "incident-when", text: fmtRelativeValue(ctx, incident.last_ts), title: formatDateTime(incident.last_ts) }),
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
    text: truncateValue(ctx, incidentMessageText(incident), 220),
  }));
  if (expanded) article.appendChild(incidentDetailNode(incident, ctx));
  return article;
}

// The store keys an incident by (run scope, signature), so the same failure in
// a later run is a new incident. The signature is that shared identity. An
// incident with no signature stays on its own: grouping those would merge
// unrelated rows the server could not identify.
function incidentGroupKey(incident) {
  const signature = typeof incident.signature === "string" ? incident.signature.trim() : "";
  if (!signature) return "";
  return `${incident.class || "unexpected"}\u0000${signature}`;
}

function incidentInstant(incident, field) {
  const time = Date.parse(incident[field] || "");
  return Number.isFinite(time) ? time : null;
}

function groupIncidentsForRecurrence(incidents) {
  const groups = [];
  const byKey = new Map();
  incidents.forEach((incident, index) => {
    const key = incidentGroupKey(incident);
    if (!key) {
      groups.push({
        key: `single:${index}:${incident.incident_id || ""}`,
        incidents: [incident],
        recurring: false,
        order: index,
      });
      return;
    }
    let group = byKey.get(key);
    if (!group) {
      group = { key, incidents: [], recurring: false, order: index };
      byKey.set(key, group);
      groups.push(group);
    }
    group.incidents.push(incident);
  });
  for (const group of groups) group.recurring = group.incidents.length > 1;

  const lastInstant = (group) => group.incidents.reduce((latest, incident) => {
    const time = incidentInstant(incident, "last_ts");
    return time == null ? latest : Math.max(latest, time);
  }, 0);
  const recurring = groups
    .filter(group => group.recurring)
    .sort((a, b) => lastInstant(b) - lastInstant(a) || a.key.localeCompare(b.key));
  for (const group of recurring) {
    group.incidents.sort((a, b) => {
      const byTime = (incidentInstant(b, "last_ts") || 0) - (incidentInstant(a, "last_ts") || 0);
      if (byTime !== 0) return byTime;
      return String(a.incident_id || "").localeCompare(String(b.incident_id || ""));
    });
  }
  const singles = groups
    .filter(group => !group.recurring)
    .sort((a, b) => a.order - b.order);
  return [...recurring, ...singles];
}

function distinctRunCount(incidents) {
  const ids = new Set();
  for (const incident of incidents) {
    for (const id of incidentRunIds(incident)) ids.add(id);
  }
  return ids.size;
}

function recurrenceCountLabel(incidents) {
  const runs = distinctRunCount(incidents);
  if (runs >= 2) return `${runs} runs`;
  if (runs === 1) return `${incidents.length} incidents · 1 run`;
  return `${incidents.length} incidents`;
}

const CADENCE_MINUTE_MS = 60 * 1000;
const CADENCE_HOUR_MS = 60 * CADENCE_MINUTE_MS;
const CADENCE_DAY_MS = 24 * CADENCE_HOUR_MS;

function formatCadence(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return "";
  if (ms >= CADENCE_DAY_MS * 0.75) {
    const days = Math.max(1, Math.round(ms / CADENCE_DAY_MS));
    if (Math.abs(ms - days * CADENCE_DAY_MS) <= 30 * CADENCE_MINUTE_MS) return `every ${days} d`;
  }
  if (ms >= CADENCE_HOUR_MS * 0.75) {
    const hours = Math.max(1, Math.round(ms / CADENCE_HOUR_MS));
    if (Math.abs(ms - hours * CADENCE_HOUR_MS) <= 90 * 1000) return `every ${hours} h`;
  }
  if (ms >= CADENCE_MINUTE_MS * 0.75) {
    const minutes = Math.max(1, Math.round(ms / CADENCE_MINUTE_MS));
    return `every ${minutes} min`;
  }
  return `every ${Math.max(1, Math.round(ms / 1000))} s`;
}

function medianGap(gaps) {
  const mid = Math.floor(gaps.length / 2);
  if (gaps.length % 2 === 1) return gaps[mid];
  return (gaps[mid - 1] + gaps[mid]) / 2;
}

// Cadence is the median gap between fires. A spread within 25% of that median
// reads as a schedule (`every 20 min`); a wider spread is qualified.
function cadenceLabel(incidents) {
  const times = incidents
    .map(incident => incidentInstant(incident, "last_ts") ?? incidentInstant(incident, "first_ts"))
    .filter(time => time != null)
    .sort((a, b) => a - b);
  const gaps = [];
  for (let index = 1; index < times.length; index += 1) {
    const gap = times[index] - times[index - 1];
    if (gap > 0) gaps.push(gap);
  }
  if (gaps.length === 0) return "";
  gaps.sort((a, b) => a - b);
  const median = medianGap(gaps);
  const phrase = formatCadence(median);
  if (!phrase) return "";
  const tight = gaps[0] >= median * 0.75 && gaps[gaps.length - 1] <= median * 1.25;
  return tight ? phrase : phrase.replace(/^every /, "about every ");
}

function recurrenceBounds(incidents) {
  let first = null;
  let last = null;
  const consider = (current, candidate, earlier) => {
    if (!candidate) return current;
    const time = Date.parse(candidate);
    if (!Number.isFinite(time)) return current;
    if (!current) return { text: candidate, time };
    const replace = earlier ? time < current.time : time > current.time;
    return replace ? { text: candidate, time } : current;
  };
  for (const incident of incidents) {
    first = consider(first, incident.first_ts || incident.last_ts, true);
    last = consider(last, incident.last_ts || incident.first_ts, false);
  }
  return { first: first ? first.text : "", last: last ? last.text : "" };
}

function sharedIncidentActor(incidents) {
  const actors = [...new Set(incidents.map(incident => (typeof incident.actor === "string" ? incident.actor : "")))];
  if (actors.length === 1) return actors[0];
  return null;
}

function recurrencePhrase(group, ctx, cap) {
  const bounds = recurrenceBounds(group.incidents);
  const count = recurrenceCountLabel(group.incidents);
  const countText = cap.capped ? `${count} in the newest ${cap.shown}` : count;
  const seen = `first ${bounds.first ? fmtRelativeValue(ctx, bounds.first) : "-"} · last ${bounds.last ? fmtRelativeValue(ctx, bounds.last) : "-"}`;
  const cadence = cadenceLabel(group.incidents);
  return {
    text: ["recurring", countText, seen, cadence].filter(Boolean).join(" · "),
    title: [
      bounds.first ? `first seen ${formatDateTime(bounds.first)}` : "",
      bounds.last ? `last seen ${formatDateTime(bounds.last)}` : "",
      cap.capped ? `run count is among the newest ${cap.shown} of ${cap.matching} incidents` : "",
    ].filter(Boolean).join(" · "),
  };
}

function listCap(payload) {
  const shown = asCount(payload && payload.shown_incident_count);
  const matching = asCount(payload && payload.matching_incident_count);
  return {
    capped: shown > 0 && matching > shown,
    shown,
    matching,
  };
}

function recurrenceRowNode(group, ctx, cap) {
  const expanded = expandedRecurrences.has(group.key);
  const sample = group.incidents[0];
  const actor = sharedIncidentActor(group.incidents);
  const phrase = recurrencePhrase(group, ctx, cap);
  const article = el("article", {
    class: ["incident-row", "incident-recurrence", sample.class || "unexpected", expanded ? "open" : ""].filter(Boolean).join(" "),
  });
  article.dataset.key = `recurrence-${group.key}`;
  article.dataset.hash = JSON.stringify([
    group.incidents.length,
    phrase.text,
    expanded,
    group.incidents.map(incident => [
      incident.incident_id,
      incident.event_count,
      incident.last_ts,
      expandedIncidents.has(incident.incident_id || incident.signature || ""),
    ]),
  ]);

  const header = el("button", {
    class: "incident-head",
    title: "Show each run of this recurring failure",
  }, [
    el("span", { class: "incident-caret", text: expanded ? "▾" : "▸" }),
    el("span", {
      class: `incident-class ${sample.class || "unexpected"}`,
      text: incidentClassLabel(sample.class, sample.class_label),
      title: sample.class_label || sample.class,
    }),
    el("span", { class: "incident-surface mono", text: sample.surface || "-" }),
    el("span", {
      class: "incident-actor",
      text: actor == null ? "several actors" : (actor ? auditActorLabel(actor) : "unknown actor"),
      title: actor || "",
    }),
    el("span", { class: "incident-recurrence-summary", text: phrase.text, title: phrase.title }),
  ]);
  header.type = "button";
  header.setAttribute("aria-expanded", expanded ? "true" : "false");
  header.addEventListener("click", () => {
    if (expandedRecurrences.has(group.key)) expandedRecurrences.delete(group.key);
    else expandedRecurrences.add(group.key);
    renderDiagnostics(ctx);
  });
  article.appendChild(header);
  article.appendChild(el("div", {
    class: "incident-message",
    text: truncateValue(ctx, incidentMessageText(sample), 220),
  }));
  if (expanded) {
    const runs = el("div", { class: "incident-recurrence-runs" });
    for (const incident of group.incidents) runs.appendChild(incidentRowNode(incident, ctx, { nested: true }));
    article.appendChild(runs);
  }
  return article;
}

function renderIncidents(payload, ctx) {
  const body = $("diag-body");
  const incidents = (Array.isArray(payload && payload.incidents) ? payload.incidents : [])
    .filter(incident => incidentClass === "all" || incident.class === incidentClass);
  const summary = incidentSummaryNode(payload || {}, ctx);
  if (incidents.length === 0) {
    syncNodes(body, [summary, el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: "No failure incidents in this window." }),
    ])]);
    return;
  }
  const cap = listCap(payload);
  const list = el("div", { class: "incident-list" });
  for (const group of groupIncidentsForRecurrence(incidents)) {
    list.appendChild(group.recurring
      ? recurrenceRowNode(group, ctx, cap)
      : incidentRowNode(group.incidents[0], ctx));
  }
  syncNodes(body, [summary, list]);
}

// The Errors feed reports where its sources were actually read from. When that
// is later than the window start (or the window is `all`), the header says so
// instead of implying the whole window was searched.
function errorsCoverageLabel(payload) {
  const coverage = Date.parse(payload && payload.coverage_since);
  if (Number.isNaN(coverage)) return "";
  const since = Date.parse(payload.since);
  if (!Number.isNaN(since) && coverage <= since) return "";
  return `covers since ${formatDateTime(payload.coverage_since)}`;
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
    // Full denominators live in the summary; the header names the displayed range.
    $("diag-count").textContent =
      `Newest ${asCount(payload.shown_incident_count)} of ${asCount(payload.matching_incident_count)} incidents · window ${payload.window || getWindow()}`;
    renderIncidents(payload, ctx);
    return;
  }

  const rows = sub === "errors" ? listItems(last.errors) : (last[sub] || []);
  const count = $("diag-count");
  if (sub === "errors") {
    const coverage = errorsCoverageLabel(last.errors);
    count.textContent = `${rows.length} error events · window ${getWindow()}${coverage ? ` · ${coverage}` : ""}`;
    count.title = coverage
      ? "Most recent step and event failures; log retention or the stderr read cap leaves the start of the selected window unread. Capped by the diag URL parameter (default 50)."
      : "Most recent step and event failures in the selected window; capped by the diag URL parameter (default 50).";
    const loadMore = loadMoreErrorsLink(last.errors, ctx);
    if (loadMore) {
      count.appendChild(document.createTextNode(" · "));
      count.appendChild(loadMore);
    }
  } else {
    count.textContent = `${rows.length} metric entries · window ${getWindow()}`;
    count.title = "Most recent invocation metrics in the selected window; capped by the diag URL parameter (default 50).";
  }
  const columns =
    sub === "metrics"
      ? getDiagMetricsColumns(ctx)
      : getDiagErrorsColumns(ctx).filter(column =>
        ["ts", "source", "job_run", "message"].includes(column.key) || errorColumnHasData(rows, column.key));
  if (sub === "errors") {
    const main = el("div", { class: "diagnostics-errors-main" });
    const internal = rows.filter(recoverableAgentDiagnostic);
    renderDiagnosticsTable(rows.filter(row => !recoverableAgentDiagnostic(row)), columns, ctx,
      rows.length ? "No other error events in this window." : "No error events in this window.", { cards: true, body: main });
    const children = [main];
    if (internal.length) {
      const details = el("details", { class: "agent-diagnostics" });
      details.open = agentDiagnosticsOpen;
      details.addEventListener("toggle", () => { if (details.isConnected) agentDiagnosticsOpen = details.open; });
      details.appendChild(el("summary", { text: `Agent diagnostics (${internal.length}) · patch verification and model timeouts` }));
      const content = el("div");
      renderDiagnosticsTable(internal, columns, ctx, "", { cards: true, body: content });
      details.appendChild(content);
      children.push(details);
    }
    syncNodes($("diag-body"), children);
    return;
  }
  renderDiagnosticsTable(
    rows,
    columns,
    ctx,
    "No metric entries in this window.",
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
  card.appendChild(el("div", { class: "card-title section-title", text: title }));
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
  return `${n}·${((n / d) * 100).toFixed(1)}%`;
}

function complexityLabel(value) {
  return value === "unset" ? "unset (unlabeled)" : (value || "unset (unlabeled)");
}

function completionByComplexityCard(rows) {
  if (!rows.length) {
    const card = el("div", { class: "audit-summary-card" });
    card.appendChild(el("div", { class: "card-title section-title", text: "Task completion by complexity" }));
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
