// Routine-definition, host clock, and auto-task operations [ORB-10875, ORB-10876].

import { captureWorkspaceVisit, requestPanel, describePullSettlements, copyText, el, infoHint, fetchJson, formatDateTime, getWorkspace, getWorkspaceRevision, hostWriteRefusal, isAggregateView, postJson, reviewLayerText, statusPill, submissionNoticeLines } from './common.js';
import { revealDockStart } from './log-tail.js';
import { renderAutomation } from './automation.js';
import { cpuReadingLabel, cpuReadingValue } from './host-resources.js';
import { DRAIN_LOCK_REASONS, drainReason, drainCrewNote, drainPilotNote, drainReadinessView, setDrainReadiness } from './drain-waits.js';
import { AUTO_DRAIN_APPROVE_RULE, drainCheckout } from './drain-start.js';
import { autoTaskDraft, autoTaskScheduleEditor, closeEditors, editingIn, editorObserved, isOpen, openEditor, routineDraft, routineTriggerEditor, scheduleEditButton } from './schedule-editor.js';

import {
  $,
  actionReason,
  autoDrainNumber,
  cadenceText,
  clockHm,
  clockServiceText,
  clockTickText,
  clockUnavailable,
  clockUnavailableReason,
  controlReason,
  configureOperationsRefresh,
  cronText,
  durationText,
  explainUnavailable,
  feedback,
  field,
  inactivePluginGroup,
  inactivePluginToggle,
  lastMintedText,
  nextEvaluationText,
  noteCronZone,
  operationCell,
  operationColumns,
  operationDetails,
  operationFact,
  operationGroup,
  operationStat,
  operationSwitch,
  operationsState,
  outcomeDot,
  pendingOperations,
  registerOperationsReset,
  relativeTime,
  routineEventDriven,
  routineScheduleText,
  runLink,
  runOperation,
  selectedWorkspace,
  selectedWorkspaceName,
  selectionSnapshot,
  setRailSubtabCount,
  taskLink,
  time,
  whenCell,
  withInactivePlugins,
  workspaceReadOnlyReason,
  workspaceSelectionReason
} from './operations-shared.js';
import { autoDrainForecast, refreshBacklogForecast } from './operations-forecast.js';
import { configureOperationsFleetCallbacks, loadFleetSnapshot, paintAggregateFleet, renderFleetIndicator, startFleetIndicatorRefresh } from './operations-fleet.js';

export { OPERATION_FEEDBACK_CLEAR_MS, initOperations, triggerFeedbackClear } from './operations-shared.js';
export { startFleetIndicatorRefresh };

const UNCONDITIONAL_MINT_WARNING = "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.";
// The Tasks dock's Start card.
const AUTO_DRAIN_DURATION_SECONDS = { "1h": 3600, "2h": 7200, "4h": 14400, "8h": 28800, "12h": 43200, "24h": 86400 };
const AUTO_DRAIN_DURATIONS = Object.keys(AUTO_DRAIN_DURATION_SECONDS);
const AUTO_DRAIN_APPROVE_WARNING = "The window will approve qualifying proposed tasks, including ones filed while it runs.";
const AUTO_DRAIN_COMPLETE_WARNING = "Also marks every task this window ships as done (review -> done), not only the ones eligible right now.";
let routineFailureRequest = 0;
let announcedDrainState = null;
let autoDrainDuration = "1h";
let autoDrainConcurrency = "";
let autoDrainMode = "";
let autoDrainComplete = false;
let autoDrainApproveProposed = false;
let autoDrainAutoFix = false;
// "" keeps workspace review policy; otherwise the layer this window adds.
let autoDrainReview = "";
// Set while Start or Stop is refreshing the card after reporting its result, so
// the state change that result caused is not announced over the result.
let holdDrainAnnouncement = false;
// The operator's unapplied cadence choice, held outside the rebuilt <select>
// so a background refresh cannot revert it. Host-scoped, like the clock itself.
let pendingCadenceSeconds = null;
const TIMELINE_ROWS = 4;

// ---------------------------------------------------------------------------
// Routines
registerOperationsReset(() => {
  closeEditors("routine");
  updateRoutineFailureBadge([]);
});


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
      render: () => { if (operationsState.lastOperations) renderOperations(operationsState.lastOperations); },
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
  link.href = "#jobs/catalogue";
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
  const offset = Number(operationsState.hostCronZone?.offset_seconds);
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

// The schedule column's edit affordance and, while open, the trigger editor.
function routineEditKey(routine) {
  return `routine-trigger:${getWorkspace()}:${routine.name}`;
}

function routineEditButton(payload, routine) {
  const key = routineEditKey(routine);
  return scheduleEditButton({
    key,
    label: `${routine.name} trigger`,
    reason: controlReason(payload, "routine_trigger"),
    onOpen: () => {
      openEditor(key, "routine", routineDraft(routine.trigger), { source: routine.source, target: routine.target, trigger: routine.trigger });
      renderOperations(operationsState.lastOperations || payload, { force: true });
    },
  });
}

function routineEditor(payload, routine) {
  const key = routineEditKey(routine);
  if (!isOpen(key)) return null;
  const selection = selectionSnapshot();
  return routineTriggerEditor({
    key,
    routine,
    cronPreview: cronText,
    save: (trigger) => {
      const observed = editorObserved(key) || { source: routine.source, target: routine.target, trigger: routine.trigger };
      return postJson("/api/routines/trigger", {
        name: routine.name, source: observed.source, target: observed.target,
        machine_name: payload.machine_name, expected_trigger: observed.trigger, trigger,
      }).then((result) => {
        if (selection.current()) feedback("routine-operation-feedback", "success", `${result.message}: ${routine.name}.`);
        return result;
      });
    },
    onClose: (saved) => {
      if (saved) fetchAndRenderOperations().catch(() => {});
      else if (operationsState.lastOperations) renderOperations(operationsState.lastOperations, { force: true });
    },
  });
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
      operationCell("Cadence", [...cadence, routineEditButton(payload, routine)]),
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
  const editor = routineEditor(payload, routine);
  if (editor) card.insertBefore(editor, card.lastChild);
  return card;
}

function renderOperations(payload, { force = false } = {}) {
  noteCronZone(payload);
  operationsState.lastOperations = payload;
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
  // An open trigger editor holds a draft: a background refresh leaves the
  // rows alone until it closes.
  const frozen = !force && editingIn("routine") && body.querySelector(".schedule-editor");
  if (!frozen) renderRoutineRows(payload, body, { workspace, workspaceId, routines, inactive, ownerOnly });
  const activeCount = routines.filter((routine) => routine.enabled).length;
  $("routines-count").textContent = workspace ? `${activeCount} active of ${routines.length} · ${workspace}` : "read-only";
  setRailSubtabCount("rail-count-ops-routines", workspace && routines.length ? `${activeCount}/${routines.length}` : "");
  renderClock(payload);
  syncAutoTaskSchedulerNote();
  if (lastJobs) renderJobs(lastJobs);
}

function renderRoutineRows(payload, body, { workspace, workspaceId, routines, inactive, ownerOnly }) {
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
      render: () => { if (operationsState.lastOperations) renderClock(operationsState.lastOperations); },
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
      render: () => { if (operationsState.lastOperations) renderClock(operationsState.lastOperations); },
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
registerOperationsReset(() => closeEditors("auto-task"));


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
      render: () => { if (operationsState.lastAutoTasks) renderAutoTasks(operationsState.lastAutoTasks); },
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
      render: () => { if (operationsState.lastAutoTasks) renderAutoTasks(operationsState.lastAutoTasks); },
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

function autoTaskEditKey(definition) {
  return `auto-task-schedule:${getWorkspace()}:${definition.name}`;
}

function autoTaskEditButton(payload, definition) {
  const key = autoTaskEditKey(definition);
  return scheduleEditButton({
    key,
    label: `${definition.name} schedule`,
    reason: definition.plugin_inactive ? "Seeded by a plugin that is off here" : actionReason(payload, "auto_task_schedule"),
    onOpen: () => {
      openEditor(key, "auto-task", autoTaskDraft(definition.schedule), { schedule: definition.schedule });
      renderAutoTasks(operationsState.lastAutoTasks || payload, { force: true });
    },
  });
}

function autoTaskEditor(definition) {
  const key = autoTaskEditKey(definition);
  if (!isOpen(key)) return null;
  const selection = selectionSnapshot();
  return autoTaskScheduleEditor({
    key,
    definition,
    cronPreview: cronText,
    save: (schedule) => {
      const observed = editorObserved(key);
      return postJson("/api/auto-tasks/schedule", {
        name: definition.name, expected_schedule: observed ? observed.schedule : definition.schedule, schedule,
      }).then((result) => {
        if (selection.current()) feedback("auto-task-operation-feedback", "success", `${result.message}: ${definition.name}.`);
        return result;
      });
    },
    onClose: (saved) => {
      if (saved) fetchAndRenderAutoTasks()?.catch?.(() => {});
      else if (operationsState.lastAutoTasks) renderAutoTasks(operationsState.lastAutoTasks, { force: true });
    },
  });
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
        autoTaskEditButton(payload, definition),
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
  const editor = autoTaskEditor(definition);
  if (editor) card.insertBefore(editor, card.lastChild);
  return card;
}

// Where the workspace defines an auto_task_scheduler routine, a paused one
// silently stops every scheduled definition, so the pane says so instead of
// leaving "next mint" looking live.
function syncAutoTaskSchedulerNote() {
  const note = $("auto-tasks-scheduler-note");
  if (!note) return;
  const workspace = selectedWorkspaceName();
  const scheduler = workspace && operationsState.lastOperations
    ? (operationsState.lastOperations.routines || []).find((routine) => routine.source === workspace && jobIdFromTarget(routine.target) === "auto_task_scheduler_pipeline")
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

function renderAutoTasks(payload, { force = false } = {}) {
  noteCronZone(payload);
  operationsState.lastAutoTasks = payload;
  const body = $("auto-tasks-body");
  if (!body) return;
  // An open schedule editor holds a draft: leave the rows alone until it closes.
  if (!force && editingIn("auto-task") && body.querySelector(".schedule-editor")) return;
  body.textContent = "";
  const workspace = selectedWorkspace();
  const readable = Boolean(workspace) && !workspaceSelectionReason();
  const reason = workspaceReadOnlyReason() || payload.read_only_reason || "";
  const listed = readable ? (payload.definitions || []) : [];
  const definitions = listed.filter((definition) => !definition.plugin_inactive);
  const inactive = listed.filter((definition) => definition.plugin_inactive);
  if (reason) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: reason }));
  }
  if (definitions.length === 0 && inactive.length === 0) {
    body.appendChild(el("div", {
      class: "empty-state",
      text: readable ? "No auto-task definitions are defined by this workspace." : "Select a workspace to list its auto-task definitions.",
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
  if (readable) {
    const toggle = inactivePluginToggle(payload.inactive_plugin_count || 0);
    if (toggle) body.appendChild(toggle);
  }
  syncAutoTaskSchedulerNote();
  const count = $("auto-tasks-count");
  const enabledCount = definitions.filter(autoTaskOn).length;
  if (count) count.textContent = readable ? `${enabledCount} enabled of ${definitions.length} · ${workspace.name}` : "read-only";
  setRailSubtabCount("rail-count-ops-auto-tasks", readable && definitions.length ? `${enabledCount}/${definitions.length}` : "");
}

// ---------------------------------------------------------------------------
// Jobs. The catalogue is projected from routine targets and recent runs.
// Manual Run submits catalog jobs; delivery pipelines use Ship or Drain.

const JOB_RUN_LIMIT = 100;
let lastJobs = null;
registerOperationsReset(() => { lastJobs = null; });

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
    : controlReason(operationsState.lastOperations, "job_run");
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
  const isLive = payload.view === "live";
  const body = $(isLive ? "jobs-live-body" : "jobs-body");
  const count = $(isLive ? "jobs-live-count" : "jobs-count");
  if (!body) return;
  body.textContent = "";
  const workspace = selectedWorkspace();
  const selectionReason = workspaceSelectionReason();
  if (selectionReason || !workspace) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: selectionReason || "Select a workspace to list its jobs." }));
    body.appendChild(el("div", { class: "empty-state", text: "Select a workspace to list its jobs." }));
    count.textContent = "read-only";
    return;
  }
  // A host that refuses writes still lists its jobs; Run says why it is off.
  const hostRefusal = hostWriteRefusal();
  if (hostRefusal) body.appendChild(el("div", { class: "operations-readonly-note", text: hostRefusal }));
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
  const catalog = jobCatalog(operationsState.lastOperations, runs, workspace.name);
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
  const live = operationsState.context.getJobsSubtab?.() === "live";
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
    (payload) => {
      // Only a fresh catalogue/live read adopts its routines snapshot. A
      // repaint from cached jobs must retain a newer Routines readback.
      operationsState.lastOperations = payload.routines;
      noteCronZone(operationsState.lastOperations);
      updateRoutineFailureBadge(scopedRoutines(operationsState.lastOperations));
      renderJobs(payload);
    }, countId);
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

// The status card and Start card share workspace and authority refusals.
function autoDrainReasons(payload) {
  const workspaceReason = workspaceReadOnlyReason();
  return {
    submit: workspaceReason,
    complete: workspaceReason || (drainCheckout(payload).replica
      ? "A pull replica does not complete work; its owner completes what it ships."
      : payload.controls_authorized === false
      ? "Automatic completion requires an authorized operator session; the window can still start with default review completion."
      : ""),
    // A pull replica cannot approve (`--pull` conflicts with
    // `--approve-proposed`): the owner's window does that.
    approve: workspaceReason || (payload.replica === true
      ? "A pull replica cannot approve proposed tasks; start the window on the owner machine."
      : payload.controls_authorized === false
        ? "Approving proposed tasks requires an authorized operator session; the window can still start and leave them for you."
        : ""),
    autoFix: workspaceReason || (payload.replica === true
      ? "A pull replica cannot repair owner pull requests; start the window on the owner machine."
      : payload.controls_authorized !== true
        ? "Auto-fix requires an authorized operator session."
        : ""),
    // A pull replica's owner decides the review of the claims it hands out
    // (`--pull` refuses `--review`).
    review: workspaceReason || (payload.replica === true
      ? "A pull replica's owner decides the review of its claims; add review on the owner machine."
      : ""),
    // Stop is also the settle-only pass (`orbit run auto --stop`): it delivers
    // recorded settlements and needs no live window, so only a read-only view
    // or a missing operator session blocks it.
    stop: workspaceReason || (payload.controls_authorized === false
      ? "Stopping admissions and settling requires an authorized operator session."
      : ""),
  };
}

// The server takes concurrency as a whole number from 1 (a `u32`). Anything
// else typed into the field would be refused by the readiness read the next
// poll makes (a 400 that blanks the card) and by Start (a 422), so it is held
// back here with the reason shown beside the field.
const AUTO_DRAIN_CONCURRENCY_MAX = 4_294_967_295;
const AUTO_DRAIN_CONCURRENCY_PROBLEM = "Parallel tasks must be a whole number, 1 or more. Leave it blank for the runtime default.";

function autoDrainConcurrencyValid() {
  if (autoDrainConcurrency === "") return true;
  if (!/^\d+$/.test(autoDrainConcurrency)) return false;
  const value = Number(autoDrainConcurrency);
  return value >= 1 && value <= AUTO_DRAIN_CONCURRENCY_MAX;
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
  const pulling = phase === "idle" && Boolean(live.pullRunId) && !workspaceSelectionReason();
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
function drainFieldHead(label, help, { warn = false, id = null } = {}) {
  const head = el("span", { class: "drain-field-head" }, [label]);
  if (help) {
    const hint = infoHint(help, { id });
    if (warn) hint.classList.add("warn");
    head.appendChild(hint);
  }
  return head;
}

// Why a control is unavailable stays visible text: a disabled control with no
// reason in view reads as broken.
function drainFieldRefusal(text, id = null) {
  if (!text) return null;
  const note = el("p", { class: "drain-field-hint warn", text });
  if (id) note.id = id;
  return note;
}

// An on/off choice: the label and its hint on the left, a switch on the right.
function drainSwitchField({ id, label, help, warn = false, checked, disabled = false, title = "", refusal = "", extraClass = "", onChange }) {
  const input = el("input", { class: "drain-switch", role: "switch" });
  input.id = id;
  input.type = "checkbox";
  input.checked = checked;
  input.disabled = disabled;
  input.dataset.drainFocus = id;
  if (title) input.title = title;
  input.setAttribute("aria-describedby", refusal ? `${id}-reason ${id}-help` : `${id}-help`);
  input.addEventListener("change", () => onChange(input.checked));
  const name = el("label", { class: "drain-field-label", text: label });
  name.htmlFor = id;
  return el("div", { class: `drain-field drain-field-row${extraClass ? ` ${extraClass}` : ""}` }, [
    el("div", { class: "drain-field-text" }, [drainFieldHead(name, help, { warn, id: `${id}-help` }), drainFieldRefusal(refusal, `${id}-reason`)]),
    input,
  ]);
}

function autoDrainDurationControl(payload) {
  const segment = el("div", { class: "drain-durations" });
  segment.setAttribute("role", "group");
  segment.setAttribute("aria-label", "Window duration");
  for (const value of AUTO_DRAIN_DURATIONS) {
    const selected = value === autoDrainDuration;
    const option = el("button", { class: `drain-duration mono${selected ? " selected" : ""}`, text: value });
    option.type = "button";
    option.dataset.drainFocus = `duration-${value}`;
    option.setAttribute("aria-pressed", selected ? "true" : "false");
    option.addEventListener("click", () => {
      autoDrainDuration = value;
      renderAutoDrain(payload);
    });
    segment.appendChild(option);
  }
  return segment;
}

const AUTO_DRAIN_MODE_HELP = {
  enforce: "Admits only when the forecast fits.",
  shadow: "Admits freely and records what the forecast would have decided.",
  off: "Admits without forecasting. Host safety limits still apply.",
};

function autoDrainModeControl(payload) {
  const configured = payload.auto_concurrency?.configured_mode;
  const label = el("label", { class: "drain-field-label", text: "Admission control" });
  label.htmlFor = "auto-drain-mode";
  const select = el("select", { class: "drain-mode-select" });
  select.id = "auto-drain-mode";
  select.dataset.drainFocus = "mode";
  for (const [value, text] of [["", `Host default (${configured || "unknown"})`], ["off", "Off"], ["shadow", "Shadow"], ["enforce", "Enforce"]]) {
    const option = el("option", { text });
    option.value = value;
    select.appendChild(option);
  }
  // Older readiness responses cannot authorize an override.
  const authorized = payload.controls_authorized === true && payload.replica !== true;
  select.disabled = !authorized;
  select.value = authorized ? autoDrainMode : "";
  select.addEventListener("change", () => {
    autoDrainMode = select.value;
    renderAutoDrain(payload);
  });
  const chosen = authorized ? autoDrainMode : "";
  const help = chosen
    ? `${AUTO_DRAIN_MODE_HELP[chosen]} For this window only; configuration is unchanged.`
    : `Uses this host's auto-concurrency mode.${AUTO_DRAIN_MODE_HELP[configured] ? ` ${AUTO_DRAIN_MODE_HELP[configured]}` : ""}`;
  return el("div", { class: "drain-field" }, [
    drainFieldHead(label, authorized ? help : ""),
    select,
    drainFieldRefusal(authorized ? "" : "A per-window mode requires an operator session on an owner workspace."),
  ]);
}

// Concurrency is the same blank-means-runtime-default number input as before,
// with − / + around it; the stepper clamps at the input's own minimum of 1.
function autoDrainConcurrencyControl(payload, form) {
  const capacity = payload.capacity || {};
  const limit = autoDrainNumber(capacity.max_active_leaf_runs);
  const fallback = Number.isFinite(limit) && limit >= 1 ? String(Math.trunc(limit)) : "";
  const label = el("label", { class: "drain-field-label", text: "Parallel tasks" });
  label.htmlFor = "auto-drain-concurrency";
  const input = el("input", { class: "drain-stepper-value mono", title: "Leaf-run concurrency (blank = runtime default)" });
  input.id = "auto-drain-concurrency";
  input.type = "number";
  input.min = "1";
  input.placeholder = fallback || "auto";
  input.value = autoDrainConcurrency;
  input.dataset.drainFocus = "concurrency";
  // The message sits under the whole settings row, not in this narrow column.
  const problem = el("div", { class: "operation-control-note drain-stop-note disabled-reason" });
  problem.id = "auto-drain-concurrency-problem";
  problem.setAttribute("role", "alert");
  problem.hidden = true;
  form.problem = problem;
  input.setAttribute("aria-describedby", problem.id);
  // Typing does not rebuild the card (that would drop the caret), so the field,
  // its message and the Start button are brought in line by hand.
  form.syncConcurrency = () => {
    const valid = autoDrainConcurrencyValid();
    input.setAttribute("aria-invalid", valid ? "false" : "true");
    problem.textContent = valid ? "" : AUTO_DRAIN_CONCURRENCY_PROBLEM;
    problem.hidden = valid;
    if (form.start) form.start.disabled = autoDrainStartBlocked(payload);
  };
  input.addEventListener("input", () => {
    autoDrainConcurrency = input.value.trim();
    form.syncConcurrency();
  });
  const step = (delta, name) => {
    const button = el("button", { class: "drain-step", text: delta < 0 ? "−" : "+" });
    button.type = "button";
    button.dataset.drainFocus = `concurrency${delta}`;
    button.setAttribute("aria-label", `${name} concurrency`);
    button.addEventListener("click", () => {
      const current = Math.trunc(Number(autoDrainConcurrency || fallback || 1));
      autoDrainConcurrency = String(Math.max(1, (Number.isFinite(current) ? current : 1) + delta));
      input.value = autoDrainConcurrency;
      form.syncConcurrency();
    });
    return button;
  };
  form.syncConcurrency();
  input.setAttribute("aria-describedby", `${problem.id} auto-drain-concurrency-hint`);
  return el("div", { class: "drain-field drain-field-row" }, [
    el("div", { class: "drain-field-text" }, [
      drainFieldHead(label, "Upper bound; the forecast may admit fewer.", { id: "auto-drain-concurrency-hint" }),
    ]),
    el("div", { class: "drain-stepper" }, [step(-1, "Decrease"), input, step(1, "Increase")]),
  ]);
}

const AUTO_DRAIN_REVIEW_STOP = "Off: shipped tasks stay in review; a separate action completes them.";

// Completion: off stops each shipped task at review, on marks it done. On is
// the riskier choice, so its hint turns amber with the warning.
function autoDrainCompletionControl(payload, reasons) {
  const unavailable = Boolean(reasons.complete) && !autoDrainComplete;
  return drainSwitchField({
    id: "auto-drain-complete",
    label: "Mark shipped tasks done",
    help: unavailable ? reasons.complete : autoDrainComplete ? AUTO_DRAIN_COMPLETE_WARNING : AUTO_DRAIN_REVIEW_STOP,
    warn: autoDrainComplete || unavailable,
    checked: autoDrainComplete,
    disabled: unavailable,
    title: unavailable ? reasons.complete : "",
    // A checked choice can still block Start (a carried-over choice on a
    // replica); its help describes the choice, so the refusal is named here.
    refusal: autoDrainComplete ? reasons.complete : "",
    extraClass: "drain-field-complete",
    onChange: (on) => {
      autoDrainComplete = on;
      renderAutoDrain(payload);
    },
  });
}

// Approving proposed tasks, off by default; on acts on work nobody has looked
// at yet, so its hint turns amber. The qualification rule is in the hint and
// a refusal stays visible.
function autoDrainApproveControl(payload, reasons) {
  return drainSwitchField({
    id: "auto-drain-approve",
    label: "Approve qualifying proposals",
    help: autoDrainApproveProposed ? `${AUTO_DRAIN_APPROVE_WARNING}\n${AUTO_DRAIN_APPROVE_RULE}` : `Off: proposed tasks stay proposed until you approve them.\n${AUTO_DRAIN_APPROVE_RULE}`,
    warn: autoDrainApproveProposed,
    checked: autoDrainApproveProposed,
    disabled: Boolean(reasons.approve) && !autoDrainApproveProposed,
    title: reasons.approve || "",
    refusal: reasons.approve && !reasons.submit ? reasons.approve : "",
    extraClass: "drain-field-approve",
    onChange: (on) => {
      autoDrainApproveProposed = on;
      renderAutoDrain(payload);
    },
  });
}

const AUTO_DRAIN_AUTO_FIX_HELP = "Repair failed required checks and merge conflicts on this window's PRs. Merging still requires Mark shipped tasks done.";

function autoDrainAutoFixControl(payload, reasons) {
  return drainSwitchField({
    id: "auto-drain-auto-fix",
    label: "Auto-fix PR blockers",
    help: AUTO_DRAIN_AUTO_FIX_HELP,
    checked: autoDrainAutoFix,
    // Keep an unavailable selected option clearable after authority changes.
    disabled: Boolean(reasons.autoFix) && !autoDrainAutoFix,
    refusal: reasons.autoFix || "",
    onChange: (on) => {
      autoDrainAutoFix = on;
      renderAutoDrain(payload);
    },
  });
}

const AUTO_DRAIN_REVIEW_HELP = "Adds a second-agent review before landing to every task this window admits. It can only add review: the layer workspace policy already applies is kept, and the other layer is refused.";

// Review before landing for this window only: workspace policy by default, or
// a layer the window adds. Workspace configuration is unchanged.
function autoDrainReviewControl(payload, reasons) {
  const label = el("label", { class: "drain-field-label", text: "Review before landing" });
  label.htmlFor = "auto-drain-review";
  const select = el("select", { class: "drain-mode-select" });
  select.id = "auto-drain-review";
  select.dataset.drainFocus = "review";
  for (const [value, text] of [["", "Workspace policy"], ["before-pr", "Add before-PR review"], ["before-landing", "Add before-landing review"]]) {
    const option = el("option", { text });
    option.value = value;
    select.appendChild(option);
  }
  // Keep an unavailable selected layer clearable after the view changes.
  select.disabled = Boolean(reasons.review) && !autoDrainReview;
  select.value = autoDrainReview;
  select.addEventListener("change", () => {
    autoDrainReview = select.value;
    renderAutoDrain(payload);
  });
  return el("div", { class: "drain-field" }, [
    drainFieldHead(label, autoDrainReview ? AUTO_DRAIN_REVIEW_HELP : "The workspace's review.timing setting decides."),
    select,
    drainFieldRefusal(reasons.review || ""),
  ]);
}

// A replica starts a pull from its owner; an owner starts its own backlog.
function autoDrainStartRefusal(payload) {
  const checkout = drainCheckout(payload, selectedWorkspace()?.id);
  return checkout.replica ? checkout.pullRefusal || (!checkout.ownerHost || !checkout.workspaceId && !checkout.pullSelector
    ? "The owner's host and workspace are unavailable. Refresh before starting a pull window."
    : "") : "";
}

// Why Start is off, in one place so typing in the concurrency field can
// re-evaluate it without a rebuild.
function autoDrainStartBlocked(payload) {
  const reasons = autoDrainReasons(payload);
  return Boolean(reasons.submit || autoDrainStartRefusal(payload))
    || pendingOperations.has("auto-drain:start")
    || !autoDrainConcurrencyValid()
    || (autoDrainComplete && Boolean(reasons.complete))
    || (autoDrainApproveProposed && Boolean(reasons.approve))
    || (autoDrainAutoFix && Boolean(reasons.autoFix))
    || (Boolean(autoDrainReview) && Boolean(reasons.review));
}

function autoDrainStartButton(payload, form) {
  const key = "auto-drain:start";
  const reasons = autoDrainReasons(payload);
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: "operation-button drain-start primary",
    text: pending ? "Starting…" : `${payload.capacity?.drain_phase === "draining" ? "Start another" : "Start"} ${autoDrainDuration} window`,
    title: reasons.submit || autoDrainStartRefusal(payload) || "Submit orbit.workflow.auto with this duration and concurrency",
  });
  button.type = "button";
  button.dataset.drainFocus = "start";
  button.disabled = autoDrainStartBlocked(payload);
  form.start = button;
  form.syncConcurrency?.();
  button.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    if (autoDrainStartBlocked(payload)) return;
    const workspace = selectedWorkspace();
    const checkout = drainCheckout(payload, workspace?.id);
    const counts = autoDrainCounts(payload);
    const duration = autoDrainDuration;
    const completeLine = checkout.replica
      ? "The owner decides completion and review for work this replica claims."
      : autoDrainComplete
        ? `WARNING: ${AUTO_DRAIN_COMPLETE_WARNING}`
      : "Shipped tasks stay in review; a separate action completes them.";
    const approveProposed = autoDrainApproveProposed;
    const autoFix = autoDrainAutoFix;
    const review = autoDrainReview;
    const mode = payload.controls_authorized === true && payload.replica !== true ? autoDrainMode : "";
    const confirmText = [
      checkout.replica
        ? `Start pulling from ${checkout.ownerHost} in workspace "${workspace?.name || workspace?.id}"?`
        : `Start a bounded auto-delivery window in workspace "${workspace?.name || workspace?.id}"?`,
      `Duration: ${duration} · Concurrency: ${autoDrainConcurrency || "runtime default"}`,
      `Auto-concurrency: ${mode || "inherit host at submission"}`,
      `Review before landing: ${checkout.replica ? "owner policy" : review ? `add ${review} review` : "workspace policy"}`,
      `Currently eligible: ${counts.eligible} · waiting: ${counts.waiting}`,
      "",
      completeLine,
      ...(autoFix ? [`Auto-fix PR blockers: ${AUTO_DRAIN_AUTO_FIX_HELP}`] : []),
      ...(approveProposed ? [`${AUTO_DRAIN_APPROVE_WARNING} ${AUTO_DRAIN_APPROVE_RULE}`] : []),
    ].join("\n");
    if (!window.confirm(confirmText)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", `Starting a ${duration} auto-delivery window…`);
    renderAutoDrain(payload);
    try {
      const body = checkout.replica
        ? { for_duration: duration, pull: checkout.workspaceId || checkout.pullSelector, host: checkout.ownerHost }
        : { for_duration: duration, complete: autoDrainComplete, approve_proposed: approveProposed, auto_fix: autoFix };
      if (!checkout.replica && mode) body.auto_concurrency = mode;
      if (!checkout.replica && review) body.review = review;
      if (autoDrainConcurrency) body.concurrency = Number(autoDrainConcurrency);
      const result = await postJson(visit.path("/api/workflows/auto"), body);
      if (!visit.isCurrent()) return;
      const runId = result?.run_id ?? null;
      const state = result?.state ?? "submitted";
      const completion = result?.completion ?? "review";
      const receipt = checkout.replica
        ? `Run ${runId ?? "(no run id)"} ${state}: pulling from ${result?.selector || checkout.pullSelector || checkout.ownerHost}.`
        : `Run ${runId ?? "(no run id)"} ${state} (completion: ${completion}).`;
      feedback("auto-drain-operation-feedback", "success", [`${receipt}${result?.approve_proposed === true ? " Approving qualifying proposed tasks." : ""}`, ...(result?.auto_fix === true ? ["Auto-fix PR blockers enabled."] : []), ...(result?.auto_concurrency?.mode ? [`Auto-concurrency: ${result.auto_concurrency.mode} (${result.auto_concurrency.source}).`] : []), ...(result?.review?.timing ? [`Review before landing: ${reviewLayerText(result.review)}.`] : []), ...submissionNoticeLines(result)].join("\n"));
      await refreshDrainAfterAction();
    } catch (error) {
      if (visit.isCurrent()) feedback("auto-drain-operation-feedback", "error", error.status
        ? `Auto-delivery window failed to start: ${error.message}`
        : `Submission outcome unknown: ${error.message}. Check the window and run history before starting again; this request was not retried.`);
    } finally {
      pendingOperations.delete(key);
      if (operationsState.lastAutoDrain) renderAutoDrain(operationsState.lastAutoDrain);
    }
  });
  return button;
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
      if (operationsState.lastAutoDrain) renderAutoDrain(operationsState.lastAutoDrain);
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

// A throttled resource's reading in the unit the rail block uses: CPU per the
// host's CPU source (load per core, "load 1.6× cores", or utilization, "cpu
// 92%"), memory and disk as percentages. A threshold is bare (no leading
// label) and keeps two decimals so 0.75× does not round to 0.8×.
function throttleReading(item, percent, threshold = false) {
  if (item.resource === "cpu") {
    const value = cpuReadingValue(percent, item.cpu_source, threshold);
    return threshold ? value : `${throttleName(item)} ${value}`;
  }
  return threshold ? `${percent}%` : `${item.resource} ${Math.round(Number(percent))}%`;
}

// The resources a throttle holds, or an empty list. Each carries the host's
// CPU source, which names and scales a CPU reading.
function throttledResources(capacity) {
  const resources = capacity?.resource_throttle?.resources;
  return Array.isArray(resources)
    ? resources.filter(item => item && item.resource).map(item => ({ ...item, cpu_source: capacity.resource_cpu_source }))
    : [];
}

// A throttled resource by the name the rail block gives it.
function throttleName(item) {
  return item.resource === "cpu" ? cpuReadingLabel(item.cpu_source) : String(item.resource);
}

function throttleNames(resources) {
  const names = [...new Set(resources.map(throttleName))];
  return names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}` : names[0] || "";
}

// The current readings, stop thresholds and resume thresholds of a throttle.
function throttleSentence(resources) {
  const held = resources
    .map(item => `${throttleReading(item, item.percent)} (throttled at ≥ ${throttleReading(item, item.high_percent, true)} since ${time(item.since)}; resumes below ${throttleReading(item, item.resume_percent, true)})`)
    .join("; ");
  return `Admissions throttled: ${held}. Running tasks are not touched.`;
}

function autoDrainThrottleLine(capacity) {
  const resources = throttledResources(capacity);
  if (resources.length === 0) return null;
  const note = el("p", { class: "operation-control-note drain-throttle-line", text: throttleSentence(resources) });
  note.setAttribute("role", "status");
  return note;
}

// How many waiting tasks name each task as their lock holder, from the last
// readiness snapshot of the selected workspace.
export function drainHolderCounts() {
  const counts = new Map();
  const tasks = Array.isArray(operationsState.lastAutoDrain?.tasks) ? operationsState.lastAutoDrain.tasks : [];
  for (const entry of tasks) {
    if (!entry || entry.eligible === true) continue;
    for (const holder of drainReadinessView(entry).badge?.holders || []) counts.set(holder, (counts.get(holder) || 0) + 1);
  }
  return counts;
}

// A dismissed provider-limit note stays hidden in this browser until its set
// of gated windows changes: another provider, account, scope or window gates,
// a reset time moves, or the skipped crews change. Only the most recent few
// are kept, so the list cannot grow without bound.
const DISMISSED_PROVIDER_LIMITS_KEY = "orbit.dashboard.dismissedProviderLimits";
const DISMISSED_PROVIDER_LIMITS_MAX = 20;

function providerLimitNoteKey(gated) {
  return JSON.stringify(gated.map(reading => [
    reading.provider, reading.account || "", reading.scope || "", reading.window || "", reading.until || "",
    Array.isArray(reading.crews) ? [...reading.crews].sort() : [],
  ]).sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b))));
}

function dismissedProviderLimits() {
  try {
    const stored = JSON.parse(window.localStorage.getItem(DISMISSED_PROVIDER_LIMITS_KEY) || "[]");
    return Array.isArray(stored) ? stored.filter(key => typeof key === "string") : [];
  } catch {
    return [];
  }
}

function dismissProviderLimits(key) {
  const kept = dismissedProviderLimits().filter(existing => existing !== key);
  kept.push(key);
  try {
    window.localStorage.setItem(DISMISSED_PROVIDER_LIMITS_KEY, JSON.stringify(kept.slice(-DISMISSED_PROVIDER_LIMITS_MAX)));
  } catch {
    /* localStorage unavailable: the note stays dismissed until the next repaint */
  }
}

// [ORB-14698] Each provider usage window that keeps crews out of admission,
// with its use, threshold, reset and the crews skipped until then. Readings
// below their threshold do not hold anything, so they are not shown here.
// The note can be dismissed; see dismissedProviderLimits.
function autoDrainProviderLimitNote(readings) {
  const gated = (Array.isArray(readings) ? readings : []).filter(reading => reading?.gated === true);
  if (gated.length === 0) return null;
  const key = providerLimitNoteKey(gated);
  if (dismissedProviderLimits().includes(key)) return null;
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
  const note = el("p", { class: "operation-control-note drain-provider-limit-note" }, [
    el("span", { class: "drain-provider-limit-text", text: `Provider limits: ${lines.join("; ")}.` }),
  ]);
  note.setAttribute("role", "status");
  const dismiss = el("button", {
    class: "drain-note-dismiss", type: "button", title: "Dismiss until these limits change",
    "aria-label": "Dismiss provider limits until they change",
  });
  dismiss.addEventListener("click", () => {
    dismissProviderLimits(key);
    note.remove();
  });
  note.append(dismiss);
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
function renderAutoDrain(payload) {
  operationsState.lastAutoDrain = payload;
  setDrainReadiness(payload);
  const body = $("auto-drain-body");
  if (!body) return;
  // A poll or a control click rebuilds both cards; keep keyboard focus on the
  // control that had it.
  const focusKey = document.activeElement?.dataset?.drainFocus;
  body.textContent = "";
  renderAutoDrainHead(payload);
  const reasons = autoDrainReasons(payload);
  const holds = [
    autoDrainThrottleLine(payload.capacity),
    autoDrainProviderLimitNote(payload.provider_limits),
    autoDrainProviderAuth(payload.provider_auth_failures),
    drainCrewNote(payload),
    drainPilotNote(payload),
  ].filter(Boolean);
  if (reasons.submit) {
    body.append(...[autoDrainForecast(payload), ...holds, autoDrainQueued(payload)].filter(Boolean), el("div", { class: "operations-readonly-note", text: reasons.submit }));
    renderAutoDrainStart(payload, reasons);
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
  renderAutoDrainStart(payload, reasons);
  if (focusKey) document.querySelector?.(`[data-drain-focus="${focusKey}"]`)?.focus();
}

function clearStartCard(id) {
  const body = $(`${id}-body`);
  if (body) body.textContent = "";
  const card = $(id);
  if (card) card.hidden = true;
}

registerOperationsReset(() => {
  announcedDrainState = null;
  updateDrainIndicators("idle", "idle");
  clearStartCard("auto-drain-quick-start");
});

// The Tasks dock's Start card: window length, parallel tasks and admission
// control, then what happens after a task ships (completion, proposed tasks,
// auto-fix, review). Each choice explains itself in an info hint beside its
// label; a refusal is visible text under the control.
// A sunk footer sums the window up, warns when a throttle will hold its
// admissions, and holds Start.
function renderAutoDrainStart(payload, reasons) {
  const card = $("auto-drain-quick-start");
  const body = $("auto-drain-quick-start-body");
  if (!card || !body) return;
  body.textContent = "";
  card.hidden = Boolean(reasons.submit);
  if (card.hidden) return;
  const counts = autoDrainCounts(payload);
  const sub = $("auto-drain-quick-start-sub");
  if (sub) sub.textContent = `${counts.eligible} eligible now · ${counts.waiting} waiting`;
  // The concurrency field and the Start button are built apart but depend on
  // each other; they meet here.
  const form = {};
  const durationLabel = el("span", { class: "drain-field-label", text: "Window length" });
  durationLabel.id = "auto-drain-duration-label";
  body.append(
    el("div", { class: "drain-field drain-field-duration" }, [
      drainFieldHead(durationLabel, "Admissions stop when the window closes; tasks already started run to the end."),
      autoDrainDurationControl(payload),
    ]),
    autoDrainConcurrencyControl(payload, form),
    form.problem,
    autoDrainModeControl(payload),
    el("div", { class: "drain-divider", role: "separator", "aria-label": "After a task ships" }, [el("span", { text: "After a task ships" })]),
    autoDrainCompletionControl(payload, reasons),
    autoDrainApproveControl(payload, reasons),
    autoDrainAutoFixControl(payload, reasons),
    autoDrainReviewControl(payload, reasons),
    autoDrainStartFoot(payload, form),
  );
  revealDockStart();
}

// "1h window · up to 5 in parallel · enforce", plus each opt-in that is on.
function autoDrainStartSummary(payload) {
  const capacity = payload.capacity || {};
  const limit = autoDrainNumber(capacity.max_active_leaf_runs);
  const parallel = autoDrainConcurrencyValid() && autoDrainConcurrency
    ? autoDrainConcurrency
    : Number.isFinite(limit) && limit >= 1 ? String(Math.trunc(limit)) : "";
  const mode = (payload.controls_authorized === true && payload.replica !== true && autoDrainMode)
    || payload.auto_concurrency?.configured_mode || "";
  return [
    `${autoDrainDuration} window`,
    parallel ? `up to ${parallel} in parallel` : "runtime default parallelism",
    mode ? (mode === "off" ? "no forecast" : mode) : "",
    autoDrainComplete ? "marks done" : "",
    autoDrainApproveProposed ? "approves proposals" : "",
    autoDrainAutoFix ? "auto-fix on" : "",
    autoDrainReview ? `adds ${autoDrainReview} review` : "",
  ].filter(Boolean).join(" · ");
}

function autoDrainStartFoot(payload, form) {
  const summary = el("p", { class: "drain-start-summary", text: autoDrainStartSummary(payload) });
  const previous = form.syncConcurrency;
  form.syncConcurrency = () => {
    previous?.();
    summary.textContent = autoDrainStartSummary(payload);
  };
  const resources = throttledResources(payload.capacity);
  const held = resources.length
    ? el("p", { class: "drain-start-held" }, [
      el("span", { class: "drain-banner-icon small", "aria-hidden": "true" }),
      el("span", { text: `Starts now, but nothing is admitted until the ${throttleNames(resources)} throttle clears.` }),
    ])
    : null;
  return el("div", { class: "drain-start-foot" }, [summary, held, drainFieldRefusal(autoDrainStartRefusal(payload)), autoDrainStartButton(payload, form)]);
}

// A panel poll paints the snapshot it already has. Opening the fleet card
// refetches only once that snapshot is older than the fleet cadence. The
// indicator timer, not this poll, is what asks the hosts again.
configureOperationsFleetCallbacks({ updateDrainIndicators, clearStartCard });
configureOperationsRefresh(fetchAndRenderOperations);

function fetchAndRenderAutoDrain() {
  const workspace = selectedWorkspace();
  if (!workspace) {
    const token = ++operationsState.aggregateFleetToken;
    const revision = getWorkspaceRevision();
    const guard = () => token === operationsState.aggregateFleetToken && revision === getWorkspaceRevision() && !selectedWorkspace();
    return requestPanel("auto-drain-body", "fleet", () => loadFleetSnapshot({ refreshWhenStale: true, onPartial: payload => { if (guard()) paintAggregateFleet(payload); } }), payload => {
      paintAggregateFleet(payload);
    }, "auto-drain-phase-label");
  }
  const concurrency = autoDrainConcurrencyValid() ? autoDrainConcurrency : "";
  const query = concurrency ? `?concurrency=${encodeURIComponent(concurrency)}` : "";
  const scope = "/api/workflows/auto/readiness";
  const path = `${scope}${query}`;
  const visit = captureWorkspaceVisit();
  const readiness = requestPanel("auto-drain-body", scope, () => fetchJson(path), payload => {
    renderAutoDrain(payload);
  }, "auto-drain-phase-label");
  const fleet = loadFleetSnapshot({ refreshWhenStale: false, onPartial: payload => { if (visit.isCurrent()) { renderFleetIndicator(payload); } } }).then(payload => {
    if (visit.isCurrent()) {
      renderFleetIndicator(payload);
    }
    return payload;
  });
  // The optional summary paints itself and contains its failures. A slow
  // forecast must not delay dashboard refresh or release of action controls.
  void refreshBacklogForecast();
  return Promise.all([readiness, fleet]).then(() => {});
}

// The chrome always names the fleet, including when an idle default workspace
// is selected. The status card describes the selected workspace.
function throwFirstPanelError(results) {
  const errors = results.filter(result => result.status === "rejected").map(result => result.reason);
  // Preserve transport classification even if a different panel also fails.
  if (errors.length) throw errors.find(error => error.networkFailure) || errors[0];
}

export async function fetchAndRenderOperations(subtab = operationsState.context.getOperationsSubtab()) {
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
