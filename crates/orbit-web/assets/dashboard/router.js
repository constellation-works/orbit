// Orbit dashboard router (hash routing + top-tab + subtab wiring).
// Pure vanilla JS, split into ES module with no build step.
//
// Moved from app.js (router was the widest cross-cut for subsequent module splits).
// Owns: TABS, DIAG_SUBTABS, RUN_DETAIL_SUBTABS, KNOWLEDGE_SUBTABS, parseHashRoute,
// setActiveTab, set*Subtab helpers, initTabs, navigateToRun.
//
// All mutable dashboard state remains in app.js. Router receives a context object
// (from app.js routerContext()) providing getters/setters + callbacks for renders,
// refresh, log fit, and audit pass-throughs. This avoids circular imports and keeps
// the "state lives in app" contract.
//
// The public exports (setActiveTab, navigateToRun, initTabs) are thin wrappers over
// impls that close over the injected ctx (set once via initRouter).
// No behavior change: every prior hash route, subtab click, back/forward, and the
// 30s setInterval(refreshDashboard) continue to work identically.
//
// Also exports parseHashRoute for symmetry (used only internally today).

import { el, isAggregateView, renderPanelPlaceholder, getWindow, setWindow, setWorkspace, parseDashboardWindow, persistScopeToUrl, syncWindowSelectors } from './common.js';
import { renderRuns } from './runs.js';

const $ = (id) => document.getElementById(id);

// ORB-10872: Reliability is fleet-wide. While that subtab is showing, the
// header workspace selector must not look like it scopes the visible panel.
function markWorkspaceSelectorScope(fleetWide) {
  const select = $("workspace-select");
  if (select) {
    select.classList.toggle("scope-ignored", fleetWide);
    select.title = fleetWide
      ? "Reliability is Fleet-wide; workspace does not apply"
      : "Workspace";
  }
  const note = $("workspace-scope-note");
  if (note) note.hidden = !fleetWide;
}

// ORB-10444: the top-level nav is exactly these four tabs plus the hash-only
// `run-detail` route. A deprecated tab was retired outright and `scoreboard`,
// being diagnostics-shaped, now routes as `#diagnostics/scoreboard`.
const TABS = ["tasks", "audit", "diagnostics", "operations", "knowledge", "run-detail"];
const DIAG_SUBTABS = ["runs", "metrics", "errors", "incidents", "reliability", "scoreboard"];
const OPERATIONS_SUBTABS = ["routines", "auto-tasks", "auto-drain"];
// ORB-10444/ORB-10588: subtabs that replace the two-column diagnostics layout
// with their own full-width <main>, keyed by the element they reveal.
const DIAG_FULL_WIDTH_MAINS = {
  scoreboard: "diagnostics-scoreboard-main",
  reliability: "diagnostics-reliability-main",
};
const RUN_DETAIL_SUBTABS = ["steps", "events"];
const KNOWLEDGE_SUBTABS = ["frictions"];

function parseHashRoute(raw) {
  const trimmed = String(raw || "").replace(/^#/, "");
  const queryIdx = trimmed.indexOf("?");
  const path = queryIdx >= 0 ? trimmed.slice(0, queryIdx) : trimmed;
  const queryStr = queryIdx >= 0 ? trimmed.slice(queryIdx + 1) : "";
  const segments = path.split("/").filter(Boolean);
  const query = new URLSearchParams(queryStr);
  return { segments, query };
}

let _routerCtx = null;

function getCtx() {
  if (!_routerCtx) {
    throw new Error("router not initialized; call initRouter(routerContext()) before using router APIs");
  }
  return _routerCtx;
}

function setRunDetailSubtabImpl(ctx, name) {
  if (!RUN_DETAIL_SUBTABS.includes(name)) name = "steps";
  ctx.setRunSubtab(name);
  for (const btn of document.querySelectorAll("#run-detail-subtabs .subtab")) {
    btn.classList.toggle("active", btn.dataset.subtab === name);
  }
  $("run-steps-body").style.display = name === "steps" ? "block" : "none";
  $("run-events-body").style.display = name === "events" ? "block" : "none";
}

function setDiagSubtabImpl(ctx, name) {
  if (!DIAG_SUBTABS.includes(name)) name = "runs";
  ctx.setDiagSubtab(name);
  for (const btn of document.querySelectorAll("#diag-subtabs .subtab")) {
    btn.classList.toggle("active", btn.dataset.subtab === name);
  }

  const subIndicator = $("subtab-indicator") || el("div", {id: "subtab-indicator", class: "tab-indicator"});
  if (!subIndicator.parentNode) document.querySelector("#diag-subtabs").appendChild(subIndicator);
  const activeBtn = document.querySelector(`.subtab[data-subtab="${name}"]`);
  if (activeBtn) {
    subIndicator.style.width = `${activeBtn.offsetWidth}px`;
    subIndicator.style.left = `${activeBtn.offsetLeft}px`;
  }

  // ORB-10444/ORB-10588: Scoreboard and Reliability are diagnostics subtabs
  // that render into their own full-width <main>, so the two-column
  // diagnostics layout (list + summary side card) is swapped out for them
  // while the subtab nav stays reachable.
  const fullWidthMain = DIAG_FULL_WIDTH_MAINS[name];
  const sideCol = $("diagnostics-side-col");
  const diagMain = $("diagnostics-main");
  for (const [subtab, mainId] of Object.entries(DIAG_FULL_WIDTH_MAINS)) {
    const node = $(mainId);
    if (node) node.style.display = subtab === name ? "grid" : "none";
  }
  if (sideCol) sideCol.style.display = fullWidthMain ? "none" : "flex";
  if (diagMain) {
    diagMain.style.gridTemplateColumns = fullWidthMain
      ? "minmax(0, 1fr)"
      : "minmax(0, 2fr) minmax(280px, 1.15fr)";
  }
  document.body.classList.toggle("reliability-active", name === "reliability");
  markWorkspaceSelectorScope(name === "reliability");
  if (fullWidthMain) {
    $("diag-body").style.display = "none";
    $("runs-body").style.display = "none";
    // Reliability aggregates across workspaces server-side, so it stays live
    // in the aggregate view; the scoreboard is per-workspace and does not.
    if (name === "scoreboard" && isAggregateView()) renderPanelPlaceholder("scoreboard-body");
    if (name === "reliability" && ctx.fetchReliability) {
      ctx.fetchReliability();
    }
    return;
  }

  // Runs remains live in the aggregate view. Other per-workspace diagnostics
  // must keep their placeholder and never repaint a prior workspace's rows.
  if (name === "runs") {
    $("diag-body").style.display = "none";
    $("runs-body").style.display = "block";
    renderRuns(ctx.getLastRuns ? ctx.getLastRuns() : []);
  } else {
    $("diag-body").style.display = "block";
    $("runs-body").style.display = "none";
    if (isAggregateView()) {
      renderPanelPlaceholder("diag-body");
    } else {
      ctx.renderDiagnostics();
    }
  }
}

function setOperationsSubtabImpl(ctx, name) {
  if (!OPERATIONS_SUBTABS.includes(name)) name = "routines";
  ctx.setOperationsSubtab(name);
  for (const btn of document.querySelectorAll("#operations-subtabs .subtab")) {
    btn.classList.toggle("active", btn.dataset.subtab === name);
  }
  const routines = $("operations-routines-main");
  const autoTasks = $("operations-auto-tasks-main");
  const autoDrain = $("operations-auto-drain-main");
  if (routines) routines.hidden = name !== "routines";
  if (autoTasks) autoTasks.hidden = name !== "auto-tasks";
  if (autoDrain) autoDrain.hidden = name !== "auto-drain";
}

function setKnowledgeSubtabImpl(ctx, name) {
  if (!KNOWLEDGE_SUBTABS.includes(name)) name = "frictions";
  ctx.setKnowledgeSubtab(name);
  for (const btn of document.querySelectorAll("#knowledge-subtabs .subtab")) {
    btn.classList.toggle("active", btn.dataset.subtab === name);
  }
  const toggle = (id, show) => {
    const node = $(id);
    if (node) node.style.display = show ? "" : "none";
  };
  toggle("friction-stats", true);
  toggle("friction-search", true);
  toggle("frictions-body", true);
  toggle("friction-detail-panel", true);
}

function setActiveTabImpl(ctx, raw, opts = {}) {
  const { segments, query } = parseHashRoute(raw);
  const head = segments[0] || "tasks";
  if (head === "runs" && !segments[1] && query.get("run_id")) {
    segments[1] = encodeURIComponent(query.get("run_id"));
  }
  let top;
  if (head === "runs" && segments[1]) {
    top = "run-detail";
    const nextRunId = decodeURIComponent(segments[1]);
    if (ctx.getRunId() !== nextRunId) {
      ctx.setRunLogs([]);
      const esi = ctx.getExpandedSteps();
      if (esi && typeof esi.clear === "function") esi.clear();
      else ctx.setExpandedSteps(new Set());
    }
    ctx.setRunId(nextRunId);
    const expandStep = query.get("step");
    if (expandStep != null && /^\d+$/.test(expandStep)) {
      const esi = ctx.getExpandedSteps();
      if (esi && typeof esi.add === "function") esi.add(Number(expandStep));
      else {
        const s = new Set(esi || []);
        s.add(Number(expandStep));
        ctx.setExpandedSteps(s);
      }
    }
    const sub = RUN_DETAIL_SUBTABS.includes(segments[2]) ? segments[2] : ctx.getRunSubtab();
    ctx.setRunSubtab(sub);
  } else if (TABS.includes(head)) {
    top = head;
  } else {
    top = "tasks";
  }
  ctx.setTab(top);
  // ORB-10972: the topbar breadcrumb names the active destination, since the
  // rail no longer sits above the content where the tab strip used to.
  const crumb = $("topbar-crumb");
  if (crumb) {
    crumb.textContent = top === "run-detail"
      ? "Run detail"
      : top.charAt(0).toUpperCase() + top.slice(1);
  }
  document.body.classList.toggle("operations-active", top === "operations");
  // ORB-10972: the Diagnostics subtabs are permanently visible in the rail now,
  // so the remembered-subtab highlight must be muted while another destination
  // is active — otherwise the rail shows two things selected at once. The
  // `.active` class itself is left alone; it is still the remembered choice.
  const diagSubtabs = $("diag-subtabs");
  if (diagSubtabs) diagSubtabs.classList.toggle("dimmed", top !== "diagnostics");
  if (top !== "diagnostics") {
    document.body.classList.remove("reliability-active");
    markWorkspaceSelectorScope(false);
  }
  for (const tab of document.querySelectorAll(".tab")) {
    tab.classList.toggle("active", tab.dataset.tab === top);
  }
  for (const pane of document.querySelectorAll(".tab-pane")) {
    pane.classList.toggle("active", pane.dataset.tab === top);
  }
  if (top === "tasks") requestAnimationFrame(ctx.fitLogPanelToViewport || (() => {}));

  const indicator = $("tab-indicator") || el("div", {id: "tab-indicator", class: "tab-indicator"});
  if (!indicator.parentNode) document.querySelector(".tabs").appendChild(indicator);
  // For run-detail (no top tab button), hide the indicator
  const activeTabEl = document.querySelector(`.tab[data-tab="${top}"]`);
  if (activeTabEl) {
    indicator.style.display = "";
    indicator.style.width = `${activeTabEl.offsetWidth}px`;
    indicator.style.left = `${activeTabEl.offsetLeft}px`;
  } else {
    indicator.style.display = "none";
  }

  let hash;
  if (top === "diagnostics") {
    const sub = DIAG_SUBTABS.includes(segments[1]) ? segments[1] : ctx.getDiagSubtab();
    const hashedWindow = parseDashboardWindow(query.get("window"));
    if (hashedWindow && hashedWindow !== getWindow()) {
      setWindow(hashedWindow);
      persistScopeToUrl();
      syncWindowSelectors();
    }
    setDiagSubtabImpl(ctx, sub);
    hash = `#diagnostics/${sub}?window=${encodeURIComponent(getWindow())}`;
  } else if (top === "audit") {
    ctx.applyAuditHashQuery(query);
    const sub = ["events", "policy"].includes(segments[1]) ? segments[1] : ctx.getActiveAuditSubtab();
    ctx.setAuditSubtab(sub);
    hash = ctx.buildAuditHash();
    ctx.syncAuditControls();
  } else if (top === "run-detail") {
    setRunDetailSubtabImpl(ctx, ctx.getRunSubtab());
    hash = `#runs/${encodeURIComponent(ctx.getRunId() || "")}` +
      (ctx.getRunSubtab() !== "steps" ? `/${ctx.getRunSubtab()}` : "");
    if (query.get("step") != null) hash += `?step=${encodeURIComponent(query.get("step"))}`;
  } else if (top === "knowledge") {
    const sub = KNOWLEDGE_SUBTABS.includes(segments[1]) ? segments[1] : ctx.getKnowledgeSubtab();
    setKnowledgeSubtabImpl(ctx, sub);
    hash = `#knowledge/${sub}`;
  } else if (top === "operations") {
    const sub = OPERATIONS_SUBTABS.includes(segments[1]) ? segments[1] : ctx.getOperationsSubtab();
    setOperationsSubtabImpl(ctx, sub);
    hash = `#operations/${sub}`;
  } else if (top === "tasks") {
    // ORB-10874: the status chips and search box are represented in the hash
    // (mirroring the audit tab) so they survive a reload or the browser's
    // back/forward navigation instead of resetting to the in-memory default.
    if (ctx.applyTasksHashQuery) ctx.applyTasksHashQuery(query);
    if (ctx.syncTaskControls) ctx.syncTaskControls();
    hash = ctx.buildTasksHash ? ctx.buildTasksHash() : "#tasks";
  } else {
    hash = `#${top}`;
  }
  const hashChanged = window.location.hash !== hash;
  const shouldUpdateHash = opts.updateHash !== false;
  if (hashChanged && shouldUpdateHash) {
    window.location.hash = hash;
  }
  if (opts.refresh !== false && (!hashChanged || !shouldUpdateHash)) ctx.refreshDashboard();
}

function navigateToRunImpl(ctx, runId, workspaceId = null) {
  if (workspaceId) {
    setWorkspace(workspaceId);
    persistScopeToUrl();
    const selector = $("workspace-select");
    if (selector) selector.value = workspaceId;
  }
  ctx.setRunId(runId);
  ctx.setExpandedSteps(new Set());
  ctx.setRunDetail(null);
  ctx.setRunEvents([]);
  setActiveTabImpl(ctx, `runs/${encodeURIComponent(runId)}`);
}

function initTabsImpl(ctx) {
  for (const tab of document.querySelectorAll(".tab")) {
    tab.addEventListener("click", () => setActiveTabImpl(ctx, tab.dataset.tab, { refresh: false }));
  }
  for (const btn of document.querySelectorAll("#diag-subtabs .subtab")) {
    btn.addEventListener("click", () =>
      setActiveTabImpl(ctx, `diagnostics/${btn.dataset.subtab}`, { refresh: false }),
    );
  }
  for (const btn of document.querySelectorAll("#run-detail-subtabs .subtab")) {
    btn.addEventListener("click", () => {
      ctx.setRunSubtab(btn.dataset.subtab);
      const path = `runs/${encodeURIComponent(ctx.getRunId() || "")}` +
        (ctx.getRunSubtab() !== "steps" ? `/${ctx.getRunSubtab()}` : "");
      setActiveTabImpl(ctx, path, { refresh: false });
      ctx.refreshDashboard();
    });
  }
  for (const btn of document.querySelectorAll("#audit-subtabs .subtab")) {
    btn.addEventListener("click", () => {
      ctx.setActiveAuditSubtabFromButton(btn.dataset.subtab);
      const newHash = ctx.buildAuditHash();
      if (window.location.hash !== newHash) {
        window.location.hash = newHash;
      } else {
        ctx.refreshDashboard();
      }
    });
  }
  for (const btn of document.querySelectorAll("#knowledge-subtabs .subtab")) {
    btn.addEventListener("click", () =>
      setActiveTabImpl(ctx, `knowledge/${btn.dataset.subtab}`, { refresh: false }),
    );
  }
  for (const btn of document.querySelectorAll("#operations-subtabs .subtab")) {
    btn.addEventListener("click", () =>
      setActiveTabImpl(ctx, `operations/${btn.dataset.subtab}`, { refresh: false }),
    );
  }
  window.addEventListener("hashchange", () => {
    setActiveTabImpl(ctx, window.location.hash);
  });
  setActiveTabImpl(ctx, window.location.hash || "tasks", {
    refresh: false,
    updateHash: false,
  });
  ctx.refreshDashboard();
  setInterval(ctx.refreshDashboard, 30000);
}

// Public named exports (the stable import surface for app.js and future modules).
// These are the thin call-time wrappers; real work happens in *Impl against the ctx.
export function initRouter(ctx) {
  _routerCtx = ctx;
}

export function setActiveTab(raw, opts = {}) {
  const ctx = getCtx();
  return setActiveTabImpl(ctx, raw, opts);
}

export function navigateToRun(runId, workspaceId = null) {
  const ctx = getCtx();
  return navigateToRunImpl(ctx, runId, workspaceId);
}

export function initTabs() {
  const ctx = getCtx();
  return initTabsImpl(ctx);
}

export function setRunDetailSubtab(name) {
  const ctx = getCtx();
  setRunDetailSubtabImpl(ctx, name);
}
