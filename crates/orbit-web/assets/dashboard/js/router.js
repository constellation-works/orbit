// Orbit dashboard router (hash routing + top-tab + subtab wiring).
// Pure vanilla JS, split into ES module with no build step.
//
// Moved from app.js (router was the widest cross-cut for subsequent module splits).
// Owns: TABS, the Health and Analytics subtabs and their secondary views, the
// Agents role tabs,
// RUN_DETAIL_SUBTABS, KNOWLEDGE_SUBTABS, parseHashRoute, setActiveTab, set*Subtab
// helpers, initTabs, navigateToRun.
//
// All mutable dashboard state remains in app.js. Router receives a context object
// (from app.js routerContext()) providing getters/setters + callbacks for renders,
// refresh, log fit, and audit/codebase pass-throughs. This avoids circular imports and keeps
// the "state lives in app" contract.
//
// The public exports (setActiveTab, navigateToRun, initTabs) are thin wrappers over
// impls that close over the injected ctx (set once via initRouter).
// Hash routes, subtab clicks and back/forward navigation are owned here; the
// router starts dashboard polling (polling.js) once the first route is shown.
//
// Also exports parseHashRoute for symmetry (used only internally today).

import { el, isAggregateView, renderPanelPlaceholder, getWindow, setWindow, setWorkspace, parseDashboardWindow, persistScopeToUrl, syncWindowSelectors, notifyScopeChange } from './common.js';
import { renderRuns } from './runs.js';
import { AGENT_ROLES } from './agents.js';
import { initBackgroundNotifications, setDashboardTitle, startDashboardPolling } from './polling.js';

const $ = (id) => document.getElementById(id);

// ORB-10872: Reliability › Pipeline is fleet-wide. While it is showing, the
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
  if (note) {
    note.classList.toggle("is-inactive", !fleetWide);
    note.setAttribute("aria-hidden", String(!fleetWide));
  }
}

// Drain is a Work destination. Retired auto-drain and Tasks dock links are
// normalized in place so Back never gets trapped on a redirect.
const TABS = ["tasks", "jobs", "drain", "diagnostics", "analytics", "agents", "operations", "knowledge", "plugins", "config", "run-detail"];
const DIAG_SUBTABS = ["incidents", "doctor"];
// Health › Incidents views: `#diagnostics/incidents[/events|/errors]`.
const INCIDENTS_VIEWS = ["incidents", "events", "errors"];
const ANALYTICS_SUBTABS = ["metrics", "reliability", "codebase", "resources", "cost"];
// Analytics › Reliability views: `#analytics/reliability[/tools]`.
const RELIABILITY_VIEWS = ["pipeline", "tools"];
const OPERATIONS_SUBTABS = ["routines", "auto-tasks"];
const JOBS_SUBTABS = ["live", "history", "catalogue"];
// Health views that replace the list panel with their own full-width <main>.
const HEALTH_MAINS = {
  events: "diagnostics-events-main",
  doctor: "diagnostics-doctor-main",
};
// Every Analytics view brings its own <main>, keyed by the view it shows.
const ANALYTICS_MAINS = {
  metrics: "analytics-metrics-main",
  pipeline: "analytics-reliability-main",
  tools: "analytics-tools-main",
  codebase: "analytics-codebase-main",
  resources: "analytics-resources-main",
  cost: "analytics-cost-main",
};
const RUN_DETAIL_SUBTABS = ["steps", "events"];
// Rail labels name destinations the way an operator reads them; some hashes
// underneath keep older names (`diagnostics` is Health).
const RAIL_LABELS = {
  tasks: "Tasks",
  jobs: "Jobs",
  drain: "Drain",
  diagnostics: "Health",
  analytics: "Analytics",
  agents: "Agents",
  operations: "Automation",
  knowledge: "Knowledge",
  plugins: "Plugins",
  config: "Settings",
};

/// The operator-facing name of the destination on screen.
export function destinationLabel(top) {
  return top === "run-detail" ? "Jobs › History" : RAIL_LABELS[top] || top;
}

// Rail destinations reopen their remembered view.
function railRoute(ctx, tab) {
  if (tab === "diagnostics") return healthHash(ctx).slice(1);
  if (tab === "analytics") return analyticsSubtabRoute(ctx, ctx.getAnalyticsSubtab());
  if (tab === "agents") return `agents/${ctx.getAgentsRole()}`;
  if (tab === "jobs") return `jobs/${ctx.getJobsSubtab()}`;
  return tab;
}

function incidentsRoute(ctx, view = ctx.getIncidentsView()) {
  if (view === "events") return ctx.buildAuditHash().slice(1);
  return view === "errors" ? "diagnostics/incidents/errors" : "diagnostics/incidents";
}

function analyticsSubtabRoute(ctx, sub) {
  if (!ANALYTICS_SUBTABS.includes(sub)) sub = "metrics";
  if (sub === "codebase") return ctx.buildCodebaseHash().slice(1);
  if (sub === "reliability" && ctx.getReliabilityView() === "tools") return "analytics/reliability/tools";
  return `analytics/${sub}`;
}

function windowQuery() {
  return `window=${encodeURIComponent(getWindow())}`;
}

/// The canonical hash of the Health view on screen.
function healthHash(ctx) {
  const sub = ctx.getDiagSubtab();
  if (sub !== "incidents") return `#diagnostics/${sub}?${windowQuery()}`;
  const view = ctx.getIncidentsView();
  if (view === "events") return ctx.buildAuditHash();
  return `#${incidentsRoute(ctx, view)}?${windowQuery()}`;
}

/// The canonical hash of the Agents role on screen, with the shared window.
function agentsHash(ctx) {
  return `#agents/${ctx.getAgentsRole()}?${windowQuery()}`;
}

/// The canonical hash of the Analytics view on screen. Codebase has its own
/// window, so it serializes its own selection instead of the shared window.
function analyticsHash(ctx, query) {
  const sub = ctx.getAnalyticsSubtab();
  if (sub === "codebase") return ctx.buildCodebaseHash();
  let hash = `#${analyticsSubtabRoute(ctx, sub)}?${windowQuery()}`;
  if (sub === "metrics") hash += `&outcome=${query.get("outcome") === "all" ? "all" : "succeeded"}`;
  return hash;
}

// Marks the active button of a secondary-tab bar and shows the bar only while
// its view is open.
function syncViewTabs(id, view, shown) {
  const bar = $(id);
  if (!bar) return;
  bar.hidden = !shown;
  for (const btn of bar.querySelectorAll(".subtab")) {
    const on = btn.dataset.view === view;
    btn.classList.toggle("active", on);
    btn.setAttribute("aria-current", on ? "page" : "false");
  }
}
const KNOWLEDGE_SUBTABS = ["frictions"];
// ORB-12724: `effective` is the layered view; the two `*-file` views are the
// `--scope` equivalents, and `keys` is the settable-key reference. `hosts` is
// the serving host's registered remote hosts [ORB-14451].
const CONFIG_SUBTABS = ["effective", "workspace-file", "global-file", "crews", "keys", "system", "hosts"];

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

function activePaneMain() {
  const pane = document.querySelector(".tab-pane.active");
  if (!pane) return null;
  return Array.from(pane.querySelectorAll("main")).find((main) => !main.hidden && main.style?.display !== "none") || null;
}

function syncSkipLinkTarget() {
  const link = $("skip-link");
  const pane = document.querySelector(".tab-pane.active");
  const main = activePaneMain();
  if (!link || !pane || !main) return;
  if (!main.id) {
    const mainIndex = Array.from(pane.querySelectorAll("main")).indexOf(main) + 1;
    main.id = `dashboard-main-${pane.dataset.tab}-${mainIndex}`;
  }
  main.tabIndex = -1;
  main.setAttribute("aria-labelledby", "topbar-crumb");
  link.href = `#${main.id}`;
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

const DIAG_TITLES = {
  errors: "Errors",
  incidents: "Incidents",
};

function setDiagSubtabImpl(ctx, name, view = ctx.getIncidentsView()) {
  if (!DIAG_SUBTABS.includes(name)) name = "incidents";
  if (!INCIDENTS_VIEWS.includes(view)) view = "incidents";
  ctx.setDiagSubtab(name);
  ctx.setIncidentsView(view);
  // The view on screen: Incidents shows one of its three views.
  const shown = name === "incidents" ? view : name;
  const title = $("diag-title");
  if (title && DIAG_TITLES[shown]) title.textContent = DIAG_TITLES[shown];
  for (const btn of document.querySelectorAll("#diag-subtabs .subtab")) {
    btn.classList.toggle("active", btn.dataset.subtab === name);
  }

  const subIndicator = $("subtab-indicator") || el("div", {id: "subtab-indicator", class: "tab-indicator"});
  if (!subIndicator.parentNode) document.querySelector("#diag-subtabs").appendChild(subIndicator);
  const activeBtn = document.querySelector(`#diag-subtabs .subtab[data-subtab="${name}"]`);
  if (activeBtn) {
    subIndicator.style.width = `${activeBtn.offsetWidth}px`;
    subIndicator.style.left = `${activeBtn.offsetLeft}px`;
  }
  syncViewTabs("incidents-views", view, name === "incidents");

  // Events and Doctor render into their own full-width <main>, so the list
  // panel is swapped out for them; it would otherwise sit there as an empty,
  // titled box.
  const fullWidthMain = HEALTH_MAINS[shown];
  for (const [key, mainId] of Object.entries(HEALTH_MAINS)) {
    const node = $(mainId);
    if (node) node.style.display = key === shown ? "grid" : "none";
  }
  const diagMain = $("diagnostics-main");
  if (diagMain) diagMain.style.display = fullWidthMain ? "none" : "";
  if (fullWidthMain) {
    $("diag-body").style.display = "none";
    // Doctor runs on open, not on the poll: show the cached report or read one.
    if (shown === "doctor" && ctx.openDoctor) ctx.openDoctor();
    return;
  }

  $("diag-body").style.display = "block";
  if (isAggregateView()) renderPanelPlaceholder("diag-body");
  else ctx.renderDiagnostics();
}

function setAnalyticsSubtabImpl(ctx, name, view = ctx.getReliabilityView()) {
  if (!ANALYTICS_SUBTABS.includes(name)) name = "metrics";
  if (!RELIABILITY_VIEWS.includes(view)) view = "pipeline";
  ctx.setAnalyticsSubtab(name);
  ctx.setReliabilityView(view);
  const shown = name === "reliability" ? view : name;
  for (const btn of document.querySelectorAll("#analytics-subtabs .subtab")) {
    btn.classList.toggle("active", btn.dataset.subtab === name);
  }
  syncViewTabs("reliability-views", view, name === "reliability");
  for (const [key, mainId] of Object.entries(ANALYTICS_MAINS)) {
    const node = $(mainId);
    if (node) node.style.display = key === shown ? "grid" : "none";
  }
  document.body.classList.toggle("reliability-active", shown === "pipeline");
  markWorkspaceSelectorScope(shown === "pipeline");
  // Pipeline aggregates across workspaces server-side, so it stays live in the
  // aggregate view; Cost is per-workspace and does not.
  if (name === "cost" && isAggregateView()) renderPanelPlaceholder("scoreboard-orchestration");
  // Metrics, Pipeline and Resources read as soon as they open, not on the next tick.
  if (shown === "metrics" && ctx.fetchMetrics) ctx.fetchMetrics();
  if (shown === "pipeline" && ctx.fetchReliability) ctx.fetchReliability();
  if (shown === "resources" && ctx.fetchResources) ctx.fetchResources();
}

// Agents shows one role at a time behind its secondary tabs, and reads it as
// soon as it opens rather than on the next tick.
function setAgentsRoleImpl(ctx, name) {
  if (!AGENT_ROLES.includes(name)) name = AGENT_ROLES[0];
  ctx.setAgentsRole(name);
  syncViewTabs("agents-roles", name, true);
  if (ctx.fetchAgents) ctx.fetchAgents();
}

function setOperationsSubtabImpl(ctx, name) {
  if (!OPERATIONS_SUBTABS.includes(name)) name = "routines";
  ctx.setOperationsSubtab(name);
  for (const btn of document.querySelectorAll("#operations-subtabs .subtab")) {
    btn.classList.toggle("active", btn.dataset.subtab === name);
  }
  const routines = $("operations-routines-main");
  const autoTasks = $("operations-auto-tasks-main");
  if (routines) routines.hidden = name !== "routines";
  if (autoTasks) autoTasks.hidden = name !== "auto-tasks";
}

function setJobsSubtabImpl(ctx, name) {
  if (!JOBS_SUBTABS.includes(name)) name = "live";
  ctx.setJobsSubtab(name);
  for (const btn of document.querySelectorAll("#jobs-subtabs .subtab")) {
    const active = btn.dataset.subtab === name;
    btn.classList.toggle("active", active);
    btn.setAttribute("aria-current", active ? "page" : "false");
  }
  for (const sub of JOBS_SUBTABS) {
    const main = $(`jobs-${sub}-main`);
    if (main) main.hidden = name !== sub;
  }
  if (name === "history") renderRuns(ctx.getLastRuns ? ctx.getLastRuns() : []);
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

function setConfigSubtabImpl(ctx, name) {
  if (!CONFIG_SUBTABS.includes(name)) name = "effective";
  ctx.setConfigSubtab(name);
  for (const btn of document.querySelectorAll("#config-subtabs .subtab")) {
    btn.classList.toggle("active", btn.dataset.subtab === name);
  }
}

function setActiveTabImpl(ctx, raw, opts = {}) {
  const previousTab = document.querySelector(".tab.active");
  const previousSubtab = document.querySelector(".rail-subtabs:not(.dimmed) .subtab.active");
  const { segments, query } = parseHashRoute(raw);
  let head = segments[0] || "tasks";
  let legacyRoute = false;
  const dockView = segments[1] || query.get("dock") || query.get("dockMode") || query.get("panel");
  if (head === "auto-drain" || (head === "operations" && segments[1] === "auto-drain") ||
      (head === "tasks" && ["drain", "auto-drain", "log"].includes(dockView))) {
    head = "drain";
    segments.splice(1, segments.length, dockView === "log" || segments[2] === "log" ? "log" : "drain");
    legacyRoute = true;
  }
  // Analytics › Orchestrators became the Agents destination's default role.
  if (head === "analytics" && segments[1] === "orchestrators") {
    head = "agents";
    segments.splice(0, segments.length, "agents", "orchestrators");
    legacyRoute = true;
  }
  if (head === "runs" && !segments[1] && query.get("run_id")) {
    segments[1] = encodeURIComponent(query.get("run_id"));
  }
  let top;
  if (head === "runs" && segments[1]) {
    let nextRunId;
    try {
      nextRunId = decodeURIComponent(segments[1]);
    } catch {
      // Bookmarks and pasted hashes are untrusted input. A malformed escape
      // must not interrupt initialization or strand the navigation listener.
      return setActiveTabImpl(ctx, "jobs/history", { ...opts, repairRoute: true });
    }
    top = "run-detail";
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
  const railKey = top === "run-detail" ? "jobs" : top;
  const crumb = $("topbar-crumb");
  const destination = destinationLabel(top);
  if (crumb) crumb.textContent = destination;
  // Browser tabs and history entries read as this page's title, so name the
  // destination there too instead of every entry saying "orbit".
  setDashboardTitle(`${destination} · orbit`);
  // Config uses the shell's bounded pane so its inspector can stick while
  // the key list scrolls. Only Operations needs a document-height layout.
  document.body.classList.toggle("operations-active", top === "operations");
  // ORB-10972: a destination's subtabs show in the rail only while it is
  // open, so the remembered-subtab highlight never reads as a second
  // selection. The `.active` class itself is left alone; it is still the
  // remembered choice.
  const diagSubtabs = $("diag-subtabs");
  if (diagSubtabs) diagSubtabs.classList.toggle("dimmed", railKey !== "diagnostics");
  const analyticsSubtabs = $("analytics-subtabs");
  if (analyticsSubtabs) analyticsSubtabs.classList.toggle("dimmed", top !== "analytics");
  // The Operations subtabs live in the rail the same way.
  const operationsSubtabs = $("operations-subtabs");
  if (operationsSubtabs) operationsSubtabs.classList.toggle("dimmed", top !== "operations");
  const configSubtabs = $("config-subtabs");
  if (configSubtabs) configSubtabs.classList.toggle("dimmed", top !== "config");
  if (top !== "analytics") {
    document.body.classList.remove("reliability-active");
    markWorkspaceSelectorScope(false);
  }
  for (const tab of document.querySelectorAll(".tab")) {
    const on = tab.dataset.tab === railKey;
    tab.classList.toggle("active", on);
    tab.setAttribute("aria-current", on ? "page" : "false");
  }
  for (const pane of document.querySelectorAll(".tab-pane")) {
    pane.classList.toggle("active", pane.dataset.tab === top);
  }
  if (ctx.placeDock) ctx.placeDock(top);
  if (top === "drain" || top === "tasks") requestAnimationFrame(ctx.fitLogPanelToViewport || (() => {}));

  const indicator = $("tab-indicator") || el("div", {id: "tab-indicator", class: "tab-indicator"});
  if (!indicator.parentNode) document.querySelector(".tabs").appendChild(indicator);
  // For run-detail (no top tab button), hide the indicator
  const activeTabEl = document.querySelector(`.tab[data-tab="${railKey}"]`);
  if (activeTabEl) {
    indicator.style.display = "";
    indicator.style.width = `${activeTabEl.offsetWidth}px`;
    indicator.style.left = `${activeTabEl.offsetLeft}px`;
    document.querySelector(".tabs").style.setProperty("--active-tab-width", `${activeTabEl.offsetWidth}px`);
  } else {
    indicator.style.display = "none";
  }

  // The shared window rides in the hash of Jobs History, Health, Analytics and
  // Agents, except Events and Codebase, whose queries carry their own windows.
  const adoptHashedWindow = () => {
    const hashedWindow = parseDashboardWindow(query.get("window"));
    if (hashedWindow && hashedWindow !== getWindow()) {
      setWindow(hashedWindow);
      persistScopeToUrl();
      syncWindowSelectors();
    }
  };
  let hash;
  if (top === "drain") {
    const mode = ["log", "start"].includes(segments[1]) ? segments[1] : "drain";
    if (ctx.setDrainView) ctx.setDrainView(mode);
    hash = mode === "drain" ? "#drain" : `#drain/${mode}`;
  } else if (top === "jobs") {
    const sub = JOBS_SUBTABS.includes(segments[1]) ? segments[1] : ctx.getJobsSubtab();
    adoptHashedWindow();
    setJobsSubtabImpl(ctx, sub);
    hash = `#jobs/${sub}` + (sub === "history" ? `?${windowQuery()}` : "");
  } else if (top === "diagnostics") {
    const sub = DIAG_SUBTABS.includes(segments[1]) ? segments[1] : ctx.getDiagSubtab();
    // `#diagnostics/incidents` is the Incidents view itself; the other views
    // name themselves. A bare `#diagnostics` keeps the remembered view.
    const view = sub !== "incidents" ? ctx.getIncidentsView()
      : !segments[1] ? ctx.getIncidentsView()
      : INCIDENTS_VIEWS.includes(segments[2]) ? segments[2] : "incidents";
    if (sub === "incidents" && view === "events") {
      if (segments[1]) ctx.applyAuditHashQuery(query);
      ctx.syncAuditControls();
    } else {
      adoptHashedWindow();
    }
    setDiagSubtabImpl(ctx, sub, view);
    hash = healthHash(ctx);
  } else if (top === "analytics") {
    const sub = ANALYTICS_SUBTABS.includes(segments[1]) ? segments[1] : ctx.getAnalyticsSubtab();
    const view = sub !== "reliability" || !segments[1] ? ctx.getReliabilityView()
      : segments[2] === "tools" ? "tools" : "pipeline";
    if (sub === "codebase") {
      if (segments[1]) ctx.applyCodebaseHashQuery(query);
    } else {
      adoptHashedWindow();
    }
    setAnalyticsSubtabImpl(ctx, sub, view);
    hash = analyticsHash(ctx, query);
  } else if (top === "agents") {
    const role = AGENT_ROLES.includes(segments[1]) ? segments[1] : ctx.getAgentsRole();
    adoptHashedWindow();
    setAgentsRoleImpl(ctx, role);
    hash = agentsHash(ctx);
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
  } else if (top === "config") {
    const sub = CONFIG_SUBTABS.includes(segments[1]) ? segments[1] : ctx.getConfigSubtab();
    setConfigSubtabImpl(ctx, sub);
    hash = `#config/${sub}`;
  } else if (top === "tasks") {
    // ORB-10874: the status chips and search box are represented in the hash
    // (mirroring the audit tab) so they survive a reload or the browser's
    // back/forward navigation instead of resetting to the in-memory default.
    // `#tasks/<id>` is one task's full-page view; a malformed id falls back
    // to the list.
    let page = null;
    try {
      page = segments[1] ? decodeURIComponent(segments[1]) : null;
    } catch {
      page = null;
    }
    if (ctx.applyTasksHashQuery) ctx.applyTasksHashQuery(query, page);
    if (ctx.syncTaskControls) ctx.syncTaskControls();
    hash = ctx.buildTasksHash ? ctx.buildTasksHash() : "#tasks";
  } else {
    hash = `#${top}`;
  }
  syncSkipLinkTarget();
  // Reveal the current destination on initial deep links and history changes,
  // as well as clicks. Phone subtabs scroll within the space beside their tab.
  // A repeated hash normalization must not undo intervening keyboard scrolling.
  const activeSubtab = document.querySelector(".rail-subtabs:not(.dimmed) .subtab.active");
  if (activeTabEl !== previousTab || activeSubtab !== previousSubtab) {
    activeSubtab?.scrollIntoView({ block: "nearest", inline: "nearest" });
    activeTabEl?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }
  const hashChanged = window.location.hash !== hash;
  const shouldUpdateHash = opts.updateHash !== false;
  // A bookmarked legacy hash is rewritten in place so the address bar and any
  // copied link name the current route, without a second hashchange — and so
  // Back does not land on the legacy hash and redirect forward again.
  // A task link (`#tasks?open=ID`) is normalized the same way: pushing the
  // full filter hash after it would leave two entries, and Back from the second
  // would land on the first and push again. So is a task page route the list
  // replaced (a malformed id).
  const taskLink = top === "tasks" && (query.has("open") || segments.length > 1);
  const rewriteInPlace = hashChanged && (legacyRoute || opts.repairRoute || taskLink) && typeof window.history?.replaceState === "function";
  if (rewriteInPlace) {
    window.history.replaceState(null, "", hash);
  } else if (hashChanged && shouldUpdateHash) {
    window.location.hash = hash;
  }
  if (opts.refresh !== false && (!hashChanged || !shouldUpdateHash || rewriteInPlace)) ctx.refreshDashboard();
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
  $("skip-link")?.addEventListener("click", (event) => {
    const main = activePaneMain();
    if (!main) return;
    event.preventDefault();
    main.focus();
  });
  // Native focus scrolling can leave half a button outside the phone's
  // overflow row when its centre is already visible. Reveal the whole entry
  // on keyboard focus, without changing the route or moving focus elsewhere.
  $("tabs")?.addEventListener("focusin", (event) => {
    if (event.target.matches('.tab, .subtab')) {
      event.target.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    }
  });
  for (const tab of document.querySelectorAll(".tab")) {
    tab.addEventListener("click", (event) => {
      if (tab.dataset.tab === "jobs" && event.target?.closest?.("#rail-count-jobs")) {
        setActiveTabImpl(ctx, "jobs/live", { refresh: false });
        return;
      }
      // The denial badge opens Health › Incidents › Events filtered to
      // denials for the window it counted; the Health label keeps the view
      // already on screen.
      const denials = event.target && event.target.closest && event.target.closest("#rail-count-denials");
      if (denials && denials.dataset.route && tab.dataset.tab === "diagnostics") {
        setActiveTabImpl(ctx, denials.dataset.route, { refresh: false });
        return;
      }
      setActiveTabImpl(ctx, railRoute(ctx, tab.dataset.tab), { refresh: false });
    });
  }
  for (const btn of document.querySelectorAll("#diag-subtabs .subtab")) {
    btn.addEventListener("click", () => {
      const route = btn.dataset.subtab === "incidents" ? incidentsRoute(ctx) : `diagnostics/${btn.dataset.subtab}`;
      setActiveTabImpl(ctx, route, { refresh: false });
    });
  }
  for (const btn of document.querySelectorAll("#incidents-views .subtab")) {
    btn.addEventListener("click", () =>
      setActiveTabImpl(ctx, incidentsRoute(ctx, btn.dataset.view), { refresh: false }),
    );
  }
  for (const btn of document.querySelectorAll("#analytics-subtabs .subtab")) {
    btn.addEventListener("click", () =>
      setActiveTabImpl(ctx, analyticsSubtabRoute(ctx, btn.dataset.subtab), { refresh: false }),
    );
  }
  for (const btn of document.querySelectorAll("#agents-roles .subtab")) {
    btn.addEventListener("click", () =>
      setActiveTabImpl(ctx, `agents/${btn.dataset.view}`, { refresh: false }),
    );
  }
  for (const btn of document.querySelectorAll("#reliability-views .subtab")) {
    btn.addEventListener("click", () => {
      const route = btn.dataset.view === "tools" ? "analytics/reliability/tools" : "analytics/reliability";
      setActiveTabImpl(ctx, route, { refresh: false });
    });
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
  for (const btn of document.querySelectorAll("#knowledge-subtabs .subtab")) {
    btn.addEventListener("click", () =>
      setActiveTabImpl(ctx, `knowledge/${btn.dataset.subtab}`, { refresh: false }),
    );
  }
  for (const btn of document.querySelectorAll("#jobs-subtabs .subtab")) {
    btn.addEventListener("click", () =>
      setActiveTabImpl(ctx, `jobs/${btn.dataset.subtab}`, { refresh: false }),
    );
  }
  for (const btn of document.querySelectorAll("#operations-subtabs .subtab")) {
    btn.addEventListener("click", () =>
      setActiveTabImpl(ctx, `operations/${btn.dataset.subtab}`, { refresh: false }),
    );
  }
  for (const btn of document.querySelectorAll("#config-subtabs .subtab")) {
    btn.addEventListener("click", () => {
      setActiveTabImpl(ctx, `config/${btn.dataset.subtab}`, { refresh: false });
      ctx.refreshDashboard();
    });
  }
  // Back and Forward fire popstate, then hashchange when the fragment differs.
  // An entry made by pushState (a task link that changes workspace) fires no
  // hashchange, so popstate routes too; the address that was just routed is not
  // routed twice. common.js restores the scope on popstate first.
  let routedHref = null;
  const routeFromLocation = () => {
    if (window.location.href === routedHref) return;
    routedHref = window.location.href;
    setActiveTabImpl(ctx, window.location.hash);
  };
  window.addEventListener("hashchange", routeFromLocation);
  window.addEventListener("popstate", routeFromLocation);
  setActiveTabImpl(ctx, window.location.hash || "tasks", {
    refresh: false,
    updateHash: false,
  });
  initBackgroundNotifications();
  startDashboardPolling(() => ctx.refreshDashboard());
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

/// The canonical hash of the Health, Analytics or Agents view on screen, for a scope
/// change to write in place; null on other destinations.
export function scopeRouteHash() {
  const ctx = getCtx();
  const { query } = parseHashRoute(window.location.hash);
  if (ctx.getTab() === "jobs" && ctx.getJobsSubtab() === "history") return `#jobs/history?${windowQuery()}`;
  if (ctx.getTab() === "diagnostics") return healthHash(ctx);
  if (ctx.getTab() === "analytics") return analyticsHash(ctx, query);
  if (ctx.getTab() === "agents") return agentsHash(ctx);
  return null;
}

export function setRunDetailSubtab(name) {
  const ctx = getCtx();
  setRunDetailSubtabImpl(ctx, name);
}
