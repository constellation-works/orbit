// Orbit dashboard audit-domain rendering and actions.
// Pure vanilla JS, split into ES modules with no build step.

import { el, fetchJson, syncNodes, makeToggleRow, positiveIntParam, isAggregateView, renderPanelPlaceholder, getWindow, setWindow, getWorkspace, setWorkspace, persistScopeToUrl, DEFAULT_DASHBOARD_WINDOW } from './common.js';

const $ = (id) => document.getElementById(id);

const AUDIT_LIMIT = positiveIntParam("audit", 50);
const AUDIT_STATUSES = ["success", "failure", "denied"];
const AUDIT_SUBTABS = ["events", "policy"];

// Audit tab state (moved from app.js)
let lastAudit = [];
let auditFilter = {
  status: null,
  q: "",
  tool: null,
  role: null,
  // Filters audit Events by `execution_id` (the orbit invocation id). The CLI
  // SQLite audit table has no real `run_id` field, so this never identifies a
  // JobRun — see T20260427-26.
  execution_id: null,
  profile: null,
  // Time-window filter for the Events sub-tab. Accepts the same shorthands as
  // the API (`24h`, `7d`, `1w`, RFC3339); null means the API-side default.
  since: null,
  // Source metric from a scoreboard drill-down (display + hash only).
  metric: null,
  // Policy sub-tab only: scopes /api/diagnostics/denials to fs vs tool denials.
  policyKind: null,
};
let expandedAuditIds = new Set();
let activeAuditSubtab = "events";
let lastAuditPolicy = null;
let policySort = {
  by_profile: "count",
  by_target: "count",
  by_run: "count",
  by_execution: "count",
  by_agent: "count",
};

const POLICY_TABLES = [
  {
    id: "by_profile",
    label: "By Profile",
    nameField: "name",
    header: "profile",
    filterKey: "profile",
  },
  {
    id: "by_target",
    label: "By Target",
    nameField: "name",
    header: "target",
    filterKey: null,
  },
  {
    id: "by_run",
    label: "By JobRun",
    nameField: "run_id",
    header: "job_run_id",
    navigateTo: "job_run",
  },
  {
    id: "by_execution",
    label: "By Audit Invocation",
    nameField: "execution_id",
    header: "execution_id",
    navigateTo: "audit_execution",
  },
  {
    id: "by_agent",
    label: "By Agent",
    nameField: "agent",
    header: "agent",
    filterKey: "role",
  },
];

const AUDIT_COLUMNS = [
  { key: "time", label: "time" },
  { key: "role", label: "role" },
  { key: "tool", label: "tool" },
  { key: "command", label: "command" },
  { key: "target", label: "target" },
  { key: "status", label: "status" },
  { key: "exit", label: "exit", num: true },
  { key: "duration", label: "duration", num: true },
];

// Context injection helpers (mirror tasks.js pattern; ctx as last arg on public entry points)
function hasCtx(ctx, key) {
  return ctx && typeof ctx[key] === "function";
}

function fmtDurationValue(ctx, v) {
  return hasCtx(ctx, "fmtDuration") ? ctx.fmtDuration(v) : (v == null ? "-" : String(v));
}

function fmtTimestampValue(ctx, v) {
  return hasCtx(ctx, "fmtTimestamp") ? ctx.fmtTimestamp(v) : (v || "-");
}

function fmtRelativeValue(ctx, v) {
  return hasCtx(ctx, "fmtRelative") ? ctx.fmtRelative(v) : (v || "-");
}

function fmtAbsTimeValue(ctx, v) {
  return hasCtx(ctx, "fmtAbsTime") ? ctx.fmtAbsTime(v) : (v || "-");
}

function truncateValue(ctx, s, n = 18) {
  return hasCtx(ctx, "truncate") ? ctx.truncate(s, n) : String(s || "").slice(0, n);
}

function doRefresh(ctx) {
  if (hasCtx(ctx, "refreshDashboard")) {
    try { ctx.refreshDashboard(); } catch (_) {}
  }
}

function doSetActiveTab(ctx, route, opts) {
  if (hasCtx(ctx, "setActiveTab")) {
    try { ctx.setActiveTab(route, opts); } catch (_) {}
  }
}

function doNavigateToRun(ctx, runId) {
  if (hasCtx(ctx, "navigateToRun")) {
    try { ctx.navigateToRun(runId); } catch (_) {}
  }
}

function buildAuditHash() {
  const sp = new URLSearchParams();
  // The Audit Events tab and the Policy sub-tab serialize different filter
  // axes. The shared `auditFilter` object holds all of them; here we project
  // only the fields meaningful for the active sub-tab so the hash stays
  // self-describing and reload-safe.
  if (activeAuditSubtab === "policy") {
    if (auditFilter.policyKind) sp.set("kind", auditFilter.policyKind);
    if (auditFilter.profile) sp.set("profile", auditFilter.profile);
    if (auditFilter.role) sp.set("role", auditFilter.role);
  } else {
    if (auditFilter.since) sp.set("since", auditFilter.since);
    if (auditFilter.status) sp.set("status", auditFilter.status);
    if (auditFilter.tool) sp.set("tool", auditFilter.tool);
    if (auditFilter.role) sp.set("role", auditFilter.role);
    if (auditFilter.execution_id) sp.set("execution_id", auditFilter.execution_id);
    if (auditFilter.profile) sp.set("profile", auditFilter.profile);
    if (auditFilter.q) sp.set("q", auditFilter.q);
    if (auditFilter.metric) sp.set("metric", auditFilter.metric);
  }
  const path = activeAuditSubtab && activeAuditSubtab !== "events"
    ? `audit/${activeAuditSubtab}`
    : "audit";
  const qs = sp.toString();
  return qs ? `#${path}?${qs}` : `#${path}`;
}

function setAuditSubtab(name) {
  if (!AUDIT_SUBTABS.includes(name)) name = "events";
  activeAuditSubtab = name;
  for (const btn of document.querySelectorAll("#audit-subtabs .subtab")) {
    btn.classList.toggle("active", btn.dataset.subtab === name);
  }
  const eventsCtl = $("audit-events-controls");
  const eventsBody = $("audit-body");
  const policyBody = $("audit-policy-body");
  if (eventsCtl) eventsCtl.style.display = name === "events" ? "" : "none";
  if (eventsBody) eventsBody.style.display = name === "events" ? "" : "none";
  if (policyBody) policyBody.style.display = name === "policy" ? "" : "none";
  const title = $("audit-title");
  if (title) title.textContent = name === "policy" ? "Policy Denials" : "Audit Events";
}

function syncAuditControls() {
  const search = $("audit-search");
  if (search && search.value !== (auditFilter.q || "")) {
    search.value = auditFilter.q || "";
  }
  for (const chip of document.querySelectorAll("#audit-filter .chip")) {
    const status = chip.dataset.status;
    chip.classList.toggle("active", auditFilter.status === status);
  }
  renderScopeChips();
}

function removableChip(label, value, onRemove) {
  const chip = el("button", {
    class: "scope-chip",
    title: `Remove ${label} filter`,
  });
  chip.type = "button";
  chip.dataset.chip = label;
  chip.appendChild(el("span", { class: "scope-chip-k", text: label }));
  chip.appendChild(el("span", { class: "scope-chip-v", text: value }));
  chip.appendChild(el("span", { class: "scope-chip-x", text: "×" }));
  chip.addEventListener("click", (event) => {
    event.preventDefault();
    onRemove();
  });
  return chip;
}

function renderScopeChips() {
  const host = $("audit-scope-chips");
  if (!host) return;
  host.innerHTML = "";
  const workspace = getWorkspace();
  if (workspace) {
    host.appendChild(removableChip("workspace", workspace, () => {
      setWorkspace(null);
      persistScopeToUrl();
      const select = $("workspace-select");
      if (select) select.value = "";
      window.location.hash = buildAuditHash();
    }));
  }
  const windowLabel = effectiveAuditWindow();
  if (windowLabel) {
    const chip = removableChip("window", windowLabel, () => {
      auditFilter.since = null;
      setWindow(DEFAULT_DASHBOARD_WINDOW);
      persistScopeToUrl();
      window.location.hash = buildAuditHash();
    });
    if (windowLabel !== getWindow()) chip.classList.add("independent");
    host.appendChild(chip);
  }
  if (auditFilter.role) {
    host.appendChild(removableChip("actor", auditFilter.role, () => {
      auditFilter.role = null;
      window.location.hash = buildAuditHash();
    }));
  }
  if (auditFilter.status) {
    host.appendChild(removableChip("status", auditFilter.status, () => {
      auditFilter.status = null;
      window.location.hash = buildAuditHash();
    }));
  }
  if (auditFilter.metric) {
    host.appendChild(removableChip("metric", auditFilter.metric, () => {
      auditFilter.metric = null;
      window.location.hash = buildAuditHash();
    }));
  }
}

function applyAuditHashQuery(query) {
  // Mutates auditFilter from URLSearchParams (hash query). Called from setActiveTab.
  // Legacy run_id alias for execution_id is preserved for old deep links.
  auditFilter.status = query.get("status") || null;
  auditFilter.tool = query.get("tool") || null;
  auditFilter.role = query.get("role") || null;
  auditFilter.execution_id =
    query.get("execution_id") || query.get("run_id") || null;
  auditFilter.profile = query.get("profile") || null;
  auditFilter.q = query.get("q") || "";
  auditFilter.since = query.get("since") || (getWindow() === "all" ? null : getWindow());
  auditFilter.metric = query.get("metric") || null;
  const kindParam = query.get("kind");
  auditFilter.policyKind = kindParam === "fs" || kindParam === "tool" ? kindParam : null;
}

function getActiveAuditSubtab() {
  return activeAuditSubtab;
}

function setActiveAuditSubtabFromButton(name) {
  if (!AUDIT_SUBTABS.includes(name)) name = "events";
  activeAuditSubtab = name;
  setAuditSubtab(name);
}

function fetchAndRenderAudit(ctx) {
  // ORB-00040: /api/audit is per-workspace and 400s without a concrete
  // workspace. In the aggregate ("All workspaces") view render the placeholder
  // and skip the fetch — covers both the auto-refresh and audit-search paths.
  if (isAggregateView()) {
    renderPanelPlaceholder("audit-body");
    return Promise.resolve();
  }
  const sp = new URLSearchParams();
  sp.set("limit", String(AUDIT_LIMIT));
  const since = effectiveAuditWindow();
  if (since) sp.set("since", since);
  if (auditFilter.status) sp.set("status", auditFilter.status);
  if (auditFilter.tool) sp.set("tool", auditFilter.tool);
  if (auditFilter.role) sp.set("role", auditFilter.role);
  if (auditFilter.execution_id) sp.set("execution_id", auditFilter.execution_id);
  if (auditFilter.profile) sp.set("profile", auditFilter.profile);
  if (auditFilter.q) sp.set("q", auditFilter.q);
  return fetchJson(`/api/audit?${sp.toString()}`).then((events) => {
    lastAudit = events;
    renderAudit(events, ctx);
  });
}

function isNamedTool(name) {
  const trimmed = String(name || "").trim();
  return trimmed.length > 0 && trimmed !== "unknown";
}

function renderFailuresByToolCard(rateRows, failuresRows, onCardClick, window = "24h") {
  const failByTool = new Map();
  for (const f of failuresRows) {
    if (isNamedTool(f.tool)) failByTool.set(f.tool, f);
  }

  const container = el("div", { class: "audit-summary-container" });
  container.appendChild(el("h3", {
    class: "summary-title",
    text: `Unexpected Failures by Callable Tool (${String(window).toUpperCase()})`,
  }));
  const grid = el("div", { class: "tool-health-grid" });

  for (const row of rateRows) {
    if (!isNamedTool(row.tool)) continue;
    const rate = row.rate || 0;
    const severity = rate >= 0.14 ? "critical" : rate >= 0.05 ? "warning" : "normal";
    const card = el("div", { class: `health-card ${severity}` });

    const header = el("div", { class: "card-header" });
    header.appendChild(el("span", { class: "tool-name", text: row.tool }));
    header.appendChild(el("span", {
      class: "status-badge",
      text: `${(rate * 100).toFixed(1)}% Unexpected Failure Rate`,
    }));
    card.appendChild(header);

    const body = el("div", { class: "card-body" });
    const breakdown = failByTool.get(row.tool);
    const failures = row.failures != null ? row.failures : (breakdown ? (breakdown.count || 0) : 0);
    const total = row.total || 0;
    const failureWord = failures === 1 ? "failure" : "failures";
    const trendText = `${failures} unexpected ${failureWord} / ${total} comparable calls (successful + unexpected failed)`;
    body.appendChild(el("span", { class: "metric-trend", text: trendText }));
    card.appendChild(body);

    if (onCardClick) {
      card.classList.add("clickable");
      card.addEventListener("click", () => onCardClick(row));
    }
    grid.appendChild(card);
  }

  container.appendChild(grid);
  return container;
}

function renderAuditSummary(data, ctx) {
  const container = $("audit-summary-body");
  if (!container) return;

  // ORB-11655: keyed cards, so the 30 s refresh replaces only the cards whose
  // data moved. Emptying the container instead collapsed this scroll box and
  // dropped the operator's scroll position on every tick.
  const cards = [];
  const addCard = (key, card, source) => {
    card.dataset.key = key;
    card.dataset.hash = JSON.stringify(source);
    cards.push(card);
  };

  const createCard = (title, renderBody) => {
    const card = el("div", { class: "audit-summary-card" });
    card.appendChild(el("div", { class: "card-title", text: title }));
    const body = el("div", { class: "card-body" });
    renderBody(body);
    card.appendChild(body);
    return card;
  };

  const renderTable = (items, cols, onRowClick) => {
    return (body) => {
      if (!items || items.length === 0) {
        body.appendChild(el("div", { class: "empty", text: "No data" }));
        return;
      }
      const table = el("table", { class: "summary-table" });
      const thead = el("thead");
      const tr = el("tr");
      for (const c of cols) tr.appendChild(el("th", { class: c.num ? "num" : "", text: c.label, title: c.title }));
      thead.appendChild(tr);
      table.appendChild(thead);

      const tbody = el("tbody");
      for (const item of items) {
        const row = el("tr");
        if (onRowClick) {
          row.classList.add("clickable");
          row.addEventListener("click", () => onRowClick(item));
        }
        for (const c of cols) {
          const val = c.format ? c.format(item[c.key]) : item[c.key];
          row.appendChild(el("td", { class: c.num ? "num" : "", text: val }));
        }
        tbody.appendChild(row);
      }
      table.appendChild(tbody);
      body.appendChild(table);
    };
  };

  const filterByTool = (item) => {
    auditFilter.tool = auditFilter.tool === item.tool ? null : item.tool;
    syncAuditControls();
    window.location.hash = buildAuditHash();
  };

  const namedRates = (data.failure_rate_by_tool || []).filter((row) => isNamedTool(row.tool));
  const namedFailures = (data.failures_by_tool || []).filter((row) => isNamedTool(row.tool));
  if (namedRates.length) {
    addCard("failures-by-tool", renderFailuresByToolCard(
      namedRates,
      namedFailures,
      filterByTool,
      data.window || "24h",
    ), [namedRates, namedFailures, data.window || "24h"]);
  }

  const categoryOrder = ["unexpected", "expected", "denied", "diagnostic"];
  const categories = data.failure_categories || {};
  const categoryRows = categoryOrder.map((key) => ({
    key,
    label: (categories[key] && categories[key].label) || key,
    incidents: Number(categories[key] && categories[key].incidents) || 0,
    raw_events: Number(categories[key] && categories[key].raw_events) || 0,
    affected_runs: Number(categories[key] && categories[key].affected_runs) || 0,
  }));
  if (categoryRows.some((row) => row.incidents || row.raw_events)) {
    addCard("failure-categories", createCard(
      `Failure categories · window ${data.window || "24h"}`,
      renderTable(categoryRows, [
        { key: "label", label: "classification" },
        { key: "incidents", label: "incidents", num: true },
        { key: "raw_events", label: "raw events", num: true },
        { key: "affected_runs", label: "affected runs", num: true },
      ]),
    ), [categoryRows, data.window || "24h"]);
  }

  const lifecycleFailures = Number(data.lifecycle_diagnostic_events) || 0;
  const lifecycleIncidents = Number(data.lifecycle_diagnostic_incidents) || 0;
  if (lifecycleFailures > 0 || lifecycleIncidents > 0) {
    const label = data.lifecycle_diagnostic_label || "lifecycle diagnostics";
    const window = data.window || "24h";
    const lifecycleCard = el("div", { class: "audit-summary-card lifecycle-failure-card" });
    lifecycleCard.appendChild(el("div", { class: "card-title", text: label }));
    const body = el("div", { class: "card-body" });
    body.appendChild(el("div", {
      class: "lifecycle-failure-counts",
      text: `${lifecycleIncidents} incidents · ${lifecycleFailures} raw events · ${Number(data.lifecycle_diagnostic_affected_run_count) || 0} affected runs`,
    }));
    body.appendChild(el("div", {
      class: "metric-trend",
      text: `Failure-only diagnostic surfaces; excluded from callable-tool denominators and rates · window ${window}`,
    }));
    lifecycleCard.appendChild(body);
    addCard("lifecycle-diagnostics", lifecycleCard, [
      label,
      window,
      lifecycleIncidents,
      lifecycleFailures,
      Number(data.lifecycle_diagnostic_affected_run_count) || 0,
    ]);
  }

  if (data.duration_by_tool) {
    addCard("duration-by-tool", createCard("Top duration (avg)", renderTable(
      data.duration_by_tool,
      [
        { key: "tool", label: "tool" },
        { key: "count", label: "count", num: true },
        { key: "avg", label: "avg", num: true, format: (v) => fmtDurationValue(ctx, v) },
        { key: "p95", label: "p95", num: true, format: (v) => fmtDurationValue(ctx, v) }
      ],
      filterByTool
    )), data.duration_by_tool);
  }

  if (data.denials_by_tool || data.denials_by_reason) {
    const card = el("div", { class: "audit-summary-card" });
    card.appendChild(el("div", { class: "card-title", text: "Denials" }));
    const body = el("div", { class: "card-body" });
    const sectionLabel = (txt) => {
      const lbl = el("div", { class: "card-subtitle", text: txt });
      lbl.style.cssText = "padding: 6px 12px; font-size: 10px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--fg-dim); border-bottom: 1px solid var(--border); background: rgba(255,255,255,0.02);";
      return lbl;
    };
    const toolRows = data.denials_by_tool || [];
    body.appendChild(sectionLabel("By tool"));
    renderTable(
      toolRows,
      [{ key: "tool", label: "tool" }, { key: "count", label: "count", num: true }],
      filterByTool
    )(body);
    const reasonRows = data.denials_by_reason || [];
    body.appendChild(sectionLabel("By reason"));
    renderTable(
      reasonRows,
      [{ key: "reason", label: "reason" }, { key: "count", label: "count", num: true }],
      null
    )(body);
    card.appendChild(body);
    addCard("denials", card, [toolRows, reasonRows]);
  }

  if (data.role_split) {
    addCard("role-split", createCard("Role split", renderTable(
      data.role_split,
      [
        { key: "label", label: "role" },
        { key: "count", label: "events", num: true, title: "All audit events in the window" },
        { key: "mcp", label: "mcp", num: true, title: "Tool calls via MCP (subcommand = run-mcp)" },
        { key: "cli", label: "cli", num: true, title: "Tool calls via CLI (subcommand = run)" },
        { key: "other", label: "other", num: true, title: "Other CLI subcommands (non-tool, e.g. show/list)" },
        { key: "no_subcommand", label: "internal", num: true, title: "Internal/system events with no subcommand (e.g. lock reservations)" },
      ],
      (item) => {
        auditFilter.role = auditFilter.role === item.label ? null : item.label;
        syncAuditControls();
        window.location.hash = buildAuditHash();
      }
    )), data.role_split);
  }

  if (data.mcp_vs_cli_split) {
    addCard("mcp-vs-cli", createCard("MCP vs CLI", renderTable(
      data.mcp_vs_cli_split,
      [{ key: "label", label: "surface" }, { key: "count", label: "count", num: true }],
      null
    )), data.mcp_vs_cli_split);
  }

  syncNodes(container, cards);
}

function fetchAndRenderPolicy(ctx) {
  // ORB-00040: the audit Policy subtab reads /api/diagnostics/denials, which is
  // also per-workspace (the `Ws` extractor 400s without a concrete workspace),
  // so guard it the same way as the events subtab.
  if (isAggregateView()) {
    renderPanelPlaceholder("audit-policy-body");
    return Promise.resolve();
  }
  const sp = new URLSearchParams();
  sp.set("since", effectiveAuditWindow() || "24h");
  if (auditFilter.policyKind) sp.set("kind", auditFilter.policyKind);
  if (auditFilter.profile) sp.set("profile", auditFilter.profile);
  if (auditFilter.role) sp.set("agent", auditFilter.role);
  return fetchJson(`/api/diagnostics/denials?${sp.toString()}`).then((data) => {
    lastAuditPolicy = data;
    renderPolicy(data, ctx);
  });
}

function renderPolicy(data, ctx) {
  const body = $("audit-policy-body");
  if (!body) return;
  $("audit-count").textContent = `${data && data.total ? data.total : 0}`;

  if (!data || (data.total || 0) === 0) {
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: `No denials in the last ${effectiveAuditWindow() || "24h"}.` }),
    ])]);
    return;
  }

  const sections = [];
  const recent = buildRecentDenials(data.recent_denials || [], ctx);
  const causes = buildTopCauses(data.top_causes || [], ctx);
  if (recent) sections.push(recent);
  if (causes) sections.push(causes);

  const grid = el("div", { class: "policy-grid" });
  for (const tbl of POLICY_TABLES) {
    const cell = el("div", { class: "policy-cell" });
    cell.appendChild(el("h5", { text: tbl.label }));
    const rawRows = (data[tbl.id] || []).slice();
    const sortMode = policySort[tbl.id] || "count";
    rawRows.sort((a, b) => {
      if (sortMode === "name") {
        return String(a[tbl.nameField] || "").localeCompare(String(b[tbl.nameField] || ""));
      }
      return (b.count || 0) - (a.count || 0);
    });
    cell.appendChild(buildPolicyTable(tbl, rawRows, sortMode, ctx));
    grid.appendChild(cell);
  }
  sections.push(grid);
  syncNodes(body, sections);
}

function buildPolicyTable(spec, rows, sortMode, ctx) {
  const table = el("table", { class: "policy-table" });
  const thead = el("thead");
  const headRow = el("tr");
  const nameTh = el("th", { text: spec.header || spec.nameField });
  if (sortMode === "name") {
    const arrow = el("span", { class: "sort-arrow", text: "▼" });
    nameTh.appendChild(arrow);
  }
  nameTh.addEventListener("click", () => {
    policySort[spec.id] = "name";
    if (lastAuditPolicy) renderPolicy(lastAuditPolicy, ctx);
  });
  headRow.appendChild(nameTh);
  const countTh = el("th", { class: "num", text: "count" });
  if (sortMode === "count") {
    const arrow = el("span", { class: "sort-arrow", text: "▼" });
    countTh.appendChild(arrow);
  }
  countTh.addEventListener("click", () => {
    policySort[spec.id] = "count";
    if (lastAuditPolicy) renderPolicy(lastAuditPolicy, ctx);
  });
  headRow.appendChild(countTh);
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = el("tbody");
  if (rows.length === 0) {
    const tr = el("tr");
    const td = el("td", { class: "value-name", text: "—" });
    td.colSpan = 2;
    tr.appendChild(td);
    tbody.appendChild(tr);
  } else {
    for (const row of rows) {
      const name = String(row[spec.nameField] ?? "");
      const tr = el("tr", { title: name });
      tr.appendChild(el("td", { class: "value-name", text: name }));
      tr.appendChild(el("td", { class: "num", text: String(row.count || 0) }));
      if (spec.navigateTo === "job_run") {
        tr.classList.add("clickable");
        tr.addEventListener("click", () => doNavigateToRun(ctx, name));
      } else if (spec.navigateTo === "audit_execution") {
        tr.classList.add("clickable");
        tr.addEventListener("click", () => navigateToAuditExecution(name, ctx));
      } else if (spec.filterKey) {
        tr.classList.add("clickable");
        tr.addEventListener("click", () => {
          auditFilter[spec.filterKey] = name;
          activeAuditSubtab = "events";
          window.location.hash = buildAuditHash();
        });
      }
      tbody.appendChild(tr);
    }
  }
  table.appendChild(tbody);
  return table;
}

function buildTopCauses(rows, ctx) {
  if (!rows.length) return null;
  const section = el("div", { class: "policy-section" });
  section.appendChild(el("h5", { text: "Top Causes" }));
  const table = el("table", { class: "policy-table policy-cause-table" });
  const thead = el("thead");
  const headRow = el("tr");
  for (const label of ["cause", "target", "count", "latest"]) {
    headRow.appendChild(el("th", { class: label === "count" ? "num" : "", text: label }));
  }
  thead.appendChild(headRow);
  table.appendChild(thead);
  const tbody = el("tbody");
  for (const row of rows) {
    const tr = el("tr");
    tr.appendChild(el("td", {
      class: "value-name",
      text: row.cause || "-",
      title: row.cause || "",
    }));
    tr.appendChild(el("td", {
      class: "value-name muted",
      text: row.target || "-",
      title: row.target || "",
    }));
    tr.appendChild(el("td", { class: "num", text: String(row.count || 0) }));
    tr.appendChild(el("td", {
      class: "muted mono",
      text: row.latest_ts ? fmtRelativeValue(ctx, row.latest_ts) : "-",
    }));
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  section.appendChild(table);
  return section;
}

function buildRecentDenials(rows, ctx) {
  if (!rows.length) return null;
  const section = el("div", { class: "policy-section" });
  section.appendChild(el("h5", { text: "Recent Denials" }));
  const table = el("table", { class: "policy-table policy-recent-table" });
  const thead = el("thead");
  const headRow = el("tr");
  for (const label of ["time", "target", "cause", "identity", "details"]) {
    headRow.appendChild(el("th", { text: label }));
  }
  thead.appendChild(headRow);
  table.appendChild(thead);
  const tbody = el("tbody");
  for (const row of rows) {
    const tr = el("tr");
    tr.appendChild(el("td", {
      class: "muted mono",
      text: row.timestamp ? fmtRelativeValue(ctx, row.timestamp) : "-",
    }));
    tr.appendChild(el("td", {
      class: "value-name",
      text: row.target || "-",
      title: row.target || "",
    }));
    tr.appendChild(el("td", {
      class: "value-name",
      text: row.cause || row.denial_kind || "-",
      title: row.cause || "",
    }));
    const identity = el("td");
    identity.appendChild(buildPolicyIdentityAction(row, ctx));
    tr.appendChild(identity);
    const details = policyDetailText(row);
    tr.appendChild(el("td", { class: "policy-detail", text: details, title: details }));
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  section.appendChild(table);
  return section;
}

function buildPolicyIdentityAction(row, ctx) {
  const identityId = row.identity_id || row.job_run_id || row.execution_id || "";
  if (!identityId) return el("span", { class: "muted", text: "-" });
  const isJobRun = row.identity_type === "job_run" && row.job_run_id;
  const label = isJobRun ? "JobRun" : "Audit";
  const btn = el("button", {
    class: "policy-link",
    text: `${label} ${truncateValue(ctx, identityId, 18)}`,
    title: identityId,
  });
  btn.addEventListener("click", (event) => {
    event.stopPropagation();
    if (isJobRun) doNavigateToRun(ctx, identityId);
    else navigateToAuditExecution(identityId, ctx);
  });
  return btn;
}

function policyDetailText(row) {
  const parts = [];
  if (row.actor) parts.push(`actor ${row.actor}`);
  const taskIds = row.requested_task_ids || [];
  if (taskIds.length) parts.push(`tasks ${taskIds.join(", ")}`);
  const files = row.requested_files || [];
  if (files.length) {
    const suffix = files.length > 2 ? " +" + (files.length - 2) : "";
    parts.push(`files ${files.slice(0, 2).join(", ")}${suffix}`);
  }
  const conflicts = row.conflicts || [];
  if (conflicts.length) {
    const first = conflicts[0] || {};
    const holder = [first.held_by, first.held_by_id].filter(Boolean).join(" ");
    parts.push(holder ? `held by ${holder}` : `${conflicts.length} conflicts`);
  }
  return parts.join(" · ") || row.denial_kind || "-";
}

function effectiveAuditWindow() {
  if (auditFilter.since) return auditFilter.since;
  return getWindow() === "all" ? null : getWindow();
}

function emptyAuditFilter() {
  return {
    status: null,
    q: "",
    tool: null,
    role: null,
    execution_id: null,
    profile: null,
    since: getWindow() === "all" ? null : getWindow(),
    metric: null,
    policyKind: null,
  };
}

function navigateToAuditExecution(executionId, ctx) {
  auditFilter = emptyAuditFilter();
  auditFilter.execution_id = executionId;
  activeAuditSubtab = "events";
  syncAuditControls();
  window.location.hash = buildAuditHash();
}

/// Navigates to the Audit tab pre-filtered by `role` (audit `role` ≈ scoreboard
/// agent name). Clears unrelated filters so the landing page is the role view.
function navigateToRole(role, ctx) {
  navigateToDrilldown({ role }, ctx);
}

/// Scoreboard actor/metric drill-down. Carries the shared dashboard window
/// and records the source metric so the landing chips explain the scope.
function navigateToDrilldown(opts = {}, ctx) {
  auditFilter = emptyAuditFilter();
  auditFilter.role = opts.role || null;
  auditFilter.metric = opts.metric || null;
  auditFilter.status = opts.status || null;
  // ORB-10871: an incident names one surface, so its "open raw events" link
  // lands on exactly the rows the incident collapsed rather than every failure
  // by that actor.
  auditFilter.tool = opts.tool || null;
  if (opts.window) auditFilter.since = opts.window === "all" ? null : opts.window;
  activeAuditSubtab = "events";
  syncAuditControls();
  window.location.hash = buildAuditHash();
}

function buildAuditChips(ctx) {
  const container = $("audit-filter");
  if (!container) return;
  container.innerHTML = "";
  const allChip = el("button", { class: "chip", text: "all" });
  allChip.addEventListener("click", () => {
    auditFilter.status = null;
    syncAuditControls();
    doSetActiveTab(ctx, "audit" + buildAuditHash().slice(6), { refresh: true });
  });
  container.appendChild(allChip);
  for (const status of AUDIT_STATUSES) {
    const chip = el("button", { class: "chip", text: status });
    chip.dataset.status = status;
    chip.style.borderLeft = `2px solid var(--audit-status-${status}, var(--border))`;
    chip.addEventListener("click", () => {
      auditFilter.status = auditFilter.status === status ? null : status;
      syncAuditControls();
      const hash = buildAuditHash();
      window.location.hash = hash;
    });
    container.appendChild(chip);
  }
  syncAuditControls();
}

function wireAuditSearch(ctx) {
  const input = $("audit-search");
  if (!input) return;
  let debounce = null;
  input.addEventListener("input", (e) => {
    auditFilter.q = e.target.value.trim();
    if (debounce) clearTimeout(debounce);
    debounce = setTimeout(() => {
      const hash = buildAuditHash();
      if (window.location.hash !== hash) {
        window.location.hash = hash;
      } else {
        doRefresh(ctx);
      }
    }, 250);
  });
}

function renderAudit(events, ctx) {
  const body = $("audit-body");
  if (!body) return;
  $("audit-count").textContent = `${events.length}`;

  if (events.length === 0) {
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: "No audit events match the current filter." }),
    ])]);
    return;
  }

  let table = body.querySelector("table.scoreboard-table");
  let tbody;
  if (!table) {
    table = el("table", { class: "scoreboard-table" });
    const thead = el("thead");
    const headRow = el("tr");
    for (const col of AUDIT_COLUMNS) {
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
  for (const ev of events) {
    const exit = ev.exit_code;
    const exitClass = exit != null && exit !== 0 ? "num exit-fail" : "num";
    const tool = ev.tool_name || "-";
    const target = ev.target_id || ev.target_type || "-";
    const cmd = ev.subcommand ? `${ev.command} ${ev.subcommand}` : ev.command;
    const tr = el("tr", { class: "audit-row", title: `event ${ev.id}` });
    tr.dataset.key = `audit-${ev.id}`;
    // Expansion is part of the row's identity: without it the keyed diff reuses
    // the collapsed node and drops the `expanded` class and `aria-expanded`
    // the toggle just set.
    tr.dataset.hash = `${ev.id}-${ev.status}-${exit}-${expandedAuditIds.has(ev.id)}`;
    tr.appendChild(el("td", { text: fmtTimestampValue(ctx, ev.timestamp) }));
    tr.appendChild(el("td", { text: ev.role || "-" }));
    tr.appendChild(el("td", { text: tool }));
    tr.appendChild(el("td", { text: cmd }));
    tr.appendChild(el("td", { text: target, title: target }));
    const statusTd = el("td");
    statusTd.appendChild(el("span", { class: `audit-status ${ev.status}`, text: ev.status }));
    tr.appendChild(statusTd);
    tr.appendChild(el("td", { class: exitClass, text: exit == null ? "-" : String(exit) }));
    tr.appendChild(el("td", { class: "num", text: fmtDurationValue(ctx, ev.duration_ms) }));
    if (expandedAuditIds.has(ev.id)) tr.classList.add("expanded");
    makeToggleRow(tr, {
      expanded: expandedAuditIds.has(ev.id),
      // The detail row only exists while the event is open, so the IDREF is
      // only published while it actually resolves.
      controls: expandedAuditIds.has(ev.id) ? `audit-detail-${ev.id}` : null,
      onToggle: () => {
        if (expandedAuditIds.has(ev.id)) expandedAuditIds.delete(ev.id);
        else expandedAuditIds.add(ev.id);
        renderAudit(lastAudit, ctx);
      },
    });
    frag.appendChild(tr);

    if (expandedAuditIds.has(ev.id)) {
      frag.appendChild(buildAuditDetailRow(ev, ctx));
    }
  }
  syncNodes(tbody, Array.from(frag.children));
}

function buildAuditDetailRow(ev, ctx) {
  const tr = el("tr", { class: "audit-detail-row" });
  tr.dataset.key = `audit-detail-${ev.id}`;
  // The event row's `aria-controls` points here, so the detail needs a real id.
  tr.id = `audit-detail-${ev.id}`;
  tr.dataset.hash = JSON.stringify(ev);
  const td = el("td");
  td.colSpan = AUDIT_COLUMNS.length;
  td.addEventListener("click", (e) => e.stopPropagation());

  const meta = el("div", { class: "audit-detail-meta" });
  const addMeta = (label, value) => {
    if (value == null || value === "") return;
    meta.appendChild(el("span", {}, [
      el("span", { class: "label", text: `${label}:` }),
      el("span", { class: "value", text: String(value) }),
    ]));
  };
  const addMetaLink = (label, value, href) => {
    if (value == null || value === "") return;
    const link = el("a", { class: "value audit-detail-link", text: String(value) });
    link.href = href;
    meta.appendChild(el("span", {}, [
      el("span", { class: "label", text: `${label}:` }),
      link,
    ]));
  };
  addMeta("execution_id", ev.execution_id);
  addMeta("session_id", ev.session_id);
  addMeta("task_id", ev.task_id || "-");
  if (ev.job_run_id) {
    addMetaLink(
      "job_run_id",
      ev.job_run_id,
      `#runs/${encodeURIComponent(ev.job_run_id)}`,
    );
  } else {
    addMeta("job_run_id", "-");
  }
  addMeta("activity_id", ev.activity_id || "-");
  if (ev.step_index != null) {
    addMeta("step_index", ev.step_index);
  }
  addMeta("host", ev.host);
  addMeta("pid", ev.pid);
  addMeta("cwd", ev.working_directory);
  addMeta("timestamp", fmtAbsTimeValue(ctx, ev.timestamp));
  td.appendChild(meta);

  if (ev.arguments_json) {
    const block = el("div", { class: "audit-detail-block" });
    block.appendChild(el("div", { class: "label", text: "arguments" }));
    let pretty = ev.arguments_json;
    try {
      pretty = JSON.stringify(JSON.parse(ev.arguments_json), null, 2);
    } catch (_) {
      /* leave raw */
    }
    block.appendChild(el("pre", { text: pretty }));
    td.appendChild(block);
  }
  if (ev.stderr_truncated) {
    const block = el("div", { class: "audit-detail-block" });
    block.appendChild(el("div", { class: "label", text: "stderr (truncated)" }));
    block.appendChild(el("pre", { text: ev.stderr_truncated }));
    td.appendChild(block);
  }
  if (ev.stdout_truncated) {
    const block = el("div", { class: "audit-detail-block" });
    block.appendChild(el("div", { class: "label", text: "stdout (truncated)" }));
    block.appendChild(el("pre", { text: ev.stdout_truncated }));
    td.appendChild(block);
  }
  if (ev.error_message) {
    const block = el("div", { class: "audit-detail-block" });
    block.appendChild(el("div", { class: "label", text: "error" }));
    block.appendChild(el("pre", { text: ev.error_message }));
    td.appendChild(block);
  }

  tr.appendChild(td);
  return tr;
}

export {
  // hash/subtab/control sync
  buildAuditHash,
  setAuditSubtab,
  syncAuditControls,
  // refresh entry points
  fetchAndRenderAudit,
  fetchAndRenderPolicy,
  renderAuditSummary,
  // module init
  buildAuditChips,
  wireAuditSearch,
  // cross-domain navigation
  navigateToAuditExecution,
  navigateToRole,
  navigateToDrilldown,
  // state inspection used by setActiveTab + activeRefreshJobs
  getActiveAuditSubtab,
  setActiveAuditSubtabFromButton,
  // hash → state import used by setActiveTab
  applyAuditHashQuery,
};
