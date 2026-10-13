// Orbit dashboard audit-domain rendering and actions.
// Pure vanilla JS, split into ES modules with no build step.

import { auditActorLabel, incidentClassLabel, el, fetchJson, fetchJsonPage, syncNodes, makeToggleRow, positiveIntParam, isAggregateView, isMultiWorkspace, renderPanelPlaceholder, requestPanel, onWorkspaceChange, getWindow, setWindow, getWorkspace, setWorkspace, persistScopeToUrl, runHref, DEFAULT_DASHBOARD_WINDOW } from './common.js';
import { applyCodebaseHashQuery, codebaseHashParams } from './codebase.js';

const $ = (id) => document.getElementById(id);

const AUDIT_LIMIT = positiveIntParam("audit", 50);
const INCIDENT_ID_BATCH_SIZE = 500;
const AUDIT_STATUSES = ["success", "failure", "denied", "non_success"];
const AUDIT_SUBTABS = ["events", "codebase"];
// Server header naming the `before` cursor that resumes the listing; absent
// once the window is exhausted.
const AUDIT_NEXT_BEFORE_HEADER = "x-audit-next-before";

// Audit tab state (moved from app.js)
let lastAudit = [];
// Paging over `lastAudit`: the head page is re-fetched on every refresh, older
// pages stay until the scope (filters, window, workspace) changes.
let auditPaging = freshAuditPaging();
// `events` of /api/audit/summary for `window`: the size of the selected window.
let auditWindowTotal = null;
let auditFilter = {
  status: null,
  q: "",
  tool: null,
  role: null,
  agent_family: null,
  // Filters audit Events by `execution_id` (the orbit invocation id). The CLI
  // SQLite audit table has no real `run_id` field, so this never identifies a
  // JobRun — see T20260427-26.
  execution_id: null,
  profile: null,
  // Exact persisted row IDs from an incident evidence drilldown.
  eventIds: [],
  // Time-window filter for the Events sub-tab. Accepts the same shorthands as
  // the API (`24h`, `7d`, `1w`, RFC3339); null means the API-side default.
  since: null,
  // Source metric from a drill-down (display + hash only).
  metric: null,
  // Hides successful calls from the unconfirmed caller (probe traffic).
  hide_unverified: false,
};
let expandedAuditIds = new Set();
let activeAuditSubtab = "events";
const AUDIT_COLUMNS = [
  { key: "time", label: "time" },
  { key: "status", label: "status" },
  { key: "role", label: "actor", title: "Recorded role: unverified = MCP caller without a trusted managed identity; unknown = unattributed CLI caller; agent names identify the recorded process; admin = operator; hook = hook process." },
  { key: "command", label: "tool / command" },
  { key: "target", label: "target" },
  { key: "duration", label: "duration", num: true },
  { key: "exit", label: "exit", num: true },
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

function fmtAbsTimeValue(ctx, v) {
  return hasCtx(ctx, "fmtAbsTime") ? ctx.fmtAbsTime(v) : (v || "-");
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

function buildAuditHash() {
  const sp = new URLSearchParams();
  // The Events and Codebase sub-tabs serialize different axes; project only
  // the active sub-tab's so the hash stays self-describing and reload-safe.
  if (activeAuditSubtab === "codebase") {
    codebaseHashParams(sp);
  } else if (auditFilter.eventIds.length > 0) {
    sp.set("ids", auditFilter.eventIds.join(","));
  } else {
    if (auditFilter.since) sp.set("since", auditFilter.since);
    if (auditFilter.status) sp.set("status", auditFilter.status);
    if (auditFilter.tool) sp.set("tool", auditFilter.tool);
    if (auditFilter.role) sp.set("role", auditFilter.role);
    if (auditFilter.agent_family) sp.set("agent_family", auditFilter.agent_family);
    if (auditFilter.execution_id) sp.set("execution_id", auditFilter.execution_id);
    if (auditFilter.profile) sp.set("profile", auditFilter.profile);
    if (auditFilter.q) sp.set("q", auditFilter.q);
    if (auditFilter.hide_unverified) sp.set("hide_unverified", "1");
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
  const codebaseCtl = $("audit-codebase-controls");
  const codebaseBody = $("audit-codebase-body");
  if (eventsCtl) eventsCtl.style.display = name === "events" ? "" : "none";
  if (eventsBody) eventsBody.style.display = name === "events" ? "" : "none";
  if (codebaseCtl) codebaseCtl.style.display = name === "codebase" ? "" : "none";
  if (codebaseBody) codebaseBody.style.display = name === "codebase" ? "" : "none";
  // The audit summary describes events, not the codebase, so Codebase drops
  // that column and takes the full width.
  const layout = $("audit-main");
  if (layout) layout.classList.toggle("audit-full", name === "codebase");
  const summary = $("audit-summary-column");
  if (summary) summary.style.display = name === "codebase" ? "none" : "";
  // The audit count is the Events listing's; Codebase states its own totals.
  const count = $("audit-count");
  if (count) count.style.display = name === "events" ? "" : "none";
  const title = $("audit-title");
  if (title) title.textContent = name === "codebase" ? "Codebase" : "Audit events";
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
  const hideChip = Array.from($("audit-filter")?.children || []).find(chip => chip.dataset.toggle === "hide_unverified");
  if (hideChip) {
    hideChip.classList.toggle("active", auditFilter.hide_unverified);
    hideChip.setAttribute("aria-pressed", String(auditFilter.hide_unverified));
  }
  renderScopeChips();
}

function scopeChip(label, value, onRemove) {
  const chip = el(onRemove ? "button" : "span", {
    class: "scope-chip",
    title: onRemove ? `Remove ${label} filter` : `${label}: ${value}`,
  });
  if (onRemove) chip.type = "button";
  chip.dataset.chip = label;
  chip.appendChild(el("span", { class: "scope-chip-k", text: label }));
  chip.appendChild(el("span", { class: "scope-chip-v", text: value }));
  if (onRemove) {
    chip.appendChild(el("span", { class: "scope-chip-x", text: "×" }));
    chip.addEventListener("click", (event) => {
      event.preventDefault();
      onRemove();
    });
  }
  return chip;
}

function renderScopeChips() {
  const host = $("audit-scope-chips");
  if (!host) return;
  host.innerHTML = "";
  const workspace = getWorkspace();
  if (workspace) {
    // Single-workspace mode has no picker to restore a cleared action scope.
    host.appendChild(scopeChip("workspace", workspace, isMultiWorkspace() ? () => {
      setWorkspace(null);
      persistScopeToUrl();
      const select = $("workspace-select");
      if (select) select.value = "";
      window.location.hash = buildAuditHash();
    } : null));
  }
  const windowLabel = effectiveAuditWindow();
  if (windowLabel) {
    const chip = scopeChip("window", windowLabel, () => {
      auditFilter.since = null;
      setWindow(DEFAULT_DASHBOARD_WINDOW);
      persistScopeToUrl();
      window.location.hash = buildAuditHash();
    });
    if (windowLabel !== getWindow()) chip.classList.add("independent");
    host.appendChild(chip);
  }
  if (auditFilter.role) {
    const chip = scopeChip("actor", auditActorLabel(auditFilter.role), () => {
      auditFilter.role = null;
      window.location.hash = buildAuditHash();
    });
    chip.title = `Recorded role: ${auditFilter.role}`;
    host.appendChild(chip);
  }
  if (auditFilter.agent_family) {
    host.appendChild(scopeChip("agent family", auditFilter.agent_family, () => {
      auditFilter.agent_family = null;
      window.location.hash = buildAuditHash();
    }));
  }
  if (auditFilter.status) {
    host.appendChild(scopeChip("status", auditFilter.status === "non_success" ? "failure + denied" : auditFilter.status, () => {
      auditFilter.status = null;
      window.location.hash = buildAuditHash();
    }));
  }
  if (auditFilter.hide_unverified) {
    host.appendChild(scopeChip("hiding", "unconfirmed-caller successes", () => {
      auditFilter.hide_unverified = false;
      window.location.hash = buildAuditHash();
    }));
  }
  if (auditFilter.eventIds.length > 0) {
    host.appendChild(scopeChip("incident events", `${auditFilter.eventIds.length} rows`, () => {
      auditFilter.eventIds = [];
      window.location.hash = buildAuditHash();
    }));
  }
  if (auditFilter.metric) {
    host.appendChild(scopeChip("metric", auditFilter.metric, () => {
      auditFilter.metric = null;
      window.location.hash = buildAuditHash();
    }));
  }
}

function applyAuditHashQuery(query, subtab = "events") {
  // Mutates auditFilter from URLSearchParams (hash query). Called from setActiveTab.
  // Legacy run_id alias for execution_id is preserved for old deep links.
  // A Codebase hash carries only that view's selection, so it leaves the
  // Events filter as it was.
  if (subtab === "codebase") {
    applyCodebaseHashQuery(query);
    return;
  }
  auditFilter.status = query.get("status") || null;
  auditFilter.tool = query.get("tool") || null;
  auditFilter.role = query.get("role") || null;
  auditFilter.agent_family = query.get("agent_family") || null;
  auditFilter.execution_id =
    query.get("execution_id") || query.get("run_id") || null;
  auditFilter.profile = query.get("profile") || null;
  auditFilter.eventIds = (query.get("ids") || "")
    .split(",")
    .map(value => Number(value))
    .filter(value => Number.isSafeInteger(value) && value > 0);
  auditFilter.q = query.get("q") || "";
  auditFilter.since = query.get("since") || (getWindow() === "all" ? null : getWindow());
  auditFilter.metric = query.get("metric") || null;
  auditFilter.hide_unverified = query.get("hide_unverified") === "1";
}

function getActiveAuditSubtab() {
  return activeAuditSubtab;
}

function setActiveAuditSubtabFromButton(name) {
  if (!AUDIT_SUBTABS.includes(name)) name = "events";
  activeAuditSubtab = name;
  setAuditSubtab(name);
}

function placeholdAuditAggregate() {
  renderPanelPlaceholder("audit-body");
  renderPanelPlaceholder("audit-codebase-body");
  lastAudit = [];
  auditPaging = freshAuditPaging();
  const count = $("audit-count");
  if (count) count.textContent = "—";
}

function fetchAndRenderAudit(ctx) {
  // ORB-00040: /api/audit is per-workspace and 400s without a concrete
  // workspace. In the aggregate ("All workspaces") view render the placeholder
  // and skip the fetch — covers both the auto-refresh and audit-search paths.
  if (isAggregateView()) {
    placeholdAuditAggregate();
    return Promise.resolve();
  }
  const eventIds = auditFilter.eventIds.slice();
  const sp = new URLSearchParams();
  sp.set("limit", String(AUDIT_LIMIT));
  if (auditFilter.eventIds.length > 0) {
    sp.set("ids", auditFilter.eventIds.join(","));
  } else {
    const since = effectiveAuditWindow();
    if (since) sp.set("since", since);
    if (auditFilter.status) sp.set("status", auditFilter.status);
    if (auditFilter.tool) sp.set("tool", auditFilter.tool);
    if (auditFilter.role) sp.set("role", auditFilter.role);
    if (auditFilter.agent_family) sp.set("agent_family", auditFilter.agent_family);
    if (auditFilter.execution_id) sp.set("execution_id", auditFilter.execution_id);
    if (auditFilter.profile) sp.set("profile", auditFilter.profile);
    if (auditFilter.q) sp.set("q", auditFilter.q);
    if (auditFilter.hide_unverified) sp.set("hide_unverified", "true");
  }
  const path = `/api/audit?${sp.toString()}`;
  const requestEvents = eventIds.length > 0
    ? async () => {
      const events = [];
      for (let start = 0; start < eventIds.length; start += INCIDENT_ID_BATCH_SIZE) {
        const batch = new URLSearchParams();
        batch.set("limit", String(AUDIT_LIMIT));
        batch.set("ids", eventIds.slice(start, start + INCIDENT_ID_BATCH_SIZE).join(","));
        events.push(...await fetchJson(`/api/audit?${batch.toString()}`));
      }
      // Each batch is newest-first; restore that order across batches.
      return { events: events.sort((a, b) => b.id - a.id), nextBefore: null };
    }
    : async () => {
      const page = await fetchJsonPage(path);
      return { events: page.body, nextBefore: page.header(AUDIT_NEXT_BEFORE_HEADER) };
    };
  // A head for another scope retires the previous paging now, not when it
  // settles: an older page still in flight for the old scope must not paint
  // into the reset body, even if this head fails. A same-scope refresh keeps
  // its paging so mergeAuditHead can keep the loaded tail.
  if (auditPaging.path !== path) auditPaging = freshAuditPaging();
  // A slower search or the previous workspace must not paint over the visit
  // now on screen, and must not become the snapshot row expansion re-renders.
  return requestPanel("audit-body", path, requestEvents, (head) => {
    mergeAuditHead(path, head, eventIds.length === 0);
    renderAudit(lastAudit, ctx);
  }, "audit-count");
}

function freshAuditPaging() {
  // `path` is the head request this paging belongs to; `epoch` changes
  // whenever older pages are dropped, so a late "load older" can tell its
  // cursor no longer continues what is on screen.
  return { path: null, nextBefore: null, loading: false, error: null, epoch: 0 };
}

// Fold a freshly fetched newest page into the rows already on screen. Older
// pages survive a refresh only when the new head reaches back to them: a head
// that ends above the newest loaded row may have skipped events in between.
function mergeAuditHead(path, head, pageable) {
  const sameScope = auditPaging.path === path;
  const prior = sameScope ? lastAudit : [];
  let rows = head.events;
  let nextBefore = head.nextBefore;
  let keptTail = false;
  if (pageable && sameScope && prior.length > rows.length && rows.length > 0) {
    const headOldest = rows[rows.length - 1].id;
    // A short head only proves it reaches the tail when the server reports the
    // window exhausted (no cursor). A capped short head resumes below its oldest
    // row, so it reaches the tail only by overlapping it.
    const exhausted = rows.length < AUDIT_LIMIT && !head.nextBefore;
    if (exhausted || headOldest <= prior[0].id) {
      const tail = prior.filter(ev => ev.id < headOldest);
      if (tail.length > 0) {
        rows = rows.concat(tail);
        nextBefore = auditPaging.nextBefore;
        keptTail = true;
      }
    }
  }
  const droppedOlder = prior.length > head.events.length && !keptTail;
  if (!sameScope) {
    auditPaging = freshAuditPaging();
  } else if (droppedOlder) {
    auditPaging.epoch += 1;
    auditPaging.loading = false;
  }
  auditPaging.path = pageable ? path : null;
  auditPaging.nextBefore = pageable ? nextBefore : null;
  auditPaging.error = null;
  lastAudit = rows;
}

async function loadOlderAudit(ctx) {
  const state = auditPaging;
  const cursor = state.nextBefore;
  if (state.loading || !cursor || !state.path) return;
  const epoch = state.epoch;
  const stale = () => auditPaging !== state || state.epoch !== epoch;
  state.loading = true;
  state.error = null;
  renderAudit(lastAudit, ctx);
  try {
    const page = await fetchJsonPage(`${state.path}&before=${encodeURIComponent(cursor)}`);
    if (stale()) return;
    const seen = new Set(lastAudit.map(ev => ev.id));
    lastAudit = lastAudit.concat(page.body.filter(ev => !seen.has(ev.id)));
    state.nextBefore = page.header(AUDIT_NEXT_BEFORE_HEADER) || null;
  } catch (error) {
    if (stale()) return;
    state.error = error.message;
  }
  state.loading = false;
  renderAudit(lastAudit, ctx);
}

// The header count: rows on screen against the size of the selected window.
// The window total is the summary's unfiltered event count, so it is only
// comparable when the list and the summary cover the same window.
function auditCountLabel() {
  const shown = lastAudit.length;
  if (auditFilter.eventIds.length > 0) return `${shown}`;
  const window = effectiveAuditWindow();
  const total = window && auditWindowTotal && auditWindowTotal.window === window
    ? Math.max(auditWindowTotal.events, shown)
    : null;
  const hasMore = Boolean(auditPaging.nextBefore);
  const narrowed = Boolean(auditFilter.status || auditFilter.tool || auditFilter.role
    || auditFilter.agent_family || auditFilter.execution_id || auditFilter.profile
    || auditFilter.q || auditFilter.hide_unverified);
  if (total == null) return hasMore ? `${shown.toLocaleString()}+` : `${shown.toLocaleString()}`;
  if (!narrowed) return `${shown.toLocaleString()} of ${total.toLocaleString()} in ${window}`;
  return `${shown.toLocaleString()} ${hasMore ? "shown" : "matching"} · ${total.toLocaleString()} in ${window}`;
}

function updateAuditCount() {
  const count = $("audit-count");
  if (count && activeAuditSubtab === "events" && auditPaging.path) count.textContent = auditCountLabel();
}

function buildAuditFooter(ctx) {
  if (!auditPaging.path) return null;
  const { nextBefore, loading, error } = auditPaging;
  const footer = el("div", { class: "audit-more" });
  footer.dataset.key = "audit-more";
  footer.dataset.hash = JSON.stringify([Boolean(nextBefore), loading, error]);
  if (error) {
    footer.appendChild(el("span", { class: "audit-more-error", text: `Could not load older events: ${error}` }));
  }
  if (nextBefore) {
    const button = el("button", {
      class: "audit-more-btn",
      type: "button",
      text: loading ? "Loading older events…" : "Load older events",
    });
    button.disabled = loading;
    button.addEventListener("click", () => { void loadOlderAudit(ctx); });
    footer.appendChild(button);
  } else if (!error) {
    footer.appendChild(el("span", { class: "audit-more-end", text: "No older events in the selected window." }));
  }
  return footer;
}

function isNamedTool(name) {
  const trimmed = String(name || "").trim();
  return trimmed.length > 0 && trimmed !== "unknown";
}

function formatFailureRatePct(rate) {
  return `${((Number(rate) || 0) * 100).toFixed(1)}%`;
}

function incidentScanPartialCoverageNote(scanLimit) {
  const limit = Number(scanLimit) || 0;
  const scannedRows = limit > 0
    ? `the newest ${limit.toLocaleString()} non-success audit rows`
    : "the capped non-success audit sample";
  return `Partial coverage: counts include only ${scannedRows} in this window. Older failures and affected runs may be omitted.`;
}

function renderToolCallFailureRateCard(stats, window = "24h") {
  const failed = Number(stats && stats.failed) || 0;
  const total = Number(stats && stats.total) || 0;
  const rate = stats && stats.rate != null ? Number(stats.rate) : (total ? failed / total : 0);
  const card = el("div", { class: "audit-summary-card" });
  card.appendChild(el("div", {
    class: "card-title section-title",
    text: `Tool call failure rate · window ${window}`,
  }));
  const body = el("div", { class: "card-body" });
  body.appendChild(el("div", {
    class: "tool-call-failure-rate",
    text: `${formatFailureRatePct(rate)} · ${failed} failed / ${total} tool calls`,
  }));
  body.appendChild(el("div", {
    class: "metric-trend tool-call-failure-rate-note",
    text: `Raw status=failure over successful + failed callable tool calls (tool run + tool run-mcp). ${Number(stats && stats.denied) || 0} denied calls excluded. Distinct from unexpected failure rate.`,
  }));
  card.appendChild(body);
  return card;
}

function renderFailuresByToolCard(rateRows, failuresRows, onCardClick, window = "24h", capped = false, scanLimit = 0) {
  const failByTool = new Map();
  for (const f of failuresRows) {
    if (isNamedTool(f.tool)) failByTool.set(f.tool, f);
  }

  const container = el("div", { class: "audit-summary-container" });
  container.appendChild(el("h3", {
    class: "summary-title section-title",
    text: `Unexpected failures by callable tool (${window})${capped ? " · capped counts" : ""}`,
  }));
  if (capped) {
    container.appendChild(el("div", {
      class: "metric-trend",
      text: `${incidentScanPartialCoverageNote(scanLimit)} Unexpected-failure counts are scanned, while successful-call counts cover the full window; rates can be understated.`,
    }));
  }
  const grid = el("div", { class: "tool-health-grid" });

  for (const row of rateRows) {
    if (!isNamedTool(row.tool)) continue;
    const rate = row.rate || 0;
    const severity = rate >= 0.14 ? "critical" : rate >= 0.05 ? "warning" : "normal";
    const card = el("div", { class: `health-card ${severity}` });

    const header = el("div", { class: "card-header" });
    header.appendChild(el("span", { class: "tool-name", text: row.tool, title: row.tool }));
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

// The canonical denial count of /api/audit/summary (`denials`: denied audit
// rows plus v2 loop denials) rides on the rail beside Audit. Its route opens
// Audit › Events filtered to denials for the window it counted.
function renderDenialCount(data) {
  const node = $("rail-count-audit-denials");
  if (!node) return;
  const denials = Number(data && data.denials) || 0;
  const window = (data && data.window) || "24h";
  node.hidden = denials === 0;
  node.textContent = denials ? `${denials >= 10000 ? `${(denials / 1000).toFixed(1)}k` : denials} denied` : "";
  if (!denials) {
    node.title = "";
    delete node.dataset.route;
    return;
  }
  const sql = Number(data.denials_sql) || 0;
  const v2 = Number(data.denials_v2) || 0;
  node.title = `${denials} policy denials in the ${window} window (${sql} audit rows, ${v2} loop denials). Click to open Audit › Events filtered to denials.`;
  const sp = new URLSearchParams();
  if (window !== "all") sp.set("since", window);
  sp.set("status", "denied");
  node.dataset.route = `audit/events?${sp.toString()}`;
}

function renderAuditSummary(data, ctx) {
  const events = Number(data && data.events);
  auditWindowTotal = Number.isFinite(events) && data.window ? { events, window: data.window } : null;
  updateAuditCount();
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

  const createCard = (title, renderBody, scrollableBody = false) => {
    const card = el("div", { class: "audit-summary-card" });
    card.appendChild(el("div", { class: "card-title section-title", text: title }));
    const body = el("div", { class: "card-body" });
    if (scrollableBody) {
      body.tabIndex = 0;
      body.setAttribute("role", "region");
      body.setAttribute("aria-label", title);
    }
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
      for (const c of cols) {
        const th = el("th", { class: `${c.num ? "num" : ""} ${c.secondary ? "summary-secondary" : ""}`, text: c.label, title: c.title });
        th.dataset.column = c.key;
        tr.appendChild(th);
      }
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
          const title = c.num ? String(val ?? "") : cols.map(col => {
            const value = col.format ? col.format(item[col.key]) : item[col.key];
            return `${col.title || col.label}: ${value ?? "-"}`;
          }).join("; ");
          const td = el("td", { class: `${c.num ? "num" : ""} ${c.secondary ? "summary-secondary" : ""}`, text: val, title });
          if (c.render) td.textContent = c.render(item[c.key], item, td);
          td.dataset.column = c.key;
          row.appendChild(td);
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

  const windowLabel = data.window || "24h";
  const title = $("audit-summary-title");
  if (title) title.textContent = `Audit summary ${windowLabel}`;

  if (data.tool_call_failure_rate) {
    addCard(
      "tool-call-failure-rate",
      renderToolCallFailureRateCard(data.tool_call_failure_rate, windowLabel),
      [data.tool_call_failure_rate, windowLabel],
    );
  }

  const namedToolFailures = (data.tool_call_failures_by_tool || []).filter((row) => isNamedTool(row.tool));
  if (namedToolFailures.length) {
    addCard("tool-call-failures-by-tool", createCard(
      `Tool call failures by tool · window ${windowLabel}`,
      renderTable(namedToolFailures, [
        { key: "tool", label: "tool" },
        { key: "failed", label: "failed", num: true },
        { key: "total", label: "total", num: true, secondary: true, title: "Successful + failed calls; denied calls excluded" },
        { key: "rate", label: "rate", num: true, format: (v) => formatFailureRatePct(v) },
        { key: "unexpected", label: "unexp.", num: true, secondary: true, title: "Failed calls classified as unexpected" },
        { key: "denied", label: "denied", num: true, title: "Calls recorded as denied; excluded from total and rate" },
      ], filterByTool),
      true,
    ), [namedToolFailures, windowLabel]);
  }

  const namedRates = (data.failure_rate_by_tool || []).filter((row) => isNamedTool(row.tool));
  const namedFailures = (data.failures_by_tool || []).filter((row) => isNamedTool(row.tool));
  const incidentScanCapped = data.failure_incidents_truncated === true;
  const incidentScanLimit = Number(data.failure_incidents_scan_limit) || 0;
  if (namedRates.length) {
    addCard("failures-by-tool", renderFailuresByToolCard(
      namedRates,
      namedFailures,
      filterByTool,
      windowLabel,
      incidentScanCapped,
      incidentScanLimit,
    ), [namedRates, namedFailures, windowLabel, incidentScanCapped, incidentScanLimit]);
  }

  const categoryOrder = ["unexpected", "expected", "denied", "diagnostic", "deferred"];
  const categories = data.failure_categories || {};
  const categoryRows = categoryOrder.map((key) => ({
    key,
    label: (categories[key] && categories[key].label) || key,
    incidents: Number(categories[key] && categories[key].incidents) || 0,
    raw_events: Number(categories[key] && categories[key].raw_events) || 0,
    affected_runs: Number(categories[key] && categories[key].affected_runs) || 0,
  }));
  if (categoryRows.some((row) => row.incidents || row.raw_events)) {
    const capped = incidentScanCapped;
    const scanLimit = incidentScanLimit;
    const card = createCard(
      `Failure categories · window ${data.window || "24h"}${capped ? " · capped counts" : ""}`,
      renderTable(categoryRows, [
        { key: "label", label: "class", title: "Failure classification", render: (value, row, td) => {
          td.title = value;
          return incidentClassLabel(row.key, value);
        } },
        { key: "incidents", label: "inc.", num: true, title: "Incidents" },
        { key: "raw_events", label: "events", num: true, title: "Raw events" },
        { key: "affected_runs", label: "runs", num: true, title: "Affected runs" },
      ]),
    );
    if (capped) {
      card.appendChild(el("div", {
        class: "metric-trend",
        text: incidentScanPartialCoverageNote(scanLimit),
      }));
    }
    addCard("failure-categories", card, [categoryRows, data.window || "24h", capped, scanLimit]);
  }

  const lifecycleFailures = Number(data.lifecycle_diagnostic_events) || 0;
  const lifecycleIncidents = Number(data.lifecycle_diagnostic_incidents) || 0;
  if (lifecycleFailures > 0 || lifecycleIncidents > 0) {
    const label = data.lifecycle_diagnostic_label || "lifecycle diagnostics";
    const window = data.window || "24h";
    const lifecycleTitle = `${label}${incidentScanCapped ? " · capped counts" : ""}`;
    const lifecycleCard = el("div", { class: "audit-summary-card lifecycle-failure-card" });
    lifecycleCard.appendChild(el("div", { class: "card-title section-title", text: lifecycleTitle }));
    const body = el("div", { class: "card-body" });
    body.appendChild(el("div", {
      class: "lifecycle-failure-counts",
      text: `${lifecycleIncidents} incidents · ${lifecycleFailures} raw events · ${Number(data.lifecycle_diagnostic_affected_run_count) || 0} affected runs`,
    }));
    body.appendChild(el("div", {
      class: "metric-trend",
      text: `Failure-only diagnostic surfaces; excluded from callable-tool denominators and rates · window ${window}`,
    }));
    if (incidentScanCapped) {
      body.appendChild(el("div", {
        class: "metric-trend",
        text: incidentScanPartialCoverageNote(incidentScanLimit),
      }));
    }
    lifecycleCard.appendChild(body);
    addCard("lifecycle-diagnostics", lifecycleCard, [
      label,
      window,
      lifecycleIncidents,
      lifecycleFailures,
      Number(data.lifecycle_diagnostic_affected_run_count) || 0,
      incidentScanCapped,
      incidentScanLimit,
    ]);
  }

  if (data.duration_by_tool) {
    const durations = data.duration_by_tool.filter(row => isNamedTool(row.tool));
    addCard("duration-by-tool", createCard("Top duration (avg)", renderTable(
      durations,
      [
        { key: "tool", label: "tool" },
        { key: "count", label: "count", num: true },
        { key: "avg", label: "avg", num: true, format: (v) => fmtDurationValue(ctx, v) },
        { key: "p95", label: "p95", num: true, format: (v) => fmtDurationValue(ctx, v) }
      ],
      filterByTool
    )), durations);
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

onWorkspaceChange(() => {
  lastAudit = [];
  auditPaging = freshAuditPaging();
  auditWindowTotal = null;
});

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
    agent_family: null,
    execution_id: null,
    profile: null,
    eventIds: [],
    since: getWindow() === "all" ? null : getWindow(),
    metric: null,
    hide_unverified: false,
  };
}

function navigateToAuditExecution(executionId, ctx) {
  auditFilter = emptyAuditFilter();
  auditFilter.execution_id = executionId;
  activeAuditSubtab = "events";
  syncAuditControls();
  window.location.hash = buildAuditHash();
}

/// Navigates to the Audit tab pre-filtered by the exact recorded `role`.
/// Clears unrelated filters so the landing page is the role view.
function navigateToRole(role, ctx) {
  navigateToDrilldown({ role }, ctx);
}

/// Family, role, or metric drill-down into Audit Events. Incidents use this
/// for evidence. Carries the shared dashboard window and records the source
/// metric so the landing chips explain the scope.
function navigateToDrilldown(opts = {}, ctx) {
  auditFilter = emptyAuditFilter();
  auditFilter.eventIds = Array.isArray(opts.eventIds)
    ? [...new Set(opts.eventIds.filter(id => Number.isSafeInteger(id) && id > 0))]
    : [];
  auditFilter.role = auditFilter.eventIds.length > 0 ? null : (opts.role || null);
  auditFilter.agent_family = auditFilter.eventIds.length > 0 ? null : (opts.agent_family || null);
  auditFilter.metric = auditFilter.eventIds.length > 0 ? null : (opts.metric || null);
  auditFilter.status = auditFilter.eventIds.length > 0 ? null : (opts.status || null);
  // Surface and status filters remain useful for ordinary metric drilldowns.
  // Incident drilldowns supply exact event IDs and clear these broader filters.
  auditFilter.tool = auditFilter.eventIds.length > 0 ? null : (opts.tool || null);
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
    const chip = el("button", { class: "chip", text: status === "non_success" ? "failure + denied" : status });
    chip.dataset.status = status;
    chip.addEventListener("click", () => {
      auditFilter.status = auditFilter.status === status ? null : status;
      syncAuditControls();
      const hash = buildAuditHash();
      window.location.hash = hash;
    });
    container.appendChild(chip);
  }
  const hideChip = el("button", {
    class: "chip",
    type: "button",
    text: "hide unconfirmed successes",
    title: "Hide successful calls from the unconfirmed caller (probe and federated reads). Their failures and denials stay listed.",
  });
  hideChip.dataset.toggle = "hide_unverified";
  hideChip.addEventListener("click", () => {
    auditFilter.hide_unverified = !auditFilter.hide_unverified;
    syncAuditControls();
    window.location.hash = buildAuditHash();
  });
  container.appendChild(hideChip);
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
  const count = $("audit-count");
  if (count) count.textContent = auditPaging.path ? auditCountLabel() : `${events.length}`;
  const footer = buildAuditFooter(ctx);

  if (events.length === 0) {
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: "No audit events match the current filter." }),
    ]), ...(footer && auditPaging.nextBefore ? [footer] : [])]);
    return;
  }

  let table = body.querySelector("table.scoreboard-table");
  let tbody;
  if (!table) {
    table = el("table", { class: "scoreboard-table card-table audit-table" });
    table.dataset.key = "audit-table";
    table.dataset.hash = "audit-table";
    const thead = el("thead");
    const headRow = el("tr");
    for (const col of AUDIT_COLUMNS) {
      headRow.appendChild(el("th", { class: col.num ? "num" : "", text: col.label, title: col.title }));
    }
    thead.appendChild(headRow);
    table.appendChild(thead);
    tbody = el("tbody");
    table.appendChild(tbody);
  } else {
    tbody = table.querySelector("tbody");
  }

  const frag = document.createDocumentFragment();
  for (const ev of events) {
    const exit = ev.exit_code;
    const exitClass = exit != null && exit !== 0 ? "num exit-fail" : "num";
    const targetValue = ev.target_id || ev.target_type || "";
    const target = targetValue === ev.tool_name ? "" : targetValue;
    const cmd = ev.subcommand ? `${ev.command} ${ev.subcommand}` : ev.command;
    const tr = el("tr", { class: "audit-row", title: `event ${ev.id}` });
    tr.dataset.key = `audit-${ev.id}`;
    // Expansion is part of the row's identity: without it the keyed diff reuses
    // the collapsed node and drops the `expanded` class and `aria-expanded`
    // the toggle just set.
    tr.dataset.hash = `${ev.id}-${ev.status}-${exit}-${expandedAuditIds.has(ev.id)}`;
    tr.appendChild(el("td", { class: "c-time", text: fmtTimestampValue(ctx, ev.timestamp), title: fmtAbsTimeValue(ctx, ev.timestamp) }));
    const statusTd = el("td", { class: "c-status" });
    statusTd.appendChild(el("span", { class: `audit-status ${ev.status}`, text: ev.status }));
    tr.appendChild(statusTd);
    tr.appendChild(el("td", { class: "c-role", text: auditActorLabel(ev.role), title: ev.role === "unverified" ? "unverified: caller identity has not been confirmed" : ev.role || "" }));
    tr.appendChild(el("td", { class: "c-command", text: ev.tool_name || cmd || "-", title: ev.tool_name || cmd || "" }));
    tr.appendChild(el("td", { class: "c-target", text: target, title: target }));
    tr.appendChild(el("td", { class: "num c-duration", text: fmtDurationValue(ctx, ev.duration_ms) }));
    tr.appendChild(el("td", { class: `${exitClass} c-exit`, text: exit == null ? "-" : String(exit) }));
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
  syncNodes(body, footer ? [table, footer] : [table]);
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
      runHref(ev.job_run_id),
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
  effectiveAuditWindow,
  // refresh entry points
  fetchAndRenderAudit,
  renderDenialCount,
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
