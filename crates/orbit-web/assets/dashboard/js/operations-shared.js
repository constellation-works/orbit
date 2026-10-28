import { bindTaskLink, detailsPanel, el, formatClock, formatDateTime, getWorkspace, getWorkspaceRevision, hostWriteRefusal, onWorkspaceChange, runHref, fmtDuration, normalizeTaskStatus } from './common.js';
import { navigateToRun } from './router.js';

export const operationsState = {
  context: null,
  lastOperations: null,
  lastAutoTasks: null,
  lastAutoDrain: null,
  hostCronZone: null,
  aggregateFleetToken: 0,
};

export const $ = (id) => document.getElementById(id);
export const pendingOperations = new Set();
let unsubscribeWorkspace = null;
let showInactivePlugins = false;
const operationsResetCallbacks = new Set();
let refreshOperations = () => Promise.resolve();

export function configureOperationsRefresh(callback) {
  refreshOperations = callback;
}

export function registerOperationsReset(callback) {
  operationsResetCallbacks.add(callback);
}

function resetOperationsForWorkspace() {
  operationsState.lastOperations = null;
  operationsState.lastAutoTasks = null;
  operationsState.lastAutoDrain = null;
  for (const reset of operationsResetCallbacks) reset();
  for (const id of ["routine-operation-feedback", "clock-operation-feedback", "auto-task-operation-feedback", "auto-drain-operation-feedback", "job-operation-feedback"]) feedback(id, "", "");
}

export const OPERATION_FEEDBACK_CLEAR_MS = 60000;
const feedbackClearTimers = new Map();

function clearFeedbackTimer(id) {
  const timer = feedbackClearTimers.get(id);
  if (timer) {
    clearTimeout(timer);
    feedbackClearTimers.delete(id);
  }
}

function clearAllFeedbackTimers() {
  for (const timer of feedbackClearTimers.values()) {
    clearTimeout(timer);
  }
  feedbackClearTimers.clear();
}

function isWarningLine(line) {
  return /^\s*(?:warning|note|refresh failed):/i.test(line);
}

function expireFeedback(id) {
  feedbackClearTimers.delete(id);
  const node = $(id);
  if (!node) return;
  const currentText = node.textContent || "";
  const lines = currentText.split("\n");
  const warningLines = lines.filter(isWarningLine);
  if (warningLines.length > 0) {
    node.textContent = warningLines.join("\n");
  } else {
    feedback(id, "", "");
  }
}

export function triggerFeedbackClear(id) {
  const timer = feedbackClearTimers.get(id);
  if (timer) {
    clearTimeout(timer);
    expireFeedback(id);
  }
}

export function initOperations(nextContext) {
  operationsState.context = nextContext;
  unsubscribeWorkspace?.();
  clearAllFeedbackTimers();
  unsubscribeWorkspace = onWorkspaceChange(() => resetOperationsForWorkspace());
}

export function selectedWorkspace() {
  const selected = getWorkspace();
  if (!selected) return null;
  return operationsState.context.getWorkspaces().find((workspace) => workspace.id === selected) || null;
}

export function selectedWorkspaceName() {
  return selectedWorkspace()?.name || null;
}

// Why the selection cannot be read as one workspace, or "". Reads need only
// this: a session without forwarding-write authority still reads the selected
// host (only unsafe methods are refused), so Jobs and Auto-task definitions
// render there while workspaceReadOnlyReason keeps their controls refused.
export function workspaceSelectionReason() {
  const selected = getWorkspace();
  if (!selected) return "All-workspace mode is read-only. Select one workspace; auto-task definitions are workspace-scoped.";
  const workspace = selectedWorkspace();
  if (!workspace) return `Workspace ${selected} is not a concrete active selection.`;
  if (workspace.status && workspace.status !== "active") {
    return `Workspace ${workspace.name} is inactive; select an active workspace.`;
  }
  return "";
}

// Why a write to the selected workspace cannot be sent, or "": the host's
// forwarding-write refusal first, then the selection.
export function workspaceReadOnlyReason() {
  return hostWriteRefusal() || workspaceSelectionReason();
}

export function feedback(id, kind, message) {
  clearFeedbackTimer(id);
  const node = $(id);
  if (!node) return;
  node.className = `operation-feedback ${kind || ""}`.trim();
  node.textContent = message || "";

  if (kind === "success") {
    const timer = setTimeout(() => {
      expireFeedback(id);
    }, OPERATION_FEEDBACK_CLEAR_MS);
    feedbackClearTimers.set(id, timer);
  }
}

function looksLikeDuration(value) {
  const text = String(value).trim();
  return /\d+\s*(?:h|hr|hrs|hour|hours|min|mins|minute|minutes|s|sec|secs|seconds)\b/i.test(text)
    && Number.isNaN(new Date(text).getTime());
}

export function time(value) {
  if (!value) return "Not observed";
  if (looksLikeDuration(value)) return `Duration ${value}`;
  return formatDateTime(value);
}

export function cadenceText(seconds) {
  if (seconds == null || seconds === "") return "Inactive";
  const n = Number(seconds);
  if (!Number.isFinite(n)) return String(seconds);
  if (n % 60 === 0) {
    const minutes = n / 60;
    const label = minutes === 1 ? "every 1 minute" : `every ${minutes} minutes`;
    return `${label} (${n}s)`;
  }
  return `every ${n}s`;
}

export function clockUnavailable(clock) {
  return clock?.health === "unknown" || (typeof clock?.error === "string" && clock.error.length > 0);
}

export function clockUnavailableReason(clock) {
  return clock?.health_issue || clock?.error || "Clock state is unavailable; controls are disabled.";
}

export function clockServiceText(clock) {
  if (clockUnavailable(clock)) return "unknown";
  if (clock.running === true && !clock.enabled) return "active (disabled)";
  return clock.enabled ? "enabled" : "paused";
}

// What a waiting definition waits for, by the trigger that holds it. A
// definition without a trigger of its own (an auto-task) waits for deliveries.
function waitingText(trigger) {
  if (trigger?.state?.kind === "execution_failed") return "Waiting for failed tasks";
  if (trigger?.state) return "Waiting for task creation or edits";
  return "Waiting for deliveries";
}

export function nextEvaluationText(projection, fallbackAt, trigger) {
  const state = projection?.state;
  const at = projection?.at || (state === "disabled" || state === "paused" ? fallbackAt : null);
  const when = at ? time(at) : null;
  if (state === "disabled") return when ? `Disabled · hypothetical next ${when}` : "Disabled";
  if (state === "paused") return when ? `Paused · hypothetical next ${when}` : "Paused";
  if (state === "waiting") return waitingText(trigger);
  if (state === "never_observed") return "Never observed";
  if (state === "unavailable") return "Unavailable";
  if (state === "scheduled") return when || "Scheduled";
  return when || "Unavailable";
}

export function lastMintedText(definition) {
  if (!definition.last_minted_task_id) return "None";
  const status = definition.last_minted_task_status ? ` · ${normalizeTaskStatus(definition.last_minted_task_status)}` : "";
  const schedulerId = definition.last_evaluation?.last_task_id;
  const source = schedulerId && definition.last_minted_task_id !== schedulerId
    ? " · manual mint"
    : schedulerId
      ? " · scheduler"
      : "";
  return `${definition.last_minted_task_id}${status}${source}`;
}

export function clockTickText(value, clock) {
  if (value) {
    if (looksLikeDuration(value)) {
      return `Duration from boot ${value} (not a wall-clock tick)`;
    }
    return time(value);
  }
  if (clockUnavailable(clock)) return "Unknown";
  if (!clock.enabled && clock.running !== true) return "Paused";
  if (clock.schedulable) return "Armed; exact wall-clock time unavailable";
  return "Not scheduled";
}

export function field(label, value) {
  return el("div", { class: "operation-field" }, [
    el("span", { class: "operation-field-label", text: label }),
    el("span", { class: "operation-field-value", text: value == null || value === "" ? "—" : String(value) }),
  ]);
}

function lastFireText(fire) {
  if (!fire) return "Never";
  const when = time(fire.finished_at || fire.started_at);
  return fire.state ? `${fire.state} · ${when}` : when;
}

// "on task creation/edit · debounce 2m": what fires a state-triggered routine
// and how long it settles first.
function stateTriggerText(state) {
  const what = state.kind === "execution_failed" ? "on task execution failure" : "on task creation/edit";
  return state.debounce_minutes ? `${what} · debounce ${state.debounce_minutes}m` : what;
}

export function routineScheduleText(routine) {
  if (routine.trigger?.deliveries_landed) {
    return `${routine.trigger.deliveries_landed.threshold} verified deliveries on ${routine.trigger.deliveries_landed.branch}`;
  }
  if (routine.trigger?.state) return stateTriggerText(routine.trigger.state);
  return routine.cron || "—";
}

// Delivery- and state-triggered routines have no cron cadence to describe.
export function routineEventDriven(routine) {
  return Boolean(routine.trigger?.deliveries_landed || routine.trigger?.state);
}

function operationIdentity(name, state) {
  return el("div", { class: "operation-identity" }, [
    el("strong", { text: name }),
    el("span", { class: `operation-state ${state}`, text: state }),
  ]);
}

export function operationFact(label, value, title) {
  return el("div", { class: "operation-fact" }, [
    el("span", { class: "operation-fact-label", text: label }),
    el("span", { class: "operation-fact-value", text: value == null || value === "" ? "—" : String(value), title: title || "" }),
  ]);
}

export function operationDetails(key, children) {
  const panel = detailsPanel(key, { class: "operation-details" });
  panel.appendChild(el("summary", { text: "Details" }));
  // The summary stays in the row; only the opened body is the inset panel.
  const body = el("div", { class: "operation-details-body" });
  for (const child of children) {
    if (child) body.appendChild(child);
  }
  panel.appendChild(body);
  return panel;
}

export function actionReason(payload, action) {
  const selectionReason = workspaceReadOnlyReason();
  if (selectionReason) return selectionReason;
  const capability = payload.capabilities?.[action];
  if (capability) return capability.authorized === true ? "" : capability.reason || "Action unavailable. See session access above.";
  return "Action availability is unavailable. Refresh the dashboard server and this page.";
}

export function controlReason(payload, action = "routine_toggle") {
  return actionReason(payload, action) || "";
}

export function explainUnavailable(button, reason) {
  if (!reason) return button;
  // The wrapper is focusable because native disabled buttons cannot receive focus.
  const wrapper = el("span", { class: "operation-action-unavailable" }, [button]);
  wrapper.tabIndex = 0;
  wrapper.title = reason;
  wrapper.setAttribute("aria-label", `${button.getAttribute("aria-label") || button.textContent}: ${reason}`);
  return wrapper;
}

export function selectionSnapshot() {
  const workspace = getWorkspace();
  const revision = getWorkspaceRevision();
  return {
    workspace,
    revision,
    current: () => workspace === getWorkspace() && revision === getWorkspaceRevision(),
  };
}

// Keep the guard until readback finishes; old results never replace a new selection.
export async function runOperation({ selection, key, feedbackId, pending, failure, render, request, refresh, success }) {
  if (!selection.current() || pendingOperations.has(key)) return;
  pendingOperations.add(key);
  feedback(feedbackId, "pending", pending);
  render();
  try {
    const result = await request();
    if (!selection.current()) return;
    feedback(feedbackId, "success", success(result));
    if (result.task_id) {
      const link = el("a", { class: "mono operation-run-link operation-task-link operation-link", text: ` Open ${result.task_id} →` });
      bindTaskLink(link, result.task_id, { workspace: selection.workspace });
      $(feedbackId)?.appendChild(link);
    }
    try {
      await refresh();
    } catch (error) {
      if (selection.current()) {
        $(feedbackId)?.appendChild(el("span", {
          text: `\nRefresh failed: ${error.message}. The action succeeded; refresh before submitting another action.`,
        }));
      }
    }
  } catch (error) {
    if (selection.current()) feedback(feedbackId, "error", `${failure}: ${error.message}`);
  } finally {
    pendingOperations.delete(key);
    // Repaint current cached data only; host clock guards span workspaces.
    render();
  }
}

// ---------------------------------------------------------------------------
// Shared row furniture for the Operations subtabs. Routines, auto-tasks and
// jobs are all "a named thing, its trigger, when it fires next, what happened
// last time, one control" — so they share one grid row, one switch and one
// group header instead of three card vocabularies.

export function relativeTime(value, now = Date.now()) {
  if (!value) return null;
  const at = new Date(value).getTime();
  if (Number.isNaN(at)) return null;
  const diff = at - now;
  const abs = Math.abs(diff);
  if (abs < 45_000) return diff > 0 ? "in under a minute" : "just now";
  let text;
  if (abs < 86_400_000) {
    // Round the total once; rounding the remainder can carry into a "60 min".
    const totalMinutes = Math.round(abs / 60_000);
    const hours = Math.floor(totalMinutes / 60);
    const minutes = totalMinutes % 60;
    if (hours === 0) text = `${minutes} min`;
    else text = minutes ? `${hours} h ${minutes} min` : `${hours} h`;
  } else {
    text = `${Math.round(abs / 86_400_000)} d`;
  }
  return diff > 0 ? `in ${text}` : `${text} ago`;
}

export function durationText(ms) {
  const text = fmtDuration(ms);
  return text === "-" ? null : text;
}

// Local wall-clock time with its zone, so a next-fire time is never read as
// the UTC of the cron trigger beside it.
export function clockHm(value, { zone = true } = {}) {
  return formatClock(value, { seconds: false, zone });
}

const CRON_DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

// The zone cron triggers are evaluated in (the host's, from the routines and
// auto-tasks payloads). Unknown until a payload names it; a trigger is then
// shown without a zone claim rather than with a wrong one.
export function noteCronZone(payload) {
  if (payload?.cron_zone) operationsState.hostCronZone = payload.cron_zone;
}

// "PDT" for an IANA host zone, else "UTC-07:00" from the offset.
function cronZoneLabel(zone) {
  if (!zone) return "";
  if (zone.name) {
    try {
      const part = new Intl.DateTimeFormat("en-US", { timeZone: zone.name, timeZoneName: "short" })
        .formatToParts(new Date()).find((p) => p.type === "timeZoneName");
      if (part?.value) return part.value;
    } catch {
      // Unknown to this browser: fall back to the offset.
    }
  }
  const offset = Number(zone.offset_seconds);
  if (!Number.isFinite(offset)) return "";
  if (offset === 0) return "UTC";
  const minutes = Math.abs(Math.round(offset / 60));
  const pad = (v) => String(v).padStart(2, "0");
  return `UTC${offset < 0 ? "-" : "+"}${pad(Math.floor(minutes / 60))}:${pad(minutes % 60)}`;
}

// A readable cadence for the common cron shapes routines use; anything else
// is shown verbatim so nothing is misdescribed. Clock times are in the host
// zone the scheduler evaluates cron in, not UTC.
export function cronText(cron) {
  const parts = String(cron || "").trim().split(/\s+/);
  if (parts.length !== 5) return cron || "—";
  const [minute, hour, dom, month, dow] = parts;
  const pad = (v) => String(v).padStart(2, "0");
  const label = cronZoneLabel(operationsState.hostCronZone);
  const zone = label ? ` ${label}` : "";
  if (dom !== "*" || month !== "*") return cron;
  if (minute === "*" && hour === "*" && dow === "*") return "every minute";
  const every = /^\*\/(\d+)$/.exec(minute);
  if (every && hour === "*" && dow === "*") return `every ${every[1]} min`;
  if (/^\d+$/.test(minute) && hour === "*" && dow === "*") return `hourly at :${pad(minute)}`;
  if (/^\d+$/.test(minute) && /^\d+$/.test(hour) && dow === "*") return `daily ${pad(hour)}:${pad(minute)}${zone}`;
  if (/^\d+$/.test(minute) && /^\d+$/.test(hour) && /^\d$/.test(dow)) {
    return `weekly ${CRON_DAYS[Number(dow)] || dow} ${pad(hour)}:${pad(minute)}${zone}`;
  }
  return cron;
}

// One switch for enable/disable. The visible text stays "Enable"/"Disable"
// (or "Pending…") for the readback contract; CSS draws it as a switch, and
// aria-label names the item independently of its enabled state.
export function operationSwitch({ name, on, text, pending, reason }) {
  const button = el("button", {
    class: `operation-switch ${on ? "on" : "off"}${pending ? " pending" : ""}`,
    text,
    title: reason || text,
  });
  button.type = "button";
  button.setAttribute("aria-label", name);
  button.setAttribute("role", "switch");
  button.setAttribute("aria-checked", on ? "true" : "false");
  button.disabled = Boolean(reason) || Boolean(pending);
  return button;
}

export function operationGroup(tone, title, count, hint) {
  const group = el("section", { class: "operation-group" });
  group.appendChild(el("div", { class: "operation-group-head" }, [
    el("div", { class: "operation-group-title" }, [
      el("span", { class: `operation-group-dot ${tone}` }),
      el("strong", { text: title }),
      el("span", { class: "operation-group-count mono", text: String(count) }),
    ]),
    hint ? el("span", { class: "operation-group-hint", text: hint }) : null,
  ]));
  return group;
}

export function operationColumns(labels) {
  const row = el("div", { class: "operation-columns col-head" }, labels.map((label) => el("span", { text: label })));
  row.setAttribute("aria-hidden", "true");
  return row;
}

export function operationCell(label, children, extraClass = "") {
  const cell = el("div", { class: `operation-cell ${extraClass}`.trim() }, children.filter(Boolean));
  cell.dataset.label = label;
  return cell;
}

export function whenCell(label, at, { fallback = "—", muted = false } = {}) {
  const relative = relativeTime(at);
  if (!relative) return operationCell(label, [el("span", { class: `operation-cell-main${muted ? " muted" : ""}`, text: fallback })]);
  return operationCell(label, [
    el("span", { class: `operation-cell-main${muted ? " muted" : ""}`, text: relative, title: time(at) }),
    el("span", { class: "operation-cell-sub mono", text: clockHm(at) }),
  ]);
}

export function outcomeDot(state) {
  const tone = state === "succeeded" || state === "success" || state === "ok" ? "ok"
    : state === "failed" || state === "error" || state === "timed_out" ? "failed"
    : state === "running" || state === "pending" || state === "intent" || state === "dispatched" ? "running"
    : "idle";
  return el("span", { class: `operation-dot ${tone}`, title: state || "never" });
}

export function runLink(runId, workspaceId, text = runId) {
  const link = el("a", { class: "mono operation-run-link", text, title: `Open run ${runId}` });
  link.href = runHref(runId, { workspace: workspaceId || null });
  link.addEventListener("click", (event) => {
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0) return;
    event.preventDefault();
    navigateToRun(runId, workspaceId || null);
  });
  return link;
}

export function operationStat(tone, label, value, note) {
  return el("div", { class: `operation-stat ${tone}` }, [
    el("span", { class: "operation-field-label", text: label }),
    el("span", { class: "operation-stat-value mono", text: String(value) }),
    note ? el("span", { class: "operation-stat-note", text: note }) : null,
  ]);
}

export function setRailSubtabCount(id, text) {
  const node = $(id);
  if (node) node.textContent = text || "";
}

export function withInactivePlugins(path) {
  return showInactivePlugins ? `${path}?include_inactive_plugins=true` : path;
}

// Offered only when something is hidden (or already shown). Flipping it
// refetches every Operations panel, so routines and auto-tasks agree.
export function inactivePluginToggle(hiddenCount) {
  if (!showInactivePlugins && !hiddenCount) return null;
  const button = el("button", {
    class: "operation-button secondary operation-inactive-toggle",
    text: showInactivePlugins ? "Hide plugin-off definitions" : `Show ${hiddenCount} hidden · plugin off`,
    title: "Definitions seeded by a plugin that is switched off here never fire.",
  });
  button.type = "button";
  button.setAttribute("aria-pressed", showInactivePlugins ? "true" : "false");
  button.addEventListener("click", () => {
    showInactivePlugins = !showInactivePlugins;
    return refreshOperations();
  });
  return el("p", { class: "operation-control-note operation-inactive-note" }, [button]);
}

// Definitions that never fire here — plugin-parked (listed on request only) or
// a replica's owner-only routines — sit outside every count, timeline and
// next-fire summary: the row is the name, what it would run, and the reason.
export function inactivePluginGroup(entries, noun, title = "Plugin off", summary = `seeded by a plugin that is switched off here · never ${noun}`) {
  const group = operationGroup("", title, entries.length, summary);
  group.className += " inactive-plugin-group";
  for (const entry of entries) {
    group.appendChild(el("article", { class: "operation-card operation-row inactive-plugin-card" }, [
      el("div", { class: "operation-row-head" }, [
        operationCell("Definition", [
          el("div", { class: "operation-identity" }, [
            el("strong", { text: entry.name }),
            el("span", { class: "operation-state inactive", text: entry.state || "inactive" }),
          ]),
          entry.detail ? el("span", { class: "operation-cell-sub mono", text: entry.detail }) : null,
        ], "operation-cell-identity"),
        operationCell("Why", [el("span", { class: "operation-cell-main muted", text: entry.reason || "" })]),
      ]),
    ]));
  }
  return group;
}


export function taskLink(taskId, workspaceId) {
  const link = el("a", { class: "mono operation-run-link operation-task-link operation-link", text: taskId, title: `Open ${taskId}` });
  bindTaskLink(link, taskId, { workspace: workspaceId || "" });
  return link;
}

export function autoDrainNumber(value) {
  return value == null || value === "" ? NaN : Number(value);
}

// Capacity in words: how many leaf runs hold a slot against the limit, a bar
// that shows any overflow past the limit, and one sentence on what a window
// started now would do with that.
