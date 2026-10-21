// Routine-definition, host clock, and auto-task operations [ORB-10875, ORB-10876].

import { normalizeTaskStatus, captureWorkspaceVisit, requestPanel, describePullSettlements, copyText, detailsPanel, el, infoHint, fetchJson, formatClock, formatDateTime, getWorkspace, getWorkspaceRevision, hostWriteRefusal, isAggregateView, onWorkspaceChange, postJson, reviewLayerText, runHref, statusPill, submissionNoticeLines, fmtDuration, bindTaskLink } from './common.js';
import { navigateToRun, setActiveTab } from './router.js';
import { chooseDockMode, setDockMode } from './log-tail.js';
import { renderAutomation } from './automation.js';
import { FLEET_CACHE_MS, fetchFleetDrains, fleetDrainLabel, peekFleetDrains, renderFleetDrains } from './fleet-drains.js';
import { cpuLoadMultiple } from './host-resources.js';
import { DRAIN_LOCK_REASONS, DRAIN_POOLS, drainCrewNote, drainReason, drainReadinessView, setDrainReadiness } from './drain-waits.js';
import { sparkline, svg } from './sparkline.js';
import { AUTO_DRAIN_APPROVE_RULE, drainStartConcurrencyQuery, initDrainStart, renderDrainStart, resetDrainStart } from './drain-start.js';
import { hideDrainStarted, renderDrainStarted } from './drain-started.js';

const $ = (id) => document.getElementById(id);
const pendingOperations = new Set();
const UNCONDITIONAL_MINT_WARNING = "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.";
let lastOperations = null;
let routineFailureRequest = 0;
let lastAutoTasks = null;
let lastAutoDrain = null;
let announcedDrainState = null;
// Set while Start or Stop is refreshing the card after reporting its result, so
// the state change that result caused is not announced over the result.
let holdDrainAnnouncement = false;
let context = null;
let unsubscribeWorkspace = null;
// The operator's unapplied cadence choice, held outside the rebuilt <select>
// so a background refresh cannot revert it. Host-scoped, like the clock itself.
let pendingCadenceSeconds = null;
// Definitions a plugin seeded stay hidden while that plugin is off where they
// live: they never fire. The operator can list them again, marked inactive,
// to find or delete the file the reason names.
let showInactivePlugins = false;

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
  context = nextContext;
  unsubscribeWorkspace?.();
  clearAllFeedbackTimers();
  initDrainStart({
    rerender: () => { if (lastAutoDrain) renderAutoDrain(lastAutoDrain); },
    lastPayload: () => lastAutoDrain,
    feedback,
    pending: pendingOperations,
    refreshAfterAction: refreshDrainAfterAction,
    selectedWorkspace,
    readOnlyReason: workspaceReadOnlyReason,
    counts: autoDrainCounts,
    throttledResources,
    throttleNames,
    // A view is a route only while the Drain page shows; elsewhere the dock
    // just switches.
    showOverview: () => (window.location.hash.startsWith("#drain") ? chooseDockMode : setDockMode)("drain"),
    showStart: () => (window.location.hash.startsWith("#drain") ? chooseDockMode : setDockMode)("start"),
  });
  unsubscribeWorkspace = onWorkspaceChange(() => {
    lastOperations = lastAutoTasks = lastAutoDrain = lastJobs = null;
    resetDrainStart();
    hideDrainStarted();
    lastDrainLocks = null;
    updateRoutineFailureBadge([]);
    announcedDrainState = null;
    updateDrainIndicators("idle", "idle");
    const start = $("auto-drain-start");
    if (start) start.hidden = true;
    for (const id of ["drain-pass-body", "drain-waits-body"]) {
      const body = $(id);
      if (body) body.textContent = "";
    }
    hideDrainPageExtras();
    for (const id of ["routine-operation-feedback", "clock-operation-feedback", "auto-task-operation-feedback", "auto-drain-operation-feedback", "job-operation-feedback"]) feedback(id, "", "");
  });
}

function selectedWorkspace() {
  const selected = getWorkspace();
  if (!selected) return null;
  return context.getWorkspaces().find((workspace) => workspace.id === selected) || null;
}

function selectedWorkspaceName() {
  return selectedWorkspace()?.name || null;
}

function workspaceReadOnlyReason() {
  const hostRefusal = hostWriteRefusal();
  if (hostRefusal) return hostRefusal;
  const selected = getWorkspace();
  if (!selected) return "All-workspace mode is read-only. Select one workspace; auto-task definitions are workspace-scoped.";
  const workspace = selectedWorkspace();
  if (!workspace) return `Workspace ${selected} is not a concrete active selection.`;
  if (workspace.status && workspace.status !== "active") {
    return `Workspace ${workspace.name} is inactive; select an active workspace.`;
  }
  return "";
}

function feedback(id, kind, message) {
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

function time(value) {
  if (!value) return "Not observed";
  if (looksLikeDuration(value)) return `Duration ${value}`;
  return formatDateTime(value);
}

function cadenceText(seconds) {
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

function clockUnavailable(clock) {
  return clock?.health === "unknown" || (typeof clock?.error === "string" && clock.error.length > 0);
}

function clockUnavailableReason(clock) {
  return clock?.health_issue || clock?.error || "Clock state is unavailable; controls are disabled.";
}

function clockServiceText(clock) {
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

function nextEvaluationText(projection, fallbackAt, trigger) {
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

function lastMintedText(definition) {
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

function clockTickText(value, clock) {
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

function field(label, value) {
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

function routineScheduleText(routine) {
  if (routine.trigger?.deliveries_landed) {
    return `${routine.trigger.deliveries_landed.threshold} verified deliveries on ${routine.trigger.deliveries_landed.branch}`;
  }
  if (routine.trigger?.state) return stateTriggerText(routine.trigger.state);
  return routine.cron || "—";
}

// Delivery- and state-triggered routines have no cron cadence to describe.
function routineEventDriven(routine) {
  return Boolean(routine.trigger?.deliveries_landed || routine.trigger?.state);
}

function operationIdentity(name, state) {
  return el("div", { class: "operation-identity" }, [
    el("strong", { text: name }),
    el("span", { class: `operation-state ${state}`, text: state }),
  ]);
}

function operationFact(label, value, title) {
  return el("div", { class: "operation-fact" }, [
    el("span", { class: "operation-fact-label", text: label }),
    el("span", { class: "operation-fact-value", text: value == null || value === "" ? "—" : String(value), title: title || "" }),
  ]);
}

function operationDetails(key, children) {
  const panel = detailsPanel(key, { class: "operation-details" });
  panel.appendChild(el("summary", { text: "Details" }));
  for (const child of children) {
    if (child) panel.appendChild(child);
  }
  return panel;
}

function actionReason(payload, action) {
  const selectionReason = workspaceReadOnlyReason();
  if (selectionReason) return selectionReason;
  const capability = payload.capabilities?.[action];
  if (capability) return capability.authorized === true ? "" : capability.reason || "Action unavailable. See session access above.";
  return "Action availability is unavailable. Refresh the dashboard server and this page.";
}

function controlReason(payload, action = "routine_toggle") {
  return actionReason(payload, action) || "";
}

function explainUnavailable(button, reason) {
  if (!reason) return button;
  // The wrapper is focusable because native disabled buttons cannot receive focus.
  const wrapper = el("span", { class: "operation-action-unavailable" }, [button]);
  wrapper.tabIndex = 0;
  wrapper.title = reason;
  wrapper.setAttribute("aria-label", `${button.getAttribute("aria-label") || button.textContent}: ${reason}`);
  return wrapper;
}

function selectionSnapshot() {
  const workspace = getWorkspace();
  const revision = getWorkspaceRevision();
  return {
    workspace,
    revision,
    current: () => workspace === getWorkspace() && revision === getWorkspaceRevision(),
  };
}

// Keep the guard until readback finishes; old results never replace a new selection.
async function runOperation({ selection, key, feedbackId, pending, failure, render, request, refresh, success }) {
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

function relativeTime(value, now = Date.now()) {
  if (!value) return null;
  const at = new Date(value).getTime();
  if (Number.isNaN(at)) return null;
  const diff = at - now;
  const abs = Math.abs(diff);
  if (abs < 45_000) return diff > 0 ? "in under a minute" : "just now";
  let text;
  if (abs < 3_600_000) {
    text = `${Math.round(abs / 60_000)} min`;
  } else if (abs < 86_400_000) {
    const hours = Math.floor(abs / 3_600_000);
    const minutes = Math.round((abs % 3_600_000) / 60_000);
    text = minutes ? `${hours} h ${minutes} min` : `${hours} h`;
  } else {
    text = `${Math.round(abs / 86_400_000)} d`;
  }
  return diff > 0 ? `in ${text}` : `${text} ago`;
}

function durationText(ms) {
  const text = fmtDuration(ms);
  return text === "-" ? null : text;
}

// Local wall-clock time with its zone, so a next-fire time is never read as
// the UTC of the cron trigger beside it.
function clockHm(value, { zone = true } = {}) {
  return formatClock(value, { seconds: false, zone });
}

const CRON_DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const TIMELINE_ROWS = 4;

// The zone cron triggers are evaluated in (the host's, from the routines and
// auto-tasks payloads). Unknown until a payload names it; a trigger is then
// shown without a zone claim rather than with a wrong one.
let hostCronZone = null;

function noteCronZone(payload) {
  if (payload?.cron_zone) hostCronZone = payload.cron_zone;
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
function cronText(cron) {
  const parts = String(cron || "").trim().split(/\s+/);
  if (parts.length !== 5) return cron || "—";
  const [minute, hour, dom, month, dow] = parts;
  const pad = (v) => String(v).padStart(2, "0");
  const label = cronZoneLabel(hostCronZone);
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
function operationSwitch({ name, on, text, pending, reason }) {
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

function operationGroup(tone, title, count, hint) {
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

function operationColumns(labels) {
  const row = el("div", { class: "operation-columns col-head" }, labels.map((label) => el("span", { text: label })));
  row.setAttribute("aria-hidden", "true");
  return row;
}

function operationCell(label, children, extraClass = "") {
  const cell = el("div", { class: `operation-cell ${extraClass}`.trim() }, children.filter(Boolean));
  cell.dataset.label = label;
  return cell;
}

function whenCell(label, at, { fallback = "—", muted = false } = {}) {
  const relative = relativeTime(at);
  if (!relative) return operationCell(label, [el("span", { class: `operation-cell-main${muted ? " muted" : ""}`, text: fallback })]);
  return operationCell(label, [
    el("span", { class: `operation-cell-main${muted ? " muted" : ""}`, text: relative, title: time(at) }),
    el("span", { class: "operation-cell-sub mono", text: clockHm(at) }),
  ]);
}

function outcomeDot(state) {
  const tone = state === "succeeded" || state === "success" || state === "ok" ? "ok"
    : state === "failed" || state === "error" || state === "timed_out" ? "failed"
    : state === "running" || state === "pending" || state === "intent" || state === "dispatched" ? "running"
    : "idle";
  return el("span", { class: `operation-dot ${tone}`, title: state || "never" });
}

function runLink(runId, workspaceId, text = runId) {
  const link = el("a", { class: "mono operation-run-link", text, title: `Open run ${runId}` });
  link.href = runHref(runId, { workspace: workspaceId || null });
  link.addEventListener("click", (event) => {
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0) return;
    event.preventDefault();
    navigateToRun(runId, workspaceId || null);
  });
  return link;
}

function operationStat(tone, label, value, note) {
  return el("div", { class: `operation-stat ${tone}` }, [
    el("span", { class: "operation-field-label", text: label }),
    el("span", { class: "operation-stat-value mono", text: String(value) }),
    note ? el("span", { class: "operation-stat-note", text: note }) : null,
  ]);
}

function setRailSubtabCount(id, text) {
  const node = $(id);
  if (node) node.textContent = text || "";
}

function withInactivePlugins(path) {
  return showInactivePlugins ? `${path}?include_inactive_plugins=true` : path;
}

// Offered only when something is hidden (or already shown). Flipping it
// refetches every Operations panel, so routines and auto-tasks agree.
function inactivePluginToggle(hiddenCount) {
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
    return fetchAndRenderOperations();
  });
  return el("p", { class: "operation-control-note operation-inactive-note" }, [button]);
}

// Definitions that never fire here — plugin-parked (listed on request only) or
// a replica's owner-only routines — sit outside every count, timeline and
// next-fire summary: the row is the name, what it would run, and the reason.
function inactivePluginGroup(entries, noun, title = "Plugin off", summary = `seeded by a plugin that is switched off here · never ${noun}`) {
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

// ---------------------------------------------------------------------------
// Routines

function routineButton(payload, routine) {
  const selection = selectionSnapshot();
  const nextEnabled = !routine.enabled;
  const key = `routine:${selection.workspace}:${routine.name}`;
  const reason = controlReason(payload);
  const pending = pendingOperations.has(key);
  const button = operationSwitch({
    name: `Routine: ${routine.name}`,
    on: Boolean(routine.enabled),
    text: pending ? "Pending…" : nextEnabled ? "Enable" : "Disable",
    pending,
    reason: reason || "",
  });
  if (!reason) button.title = `${nextEnabled ? "Enable" : "Disable"} ${routine.name} → ${routine.target}`;
  button.addEventListener("click", () => {
    if (reason || !selection.current()) return;
    return runOperation({
      selection, key, feedbackId: "routine-operation-feedback",
      pending: `Updating ${routine.name} → ${routine.target}…`, failure: "Routine change failed",
      render: () => { if (lastOperations) renderOperations(lastOperations); },
      request: () => postJson("/api/routines/toggle", {
        name: routine.name, source: routine.source, target: routine.target,
        machine_name: payload.machine_name, expected_enabled: routine.enabled, enabled: nextEnabled,
      }),
      refresh: fetchAndRenderOperations,
      success: (result) => `${result.message}: ${routine.name} → ${routine.target}.`,
    });
  });
  return explainUnavailable(button, reason);
}

function routineNextAt(routine) {
  return routine.next_evaluation?.at || routine.next_due || null;
}

function routineState(routine) {
  if (!routine.enabled) return "paused";
  return routine.effective === false ? "blocked" : "active";
}

function jobIdFromTarget(target) {
  const match = /^job:(.+)$/.exec(String(target || ""));
  return match ? match[1] : null;
}

function jobLink(target) {
  const jobId = jobIdFromTarget(target);
  if (!jobId) return el("span", { class: "mono", text: target || "—" });
  const link = el("a", { class: "mono operation-run-link operation-job-link operation-link", text: jobId, title: `Open ${jobId} under Jobs` });
  link.href = `#jobs/catalogue?job=${encodeURIComponent(jobId)}`;
  return link;
}

// One cron field as a membership test, or null for a shape this does not
// read (names, `L`, `?`): the caller then plots only the slot the server named.
function cronFieldMatcher(field, min, max) {
  const accepted = new Set();
  for (const part of String(field).split(",")) {
    const match = /^(?:\*|(\d+)(?:-(\d+))?)(?:\/(\d+))?$/.exec(part);
    if (!match) return null;
    const step = match[3] === undefined ? 1 : Number(match[3]);
    if (step < 1) return null;
    const ranged = part.startsWith("*") ? [min, max] : [Number(match[1]), match[2] === undefined ? (match[3] === undefined ? Number(match[1]) : max) : Number(match[2])];
    if (ranged[0] < min || ranged[1] > max || ranged[0] > ranged[1]) return null;
    for (let value = ranged[0]; value <= ranged[1]; value += step) accepted.add(value);
  }
  return (value) => accepted.has(value);
}

// Every fire of a cron routine from its next slot to the end of the window.
// The server names only the next slot; the rest are the minutes after it that
// the cron expression matches, read in the host's cron zone. A zone-sensitive
// expression on a host whose zone is unknown keeps just the named slot.
function routineFireTimes(routine, first, until) {
  const fields = String(routine.cron || "").trim().split(/\s+/);
  if (fields.length !== 5 || routineEventDriven(routine)) return [first];
  const matchers = [
    cronFieldMatcher(fields[0], 0, 59), cronFieldMatcher(fields[1], 0, 23), cronFieldMatcher(fields[2], 1, 31),
    cronFieldMatcher(fields[3], 1, 12), cronFieldMatcher(fields[4], 0, 7),
  ];
  const offset = Number(hostCronZone?.offset_seconds);
  if (matchers.includes(null) || (!Number.isFinite(offset) && fields.slice(1).some((f) => f !== "*"))) return [first];
  const [minute, hour, dom, month, dow] = matchers;
  const zoneOffset = Number.isFinite(offset) ? offset * 1000 : 0;
  const times = [first];
  for (let at = Math.floor(first / 60_000) * 60_000 + 60_000; at <= until; at += 60_000) {
    const local = new Date(at + zoneOffset);
    const weekday = local.getUTCDay();
    // Day-of-month and day-of-week combine with OR unless one is a wildcard.
    const days = fields[2] !== "*" && fields[4] !== "*"
      ? dom(local.getUTCDate()) || dow(weekday) || (weekday === 0 && dow(7))
      : dom(local.getUTCDate()) && (dow(weekday) || (weekday === 0 && dow(7)));
    if (minute(local.getUTCMinutes()) && hour(local.getUTCHours()) && month(local.getUTCMonth() + 1) && days) times.push(at);
  }
  return times;
}

// The next hour as a strip: where each routine's slots land, with paused
// routines drawn hollow so an operator sees skipped slots, not only live ones.
// A routine due several times in the hour is drawn at each of its slots.
function routineTimeline(routines, now = Date.now()) {
  const horizon = 60 * 60_000;
  const due = routines
    .map((routine) => ({ routine, first: new Date(routineNextAt(routine) || NaN).getTime() }))
    .filter(({ first }) => Number.isFinite(first) && first >= now - 60_000 && first <= now + horizon)
    .flatMap(({ routine, first }) => routineFireTimes(routine, first, now + horizon)
      .map((at, index) => ({ routine, at, repeat: index > 0 })))
    .sort((a, b) => a.at - b.at);
  const strip = el("section", { class: "operation-timeline" });
  strip.setAttribute("aria-label", "Next hour");
  const fires = due.filter(({ routine }) => routine.enabled && routine.effective !== false).length;
  const active = routines.filter((routine) => routineState(routine) === "active").length;
  strip.appendChild(el("div", { class: "operation-timeline-head" }, [
    el("strong", { text: "Next hour" }),
    el("span", { class: "operation-timeline-range mono", text: `${clockHm(now, { zone: false })} → ${clockHm(now + horizon)}` }),
    el("span", { class: "operation-timeline-summary", text: due.length
      ? `${fires} fire${fires === 1 ? "" : "s"} from ${active} active routine${active === 1 ? "" : "s"}`
      : "nothing is due in the next hour" }),
    el("span", { class: "operation-timeline-legend" }, [
      el("span", { class: "operation-timeline-key fires", text: "will fire" }),
      el("span", { class: "operation-timeline-key skipped", text: "paused · slot skipped" }),
    ]),
  ]));
  const track = el("div", { class: "operation-timeline-track" });
  track.appendChild(el("div", { class: "operation-timeline-axis" }));
  // Labels are stacked into rows so two routines due in the same minute
  // stay legible: each label takes the first row where it does not overlap
  // the previous label in that row (width estimated from the name length).
  // A routine's later slots reuse its first slot's row and carry no label of
  // their own, so a frequent routine marks every slot without repeating its name.
  const rowEnds = [];
  const routineRows = new Map();
  let rows = 0;
  due.forEach(({ routine, at, repeat }) => {
    const pct = Math.min(100, Math.max(0, ((at - now) / horizon) * 100));
    let row = routineRows.get(routine.name);
    if (!repeat || row === undefined) {
      const halfWidth = routine.name.length * 0.42;
      row = rowEnds.findIndex((end) => end <= pct - halfWidth);
      if (row < 0) row = Math.min(rowEnds.length, TIMELINE_ROWS - 1);
      rowEnds[row] = pct + halfWidth;
      routineRows.set(routine.name, row);
    }
    rows = Math.max(rows, row + 1);
    const fires = routine.enabled && routine.effective !== false;
    const tick = el("div", { class: `operation-timeline-tick ${fires ? "fires" : "skipped"} row-${row}` }, [
      el("span", { class: "operation-timeline-mark" }),
      repeat ? null : el("span", { class: "operation-timeline-label mono", text: routine.name }),
    ]);
    tick.style.left = `${pct}%`;
    tick.title = `${routine.name} · ${clockHm(at)}${fires ? "" : " · paused"}`;
    track.appendChild(tick);
  });
  track.className = `operation-timeline-track rows-${Math.max(rows, 1)}`;
  strip.appendChild(track);
  return strip;
}

function routineFailureText(routine) {
  const streak = routine.failure_streak;
  if (!streak?.count) return "";
  return `Failing ${streak.truncated ? "at least " : ""}${streak.count} fire${streak.count === 1 ? "" : "s"} in a row since ${time(streak.since)}${streak.truncated ? " or earlier" : ""}`;
}

function failingRoutines(routines) {
  return routines.filter(routine => routine.failure_streak?.count >= 3);
}

function scopedRoutines(payload) {
  const workspace = selectedWorkspaceName();
  return workspace ? (payload.routines || []).filter(routine => routine.source === workspace) : [];
}

function updateRoutineFailureBadge(routines) {
  routineFailureRequest += 1;
  const node = $("rail-count-routine-failures");
  if (!node) return;
  const failures = failingRoutines(routines);
  node.hidden = failures.length === 0;
  node.textContent = failures.length ? `${failures.length} routine${failures.length === 1 ? "" : "s"}` : "";
  node.title = failures.length
    ? `${failures.map(routine => `${routine.name}: ${routineFailureText(routine)}`).join("; ")}. See Automation › Routines.`
    : "";
}

// Keep Health's warning visible from every tab, including before Routines is
// opened. A reply from an earlier host/workspace visit must not restore it.
export async function peekRoutineFailures() {
  if (isAggregateView() || !selectedWorkspaceName()) {
    updateRoutineFailureBadge([]);
    return;
  }
  const visit = captureWorkspaceVisit();
  const request = ++routineFailureRequest;
  try {
    const payload = await fetchJson("/api/routines");
    if (visit.isCurrent() && request === routineFailureRequest) updateRoutineFailureBadge(scopedRoutines(payload));
  } catch (_) {
    if (visit.isCurrent() && request === routineFailureRequest) updateRoutineFailureBadge([]);
  }
}

function routineFireStrip(routine, workspaceId) {
  const fires = routine.recent_fires || (routine.last_fire ? [routine.last_fire] : []);
  if (!fires.length) return null;
  const strip = el("div", { class: "routine-fire-strip", role: "group", "aria-label": "Recent fires, oldest to newest" });
  for (const fire of [...fires].reverse()) {
    const label = `${fire.state || "unknown"} · ${time(fire.slot || fire.started_at)} · attempt ${fire.attempt || 1}${fire.run_id ? ` · Open run ${fire.run_id}` : " · No run was dispatched"}`;
    const node = fire.run_id ? runLink(fire.run_id, workspaceId, "") : el("span");
    node.className = "routine-fire";
    node.title = label;
    node.setAttribute("aria-label", label);
    node.appendChild(outcomeDot(fire.state));
    strip.appendChild(node);
  }
  return strip;
}

function routineFailureSummary(routines) {
  const failures = failingRoutines(routines);
  if (!failures.length) return null;
  return el("aside", { class: "routine-failures", role: "status" }, [
    el("strong", { text: `${failures.length} routine${failures.length === 1 ? "" : "s"} failing repeatedly` }),
    ...failures.map(routine => el("div", { text: `${routine.name} · ${routineFailureText(routine)}` })),
  ]);
}

function routineRow(payload, routine, workspaceId) {
  const fire = routine.last_fire;
  const state = routineState(routine);
  const detailsKey = `routine:${routine.name}`;
  const nextAt = routineNextAt(routine);
  const card = el("article", { class: `operation-card operation-row routine-card ${state}` });
  const identity = el("div", { class: "operation-identity" }, [
    el("strong", { text: routine.name }),
    state === "blocked" ? el("span", { class: "operation-state blocked", text: "blocked" }) : null,
  ]);
  const cadence = routineEventDriven(routine)
    ? [el("span", { class: "operation-cell-main", text: routineScheduleText(routine) })]
    : [
      el("span", { class: "operation-cell-main", text: cronText(routine.cron) }),
      routine.cron && cronText(routine.cron) !== routine.cron ? el("span", { class: "operation-cell-sub mono", text: routine.cron }) : null,
    ];
  const lastRun = fire
    ? [
      el("span", { class: "operation-cell-main operation-outcome" }, [
        outcomeDot(fire.state),
        el("span", { text: `${fire.state || "unknown"} ${relativeTime(fire.finished_at || fire.started_at) || ""}`.trim(), title: time(fire.finished_at || fire.started_at) }),
      ]),
      el("span", { class: "operation-cell-sub" }, [
        durationText(fire.duration_ms) ? el("span", { class: "mono", text: durationText(fire.duration_ms) }) : null,
        fire.run_id ? runLink(fire.run_id, workspaceId) : null,
      ].filter(Boolean)),
    ]
    : [el("span", { class: "operation-cell-main muted", text: "Never" })];
  lastRun.push(routineFireStrip(routine, workspaceId));
  if (routine.failure_streak?.count) {
    lastRun.push(el("span", { class: "operation-cell-sub routine-failure-streak", text: routineFailureText(routine) }));
  }
  card.append(
    el("div", { class: "operation-row-head" }, [
      operationCell("", [routineButton(payload, routine)], "operation-cell-control"),
      operationCell("Routine", [
        identity,
        el("span", { class: "operation-cell-sub" }, [el("span", { text: "runs " }), jobLink(routine.target)]),
      ], "operation-cell-identity"),
      operationCell("Cadence", cadence),
      routine.enabled
        ? whenCell("Next fire", nextAt, { fallback: nextEvaluationText(routine.next_evaluation, routine.next_due, routine.trigger) })
        : operationCell("Next fire", [el("span", { class: "operation-cell-main muted", text: nextAt ? `would be ${clockHm(nextAt)}` : "Paused" })]),
      operationCell("Last run", lastRun),
    ]),
    operationDetails(detailsKey, [
      el("div", { class: "operation-target mono", text: routine.target }),
      el("div", { class: "operation-grid" }, [
        field("Source workspace", routine.source),
        field("Schedule", routineScheduleText(routine)),
        field("Last evaluation", time(routine.last_evaluated_slot || routine.first_observed_at)),
        field("Next evaluation", nextEvaluationText(routine.next_evaluation, routine.next_due, routine.trigger)),
        field("Last fire", fire ? time(fire.finished_at || fire.started_at) : "Never"),
        field("Linked run / outcome", fire ? `${fire.run_id || "No run"} · ${fire.state}` : "No fire recorded"),
      ]),
      renderAutomation(routine.automation, `${detailsKey}:automation`, { kind: "routine", name: routine.name, workspace: workspaceId }),
      routine.description ? el("p", { class: "operation-description", text: routine.description }) : null,
    ]),
  );
  return card;
}

function renderOperations(payload) {
  noteCronZone(payload);
  lastOperations = payload;
  if ($("operations-session")) $("operations-session").textContent = payload.session_explanation || "Operations actions require the capabilities granted to this dashboard server. Refresh to load session access details.";
  const workspace = selectedWorkspaceName();
  const workspaceId = selectedWorkspace()?.id || null;
  const routines = scopedRoutines(payload);
  updateRoutineFailureBadge(routines);
  const inactive = workspace
    ? (payload.retired || []).filter((routine) => routine.plugin_inactive && routine.source === workspace)
    : [];
  // A replica schedules only worktree GC and store GC on a cron trigger; its owner schedules the rest.
  const ownerOnly = workspace
    ? (payload.owner_only || []).filter((routine) => routine.source === workspace)
    : [];
  const body = $("routines-body");
  body.textContent = "";
  if (!workspace) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: `All-workspace mode is read-only. Select one workspace; this machine is already resolved as ${payload.machine_name}.` }));
  }
  if (routines.length === 0 && inactive.length === 0 && ownerOnly.length === 0) {
    body.appendChild(el("div", { class: "empty-state", text: workspace ? "No routines are defined by this workspace." : "Select a workspace to list its routines." }));
  } else if (routines.length) {
    const warning = routineFailureSummary(routines);
    if (warning) body.appendChild(warning);
    body.appendChild(routineTimeline(routines));
    const active = routines.filter((routine) => routine.enabled);
    const paused = routines.filter((routine) => !routine.enabled);
    const columns = ["", "Routine", "Cadence", "Next fire", "Last run"];
    if (active.length) {
      const group = operationGroup("active", "Active", active.length, "enabled and delivered by the host clock");
      group.appendChild(operationColumns(columns));
      for (const routine of active) group.appendChild(routineRow(payload, routine, workspaceId));
      body.appendChild(group);
    }
    if (paused.length) {
      const group = operationGroup("paused", "Paused", paused.length, "enabled: false in the definition file · slots are skipped, not queued");
      group.appendChild(operationColumns(columns));
      for (const routine of paused) group.appendChild(routineRow(payload, routine, workspaceId));
      body.appendChild(group);
    }
  }
  if (inactive.length) {
    body.appendChild(inactivePluginGroup(inactive.map((routine) => ({ name: routine.name, detail: routine.target, reason: routine.reason })), "fires"));
  }
  if (ownerOnly.length) {
    body.appendChild(inactivePluginGroup(
      ownerOnly.map((routine) => ({ name: routine.name, detail: routine.target, reason: routine.reason, state: "owner-only" })),
      "fires", "Owner only", "this replica schedules only worktree GC and store GC on a cron trigger · enable these on the owner machine",
    ));
  }
  if (workspace) {
    const toggle = inactivePluginToggle(payload.inactive_plugin_counts?.[workspace] || 0);
    if (toggle) body.appendChild(toggle);
  }
  const activeCount = routines.filter((routine) => routine.enabled).length;
  $("routines-count").textContent = workspace ? `${activeCount} active of ${routines.length} · ${workspace}` : "read-only";
  setRailSubtabCount("rail-count-ops-routines", workspace && routines.length ? `${activeCount}/${routines.length}` : "");
  renderClock(payload);
  syncAutoTaskSchedulerNote();
  if (lastJobs) renderJobs(lastJobs);
}

function clockButton(payload, action, label) {
  const selection = selectionSnapshot();
  const key = `clock:${payload.machine_name}`;
  const unavailable = clockUnavailable(payload.clock);
  const reason = unavailable
    ? clockUnavailableReason(payload.clock)
    : controlReason(payload, "clock_service");
  const button = el("button", { class: "operation-button", text: pendingOperations.has(key) ? "Pending…" : label, title: reason });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", () => {
    if (reason || unavailable || !selection.current() || pendingOperations.has(key)) return;
    const verb = action === "enable" ? "Start" : "Stop";
    if (!window.confirm(`${verb} the ${payload.clock.provider} clock on ${payload.machine_name}? This does not change any routine definition.`)) return;
    return runOperation({
      selection, key, feedbackId: "clock-operation-feedback",
      pending: `${verb} host clock…`, failure: "Clock service change failed",
      render: () => { if (lastOperations) renderClock(lastOperations); },
      request: () => postJson("/api/routines/clock", {
        action, machine_name: payload.machine_name, expected_enabled: payload.clock.enabled,
        expected_cadence_seconds: payload.clock.configured_cadence_seconds,
      }),
      refresh: fetchAndRenderOperations,
      success: (result) => `${result.message} on ${payload.machine_name}.`,
    });
  });
  return explainUnavailable(button, reason);
}

// The clock is one host-wide fact that governs every routine, so it reads as
// a status bar above the list rather than a card beside it.
function renderClock(payload) {
  const clock = payload.clock;
  const body = $("clock-body");
  body.textContent = "";
  const unavailable = clockUnavailable(clock);
  const reason = unavailable
    ? clockUnavailableReason(clock)
    : controlReason(payload, "clock_cadence");
  const selection = selectionSnapshot();
  const key = `clock:${payload.machine_name}`;
  const active = clock.enabled || clock.running === true;
  const serviceLabel = `service ${clockServiceText(clock)}`;
  const cadenceLabel = unavailable ? "Unknown" : cadenceText(clock.configured_cadence_seconds);
  const effectiveCadenceLabel = unavailable ? "Unknown" : cadenceText(clock.effective_cadence_seconds);
  const actions = el("div", { class: "operation-clock-actions" });
  actions.appendChild(clockButton(
    payload,
    unavailable ? "enable" : (active ? "disable" : "enable"),
    unavailable ? "Clock unavailable" : (active ? "Pause clock" : "Enable clock"),
  ));
  // A cadence the operator picked but has not applied yet outlives this render;
  // it clears once the host reports that value as configured, whoever applied it.
  if (pendingCadenceSeconds === clock.configured_cadence_seconds) pendingCadenceSeconds = null;
  const selectedCadence = pendingCadenceSeconds ?? clock.configured_cadence_seconds;
  const cadence = el("select", { class: "operation-cadence", title: "Clock cadence" });
  for (const seconds of [60, 300, 900, 1800, 3600]) {
    const option = el("option", { text: seconds === 60 ? "Every minute" : `Every ${seconds / 60} minutes` });
    option.value = String(seconds);
    option.selected = seconds === selectedCadence;
    cadence.appendChild(option);
  }
  cadence.disabled = Boolean(reason) || pendingOperations.has(key);
  const apply = el("button", { class: "operation-button secondary", text: pendingOperations.has(key) ? "Pending…" : "Apply cadence", title: "Reload cadence without changing whether the clock is enabled" });
  apply.type = "button";
  const syncApplyState = (chosen) => {
    apply.disabled = Boolean(reason) || pendingOperations.has(key) || chosen === clock.configured_cadence_seconds;
  };
  syncApplyState(selectedCadence);
  cadence.addEventListener("change", () => {
    pendingCadenceSeconds = Number(cadence.value);
    syncApplyState(pendingCadenceSeconds);
  });
  apply.addEventListener("click", () => {
    if (reason || unavailable || !selection.current()) return;
    return runOperation({
      selection, key, feedbackId: "clock-operation-feedback",
      pending: `Changing cadence to ${cadence.value}s…`, failure: "Cadence change failed",
      render: () => { if (lastOperations) renderClock(lastOperations); },
      request: () => postJson("/api/routines/clock", {
        action: "set_cadence", machine_name: payload.machine_name, expected_enabled: clock.enabled,
        expected_cadence_seconds: clock.configured_cadence_seconds, cadence_seconds: Number(cadence.value),
      }),
      refresh: fetchAndRenderOperations,
      success: (result) => `${result.message}; service is ${clockServiceText(result.clock)}.`,
    });
  });
  actions.append(cadence, explainUnavailable(apply, reason));

  body.append(
    el("div", { class: "operation-clock-bar" }, [
      el("div", { class: "operation-clock-summary" }, [
        el("span", { class: `operation-state ${clock.health}`, text: clock.health }),
        el("span", { class: "mono", text: unavailable ? (clock.provider || "unknown") : clock.provider }),
        el("span", { text: serviceLabel }),
      ]),
      el("div", { class: "operation-row-facts operation-clock-facts" }, [
        operationFact("Cadence", cadenceLabel),
        operationFact("Last tick", clock.last_tick_at ? (relativeTime(clock.last_tick_at) || time(clock.last_tick_at)) : unavailable ? "Unknown" : "Not exposed", clock.last_tick_at ? time(clock.last_tick_at) : ""),
        operationFact("Next tick", clockTickText(clock.next_tick_at, clock)),
      ]),
      actions,
    ]),
    operationDetails("clock", [
      el("div", { class: "operation-grid" }, [
        field("Configured cadence", cadenceLabel),
        field("Effective cadence", effectiveCadenceLabel),
        field("Loaded", unavailable ? "Unknown" : clock.loaded ? "Yes" : "No"),
        field("Running / waiting", unavailable ? "Unknown" : clock.running == null ? "Provider does not expose" : clock.running ? "Yes" : "No"),
        field("Last tick", clock.last_tick_at ? time(clock.last_tick_at) : unavailable ? "Unknown" : "Provider does not expose"),
        field("Next expected tick", clockTickText(clock.next_tick_at, clock)),
      ]),
    ]),
  );
  if (clock.health_issue) body.appendChild(el("p", { class: "operation-control-note error", text: clock.health_issue }));
  $("clock-host").textContent = payload.machine_name || "unknown machine";
}

// ---------------------------------------------------------------------------
// Auto-tasks

function lastEvaluationText(definition) {
  const evaluation = definition.last_evaluation;
  if (!evaluation) return "Never evaluated";
  if (evaluation.kind === "fired") {
    const task = evaluation.last_task_id ? ` · ${evaluation.last_task_id}` : "";
    return `${time(evaluation.last_fired_at || evaluation.last_slot)}${task}`;
  }
  return `Baselined ${time(evaluation.baseline_at)}; no fire yet`;
}

function lastOutcomeText(definition) {
  if (definition.last_minted_task_id) return lastMintedText(definition);
  return lastEvaluationText(definition);
}

// Grouping, counts, row state and the switch follow the effective switch.
// The raw `enabled` flag stays the compare-and-set token the toggle posts:
// a legacy after-landing consumer can be on while that flag is still false.
function autoTaskOn(definition) {
  return definition.effective_enabled ?? Boolean(definition.enabled);
}

function autoTaskToggleButton(payload, definition) {
  const selection = selectionSnapshot();
  const on = autoTaskOn(definition);
  const nextEnabled = !on;
  const key = `auto-task-toggle:${selection.workspace}:${definition.name}`;
  const reason = actionReason(payload, "auto_task_toggle");
  const verb = nextEnabled ? "Enable" : "Disable";
  const pending = pendingOperations.has(key);
  const button = operationSwitch({
    name: `Auto-task: ${definition.name}`,
    on,
    text: pending ? "Pending…" : verb,
    pending,
    reason: reason || "",
  });
  if (!reason) button.title = `${verb} ${definition.name}`;
  button.addEventListener("click", () => {
    if (reason || !selection.current() || pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const summary = definition.template_summary || definition.template?.title || definition.name;
    if (!window.confirm(`${verb} auto-task "${definition.name}" in workspace "${workspace?.name || workspace?.id}"?\n\nTarget: ${summary}\nThis writes the definition's enabled field.`)) return;
    return runOperation({
      selection, key, feedbackId: "auto-task-operation-feedback",
      pending: `${verb} ${definition.name}…`, failure: "Auto-task change failed",
      render: () => { if (lastAutoTasks) renderAutoTasks(lastAutoTasks); },
      request: () => postJson("/api/auto-tasks/toggle", {
        name: definition.name, expected_enabled: definition.enabled, enabled: nextEnabled,
      }),
      refresh: fetchAndRenderAutoTasks,
      success: (result) => `${result.message}: ${definition.name}.`,
    });
  });
  return explainUnavailable(button, reason);
}

function autoTaskMintButton(payload, definition) {
  const selection = selectionSnapshot();
  const key = `auto-task-mint:${selection.workspace}:${definition.name}`;
  const reason = actionReason(payload, "auto_task_mint");
  const button = el("button", {
    class: "operation-button secondary",
    text: pendingOperations.has(key) ? "Pending…" : "Mint now",
    title: reason || "Mint one task now, ignoring schedule, enabled, and dedupe",
  });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", async () => {
    if (reason || !selection.current() || pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const template = definition.template || {};
    const duplicateLine = definition.may_create_open_duplicate
      ? "An open instance already exists; this will create another open task."
      : "No open instance is currently tagged for this definition.";
    const confirmText = [
      `Mint auto-task "${definition.name}" now in workspace "${workspace?.name || workspace?.id}"?`,
      "",
      `Resulting task: ${definition.template_summary || template.title || definition.name}`,
      `Crew: ${template.crew || "none"} · Status: ${template.status || "backlog"} · Priority: ${template.priority || "medium"}`,
      "",
      `WARNING: ${payload.unconditional_mint_warning || UNCONDITIONAL_MINT_WARNING}`,
      duplicateLine,
    ].join("\n");
    if (!window.confirm(confirmText)) return;
    return runOperation({
      selection, key, feedbackId: "auto-task-operation-feedback",
      pending: `Minting ${definition.name} now…`, failure: "Manual mint failed",
      render: () => { if (lastAutoTasks) renderAutoTasks(lastAutoTasks); },
      request: () => postJson("/api/auto-tasks/mint", {
        name: definition.name, acknowledge_unconditional: true,
      }),
      refresh: fetchAndRenderAutoTasks,
      success: (result) => `${result.message}. Task created; no delivery was started.`,
    });
  });
  return explainUnavailable(button, reason);
}

function autoTaskTrigger(definition) {
  const schedule = definition.schedule || {};
  if (schedule.deliveries != null || schedule.deliveries_landed || /deliver/i.test(definition.schedule_summary || "")) return "delivery";
  return "schedule";
}

function autoTaskChip(text, tone = "") {
  return el("span", { class: `operation-chip mono ${tone}`.trim(), text });
}

function taskLink(taskId, workspaceId) {
  const link = el("a", { class: "mono operation-run-link operation-task-link operation-link", text: taskId, title: `Open ${taskId}` });
  bindTaskLink(link, taskId, { workspace: workspaceId || "" });
  return link;
}

function autoTaskRow(payload, definition, workspaceId) {
  const on = autoTaskOn(definition);
  const state = on ? "enabled" : "disabled";
  const detailsKey = `auto-task:${definition.name}`;
  const template = definition.template || {};
  const duplicate = definition.open_duplicate ? "Yes — mint will create another" : "No";
  const schedulerId = definition.last_evaluation?.last_task_id;
  const mintSource = definition.last_minted_task_id && schedulerId && definition.last_minted_task_id !== schedulerId
    ? "manual mint"
    : schedulerId ? "scheduler" : "";
  const lastMinted = definition.last_minted_task_id
    ? [
      el("span", { class: "operation-cell-main operation-outcome" }, [
        taskLink(definition.last_minted_task_id, workspaceId),
        definition.last_minted_task_status ? statusPill(definition.last_minted_task_status) : null,
      ].filter(Boolean)),
      el("span", { class: `operation-cell-sub${definition.open_duplicate ? " warn" : ""}`, text: definition.open_duplicate
        ? "still open · scheduler will skip"
        : [mintSource, relativeTime(definition.last_evaluation?.last_fired_at)].filter(Boolean).join(" · "),
      title: definition.last_evaluation?.last_fired_at ? time(definition.last_evaluation.last_fired_at) : "" }),
    ]
    : [el("span", { class: "operation-cell-main muted", text: lastEvaluationText(definition) })];
  const next = definition.next_evaluation;
  const nextCell = next?.state === "scheduled" && next.at
    ? whenCell("Next mint", next.at)
    : operationCell("Next mint", [el("span", { class: `operation-cell-main${on ? "" : " muted"}`, text: nextEvaluationText(next) })]);
  const card = el("article", { class: `operation-card operation-row auto-task-card ${state}` });
  card.append(
    el("div", { class: "operation-row-head" }, [
      operationCell("", [autoTaskToggleButton(payload, definition)], "operation-cell-control"),
      operationCell("Definition", [
        el("div", { class: "operation-identity" }, [el("strong", { text: definition.name })]),
        el("span", { class: "operation-cell-sub operation-chips" }, [
          template.crew ? autoTaskChip(template.crew, "crew") : null,
          template.priority ? autoTaskChip(template.priority) : null,
        ].filter(Boolean)),
      ], "operation-cell-identity"),
      operationCell("Trigger", [
        el("span", { class: "operation-cell-main", text: definition.schedule?.cron ? cronText(definition.schedule.cron) : (definition.schedule_summary || "—") }),
        el("span", { class: "operation-cell-sub", text: [definition.schedule?.cron, definition.dedupe === "always" ? "always fire" : "skip if open"].filter(Boolean).join(" · ") }),
      ]),
      nextCell,
      operationCell("Last minted", lastMinted),
      operationCell("", [autoTaskMintButton(payload, definition)], "operation-cell-action"),
    ]),
    operationDetails(detailsKey, [
      el("div", { class: "operation-target mono", text: definition.template_summary || template.title || "" }),
      el("div", { class: "operation-grid" }, [
        field("Schedule", definition.schedule_summary || "—"),
        field("Dedupe", definition.dedupe === "always" ? "always fire" : "skip if open"),
        field("Last scheduler evaluation", lastEvaluationText(definition)),
        field("Last minted task", lastMintedText(definition)),
        field("Next evaluation", nextEvaluationText(definition.next_evaluation)),
        field("Open duplicate", duplicate),
        field("Template", [template.crew && `crew ${template.crew}`, template.priority && `priority ${template.priority}`, template.complexity && `complexity ${template.complexity}`, `→ ${template.status || "backlog"}`].filter(Boolean).join(" · ")),
      ]),
      renderAutomation(definition.automation, `${detailsKey}:automation`, { kind: "auto-task", name: definition.name, workspace: getWorkspace() }),
      definition.description ? el("p", { class: "operation-description", text: definition.description }) : null,
      el("p", {
        class: "operation-control-note operation-mint-warning",
        text: payload.unconditional_mint_warning || UNCONDITIONAL_MINT_WARNING,
      }),
    ]),
  );
  return card;
}

// Where the workspace defines an auto_task_scheduler routine, a paused one
// silently stops every scheduled definition, so the pane says so instead of
// leaving "next mint" looking live.
function syncAutoTaskSchedulerNote() {
  const note = $("auto-tasks-scheduler-note");
  if (!note) return;
  const workspace = selectedWorkspaceName();
  const scheduler = workspace && lastOperations
    ? (lastOperations.routines || []).find((routine) => routine.source === workspace && jobIdFromTarget(routine.target) === "auto_task_scheduler_pipeline")
    : null;
  note.textContent = "";
  note.className = "operation-control-note auto-tasks-scheduler-note";
  note.title = "Definitions are stored in .orbit/auto_tasks/ and evaluated on the host sweep clock. Toggle enables or disables a definition; Mint now bypasses its schedule, enabled flag and duplicate checks.";
  if (!scheduler) {
    note.appendChild(el("span", { text: "Enabled definitions create tasks automatically when their trigger is due." }));
    return;
  }
  const link = el("a", { class: "operation-run-link operation-routine-link operation-link", text: scheduler.name });
  link.href = "#operations/routines";
  if (!scheduler.enabled) {
    note.className += " warn";
    note.appendChild(el("span", { text: "The " }));
    note.append(link, el("span", { text: " routine is paused, so scheduled tasks will wait until it resumes." }));
    return;
  }
  note.appendChild(el("span", { text: "Scheduled tasks are created by " }));
  note.append(link, el("span", { text: ` (${cronText(scheduler.cron)}).` }));
}

function renderAutoTasks(payload) {
  noteCronZone(payload);
  lastAutoTasks = payload;
  const body = $("auto-tasks-body");
  if (!body) return;
  body.textContent = "";
  const workspaceReason = workspaceReadOnlyReason();
  const reason = workspaceReason || payload.read_only_reason || "";
  const workspace = selectedWorkspace();
  const listed = workspace && !workspaceReason ? (payload.definitions || []) : [];
  const definitions = listed.filter((definition) => !definition.plugin_inactive);
  const inactive = listed.filter((definition) => definition.plugin_inactive);
  if (reason) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: reason }));
  }
  if (definitions.length === 0 && inactive.length === 0) {
    body.appendChild(el("div", {
      class: "empty-state",
      text: workspace && !workspaceReason ? "No auto-task definitions are defined by this workspace." : "Select a workspace to list its auto-task definitions.",
    }));
  } else if (definitions.length) {
    const enabled = definitions.filter(autoTaskOn);
    const duplicates = definitions.filter((definition) => definition.open_duplicate).length;
    const nextMint = enabled
      .map((definition) => definition.next_evaluation?.state === "scheduled" ? new Date(definition.next_evaluation.at || NaN).getTime() : NaN)
      .filter(Number.isFinite)
      .sort((a, b) => a - b)[0];
    body.appendChild(el("div", { class: "operation-summary auto-tasks-summary" }, [
      operationStat("", "Definitions", definitions.length),
      operationStat("active", "Enabled", enabled.length),
      operationStat("accent", "Next mint", nextMint ? relativeTime(nextMint) : "—", nextMint ? clockHm(nextMint) : "no scheduled definition"),
      operationStat(duplicates ? "warn" : "", "Open duplicates", duplicates, duplicates ? "scheduler skips these slots" : null),
    ]));
    const schedulerNote = el("p", { class: "operation-control-note auto-tasks-scheduler-note" });
    schedulerNote.id = "auto-tasks-scheduler-note";
    body.appendChild(schedulerNote);
    const groups = [
      ["active", "On a schedule", "minted by the scheduler when due and no open instance exists", enabled.filter((definition) => autoTaskTrigger(definition) === "schedule")],
      ["accent", "On delivery", "minted after landed deliveries, not on a clock", enabled.filter((definition) => autoTaskTrigger(definition) === "delivery")],
      ["paused", "Disabled", "kept in the repo, never evaluated", definitions.filter((definition) => !autoTaskOn(definition))],
    ];
    const columns = ["", "Definition", "Trigger", "Next mint", "Last minted", ""];
    for (const [tone, title, hint, members] of groups) {
      if (!members.length) continue;
      const group = operationGroup(tone, title, members.length, hint);
      group.appendChild(operationColumns(columns));
      for (const definition of members) group.appendChild(autoTaskRow(payload, definition, workspace.id));
      body.appendChild(group);
    }
  }
  if (inactive.length) {
    body.appendChild(inactivePluginGroup(inactive.map((definition) => ({ name: definition.name, detail: definition.schedule_summary, reason: definition.skipped_reason })), "minted"));
  }
  if (workspace && !workspaceReason) {
    const toggle = inactivePluginToggle(payload.inactive_plugin_count || 0);
    if (toggle) body.appendChild(toggle);
  }
  syncAutoTaskSchedulerNote();
  const count = $("auto-tasks-count");
  const enabledCount = definitions.filter(autoTaskOn).length;
  if (count) count.textContent = workspace && !workspaceReason ? `${enabledCount} enabled of ${definitions.length} · ${workspace.name}` : "read-only";
  setRailSubtabCount("rail-count-ops-auto-tasks", workspace && !workspaceReason && definitions.length ? `${enabledCount}/${definitions.length}` : "");
}

// ---------------------------------------------------------------------------
// Jobs. The catalogue is projected from routine targets and recent runs.
// Manual Run submits catalog jobs; delivery pipelines use Ship or Drain.

const JOB_RUN_LIMIT = 100;
let lastJobs = null;

function jobFamily(jobId) {
  if (/sweep|gc|pilot|triage|scheduler/.test(jobId)) return "sweep";
  if (/^(task|workspace|epic)_/.test(jobId)) return "delivery";
  return "other";
}

function jobCatalog(routinesPayload, runs, workspaceName) {
  const catalog = new Map();
  const entry = (jobId) => {
    if (!catalog.has(jobId)) catalog.set(jobId, { id: jobId, routines: [], runs: [], running: 0, lastRun: null });
    return catalog.get(jobId);
  };
  for (const routine of routinesPayload?.routines || []) {
    if (routine.source !== workspaceName) continue;
    const jobId = jobIdFromTarget(routine.target);
    if (jobId) entry(jobId).routines.push(routine);
  }
  const sorted = [...runs].sort((a, b) => new Date(b.created_at || 0) - new Date(a.created_at || 0));
  for (const run of sorted) {
    if (!run.job_id) continue;
    const job = entry(run.job_id);
    job.runs.push(run);
    if (run.state === "running" || run.state === "pending") job.running += 1;
    if (!job.lastRun) job.lastRun = run;
  }
  return catalog;
}

function jobRunningCard(run, workspaceId) {
  const startedAt = run.started_at || run.scheduled_at || run.created_at;
  return el("div", { class: "operation-running-card" }, [
    el("div", { class: "operation-running-head" }, [
      outcomeDot(run.state),
      el("strong", { class: "mono", text: run.job_id }),
      el("span", { class: "operation-state running", text: run.state }),
    ]),
    el("div", { class: "operation-running-meta" }, [
      el("span", { text: `started ${relativeTime(startedAt) || "—"}`, title: time(startedAt) }),
      run.run_role ? el("span", { class: "mono", text: run.run_role }) : null,
      run.resolved_crew ? el("span", { class: "mono", text: run.resolved_crew }) : null,
      runLink(run.run_id, workspaceId),
    ].filter(Boolean)),
  ]);
}

// A run names the tasks it works on: `tasks` carries titles when the server
// has them, `task_ids` is the fallback.
function runTaskRefs(run) {
  const refs = Array.isArray(run.tasks) ? run.tasks : (Array.isArray(run.task_ids) ? run.task_ids : []).map((id) => ({ id }));
  return refs.filter((task) => task && task.id);
}

// One group per task, so a task's auto, gate and pr runs share a card. A run
// naming several tasks appears under each. Runs with no task stay separate.
function runningGroups(running) {
  const byTask = new Map();
  const untasked = [];
  for (const run of running) {
    const tasks = runTaskRefs(run);
    if (!tasks.length) untasked.push(run);
    for (const task of tasks) {
      if (!byTask.has(task.id)) byTask.set(task.id, { task, runs: [] });
      const group = byTask.get(task.id);
      if (!group.task.title && task.title) group.task = task;
      group.runs.push(run);
    }
  }
  return { tasks: [...byTask.values()], untasked };
}

function runningTaskCard(group, workspaceId) {
  const { task, runs } = group;
  const startedAt = (run) => run.started_at || run.scheduled_at || run.created_at;
  const oldest = runs.reduce((earliest, run) => (new Date(startedAt(run) || 0) < new Date(startedAt(earliest) || 0) ? run : earliest));
  const card = el("div", { class: "operation-running-card operation-running-task" }, [
    el("div", { class: "operation-running-head" }, [
      outcomeDot(oldest.state),
      taskLink(task.id, workspaceId),
      task.title ? el("span", { class: "operation-running-task-title", text: task.title, title: task.title }) : null,
    ].filter(Boolean)),
    el("div", { class: "operation-running-meta" }, [
      el("span", { text: `started ${relativeTime(startedAt(oldest)) || "—"}`, title: time(startedAt(oldest)) }),
      el("span", { text: `${runs.length} in flight` }),
    ]),
    el("ul", { class: "operation-running-steps" }, runs.map((run) => el("li", { class: "operation-running-step" }, [
      el("span", { class: "mono", text: run.job_id }),
      run.run_role ? el("span", { class: "mono", text: run.run_role }) : null,
      run.resolved_crew ? el("span", { class: "mono", text: run.resolved_crew }) : null,
      runLink(run.run_id, workspaceId),
    ].filter(Boolean)))),
  ]);
  card.dataset.task = task.id;
  return card;
}

function jobCommand(jobId, workspace) {
  return `orbit run job ${jobId} --workspace ${workspace?.name || workspace?.id || "<workspace>"}`;
}

function jobRow(job, workspace) {
  const detailsKey = `job:${job.id}`;
  const last = job.lastRun;
  const command = jobCommand(job.id, workspace);
  const key = `job:run:${workspace.id}:${job.id}`;
  const runReason = jobFamily(job.id) === "delivery"
    ? "Delivery jobs require task input or a delivery window. Use Ship or Drain."
    : controlReason(lastOperations, "job_run");
  const pending = pendingOperations.has(key);
  const run = el("button", { class: "operation-button primary job-run", text: pending ? "Submitting…" : "Run ▸", title: runReason || `Run ${job.id} in ${workspace.name}` });
  run.type = "button";
  run.disabled = Boolean(runReason) || pending;
  run.addEventListener("click", () => {
    if (runReason) return;
    const selection = selectionSnapshot();
    runOperation({
      selection, key, feedbackId: "job-operation-feedback",
      pending: `Submitting ${job.id}…`,
      failure: `Could not run ${job.id}`,
      render: () => renderJobs(lastJobs),
      request: () => postJson(`/api/jobs/${encodeURIComponent(job.id)}/run`, {}),
      refresh: fetchAndRenderJobs,
      success: (result) => `Run ${result.run_id} ${result.state} for ${result.job_id}.`,
    });
  });
  const scheduledBy = job.routines.length
    ? job.routines.map((routine) => {
      const chip = el("a", { class: `operation-chip ${routine.enabled ? "" : "paused"}`.trim(), text: `${routine.name} · ${routine.trigger?.deliveries_landed ? "on delivery" : routine.trigger?.state ? "on task change" : cronText(routine.cron)}${routine.enabled ? "" : " · paused"}` });
      chip.href = "#operations/routines";
      return chip;
    })
    : [el("span", { class: "operation-cell-main muted", text: "manual only" })];
  const lastCell = last
    ? [
      el("span", { class: "operation-cell-main operation-outcome" }, [
        outcomeDot(last.state),
        el("span", { text: `${last.state} ${relativeTime(last.finished_at || last.started_at || last.created_at) || ""}`.trim(), title: time(last.finished_at || last.started_at || last.created_at) }),
      ]),
      el("span", { class: "operation-cell-sub" }, [
        durationText(last.duration_ms) ? el("span", { class: "mono", text: durationText(last.duration_ms) }) : null,
        runLink(last.run_id, workspace?.id),
      ].filter(Boolean)),
    ]
    : [el("span", { class: "operation-cell-main muted", text: "no recent run" })];
  const copy = el("button", { class: "operation-button secondary", text: "Copy command", title: "Copy the CLI command" });
  copy.type = "button";
  copy.addEventListener("click", async () => {
    copy.textContent = (await copyText(command)) ? "Copied" : "Copy failed";
    setTimeout(() => { copy.textContent = "Copy command"; }, 1500);
  });
  const recent = job.runs.slice(0, 5);
  const card = el("article", { class: `operation-card operation-row job-card ${jobFamily(job.id)}` });
  card.dataset.job = job.id;
  card.append(
    el("div", { class: "operation-row-head" }, [
      operationCell("Job", [
        el("div", { class: "operation-identity" }, [el("strong", { class: "mono", text: job.id })]),
        el("span", { class: "operation-cell-sub", text: `${job.runs.length} recent run${job.runs.length === 1 ? "" : "s"} in this window` }),
      ], "operation-cell-identity"),
      operationCell("Scheduled by", [el("span", { class: "operation-chips" }, scheduledBy)]),
      operationCell("Last run", lastCell),
      operationCell("Active", [el("span", { class: `operation-cell-main mono${job.running ? " live" : " muted"}`, text: job.running ? `${job.running} running` : "idle" })]),
      operationCell("", [explainUnavailable(run, runReason)], "operation-cell-action"),
    ]),
    operationDetails(detailsKey, [
      el("div", { class: "operation-command" }, [
        el("code", { class: "mono", text: command }),
        copy,
      ]),
      el("p", { class: "operation-control-note", text: "Run submits without extra input. Delivery jobs need a task id and are started through Ship or Drain." }),
      recent.length
        ? el("ul", { class: "operation-recent-runs" }, recent.map((run) => el("li", {}, [
          outcomeDot(run.state),
          runLink(run.run_id, workspace?.id),
          el("span", { class: "muted", text: `${run.state} · ${relativeTime(run.finished_at || run.started_at || run.created_at) || ""}${durationText(run.duration_ms) ? ` · ${durationText(run.duration_ms)}` : ""}`, title: time(run.finished_at || run.started_at || run.created_at) }),
        ])))
        : null,
    ]),
  );
  return card;
}

function renderJobs(payload) {
  if (payload.view !== "live") lastJobs = payload;
  if (payload.routines) {
    lastOperations = payload.routines;
    noteCronZone(lastOperations);
    updateRoutineFailureBadge(scopedRoutines(lastOperations));
  }
  const isLive = payload.view === "live";
  const body = $(isLive ? "jobs-live-body" : "jobs-body");
  const count = $(isLive ? "jobs-live-count" : "jobs-count");
  if (!body) return;
  body.textContent = "";
  const workspace = selectedWorkspace();
  const workspaceReason = workspaceReadOnlyReason();
  if (workspaceReason || !workspace) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: workspaceReason || "Select a workspace to list its jobs." }));
    body.appendChild(el("div", { class: "empty-state", text: "Select a workspace to list its jobs." }));
    count.textContent = "read-only";
    return;
  }
  const runs = Array.isArray(payload.runs?.items) ? payload.runs.items : Array.isArray(payload.runs) ? payload.runs : [];
  const running = runs.filter((run) => run.state === "running" || run.state === "pending");
  if (isLive) {
    const groups = runningGroups(running);
    const taskCount = groups.tasks.length;
    const strip = el("section", { class: "operation-running" });
    strip.setAttribute("aria-label", "Running now");
    strip.appendChild(el("div", { class: "operation-running-title" }, [
      el("strong", { text: "Running now" }),
      el("span", { class: "mono", text: `${taskCount} task${taskCount === 1 ? "" : "s"}` }),
      el("span", { class: "muted", text: running.length ? `(${running.length} run${running.length === 1 ? "" : "s"} in flight)` : "nothing in flight" }),
    ]));
    if (running.length) {
      strip.appendChild(el("div", { class: "operation-running-grid" }, [
        ...groups.tasks.map((group) => runningTaskCard(group, workspace.id)),
        ...groups.untasked.map((run) => jobRunningCard(run, workspace.id)),
      ]));
    }
    body.appendChild(strip);
    if (payload.runs?.truncated) body.appendChild(el("p", { class: "operation-control-note", text: `${running.length} shown of ${payload.runs.total} runs in flight.` }));
    count.textContent = `${running.length} run${running.length === 1 ? "" : "s"} in flight · ${workspace.name}`;
    return;
  }
  const catalog = jobCatalog(lastOperations, runs, workspace.name);
  if (catalog.size === 0) {
    body.appendChild(el("div", { class: "empty-state", text: "No job has been referenced by a routine or run in this workspace recently." }));
  } else {
    const jobs = [...catalog.values()].sort((a, b) => a.id.localeCompare(b.id));
    const groups = [
      ["accent", "Sweeps", "housekeeping and intake · safe to run by hand", jobs.filter((job) => jobFamily(job.id) === "sweep")],
      ["delivery", "Delivery", "task and workspace pipelines · started by a ship or a drain, by hand only with a task_id", jobs.filter((job) => jobFamily(job.id) === "delivery")],
      ["", "Other", "", jobs.filter((job) => jobFamily(job.id) === "other")],
    ];
    const columns = ["Job", "Scheduled by", "Last run", "Active", ""];
    for (const [tone, title, hint, members] of groups) {
      if (!members.length) continue;
      const group = operationGroup(tone, title, members.length, hint);
      group.appendChild(operationColumns(columns));
      for (const job of members) group.appendChild(jobRow(job, workspace));
      body.appendChild(group);
    }
    body.appendChild(el("p", { class: "operation-control-note", text: `Catalogue projected from routine targets and the last ${JOB_RUN_LIMIT} runs.` }));
  }
  $("jobs-count").textContent = `${catalog.size} job${catalog.size === 1 ? "" : "s"} · ${running.length} running · ${workspace.name}`;
}

export function fetchAndRenderJobs() {
  const live = context.getJobsSubtab?.() === "live";
  const bodyId = live ? "jobs-live-body" : "jobs-body";
  const countId = live ? "jobs-live-count" : "jobs-count";
  if (!selectedWorkspace()) {
    return requestPanel(bodyId, "unselected", () => Promise.resolve({ runs: [], view: live ? "live" : "catalogue" }), renderJobs, countId);
  }
  return requestPanel(bodyId, "jobs",
    () => Promise.all([
      fetchJson(withInactivePlugins("/api/routines")),
      fetchJson(`/api/job-runs?limit=${JOB_RUN_LIMIT}${live ? "&state=active" : ""}`),
    ]).then(([routines, runs]) => ({ routines, runs, view: live ? "live" : "catalogue" })),
    renderJobs, countId);
}

function operationCountId(bodyId) {
  return bodyId === "clock-body" ? "clock-host" : bodyId.replace("-body", "-count");
}

function loadOperationPanel(bodyId, path, render) {
  return requestPanel(bodyId, path, () => fetchJson(path), render, operationCountId(bodyId));
}

function fetchAndRenderAutoTasks() {
  return loadOperationPanel("auto-tasks-body", withInactivePlugins("/api/auto-tasks"), renderAutoTasks);
}

// ORB-11250: bounded backlog auto-drain window ("orbit run auto --for
// <duration> [--complete]" from the dashboard). One workspace-scoped action,
// not a per-row one, so it follows the mint/clock in-flight idiom (a single
// fixed `pendingOperations` key, guard released in `finally`) rather than
// tasks.js's per-task Ship guard.
//
// The Drain page leads with a host-throttle banner and summary tiles, then two
// columns: the live window and forecast, why tasks wait (with the last
// admission pass) and fleet drains on the left; Start a window and the locks
// held on the right.

// [ORB-12728] The live coordinator readiness reports, if any. The server
// nests it under `capacity` (where the slot picture comes from); an older
// payload shape with the fields at the top level is read the same way so a
// mixed-version dashboard never hides a running window.
function autoDrainLiveWindow(payload) {
  const capacity = payload.capacity || {};
  const source = payload.capacity && "drain_run_id" in payload.capacity ? payload.capacity : payload;
  const runId = source?.drain_run_id ? String(source.drain_run_id) : "";
  return {
    runId,
    admissionsStopped: source?.admissions_stopped === true,
    stop: source?.admissions_stop && typeof source.admissions_stop === "object" ? source.admissions_stop : null,
    // A replica's pull drain is a second coordinator the same Stop acts on. It
    // is reported beside `drain_run_id`, never in place of it.
    pullRunId: capacity.pull_drain_run_id ? String(capacity.pull_drain_run_id) : "",
    pullAdmissionsStopped: capacity.pull_drain_admissions_stopped === true,
    pullStop: capacity.pull_drain_admissions_stop && typeof capacity.pull_drain_admissions_stop === "object" ? capacity.pull_drain_admissions_stop : null,
  };
}

// The status card's own refusals; each Start control words its own in
// drain-start.js.
function autoDrainReasons(payload) {
  const workspaceReason = workspaceReadOnlyReason();
  return {
    submit: workspaceReason,
    // Stop is also the settle-only pass (`orbit run auto --stop`): it delivers
    // recorded settlements and needs no live window, so only a read-only view
    // or a missing operator session blocks it.
    stop: workspaceReason || (payload.controls_authorized === false
      ? "Stopping admissions and settling requires an authorized operator session."
      : ""),
  };
}

function autoDrainCounts(payload) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  const eligible = tasks.filter((task) => task.eligible === true).length;
  return { eligible, waiting: tasks.length - eligible };
}

function autoDrainBlocked(payload) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  return tasks.filter((task) => task.eligible !== true && DRAIN_LOCK_REASONS.has(drainReason(task)));
}

function autoDrainShortRunId(runId) {
  const match = /^jrun-\d{8}-(.+)$/.exec(runId);
  return match ? `jrun-…${match[1]}` : runId;
}

// The server-stamped window deadline is shared by CLI and browser starts.
function autoDrainTimeLeft(endsAt) {
  const deadline = Date.parse(endsAt || "");
  if (!Number.isFinite(deadline)) return "";
  const minutes = Math.max(0, Math.ceil((deadline - Date.now()) / 60_000));
  if (minutes === 0) return "window closed";
  return minutes < 60 ? `${minutes}m left` : `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m left`;
}

function updateDrainIndicators(phase, label) {
  const aggregate = isAggregateView();
  const indicatorPhase = aggregate ? "fleet" : phase;
  const indicatorLabel = aggregate ? "Fleet drains" : label;
  const tab = document.querySelector?.('#dock-mode-toggle [data-mode="drain"]');
  const tabState = $("dock-drain-state");
  if (tab) {
    tab.dataset.drainState = indicatorPhase;
    // The tab's own name differs by page (Overview on Drain, Drain on Tasks);
    // the dock relabels from `drainLabel` when it moves.
    tab.dataset.drainLabel = indicatorLabel;
    const name = tab.querySelector?.(".dock-seg-label")?.textContent || "Overview";
    tab.setAttribute("aria-label", `${name}: ${indicatorLabel}`);
    tab.title = indicatorLabel;
  }
  if (tabState) tabState.textContent = aggregate ? "fleet" : phase === "winding_down" ? label : "";
}

// The card and dock tab describe the selected workspace. The top-bar pill
// separately describes the fleet, including work outside that selection.
function autoDrainPhase(payload) {
  const live = autoDrainLiveWindow(payload);
  const capacity = payload.capacity || {};
  const phase = capacity.drain_phase || (live.runId && !live.admissionsStopped ? "draining" : "idle");
  const running = Number(capacity.running_admitted_workers) || 0;
  const label = phase === "draining" ? "Draining"
    : phase === "winding_down" ? "Winding down" : "idle";
  // A replica's pull drain has its own indicator; the owner's phase stays
  // "idle" here, so the header and the announcement below keep their wording.
  const pulling = phase === "idle" && Boolean(live.pullRunId) && !workspaceReadOnlyReason();
  const pullLabel = live.pullAdmissionsStopped ? "Pull drain · admissions stopped" : "Pull drain";
  return { live, capacity, phase, running, label, pulling, pullLabel };
}

function renderAutoDrainHead(payload) {
  const { capacity, phase, running, label, pulling, pullLabel } = autoDrainPhase(payload);
  updateDrainIndicators(pulling ? "pulling" : phase, pulling ? pullLabel : label);
  const card = $("auto-drain-panel");
  if (card) {
    card.dataset.drainState = pulling ? "pulling" : phase;
    const title = card.querySelector('.drain-title');
    if (title) title.textContent = 'Auto-drain';
  }
  const pill = $("auto-drain-phase");
  if (pill) pill.dataset.drainState = pulling ? "pulling" : phase;
  const dot = $("auto-drain-dot");
  if (dot) dot.className = `drain-dot ${pulling ? "pulling" : phase}`;
  const text = $("auto-drain-phase-label");
  if (text) {
    text.textContent = pulling ? pullLabel
      : phase === "idle" ? (workspaceReadOnlyReason() ? "Read-only" : "Idle") : label;
  }
  // The window's deadline sits beside its phase; the pill already says when
  // admissions have stopped.
  const when = $("auto-drain-when");
  if (when) when.textContent = !pulling && phase === "draining" ? autoDrainTimeLeft(capacity.ends_at) : "";
  const stateKey = `${phase}:${phase === "winding_down" ? running : ""}`;
  if (announcedDrainState !== null && announcedDrainState !== stateKey && !holdDrainAnnouncement) {
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
    autoDrainQueuedCancel(payload, runId),
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
// window has admitted nothing, and the live window is not touched.
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
      const result = await postJson(visit.path(`/api/runs/${encodeURIComponent(runId)}/cancel`), { reason: null, force: false });
      if (!visit.isCurrent()) return;
      feedback("auto-drain-operation-feedback", "success", result?.outcome === "already_terminal"
        ? `Queued window ${runId} had already ended.`
        : `Queued window ${runId} cancelled.`);
      await refreshDrainAfterAction();
    } catch (error) {
      if (visit.isCurrent()) feedback("auto-drain-operation-feedback", "error", `Cancelling queued window ${runId} failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastAutoDrain) renderAutoDrain(lastAutoDrain);
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

// Re-read the card once Start or Stop has reported. The action has already
// happened by then, so a failed read must not replace its result with a
// failure message (the panel's own note says the refresh failed), and the
// state change the action caused must not be announced over that result.
async function refreshDrainAfterAction() {
  holdDrainAnnouncement = true;
  try {
    await fetchAndRenderAutoDrain();
  } catch (_) {
    /* reported by the panel's own status note */
  } finally {
    holdDrainAnnouncement = false;
  }
}

const AUTO_DRAIN_STOP_CONFIRM = "Stop new admissions for the active auto-delivery window? Already admitted workers keep running under their captured completion authority. This is not cancellation. Any settlement already recorded is also delivered to its owner.";
// The pull drain's stop reads differently from the auto window's: its leaves
// were assigned by the owner and stay claimed there until they settle.
const PULL_DRAIN_STOP_CONFIRM = "Stop new admissions for this replica's pull drain? Leaves it already admitted keep running and stay claimed by their owner. This is not cancellation, and cancelling one leaf fails its claim on the owner. Any settlement already recorded is also delivered to its owner.";
const AUTO_DRAIN_SETTLE_CONFIRM = "Send completed task results to their owner? This retries results waiting to be delivered, even after the drain has finished. Running tasks keep running.";

// [ORB-12728] Counterpart to `orbit run auto --stop`: stops new admissions on
// the live coordinator without cancelling it or the workers it already
// admitted, then runs the settle-only pass that delivers recorded settlements
// [ORB-13663]. That pass needs no live window, so with none (or with
// admissions already stopped) the button stays usable as "Send pending results" —
// the remedy for leaves stuck `settling` and claims stranded `running`.
// Only a read-only view or a missing operator session disables it, and the
// reason is visible text beside the button rather than a title alone.
function autoDrainStopMode(payload) {
  const live = autoDrainLiveWindow(payload);
  const autoOpen = live.runId && !live.admissionsStopped;
  const pullOpen = live.pullRunId && !live.pullAdmissionsStopped;
  return autoOpen || pullOpen ? "stop" : "settle";
}

// The windows a Stop would close now, named for confirm and status text.
function autoDrainStopTargets(live) {
  const targets = [];
  if (live.runId && !live.admissionsStopped) targets.push(`auto window ${live.runId}`);
  if (live.pullRunId && !live.pullAdmissionsStopped) targets.push(`pull drain ${live.pullRunId}`);
  return targets;
}

function autoDrainStopCopy(payload) {
  const live = autoDrainLiveWindow(payload);
  if (autoDrainStopMode(payload) === "stop") {
    const autoOpen = live.runId && !live.admissionsStopped;
    const pullOpen = live.pullRunId && !live.pullAdmissionsStopped;
    const confirm = autoOpen && pullOpen
      ? `${AUTO_DRAIN_STOP_CONFIRM} The pull drain ${live.pullRunId} on this replica is stopped too; its admitted leaves stay claimed by their owner.`
      : autoOpen ? AUTO_DRAIN_STOP_CONFIRM : PULL_DRAIN_STOP_CONFIRM;
    const description = autoOpen
      ? "Stop starting new tasks in this window. Started tasks keep running; completed results go to their owner."
      : "Stop starting new tasks on this replica. Started tasks keep running; completed results go to their owner.";
    return { label: "Stop", busy: "Stopping…", aria: "Stop admissions", confirm, description };
  }
  const stoppedBy = (stop) => (stop?.actor ? ` (by ${stop.actor})` : "");
  const stopped = [];
  if (live.runId) stopped.push(`${live.runId}${stoppedBy(live.stop)}`);
  if (live.pullRunId) stopped.push(`pull drain ${live.pullRunId}${stoppedBy(live.pullStop)}`);
  const already = stopped.length
    ? `Admissions are already stopped for ${stopped.join(" and ")}. `
    : "No auto-delivery window is live. ";
  return { label: "Send pending results", busy: "Sending…", aria: "Send pending task results", confirm: AUTO_DRAIN_SETTLE_CONFIRM, description: `${already}Send completed task results to their owner.` };
}

// The outcome word for the admissions half of a stop result.
function autoDrainStopHeadline(outcome) {
  if (outcome === "idle") return "No auto-delivery window was live";
  if (outcome === "unchanged") return "Admissions were already stopped";
  if (outcome === "cancelled_queued") return "Queued window cancelled";
  return `Admissions ${outcome ?? "stop requested"}`;
}

function autoDrainStopButton(payload) {
  const key = "auto-drain:stop";
  const reasons = autoDrainReasons(payload);
  const live = autoDrainLiveWindow(payload);
  const copy = autoDrainStopCopy(payload);
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: `operation-button drain-stop${autoDrainStopMode(payload) === "settle" ? " settle" : ""}`,
    text: pending ? copy.busy : copy.label,
    title: reasons.stop || `${copy.description}${autoDrainStopMode(payload) === "settle" ? " (deliver recorded settlements)" : ""}`,
  });
  button.type = "button";
  button.dataset.drainFocus = "stop";
  button.setAttribute("aria-label", pending ? copy.busy : copy.aria);
  button.disabled = Boolean(reasons.stop) || pending;
  button.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    if (pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const targets = autoDrainStopTargets(live);
    const windowLine = targets.length ? `Window: ${targets.join(" and ")} in workspace "${workspace?.name || workspace?.id}"` : `Workspace "${workspace?.name || workspace?.id}"`;
    if (!window.confirm(`${copy.confirm}\n\n${windowLine}`)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", autoDrainStopMode(payload) === "stop" ? `Stopping admissions for ${targets.join(" and ")}…` : "Sending completed task results…");
    renderAutoDrain(payload);
    try {
      const result = await postJson(visit.path("/api/workflows/auto/stop"), {});
      if (!visit.isCurrent()) return;
      const coordinators = Array.isArray(result?.coordinators) ? result.coordinators : [];
      const remaining = coordinators.reduce((sum, change) => sum + (Array.isArray(change?.remaining_children) ? change.remaining_children.length : 0), 0);
      const changes = coordinators.map((change) => `${change?.run_id ?? "(no run id)"}: ${change?.outcome ?? "?"}`).join(", ");
      const settlements = describePullSettlements(result?.pull_settlements);
      const headline = [
        autoDrainStopHeadline(result?.outcome),
        changes ? `(${changes})` : "",
        remaining > 0 ? `· ${remaining} admitted worker${remaining === 1 ? "" : "s"} still running.` : ".",
      ].filter(Boolean).join(" ");
      // A settlement that did not reach its owner is not a plain success: the
      // stop happened, but the operator still has something to wait out or do.
      feedback(
        "auto-drain-operation-feedback",
        settlements.attention ? "error" : "success",
        settlements.text ? `${headline} Settlements: ${settlements.text}.` : headline,
      );
      await refreshDrainAfterAction();
    } catch (error) {
      if (visit.isCurrent()) feedback("auto-drain-operation-feedback", "error", `${copy.label === "Stop" ? "Stopping admissions" : "Settling"} failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastAutoDrain) renderAutoDrain(lastAutoDrain);
    }
  });
  return button;
}

// Visible text (not only a tooltip, which keyboard and touch users never see)
// says why the control is off, or what it does right now.
function autoDrainStopNote(payload) {
  const reasons = autoDrainReasons(payload);
  return el("div", {
    class: reasons.stop ? "operation-control-note drain-stop-note disabled-reason" : "operation-control-note drain-stop-note",
    text: reasons.stop || autoDrainStopCopy(payload).description,
  });
}

// A missing figure is unknown, not zero: `Number(null)` is 0, which would
// read a payload that simply lacks the field as "0 free slots".
function autoDrainNumber(value) {
  return value == null || value === "" ? NaN : Number(value);
}

// Capacity in words: how many leaf runs hold a slot against the limit, a bar
// that shows any overflow past the limit, and one sentence on what a window
// started now would do with that.
function autoDrainCapacity(capacity, counts, blocked) {
  const busy = autoDrainNumber(capacity.active_leaf_runs ?? capacity.occupancy?.active_leaf_runs);
  const limit = autoDrainNumber(capacity.max_active_leaf_runs);
  const free = autoDrainNumber(capacity.free_slots);
  const known = Number.isFinite(busy) && Number.isFinite(limit) && limit > 0;
  const admits = Number.isFinite(free) ? Math.max(0, Math.min(free, counts.eligible)) : counts.eligible;
  let summary;
  if (capacity.host_shutdown && capacity.host_shutdown_holding !== false) {
    summary = "New tasks cannot start while a host shutdown is scheduled.";
  } else if (capacity.host_shutdown) {
    const mode = capacity.host_shutdown.mode || "shutdown";
    const at = capacity.host_shutdown.scheduled_at || "a later time";
    summary = `A host ${mode} is scheduled for ${at}. Admissions continue until the hold horizon.`;
  } else if (capacity.resource_throttle) {
    const resources = Array.isArray(capacity.resource_throttle.resources)
      ? capacity.resource_throttle.resources.map(item => item.resource).join(", ") : "";
    summary = `New tasks cannot start until the host resource throttle clears${resources ? ` (${resources})` : ""}.`;
  } else if (capacity.admissions_stopped === true) {
    summary = "This window has stopped starting new tasks. Tasks already admitted keep running.";
  } else if (Number.isFinite(free) && free <= 0 && known && busy >= limit) {
    const toClear = busy - limit + 1;
    summary = `The workspace leaf limit is reached; ${autoDrainPlural(toClear, "occupied slot")} must clear before another task can start.`;
  } else if (Number.isFinite(free) && free <= 0) {
    summary = "No admissions are available in this readiness snapshot.";
  } else if (counts.eligible === 0) {
    if (blocked > 0) {
      summary = `No tasks are eligible yet; ${autoDrainPlural(blocked, "pool task")} ${blocked === 1 ? "waits" : "wait"} on task locks or live claims.`;
    } else {
      summary = counts.waiting > 0
      ? `Nothing is eligible yet; ${autoDrainPlural(counts.waiting, "task")} ${counts.waiting === 1 ? "waits" : "wait"} in the pool.`
      : "Nothing is waiting in the backlog.";
    }
  } else {
    summary = `A window started now admits up to ${autoDrainPlural(admits, "task")}.`;
  }
  const label = known ? `Workspace: ${busy} of ${limit} leaf slots in use` : "Workspace: capacity unknown";
  const full = Number.isFinite(free) && free <= 0;
  const bar = el("div", { class: "drain-capacity-bar" });
  bar.setAttribute("role", "img");
  bar.setAttribute("aria-label", label);
  if (known) {
    const scale = Math.max(limit, busy);
    // One segment per slot while they stay wide enough to count at a glance;
    // past that a continuous fill reads better than slivers.
    if (scale <= 24) {
      bar.classList.add("segmented");
      for (let slot = 0; slot < scale; slot += 1) {
        const state = slot >= limit ? " over" : slot < busy ? " used" : "";
        bar.appendChild(el("span", { class: `drain-capacity-seg${state}` }));
      }
    } else {
      const within = Math.min(busy, limit);
      bar.append(
        el("span", { class: "drain-capacity-used", style: { width: `${(within / scale) * 100}%` } }),
        el("span", { class: "drain-capacity-over", style: { width: `${(Math.max(0, busy - limit) / scale) * 100}%` } }),
      );
    }
  }
  const pipelineCounts = Object.entries(capacity.leaf_occupancy_by_pipeline || {})
    .filter(([, count]) => Number(count) > 0);
  const pipelines = pipelineCounts.map(([pipeline, count]) => [pipeline, Number(count)]);
  // Legacy wrappers can occupy slots without appearing in the pipeline map.
  const other = busy - pipelineCounts.reduce((sum, [, count]) => sum + Number(count), 0);
  if (pipelines.length && other > 0) pipelines.push(["other", other]);
  const byPipeline = pipelines.map(([pipeline, count]) => `${pipeline}: ${autoDrainPlural(count, "slot")}`);
  // A shortfall the operator should notice reads amber; a stopped window is
  // the operator's own choice and does not.
  const held = full && capacity.admissions_stopped !== true;
  const count = el("span", {
    class: `drain-capacity-count mono${full ? " full" : ""}`,
    text: known ? `${busy}/${limit} slots` : "capacity unknown",
    title: [label, Number.isFinite(free) ? `${Math.max(0, free)} free` : "", ...byPipeline].filter(Boolean).join("\n"),
  });
  const result = el("div", { class: "drain-capacity" }, [
    bar,
    el("div", { class: "drain-capacity-line" }, [
      el("p", { class: `drain-slots${held ? " held" : ""}`, text: summary }),
      count,
    ]),
  ]);
  // One pipeline holding every slot is already in the count's tooltip; a mix
  // is worth a visible line.
  if (pipelines.length > 1) {
    const list = el("ul", { class: "drain-pipeline-occupancy" }, byPipeline.map(text => el("li", { class: "drain-pipeline mono", text })));
    list.setAttribute("aria-label", "Workspace slots by pipeline");
    result.appendChild(list);
  }
  return result;
}

function autoDrainPlural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

// A throttle reading in the unit the top bar uses: CPU as load per core
// ("load 1.6× cores"), memory and disk as percentages. A threshold is bare
// (no leading label) and keeps two decimals so 0.75× does not round to 0.8×.
function throttleReading(resource, percent, threshold = false) {
  if (resource === "cpu") {
    const load = cpuLoadMultiple(Number(percent), threshold);
    return threshold ? `${load} cores` : `load ${load} cores`;
  }
  return threshold ? `${percent}%` : `${resource} ${Math.round(Number(percent))}%`;
}

// The resources a throttle holds, or an empty list.
function throttledResources(capacity) {
  const resources = capacity?.resource_throttle?.resources;
  return Array.isArray(resources) ? resources.filter(item => item && item.resource) : [];
}

// A throttled resource by the name the top bar gives it.
function throttleName(resource) {
  return resource === "cpu" ? "load" : String(resource);
}

function throttleNames(resources) {
  const names = [...new Set(resources.map(item => throttleName(item.resource)))];
  return names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}` : names[0] || "";
}

// One resource's reading against its two thresholds: the fill is the reading,
// the green tick where admissions resume, the red one where they stop. The
// scale runs a quarter past the higher of the reading and the stop threshold.
function throttleGauge(item) {
  const percent = Number(item.percent);
  const high = Number(item.high_percent);
  const resume = Number(item.resume_percent);
  if (![percent, high, resume].every(Number.isFinite)) return null;
  const scale = Math.max(high, percent) * 1.25 || 100;
  const at = value => `${Math.min(100, Math.max(0, (value / scale) * 100))}%`;
  const tick = value => (item.resource === "cpu" ? cpuLoadMultiple(value, true) : `${Math.round(value)}%`);
  const now = item.resource === "cpu" ? `${cpuLoadMultiple(percent)} cores` : `${Math.round(percent)}%`;
  return el("div", { class: "drain-gauge" }, [
    el("div", { class: "drain-gauge-head" }, [
      el("span", { text: `${throttleName(item.resource)} now` }),
      el("span", { class: "drain-gauge-now mono", text: now }),
    ]),
    el("div", { class: "drain-gauge-track" }, [
      el("span", { class: "drain-gauge-fill", style: { width: at(percent) } }),
      el("span", { class: "drain-gauge-mark resume", style: { left: at(resume) } }),
      el("span", { class: "drain-gauge-mark high", style: { left: at(high) } }),
    ]),
    el("div", { class: "drain-gauge-scale mono" }, [
      el("span", { class: "resume", text: `resume ${tick(resume)}`, style: { left: at(resume) } }),
      el("span", { class: "high", text: `stop ${tick(high)}`, style: { left: at(high) } }),
    ]),
  ]);
}

// Sustained host pressure holds every new admission until it clears, so the
// page leads with it: what is held, when it resumes and since when, and a
// gauge per resource. The full sentence (each reading, both thresholds and
// since-when) is what assistive technology announces; the visible parts say
// the same in short.
function throttleSentence(resources) {
  const held = resources
    .map(item => `${throttleReading(item.resource, item.percent)} (throttled at ≥ ${throttleReading(item.resource, item.high_percent, true)} since ${time(item.since)}; resumes below ${throttleReading(item.resource, item.resume_percent, true)})`)
    .join("; ");
  return `Admissions throttled: ${held}. Running tasks are not touched.`;
}

function autoDrainThrottleNote(capacity) {
  const resources = throttledResources(capacity);
  if (resources.length === 0) return null;
  const sentence = throttleSentence(resources);
  const resumes = resources
    .map(item => `${throttleName(item.resource)} falls below ${throttleReading(item.resource, item.resume_percent, true)}`)
    .join(" and ");
  const since = resources.map(item => Date.parse(item.since || "")).filter(Number.isFinite).sort((a, b) => a - b)[0];
  const detail = [
    `New tasks start once ${resumes}.`,
    Number.isFinite(since) ? `Throttled since ${clockHm(new Date(since).toISOString())}.` : "",
    "Running tasks are not affected.",
  ].filter(Boolean).join(" ");
  const note = el("section", { class: "operation-control-note drain-throttle-note", title: sentence }, [
    el("div", { class: "drain-banner-text", "aria-hidden": "true" }, [
      el("span", { class: "drain-banner-icon" }),
      el("div", { class: "drain-banner-copy" }, [
        el("p", { class: "drain-banner-headline", text: `Admissions paused — host ${throttleNames(resources)} ${resources.length > 1 ? "are" : "is"} throttled` }),
        el("p", { class: "drain-banner-detail", text: detail }),
      ]),
    ]),
    el("div", { class: "drain-banner-gauges", "aria-hidden": "true" }, resources.map(throttleGauge).filter(Boolean)),
    el("span", { class: "visually-hidden", text: sentence }),
  ]);
  note.setAttribute("role", "status");
  return note;
}

// The status card's own copy of the throttle: the full sentence as one amber
// note. The Tasks dock hides the page banner, so drain.css shows this note
// there and nowhere else.
function autoDrainThrottleLine(capacity) {
  const resources = throttledResources(capacity);
  if (resources.length === 0) return null;
  const note = el("p", { class: "operation-control-note drain-throttle-line", text: throttleSentence(resources) });
  note.setAttribute("role", "status");
  return note;
}

// The top of the page: the throttle banner, when a throttle holds admissions.
function renderDrainBanner(payload) {
  const banner = $("drain-banner");
  if (!banner) return;
  const note = payload ? autoDrainThrottleNote(payload.capacity) : null;
  banner.textContent = "";
  banner.hidden = !note;
  if (note) banner.append(note);
}

function hideDrainPageExtras() {
  for (const id of ["drain-banner", "drain-summary"]) {
    const node = $(id);
    if (!node) continue;
    node.textContent = "";
    node.hidden = true;
  }
}

// The locks payload (`/api/tasks/locks`) the Locks held card renders; the
// summary tiles and the wait rows read it too. app.js hands it over.
let lastDrainLocks = null;

export function setDrainLocks(payload) {
  lastDrainLocks = payload && Array.isArray(payload.by_task) ? payload : null;
  if (!lastAutoDrain || !selectedWorkspace()) return;
  renderDrainSummary(lastAutoDrain);
  renderDrainWaits(lastAutoDrain);
}

// How many waiting tasks name each task as their lock holder, from the last
// readiness snapshot of the selected workspace.
export function drainHolderCounts() {
  const counts = new Map();
  const tasks = Array.isArray(lastAutoDrain?.tasks) ? lastAutoDrain.tasks : [];
  for (const entry of tasks) {
    if (!entry || entry.eligible === true) continue;
    for (const holder of drainReadinessView(entry).badge?.holders || []) counts.set(holder, (counts.get(holder) || 0) + 1);
  }
  return counts;
}

function drainTile(key, label, value, sub, lead = null) {
  const tile = el("div", { class: "drain-tile" }, [
    el("span", { class: "drain-tile-label", text: label }),
    el("span", { class: "drain-tile-value" }, [lead, ...value]),
    sub ? el("span", { class: "drain-tile-sub", text: sub }) : null,
  ]);
  tile.dataset.tile = key;
  return tile;
}

// Four figures across the top: the window's state, slots in use, the queue
// and locked files. A tile whose figures are unknown is left out.
function renderDrainSummary(payload) {
  const summary = $("drain-summary");
  if (!summary) return;
  summary.textContent = "";
  if (!payload || !selectedWorkspace()) {
    summary.hidden = true;
    return;
  }
  const { capacity, phase, running, label, pulling, pullLabel } = autoDrainPhase(payload);
  const tiles = [];
  const state = pulling ? "pulling" : phase;
  const word = pulling ? pullLabel : phase === "idle" ? (workspaceReadOnlyReason() ? "Read-only" : "Idle") : label;
  const stateSub = pulling ? "A replica pull drain is admitting"
    : phase === "draining" ? autoDrainTimeLeft(capacity.ends_at) || "A window is admitting"
      : phase === "winding_down" ? `${autoDrainPlural(running, "task")} still running`
        : "No window is running";
  tiles.push(drainTile("state", "Auto-drain", [el("span", { text: word })], stateSub,
    el("span", { class: `drain-dot ${state}`, "aria-hidden": "true" })));
  const busy = autoDrainNumber(capacity.active_leaf_runs ?? capacity.occupancy?.active_leaf_runs);
  const limit = autoDrainNumber(capacity.max_active_leaf_runs);
  if (Number.isFinite(busy) && Number.isFinite(limit) && limit > 0) {
    const free = autoDrainNumber(capacity.free_slots);
    tiles.push(drainTile("slots", "Slots in use", [String(busy), el("span", { class: "drain-tile-of", text: ` / ${limit}` })],
      Number.isFinite(free) ? `${Math.max(0, free)} free · parallel limit ${limit}` : `Parallel limit ${limit}`));
  }
  if (Array.isArray(payload.tasks)) {
    const counts = autoDrainCounts(payload);
    tiles.push(drainTile("queue", "Queue", [String(counts.waiting), el("span", { class: "drain-tile-unit", text: " waiting" })],
      `${counts.eligible} eligible right now`));
  }
  if (lastDrainLocks) {
    const files = Number(lastDrainLocks.total_locked);
    const holders = Number.isFinite(Number(lastDrainLocks.total_tasks)) ? Number(lastDrainLocks.total_tasks) : lastDrainLocks.by_task.length;
    tiles.push(drainTile("locks", "Locked files", [String(Number.isFinite(files) ? files : 0)],
      holders > 0 ? `Held by ${autoDrainPlural(holders, "task")}` : "No task holds a lock"));
  }
  summary.append(...tiles);
  summary.hidden = false;
}

// [ORB-14698] Each provider usage window that keeps crews out of admission,
// with its use, threshold, reset and the crews skipped until then. Readings
// below their threshold do not hold anything, so they are not shown here.
function autoDrainProviderLimitNote(readings) {
  const gated = (Array.isArray(readings) ? readings : []).filter(reading => reading?.gated === true);
  if (gated.length === 0) return null;
  const lines = gated.map((reading) => {
    // A crew that sets `env` is its own account, named `provider/crew`.
    const account = reading.account ? `${reading.provider}/${reading.account}` : reading.provider;
    const label = `${account}${reading.scope ? ` [${reading.scope}]` : ""} ${reading.window || "usage window"}`;
    const used = reading.used_percent == null ? "exhausted" : `${reading.used_percent}% ≥ ${reading.threshold}%`;
    // A ledger budget in dollars leaves out invocations that recorded no cost.
    const usage = reading.partial === true ? `${used} (partial: some invocations report no cost)` : used;
    const crews = Array.isArray(reading.crews) && reading.crews.length ? reading.crews.join(", ") : "no configured crew";
    return `${label} ${usage} until ${time(reading.until)}: ${crews} skipped`;
  });
  const note = el("p", { class: "operation-control-note drain-provider-limit-note", text: `Provider limits: ${lines.join("; ")}.` });
  note.setAttribute("role", "status");
  return note;
}

// One row per host and provider account whose login is still failing: the
// provider's default login, or a crew that sets `env` (`provider/crew`). A
// later successful run of that account on that host drops the row.
function autoDrainProviderAuth(failures) {
  const items = Array.isArray(failures) ? failures : [];
  if (items.length === 0) return null;
  const list = el("ul", { class: "drain-provider-auth" });
  list.setAttribute("aria-label", "Provider authentication failures");
  for (const item of items) {
    const row = el("li", { class: "drain-provider-auth-item" }, [
      `${item.provider || "provider"}${item.account ? `/${item.account}` : ""} auth failed on ${item.host || "this host"}`,
    ]);
    const runs = Array.isArray(item.runs) ? item.runs : [];
    runs.forEach((run, index) => {
      row.append(index === 0 ? " " : ", ");
      if (run?.run_id) row.append(runLink(run.run_id));
    });
    if (item.fix_command) {
      row.append(" ");
      row.append(el("code", { class: "drain-provider-auth-fix", text: item.fix_command }));
    }
    list.append(row);
  }
  return list;
}

// A recorded forecast object, or null when the field is absent or not an object.
function asForecast(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}

function forecastBuiltAt(forecast) {
  return typeof forecast?.forecast_at === "string" && forecast.forecast_at ? forecast.forecast_at : "";
}

// `detail` and `stall.friction_id` are not rendered: a persisted starvation
// sentence names a task id, and a friction id is not the hold itself.
function forecastHoldDetail(item) {
  const percent = Number(item?.percent);
  const minute = Number(item?.minute);
  if (item?.resource && Number.isFinite(percent) && Number.isFinite(minute)) {
    return `${item.resource} ${percent.toFixed(0)}% at +${minute}m`;
  }
  return "waiting for the protected queue head to fit";
}

function forecastAmount(resource, value) {
  const amount = Number(value);
  if (!Number.isFinite(amount)) return "";
  if (resource === "cpu") return `${amount.toFixed(1)} cores`;
  const units = ["bytes", "KiB", "MiB", "GiB", "TiB"];
  let size = amount;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const text = unit === 0 ? String(Math.round(size)) : size.toFixed(size >= 10 ? 0 : 1);
  return `${text} ${units[unit]}`;
}

function forecastTaskRef(forecast, taskId) {
  const title = forecast.task_titles?.[taskId];
  const link = taskLink(String(taskId), selectedWorkspace()?.id);
  if (!title) return link;
  return el("span", { class: "drain-forecast-task-name" }, [link, " ", String(title)]);
}

function forecastHasDecisions(forecast) {
  if (!forecast) return false;
  const admitted = Array.isArray(forecast.admitted) && forecast.admitted.some(task => task?.task_id);
  const skipped = Array.isArray(forecast.skipped) && forecast.skipped.some(task => task?.task_id);
  return admitted || skipped || Boolean(forecast.head?.task_id && (forecast.head.protected === true || forecast.head.stall?.stuck === true));
}

function forecastTime(at) {
  const node = el("time", { class: "drain-forecast-when", text: time(at), title: String(at) });
  node.dateTime = String(at);
  return node;
}

// The minutes a forecast's recorded series spans.
const FORECAST_SERIES_MINUTES = 60;

// `resource`'s recorded first hour of percents, or null for a record written
// before the series was kept or from an unmeasured host.
function forecastSeries(forecast, resource) {
  const series = asForecast(forecast.series);
  const bucket = Number(series?.bucket_minutes);
  const points = series?.[`${resource}_percent`];
  if (!(bucket > 0) || !Array.isArray(points) || points.length < 2) return null;
  const values = points.map(value => (value == null ? NaN : Number(value)));
  return values.every(Number.isFinite) ? { bucket, values } : null;
}

// The series as a line over 0–60 minutes on a 0–100% scale (taller when a
// point or the limit is above it), the admit threshold as a dashed line, and
// a dot on the highest point labelled with the peak's minute. That point is
// the peak's bucket whenever the peak falls within the hour; otherwise the
// label names the bucket, and the row's text the peak.
function forecastSpark(series, peak, admitNumber, label, hypothetical) {
  const { bucket, values } = series;
  const limited = Number.isFinite(admitNumber);
  const top = Math.max(100, ...values, limited ? admitNumber : 0);
  const x = index => Math.min(100, index * bucket / FORECAST_SERIES_MINUTES * 100);
  const y = value => 100 - Math.max(0, value) / top * 100;
  const highest = values.indexOf(Math.max(...values));
  const peakMinute = Number(peak?.minute);
  const inHour = Number.isFinite(peakMinute) && peakMinute <= FORECAST_SERIES_MINUTES && Math.ceil(peakMinute / bucket) === highest;
  const minute = inHour ? peakMinute : highest * bucket;
  const extra = limited
    ? [svg("line", { x1: 0, x2: 1000, y1: (y(admitNumber) * 10).toFixed(1), y2: (y(admitNumber) * 10).toFixed(1), class: "drain-forecast-spark-limit", "vector-effect": "non-scaling-stroke" })]
    : [];
  const line = sparkline(values.map((value, index) => ({ x: x(index), y: y(value) })), { className: "drain-forecast-spark-svg", lineClass: "drain-forecast-spark-line", extra });
  const left = x(highest);
  const edge = left > 75 ? " end" : left < 25 ? " start" : "";
  const marker = el("span", { class: "drain-forecast-spark-peak", style: { left: `${left}%`, top: `${y(values[highest])}%` } });
  const chart = el("span", { class: `drain-forecast-spark${hypothetical ? " hypothetical" : ""}`, role: "img", "aria-label": label }, [
    el("span", { class: `drain-forecast-spark-label${edge}`, text: `+${minute}m`, style: { left: `${left}%` }, "aria-hidden": "true" }),
    el("span", { class: "drain-forecast-spark-plot", "aria-hidden": "true" }, [line, marker]),
  ]);
  chart.dataset.peakMinute = String(minute);
  return chart;
}

// Each peak against its admit threshold, with both figures beside it: a
// sparkline of the forecast's first hour when the record keeps one, or else
// (and always in the Tasks dock) a meter whose fill is the peak and whose
// tick is the threshold. The amount,
// minute and headroom (in percentage points) are in the row's tooltip and
// read aloud. A shadow forecast's chart is marked hypothetical.
function forecastPeaks(forecast) {
  const headroom = asForecast(forecast.headroom);
  const hypothetical = forecast.mode === "shadow";
  const rows = ["cpu", "memory"].map((resource) => {
    const peak = asForecast(forecast[`${resource}_peak`]);
    if (!peak || (!Number.isFinite(Number(peak.value)) && !Number.isFinite(Number(peak.percent)))) return null;
    const percent = Number(peak.percent);
    const known = Number.isFinite(percent);
    const figure = known ? `${percent.toFixed(1)}%` : "host capacity unknown";
    const amount = forecastAmount(resource, peak.value);
    const when = Number.isFinite(Number(peak.minute)) ? `at +${peak.minute}m` : "";
    const admitKey = resource === "cpu" ? "admit_cpu_percent" : "admit_memory_percent";
    const roomKey = resource === "cpu" ? "cpu_percent" : "memory_percent";
    // An unmeasured host sends its headroom as null, and Number(null) is 0.
    const admitNumber = headroom?.[admitKey] == null ? NaN : Number(headroom[admitKey]);
    const admit = headroom
      ? (Number.isFinite(admitNumber) ? `admit ${headroom[admitKey]}%` : "admit threshold unknown")
      : "";
    const roomNumber = headroom?.[roomKey] == null ? NaN : Number(headroom[roomKey]);
    const room = headroom ? (Number.isFinite(roomNumber) ? `headroom ${roomNumber.toFixed(1)}` : "headroom unknown") : "";
    const over = Number.isFinite(roomNumber) && roomNumber < 0;
    const name = resource === "cpu" ? "CPU" : "Memory";
    const detail = `${name} peak ${[`${figure}${amount ? ` (${amount})` : ""}`, when, admit, room].filter(Boolean).join(" · ")}`;
    const series = known ? forecastSeries(forecast, resource) : null;
    const clamp = value => `${Math.min(100, Math.max(0, value))}%`;
    const meter = el("span", { class: "drain-forecast-meter", "aria-hidden": "true" }, [
      known ? el("span", { class: "drain-forecast-fill", style: { width: clamp(percent) } }) : null,
      Number.isFinite(admitNumber) ? el("span", { class: "drain-forecast-tick", style: { left: clamp(admitNumber) } }) : null,
    ]);
    // With a series the chart leads and the meter follows it for the narrow
    // Tasks dock, which draws the meter instead (drain.css, tasks.css).
    const graphic = [meter];
    if (series) {
      const limit = Number.isFinite(admitNumber) ? `admit limit ${admitNumber}%` : "admit limit unknown";
      const label = `${hypothetical ? "Hypothetical " : ""}${name} forecast over the next ${FORECAST_SERIES_MINUTES} minutes: peak ${figure} ${when}, ${limit}`;
      graphic.unshift(forecastSpark(series, peak, admitNumber, label, hypothetical));
    }
    // The figure reads as the peak against its admit threshold, then when it
    // peaks, the amount and how far it sits from that ceiling, in percentage
    // points.
    const margin = Number.isFinite(roomNumber)
      ? `${Math.abs(roomNumber).toFixed(1)} pts ${roomNumber < 0 ? "over" : "under"} the ceiling`
      : "";
    return el("div", { class: `drain-forecast-peak${over ? " over" : ""}`, title: detail }, [
      el("dt", { text: name }),
      el("dd", { class: "drain-forecast-bar" }, graphic),
      el("dd", { class: "drain-forecast-figure" }, [
        el("span", { class: "drain-forecast-figure-main mono", "aria-hidden": "true" }, [
          known ? `${percent.toFixed(1)}%` : "host capacity unknown",
          Number.isFinite(admitNumber) ? el("span", { class: "drain-forecast-threshold", text: ` / ${admitNumber}%` }) : null,
        ]),
        el("span", { class: "drain-forecast-figure-sub", "aria-hidden": "true", text: [when ? `peak ${when}` : "", amount, margin].filter(Boolean).join(" · ") }),
        el("span", { class: "visually-hidden", text: detail }),
      ]),
    ]);
  }).filter(Boolean);
  if (rows.length === 0) return null;
  const list = el("dl", { class: "drain-forecast-peaks" }, rows);
  list.setAttribute("aria-label", "Forecast peaks against admit thresholds");
  return list;
}

function forecastDecisions(forecast, hypothetical) {
  const items = [];
  const admitVerb = hypothetical ? "Would admit " : "Admitted ";
  const holdVerb = hypothetical ? "Would hold " : "Held ";
  for (const task of Array.isArray(forecast.admitted) ? forecast.admitted : []) {
    if (!task?.task_id) continue;
    const notes = [`profile ${task.profile_source || "unknown"}`];
    if (task.idle_host) notes.push(String(task.idle_host));
    // An admitted task stays on one line; its full text is the tooltip.
    const item = el("li", { class: "drain-forecast-task admitted" }, [
      admitVerb,
      forecastTaskRef(forecast, task.task_id),
      ` · ${notes.join(" · ")}`,
    ]);
    item.title = item.textContent;
    items.push(item);
  }
  for (const task of Array.isArray(forecast.skipped) ? forecast.skipped : []) {
    if (!task?.task_id) continue;
    items.push(el("li", { class: "drain-forecast-task held" }, [
      holdVerb,
      forecastTaskRef(forecast, task.task_id),
      ` · ${forecastHoldDetail(task)} · profile ${task.profile_source || "unknown"}`,
    ]));
  }
  if (forecast.head?.protected === true && forecast.head.task_id) {
    items.push(el("li", { class: "drain-forecast-task held" }, [
      hypothetical ? "Starvation guard would protect " : "Starvation guard protects ",
      forecastTaskRef(forecast, forecast.head.task_id),
    ]));
  }
  const stall = forecast.head?.stall;
  if (stall?.stuck === true && forecast.head.task_id) {
    const miss = forecast.head.miss || {};
    const resource = miss.resource || "resource";
    const profile = miss.profile_key
      ? `profile ${miss.profile_key} via ${miss.profile_source || "?"}`
      : `profile ${miss.profile_source || "none"}`;
    const baseline = Number.isFinite(Number(miss.baseline_percent))
      ? `${Number(miss.baseline_percent).toFixed(0)}%`
      : "unmeasured";
    const since = stall.since ? time(stall.since) : "unknown";
    items.push(el("li", { class: "drain-forecast-task held" }, [
      `${hypothetical ? "Stuck head (shadow; hypothetical)" : "Stuck head"}: `,
      forecastTaskRef(forecast, forecast.head.task_id),
      ` — ${forecastHoldDetail(miss)} (${profile}; ${resource} baseline ${baseline}) has not improved over ${stall.passes ?? "?"} protected passes since ${since}.`,
    ]));
  }
  if (items.length === 0) return null;
  const list = el("ul", { class: "drain-forecast-tasks" }, items);
  list.setAttribute("aria-label", hypothetical ? "Hypothetical forecast decisions" : "Forecast decisions");
  return list;
}

function forecastHead(label, ...meta) {
  return el("div", { class: "drain-forecast-head" }, [
    el("span", { class: "drain-section-label", text: label }),
    el("span", { class: "drain-forecast-meta" }, meta.filter(Boolean)),
  ]);
}

function forecastBlock(forecast, { retained }) {
  const hypothetical = forecast.mode === "shadow";
  const at = forecastBuiltAt(forecast);
  const block = el("div", { class: retained ? "drain-forecast-retained" : "drain-forecast-built" });
  block.dataset.forecastMode = forecast.mode || "";
  block.append(
    forecastHead(
      retained ? "Last forecast" : "Forecast",
      at ? forecastTime(at) : null,
      at ? " · " : null,
      el("span", { class: `drain-forecast-mode${hypothetical ? " shadow" : ""}`, text: hypothetical ? "shadow · hypothetical" : (forecast.mode || "forecast") }),
    ),
    ...[forecastPeaks(forecast), forecastDecisions(forecast, hypothetical)].filter(Boolean),
  );
  return block;
}

// The last local pass's gap-fit forecast. Readiness puts the pass on
// `capacity.admission_forecast` and the last forecast that was actually built
// on `capacity.last_admission_forecast`. A pass that built none says so, unless
// a retained forecast stands in and this pass decided nothing of its own.
function autoDrainForecast(payload) {
  const capacity = payload?.capacity || {};
  const current = asForecast(capacity.admission_forecast);
  const last = asForecast(capacity.last_admission_forecast);
  const section = el("div", { class: "drain-forecast", role: "group", "aria-label": "Admission forecast" });
  if (forecastBuiltAt(current)) {
    section.dataset.forecastState = "built";
    section.append(forecastBlock(current, { retained: false }));
    return section;
  }
  const retained = forecastBuiltAt(last);
  section.dataset.forecastState = retained ? "retained" : "none";
  const decisions = forecastHasDecisions(current) ? forecastDecisions(current, current.mode === "shadow") : null;
  if (decisions || !retained) {
    section.append(forecastHead("Forecast", el("span", { class: "drain-forecast-state", text: "no forecast this pass" })));
    if (decisions) section.append(decisions);
  }
  if (retained) section.append(forecastBlock(last, { retained: true }));
  return section;
}

// The status card reads top to bottom: the live window's figures (if any),
// the slots it competes for and why nothing more starts, any provider or crew
// hold, the last admission forecast, then the run and Stop. Its deadline sits
// in the header. A host throttle leads the page as a banner above the summary
// tiles (the Tasks dock, which hides the banner, shows the card's own note),
// and starting a window is its own card beside, hidden in a read-only view.
function renderAutoDrain(payload) {
  lastAutoDrain = payload;
  const waits = $("drain-waits-panel");
  if (waits) waits.hidden = false;
  setDrainReadiness(payload);
  renderDrainWaits(payload);
  renderDrainBanner(payload);
  renderDrainSummary(payload);
  const body = $("auto-drain-body");
  if (!body) return;
  // A poll or a control click rebuilds both cards; keep keyboard focus on the
  // control that had it.
  const focusKey = document.activeElement?.dataset?.drainFocus;
  body.textContent = "";
  renderAutoDrainHead(payload);
  const reasons = autoDrainReasons(payload);
  const holds = [
    // Shown only where the banner is not (the Tasks dock).
    autoDrainThrottleLine(payload.capacity),
    autoDrainProviderLimitNote(payload.provider_limits),
    autoDrainProviderAuth(payload.provider_auth_failures),
    drainCrewNote(payload),
  ].filter(Boolean);
  if (reasons.submit) {
    body.append(...[autoDrainForecast(payload), ...holds, autoDrainQueued(payload)].filter(Boolean), el("div", { class: "operations-readonly-note", text: reasons.submit }));
    renderDrainStart(payload, reasons.submit);
    return;
  }
  body.append(...[
    autoDrainWindow(payload),
    autoDrainCapacity(payload.capacity || {}, autoDrainCounts(payload), autoDrainBlocked(payload).length),
    ...holds,
    autoDrainForecast(payload),
    autoDrainFoot(payload),
    autoDrainQueued(payload),
  ].filter(Boolean));
  renderDrainStart(payload, reasons.submit);
  if (focusKey) document.querySelector?.(`[data-drain-focus="${focusKey}"]`)?.focus();
}

const DRAIN_POOL_SHORT = { eligible: "Eligible", locks: "Locks", capacity: "Capacity", other: "Other" };
// Holder and dependency links a wait row shows before folding the rest.
const DRAIN_WAIT_LINKS = 3;
const drainExpandedWaitGroups = new Set();

// Task links for `ids`, comma-separated, the rest folded into "+N more".
function drainTaskLinks(ids) {
  const shown = ids.slice(0, DRAIN_WAIT_LINKS);
  const nodes = [];
  shown.forEach((id, index) => {
    if (index > 0) nodes.push(", ");
    nodes.push(taskLink(id, selectedWorkspace()?.id));
  });
  if (ids.length > shown.length) nodes.push(` +${ids.length - shown.length} more`);
  return nodes;
}

function drainWaitingTaskLinks(group) {
  if (!group.taskIds.length) return null;
  const workspaceId = selectedWorkspace()?.id;
  const links = el("span", { class: "drain-wait-task-links" }, group.taskIds.flatMap((taskId, index) => [
    ...(index ? [", "] : []),
    taskLink(taskId, workspaceId),
  ]));
  if (group.taskIds.length <= DRAIN_WAIT_LINKS) {
    return el("div", { class: "drain-wait-tasks" }, [el("span", { class: "drain-wait-task-label", text: "Waiting tasks: " }), links]);
  }
  const disclosure = el("details", { class: "drain-wait-task-disclosure" });
  disclosure.open = drainExpandedWaitGroups.has(group.key);
  const summary = el("summary", { text: `${group.taskIds.length} waiting tasks` });
  summary.addEventListener("click", () => {
    if (disclosure.open) drainExpandedWaitGroups.delete(group.key);
    else drainExpandedWaitGroups.add(group.key);
  });
  disclosure.append(summary, links);
  disclosure.addEventListener("toggle", () => {
    if (disclosure.open) drainExpandedWaitGroups.add(group.key);
    else drainExpandedWaitGroups.delete(group.key);
  });
  return el("div", { class: "drain-wait-tasks" }, [disclosure]);
}

// The muted line under a wait: what the holder holds (from the locks card's
// payload), the run that claims it, the dependencies still open, when a
// throttle clears, or the server's own detail.
function drainWaitDetail(group, capacity) {
  const { badge } = group;
  if (badge.holders.length) {
    const held = badge.holders
      .map(id => lastDrainLocks?.by_task.find(task => String(task?.id) === id))
      .filter(Boolean)
      .map(task => {
        const files = Array.isArray(task.context_files) ? task.context_files.length : 0;
        return `${task.id} holds ${autoDrainPlural(files, "locked file")}${task.status ? ` · ${normalizeTaskStatus(task.status)}` : ""}`;
      });
    return held.length ? held.join("; ") : badge.detail;
  }
  if (badge.runIds.length) {
    const runs = [];
    badge.runIds.slice(0, DRAIN_WAIT_LINKS).forEach((runId, index) => {
      if (index > 0) runs.push(", ");
      runs.push(runLink(runId, selectedWorkspace()?.id, autoDrainShortRunId(runId)));
    });
    return ["Claimed by ", ...runs];
  }
  if (group.dependencies.length) return ["Waiting for ", ...drainTaskLinks(group.dependencies)];
  if (badge.reason === "resource_throttled") {
    const resources = throttledResources(capacity);
    if (resources.length) {
      return `Clears when ${resources.map(item => `${throttleName(item.resource)} falls below ${throttleReading(item.resource, item.resume_percent, true)}`).join(" and ")}`;
    }
  }
  return badge.detail;
}

// Why the backlog waits: the snapshot split by pool as a stacked bar, then
// one row per wait (the same words as the Tasks readiness dots), with its
// count, its pool's colour, a detail line, holder links and member task links.
function renderDrainWaits(payload) {
  const body = $("drain-waits-body");
  if (!body) return;
  body.textContent = "";
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  const counts = autoDrainCounts(payload);
  const title = $("drain-waits-title");
  if (title) {
    title.textContent = counts.waiting === 0 ? "Nothing is waiting"
      : `Why ${autoDrainPlural(counts.waiting, "task")} ${counts.waiting === 1 ? "is" : "are"} waiting`;
  }
  const pools = Object.fromEntries(DRAIN_POOLS.map(({ key }) => [key, 0]));
  const groups = new Map();
  for (const entry of tasks) {
    const view = drainReadinessView(entry);
    pools[view.pool] = (pools[view.pool] || 0) + 1;
    if (!view.badge) continue;
    const key = view.badge.text;
    const group = groups.get(key) || { key, count: 0, pool: view.pool, badge: view.badge, dependencies: [], taskIds: [] };
    group.count += 1;
    if (typeof entry.task_id === "string" && entry.task_id) group.taskIds.push(entry.task_id);
    for (const { taskId } of view.badge.dependencies) if (!group.dependencies.includes(taskId)) group.dependencies.push(taskId);
    groups.set(key, group);
  }
  for (const key of drainExpandedWaitGroups) if (!groups.has(key)) drainExpandedWaitGroups.delete(key);
  if (tasks.length) {
    const bar = el("div", { class: "drain-pool-bar", role: "img",
      "aria-label": DRAIN_POOLS.map(({ key, label }) => `${label}: ${pools[key]}`).join(", ") });
    for (const { key } of DRAIN_POOLS) {
      if (!pools[key]) continue;
      const segment = el("span", { class: "drain-pool-seg", style: { flexGrow: String(pools[key]) } });
      segment.dataset.pool = key;
      bar.appendChild(segment);
    }
    const legend = el("ul", { class: "drain-pool-legend", "aria-hidden": "true" }, DRAIN_POOLS.map(({ key }) => {
      const item = el("li", { class: "drain-pool-key" }, [el("span", { class: "drain-pool-swatch" }), `${DRAIN_POOL_SHORT[key]} ${pools[key]}`]);
      item.dataset.pool = key;
      return item;
    }));
    body.append(el("div", { class: "drain-pools" }, [bar, legend]));
  }
  if (groups.size) {
    const rows = [...groups.values()].sort((left, right) => right.count - left.count).map((group) => {
      const { badge } = group;
      const reason = el("span", { class: "drain-wait-reason" }, badge.holders.length
        ? ["waits on ", ...drainTaskLinks(badge.holders)]
        : [badge.text]);
      const detail = drainWaitDetail(group, payload.capacity);
      const row = el("li", { class: "drain-wait", title: badge.title }, [
        el("span", { class: "drain-wait-count mono", text: String(group.count) }),
        el("span", { class: "drain-wait-swatch", "aria-hidden": "true" }),
        el("span", { class: "drain-wait-text" }, [
          reason,
          detail && detail.length ? el("span", { class: "drain-wait-detail" }, Array.isArray(detail) ? detail : [detail]) : null,
          drainWaitingTaskLinks(group),
        ]),
      ]);
      row.dataset.pool = group.pool;
      return row;
    });
    body.append(el("ul", { class: "drain-waits-list" }, rows));
  } else {
    body.append(el("p", { class: "drain-waits-note", text: tasks.length
      ? `Every backlog task is eligible: ${autoDrainPlural(counts.eligible, "task")} can start now.`
      : "Nothing is waiting in the backlog." }));
  }
  if (payload.capacity?.candidate_pool_truncated) body.append(el("p", {
    class: "drain-waits-note", text: "The candidate pool is truncated; these waits cover only the returned tasks.",
  }));
}

// Run detail already exposes the last durable admission pass, the inputs the
// window recorded and a pull drain's claims. Read it only while Drain is open;
// the readiness and fleet polling cadence stays intact.
function fetchDrainPass(payload) {
  const body = $("drain-pass-body");
  if (!body || context?.getTab?.() !== "drain") return Promise.resolve();
  const capacity = payload.capacity || {};
  const runId = capacity.drain_status_run_id || capacity.drain_run_id || capacity.pull_drain_run_id;
  if (!runId) {
    return requestPanel("drain-pass-body", "idle", () => Promise.resolve(null), () => {
      body.textContent = "No active admission window.";
      hideDrainStarted();
    });
  }
  const path = `/api/runs/${encodeURIComponent(runId)}`;
  return requestPanel("drain-pass-body", path, () => fetchJson(path), detail => {
    body.textContent = "";
    renderDrainStarted(detail, payload);
    const link = runLink(runId, getWorkspace(), "Open admission run");
    const pass = detail.run?.drain_last_pass;
    if (pass) {
      body.append(el("p", { text: `Last pass: ${pass.queued || 0} queued · ${Math.max(pass.deferred_total || 0, pass.deferred?.length || 0)} deferred · ${Math.max(pass.excluded_total || 0, pass.excluded?.length || 0)} excluded` }));
      if (pass.recorded_at) body.append(el("p", { class: "drain-waits-note", text: `Recorded ${formatDateTime(pass.recorded_at)}` }));
      if (pass.last_pass_error) body.append(el("p", { class: "drain-throttle-note", text: pass.last_pass_error }));
    } else body.append(el("p", { class: "drain-waits-note", text: "No admission pass recorded yet." }));
    body.append(link);
  });
}

// A panel poll paints the snapshot it already has. Opening the fleet card
// refetches only once that snapshot is older than the fleet cadence. The
// indicator timer, not this poll, is what asks the hosts again.
function loadFleetSnapshot({ refreshWhenStale }) {
  const seen = peekFleetDrains();
  if (seen && (!refreshWhenStale || seen.fresh)) return Promise.resolve(seen.payload);
  return fetchFleetDrains();
}

function renderDrainFleet(payload) {
  const panel = $("drain-fleet-panel");
  const body = $("drain-fleet-body");
  if (!panel || !body) return;
  // Aggregate mode already shows this table in the status card.
  panel.hidden = !selectedWorkspace();
  if (panel.hidden) {
    body.textContent = "";
    return;
  }
  body.replaceChildren(renderFleetDrains(payload, context.selectFleetScope));
}

function paintFleetSurfaces(payload) {
  renderFleetIndicator(payload);
  renderDrainFleet(payload);
  const card = $("auto-drain-panel");
  if (card?.dataset.drainState !== "fleet") return;
  const body = $("auto-drain-body");
  if (body) body.replaceChildren(renderFleetDrains(payload, context.selectFleetScope));
}

let fleetRefreshStarted = false;

// One read after hosts are known, then again on the fleet cadence. This is
// not a panel-poll job: a view's 30s refresh must not fan out to every host.
export function startFleetIndicatorRefresh() {
  const refresh = () => fetchFleetDrains({ force: true }).then(payload => {
    const seen = peekFleetDrains();
    if (seen && seen.payload === payload) paintFleetSurfaces(payload);
  });
  const arm = () => {
    const timer = setTimeout(() => {
      if (document.hidden) {
        arm();
        return;
      }
      refresh().catch(error => console.error(error)).finally(arm);
    }, FLEET_CACHE_MS);
    if (typeof timer === "object" && typeof timer.unref === "function") timer.unref();
  };
  if (!fleetRefreshStarted) {
    fleetRefreshStarted = true;
    document.addEventListener("visibilitychange", () => {
      if (document.hidden) return;
      const seen = peekFleetDrains();
      if (seen?.fresh) return;
      refresh().catch(error => console.error(error));
    });
    arm();
  }
  const seen = peekFleetDrains();
  if (seen?.fresh) {
    paintFleetSurfaces(seen.payload);
    return Promise.resolve(seen.payload);
  }
  return fetchFleetDrains().then(payload => {
    paintFleetSurfaces(payload);
    return payload;
  });
}

function fetchAndRenderAutoDrain() {
  const workspace = selectedWorkspace();
  if (!workspace) {
    return requestPanel("auto-drain-body", "fleet", () => loadFleetSnapshot({ refreshWhenStale: true }), payload => {
      setDrainReadiness({});
      hideDrainPageExtras();
      for (const id of ["drain-waits-panel", "drain-fleet-panel"]) {
        const panel = $(id);
        if (panel) panel.hidden = true;
      }
      const pass = $("drain-pass-body");
      if (pass) pass.textContent = "";
      hideDrainStarted();
      updateDrainIndicators('fleet', 'Fleet drains');
      const card = $("auto-drain-panel");
      if (card) {
        card.dataset.drainState = 'fleet';
        const title = card.querySelector('.drain-title');
        if (title) title.textContent = 'Fleet drains';
      }
      const pill = $("auto-drain-phase");
      if (pill) pill.dataset.drainState = 'fleet';
      const dot = $("auto-drain-dot");
      if (dot) dot.className = 'drain-dot';
      const body = $("auto-drain-body");
      body.replaceChildren(renderFleetDrains(payload, context.selectFleetScope));
      const start = $("auto-drain-start");
      if (start) start.hidden = true;
      const label = $("auto-drain-phase-label");
      if (label) label.textContent = "Fleet";
      const when = $("auto-drain-when");
      if (when) when.textContent = "";
      renderFleetIndicator(payload);
      renderDrainFleet(payload);
    }, "auto-drain-phase-label");
  }
  const concurrency = drainStartConcurrencyQuery();
  const query = concurrency ? `?concurrency=${encodeURIComponent(concurrency)}` : "";
  const scope = "/api/workflows/auto/readiness";
  const path = `${scope}${query}`;
  const visit = captureWorkspaceVisit();
  let rendered = null;
  const readiness = requestPanel("auto-drain-body", scope, () => fetchJson(path), payload => {
    rendered = payload;
    renderAutoDrain(payload);
  }, "auto-drain-phase-label");
  const fleet = loadFleetSnapshot({ refreshWhenStale: false }).then(payload => {
    if (visit.isCurrent()) {
      renderFleetIndicator(payload);
      renderDrainFleet(payload);
    }
    return payload;
  });
  return Promise.all([readiness.then(() => visit.isCurrent() && rendered ? fetchDrainPass(rendered) : undefined), fleet]).then(() => {});
}

// The chrome always names the fleet, including when an idle default workspace
// is selected. The status card describes the selected workspace.
function renderFleetIndicator(payload) {
  const global = $("global-drain-state");
  if (!global) return;
  global.hidden = false;
  const label = fleetDrainLabel(payload);
  const active = payload.items.some(row => ['draining', 'pulling'].includes(row.phase));
  global.dataset.drainState = active ? 'draining' : payload.items.some(row => row.phase === 'winding_down') ? 'winding_down' : 'idle';
  global.setAttribute('aria-label', `${label}. Open fleet drains`);
  global.querySelector('.global-drain-label').textContent = label;
  global.querySelector('.global-drain-running').textContent = '';
  // Open the fleet through the same scope switch used by its rows.
  if (!global.dataset.fleetWired) {
    global.addEventListener('click', () => context.selectFleetScope(null, null));
    global.dataset.fleetWired = 'true';
  }
}

function throwFirstPanelError(results) {
  const errors = results.filter(result => result.status === "rejected").map(result => result.reason);
  // Preserve transport classification even if a different panel also fails.
  if (errors.length) throw errors.find(error => error.networkFailure) || errors[0];
}

export async function fetchAndRenderOperations(subtab = context.getOperationsSubtab()) {
  if (subtab === "auto-tasks") {
    await fetchAndRenderAutoTasks();
    return;
  }
  const routines = fetchJson(withInactivePlugins("/api/routines"));
  throwFirstPanelError(await Promise.allSettled([
    requestPanel("routines-body", "routines", () => routines, renderOperations, "routines-count"),
    requestPanel("clock-body", "clock", () => routines, renderClock, "clock-host"),
  ]));
}

// Readiness refreshes on every page to feed the chip and task readiness.
export async function fetchAndRenderAutoDrainPane() {
  await fetchAndRenderAutoDrain();
}
