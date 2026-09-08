const params = new URLSearchParams(window.location.search);

// ORB-00030: the dashboard can serve multiple workspaces. `currentWorkspace`
// (a workspace id, or null for the aggregate "all" view) is transparently
// appended as `?workspace=<id>` to every API request below, so individual view
// modules stay workspace-agnostic. Initialized from the URL for shareable links.
let currentWorkspace = params.get("workspace") || null;

export function getWorkspace() {
  return currentWorkspace;
}

let workspaceRevision = 0;
const workspaceListeners = new Set();

export function getWorkspaceRevision() {
  return workspaceRevision;
}

export function onWorkspaceChange(listener) {
  workspaceListeners.add(listener);
  return () => workspaceListeners.delete(listener);
}

export function setWorkspace(id) {
  const next = id || null;
  if (next === currentWorkspace) return;
  currentWorkspace = next;
  workspaceRevision += 1;
  for (const listener of workspaceListeners) listener();
}

// ORB-10872: workspace + time window are one dashboard scope. Scoreboard,
// Audit, Reliability, and Managed Execution either honor this window or show
// an equally prominent independent-scope label. Seeded from `?window=` or the
// hash query so reload and shared links restore the same cutoff.
export const DASHBOARD_WINDOWS = ["1h", "24h", "7d", "30d", "all"];
export const RELIABILITY_WINDOWS = ["1h", "24h", "7d", "30d"];
export const DEFAULT_DASHBOARD_WINDOW = "24h";
export const INDEPENDENT_RELIABILITY_WINDOW = "7d";

export function parseDashboardWindow(raw, allowed = DASHBOARD_WINDOWS) {
  return allowed.includes(raw) ? raw : null;
}

function windowFromLocation() {
  const fromSearch = parseDashboardWindow(params.get("window"));
  if (fromSearch) return fromSearch;
  const hash = String(window.location.hash || "");
  const queryIdx = hash.indexOf("?");
  if (queryIdx >= 0) {
    const fromHash = parseDashboardWindow(new URLSearchParams(hash.slice(queryIdx + 1)).get("window"));
    if (fromHash) return fromHash;
  }
  return DEFAULT_DASHBOARD_WINDOW;
}

let currentWindow = windowFromLocation();

export function getWindow() {
  return currentWindow;
}

export function setWindow(raw) {
  const next = parseDashboardWindow(raw) || DEFAULT_DASHBOARD_WINDOW;
  const changed = next !== currentWindow;
  currentWindow = next;
  return changed;
}

/// Reliability cannot serve an unbounded `all` window. When the dashboard
/// selection is `all`, Reliability keeps a labeled independent 7d cutoff.
export function reliabilityWindowFor(selected = currentWindow) {
  if (RELIABILITY_WINDOWS.includes(selected)) {
    return { window: selected, independent: false };
  }
  return { window: INDEPENDENT_RELIABILITY_WINDOW, independent: true };
}

/// True only when the payload's reported window matches the active selection.
/// A 24h scoreboard/orchestration body must not render under an active 7d.
export function payloadHonorsWindow(payload, selected) {
  if (!payload || typeof payload !== "object" || !selected) return false;
  const reported = payload.window;
  if (typeof reported === "string") return reported === selected;
  if (reported && typeof reported.label === "string") return reported.label === selected;
  return false;
}

let scopeChangeListener = null;

export function setScopeChangeListener(fn) {
  scopeChangeListener = typeof fn === "function" ? fn : null;
}

export function notifyScopeChange() {
  if (scopeChangeListener) scopeChangeListener();
}

// Mirror workspace + window into the query string without a navigation.
// Hash routes still own view/filter history; this keeps reload-safe scope.
export function persistScopeToUrl() {
  const url = new URL(window.location.href);
  if (currentWorkspace) url.searchParams.set("workspace", currentWorkspace);
  else url.searchParams.delete("workspace");
  if (currentWindow) url.searchParams.set("window", currentWindow);
  else url.searchParams.delete("window");
  if (url.href !== window.location.href) {
    history.replaceState(null, "", url);
  }
}

function windowTabs(selector) {
  return Array.from(selector.querySelectorAll(".scoreboard-window-seg"));
}

function syncWindowTabState(selector, target) {
  for (const tab of windowTabs(selector)) {
    const on = tab.dataset.window === target;
    tab.classList.toggle("on", on);
    tab.setAttribute("aria-selected", on ? "true" : "false");
    tab.tabIndex = on ? 0 : -1;
  }
}

export function syncWindowSelectors() {
  const selected = currentWindow;
  const rel = reliabilityWindowFor(selected);
  for (const [id, target] of [
    ["scoreboard-window-selector", selected],
    ["reliability-window-selector", rel.window],
  ]) {
    const selector = document.getElementById(id);
    if (!selector) continue;
    syncWindowTabState(selector, target);
  }
}

function activateWindowTab(tab, allowed) {
  const next = tab && tab.dataset.window;
  if (!next || !allowed.includes(next) || next === currentWindow) return;
  setWindow(next);
  persistScopeToUrl();
  syncWindowSelectors();
  notifyScopeChange();
}

export function wireWindowSelector(selectorId, opts = {}) {
  const selector = document.getElementById(selectorId);
  if (!selector || selector.dataset.wired === "true") return;
  selector.dataset.wired = "true";
  const allowed = opts.allowAll === false ? RELIABILITY_WINDOWS : DASHBOARD_WINDOWS;
  selector.addEventListener("click", (event) => {
    const tab = event.target && event.target.closest(".scoreboard-window-seg");
    if (!tab || !selector.contains(tab)) return;
    activateWindowTab(tab, allowed);
  });
  selector.addEventListener("keydown", (event) => {
    const tab = event.target && event.target.closest(".scoreboard-window-seg");
    if (!tab || !selector.contains(tab)) return;
    const tabs = windowTabs(selector);
    const index = tabs.indexOf(tab);
    if (index < 0) return;
    let nextIndex = null;
    if (event.key === "ArrowRight") nextIndex = (index + 1) % tabs.length;
    else if (event.key === "ArrowLeft") nextIndex = (index - 1 + tabs.length) % tabs.length;
    else if (event.key === "Home") nextIndex = 0;
    else if (event.key === "End") nextIndex = tabs.length - 1;
    if (nextIndex == null) return;
    event.preventDefault();
    const nextTab = tabs[nextIndex];
    nextTab.focus();
    activateWindowTab(nextTab, allowed);
  });
}

// ORB-00030/00039/00040: the dashboard can serve multiple workspaces. "Multi-
// workspace mode" is on when more than one workspace is servable; the aggregate
// ("All workspaces") view is that mode with no concrete workspace selected. In
// that view the per-workspace endpoints have no workspace to scope to and the
// backend `Ws` extractor 400s, so every view module (app.js, audit.js,
// scoreboard.js) guards its per-workspace fetches on isAggregateView() and
// renders a placeholder instead. This lives in the shared leaf module so all
// three modules query the same live predicate without a circular import.
let multiWorkspace = false;

export function setMultiWorkspace(value) {
  multiWorkspace = !!value;
}

export function isAggregateView() {
  return multiWorkspace && !currentWorkspace;
}

// Inline text shown in place of a per-workspace panel's body while the aggregate
// view is active, instead of erroring or holding stale content.
export const AGGREGATE_PANEL_PLACEHOLDER = "Select a workspace to view this panel";

export function renderPanelPlaceholder(bodyId) {
  const body = document.getElementById(bodyId);
  if (!body) return;
  panelRequests.delete(bodyId);
  body.setAttribute("aria-busy", "false");
  const note = el("div", { class: "panel-placeholder", text: AGGREGATE_PANEL_PLACEHOLDER });
  note.dataset.key = "aggregate-placeholder";
  note.dataset.hash = "aggregate-placeholder";
  syncNodes(body, [note]);
}

// One state per rendered panel. Revision plus request identity rejects A→B→A
// responses and overlapping refreshes, even when their URLs happen to match.
const panelRequests = new Map();

function panelMessage(bodyId, state) {
  const body = document.getElementById(bodyId);
  if (!body) return;
  body.setAttribute("aria-busy", state.pending ? "true" : "false");
  let note = Array.from(body.children).find(node => node.dataset.panelStatus);
  if (!note) {
    note = el("div", { class: "panel-placeholder" });
    note.dataset.panelStatus = "true";
    note.setAttribute("role", "status");
    note.setAttribute("aria-live", "polite");
    body.insertBefore(note, body.children[0] || null);
  }
  note.className = state.error ? "panel-placeholder action-error" : "panel-placeholder";
  if (state.error) {
    const label = state.loaded ? "Refresh failed; showing stale data" : "Unable to load";
    note.textContent = `${label}: ${state.error.message}. Use Refresh to retry.`;
  } else if (state.pending) {
    note.textContent = state.loaded ? "Refreshing… showing previous data." : "Loading…";
  } else {
    note.textContent = "Updated.";
  }
}

export function panelCanRender(bodyId) {
  const state = panelRequests.get(bodyId);
  return !state || state.loaded;
}

export function resetPanel(bodyId, countId) {
  const state = { loaded: false, pending: true, countId };
  panelRequests.set(bodyId, state);
  const body = document.getElementById(bodyId);
  if (body) body.textContent = "";
  const count = document.getElementById(countId);
  if (count) count.textContent = "—";
  panelMessage(bodyId, state);
}

onWorkspaceChange(() => {
  for (const [bodyId, state] of panelRequests) resetPanel(bodyId, state.countId);
});

export async function requestPanel(bodyId, scope, request, render, countId) {
  const revision = getWorkspaceRevision();
  const previous = panelRequests.get(bodyId);
  if (!previous || previous.scope !== scope) resetPanel(bodyId, countId);
  const state = { ...panelRequests.get(bodyId), scope, pending: true, error: null };
  panelRequests.set(bodyId, state);
  panelMessage(bodyId, state);
  const current = () => revision === getWorkspaceRevision() && panelRequests.get(bodyId) === state;
  try {
    const payload = await request();
    if (!current()) return;
    state.loaded = true;
    state.pending = false;
    render(payload);
  } catch (error) {
    if (!current()) return;
    state.error = error;
    throw error;
  } finally {
    if (current()) {
      state.pending = false;
      panelMessage(bodyId, state);
    }
  }
}

// Append the selected workspace to an API path, unless one is already present
// (aggregate endpoints like /api/tasks/all are called with no workspace set).
export function withWorkspace(path) {
  if (!currentWorkspace || /[?&]workspace=/.test(path)) return path;
  const sep = path.includes("?") ? "&" : "?";
  return `${path}${sep}workspace=${encodeURIComponent(currentWorkspace)}`;
}

export function positiveIntParam(name, fallback) {
  const parsed = parseInt(params.get(name) || String(fallback), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

export function el(tag, opts = {}, children = []) {
  const node = document.createElement(tag);
  if (opts.class) node.className = opts.class;
  if (opts.text != null) node.textContent = opts.text;
  if (opts.title != null) node.title = opts.title;
  if (opts.style) Object.assign(node.style, opts.style);
  for (const child of children) {
    if (child == null) continue;
    node.appendChild(typeof child === "string" ? document.createTextNode(child) : child);
  }
  return node;
}

// ORB-11658: expanding a row is the dashboard's primary interaction, so it has
// to be operable without a mouse. The row itself carries the button semantics —
// wrapping the cells in a real <button> would break the CSS grid every row type
// lays out in — so this is the one place that grants the tab stop, the ARIA
// role and state, and the Enter/Space binding, and it binds `click` from the
// same handler so pointer and keyboard can never drift apart.
//
// `expanded` is omitted for rows that navigate instead of disclosing; those get
// button semantics with no expansion state. `controls` names the detail node
// when the row renders one with a stable id.
export function makeToggleRow(node, { expanded, onToggle, controls } = {}) {
  node.tabIndex = 0;
  node.setAttribute("role", "button");
  if (expanded != null) node.setAttribute("aria-expanded", String(!!expanded));
  if (controls) node.setAttribute("aria-controls", controls);
  node.addEventListener("click", onToggle);
  node.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    // Key events target whatever holds focus. A nested select or button is its
    // own tab stop and owns its keys; only the row's own activation belongs to
    // the row, so a bubbled key press must not toggle it.
    if (event.target !== node) return;
    event.preventDefault();
    onToggle(event);
  });
  return node;
}

// ORB-11655: a panel refresh rebuilds its nodes every 30 s, but disclosure is
// operator state, not payload state — a <details> the operator opened has to
// come back open. Keyed in one store so every rebuilt panel restores the same
// way; `key` must identify the disclosure across renders, not the node.
const expandedDetails = new Set();

export function detailsPanel(key, opts = {}) {
  const panel = el("details", opts);
  panel.open = expandedDetails.has(key);
  panel.addEventListener("toggle", () => {
    if (panel.open) expandedDetails.add(key);
    else expandedDetails.delete(key);
  });
  return panel;
}

export function statusPill(status) {
  const color = `var(--status-${status}, var(--fg))`;
  const pill = el("span", { class: "pill mono", text: status });
  pill.style.color = color;
  pill.style.borderLeft = `2px solid ${color}`;
  return pill;
}

export function priorityCell(p) {
  const node = el("span", { class: "priority mono", text: p });
  node.style.color = `var(--priority-${p}, var(--fg-dim))`;
  return node;
}

export function stateCell(state) {
  const node = el("span", { class: "mono", text: state });
  node.style.color = `var(--state-${state}, var(--fg-dim))`;
  return node;
}

export async function fetchJson(path) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30000);
  try {
    const res = await fetch(withWorkspace(path), { headers: { accept: "application/json" }, signal: controller.signal });
    if (!res.ok) {
      const text = await res.text();
      let message = `${path}: HTTP ${res.status}`;
      try {
        const body = JSON.parse(text);
        if (body && body.error) message = body.error;
      } catch (_) {}
      const error = new Error(message);
      error.status = res.status;
      throw error;
    }
    return await res.json();
  } catch (error) {
    if (controller.signal.aborted) throw new Error("Request timed out after 30 seconds");
    // Fetch and response-body transport failures are TypeErrors; HTTP and JSON
    // errors describe an available server and must stay local to the panel.
    error.networkFailure = error instanceof TypeError;
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

// ORB-10400: task-list endpoints answer a paginated envelope
// `{ items, total, limit, truncated }` so a client can tell an empty result from
// a truncated window. Accept either shape for compatibility with non-task list
// call sites that use this helper.
export function listItems(payload) {
  if (Array.isArray(payload)) return payload;
  if (payload && Array.isArray(payload.items)) return payload.items;
  return [];
}

export function requestJson(path, method, body) {
  const headers = { accept: "application/json" };
  const opts = {
    method,
    headers,
  };
  if (body !== undefined) {
    headers["content-type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  return fetch(withWorkspace(path), opts).then(async (res) => {
    const text = await res.text();
    let body = {};
    if (text) {
      try {
        body = JSON.parse(text);
      } catch {
        body = { error: text };
      }
    }
    if (!res.ok) {
      throw new Error(body.error || `${path}: HTTP ${res.status}`);
    }
    return body;
  });
}

export function postJson(path, body) {
  return requestJson(path, "POST", body);
}

export function patchJson(path, body) {
  return requestJson(path, "PATCH", body);
}

export function syncNodes(container, newNodesArr) {
  const state = panelRequests.get(container.id);
  if (state) {
    panelMessage(container.id, state);
    const note = Array.from(container.children).find(node => node.dataset.panelStatus);
    newNodesArr = [note, ...newNodesArr];
  }
  const oldNodes = Array.from(container.children);
  const oldMap = new Map();
  for (const node of oldNodes) {
    if (node.dataset.key) oldMap.set(node.dataset.key, node);
  }

  for (let i = 0; i < newNodesArr.length; i++) {
    const newNode = newNodesArr[i];
    const key = newNode.dataset.key;
    let nodeToPlace = newNode;

    if (key && oldMap.has(key)) {
      const oldNode = oldMap.get(key);
      if (oldNode.dataset.hash === newNode.dataset.hash) {
        nodeToPlace = oldNode;
      } else {
        nodeToPlace.classList.add("data-changed");
      }
    } else if (key) {
      nodeToPlace.classList.add("data-new");
    }

    if (container.children[i] !== nodeToPlace) {
      if (container.children[i]) {
        container.insertBefore(nodeToPlace, container.children[i]);
      } else {
        container.appendChild(nodeToPlace);
      }
    }
  }

  while (container.children.length > newNodesArr.length) {
    container.removeChild(container.lastElementChild);
  }
  if (state) panelMessage(container.id, state);
}
