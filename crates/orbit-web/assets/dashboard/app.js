// Orbit dashboard — terminal-dark, manually refreshed SPA.
// Pure vanilla JS, split into ES modules with no build step.

import { el, statusPill, stateCell, fetchJson, listItems, requestJson, postJson, patchJson, syncNodes, positiveIntParam, getWorkspace, setWorkspace, setMultiWorkspace, isAggregateView, renderPanelPlaceholder, getWindow, persistScopeToUrl, setScopeChangeListener, syncWindowSelectors, payloadHonorsWindow } from './common.js';
import { buildChips, buildTasksHash, applyTasksHashQuery, cacheCrewPayload, copyTaskIdWithNotice, hasCrewOptions, openVisibleTask, renderTasks, setPinnedExternalTask, syncTaskControls, wireSearch } from './tasks.js';
import { applyAuditHashQuery, buildAuditChips, buildAuditHash, fetchAndRenderAudit, fetchAndRenderPolicy, getActiveAuditSubtab, navigateToAuditExecution, renderAuditSummary, setActiveAuditSubtabFromButton, setAuditSubtab, syncAuditControls, wireAuditSearch, } from './audit.js';
import { renderScoreboard } from './scoreboard.js';
import { fetchAndRenderReliability, wireReliabilityWindowSelector } from './reliability.js';
import { initLogTail, fitLogPanelToViewport } from './log-tail.js';
import { renderDiagnostics } from './diagnostics.js';
import { renderMarkdown } from './markdown.js';
import { initRouter, initTabs as iT, navigateToRun as nTR, setActiveTab as sAT, setRunDetailSubtab, } from './router.js';
import { initRuns, getRunFilter, mergeRunsWithFriction, renderRuns, runIsCancellable, buildCancelRunButton, buildReplayRunButton } from './runs.js';
import { fetchAndRenderOperations, initOperations } from './operations.js';
import {
  renderRunDetailEmpty,
  renderRunDetailMeta,
  renderRunSteps,
  renderRunKnowledge,
  renderRunGantt,
  renderRunEvents,
  RUN_EVENTS_LIMIT,
  getActiveRunId,
  setActiveRunId,
  getActiveRunDetail,
  setActiveRunDetail,
  getActiveRunEvents,
  setActiveRunEvents,
  getActiveRunLogs,
  setActiveRunLogs,
  getActiveRunSubtab,
  setActiveRunSubtab,
  getExpandedStepIndices,
  setExpandedStepIndices,
  clearExpandedStepIndices,
  toggleExpandedStepIndex,
  initRunDetail,
} from './run-detail.js';

const STATUS_ORDER = [
  "in-progress",
  "review",
  "blocked",
  "proposed",
  "backlog",
  "someday",
];

const DEFAULT_INACTIVE_STATUSES = new Set(["someday"]);
const STATUS_UPDATE_TARGETS = STATUS_ORDER
  .filter((status) => !["rejected", "archived"].includes(status))
  .concat(["done"]);
// ORB-10874: the statuses shown when no `status` filter is represented in the
// URL — a single source both the initial in-memory state and the hash-parsing
// default (applyTasksHashQuery) read from, so they cannot drift apart.
const DEFAULT_ACTIVE_STATUSES = STATUS_ORDER.filter((s) => !DEFAULT_INACTIVE_STATUSES.has(s));

const JOB_RUN_LIMIT = positiveIntParam("runs", 25);
const DIAG_LIMIT = positiveIntParam("diag", 50);
const FRICTION_LIMIT = positiveIntParam("frictions", 100);

const FRICTION_STATUSES = ["open", "triaged", "resolved"];
const DEFAULT_FRICTION_STATUS_FILTER = "active";
const FRICTION_ACCORDION_QUERY = "(max-width: 1000px)";
const frictionAccordionMedia = window.matchMedia(FRICTION_ACCORDION_QUERY);

const $ = (id) => document.getElementById(id);

let searchQuery = "";
let activeStatuses = new Set(DEFAULT_ACTIVE_STATUSES);
let lastTasks = [];
// ORB-10874: paging metadata from the single-workspace `/api/tasks` envelope
// (`{ items, total, limit, truncated }`) so the count can state a shown/total/
// server-limit fact instead of an ambiguous `N/50`. Null for the aggregate
// `/api/tasks/all` view, which answers a bare array with no such metadata.
let lastTasksMeta = null;
let lastRuns = [];
let lastRunsMeta = null;
let lastRunSourcesUnavailable = [];
let lastDiagnostics = { metrics: [], errors: [], incidents: null, implement_one: [], implement_one_by_complexity: [], completion_by_complexity: [] };
let lastFrictionPayload = { stats: {}, tags: [], items: [] };
let activeTab = "tasks";
let activeDiagSubtab = "runs";
let activeKnowledgeSubtab = "frictions";
let activeOperationsSubtab = "routines";
let isRefreshing = false;
let activeFrictionId = null;
let frictionSearchQuery = "";
let frictionStatusFilter = DEFAULT_FRICTION_STATUS_FILTER;

// Health strip state
let lastSummary = null;

// ORB-00030: workspaces the dashboard is serving. Empty/one entry => single
// mode (no selector). More than one => global mode (selector shown).
let dashboardWorkspaces = [];

function taskContext() {
  return {
    getTasks: () => lastTasks,
    getTasksMeta: () => lastTasksMeta,
    replaceTask: (updatedTask) => {
      const index = lastTasks.findIndex((task) => task.id === updatedTask.id);
      if (index >= 0) {
        lastTasks[index] = updatedTask;
      }
    },
    getSearchQuery: () => searchQuery,
    setSearchQuery: (value) => { searchQuery = value; },
    getActiveStatuses: () => activeStatuses,
    setActiveStatuses: (statuses) => { activeStatuses = statuses; },
    statusOrder: STATUS_ORDER,
    defaultActiveStatuses: DEFAULT_ACTIVE_STATUSES,
    statusUpdateTargets: STATUS_UPDATE_TARGETS,
    fmtAbsTime,
    refreshDashboard,
  };
}

function auditContext() {
  return {
    navigateToRun: nTR,
    setActiveTab: sAT,
    refreshDashboard,
    fmtDuration,
    fmtTimestamp,
    fmtRelative,
    fmtAbsTime,
    truncate,
  };
}

function diagnosticsContext() {
  return {
    getLastDiagnostics: () => lastDiagnostics,
    getActiveDiagSubtab: () => activeDiagSubtab,
    fmtRelative,
    fmtDuration,
    // ORB-10871: incident expansion states exact first/last timestamps, not
    // just "3h ago" — the raw evidence has to be locatable in the Audit view.
    fmtAbsTime,
    truncate,
    setActiveTab: sAT,
    navigateToRun: nTR,
  };
}

function routerContext() {
  return {
    // getters/setters for router-owned state (kept in app.js per extraction contract)
    getTab: () => activeTab,
    setTab: (v) => { activeTab = v; },
    getDiagSubtab: () => activeDiagSubtab,
    setDiagSubtab: (v) => { activeDiagSubtab = v; },
    getKnowledgeSubtab: () => activeKnowledgeSubtab,
    setKnowledgeSubtab: (v) => { activeKnowledgeSubtab = v; },
    getOperationsSubtab: () => activeOperationsSubtab,
    setOperationsSubtab: (v) => { activeOperationsSubtab = v; },
    getRunId: getActiveRunId,
    setRunId: setActiveRunId,
    getRunSubtab: getActiveRunSubtab,
    setRunSubtab: setActiveRunSubtab,
    getRunDetail: getActiveRunDetail,
    setRunDetail: setActiveRunDetail,
    getRunEvents: getActiveRunEvents,
    setRunEvents: setActiveRunEvents,
    getRunLogs: getActiveRunLogs,
    setRunLogs: setActiveRunLogs,
    getExpandedSteps: getExpandedStepIndices,
    setExpandedSteps: setExpandedStepIndices,

    // last* for render helpers used by router
    getLastRuns: () => lastRuns,

    // callbacks (close over app.js scope)
    refreshDashboard,
    renderDiagnostics: () => renderDiagnostics(diagnosticsContext()),
    // ORB-10588: Reliability aggregates server-side across workspaces, so a
    // subtab switch can fetch straight away instead of waiting for the next
    // refresh tick (and without the aggregate-view guard the other diagnostics
    // fetches need).
    fetchReliability: () => fetchAndRenderReliability().catch((e) => console.error("Failed to fetch reliability metrics", e)),
    fitLogPanelToViewport,

    // audit pass-throughs (re-exported here for router; imported at top of this file)
    applyAuditHashQuery,
    setAuditSubtab,
    getActiveAuditSubtab,
    setActiveAuditSubtabFromButton,
    buildAuditHash,
    syncAuditControls,

    // ORB-10874: tasks pass-throughs (mirrors the audit ones above) so the
    // status/search filters shown on the Tasks tab are represented in the
    // hash and survive reload/back-navigation the same way audit's do.
    applyTasksHashQuery: (query) => applyTasksHashQuery(query, taskContext()),
    buildTasksHash: () => buildTasksHash(taskContext()),
    syncTaskControls: () => syncTaskControls(taskContext()),
  };
}

function runsContext() {
  return {
    navigateToRun: nTR,
    fetchAndRenderRuns,
    fetchAndRenderRunDetail,
    fetchAndRenderRunEvents,
    getActiveRunId,
    getLastRuns: () => lastRuns,
    getRunsMeta: () => lastRunsMeta,
    getRunSourcesUnavailable: () => lastRunSourcesUnavailable,
    fmtTimestamp,
    fmtDuration,
  };
}

function runDetailContext() {
  return {
    // state getters/setters (module-scoped in run-detail.js)
    getActiveRunId,
    setActiveRunId,
    getActiveRunDetail,
    setActiveRunDetail,
    getActiveRunEvents,
    setActiveRunEvents,
    getActiveRunLogs,
    setActiveRunLogs,
    getActiveRunSubtab,
    setActiveRunSubtab,
    getExpandedStepIndices,
    setExpandedStepIndices,
    clearExpandedStepIndices,
    toggleExpandedStepIndex,
    // callbacks the run-detail renderers invoke (Gantt click handler etc.)
    setRunDetailSubtab,
    // formatters (stay in app.js until common.js extraction)
    fmtTimestamp,
    fmtDuration,
    fmtRelative,
    fmtAbsTime,
    truncate,
    // run action builders (from runs.js) and nav for renderRunDetailMeta
    navigateToRun: nTR,
    setActiveTab: sAT,
    runIsCancellable,
    buildCancelRunButton,
    buildReplayRunButton,
    // render fns for orchestrator symmetry
    renderRunDetailEmpty,
    renderRunDetailMeta,
    renderRunSteps,
    renderRunKnowledge,
    renderRunGantt,
    renderRunEvents,
  };
}

function renderBodyBlock(body, fallbackClass) {
  if (!body || !body.trim()) return null;
  const rendered = renderMarkdown(body);
  const view = el(rendered !== null ? "div" : "pre", {
    class: rendered !== null ? "markdown-body" : fallbackClass,
  });
  if (rendered !== null) {
    view.innerHTML = rendered;
  } else {
    view.textContent = body;
  }
  return el("div", { class: "field-block" }, [
    el("h4", { text: "body" }),
    view,
  ]);
}

function renderLocksPanel(payload) {
  const body = $("locks-body");
  const count = $("locks-count");
  if (!body || !count) return;
  const byTask = Array.isArray(payload && payload.by_task) ? payload.by_task : [];
  const totalLocked = Number.isFinite(Number(payload && payload.total_locked))
    ? Number(payload.total_locked)
    : 0;
  const totalTasks = Number.isFinite(Number(payload && payload.total_tasks))
    ? Number(payload.total_tasks)
    : byTask.length;
  count.textContent = `${totalLocked} files / ${totalTasks} tasks`;

  if (byTask.length === 0) {
    const empty = el("div", { class: "locks-empty", text: "No files currently locked." });
    empty.dataset.key = "locks-empty";
    empty.dataset.hash = "locks-empty";
    syncNodes(body, [empty]);
    return;
  }

  const nodes = byTask.map((task) => {
    const taskId = String(task.id || "");
    const group = el("div", { class: "lock-task-group" });
    group.dataset.key = `lock-task-${taskId}`;
    group.dataset.hash = JSON.stringify(task);

    const idButton = el("button", {
      class: "lock-task-id mono",
      text: `[${taskId}]`,
      title: `Open ${taskId} in the task list`,
    });
    idButton.addEventListener("click", (e) => {
      e.stopPropagation();
      openVisibleTask(taskId, taskContext());
    });

    const header = el("div", { class: "lock-task-header" }, [
      idButton,
      el("span", { class: "lock-separator", text: "·" }),
      statusPill(task.status || "unknown"),
    ]);
    if (task.job_run_id) {
      header.appendChild(el("span", { class: "lock-separator", text: "·" }));
      header.appendChild(el("span", {
        class: "lock-job mono",
        text: `job_run=${task.job_run_id}`,
        title: task.job_run_id,
      }));
    }
    group.appendChild(header);

    const files = Array.isArray(task.context_files) ? task.context_files : [];
    for (const path of files) {
      group.appendChild(el("div", {
        class: "lock-file-row mono",
        text: String(path),
        title: String(path),
      }));
    }
    return group;
  });
  syncNodes(body, nodes);
}

function fmtTimestamp(iso) {
  if (!iso) return "-";
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  const now = Date.now();
  const diff = (now - d.getTime()) / 1000;
  if (diff < 60) return `${Math.floor(diff)}s`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

function fmtAbsTime(iso) {
  if (!iso) return "-";
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fmtDuration(ms) {
  if (ms == null) return "-";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60000)}m${Math.floor((ms % 60000) / 1000)}s`;
}

function knowledgeStatusPill(status) {
  const value = status || "active";
  return el("span", { class: `knowledge-pill ${value}`, text: value });
}

function knowledgeTagWrap(nodes) {
  const values = Array.isArray(nodes) ? nodes : [];
  return el("span", { class: "knowledge-tags" }, values.length > 0
    ? values
    : [el("span", { class: "knowledge-tag dim", text: "-" })]);
}

function detailMetaRows(entries) {
  const rows = entries
    .filter(([, value]) => value != null && value !== "")
    .map(([label, value]) => el("div", { class: "meta-row" }, [
      el("span", { class: "k", text: label }),
      el("span", { class: "v", text: String(value) }),
    ]));
  return el("div", { class: "meta-list" }, rows);
}

function detailGroup(title, content) {
  return el("div", { class: "knowledge-side-group" }, [
    el("h4", { text: title }),
    content || el("div", { class: "empty", text: "-" }),
  ]);
}

function markdownPanel(body, fallbackClass) {
  const text = body || "";
  const rendered = renderMarkdown(text || "_No body._");
  const view = el(rendered !== null ? "div" : "pre", {
    class: rendered !== null ? "markdown-body" : fallbackClass,
  });
  if (rendered !== null) {
    view.innerHTML = rendered;
  } else {
    view.textContent = text || "No body.";
  }
  return view;
}

function frictionTagNodes(tags = []) {
  const values = Array.isArray(tags) ? tags : [];
  if (values.length === 0) return [el("span", { class: "knowledge-tag dim", text: "-" })];
  return values.map((tag) => el("span", { class: "knowledge-tag", text: tag, title: tag }));
}

function renderFrictionStats(stats = {}) {
  $("friction-open-value").textContent = formatBigInt(stats.open || 0);
  $("friction-triaged-value").textContent = formatBigInt(stats.triaged || 0);
  $("friction-resolved-month-value").textContent = formatBigInt(stats.resolved_this_month || 0);
}

function frictionFilterLabel() {
  return frictionStatusFilter === "all" ? "all" : frictionStatusFilter;
}

function frictionAvailableCount(stats = {}) {
  const open = Number(stats.open) || 0;
  const triaged = Number(stats.triaged) || 0;
  const total = Number(stats.total) || 0;
  if (frictionStatusFilter === "active") return open + triaged;
  if (frictionStatusFilter === "open") return open;
  if (frictionStatusFilter === "triaged") return triaged;
  if (frictionStatusFilter === "resolved") return Math.max(0, total - open - triaged);
  return total;
}

function renderFrictions(payload) {
  const body = $("frictions-body");
  if (!body) return;
  const items = Array.isArray(payload && payload.items) ? payload.items : [];
  const stats = (payload && payload.stats) || {};
  const accordion = frictionAccordionMedia.matches;
  renderFrictionStats(stats);
  const available = frictionAvailableCount(stats);
  const count = $("knowledge-count");
  count.textContent = `${items.length}/${available}`;
  count.title = `Showing ${items.length} of ${available} ${frictionFilterLabel()} frictions`;

  if (items.length > 0 && !items.some((item) => item.id === activeFrictionId)) {
    activeFrictionId = accordion ? null : items[0].id;
  }
  if (items.length === 0) activeFrictionId = null;

  if (items.length === 0) {
    const suffix = frictionSearchQuery ? " match the current search." : ".";
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: `No ${frictionFilterLabel()} frictions${suffix}` }),
    ])]);
    renderFrictionDetail(null);
    return;
  }

  const frag = document.createDocumentFragment();

  for (const friction of items) {
    const title = friction.title || friction.id;
    const expanded = activeFrictionId === friction.id;
    const detailId = `friction-accordion-${friction.id}`;
    const row = el("div", { class: "knowledge-row friction-row", title }, [
      el("div", { class: "top" }, [
        el("span", { class: "id", text: friction.id, title: friction.id }),
        el("span", { class: "spacer" }),
        el("span", { class: "when", text: fmtTimestamp(friction.created_at), title: fmtAbsTime(friction.created_at) }),
        accordion ? el("span", { class: "friction-row-toggle", text: expanded ? "▾" : "▸" }) : null,
      ]),
      el("div", { class: "title", text: title }),
      el("div", { class: "summary", text: truncate(friction.body || title, 180) }),
      el("div", { class: "meta" }, [
        knowledgeStatusPill(friction.status || "open"),
        el("span", { class: "dot", text: "·" }),
        knowledgeTagWrap(frictionTagNodes(friction.tags)),
        ...(friction.during_task ? [
          el("span", { class: "dot", text: "·" }),
          el("span", { text: `during ${friction.during_task}` }),
        ] : []),
      ]),
    ]);
    row.dataset.key = `friction-${friction.id}`;
    row.dataset.hash = `${friction.id}-${friction.status}-${(friction.tags || []).join(",")}-${friction.created_at}-${accordion}-${expanded}`;
    if (expanded) row.classList.add("active");
    const toggle = () => {
      activeFrictionId = accordion && expanded ? null : friction.id;
      renderFrictions(lastFrictionPayload);
    };
    row.addEventListener("click", toggle);
    if (accordion) {
      row.tabIndex = 0;
      row.setAttribute("role", "button");
      row.setAttribute("aria-expanded", String(expanded));
      row.setAttribute("aria-controls", detailId);
      row.addEventListener("keydown", (event) => {
        if (event.key !== "Enter" && event.key !== " ") return;
        event.preventDefault();
        toggle();
      });
    }
    frag.appendChild(row);
    if (accordion && expanded) {
      const inlineDetail = el("section", { class: "friction-accordion-detail" });
      inlineDetail.id = detailId;
      inlineDetail.dataset.key = `friction-detail-${friction.id}`;
      inlineDetail.dataset.hash = JSON.stringify({ friction, tags: lastFrictionPayload.tags });
      inlineDetail.setAttribute("aria-label", `Details for ${friction.id}`);
      renderFrictionDetail(friction, inlineDetail);
      frag.appendChild(inlineDetail);
    }
  }

  syncNodes(body, Array.from(frag.children));
  renderFrictionDetail(accordion ? null : items.find((item) => item.id === activeFrictionId) || items[0]);
}

function renderFrictionDetail(friction, detail = $("friction-detail")) {
  if (!detail) return;
  const count = detail.id === "friction-detail" ? $("friction-detail-count") : null;
  if (!friction) {
    if (count) count.textContent = "-";
    syncNodes(detail, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: "No friction selected." }),
    ])]);
    return;
  }
  if (count) count.textContent = friction.status || "open";

  const controls = el("div", { class: "friction-controls" });
  const controlGrid = el("div", { class: "friction-control-grid" });
  controlGrid.appendChild(buildFrictionStatusControl(friction, detail));
  controlGrid.appendChild(buildFrictionTagPicker(friction, detail));
  controls.appendChild(controlGrid);

  const actions = el("div", { class: "actions" });
  const resolve = el("button", {
    class: "knowledge-btn primary",
    text: "resolve",
    title: `Resolve ${friction.id}`,
  });
  resolve.type = "button";
  resolve.disabled = friction.status === "resolved";
  resolve.addEventListener("click", () => resolveFriction(friction, resolve, detail));
  actions.appendChild(resolve);

  const body = el("div", { class: "knowledge-detail-body" }, [
    el("div", { class: "knowledge-body" }, [
      markdownPanel(friction.body || friction.title, "friction-detail-body"),
    ]),
    el("aside", { class: "knowledge-side" }, [
      detailGroup("metadata", detailMetaRows([
        ["id", friction.id],
        ["status", friction.status || "open"],
        ["model", friction.model],
        ["reported", fmtAbsTime(friction.created_at)],
        ["resolved", friction.resolved_at ? fmtAbsTime(friction.resolved_at) : "—"],
      ])),
      detailGroup("triage", controls),
      detailGroup("tags", knowledgeTagWrap(frictionTagNodes(friction.tags))),
      friction.during_task ? detailGroup("during task", buildKnowledgeValueList([friction.during_task], { taskLinks: true })) : null,
    ].filter(Boolean)),
  ]);

  syncNodes(detail, [
    el("div", { class: "knowledge-detail-head" }, [
      el("div", { class: "crumb" }, [
        el("span", { class: "id", text: friction.id }),
        el("span", { text: "·" }),
        el("span", { text: "friction event" }),
        el("span", { text: "·" }),
        el("span", { text: friction.status || "open" }),
      ]),
      el("h2", { text: friction.title || friction.id }),
      el("div", { class: "sub" }, [
        knowledgeStatusPill(friction.status || "open"),
        friction.model ? el("span", { text: `reported by ${friction.model}` }) : null,
        el("span", { class: "dot", text: "·" }),
        el("span", { text: fmtAbsTime(friction.created_at) }),
        ...(friction.during_task ? [
          el("span", { class: "dot", text: "·" }),
          el("span", { text: `during ${friction.during_task}` }),
        ] : []),
      ]),
      actions,
    ]),
    body,
  ]);
}

function buildFrictionStatusControl(friction, detail) {
  const wrap = el("label", { class: "friction-control" });
  wrap.appendChild(el("span", { class: "friction-control-label", text: "status" }));
  const select = el("select", { class: "action status-update", title: `Status for ${friction.id}` });
  for (const status of FRICTION_STATUSES) {
    const option = el("option", { text: status });
    option.value = status;
    option.selected = (friction.status || "open") === status;
    select.appendChild(option);
  }
  select.addEventListener("change", () => {
    patchFriction(friction, { status: select.value }, select, detail);
  });
  wrap.appendChild(select);
  return wrap;
}

function buildFrictionTagPicker(friction, detail) {
  const wrap = el("div", { class: "friction-control friction-tag-picker" }, [
    el("span", { class: "friction-control-label", text: "tags" }),
  ]);
  const options = Array.isArray(lastFrictionPayload.tags) ? lastFrictionPayload.tags : [];
  const selected = new Set(Array.isArray(friction.tags) ? friction.tags : []);
  const grid = el("div", { class: "friction-tag-options" });
  const checkboxes = new Map();
  if (options.length === 0) {
    grid.appendChild(el("span", { class: "knowledge-tag dim", text: "-" }));
  }
  for (const tag of options) {
    const id = `friction-tag-${friction.id}-${tag}`;
    const checkbox = el("input");
    checkbox.type = "checkbox";
    checkbox.id = id;
    checkbox.checked = selected.has(tag);
    checkboxes.set(tag, checkbox);
    checkbox.addEventListener("change", () => {
      const tags = options.filter((option) => checkboxes.get(option)?.checked);
      if (tags.length === 0) {
        checkbox.checked = true;
        return;
      }
      patchFriction(friction, { tags }, checkbox, detail);
    });
    const label = el("label", { class: "friction-tag-option", title: tag }, [
      checkbox,
      el("span", { text: tag }),
    ]);
    grid.appendChild(label);
  }
  wrap.appendChild(grid);
  return wrap;
}

async function patchFriction(friction, patch, control, detail) {
  if (!friction || !friction.id) return;
  if (control) control.disabled = true;
  for (const node of detail.querySelectorAll(".action-error")) node.remove();
  try {
    const updated = await patchJson(`/api/frictions/${encodeURIComponent(friction.id)}`, patch);
    activeFrictionId = updated.id || friction.id;
    await fetchAndRenderFrictions();
  } catch (e) {
    detail.prepend(el("div", { class: "action-error", text: e.message || "friction update failed" }));
    if (patch.status && control) control.value = friction.status || "open";
  } finally {
    if (control) control.disabled = false;
  }
}

async function resolveFriction(friction, btn, detail) {
  const oldText = btn.textContent;
  btn.disabled = true;
  btn.innerHTML = `<span class="spinner"></span>wait`;
  for (const node of detail.querySelectorAll(".action-error")) node.remove();
  try {
    const updated = await postJson(`/api/frictions/${encodeURIComponent(friction.id)}/resolve`);
    activeFrictionId = updated.id || friction.id;
    await fetchAndRenderFrictions();
  } catch (e) {
    detail.prepend(el("div", { class: "action-error", text: e.message || "resolve failed" }));
  } finally {
    btn.disabled = false;
    btn.textContent = oldText;
  }
}

function buildKnowledgeValueList(values, opts = {}) {
  const wrap = el("div", { class: "knowledge-detail-list" });
  if (!values || values.length === 0) {
    wrap.appendChild(el("span", { class: "knowledge-tag dim", text: "-" }));
    return wrap;
  }
  for (const value of values) {
    if (opts.taskLinks) {
      const btn = el("button", { class: "knowledge-link-row", text: value, title: `Open task ${value}` });
      btn.type = "button";
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        openTaskFromKnowledge(value);
      });
      wrap.appendChild(btn);
    } else {
      wrap.appendChild(el("span", { class: "knowledge-tag", text: value, title: value }));
    }
  }
  return wrap;
}

function openTaskFromKnowledge(taskId) {
  activeStatuses = new Set(STATUS_ORDER);
  searchQuery = "";
  const taskSearch = $("task-search");
  if (taskSearch) taskSearch.value = "";
  sAT("tasks", { refresh: false });
  const open = () => openVisibleTask(taskId, taskContext());
  if (lastTasks.length > 0 && hasCrewOptions()) {
    open();
    return;
  }
  fetchAndRenderTasks().then(() => {
    open();
  }).catch(() => copyTaskIdWithNotice(taskId, taskContext()));
}

function wireFrictionSearch() {
  const input = $("friction-search");
  if (!input) return;
  let debounce = null;
  input.addEventListener("input", (e) => {
    frictionSearchQuery = e.target.value.trim();
    if (debounce) clearTimeout(debounce);
    debounce = setTimeout(() => {
      if (activeTab === "knowledge") fetchAndRenderFrictions().catch(console.error);
    }, 200);
  });
}

function wireFrictionStatusFilter() {
  const select = $("friction-status-filter");
  if (!select) return;
  select.value = frictionStatusFilter;
  select.addEventListener("change", (e) => {
    frictionStatusFilter = e.target.value;
    activeFrictionId = null;
    if (activeTab === "knowledge") fetchAndRenderFrictions().catch(console.error);
  });
}

function wireFrictionResponsiveDetail() {
  frictionAccordionMedia.addEventListener("change", () => {
    renderFrictions(lastFrictionPayload);
  });
}

/* Global task ID resolver (ORB-00211).
   - Only fires on exact ^ORB-\d{5}$ (case-insens) after trim/upper.
   - 250ms debounce to avoid hammering dashboard_status_index full scan.
   - On success: switch tab, clear per-tab search, freshen lastTasks, openVisible (active status)
     or setPinnedExternalTask + render (for done/rejected/archived bypassing filter).
   - Error: inline .error + span msg with ID, cleared on any next input.
   - Works from any tab because header input + sAT('tasks').
*/
function wireGlobalTaskResolver() {
  const input = $("global-task-id");
  if (!input) return;
  let debounce = null;
  const ID_RE = /^ORB-\d{5}$/i;

  function clearError() {
    input.classList.remove("error");
    const wrap = input.parentNode;
    if (wrap && wrap.classList) wrap.classList.remove("error");
    const err = $("global-task-id-error");
    if (err) err.textContent = "";
  }

  input.addEventListener("input", () => {
    clearError();
    if (debounce) clearTimeout(debounce);
    const raw = (input.value || "").trim();
    if (!raw) return;
    const candidate = raw.toUpperCase();
    if (!ID_RE.test(candidate)) {
      // partial input: explicitly do not fetch (per AC and CPU guard)
      return;
    }
    // full match: debounce then fetch exactly once
    debounce = setTimeout(() => {
      const id = candidate;
      fetch(`/api/tasks/${encodeURIComponent(id)}`, { headers: { accept: "application/json" } })
        .then(async (res) => {
          if (res.ok) {
            const task = await res.json();
            sAT("tasks");
            searchQuery = "";
            const ts = $("task-search");
            if (ts) ts.value = "";
            // freshen cache so open sees latest (incl sidecars)
            const idx = lastTasks.findIndex((t) => t && t.id === task.id);
            if (idx >= 0) lastTasks[idx] = task;
            else lastTasks.push(task);
            const ctx = taskContext();
            if (activeStatuses.has(task.status)) {
              openVisibleTask(task.id, ctx);
            } else {
              setPinnedExternalTask(task, ctx);
              renderTasks(lastTasks, ctx);
            }
            // success: clear the jump input for next use
            input.value = "";
          } else if (res.status === 404) {
            showGlobalIdError(id, `${id} not found`);
          } else {
            showGlobalIdError(id, `Error ${res.status} resolving ${id}`);
          }
        })
        .catch(() => {
          showGlobalIdError(id, `Network error resolving ${id}`);
        });
    }, 250);
  });

  function showGlobalIdError(id, msg) {
    input.classList.add("error");
    const wrap = input.parentNode;
    if (wrap && wrap.classList) wrap.classList.add("error");
    const err = $("global-task-id-error");
    if (err) err.textContent = msg;
  }
}

function fmtRelative(iso) {
  return fmtTimestamp(iso);
}

function truncate(text, max) {
  if (text == null) return "";
  if (text.length <= max) return text;
  return text.slice(0, max) + "\u2026";
}

// ORB-00039/00040: the aggregate ("All workspaces") view has no concrete
// workspace to scope per-workspace endpoints to — the backend `Ws` extractor
// finds no default and returns 400. The single `isAggregateView()` predicate
// (shared from common.js, backed by the multi-workspace flag set in
// initWorkspaceSelector) gates every per-workspace fetch across app.js,
// audit.js and scoreboard.js; picking a concrete workspace flips it false and
// restores every panel.

// ORB-00039: in aggregate mode the per-workspace fetches are skipped, so the
// panels they feed (audit summary, locked files) show an inline placeholder and
// the health strip is neutralized rather than left displaying one workspace's
// stale counts.
function renderAggregatePlaceholders() {
  renderPanelPlaceholder("audit-summary-body");
  renderPanelPlaceholder("locks-body");
  const locksCount = $("locks-count");
  if (locksCount) locksCount.textContent = "—";
  resetHealthStrip();
}

// Most Diagnostics panels are fed exclusively by per-workspace endpoints and
// remain unavailable in aggregate mode. Runs is the exception: its body is
// managed by the bounded /api/job-runs/all response and must not be overwritten
// by these placeholders.
function renderDiagnosticsPlaceholders() {
  renderPanelPlaceholder("diag-body");
  renderPanelPlaceholder("scoreboard-body");
  renderPanelPlaceholder("diag-implement-one-body");
  const diagCount = $("diag-count");
  if (diagCount) diagCount.textContent = "—";
}

// ORB-00044: the friction detail panel holds live action buttons and status/tag
// controls that POST/PATCH per-workspace endpoints.
// Left stale in aggregate mode, a click would fire without ?workspace= — 400 in
// pure-global mode, or a silent write to the default workspace inside a
// --global workspace — so the stale detail is replaced by the same placeholder
// as its list panel. Selecting a concrete workspace re-fetches and re-renders.
function renderKnowledgeDetailPlaceholder(prefix) {
  renderPanelPlaceholder(`${prefix}-detail`);
  const count = $(`${prefix}-detail-count`);
  if (count) count.textContent = "—";
}

// The health strip is fed by the same per-workspace /api/audit/summary endpoint,
// so in aggregate mode reset its tiles to a neutral dash rather than showing the
// last-selected workspace's numbers as if they were machine-wide.
function resetHealthStrip() {
  for (const id of [
    "tile-events-value",
    "tile-denials-value",
    "tile-failed-value",
    "tile-active-value",
  ]) {
    const node = $(id);
    if (node) node.textContent = "—";
  }
  const denials = $("tile-denials");
  if (denials) denials.classList.remove("tile-alert");
  renderSparkline([]);
}

function fetchAndCacheCrews() {
  return fetchJson("/api/crews").then((payload) => {
    return cacheCrewPayload(payload);
  });
}

// ORB-10874: the single-workspace list endpoint filters server-side, so the
// active status chips are sent as `?status=a,b` instead of over-fetching every
// status and filtering client-side. That keeps the `total`/`limit`/`truncated`
// envelope meaningful for the filter actually in effect (see formatTaskCount
// in tasks.js). Omitted when every status (or none) is active: the endpoint's
// status-neutral response is correct for all-active, while an empty selection
// is applied client-side because the API has no empty status set.
function tasksListPath() {
  const sp = new URLSearchParams();
  if (activeStatuses.size > 0 && activeStatuses.size < STATUS_ORDER.length) {
    sp.set("status", STATUS_ORDER.filter((s) => activeStatuses.has(s)).join(","));
  }
  const qs = sp.toString();
  return qs ? `/api/tasks?${qs}` : "/api/tasks";
}

function fetchAndRenderTasks() {
  // In global mode with the aggregate ("All workspaces") view selected, pull
  // every workspace's tasks; otherwise the single/selected workspace's tasks
  // (the workspace query param is applied by fetchJson).
  const aggregate = isAggregateView();
  const path = aggregate ? "/api/tasks/all" : tasksListPath();
  // /api/crews is per-workspace and 400s without a concrete workspace, so in
  // aggregate mode skip the crew fetch and let the crew controls degrade to a
  // disabled "crew unavailable" fallback rather than rejecting the Promise.all
  // and blocking the whole task list. Clear any crew cache left over from a
  // previously-selected concrete workspace so the fallback is consistent (a
  // stale, still-enabled crew <select> would PATCH with no workspace).
  if (aggregate) cacheCrewPayload({ crews: [] });
  const crews = aggregate ? Promise.resolve() : fetchAndCacheCrews();
  return Promise.all([
    fetchJson(path),
    crews,
  ]).then(([payload]) => {
    // /api/tasks answers `{ items, total, limit, truncated }` (ORB-10400);
    // /api/tasks/all a bare array with no paging metadata.
    const tasks = listItems(payload);
    lastTasks = tasks;
    lastTasksMeta = !aggregate && payload && !Array.isArray(payload)
      ? { total: payload.total, limit: payload.limit, truncated: payload.truncated }
      : null;
    renderTasks(tasks, taskContext());
  });
}

// ORB-00030: discover servable workspaces and, in global mode, install a
// header selector. Runs before the first refresh so the initial fetches target
// the right workspace. Failures are non-fatal (single-workspace fallback).
async function initWorkspaceSelector() {
  let entries;
  try {
    entries = await fetchJson("/api/workspaces");
  } catch (e) {
    console.error(e);
    return;
  }
  dashboardWorkspaces = Array.isArray(entries) ? entries : [];
  // Feed the shared aggregate-view predicate (common.js): multi-workspace mode
  // is what makes the "All workspaces" (no concrete workspace) view possible.
  setMultiWorkspace(dashboardWorkspaces.length > 1);
  if (dashboardWorkspaces.length <= 1) {
    const only = dashboardWorkspaces.find((workspace) => workspace.status === "active");
    if (only) setWorkspace(only.id);
    return; // single mode: selected implicitly, no selector needed
  }

  // Default to the workspace flagged by the server (the cwd workspace, if the
  // server was launched inside one) so every tab works out of the box; else the
  // first active workspace. "All workspaces" (aggregate) is an explicit choice.
  if (!getWorkspace()) {
    const def = dashboardWorkspaces.find((w) => w.is_default);
    const firstActive = dashboardWorkspaces.find((w) => w.status === "active");
    const initial = (def || firstActive || dashboardWorkspaces[0]).id;
    setWorkspace(initial);
  }
  buildWorkspaceSelector();
}

function buildWorkspaceSelector() {
  const select = el("select", { class: "workspace-select", title: "Workspace" });
  select.id = "workspace-select";

  const allOption = el("option", { text: "All workspaces" });
  allOption.value = "";
  select.appendChild(allOption);

  const current = getWorkspace() || "";
  for (const ws of dashboardWorkspaces) {
    const active = ws.status === "active";
    const label = active ? ws.name : `${ws.name} (unavailable)`;
    const option = el("option", { text: label });
    option.value = ws.id;
    option.disabled = !active;
    if (ws.id === current) option.selected = true;
    select.appendChild(option);
  }
  if (!current) allOption.selected = true;

  select.addEventListener("change", () => {
    setWorkspace(select.value);
    persistScopeToUrl();
    // Clear the prior scope synchronously. A slower request from that scope is
    // also discarded in fetchAndRenderRuns, so it cannot repaint stale rows.
    lastRuns = [];
    lastRunsMeta = null;
    lastRunSourcesUnavailable = [];
    renderRuns(lastRuns);
    refreshDashboard();
  });

  const note = el("span", {
    class: "workspace-scope-note",
    text: "Fleet-wide on Reliability",
  });
  note.id = "workspace-scope-note";
  note.hidden = true;
  note.title = "Reliability ignores the selected workspace";

  // ORB-10972: the selector moved from the header meta cluster into the rail
  // foot, beside the connection line. Same element, same id, same listeners.
  const host = $("rail-workspace");
  if (!host) return;
  host.innerHTML = "";
  host.appendChild(select);
  host.appendChild(note);
}

// ORB-10874/ORB-10872: workspace + window live in the query string so a
// reload or copied link restores the same dashboard scope. `persistScopeToUrl`
// is the single writer; this wrapper stays for the existing call sites.
function persistWorkspaceToUrl(id) {
  setWorkspace(id);
  persistScopeToUrl();
}

function activeRefreshJobs() {
  // ORB-00039: in the aggregate "All workspaces" view there is no concrete
  // workspace to scope per-workspace endpoints to, so skip every per-workspace
  // fetch (they'd 400) and render placeholders for the panels they feed. Only
  // the cross-workspace aggregate task list (/api/tasks/all) is fetched.
  const aggregate = isAggregateView();

  // The health strip is global; refresh on every tick alongside the active tab.
  // The per-workspace summary (/api/audit/summary) is replaced by a placeholder
  // instead of fetched in aggregate mode.
  const jobs = [];
  if (aggregate) {
    renderAggregatePlaceholders();
  } else {
    jobs.push(fetchAndRenderSummary());
  }

  if (activeTab === "tasks") {
    jobs.push(fetchAndRenderTasks());
    // /api/tasks/locks is per-workspace; skip it in aggregate mode (the locks
    // panel shows the placeholder rendered above).
    if (!aggregate && !document.hidden) jobs.push(fetchAndRenderTaskLocks());
    return jobs;
  }

  if (activeTab === "audit") {
    if (getActiveAuditSubtab() === "policy") {
      jobs.push(fetchAndRenderPolicy(auditContext()));
    } else {
      jobs.push(fetchAndRenderAudit(auditContext()));
    }
    return jobs;
  }

  if (activeTab === "knowledge") {
    jobs.push(fetchAndRenderFrictions());
    return jobs;
  }

  if (activeTab === "operations") {
    jobs.push(fetchAndRenderOperations());
    return jobs;
  }

  if (activeTab === "run-detail") {
    if (!getActiveRunId()) {
      renderRunDetailEmpty("No run selected.");
      return jobs;
    }
    jobs.push(fetchAndRenderRunDetail());
    // Events power both the Events sub-tab and the Gantt's retry markers, so
    // they're fetched on every run-detail refresh regardless of which sub-tab
    // is active.
    jobs.push(fetchAndRenderRunEvents());
    jobs.push(fetchAndRenderRunLogs());
    return jobs;
  }

  if (activeTab === "diagnostics") {
    // Most diagnostics fetches are per-workspace. Runs has a bounded aggregate
    // endpoint; metrics, errors, friction, implement_one and Scoreboard still
    // require a concrete workspace and show placeholders without one.
    // ORB-10588: /api/metrics/reliability takes the whole DashboardState, not
    // the `Ws` extractor, so it answers in aggregate mode too — it is fetched
    // ahead of the guard below rather than being placeheld with the rest.
    if (activeDiagSubtab === "reliability") {
      jobs.push(
        fetchAndRenderReliability()
          .catch((e) => console.error("Failed to fetch reliability metrics", e))
      );
      return jobs;
    }
    if (aggregate && activeDiagSubtab === "runs") {
      renderDiagnosticsPlaceholders();
      jobs.push(fetchAndRenderRuns());
      return jobs;
    }
    if (aggregate) {
      renderDiagnosticsPlaceholders();
      return jobs;
    }
    if (activeDiagSubtab === "scoreboard") {
      // ORB-10444: Scoreboard folded in from the retired top-level tab.
      // ORB-10872: every refresh honors the shared dashboard window so
      // delivery/operations and Managed Execution stay on the same cutoff.
      // A payload that reports a different window is refused rather than
      // painted under a mismatched selector (the 7d-selected / 24h-body bug).
      const selectedWindow = getWindow();
      jobs.push(
        fetchJson(`/api/scoreboard?window=${encodeURIComponent(selectedWindow)}`).then((summary) => {
          if (!payloadHonorsWindow(summary, selectedWindow)) {
            console.error(
              `scoreboard payload window ${summary && summary.window} rejected under ${selectedWindow} selection`,
            );
            return;
          }
          renderScoreboard(summary);
        }),
      );
      return jobs;
    }
    if (activeDiagSubtab === "runs") {
      jobs.push(fetchAndRenderRuns());
    } else if (activeDiagSubtab === "metrics") {
      jobs.push(
        fetchJson(`/api/diagnostics/metrics?limit=${DIAG_LIMIT}`).then((rows) => {
          lastDiagnostics.metrics = rows;
          renderDiagnostics(diagnosticsContext());
        })
      );
    } else if (activeDiagSubtab === "errors") {
      jobs.push(
        fetchJson(`/api/diagnostics/errors?limit=${DIAG_LIMIT}`).then((rows) => {
          lastDiagnostics.errors = rows;
          renderDiagnostics(diagnosticsContext());
        })
      );
    } else if (activeDiagSubtab === "incidents") {
      // ORB-10871: grouped failure incidents. Scoped to the shared dashboard
      // window so the incident count and the raw failed-event count it states
      // its denominator against are read off the same cutoff.
      const selectedWindow = getWindow();
      jobs.push(
        fetchJson(`/api/audit/incidents?since=${encodeURIComponent(selectedWindow)}&limit=${DIAG_LIMIT}`).then((payload) => {
          lastDiagnostics.incidents = payload;
          renderDiagnostics(diagnosticsContext());
        })
      );
    }

    jobs.push(
      Promise.all([
        fetchJson(`/api/diagnostics/implement_one`),
        fetchJson(`/api/tasks/completion-by-complexity`).catch((e) => {
          console.error("Failed to fetch completion-by-complexity", e);
          return { by_complexity: [] };
        }),
      ])
        .then(([implOne, completion]) => {
          lastDiagnostics.implement_one = implOne.implement_one_by_actor || [];
          lastDiagnostics.implement_one_by_complexity = implOne.implement_one_by_complexity || [];
          lastDiagnostics.completion_by_complexity = completion.by_complexity || [];
          if ($("diagnostics-side-panel")) {
            renderDiagnostics(diagnosticsContext());
          }
        })
        .catch(e => console.error("Failed to fetch implement_one metrics", e))
    );
  }
  return jobs;
}

function fetchAndRenderRuns() {
  const requestedWorkspace = getWorkspace();
  const requestedAggregate = isAggregateView();
  const runFilter = getRunFilter();
  const scopeIsCurrent = () =>
    requestedWorkspace === getWorkspace() &&
    requestedAggregate === isAggregateView() &&
    runFilter === getRunFilter();
  const request = requestedAggregate
    ? fetchJson(`/api/job-runs/all?limit=${JOB_RUN_LIMIT}&state=${encodeURIComponent(runFilter)}`).then((payload) => ({
        runs: listItems(payload),
        frictionRows: [],
        meta: payload,
        unavailable: Array.isArray(payload && payload.unavailable) ? payload.unavailable : [],
      }))
    : Promise.all([
        fetchJson(`/api/job-runs?limit=${JOB_RUN_LIMIT}`),
        fetchJson(`/api/diagnostics/friction?limit=${DIAG_LIMIT}`),
      ]).then(([runs, frictionRows]) => ({ runs, frictionRows, meta: null, unavailable: [] }));

  return request.then(({ runs, frictionRows, meta, unavailable }) => {
    if (!scopeIsCurrent()) return;
    lastRuns = mergeRunsWithFriction(runs, frictionRows);
    lastRunsMeta = meta;
    lastRunSourcesUnavailable = unavailable;
    renderRuns(lastRuns);
  }).catch((error) => {
    if (scopeIsCurrent()) {
      const workspace = dashboardWorkspaces.find((entry) => entry.id === requestedWorkspace);
      lastRuns = [];
      lastRunsMeta = null;
      lastRunSourcesUnavailable = [{
        workspace_id: requestedWorkspace,
        workspace_name: requestedAggregate ? "All workspaces" : (workspace && workspace.name) || requestedWorkspace || "workspace",
        error: error.message || "run query failed",
      }];
      renderRuns(lastRuns);
    }
    throw error;
  });
}

function fetchAndRenderTaskLocks() {
  return fetchJson("/api/tasks/locks").then(renderLocksPanel);
}

function fetchAndRenderRunDetail() {
  if (!getActiveRunId()) return Promise.resolve();
  return fetchJson(`/api/runs/${encodeURIComponent(getActiveRunId())}`).then((data) => {
    setActiveRunDetail(data);
    renderRunDetailMeta();
    renderRunKnowledge();
    renderRunGantt();
    renderRunSteps();
  }).catch((e) => {
    renderRunDetailEmpty(`Run not found: ${getActiveRunId()}`);
    throw e;
  });
}

function fetchAndRenderRunEvents() {
  if (!getActiveRunId()) return Promise.resolve();
  return fetchJson(`/api/runs/${encodeURIComponent(getActiveRunId())}/events?limit=${RUN_EVENTS_LIMIT}`).then((events) => {
    setActiveRunEvents(events);
    renderRunEvents();
    renderRunGantt();
  }).catch(() => {
    // Missing v2 events file is non-fatal — run detail still renders.
    setActiveRunEvents([]);
    renderRunGantt();
  });
}

function fetchAndRenderRunLogs() {
  if (!getActiveRunId()) return Promise.resolve();
  return fetchJson(`/api/runs/${encodeURIComponent(getActiveRunId())}/logs?limit=${RUN_EVENTS_LIMIT}`).then((logs) => {
    setActiveRunLogs(logs);
    renderRunSteps();
  }).catch(() => {
    setActiveRunLogs([]);
    renderRunSteps();
  });
}

function fetchAndRenderSummary() {
  return fetchJson(`/api/audit/summary?since=24h`).then((data) => {
    lastSummary = data;
    renderHealthStrip(data);
    renderAuditSummary(data, auditContext());
  });
}

function fetchAndRenderFrictions() {
  // ORB-00040: /api/frictions and /api/frictions/stats are per-workspace; skip
  // both in aggregate mode.
  // ORB-00044: also replace the stale detail panel (live resolve/status/tags).
  if (isAggregateView()) {
    renderPanelPlaceholder("frictions-body");
    renderKnowledgeDetailPlaceholder("friction");
    return Promise.resolve();
  }
  const statuses = frictionStatusFilter === "active" ? ["open", "triaged"] : [frictionStatusFilter];
  const listRequests = statuses.map((status) => {
    const sp = new URLSearchParams();
    sp.set("limit", String(FRICTION_LIMIT));
    if (status !== "all") sp.set("status", status);
    if (frictionSearchQuery) sp.set("q", frictionSearchQuery);
    return fetchJson(`/api/frictions?${sp.toString()}`);
  });
  return Promise.all([
    Promise.all(listRequests),
    fetchJson("/api/frictions/stats"),
  ]).then(([payloads, stats]) => {
    const items = payloads
      .flatMap((payload) => Array.isArray(payload && payload.items) ? payload.items : [])
      .sort((a, b) => {
        const byCreatedAt = Date.parse(b.created_at || "") - Date.parse(a.created_at || "");
        return Number.isNaN(byCreatedAt) || byCreatedAt === 0
          ? String(b.id || "").localeCompare(String(a.id || ""))
          : byCreatedAt;
      })
      .slice(0, FRICTION_LIMIT);
    const tags = [...new Set(payloads.flatMap((payload) => Array.isArray(payload && payload.tags) ? payload.tags : []))].sort();
    lastFrictionPayload = { stats: stats || {}, tags, items };
    renderFrictions(lastFrictionPayload);
  });
}

// ORB-10972: sets a count badge on a rail entry. `alert` renders it in the
// blocked colour so a failure count is legible from any tab without opening
// the tab it belongs to. Every value here comes from a fetch the dashboard
// already makes — this adds no endpoint.
function setRailCount(id, value, alert = false) {
  const node = $(id);
  if (!node) return;
  const empty = value == null || value === 0;
  node.textContent = empty ? "" : formatBigInt(value);
  node.classList.toggle("alert", Boolean(alert) && !empty);
}

function renderHealthStrip(data) {
  if (!data) return;
  $("tile-events-value").textContent = formatBigInt(data.events);
  $("tile-denials-value").textContent = formatBigInt(data.denials);
  $("tile-failed-value").textContent = formatBigInt(data.failed_runs);
  $("tile-active-value").textContent = formatBigInt(data.active_long_runs);
  const tile = $("tile-denials");
  const threshold = data.denial_threshold ?? 10;
  if (data.denials > threshold) {
    tile.classList.add("tile-alert");
  } else {
    tile.classList.remove("tile-alert");
  }
  const failed = $("tile-failed");
  if (failed) failed.classList.toggle("tile-alert", (data.failed_runs || 0) > 0);

  setRailCount("rail-count-audit", data.events);
  setRailCount("rail-count-diagnostics", data.failed_runs, true);
  setRailCount("rail-count-diag-errors", data.failed_runs, true);
  renderSparkline(data.sparkline || []);
}

function formatBigInt(n) {
  if (n == null) return "-";
  if (n >= 10000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

function renderSparkline(buckets) {
  const svg = $("tile-events-sparkline");
  if (!svg) return;
  while (svg.firstChild) svg.removeChild(svg.firstChild);
  if (buckets.length === 0) return;
  const counts = buckets.map((b) => b.count || 0);
  const max = Math.max(1, ...counts);
  const w = 100;
  const h = 22;
  const stepX = buckets.length > 1 ? w / (buckets.length - 1) : 0;
  const points = counts.map((c, i) => {
    const x = i * stepX;
    const y = h - (c / max) * (h - 2) - 1;
    return `${x.toFixed(2)},${y.toFixed(2)}`;
  });
  const baseline = document.createElementNS("http://www.w3.org/2000/svg", "line");
  baseline.setAttribute("x1", "0");
  baseline.setAttribute("y1", String(h - 0.5));
  baseline.setAttribute("x2", String(w));
  baseline.setAttribute("y2", String(h - 0.5));
  baseline.setAttribute("class", "baseline");
  svg.appendChild(baseline);
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", `M${points.join(" L")}`);
  svg.appendChild(path);
}

function refreshLabel() {
  if (activeTab === "diagnostics") return `diagnostics/${activeDiagSubtab}`;
  if (activeTab === "run-detail") return `run/${getActiveRunId() || "?"}`;
  return activeTab;
}




async function refreshDashboard() {
  if (isRefreshing) return;
  isRefreshing = true;
  const now = new Date();
  $("meta-text").textContent = `fetching...`;
  $("conn-status").className = "status-dot orange";
  const btn = $("refresh-btn");
  if (btn) btn.disabled = true;
  
  let hasErrors = false;
  
  await Promise.all(
    activeRefreshJobs().map((job) =>
      job.catch((e) => {
        hasErrors = true;
        console.error(e);
      }),
    ),
  );
  
  if (hasErrors) {
    $("conn-status").className = "status-dot red";
    $("meta-text").textContent = `offline · ${now.toLocaleTimeString()}`;
  } else {
    $("conn-status").className = "status-dot green";
    $("meta-text").textContent = `refreshed ${refreshLabel()} · ${now.toLocaleTimeString()}`;
  }
  if (btn) btn.disabled = false;
  isRefreshing = false;
  if (activeTab === "tasks") fitLogPanelToViewport();
}

const tasksContext = taskContext();
buildChips(tasksContext);
wireSearch(tasksContext);
wireFrictionSearch();
wireFrictionStatusFilter();
wireFrictionResponsiveDetail();
wireGlobalTaskResolver();
buildAuditChips(auditContext());
wireAuditSearch(auditContext());
$("refresh-btn").addEventListener("click", refreshDashboard);
wireReliabilityWindowSelector();
setScopeChangeListener(() => {
  persistScopeToUrl();
  syncWindowSelectors();
  if (activeTab === "diagnostics") {
    const url = new URL(window.location.href);
    url.hash = `diagnostics/${activeDiagSubtab}?window=${encodeURIComponent(getWindow())}`;
    if (url.href !== window.location.href) history.replaceState(null, "", url);
  } else if (activeTab === "audit") {
    const next = buildAuditHash();
    const url = new URL(window.location.href);
    url.hash = next.replace(/^#/, "");
    if (url.href !== window.location.href) history.replaceState(null, "", url);
  }
  refreshDashboard();
});
initOperations({ getWorkspaces: () => dashboardWorkspaces, formatAbsoluteTime: fmtAbsTime });

initRuns(runsContext());
initRunDetail(runDetailContext());
const rctx = routerContext();
initRouter(rctx);
// Resolve workspaces before the router fires its first refresh so the initial
// fetches carry the right workspace (top-level await; app.js is an ES module).
await initWorkspaceSelector();
persistScopeToUrl();
syncWindowSelectors();
iT();

initLogTail();
