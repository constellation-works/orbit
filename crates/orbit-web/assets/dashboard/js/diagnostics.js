// Orbit dashboard diagnostics-domain (errors table and incidents). Metrics
// renders its own full-width view from metrics.js.
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

import { panelCanRender, resetPanel, el, syncNodes, getWindow, getWorkspace, formatDateTime, listItems, runHref } from './common.js';
import { renderIncidents } from './incidents.js';

const $ = (id) => document.getElementById(id);

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

function errorRunLabel(row) {
  if (row.job_run) return row.job_run;
  if (row.affiliation === "unaffiliated") return "unaffiliated";
  return "-";
}

function runDashboardLink(runId, workspaceId) {
  const link = el("a", { class: "mono", text: runId, title: `Open ${runId}` });
  link.href = runHref(runId, { workspace: workspaceId });
  return link;
}

// The feed bounds each message and flags a cut one `truncated` [ORB-15527].
// The owning run keeps the full text: its step error and run events.
function truncatedMessageNote(row, ctx) {
  if (row.truncated !== true) return null;
  if (!row.job_run) {
    return el("p", { class: "error-message-note", text: "The feed shows the start of this long message; the process log keeps the rest." });
  }
  const workspaceId = row.workspace_id || getWorkspace();
  const link = runDashboardLink(row.job_run, workspaceId);
  link.textContent = "Open the run";
  link.addEventListener("click", event => {
    event.stopPropagation();
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0) return;
    event.preventDefault();
    if (hasCtx(ctx, "navigateToRun")) ctx.navigateToRun(row.job_run, workspaceId);
  });
  return el("p", { class: "error-message-note" }, ["The feed shows the start of this long message. ", link, " for the full text."]);
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
  const note = truncatedMessageNote(row, ctx);
  if (note) details.appendChild(note);
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
  const sub = ctx.getActiveDiagSubtab ? ctx.getActiveDiagSubtab() : "errors";
  const last = ctx.getLastDiagnostics ? ctx.getLastDiagnostics() : { errors: [], incidents: null };

  if (!panelCanRender("diag-body")) return;
  if (ctx.getLastDiagnostics && last[sub] == null) {
    resetPanel("diag-body", "diag-count");
    return;
  }

  if (sub === "incidents") {
    renderIncidents(last.incidents || {}, ctx);
    return;
  }

  const rows = listItems(last.errors);
  const count = $("diag-count");
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
  const columns = getDiagErrorsColumns(ctx).filter(column =>
    ["ts", "source", "job_run", "message"].includes(column.key) || errorColumnHasData(rows, column.key));
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
}

export { renderDiagnostics };
