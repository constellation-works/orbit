import { captureWorkspaceVisit, requestPanel, el, fetchJson, formatDateTime, getHost, getServingHost, getWorkspaceRevision, postJson, isAggregateView, getRegisteredHosts, workspaceCatalogEntries, workspaceDisplayName, findRegisteredHost, hostWriteRefusal } from './common.js';
import { drainCrewNote, drainPilotNote, setDrainReadiness, setDrainReadinessFailure } from './drain-waits.js';
import { AUTO_DRAIN_APPROVE_RULE, drainCheckout } from './drain-start.js';
import { autoDrainForecast, refreshBacklogForecast } from './operations-forecast.js';
import { loadFleetSnapshot, paintAggregateFleet, paintRailDrainReading, repaintRailDrainFromFleet } from './operations-fleet.js';
import { $, feedback, operationsState, pendingOperations, registerOperationsReset, runLink, selectedWorkspace, workspaceReadOnlyReason } from './operations-shared.js';
import { taskList, taskState } from './task-shared.js';
import { configureOperationsDrainControls, autoDrainConcurrencyValid, autoDrainConcurrencyValue, autoDrainStopButton, autoDrainStopNote, refreshDrainAfterAction, renderAutoDrainStart } from './operations-drain-controls.js';
import { autoDrainBlocked, autoDrainCapacity, autoDrainCounts, autoDrainPhase, autoDrainPlural, autoDrainProviderAuth, autoDrainProviderLimitNote, autoDrainReasons, autoDrainShortRunId, autoDrainThrottleLine, autoDrainTimeLeft, updateDrainIndicators } from './operations-drain-capacity.js';

let announcedDrainState = null;
// Set while Start or Stop is refreshing the card after reporting its result, so
// the state change that result caused is not announced over the result.
let holdDrainAnnouncement = false;
// Bumped on every host or workspace reset. A readiness read captures the value
// it started with and will not paint after a newer scope has taken the card.
let drainScopeGeneration = 0;
// What the card shows: resolved from the operator's stored pick on each paint.
let aggregateDrainHost = null;
let aggregateDrainWorkspace = null;
// The operator's own picks, held here for a page whose storage refuses them.
let chosenAggregateHost = null;
let chosenAggregateWorkspace = null;
// On an owner's concrete board, a follower pick (null means the board's own
// host, which keeps today's request path). Storage is separate from the All
// workspaces pick, so opening the board still starts on the owner.
let boardDrainHost = null;
let chosenBoardHost = null;
let chosenBoardWorkspace = null;
const aggregateDrainScopes = new WeakMap();
const PULL_OWNER_UNAVAILABLE = "The owner's host and workspace are unavailable. Reload the page before starting a pull window.";

// A card selection is a separate visit from the rail's host/workspace. Polls
// keep that visit; switching either select retires actions and readbacks.
function resetAggregateDrainSelection() {
  drainScopeGeneration += 1;
  operationsState.lastAutoDrain = null;
  announcedDrainState = null;
  clearDrainChrome();
  clearStartCard("auto-drain-quick-start");
  feedback("auto-drain-operation-feedback", "", "");
}

function aggregateDrainScope(workspace, hostName, connection, generation, checkoutUnavailable, mode = "aggregate") {
  const host = findRegisteredHost(hostName)?.local ? "" : hostName;
  const revision = getWorkspaceRevision();
  const refusal = hostWriteRefusal(host, connection?.forward_writes) || (checkoutUnavailable ? PULL_OWNER_UNAVAILABLE : "");
  return {
    workspace, host, hostName, refusal,
    isCurrent: () => {
      if (generation !== drainScopeGeneration || revision !== getWorkspaceRevision()) return false;
      if (mode === "board") {
        return !isAggregateView() && selectedWorkspace()?.id === workspace.id && boardDrainHost === hostName;
      }
      return isAggregateView() && aggregateDrainWorkspace === workspace.id && aggregateDrainHost === hostName;
    },
    writeRefusal: () => refusal,
    path: path => `${host ? `/api/on/${encodeURIComponent(host)}` : "/api"}/${path.slice("/api/".length)}?workspace=${encodeURIComponent(workspace.id)}`,
  };
}
// The card's chosen workspace and host survive a reload like the dock's other
// selections. Only an operator's change is stored. Each paint resolves the card
// from the stored pick, falling back to the defaults without storing them, so a
// default keeps following the fleet until the operator chooses. Storage can
// refuse; the in-memory choice then serves this page.
const AGGREGATE_WORKSPACE_STORAGE_KEY = "orbit.dashboard.autoDrainWorkspace";
const AGGREGATE_HOST_STORAGE_KEY = "orbit.dashboard.autoDrainHost";
const LIVE_DRAIN_PHASES = ["draining", "pulling", "winding_down"];
function rememberedAggregateWorkspace() {
  try { return window.localStorage.getItem(AGGREGATE_WORKSPACE_STORAGE_KEY) || chosenAggregateWorkspace; } catch { return chosenAggregateWorkspace; }
}
function chooseAggregateWorkspace(id) {
  chosenAggregateWorkspace = id || null;
  try {
    if (id) window.localStorage.setItem(AGGREGATE_WORKSPACE_STORAGE_KEY, id);
    else window.localStorage.removeItem(AGGREGATE_WORKSPACE_STORAGE_KEY);
  } catch { /* the in-memory choice serves */ }
}
function rememberedAggregateHost() {
  try { return window.localStorage.getItem(AGGREGATE_HOST_STORAGE_KEY) || chosenAggregateHost; } catch { return chosenAggregateHost; }
}
function chooseAggregateHost(name) {
  chosenAggregateHost = name || null;
  try {
    if (name) window.localStorage.setItem(AGGREGATE_HOST_STORAGE_KEY, name);
    else window.localStorage.removeItem(AGGREGATE_HOST_STORAGE_KEY);
  } catch { /* the in-memory choice serves */ }
}
// A follower chosen on the concrete board, remembered with the workspace it
// belongs to. The owner is the default and is not stored, so the board keeps
// today's path until the operator picks a follower here.
const BOARD_HOST_STORAGE_KEY = "orbit.dashboard.concreteDrainHost";
function rememberedBoardHost(workspaceId) {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(BOARD_HOST_STORAGE_KEY) || "null");
    if (parsed && parsed.workspace === workspaceId && parsed.host) return parsed.host;
  } catch { /* storage refused; the in-memory choice follows */ }
  return chosenBoardWorkspace === workspaceId ? chosenBoardHost : null;
}
function chooseBoardHost(workspaceId, host) {
  chosenBoardWorkspace = workspaceId || null;
  chosenBoardHost = host || null;
  try {
    if (workspaceId && host) window.localStorage.setItem(BOARD_HOST_STORAGE_KEY, JSON.stringify({ workspace: workspaceId, host }));
    else window.localStorage.removeItem(BOARD_HOST_STORAGE_KEY);
  } catch { /* the in-memory choice serves */ }
}
// The host this board already addresses. The serving host has no rail host.
function boardHostName() {
  return getHost() || getServingHost()?.name || "";
}
// The selected host's owned workspaces, in alphabetical order. All workspaces
// is the whole aggregate board, so the Tasks chip filter does not narrow this
// select. Replicas are never in the catalog.
function aggregateWorkspaceOptions(fleet) {
  const names = new Map();
  for (const row of fleet?.items || []) {
    if (row.workspace_id && row.workspace_name && !names.has(row.workspace_id)) names.set(row.workspace_id, row.workspace_name);
  }
  return workspaceCatalogEntries()
    .filter(workspace => workspace.status === "active" && workspace.id)
    .map(workspace => ({ id: workspace.id, name: names.get(workspace.id) || workspaceDisplayName(workspace.id) || workspace.id }))
    .sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}
// The hosts that can show this workspace's drain: its owner first, then the
// follower hosts holding a replica of it. A fleet row names its owner only
// on a replica (`owner_machine_id`); the owner's own row names none. A host
// whose fleet read failed has one row with no workspace, so it cannot be
// placed on the workspace; it is kept in the list rather than dropped, since
// it may hold the workspace and the card must be able to show its error.
function aggregateWorkspaceHosts(fleet, workspaceId, hosts) {
  const rows = (fleet?.items || []).filter(row => row.workspace_id === workspaceId && !row.pending);
  let owner = rows.find(row => !row.owner_machine_id)?.host || null;
  if (!owner) {
    for (const row of rows) {
      owner = findRegisteredHost(row.owner_machine_id)?.name || null;
      if (owner) break;
    }
  }
  const ordered = [];
  if (owner) ordered.push(owner);
  for (const row of rows) {
    if (row.host && !ordered.includes(row.host)) ordered.push(row.host);
  }
  if (!ordered.length) return hosts.map(host => host.name);
  for (const row of fleet?.items || []) {
    if (hostFleetFailure(row) && !ordered.includes(row.host)) ordered.push(row.host);
  }
  return ordered;
}
// The host-level error row of a host whose fleet read failed. Workspace rows
// carry their own `error` and are left to the readiness read.
function hostFleetFailure(row) {
  return Boolean(row?.host && row.error && !row.workspace_id && !row.pending);
}
// The Drain host select shared by All workspaces and an owner's concrete
// board. One listener covers both: the select is removed when the scope resets.
function paintDrainHostSelect(ordered, selected) {
  const phase = $("auto-drain-phase");
  if (!phase) return;
  let select = $("auto-drain-host");
  if (!select) {
    select = el("select", { id: "auto-drain-host", class: "workspace-select", "aria-label": "Drain host" });
    select.addEventListener("change", () => onDrainHostChanged(select.value));
    phase.before(select);
  }
  const signature = JSON.stringify(ordered);
  if (select.dataset.hosts !== signature) {
    select.replaceChildren(...ordered.map(name => {
      const node = el("option", { text: name });
      node.value = name;
      return node;
    }));
    select.dataset.hosts = signature;
  }
  select.value = selected || "";
}
function onDrainHostChanged(value) {
  if (isAggregateView()) {
    chooseAggregateWorkspace(aggregateDrainWorkspace);
    chooseAggregateHost(value);
    aggregateDrainHost = value || null;
  } else {
    const workspace = selectedWorkspace();
    const follower = Boolean(value) && Boolean(boardHostName()) && value !== boardHostName();
    if (workspace) chooseBoardHost(workspace.id, follower ? value : null);
    boardDrainHost = follower ? value : null;
  }
  resetAggregateDrainSelection();
  fetchAndRenderAutoDrain().catch(error => console.error(error));
}
// Readiness, connection and refusal for one chosen host. All workspaces and a
// concrete board's follower use this; the owner's own board does not.
async function loadScopedDrainReadiness({ fleet, workspace, hostName, generation, mode, stillCurrent }) {
  const failure = (fleet?.items || []).find(item => item.host === hostName && hostFleetFailure(item));
  if (failure) throw new Error(failure.error);
  const row = (fleet?.items || []).find(item => item.host === hostName && item.workspace_id === workspace.id);
  let checkoutUnavailable = !row || row.pending;
  const unavailable = () => ({ capacity: { drain_phase: "idle" }, replica: true,
    start_options: { checkout: "replica", workspace_id: workspace.id, pull_refusal: PULL_OWNER_UNAVAILABLE } });
  const [connection, readiness] = await Promise.all([
    findRegisteredHost(hostName)?.local ? null : fetchJson(`/api/hosts/${encodeURIComponent(hostName)}/connection`),
    checkoutUnavailable ? unavailable() : fetchJson(`/api/on/${encodeURIComponent(hostName)}/workflows/auto/readiness?workspace=${encodeURIComponent(workspace.id)}`)
      .catch(error => {
        // Only a replica reads as an unavailable owner; an owner whose
        // workspace is inactive keeps the error its readiness returned.
        if (error.code !== "workspace_unavailable" || !row?.owner_machine_id) throw error;
        checkoutUnavailable = true;
        return unavailable();
      }),
  ]);
  if (stillCurrent && !stillCurrent()) return null;
  const scope = aggregateDrainScope(workspace, hostName, connection, generation, checkoutUnavailable, mode);
  // A failed connection read must never leave a usable forwarded control.
  if (connection && !connection.reachable) throw new Error(connection.error?.message || "Host unavailable");
  aggregateDrainScopes.set(readiness, scope);
  return readiness;
}
// Hosts for this workspace once a fleet row places it. Before that, the
// all-hosts fallback would offer machines that do not hold a replica.
function concreteHostOptions(fleet, workspaceId) {
  const items = fleet?.items || [];
  if (!items.some(row => row.workspace_id === workspaceId && !row.pending)) return null;
  return aggregateWorkspaceHosts(fleet, workspaceId, getRegisteredHosts());
}
function paintConcreteHostSelect(fleet, workspace) {
  if (!workspace || isAggregateView()) return null;
  const ordered = concreteHostOptions(fleet, workspace.id);
  if (!ordered?.length) {
    $("auto-drain-host")?.remove();
    return null;
  }
  const own = boardHostName();
  const remembered = rememberedBoardHost(workspace.id);
  // The board's own host stays the default even when the fleet lists it after
  // a failed read. A stored follower applies only while it is still listed.
  const selected = ordered.includes(remembered) ? remembered : ordered.includes(own) ? own : ordered[0];
  paintDrainHostSelect(ordered, selected);
  return { ordered, selected, follower: Boolean(own) && selected !== own };
}
function renderAutoDrainHead(payload) {
  const { capacity, phase, running, label, pulling, pullLabel } = autoDrainPhase(payload);
  updateDrainIndicators(pulling ? "pulling" : phase, pulling ? pullLabel : label);
  if (!isAggregateView()) paintRailDrainReading({
    phase: pulling ? "pulling" : phase,
    used: capacity.active_leaf_runs ?? capacity.occupancy?.active_leaf_runs ?? null,
    ceiling: capacity.max_active_leaf_runs ?? null,
    held: Boolean(capacity.resource_throttle || (capacity.host_shutdown && capacity.host_shutdown_holding !== false)),
  });
  const card = $("auto-drain-panel");
  if (card) {
    card.dataset.drainState = pulling ? "pulling" : phase;
    const title = card.querySelector('.drain-title');
    if (title) title.textContent = isAggregateView() ? 'Auto-draining' : 'Auto-drain';
  }
  const pill = $("auto-drain-phase");
  if (pill) pill.dataset.drainState = pulling ? "pulling" : phase;
  const dot = $("auto-drain-dot");
  if (dot) dot.className = `drain-dot ${pulling ? "pulling" : phase}`;
  const text = $("auto-drain-phase-label");
  if (text) {
    text.textContent = pulling ? pullLabel
      : phase === "idle" ? (!isAggregateView() && workspaceReadOnlyReason() ? "Read-only" : "Idle") : label;
  }
  // The window's deadline sits beside its phase; the pill already says when
  // admissions have stopped.
  const when = $("auto-drain-when");
  if (when) when.textContent = !pulling && phase === "draining" ? autoDrainTimeLeft(capacity.ends_at) : "";
  const stateKey = `${phase}:${phase === "winding_down" ? running : ""}`;
  if (announcedDrainState !== null && announcedDrainState !== stateKey && !holdDrainAnnouncement
      && !$('auto-drain-operation-feedback')?.matches(".pending, .success, .error")) {
    feedback("auto-drain-operation-feedback", "", `Auto-drain ${label}${phase === "winding_down" ? ` · this window: ${running} still running` : ""}.`);
  }
  announcedDrainState = stateKey;
}

// The live window as one row of figures: what still runs, what it has
// admitted and, when it was started with approve-proposed, what it approved
// and held. Its run and deadline live in the footer and header. Empty when no
// auto window is live.
function autoDrainWindow(payload) {
  const { capacity, phase, running, pulling } = autoDrainPhase(payload);
  if (pulling || phase === "idle") return null;
  const stats = el("dl", { class: "drain-window-stats" });
  stats.id = "auto-drain-live";
  stats.setAttribute("aria-label", "This window");
  const stat = (key, label, value, detail = "", title = "") => {
    const figure = el("div", { class: "drain-window-stat", title }, [
      el("dt", { text: label }),
      el("dd", { text: String(value) }),
      detail ? el("span", { class: "drain-window-stat-detail", text: detail }) : null,
    ]);
    figure.dataset.stat = key;
    stats.appendChild(figure);
  };
  if (capacity.admitted_workers != null) {
    stat("running", "running", running);
    stat("admitted", "admitted", capacity.admitted_workers);
  }
  const approvals = autoDrainApprovals(payload.approvals);
  if (approvals) stat("approved", "approved", approvals.approved, approvals.held, approvals.title);
  if (stats.children.length === 0) return null;
  if (!approvals || !approvals.reasons.length) return stats;
  const summary = el("div", { class: "drain-approval-summary" });
  approvals.reasons.forEach(([reason, count], index) => {
    if (index) summary.append(" · ");
    const label = reason === "warnings" ? "held for pilot warnings" : (reason || "unclassified").replaceAll("_", " ");
    const item = el(reason === "warnings" ? "a" : "span", { text: `${count} ${label}` });
    if (reason === "warnings") {
      item.href = "#tasks?status=proposed&pilot=warnings";
      item.title = "Show proposed tasks with pilot warnings";
    }
    summary.append(item);
  });
  return el("div", { class: "drain-window" }, [stats, summary]);
}

// The card's last row: the coordinator run (or the replica's pull drain), the
// window's auto-concurrency, and Stop. Its help line follows.
function autoDrainFoot(payload) {
  const { live, capacity, phase, pulling } = autoDrainPhase(payload);
  const runId = pulling ? live.pullRunId : phase === "idle" ? "" : capacity.drain_status_run_id || live.runId;
  if (isAggregateView() && !runId && !drainCheckout(payload).replica) return null;
  const meta = el("div", { class: "drain-foot-meta" });
  meta.id = "auto-drain-run";
  if (runId) meta.appendChild(runLink(runId, selectedWorkspace()?.id, autoDrainShortRunId(runId)));
  const mode = !pulling && phase !== "idle" ? payload.auto_concurrency?.mode : "";
  if (mode) {
    meta.appendChild(el("span", {
      class: "drain-foot-concurrency",
      text: `auto-concurrency ${mode}`,
      title: `Auto-concurrency: ${mode} (${payload.auto_concurrency.source}).`,
    }));
  }
  return el("div", { class: "drain-foot" }, [
    el("div", { class: "drain-foot-row" }, [meta, autoDrainStopButton(payload)]),
    autoDrainStopNote(payload),
  ]);
}

// A queued window's length: `for_seconds` 0 is a single pass.
function autoDrainWindowLength(seconds) {
  const total = Number(seconds);
  if (!Number.isFinite(total) || total <= 0) return "single pass";
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  if (!hours) return minutes ? `${minutes}m window` : `${total}s window`;
  return `${hours}h${minutes ? ` ${String(minutes).padStart(2, "0")}m` : ""} window`;
}

// Windows started while another runs wait for its slot (one coordinator per
// workspace). Each is listed under the live window with what it will admit,
// when it starts and, when its start lacked every Claude credential, the
// Claude crews in its window that will run without one. Null when none wait,
// so the card is unchanged.
function autoDrainQueued(payload) {
  const capacity = payload.capacity || {};
  const queued = (Array.isArray(capacity.queued_drains) ? capacity.queued_drains : []).filter(drain => drain?.run_id);
  if (!queued.length) return null;
  const section = el("section", { class: "drain-queued" });
  section.id = "auto-drain-queued";
  section.setAttribute("aria-label", "Queued windows");
  section.append(el("h4", { class: "drain-queued-title", text: `Queued · ${autoDrainPlural(queued.length, "window")}` }));
  const list = el("ul", { class: "drain-queued-list" });
  for (const drain of queued) list.append(autoDrainQueuedRow(payload, drain));
  section.append(list);
  return section;
}

function autoDrainQueuedRow(payload, drain) {
  const capacity = payload.capacity || {};
  const runId = String(drain.run_id);
  const row = el("li", { class: "drain-queued-row" });
  row.dataset.runId = runId;
  const head = el("div", { class: "drain-queued-head" }, [
    runLink(runId, selectedWorkspace()?.id, autoDrainShortRunId(runId)),
    el("span", { class: "drain-queued-state", text: capacity.drain_run_id ? "Waiting for the live window" : "Waiting for its slot" }),
    isAggregateView() || boardDrainHost ? null : autoDrainQueuedCancel(payload, runId),
  ]);
  const crews = Array.isArray(drain.allowed_crews) && drain.allowed_crews.length ? drain.allowed_crews.join(", ") : "all crews";
  const facts = el("div", { class: "drain-queued-facts" }, [
    ["ceiling", `${drain.max_active_leaf_runs ?? "default"} parallel`],
    ["completion", drain.completion === "done" ? "completes to done" : "completes to review"],
    ["crews", crews],
    ["window", autoDrainWindowLength(drain.for_seconds)],
  ].map(([key, text]) => {
    const fact = el("span", { class: "drain-queued-fact", text });
    fact.dataset.field = key;
    return fact;
  }));
  row.append(head, facts);
  if (capacity.drain_run_id) {
    const left = autoDrainTimeLeft(capacity.ends_at);
    const ends = Date.parse(capacity.ends_at || "");
    row.append(el("div", {
      class: "drain-queued-start",
      text: `Starts when ${autoDrainShortRunId(String(capacity.drain_run_id))} ends${left && left !== "window closed" ? ` · ${left}` : ""}`,
      title: Number.isFinite(ends) ? `The live window closes ${formatDateTime(capacity.ends_at)}` : "",
    }));
  }
  const unset = Array.isArray(drain.env_pass_unset) ? drain.env_pass_unset.filter(Boolean) : [];
  const uncredentialed = Array.isArray(drain.uncredentialed_crews) ? drain.uncredentialed_crews.filter(Boolean) : [];
  if (uncredentialed.length) {
    const flag = el("div", {
      class: "drain-queued-flag",
      text: `No Claude credential for ${uncredentialed.join(", ")}: this window was started without ${unset.join(", ")}.`,
    });
    flag.setAttribute("role", "note");
    row.append(flag);
  } else if (unset.length) {
    row.append(el("div", { class: "drain-queued-env", text: `Started without ${unset.join(", ")}` }));
  }
  return row;
}

// Cancels this queued run alone through the run cancel path; a never-started
// window has admitted nothing, and the live window is not touched. The request
// is queued-only: a window that started after this card rendered is refused
// (409) by the server and keeps running.
function autoDrainQueuedCancel(payload, runId) {
  const key = `auto-drain:cancel-queued:${runId}`;
  const refusal = workspaceReadOnlyReason();
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: "operation-button drain-queued-cancel",
    text: pending ? "Cancelling…" : "Cancel",
    title: refusal || `Cancel queued window ${runId} before it starts`,
  });
  button.type = "button";
  button.dataset.drainFocus = `cancel-queued:${runId}`;
  button.setAttribute("aria-label", `Cancel queued window ${runId}`);
  button.disabled = Boolean(refusal) || pending;
  button.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    if (pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    if (!window.confirm(`Cancel queued window ${runId}? It has not started and has admitted no work; the live window keeps running.\n\nWorkspace "${workspace?.name || workspace?.id}"`)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", `Cancelling queued window ${runId}…`);
    renderAutoDrain(payload);
    try {
      const result = await postJson(visit.path(`/api/runs/${encodeURIComponent(runId)}/cancel`), { reason: null, force: false, queued_only: true });
      if (!visit.isCurrent()) return;
      feedback("auto-drain-operation-feedback", "success", result?.outcome === "already_terminal"
        ? `Queued window ${runId} had already ended.`
        : `Queued window ${runId} cancelled.`);
      await refreshDrainAfterAction();
    } catch (error) {
      if (!visit.isCurrent()) return;
      feedback("auto-drain-operation-feedback", "error", error.status === 409
        ? `Queued window ${runId} has already started, so it was not cancelled. Stop it from the live window's controls if you mean to.`
        : `Cancelling queued window ${runId} failed: ${error.message}`);
      if (error.status === 409) await refreshDrainAfterAction();
    } finally {
      pendingOperations.delete(key);
      if (operationsState.lastAutoDrain) renderAutoDrain(operationsState.lastAutoDrain);
    }
  });
  return button;
}

// Whether the live window was started with approve-proposed, and what its
// passes have done so far. The payload's `approvals` reports the status
// drain's own input: `approved_total` counts the whole window, while the
// held figures describe the latest pass. Findings live on each task.
function autoDrainApprovals(approvals) {
  if (!approvals || approvals.enabled !== true) return null;
  const approved = Number(approvals.approved_total);
  const held = Number(approvals.held_total);
  const reasons = Object.entries(approvals.held_by_reason || {});
  return {
    approved: Number.isFinite(approved) ? approved : "—",
    held: Number.isFinite(held) && held > 0 ? `${held} held` : "",
    title: `Approving proposed tasks. ${AUTO_DRAIN_APPROVE_RULE}`,
    reasons,
  };
}

// A Start-card field's label with its help as an info hint beside it; `warn`
// tints the hint amber. The hint sits outside the <label>, so pressing it never
// focuses or toggles the control.
// Run and task links follow the host the card is showing, including a
// modified click or a new tab. The owner's own board leaves them on the rail.
function retargetDrainLinks(body, scope) {
  const hostName = scope?.hostName;
  const workspaceId = scope?.workspace?.id;
  if (!hostName || !workspaceId) return;
  for (const anchor of body.querySelectorAll("a[href]")) {
    const url = new URL(anchor.href, window.location.href);
    if (url.origin !== window.location.origin) continue;
    url.searchParams.set("host", hostName);
    url.searchParams.set("workspace", workspaceId);
    url.searchParams.delete("workspaces");
    anchor.href = url.href;
    anchor.addEventListener("click", event => event.stopImmediatePropagation(), { capture: true });
  }
}

function renderAutoDrain(payload, generation = drainScopeGeneration) {
  const scope = aggregateDrainScopes.get(payload) || null;
  if (generation !== drainScopeGeneration || scope?.isCurrent() === false) return;
  operationsState.lastAutoDrain = payload;
  // A scoped card (All workspaces, or a follower on the owner's board) is that
  // host's drain, not the task list's readiness snapshot.
  setDrainReadiness(scope ? {} : payload);
  const body = $("auto-drain-body");
  if (!body) return;
  // A poll or a control click rebuilds both cards; keep keyboard focus on the
  // control that had it.
  const focusKey = document.activeElement?.dataset?.drainFocus;
  body.textContent = "";
  renderAutoDrainHead(payload);
  const reasons = autoDrainReasons(payload, scope?.refusal);
  const holds = [
    autoDrainThrottleLine(payload.capacity),
    autoDrainProviderLimitNote(payload.provider_limits),
    autoDrainProviderAuth(payload.provider_auth_failures),
    drainCrewNote(payload),
    drainPilotNote(payload),
  ].filter(Boolean);
  // A follower scope keeps Start and Stop on the card, disabled, the way All
  // workspaces does. Only the board's own read-only reason replaces the body.
  if (reasons.submit && !isAggregateView() && !scope) {
    body.append(...[autoDrainForecast(payload), ...holds, autoDrainQueued(payload)].filter(Boolean), el("div", { class: "operations-readonly-note", text: reasons.submit }));
    retargetDrainLinks(body, scope);
    renderAutoDrainStart(payload, reasons);
    return;
  }
  body.append(...[
    autoDrainWindow(payload),
    autoDrainCapacity(payload.capacity || {}, autoDrainCounts(payload), autoDrainBlocked(payload).length, { pull: drainCheckout(payload).replica }),
    ...holds,
    autoDrainForecast(payload),
    autoDrainFoot(payload),
    autoDrainQueued(payload),
  ].filter(Boolean));
  retargetDrainLinks(body, scope);
  if (!isAggregateView() || scope) renderAutoDrainStart(payload, reasons);
  else clearStartCard("auto-drain-quick-start");
  if (focusKey) document.querySelector?.(`[data-drain-focus="${focusKey}"]`)?.focus();
}

function clearStartCard(id) {
  const body = $(`${id}-body`);
  if (body) body.textContent = "";
  const card = $(id);
  if (card) card.hidden = true;
}

// The shared panel reset clears the body and shows Loading. The phase pill,
// its dot and the window's time left sit outside that body, so clear them
// here or the previous host's drain stays on the card until the next paint.
function clearDrainChrome() {
  const card = $("auto-drain-panel");
  if (card) {
    delete card.dataset.drainState;
    const title = card.querySelector(".drain-title");
    if (title) title.textContent = "Auto-drain";
  }
  const pill = $("auto-drain-phase");
  if (pill) delete pill.dataset.drainState;
  const dot = $("auto-drain-dot");
  if (dot) dot.className = "drain-dot";
  const text = $("auto-drain-phase-label");
  if (text) text.textContent = "—";
  const when = $("auto-drain-when");
  if (when) when.textContent = "";
}

registerOperationsReset(() => {
  drainScopeGeneration += 1;
  announcedDrainState = null;
  aggregateDrainHost = null;
  aggregateDrainWorkspace = null;
  chosenAggregateHost = null;
  chosenAggregateWorkspace = null;
  boardDrainHost = null;
  chosenBoardHost = null;
  chosenBoardWorkspace = null;
  $("auto-drain-host")?.remove();
  $("auto-drain-workspace")?.remove();
  clearDrainChrome();
  updateDrainIndicators("idle", "idle");
  repaintRailDrainFromFleet();
  clearStartCard("auto-drain-quick-start");
});

function fetchAndRenderAutoDrain({ detailTaskId = null, forecast = true } = {}) {
  const generation = drainScopeGeneration;
  const workspace = selectedWorkspace();
  if (!workspace) {
    const token = ++operationsState.aggregateFleetToken;
    const revision = getWorkspaceRevision();
    const guard = () => generation === drainScopeGeneration && token === operationsState.aggregateFleetToken && revision === getWorkspaceRevision() && !selectedWorkspace();
    return requestPanel("auto-drain-body", `aggregate:${aggregateDrainHost || "owner"}:${aggregateDrainWorkspace || "none"}`, async () => {
      const fleet = await loadFleetSnapshot({ refreshWhenStale: true, onPartial: payload => { if (guard()) paintAggregateFleet(payload); } });
      if (!guard()) return null;
      paintAggregateFleet(fleet);
      const hosts = getRegisteredHosts();
      const options = aggregateWorkspaceOptions(fleet);
      const rememberedWorkspace = rememberedAggregateWorkspace();
      const liveDefault = options.find(option => (fleet.items || []).some(row => row.workspace_id === option.id && !row.error && LIVE_DRAIN_PHASES.includes(row.phase)));
      aggregateDrainWorkspace = options.some(option => option.id === rememberedWorkspace)
        ? rememberedWorkspace
        : (liveDefault || options[0])?.id || null;
      let workspaceSelect = $("auto-drain-workspace");
      if (!workspaceSelect) {
        workspaceSelect = el("select", { id: "auto-drain-workspace", class: "workspace-select", "aria-label": "Drain workspace" });
        workspaceSelect.addEventListener("change", () => {
          chooseAggregateWorkspace(workspaceSelect.value);
          // Changing the workspace resets the host to its owner.
          chooseAggregateHost(null);
          // The panel's scope key reads the shown selection, so the pick is
          // shown now; the next paint resolves it the same way from storage.
          aggregateDrainWorkspace = workspaceSelect.value || null;
          aggregateDrainHost = null;
          resetAggregateDrainSelection();
          fetchAndRenderAutoDrain().catch(error => console.error(error));
        });
        $("auto-drain-phase").before(workspaceSelect);
      }
      const workspaceSignature = JSON.stringify(options.map(option => [option.id, option.name]));
      if (workspaceSelect.dataset.workspaces !== workspaceSignature) {
        workspaceSelect.replaceChildren(...options.map(option => {
          const node = el("option", { text: option.name });
          node.value = option.id;
          return node;
        }));
        workspaceSelect.dataset.workspaces = workspaceSignature;
      }
      workspaceSelect.value = aggregateDrainWorkspace || "";
      const ordered = aggregateDrainWorkspace ? aggregateWorkspaceHosts(fleet, aggregateDrainWorkspace, hosts) : hosts.map(host => host.name);
      // A remembered host applies only to the workspace it was picked with. One
      // missing from this paint's list (its fleet read is briefly unreachable)
      // shows the owner for now and stays stored.
      const rememberedHost = aggregateDrainWorkspace === rememberedWorkspace ? rememberedAggregateHost() : null;
      aggregateDrainHost = ordered.includes(rememberedHost) ? rememberedHost : ordered[0] || null;
      paintDrainHostSelect(ordered, aggregateDrainHost);
      if (!aggregateDrainWorkspace || !aggregateDrainHost) return { capacity: { drain_phase: "idle" } };
      const chosenWorkspace = options.find(option => option.id === aggregateDrainWorkspace);
      if (!chosenWorkspace) return { capacity: { drain_phase: "idle" } };
      // Idle hosts need readiness too: checkout and owner information are
      // required to start the same window their concrete board would.
      return loadScopedDrainReadiness({
        fleet, workspace: chosenWorkspace, hostName: aggregateDrainHost, generation, mode: "aggregate", stillCurrent: guard,
      });
    }, payload => {
      if (guard() && payload) renderAutoDrain(payload, generation);
    }, "auto-drain-phase-label").catch(error => {
      if (guard()) {
        // Retire any retained controls too: a failed connection/readiness
        // refresh cannot leave a write enabled with stale authority.
        resetAggregateDrainSelection();
        for (const button of $("auto-drain-body")?.querySelectorAll("button") || []) button.disabled = true;
      }
      throw error;
    });
  }
  const visit = captureWorkspaceVisit();
  const remembered = rememberedBoardHost(workspace.id);
  // The owner's panel key stays the readiness path, so a poll does not reset
  // the card. A remembered follower is a different read and resets on switch.
  const panelScope = remembered ? `board:${workspace.id}:${remembered}` : "/api/workflows/auto/readiness";
  const fleet = loadFleetSnapshot({ refreshWhenStale: false });
  const ownerPath = () => {
    const concurrency = autoDrainConcurrencyValid() ? autoDrainConcurrencyValue() : "";
    const params = new URLSearchParams();
    if (concurrency) params.set("concurrency", concurrency);
    if (detailTaskId) params.set("also_task", detailTaskId);
    const query = params.size ? `?${params.toString()}` : "";
    return `/api/workflows/auto/readiness${query}`;
  };
  const readiness = requestPanel("auto-drain-body", panelScope, async () => {
    if (!remembered) {
      if (generation !== drainScopeGeneration) return null;
      boardDrainHost = null;
      void fleet.then(snapshot => {
        if (generation !== drainScopeGeneration || selectedWorkspace()?.id !== workspace.id) return;
        paintConcreteHostSelect(snapshot, workspace);
      }).catch(error => console.error(error));
      return fetchJson(ownerPath());
    }
    const snapshot = await fleet;
    if (generation !== drainScopeGeneration) return null;
    const resolved = paintConcreteHostSelect(snapshot, workspace);
    if (!resolved?.follower) {
      boardDrainHost = null;
      return fetchJson(ownerPath());
    }
    boardDrainHost = resolved.selected;
    return loadScopedDrainReadiness({
      fleet: snapshot,
      workspace,
      hostName: resolved.selected,
      generation,
      mode: "board",
      stillCurrent: () => generation === drainScopeGeneration && selectedWorkspace()?.id === workspace.id && boardDrainHost === resolved.selected,
    });
  }, payload => {
    if (payload && generation === drainScopeGeneration) renderAutoDrain(payload, generation);
  }, "auto-drain-phase-label").catch(error => {
    if (detailTaskId && visit.isCurrent() && generation === drainScopeGeneration && !boardDrainHost) {
      setDrainReadinessFailure({ id: detailTaskId, workspace_id: workspace.id }, error.message || String(error));
    }
    if (boardDrainHost && generation === drainScopeGeneration) {
      for (const button of $("auto-drain-body")?.querySelectorAll("button") || []) button.disabled = true;
    }
    throw error;
  });
  // The optional summary paints itself and contains its failures. A slow
  // forecast must not delay dashboard refresh or release of action controls.
  // The Tasks page's 30 s tick leaves it to the page's slow reads.
  if (forecast) void refreshBacklogForecast();
  return Promise.all([readiness, fleet]).then(() => {});
}


configureOperationsDrainControls({
  scope: payload => aggregateDrainScopes.get(payload) || null,
  render: renderAutoDrain,
  fetch: () => {
    // Action refreshes replace the shared snapshot just like a poll. Keep
    // the selected task in it even when it is outside the bounded backlog.
    const selectedTask = !taskState.pinnedExternalTask
      ? taskList(taskState.paneContext).find(task => task.id === taskState.selectedTaskId)
      : null;
    const detailTaskId = ["proposed", "backlog"].includes(selectedTask?.status) ? selectedTask.id : null;
    return fetchAndRenderAutoDrain({ detailTaskId });
  },
  holdAnnouncement: held => { holdDrainAnnouncement = held; },
});

// Readiness refreshes on every page to feed the chip and task readiness;
// `forecast: false` skips the backlog forecast read.
export async function fetchAndRenderAutoDrainPane({ detailTaskId = null, forecast = true } = {}) {
  await fetchAndRenderAutoDrain({ detailTaskId, forecast });
}
