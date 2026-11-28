// Sandboxed plugin page frame, strip and postMessage bridge relay
// (docs/design/plugin-pages/2_design.md §3–§4).
//
// Each active page loads as one document from `/api/plugins/<ns>/pages/<page>/`
// inside `<iframe sandbox="allow-scripts">`. The frame has an opaque origin
// (`"null"`), so the relay identifies it strictly by `event.source ===
// activeFrame.contentWindow`, forwards declared tool calls to
// `POST /api/plugins/<ns>/pages/<page>/call` without client-side filtering,
// sends the init context on load, reloads on workspace or host change, and
// updates the hash query in place when the page asks.
//
// An action (`{act: tool, input}`) is a mutating tool the page names under
// `actions`. The relay asks the server to check it and issue a one-time
// confirmation, draws its own modal dialog in the dashboard document naming
// the plugin, the tool and the input, and sends the run only when the
// operator presses Run. The dialog sits in the top layer and makes the rest
// of the document, the frame included, inert: the page can neither draw over
// it, dismiss it nor confirm it.

import {
  AGGREGATE_PANEL_PLACEHOLDER,
  captureWorkspaceVisit,
  currentHostToken,
  el,
  escCloseButton,
  getWorkspaceRevision,
  hostWriteRefusal,
  isAggregateView,
  onWorkspaceChange,
  postJson,
  requestJson,
  syncNodes,
} from './common.js';

const $ = (id) => document.getElementById(id);

const BRIDGE_VERSION = 1;

// Run is armed only after the dialog has been on screen this long, so a
// click the page timed to land where the button appears confirms nothing.
const ACTION_ARM_MS = 800;

// The open action dialog's settle function, or null. One action at a time:
// a page asking again while one is open is answered `busy`.
let openActionDialog = null;

// Remembered route while Plugins is open or revisited in the same workspace:
// `{ namespace, page, query }` or null when Installed is selected.
let activeRoute = null;
let viewVisible = false;

// Currently mounted `<iframe sandbox="allow-scripts">` and its captured scope.
// Cleared on unmount, navigation away, reload, blocked frame navigation or
// workspace/host change so a previous or foreign window can never drive the
// relay.
let activeFrame = null;
let activeFrameMeta = null;
let blockedNavigation = null;

// Latest `/api/plugins` catalog for the current workspace visit.
let latestPlugins = null;
let latestPluginsRevision = null;
let latestPluginsError = null;
let latestAggregate = false;

// Inline enable state for a disabled plugin's diagnostic card.
let pendingEnable = false;
let enableError = null;
let lastStripSignature = null;

let bridgeWired = false;
let onRefreshRequested = () => {};
let onQueryReplaced = () => {};

/// Canonical hash for a plugin page route, preserving any non-empty query string.
export function buildPluginPageHash(namespace, page, query = "") {
  const base = `#plugins/${encodeURIComponent(namespace)}/${encodeURIComponent(page)}`;
  const clean = normalizeQuery(query);
  return clean ? `${base}?${clean}` : base;
}

function normalizeQuery(raw) {
  return String(raw || "").replace(/^[?#]+/, "");
}

function currentTheme() {
  const explicit = document.documentElement?.dataset?.theme;
  if (explicit === "light" || explicit === "dark") return explicit;
  return "dark";
}

function sessionOperator(plugin) {
  return plugin?.capabilities?.enable?.authorized === true && !hostWriteRefusal();
}

/// Wire the single window-level message relay and parent CSP navigation guard once.
export function initPluginPage({ onRefresh = () => {}, onQueryReplaced: onQuery = () => {} } = {}) {
  onRefreshRequested = typeof onRefresh === "function" ? onRefresh : () => {};
  onQueryReplaced = typeof onQuery === "function" ? onQuery : () => {};
  if (bridgeWired || typeof window === "undefined") return;
  bridgeWired = true;
  window.addEventListener("message", handleBridgeMessage);
  window.addEventListener("securitypolicyviolation", handleFramePolicyViolation);
}

/// The active or remembered plugin page `{ namespace, page, query }`, or null.
export function getActivePluginPage() {
  return activeRoute ? { ...activeRoute } : null;
}

/// Select a plugin page route (`{ namespace, page, query }`) or Installed (`null`).
export function setPluginPageRoute(route, { forceReload = false } = {}) {
  viewVisible = true;
  if (!route) {
    activeRoute = null;
    enableError = null;
    blockedNavigation = null;
    unmountFrame();
    return;
  }
  const next = {
    namespace: String(route.namespace || ""),
    page: String(route.page || ""),
    query: normalizeQuery(route.query),
  };
  const changed = !activeRoute
    || activeRoute.namespace !== next.namespace
    || activeRoute.page !== next.page
    || activeRoute.query !== next.query;
  activeRoute = next;
  if (changed || forceReload) {
    enableError = null;
    blockedNavigation = null;
  }
  syncPluginPageView({ forceReload: changed || forceReload });
}

/// Tear down any mounted frame when leaving the Plugins top-level tab, while
/// remembering the route so returning to Plugins reopens it.
export function leavePluginPageView() {
  viewVisible = false;
  unmountFrame();
}

/// Publish the latest `/api/plugins` listing (or failure/aggregate state) for
/// the current workspace visit.
export function updatePluginPageCatalog(plugins, { revision = getWorkspaceRevision(), error = null, aggregate = false } = {}) {
  latestAggregate = Boolean(aggregate);
  latestPluginsRevision = revision;
  latestPluginsError = error ? String(error.message || error) : null;
  latestPlugins = Array.isArray(plugins) ? plugins : null;
  if (viewVisible && activeRoute) {
    syncPluginPageView({ forceReload: false });
  }
}

/// Reload the mounted frame, or re-read `/api/plugins` when showing a diagnostic.
export function reloadActivePluginPage() {
  if (!viewVisible || !activeRoute) return;
  if (activeFrame || blockedNavigation) {
    blockedNavigation = null;
    syncPluginPageView({ forceReload: true });
    return;
  }
  onRefreshRequested();
}

function unmountFrame() {
  // The frame that asked is gone; its action is cancelled, never run.
  if (openActionDialog) openActionDialog(false);
  if (activeFrame) {
    activeFrame.remove();
    activeFrame = null;
  }
  activeFrameMeta = null;
}

function sameMountedPage(visit, namespace, page) {
  return Boolean(
    activeFrame
    && activeFrame.isConnected
    && activeFrameMeta
    && activeFrameMeta.visit.revision === visit.revision
    && activeFrameMeta.namespace === namespace
    && activeFrameMeta.page === page,
  );
}

function renderStrip(plugin, namespace) {
  const strip = $("plugin-page-strip");
  if (!strip) return;
  const label = plugin?.name || namespace || "Plugin";
  const version = plugin?.version ? `v${plugin.version}` : null;
  const signature = `${label}\0${version || ""}`;
  if (signature === lastStripSignature && strip.childElementCount) return;
  lastStripSignature = signature;
  const reload = el("button", {
    type: "button",
    class: "action preview plugin-page-reload",
    text: "Reload",
    "aria-description": "Reload this plugin page",
  });
  reload.addEventListener("click", () => reloadActivePluginPage());
  const identity = el("div", { class: "plugin-page-identity" }, [
    el("span", { class: "plugin-page-name", text: label }),
    version ? el("span", { class: "plugin-page-version", text: version }) : null,
  ].filter(Boolean));
  strip.replaceChildren(identity, reload);
}

function renderLoading() {
  const body = $("plugin-page-body");
  if (!body) return;
  body.classList.remove("is-framed");
  body.replaceChildren(
    el("div", { class: "skeleton-state plugin-page-state", role: "status", "aria-live": "polite" }, [
      el("span", { text: "Loading…" }),
      el("div", { class: "skeleton skeleton-row" }),
      el("div", { class: "skeleton skeleton-row", style: { width: "75%" } }),
    ]),
  );
}

function renderPlaceholder(text) {
  const body = $("plugin-page-body");
  if (!body) return;
  body.classList.remove("is-framed");
  body.replaceChildren(el("div", { class: "panel-placeholder plugin-page-state", text }));
}

function renderDiagnosticCard({ title, status, diagnostic, fixStep, plugin = null, enableScope = null, siblingPages = [] }) {
  const body = $("plugin-page-body");
  if (!body) return;
  body.classList.remove("is-framed");

  const card = el("div", { class: `card card-dense plugin-page-diagnostic${status ? ` plugin-${status}` : ""}` });
  card.dataset.key = "plugin-page-diagnostic";
  card.dataset.hash = JSON.stringify([
    title,
    status || null,
    diagnostic || null,
    fixStep || null,
    plugin?.version || null,
    plugin?.disabled_by || null,
    enableScope || null,
    Boolean(enableScope && plugin?.capabilities?.enable?.authorized === true && !hostWriteRefusal()),
    pendingEnable,
    enableError || null,
    siblingPages.map((entry) => [entry.id, entry.title]),
  ]);
  const head = [el("span", { class: "plugin-name", text: title })];
  if (status) head.push(el("span", { class: `plugin-status status-${status}`, text: status }));
  card.appendChild(el("div", { class: "plugin-head" }, head));

  if (diagnostic) {
    card.appendChild(el("p", { class: "plugin-diagnostic", text: diagnostic }));
  }

  if (plugin?.disabled_by) {
    card.appendChild(el("p", { class: "plugin-page-fix", text: `Effective: disabled by ${plugin.disabled_by}.` }));
  }

  if (fixStep) {
    const fix = el("p", { class: "plugin-page-fix" });
    appendFormattedFix(fix, fixStep);
    card.appendChild(fix);
  }

  const actions = [];
  if (enableScope && plugin && plugin.capabilities?.enable?.authorized === true && !hostWriteRefusal()) {
    const btn = el("button", {
      type: "button",
      class: "plugin-toggle",
      text: enableScope === "host" ? "Enable host" : "Enable workspace",
    });
    btn.disabled = pendingEnable;
    btn.addEventListener("click", () => enableFromDiagnostic(plugin, enableScope, btn));
    actions.push(btn);
  }

  const installedLink = el("a", { class: "plugin-page-installed-link", text: "Open Installed" });
  installedLink.href = "#plugins";
  actions.push(installedLink);

  if (actions.length) {
    card.appendChild(el("div", { class: "plugin-page-actions" }, actions));
  }

  if (enableError) {
    card.appendChild(el("p", { class: "plugin-change-error", text: enableError }));
  }

  if (siblingPages.length) {
    card.appendChild(el("div", { class: "plugin-pages" }, siblingPages.map((page) => {
      const tile = el("a", { class: "plugin-page-tile" }, [
        el("span", { class: "plugin-page-tile-title", text: page.title || page.id }),
      ]);
      tile.href = buildPluginPageHash(plugin.name, page.id);
      return tile;
    })));
  }

  syncNodes(body, [card]);
}

// Render backtick-delimited CLI commands in monospace without innerHTML.
function appendFormattedFix(container, text) {
  const parts = String(text).split(/(`[^`]+`)/g);
  for (const part of parts) {
    if (!part) continue;
    if (part.startsWith("`") && part.endsWith("`") && part.length > 2) {
      container.appendChild(el("code", { class: "mono plugin-page-cmd", text: part.slice(1, -1) }));
    } else {
      container.append(part);
    }
  }
}

async function enableFromDiagnostic(plugin, scope, button) {
  if (pendingEnable) return;
  const revision = getWorkspaceRevision();
  pendingEnable = true;
  enableError = null;
  button.disabled = true;
  try {
    await postJson(`/api/plugins/${encodeURIComponent(plugin.name)}/enable`, { scope });
    onRefreshRequested();
  } catch (error) {
    if (revision === getWorkspaceRevision()) {
      enableError = error?.message || String(error);
      syncPluginPageView({ forceReload: false });
    }
  } finally {
    pendingEnable = false;
    button.disabled = false;
  }
}

function disabledFixStep(plugin) {
  if (plugin.disabled_by === "workspace" || (plugin.host_enabled && plugin.workspace_toggle === false)) {
    return {
      scope: "workspace",
      text: `Enable it in this workspace with \`orbit plugin enable ${plugin.name} --scope workspace\` or from the Installed tab.`,
    };
  }
  return {
    scope: "host",
    text: `Enable it on this host with \`orbit plugin enable ${plugin.name}\` or from the Installed tab.`,
  };
}

function mountFrame(visit, plugin, pageSummary) {
  const body = $("plugin-page-body");
  if (!body) return;
  unmountFrame();

  const namespace = plugin.name;
  const pageId = pageSummary.id;
  const docPath = `/api/plugins/${encodeURIComponent(namespace)}/pages/${encodeURIComponent(pageId)}/`;
  const src = visit.path(docPath);

  const frame = document.createElement("iframe");
  frame.className = "plugin-page-frame";
  // Sandbox contract (design §2): unique opaque origin, script-only, no
  // same-origin, popups, forms, downloads or top navigation.
  frame.setAttribute("sandbox", "allow-scripts");
  frame.setAttribute("referrerpolicy", "no-referrer");
  frame.title = `${pageSummary.title || pageId} (${namespace})`;

  const meta = {
    namespace,
    page: pageId,
    pageTitle: pageSummary.title || pageId,
    plugin,
    visit,
    operator: sessionOperator(plugin),
    loaded: false,
  };
  activeFrame = frame;
  activeFrameMeta = meta;

  frame.addEventListener("load", () => {
    if (frame !== activeFrame || !frame.isConnected || !frame.contentWindow) return;
    // Dashboard-owned reloads always create a fresh iframe in `mountFrame`. A
    // second `load` on the same element means the frame navigated itself.
    if (meta.loaded) {
      blockFrameNavigation(
        meta,
        "Blocked a frame navigation from inside this plugin page. Plugin pages stay on the single document loaded by the dashboard.",
      );
      return;
    }
    meta.loaded = true;
    frame.contentWindow.postMessage({
      v: BRIDGE_VERSION,
      init: {
        theme: currentTheme(),
        host: currentHostToken(),
        workspace: meta.visit.workspace || null,
        operator: meta.operator,
        query: activeRoute?.query || "",
      },
    }, "*");
  });

  // Set `src` before attaching the iframe so the browser navigates straight to
  // the host-owned document instead of firing an extra initial `about:blank`
  // `load` event first.
  frame.src = src;
  body.classList.add("is-framed");
  body.replaceChildren(frame);
}

function blockFrameNavigation(meta, reason) {
  blockedNavigation = {
    revision: meta.visit.revision,
    namespace: meta.namespace,
    page: meta.page,
    reason,
  };
  unmountFrame();
  if (!viewVisible || !activeRoute) return;
  if (activeRoute.namespace !== meta.namespace || activeRoute.page !== meta.page) return;
  renderDiagnosticCard({
    title: meta.pageTitle,
    status: "inactive",
    plugin: meta.plugin,
    diagnostic: reason,
    fixStep: "Use Reload above to open a fresh dashboard-owned frame, or return to Installed.",
  });
}

function handleFramePolicyViolation(event) {
  if (!activeFrame || !activeFrame.isConnected || !activeFrameMeta) return;
  const directive = String(event?.effectiveDirective || event?.violatedDirective || "").toLowerCase();
  if (!directive.startsWith("frame-src") && !directive.startsWith("child-src")) return;
  blockFrameNavigation(
    activeFrameMeta,
    "Blocked an external frame navigation from this plugin page under the dashboard's frame policy.",
  );
}

function syncPluginPageView({ forceReload = false } = {}) {
  if (!viewVisible || !activeRoute) return;
  const { namespace, page } = activeRoute;

  if (isAggregateView() || latestAggregate) {
    unmountFrame();
    renderStrip(null, namespace);
    renderPlaceholder(AGGREGATE_PANEL_PLACEHOLDER);
    return;
  }

  const revision = getWorkspaceRevision();
  if (latestPluginsRevision !== revision) {
    unmountFrame();
    renderStrip(null, namespace);
    renderLoading();
    return;
  }

  if (latestPluginsError) {
    unmountFrame();
    renderStrip(null, namespace);
    renderDiagnosticCard({
      title: `Page '${page}'`,
      status: "inactive",
      diagnostic: `Unable to load plugins: ${latestPluginsError}`,
      fixStep: "Reload to retry, or check the Installed tab.",
    });
    return;
  }

  const plugin = (latestPlugins || []).find((item) => item?.name === namespace) || null;
  renderStrip(plugin, namespace);

  if (!plugin) {
    unmountFrame();
    renderDiagnosticCard({
      title: `Page '${page}'`,
      status: "missing",
      diagnostic: `Plugin '${namespace}' is neither installed on this host nor pinned by this workspace.`,
      fixStep: "Install it with `orbit plugin add <source>` and enable it with `orbit plugin enable <name>`.",
    });
    return;
  }

  const pageSummary = (plugin.pages || []).find((item) => item?.id === page) || null;
  const pageTitle = pageSummary?.title || `Page '${page}'`;

  if (plugin.status === "missing") {
    unmountFrame();
    renderDiagnosticCard({
      title: pageTitle,
      status: plugin.status,
      plugin,
      diagnostic: plugin.diagnostic || `Plugin '${plugin.name}' is pinned by this workspace but not installed on this host.`,
      fixStep: "Install it with `orbit plugin add <source>` and enable it with `orbit plugin enable " + plugin.name + "`.",
    });
    return;
  }

  if (plugin.status === "disabled") {
    unmountFrame();
    const fix = disabledFixStep(plugin);
    renderDiagnosticCard({
      title: pageTitle,
      status: plugin.status,
      plugin,
      diagnostic: pageSummary?.diagnostic || plugin.diagnostic || `Page '${page}' is not served while plugin '${plugin.name}' is disabled.`,
      fixStep: fix.text,
      enableScope: fix.scope,
    });
    return;
  }

  if (plugin.status !== "active") {
    unmountFrame();
    const diagnostic = plugin.diagnostic
      || pageSummary?.diagnostic
      || `Plugin '${plugin.name}' is ${plugin.status || "inactive"} and its pages cannot load.`;
    const hasCommand = diagnostic.includes("orbit plugin ");
    renderDiagnosticCard({
      title: pageTitle,
      status: plugin.status || "inactive",
      plugin,
      diagnostic,
      fixStep: hasCommand ? null : `Inspect the failure with \`orbit plugin show ${plugin.name}\` or reinstall it with \`orbit plugin add --force <source>\`.`,
    });
    return;
  }

  if (!pageSummary) {
    unmountFrame();
    renderDiagnosticCard({
      title: pageTitle,
      status: plugin.status,
      plugin,
      diagnostic: `Plugin '${plugin.name}' declares no page '${page}'.`,
      fixStep: (plugin.pages || []).length
        ? "Choose one of this plugin's declared pages below, or open Installed."
        : "Open the Installed tab to inspect this plugin's panels and tools.",
      siblingPages: (plugin.pages || []).filter((entry) => entry?.id),
    });
    return;
  }

  if (pageSummary.state !== "active") {
    unmountFrame();
    const diagnostic = pageSummary.diagnostic
      || `Page '${pageSummary.id}' of plugin '${plugin.name}' is inactive.`;
    const hasCommand = diagnostic.includes("orbit plugin ");
    renderDiagnosticCard({
      title: pageTitle,
      status: "inactive",
      plugin,
      diagnostic,
      fixStep: hasCommand ? null : `Reinstall the plugin with \`orbit plugin add --force <source>\` to record the current bundle digest.`,
    });
    return;
  }

  const visit = captureWorkspaceVisit();
  if (
    !forceReload
    && blockedNavigation
    && blockedNavigation.revision === visit.revision
    && blockedNavigation.namespace === namespace
    && blockedNavigation.page === page
  ) {
    unmountFrame();
    renderDiagnosticCard({
      title: pageTitle,
      status: "inactive",
      plugin,
      diagnostic: blockedNavigation.reason,
      fixStep: "Use Reload above to open a fresh dashboard-owned frame, or return to Installed.",
    });
    return;
  }
  blockedNavigation = null;
  if (!forceReload && sameMountedPage(visit, namespace, page)) {
    activeFrameMeta.operator = sessionOperator(plugin);
    return;
  }
  mountFrame(visit, plugin, pageSummary);
}

function handleBridgeMessage(event) {
  if (!activeFrame || !activeFrame.isConnected || !activeFrame.contentWindow) return;
  if (event.source !== activeFrame.contentWindow) return;
  if (event.origin !== "null") return;
  const message = event.data;
  if (!message || typeof message !== "object" || message.v !== BRIDGE_VERSION) return;
  const meta = activeFrameMeta;
  if (!meta) return;

  if (typeof message.call === "string") {
    if (message.id === undefined || message.id === null) return;
    void relayToolCall(activeFrame, meta, message.id, message.call, message.input);
    return;
  }

  if (typeof message.act === "string") {
    if (message.id === undefined || message.id === null) return;
    void relayAction(activeFrame, meta, message.id, message.act, message.input);
    return;
  }

  if (typeof message.query === "string") {
    applyPageQuery(message.query);
  }
}

function replyToFrame(frame, reply) {
  if (frame !== activeFrame || !frame.isConnected || !frame.contentWindow) return;
  frame.contentWindow.postMessage({ v: BRIDGE_VERSION, ...reply }, "*");
}

function replyError(frame, id, code, message) {
  replyToFrame(frame, { id, ok: false, error: { code: String(code), message: String(message) } });
}

async function relayAction(frame, meta, id, tool, input) {
  if (openActionDialog) {
    replyError(frame, id, "busy", "another action is waiting for the operator");
    return;
  }
  const base = `/api/plugins/${encodeURIComponent(meta.namespace)}/pages/${encodeURIComponent(meta.page)}/actions`;
  const body = { tool, input: input === undefined ? {} : input };
  let checked;
  try {
    checked = await requestJson(`${base}/confirm`, "POST", body, meta.visit);
  } catch (error) {
    replyError(frame, id, error?.code || "action_refused", error?.message || "the action was refused");
    return;
  }
  if (frame !== activeFrame || !frame.isConnected) return;
  if (openActionDialog) {
    replyError(frame, id, "busy", "another action is waiting for the operator");
    return;
  }
  const confirmed = await confirmPageAction(checked);
  if (!confirmed) {
    replyError(frame, id, "cancelled", "the operator cancelled the action");
    return;
  }
  if (frame !== activeFrame || !frame.isConnected) return;
  try {
    const payload = await requestJson(
      `${base}/run`,
      "POST",
      { ...body, confirmation: checked.confirmation },
      meta.visit,
    );
    const reply = { id, ok: true, output: payload?.output };
    if (payload?.truncated === true) reply.truncated = true;
    if (typeof payload?.diagnostic === "string") reply.diagnostic = payload.diagnostic;
    replyToFrame(frame, reply);
  } catch (error) {
    replyError(frame, id, error?.code || "call_failed", error?.message || "the action failed");
  }
}

function prettyInput(input) {
  try {
    return JSON.stringify(input ?? {}, null, 2);
  } catch {
    return String(input);
  }
}

// The dashboard-drawn confirmation for one checked action. Resolves true
// only on Run; Cancel, Escape, dismissal or the frame going away resolve
// false. Every line it shows comes from the server's check, not the page.
function confirmPageAction(checked) {
  if (typeof document === "undefined" || typeof document.createElement !== "function") {
    return Promise.resolve(false);
  }
  const dialog = document.createElement("dialog");
  if (!dialog || typeof dialog.showModal !== "function") return Promise.resolve(false);
  return new Promise((resolve) => {
    dialog.className = "plugin-action-dialog dialog-surface";
    dialog.setAttribute("aria-labelledby", "plugin-action-heading");
    dialog.setAttribute("aria-describedby", "plugin-action-body");
    const title = el("h3", { text: "Run a plugin action?" });
    title.id = "plugin-action-heading";
    const plugin = checked.version ? `${checked.plugin} v${checked.version}` : String(checked.plugin || "");
    const facts = el("dl", { class: "plugin-action-facts" }, [
      el("dt", { text: "Plugin" }),
      el("dd", { class: "mono", text: plugin }),
      el("dt", { text: "Page" }),
      el("dd", { text: String(checked.page_title || checked.page || "") }),
      el("dt", { text: "Tool" }),
      el("dd", { class: "mono", text: String(checked.tool || "") }),
    ]);
    const children = [facts];
    if (checked.description) {
      children.push(el("p", { class: "plugin-action-description", text: String(checked.description) }));
    }
    children.push(
      el("p", { class: "plugin-action-label", text: "Input" }),
      el("pre", { class: "plugin-action-input mono", text: prettyInput(checked.input) }),
      el("p", {
        class: "plugin-action-note",
        text: "The page asked for this. It runs once, with your authority, only if you press Run.",
      }),
    );
    const copy = el("div", { class: "dialog-body" }, children);
    copy.id = "plugin-action-body";
    const runButton = el("button", { class: "action ship", text: "Run", type: "button" });
    const cancelButton = el("button", { class: "action cancel", text: "Cancel", type: "button" });
    runButton.disabled = true;
    const actions = el("div", { class: "plugin-action-actions dialog-actions" }, [runButton, cancelButton]);
    const closeButton = escCloseButton("Cancel (Esc)", { onClick: () => settle(false) });
    dialog.append(el("header", { class: "dialog-header" }, [title, closeButton]), copy, actions);
    const previouslyFocused = document.activeElement;
    let settled = false;
    const arm = setTimeout(() => { runButton.disabled = false; }, ACTION_ARM_MS);
    const onDocumentEscape = (event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      settle(false);
    };
    const settle = (value) => {
      if (settled) return;
      settled = true;
      clearTimeout(arm);
      openActionDialog = null;
      document.removeEventListener("keydown", onDocumentEscape, true);
      if (dialog.open) dialog.close();
      dialog.remove();
      if (previouslyFocused && typeof previouslyFocused.focus === "function") {
        previouslyFocused.focus();
      }
      resolve(value);
    };
    openActionDialog = settle;
    // Only a trusted click from the operator runs the action.
    runButton.addEventListener("click", (event) => {
      if (!event.isTrusted || runButton.disabled) return;
      settle(true);
    });
    cancelButton.addEventListener("click", () => settle(false));
    dialog.addEventListener("cancel", (event) => {
      event.preventDefault();
      settle(false);
    });
    dialog.addEventListener("close", () => settle(false));
    dialog.addEventListener("keydown", (event) => {
      if (event.key !== "Tab" || event.altKey || event.ctrlKey || event.metaKey) return;
      const controls = [closeButton, runButton, cancelButton].filter((node) => !node.disabled);
      if (!controls.length) return;
      const active = document.activeElement;
      if (event.shiftKey ? active === controls[0] : active === controls[controls.length - 1]) {
        event.preventDefault();
        (event.shiftKey ? controls[controls.length - 1] : controls[0]).focus();
      }
    });
    document.addEventListener("keydown", onDocumentEscape, true);
    document.body.appendChild(dialog);
    dialog.showModal();
    cancelButton.focus();
  });
}

async function relayToolCall(frame, meta, id, tool, input) {
  const path = `/api/plugins/${encodeURIComponent(meta.namespace)}/pages/${encodeURIComponent(meta.page)}/call`;
  const body = { tool, input: input === undefined ? {} : input };
  try {
    const payload = await requestJson(path, "POST", body, meta.visit);
    if (frame !== activeFrame || !frame.isConnected || !frame.contentWindow) return;
    const reply = { v: BRIDGE_VERSION, id, ok: true, output: payload?.output };
    if (payload?.truncated === true) reply.truncated = true;
    if (typeof payload?.diagnostic === "string") reply.diagnostic = payload.diagnostic;
    frame.contentWindow.postMessage(reply, "*");
  } catch (error) {
    if (frame !== activeFrame || !frame.isConnected || !frame.contentWindow) return;
    frame.contentWindow.postMessage({
      v: BRIDGE_VERSION,
      id,
      ok: false,
      error: {
        code: String(error?.code || "call_failed"),
        message: String(error?.message || "the call failed"),
      },
    }, "*");
  }
}

function applyPageQuery(rawQuery) {
  if (!viewVisible || !activeRoute) return;
  const nextQuery = normalizeQuery(rawQuery);
  activeRoute.query = nextQuery;
  const hash = buildPluginPageHash(activeRoute.namespace, activeRoute.page, nextQuery);
  const url = new URL(window.location.href);
  url.hash = hash.slice(1);
  if (url.href !== window.location.href && typeof window.history?.replaceState === "function") {
    window.history.replaceState(window.history.state, "", url);
    onQueryReplaced();
  }
}

onWorkspaceChange(() => {
  latestPlugins = null;
  latestPluginsRevision = null;
  latestPluginsError = null;
  latestAggregate = false;
  enableError = null;
  blockedNavigation = null;
  unmountFrame();
  if (!viewVisible) {
    activeRoute = null;
    return;
  }
  if (activeRoute) {
    renderStrip(null, activeRoute.namespace);
    if (isAggregateView()) renderPlaceholder(AGGREGATE_PANEL_PLACEHOLDER);
    else renderLoading();
  }
});
