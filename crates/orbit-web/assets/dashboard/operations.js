// Routine-definition, host clock, and auto-task operations [ORB-10875, ORB-10876].

import { requestPanel, detailsPanel, el, fetchJson, getWorkspace, getWorkspaceRevision, onWorkspaceChange, postJson, statusPill } from './common.js';
import { navigateToRun } from './router.js';
import { renderAutomation } from './automation.js';

const $ = (id) => document.getElementById(id);
const pendingOperations = new Set();
const UNCONDITIONAL_MINT_WARNING = "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.";
const AUTO_DRAIN_DURATIONS = ["15m", "30m", "1h", "2h", "4h", "8h"];
const AUTO_DRAIN_COMPLETE_WARNING = "Also marks every task this window ships as done (review -> done), not only the ones eligible right now.";
let lastOperations = null;
let lastAutoTasks = null;
let lastAutoDrain = null;
let autoDrainDuration = "1h";
let autoDrainConcurrency = "";
let autoDrainComplete = false;
let lastAutoDrainRun = null;
// Whether the dependency-waiting rows are expanded; held outside the render so
// a background refresh or a duration click does not collapse them.
let autoDrainDependencyRowsOpen = false;
let lastOperationMode = null;
let context = null;
let unsubscribeWorkspace = null;
// The operator's unapplied cadence choice, held outside the rebuilt <select>
// so a background refresh cannot revert it. Host-scoped, like the clock itself.
let pendingCadenceSeconds = null;

export function initOperations(nextContext) {
  context = nextContext;
  unsubscribeWorkspace?.();
  unsubscribeWorkspace = onWorkspaceChange(() => {
    lastOperations = lastAutoTasks = lastAutoDrain = lastOperationMode = null;
    for (const id of ["routine-operation-feedback", "clock-operation-feedback", "auto-task-operation-feedback", "auto-drain-operation-feedback", "operation-mode-operation-feedback"]) feedback(id, "", "");
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
  const node = $(id);
  if (!node) return;
  node.className = `operation-feedback ${kind || ""}`;
  node.textContent = message || "";
}

function timezoneName(date) {
  return new Intl.DateTimeFormat("en-US", { timeZoneName: "short" })
    .formatToParts(date)
    .find((part) => part.type === "timeZoneName")?.value || "UTC";
}

function looksLikeDuration(value) {
  const text = String(value).trim();
  return /\d+\s*(?:h|hr|hrs|hour|hours|min|mins|minute|minutes|s|sec|secs|seconds)\b/i.test(text)
    && Number.isNaN(new Date(text).getTime());
}

function time(value) {
  if (!value) return "Not observed";
  if (looksLikeDuration(value)) return `Duration ${value}`;
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return String(value);
  const formatted = String(context.formatAbsoluteTime(value));
  const tz = timezoneName(parsed);
  if (formatted.includes(tz) || /\bUTC\b/.test(formatted) || /[+-]\d{2}:\d{2}$/.test(formatted)) {
    return formatted;
  }
  return `${formatted} ${tz}`;
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

function nextEvaluationText(projection, fallbackAt) {
  const state = projection?.state;
  const at = projection?.at || (state === "disabled" || state === "paused" ? fallbackAt : null);
  const when = at ? time(at) : null;
  if (state === "disabled") return when ? `Disabled · hypothetical next ${when}` : "Disabled";
  if (state === "paused") return when ? `Paused · hypothetical next ${when}` : "Paused";
  if (state === "waiting") return "Waiting for deliveries";
  if (state === "never_observed") return "Never observed";
  if (state === "unavailable") return "Unavailable";
  if (state === "scheduled") return when || "Scheduled";
  return when || "Unavailable";
}

function lastMintedText(definition) {
  if (!definition.last_minted_task_id) return "None";
  const status = definition.last_minted_task_status ? ` · ${definition.last_minted_task_status}` : "";
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
  if (!clock.enabled) return "Paused";
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

function routineScheduleText(routine) {
  if (routine.trigger?.deliveries_landed) {
    return `${routine.trigger.deliveries_landed.threshold} verified deliveries on ${routine.trigger.deliveries_landed.branch}`;
  }
  return routine.cron || "—";
}

function operationIdentity(name, state) {
  return el("div", { class: "operation-identity" }, [
    el("strong", { text: name }),
    el("span", { class: `operation-state ${state}`, text: state }),
  ]);
}

function operationFact(label, value) {
  return el("div", { class: "operation-fact" }, [
    el("span", { class: "operation-fact-label", text: label }),
    el("span", { class: "operation-fact-value", text: value == null || value === "" ? "—" : String(value) }),
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
  wrapper.setAttribute("aria-label", `${button.textContent}: ${reason}`);
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
      const link = el("a", { text: ` Open ${result.task_id} →` });
      link.href = `?workspace=${encodeURIComponent(selection.workspace)}#tasks?status=all&q=${encodeURIComponent(result.task_id)}`;
      $(feedbackId)?.appendChild(link);
    }
    try {
      await refresh();
    } catch (error) {
      if (selection.current()) {
        $(feedbackId)?.appendChild(el("span", {
          text: ` Refresh failed: ${error.message}. The action succeeded; refresh before submitting another action.`,
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
  if (ms == null || ms === "") return null;
  const n = Number(ms);
  if (!Number.isFinite(n) || n < 0) return null;
  const seconds = Math.round(n / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${String(seconds % 60).padStart(2, "0")}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
}

function clockHm(value) {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return "";
  return parsed.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

const CRON_DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const TIMELINE_ROWS = 4;

// A readable cadence for the common cron shapes routines use; anything else
// is shown verbatim so nothing is misdescribed.
function cronText(cron) {
  const parts = String(cron || "").trim().split(/\s+/);
  if (parts.length !== 5) return cron || "—";
  const [minute, hour, dom, month, dow] = parts;
  const pad = (v) => String(v).padStart(2, "0");
  if (dom !== "*" || month !== "*") return cron;
  if (minute === "*" && hour === "*" && dow === "*") return "every minute";
  const every = /^\*\/(\d+)$/.exec(minute);
  if (every && hour === "*" && dow === "*") return `every ${every[1]} min`;
  if (/^\d+$/.test(minute) && hour === "*" && dow === "*") return `hourly at :${pad(minute)}`;
  if (/^\d+$/.test(minute) && /^\d+$/.test(hour) && dow === "*") return `daily ${pad(hour)}:${pad(minute)} UTC`;
  if (/^\d+$/.test(minute) && /^\d+$/.test(hour) && /^\d$/.test(dow)) {
    return `weekly ${CRON_DAYS[Number(dow)] || dow} ${pad(hour)}:${pad(minute)} UTC`;
  }
  return cron;
}

// One switch for enable/disable. The visible text stays "Enable"/"Disable"
// (or "Pending…") so the control keeps its accessible name and the readback
// contract; CSS draws it as a switch.
function operationSwitch({ on, text, pending, reason }) {
  const button = el("button", {
    class: `operation-switch ${on ? "on" : "off"}${pending ? " pending" : ""}`,
    text,
    title: reason || text,
  });
  button.type = "button";
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
  const row = el("div", { class: "operation-columns" }, labels.map((label) => el("span", { text: label })));
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
    : state === "failed" || state === "error" ? "failed"
    : state === "running" || state === "pending" ? "running"
    : "idle";
  return el("span", { class: `operation-dot ${tone}`, title: state || "never" });
}

function runLink(runId, workspaceId) {
  const link = el("a", { class: "mono operation-run-link", text: runId, title: `Open run ${runId}` });
  link.href = `?workspace=${encodeURIComponent(workspaceId || "")}#runs/${encodeURIComponent(runId)}`;
  link.addEventListener("click", (event) => {
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

// ---------------------------------------------------------------------------
// Routines

function routineButton(payload, routine) {
  const selection = selectionSnapshot();
  const nextEnabled = !routine.enabled;
  const key = `routine:${selection.workspace}:${routine.name}`;
  const reason = controlReason(payload);
  const pending = pendingOperations.has(key);
  const button = operationSwitch({
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
  const link = el("a", { class: "mono", text: jobId, title: `Open ${jobId} under Jobs` });
  link.href = `#operations/jobs?job=${encodeURIComponent(jobId)}`;
  return link;
}

// The next hour as a strip: where each routine's next slot lands, with paused
// routines drawn hollow so an operator sees skipped slots, not only live ones.
function routineTimeline(routines, now = Date.now()) {
  const horizon = 60 * 60_000;
  const due = routines
    .map((routine) => ({ routine, at: new Date(routineNextAt(routine) || NaN).getTime() }))
    .filter(({ at }) => Number.isFinite(at) && at >= now - 60_000 && at <= now + horizon)
    .sort((a, b) => a.at - b.at);
  const strip = el("section", { class: "operation-timeline" });
  strip.setAttribute("aria-label", "Next hour");
  const fires = due.filter(({ routine }) => routine.enabled && routine.effective !== false).length;
  const active = routines.filter((routine) => routineState(routine) === "active").length;
  strip.appendChild(el("div", { class: "operation-timeline-head" }, [
    el("strong", { text: "Next hour" }),
    el("span", { class: "operation-timeline-range mono", text: `${clockHm(now)} → ${clockHm(now + horizon)}` }),
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
  const rowEnds = [];
  let rows = 0;
  due.forEach(({ routine, at }) => {
    const pct = Math.min(100, Math.max(0, ((at - now) / horizon) * 100));
    const halfWidth = routine.name.length * 0.42;
    let row = rowEnds.findIndex((end) => end <= pct - halfWidth);
    if (row < 0) row = Math.min(rowEnds.length, TIMELINE_ROWS - 1);
    rowEnds[row] = pct + halfWidth;
    rows = Math.max(rows, row + 1);
    const fires = routine.enabled && routine.effective !== false;
    const tick = el("div", { class: `operation-timeline-tick ${fires ? "fires" : "skipped"} row-${row}` }, [
      el("span", { class: "operation-timeline-mark" }),
      el("span", { class: "operation-timeline-label mono", text: routine.name }),
    ]);
    tick.style.left = `${pct}%`;
    tick.title = `${routine.name} · ${clockHm(at)}${fires ? "" : " · paused"}`;
    track.appendChild(tick);
  });
  track.className = `operation-timeline-track rows-${Math.max(rows, 1)}`;
  strip.appendChild(track);
  return strip;
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
  const cadence = routine.trigger?.deliveries_landed
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
  card.append(
    el("div", { class: "operation-row-head" }, [
      operationCell("", [routineButton(payload, routine)], "operation-cell-control"),
      operationCell("Routine", [
        identity,
        el("span", { class: "operation-cell-sub" }, [el("span", { text: "runs " }), jobLink(routine.target)]),
      ], "operation-cell-identity"),
      operationCell("Cadence", cadence),
      routine.enabled
        ? whenCell("Next fire", nextAt, { fallback: nextEvaluationText(routine.next_evaluation, routine.next_due) })
        : operationCell("Next fire", [el("span", { class: "operation-cell-main muted", text: nextAt ? `would be ${clockHm(nextAt)}` : "Paused" })]),
      operationCell("Last run", lastRun),
    ]),
    operationDetails(detailsKey, [
      el("div", { class: "operation-target mono", text: routine.target }),
      el("div", { class: "operation-grid" }, [
        field("Source workspace", routine.source),
        field("Schedule", routineScheduleText(routine)),
        field("Last evaluation", time(routine.last_evaluated_slot || routine.first_observed_at)),
        field("Next evaluation", nextEvaluationText(routine.next_evaluation, routine.next_due)),
        field("Last fire", fire ? time(fire.finished_at || fire.started_at) : "Never"),
        field("Linked run / outcome", fire ? `${fire.run_id || "No run"} · ${fire.state}` : "No fire recorded"),
      ]),
      renderAutomation(routine.automation, `${detailsKey}:automation`),
      routine.description ? el("p", { class: "operation-description", text: routine.description }) : null,
    ]),
  );
  return card;
}

function renderOperations(payload) {
  lastOperations = payload;
  if ($("operations-session")) $("operations-session").textContent = payload.session_explanation || "Operations actions require the capabilities granted to this dashboard server. Refresh to load session access details.";
  const workspace = selectedWorkspaceName();
  const workspaceId = selectedWorkspace()?.id || null;
  const routines = workspace
    ? (payload.routines || []).filter((routine) => routine.source === workspace)
    : [];
  const body = $("routines-body");
  body.textContent = "";
  if (!workspace) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: `All-workspace mode is read-only. Select one workspace; this machine is already resolved as ${payload.machine_name}.` }));
  }
  if (routines.length === 0) {
    body.appendChild(el("div", { class: "empty-state", text: workspace ? "No routines are defined by this workspace." : "Select a workspace to list its routines." }));
  } else {
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
      const group = operationGroup("paused", "Paused", paused.length, "enabled: false in the versioned definition · slots are skipped, not queued");
      group.appendChild(operationColumns(columns));
      for (const routine of paused) group.appendChild(routineRow(payload, routine, workspaceId));
      body.appendChild(group);
    }
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
  const serviceLabel = unavailable ? "service unknown" : (clock.enabled ? "service enabled" : "service paused");
  const cadenceLabel = unavailable ? "Unknown" : cadenceText(clock.configured_cadence_seconds);
  const effectiveCadenceLabel = unavailable ? "Unknown" : cadenceText(clock.effective_cadence_seconds);
  const actions = el("div", { class: "operation-clock-actions" });
  actions.appendChild(clockButton(
    payload,
    unavailable ? "enable" : (clock.enabled ? "disable" : "enable"),
    unavailable ? "Clock unavailable" : (clock.enabled ? "Pause clock" : "Enable clock"),
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
      success: (result) => `${result.message}; service is ${result.clock.enabled ? "enabled" : "paused"}.`,
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
        operationFact("Last tick", clock.last_tick_at ? (relativeTime(clock.last_tick_at) || time(clock.last_tick_at)) : unavailable ? "Unknown" : "Not exposed"),
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

function autoTaskToggleButton(payload, definition) {
  const selection = selectionSnapshot();
  const nextEnabled = !definition.enabled;
  const key = `auto-task-toggle:${selection.workspace}:${definition.name}`;
  const reason = actionReason(payload, "auto_task_toggle");
  const verb = nextEnabled ? "Enable" : "Disable";
  const pending = pendingOperations.has(key);
  const button = operationSwitch({
    on: Boolean(definition.enabled),
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
  const link = el("a", { class: "mono", text: taskId, title: `Open ${taskId}` });
  link.href = `?workspace=${encodeURIComponent(workspaceId || "")}#tasks?status=all&q=${encodeURIComponent(taskId)}`;
  return link;
}

function autoTaskRow(payload, definition, workspaceId) {
  const state = definition.enabled ? "enabled" : "disabled";
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
        : [mintSource, relativeTime(definition.last_evaluation?.last_fired_at)].filter(Boolean).join(" · ") }),
    ]
    : [el("span", { class: "operation-cell-main muted", text: lastEvaluationText(definition) })];
  const next = definition.next_evaluation;
  const nextCell = next?.state === "scheduled" && next.at
    ? whenCell("Next mint", next.at)
    : operationCell("Next mint", [el("span", { class: `operation-cell-main${definition.enabled ? "" : " muted"}`, text: nextEvaluationText(next) })]);
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
      renderAutomation(definition.automation, `${detailsKey}:automation`),
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
  if (!scheduler) {
    note.appendChild(el("span", { text: "Definitions live in .orbit/auto_tasks/ and are evaluated on the host sweep clock. Toggle writes the definition's enabled field; Mint now bypasses schedule, enabled and dedupe." }));
    return;
  }
  const link = el("a", { text: scheduler.name });
  link.href = "#operations/routines";
  if (!scheduler.enabled) {
    note.className += " warn";
    note.appendChild(el("span", { text: "The " }));
    note.append(link, el("span", { text: " routine is paused, so scheduled definitions will not mint until it runs. Toggle writes the definition's enabled field; Mint now bypasses schedule, enabled and dedupe." }));
    return;
  }
  note.appendChild(el("span", { text: "Minted by " }));
  note.append(link, el("span", { text: ` (${cronText(scheduler.cron)}). Toggle writes the definition's enabled field; Mint now bypasses schedule, enabled and dedupe.` }));
}

function renderAutoTasks(payload) {
  lastAutoTasks = payload;
  const body = $("auto-tasks-body");
  if (!body) return;
  body.textContent = "";
  const workspaceReason = workspaceReadOnlyReason();
  const reason = workspaceReason || payload.read_only_reason || "";
  const workspace = selectedWorkspace();
  const definitions = workspace && !workspaceReason ? (payload.definitions || []) : [];
  if (reason) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: reason }));
  }
  if (definitions.length === 0) {
    body.appendChild(el("div", {
      class: "empty-state",
      text: workspace && !workspaceReason ? "No auto-task definitions are defined by this workspace." : "Select a workspace to list its auto-task definitions.",
    }));
  } else {
    const enabled = definitions.filter((definition) => definition.enabled);
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
      ["paused", "Disabled", "kept in the repo, never evaluated", definitions.filter((definition) => !definition.enabled)],
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
  syncAutoTaskSchedulerNote();
  const count = $("auto-tasks-count");
  const enabledCount = definitions.filter((definition) => definition.enabled).length;
  if (count) count.textContent = workspace && !workspaceReason ? `${enabledCount} enabled of ${definitions.length} · ${workspace.name}` : "read-only";
  setRailSubtabCount("rail-count-ops-auto-tasks", workspace && !workspaceReason && definitions.length ? `${enabledCount}/${definitions.length}` : "");
}

// ---------------------------------------------------------------------------
// Jobs. The catalogue is projected from what the dashboard already serves:
// every `job:` target a routine names plus every job id in this workspace's
// recent runs. There is no job endpoint yet, so Run is offered but not wired;
// the row hands the operator the exact CLI command instead.

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

function jobCommand(jobId, workspace) {
  return `orbit run job ${jobId} --workspace ${workspace?.name || workspace?.id || "<workspace>"}`;
}

function jobRow(job, workspace) {
  const detailsKey = `job:${job.id}`;
  const last = job.lastRun;
  const command = jobCommand(job.id, workspace);
  const runReason = `Running a job from the dashboard is not wired yet. From a terminal: ${command}`;
  const run = el("button", { class: "operation-button primary job-run", text: "Run ▸", title: runReason });
  run.type = "button";
  run.disabled = true;
  const scheduledBy = job.routines.length
    ? job.routines.map((routine) => {
      const chip = el("a", { class: `operation-chip ${routine.enabled ? "" : "paused"}`.trim(), text: `${routine.name} · ${routine.trigger?.deliveries_landed ? "on delivery" : cronText(routine.cron)}${routine.enabled ? "" : " · paused"}` });
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
    try {
      await navigator.clipboard?.writeText(command);
      copy.textContent = "Copied";
      setTimeout(() => { copy.textContent = "Copy command"; }, 1500);
    } catch (_) {
      copy.textContent = "Copy failed";
    }
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
      el("p", { class: "operation-control-note", text: "Every step receives --input key=value pairs; the dashboard button will submit the same run once the job endpoint lands." }),
      recent.length
        ? el("ul", { class: "operation-recent-runs" }, recent.map((run) => el("li", {}, [
          outcomeDot(run.state),
          runLink(run.run_id, workspace?.id),
          el("span", { class: "muted", text: `${run.state} · ${relativeTime(run.finished_at || run.started_at || run.created_at) || ""}${durationText(run.duration_ms) ? ` · ${durationText(run.duration_ms)}` : ""}` }),
        ])))
        : null,
    ]),
  );
  return card;
}

function renderJobs(payload) {
  lastJobs = payload;
  const body = $("jobs-body");
  if (!body) return;
  body.textContent = "";
  const workspace = selectedWorkspace();
  const workspaceReason = workspaceReadOnlyReason();
  if (workspaceReason || !workspace) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: workspaceReason || "Select a workspace to list its jobs." }));
    body.appendChild(el("div", { class: "empty-state", text: "Select a workspace to list its jobs." }));
    $("jobs-count").textContent = "read-only";
    setRailSubtabCount("rail-count-ops-jobs", "");
    return;
  }
  const runs = Array.isArray(payload.runs?.items) ? payload.runs.items : Array.isArray(payload.runs) ? payload.runs : [];
  const catalog = jobCatalog(lastOperations, runs, workspace.name);
  const running = runs.filter((run) => run.state === "running" || run.state === "pending");
  const strip = el("section", { class: "operation-running" });
  strip.setAttribute("aria-label", "Running now");
  strip.appendChild(el("div", { class: "operation-running-title" }, [
    el("strong", { text: "Running now" }),
    el("span", { class: "mono", text: String(running.length) }),
    el("span", { class: "muted", text: running.length ? "runs with a job step in flight" : "nothing in flight" }),
  ]));
  if (running.length) {
    strip.appendChild(el("div", { class: "operation-running-grid" }, running.map((run) => jobRunningCard(run, workspace.id))));
  }
  body.appendChild(strip);
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
    body.appendChild(el("p", { class: "operation-control-note", text: `Catalogue projected from routine targets and the last ${JOB_RUN_LIMIT} runs; the same list as orbit job list once the job endpoint lands.` }));
  }
  $("jobs-count").textContent = `${catalog.size} job${catalog.size === 1 ? "" : "s"} · ${running.length} running · ${workspace.name}`;
  setRailSubtabCount("rail-count-ops-jobs", running.length ? `${running.length} running` : "");
}

function fetchAndRenderJobs() {
  if (!selectedWorkspace()) {
    return requestPanel("jobs-body", "unselected", () => Promise.resolve({ runs: [] }), renderJobs, "jobs-count");
  }
  return requestPanel("jobs-body", "jobs",
    () => fetchJson(`/api/job-runs?limit=${JOB_RUN_LIMIT}`).then((runs) => ({ runs })),
    renderJobs, "jobs-count");
}

function operationCountId(bodyId) {
  return bodyId === "clock-body" ? "clock-host" : bodyId.replace("-body", "-count");
}

function loadOperationPanel(bodyId, path, render) {
  return requestPanel(bodyId, path, () => fetchJson(path), render, operationCountId(bodyId));
}

function fetchAndRenderAutoTasks() {
  return loadOperationPanel("auto-tasks-body", "/api/auto-tasks", renderAutoTasks);
}

// ORB-11250: bounded backlog auto-drain window ("orbit run auto --for
// <duration> [--complete]" from the dashboard). One workspace-scoped action,
// not a per-row one, so it follows the mint/clock in-flight idiom (a single
// fixed `pendingOperations` key, guard released in `finally`) rather than
// tasks.js's per-task Ship guard.

// [ORB-12728] The live coordinator readiness reports, if any. The server
// nests it under `capacity` (where the slot picture comes from); an older
// payload shape with the fields at the top level is read the same way so a
// mixed-version dashboard never hides a running window.
function autoDrainLiveWindow(payload) {
  const source = payload.capacity && "drain_run_id" in payload.capacity ? payload.capacity : payload;
  const runId = source?.drain_run_id ? String(source.drain_run_id) : "";
  return {
    runId,
    admissionsStopped: source?.admissions_stopped === true,
    stop: source?.admissions_stop && typeof source.admissions_stop === "object" ? source.admissions_stop : null,
  };
}

function autoDrainReasons(payload) {
  const workspaceReason = workspaceReadOnlyReason();
  const live = autoDrainLiveWindow(payload);
  return {
    submit: workspaceReason,
    complete: workspaceReason || (payload.controls_authorized === false
      ? "Automatic completion requires an authorized operator session; the window can still start with default review completion."
      : ""),
    stop: workspaceReason || (!live.runId
      ? "No auto-delivery window is live in this workspace."
      : live.admissionsStopped
        ? `Admissions are already stopped for ${live.runId}${live.stop?.actor ? ` (by ${live.stop.actor})` : ""}; admitted workers keep running.`
        : payload.controls_authorized === false
          ? "Stopping admissions requires an authorized operator session."
          : ""),
  };
}

function autoDrainCounts(payload) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  const eligible = tasks.filter((task) => task.eligible === true).length;
  return { eligible, waiting: tasks.length - eligible };
}

const AUTO_DRAIN_REASON_LABELS = {
  ready: "Ready for admission",
  ready_as_epic: "Ready as the next epic",
  not_backlog: "Not in backlog",
  unmet_dependency: "Waiting on a dependency",
  task_pilot_preparation_required: "Task-pilot preparation required",
  crew_not_allowed: "Crew excluded by this drain",
  epic_managed: "Managed by an epic",
  admissions_stopped: "Admissions stopped",
  epic_run_active: "Another epic run is active",
  queued_behind_epic: "Queued behind another epic",
  context_lock_conflict: "Context is locked",
  group_member_conflict: "A grouped task conflicts",
  claimed_by_live_child: "Claimed by a live child run",
  outside_grant_scope: "Outside the active grant scope",
  grant_expired: "The active grant expired",
  grant_stopped: "The active grant stopped admitting",
  grant_revoked: "The active grant was revoked",
  outside_candidate_pool: "Outside the examined candidate pool",
  conflict_deferred: "Deferred behind a conflicting candidate",
  capacity_saturated: "No admission capacity",
};

function autoDrainTaskLink(taskId, workspace) {
  const link = el("a", { class: "auto-drain-reference mono", text: taskId, title: `Open task ${taskId}` });
  link.href = `?workspace=${encodeURIComponent(workspace.id)}#tasks?status=all&q=${encodeURIComponent(taskId)}`;
  return link;
}

function autoDrainRunLink(runId, workspace) {
  const link = el("a", { class: "auto-drain-reference mono", text: runId, title: `Open run ${runId}` });
  link.href = `?workspace=${encodeURIComponent(workspace.id)}#runs/${encodeURIComponent(runId)}`;
  link.addEventListener("click", (event) => {
    event.preventDefault();
    navigateToRun(runId, workspace.id);
  });
  return link;
}

function autoDrainEvidenceRow(label, values) {
  return el("div", { class: "auto-drain-evidence-row" }, [
    el("span", { class: "auto-drain-evidence-label", text: label }),
    el("div", { class: "auto-drain-evidence-values" }, values),
  ]);
}

function autoDrainTextValues(values) {
  return values
    .filter((value) => value != null && String(value).trim() !== "")
    .map((value) => el("span", { class: "auto-drain-evidence-value mono", text: String(value) }));
}

function autoDrainMissingEvidence(task, reason, evidence) {
  if (reason === "unmet_dependency" && evidence.dependencies === 0) {
    return "Dependency details were not supplied.";
  }
  if (["context_lock_conflict", "group_member_conflict", "conflict_deferred"].includes(reason)
    && evidence.conflicts === 0 && evidence.blockers === 0) {
    return "Conflict details were not supplied.";
  }
  if (reason === "claimed_by_live_child" && evidence.claimingRuns === 0) {
    return "Claiming run details were not supplied.";
  }
  if (reason === "capacity_saturated" && evidence.activeRuns === 0) {
    return "Active run details were not supplied.";
  }
  if (reason === "crew_not_allowed" && task.crew == null && !Array.isArray(task.allowed_crews)) {
    return "Crew restriction details were not supplied.";
  }
  if (["outside_grant_scope", "grant_expired", "grant_stopped", "grant_revoked"].includes(reason) && !task.grant_id) {
    return "Grant details were not supplied.";
  }
  if (!AUTO_DRAIN_REASON_LABELS[reason] && evidence.rows === 0) {
    return "No additional evidence was supplied for this server reason.";
  }
  return "";
}

function autoDrainTaskEvidence(task, workspace) {
  const rows = [];
  if (task.status && task.status !== "backlog") {
    rows.push(autoDrainEvidenceRow("Task status", autoDrainTextValues([task.status])));
  }
  const dependencies = Array.isArray(task.dependencies) ? task.dependencies : [];
  if (dependencies.length > 0) {
    rows.push(autoDrainEvidenceRow("Dependencies", dependencies.map((dependency) => {
      const taskId = typeof dependency === "string" ? dependency : dependency?.task_id;
      const value = el("span", { class: "auto-drain-evidence-value" });
      if (taskId) value.appendChild(autoDrainTaskLink(taskId, workspace));
      else value.appendChild(el("span", { text: "Unknown dependency" }));
      if (dependency?.status) value.appendChild(el("span", { text: ` · ${dependency.status}` }));
      return value;
    })));
  }

  const conflicts = Array.isArray(task.conflicts) ? task.conflicts : [];
  if (conflicts.length > 0) {
    rows.push(autoDrainEvidenceRow("Conflicts", conflicts.map((conflict) => {
      const value = el("span", { class: "auto-drain-evidence-value" });
      value.appendChild(el("span", { class: "mono", text: conflict?.requested_file || "File not supplied" }));
      if (conflict?.locking_task_id) {
        value.append(el("span", { text: " · held by " }), autoDrainTaskLink(conflict.locking_task_id, workspace));
      }
      return value;
    })));
  }

  const blockingTaskIds = Array.isArray(task.blocking_task_ids) ? task.blocking_task_ids : [];
  if (blockingTaskIds.length > 0) {
    rows.push(autoDrainEvidenceRow("Blocking tasks", blockingTaskIds.map((taskId) => autoDrainTaskLink(taskId, workspace))));
  }
  const liveRunIds = Array.isArray(task.run_ids) ? task.run_ids : [];
  if (liveRunIds.length > 0) {
    rows.push(autoDrainEvidenceRow("Claiming runs", liveRunIds.map((runId) => autoDrainRunLink(runId, workspace))));
  }
  const activeRunIds = Array.isArray(task.active_run_ids) ? task.active_run_ids : [];
  if (activeRunIds.length > 0) {
    rows.push(autoDrainEvidenceRow("Active runs", activeRunIds.map((runId) => autoDrainRunLink(runId, workspace))));
  }
  if (task.crew != null || Array.isArray(task.allowed_crews)) {
    const allowed = Array.isArray(task.allowed_crews) && task.allowed_crews.length > 0
      ? task.allowed_crews.join(", ")
      : "none supplied";
    rows.push(autoDrainEvidenceRow("Crew", autoDrainTextValues([`${task.crew ?? "not supplied"} · allowed: ${allowed}`])));
  }
  if (task.grant_id) rows.push(autoDrainEvidenceRow("Grant", autoDrainTextValues([task.grant_id])));
  if (task.epic_run_id) rows.push(autoDrainEvidenceRow("Active epic run", [autoDrainRunLink(task.epic_run_id, workspace)]));
  if (task.next_epic_task_id) rows.push(autoDrainEvidenceRow("Next epic", [autoDrainTaskLink(task.next_epic_task_id, workspace)]));

  const reason = typeof task.reason === "string" && task.reason.trim() ? task.reason : "unknown";
  const evidenceMissing = autoDrainMissingEvidence(task, reason, {
    dependencies: dependencies.length,
    conflicts: conflicts.length,
    blockers: blockingTaskIds.length,
    claimingRuns: liveRunIds.length,
    activeRuns: activeRunIds.length,
    rows: rows.length,
  });
  if (evidenceMissing) rows.push(autoDrainEvidenceRow("Details", autoDrainTextValues([evidenceMissing])));

  return rows;
}

// Readiness groups. The server hands back one flat row per task with a
// reason; the pane sorts those into the three questions an operator asks
// before starting a window — what starts now, what is only waiting on a
// running task, and what is waiting on other backlog — and leaves every
// other reason in a fourth group so no server row is dropped.
const AUTO_DRAIN_LOCK_REASONS = new Set(["context_lock_conflict", "group_member_conflict", "conflict_deferred", "claimed_by_live_child"]);

function autoDrainTaskId(task) {
  return typeof task.task_id === "string" && task.task_id.trim() ? task.task_id : null;
}

function autoDrainReason(task) {
  return typeof task.reason === "string" && task.reason.trim() ? task.reason : "unknown";
}

function autoDrainGroups(tasks) {
  const groups = { eligible: [], locked: [], dependency: [], other: [] };
  for (const task of tasks) {
    const reason = autoDrainReason(task);
    if (task.eligible === true) groups.eligible.push(task);
    else if (AUTO_DRAIN_LOCK_REASONS.has(reason)) groups.locked.push(task);
    else if (reason === "unmet_dependency") groups.dependency.push(task);
    else groups.other.push(task);
  }
  return groups;
}

// Holder ids for a lock-blocked row. Context locks report
// `conflicts[].locking_task_id`; same-wave deferrals report
// `conflicts[].blocking_task_id` plus `blocking_task_ids`; live-child claims
// report only `run_ids`, which have no task holder and fall into "unknown".
function autoDrainHolders(task) {
  const holders = new Set();
  for (const conflict of Array.isArray(task.conflicts) ? task.conflicts : []) {
    const holder = conflict?.locking_task_id || conflict?.blocking_task_id;
    if (holder) holders.add(holder);
  }
  for (const holder of Array.isArray(task.blocking_task_ids) ? task.blocking_task_ids : []) {
    if (holder) holders.add(holder);
  }
  return [...holders].sort();
}

function autoDrainConflictSelectors(task, holder) {
  const selectors = [];
  for (const conflict of Array.isArray(task.conflicts) ? task.conflicts : []) {
    const conflictHolder = conflict?.locking_task_id || conflict?.blocking_task_id;
    if (holder && conflictHolder && conflictHolder !== holder) continue;
    const selector = conflict?.requested_file || conflict?.requested_selector || conflict?.blocking_selector;
    if (selector && !selectors.includes(selector)) selectors.push(selector);
  }
  return selectors;
}

function autoDrainDependencyIds(task) {
  const ids = [];
  for (const dependency of Array.isArray(task.dependencies) ? task.dependencies : []) {
    const taskId = typeof dependency === "string" ? dependency : dependency?.task_id;
    if (taskId && !ids.includes(taskId)) ids.push(taskId);
  }
  return ids;
}

// Depth of every dependency-waiting task within its own group: 0 for a task
// whose dependencies all lie outside the group (the chain roots), otherwise
// one more than its deepest in-group dependency. A cycle, which the server
// should never emit, stops at the visited node rather than recursing.
function autoDrainDependencyDepths(tasks) {
  const byId = new Map(tasks.map((task) => [autoDrainTaskId(task), task]).filter(([id]) => id));
  const depths = new Map();
  const visiting = new Set();
  const depth = (id) => {
    if (depths.has(id)) return depths.get(id);
    if (visiting.has(id)) return 0;
    visiting.add(id);
    let value = 0;
    for (const dependency of autoDrainDependencyIds(byId.get(id))) {
      if (byId.has(dependency)) value = Math.max(value, depth(dependency) + 1);
    }
    visiting.delete(id);
    depths.set(id, value);
    return value;
  };
  for (const id of byId.keys()) depth(id);
  return depths;
}

// Dependencies referenced by the waiting group that are not themselves in it:
// the tasks the whole group is really waiting on.
function autoDrainDependencyRoots(tasks, allTasks) {
  const inGroup = new Set(tasks.map(autoDrainTaskId).filter(Boolean));
  const byId = new Map(allTasks.map((task) => [autoDrainTaskId(task), task]).filter(([id]) => id));
  const roots = new Map();
  for (const task of tasks) {
    for (const dependency of Array.isArray(task.dependencies) ? task.dependencies : []) {
      const taskId = typeof dependency === "string" ? dependency : dependency?.task_id;
      if (!taskId || inGroup.has(taskId) || roots.has(taskId)) continue;
      const row = byId.get(taskId);
      const status = typeof dependency === "object" ? dependency?.status : null;
      roots.set(taskId, row
        ? AUTO_DRAIN_REASON_LABELS[autoDrainReason(row)] || autoDrainReason(row)
        : status || "outside this snapshot");
    }
  }
  return roots;
}

function autoDrainTaskIdentity(task, workspace) {
  const taskId = autoDrainTaskId(task);
  return taskId
    ? autoDrainTaskLink(taskId, workspace)
    : el("strong", { class: "auto-drain-missing-id", text: "Task ID not supplied" });
}

function autoDrainReasonText(task) {
  const reason = autoDrainReason(task);
  return `${AUTO_DRAIN_REASON_LABELS[reason] || "Unknown readiness reason"} · ${reason}`;
}

// The full per-task card: identity, state pill, server reason, and every
// reason-specific evidence row. Used as-is inside the expanded dependency and
// "other" groups so nothing the server said is hidden.
function autoDrainTaskCard(task, workspace) {
  const eligible = task.eligible === true;
  const state = eligible ? "eligible" : "waiting";
  const item = el("li", { class: `operation-card auto-drain-task ${state}` });
  item.appendChild(el("div", { class: "auto-drain-task-head" }, [
    autoDrainTaskIdentity(task, workspace),
    el("span", { class: `operation-state ${eligible ? "enabled" : "waiting"}`, text: state }),
    el("span", { class: "auto-drain-reason", text: autoDrainReasonText(task) }),
  ]));
  const evidence = autoDrainTaskEvidence(task, workspace);
  if (evidence.length > 0) item.appendChild(el("div", { class: "auto-drain-evidence" }, evidence));
  return item;
}

function autoDrainGroupHeader(tone, title, count, hint, trailing = null) {
  return el("div", { class: "auto-drain-group-head" }, [
    el("div", { class: "auto-drain-group-title" }, [
      el("span", { class: `auto-drain-group-dot ${tone}` }),
      el("strong", { text: title }),
      el("span", { class: "auto-drain-group-count mono", text: String(count) }),
    ]),
    trailing || (hint ? el("span", { class: "auto-drain-group-hint", text: hint }) : null),
  ]);
}

function autoDrainEligibleGroup(tasks, workspace) {
  const group = el("section", { class: "auto-drain-group" });
  group.appendChild(autoDrainGroupHeader("eligible", "Eligible now", tasks.length,
    tasks.length > 0 ? "Admitted in this order when the window starts" : "Nothing in this snapshot can start right now"));
  if (tasks.length === 0) return group;
  const list = el("ol", { class: "auto-drain-rows" });
  tasks.forEach((task, index) => {
    const reason = autoDrainReason(task);
    list.appendChild(el("li", { class: "auto-drain-row auto-drain-row-eligible auto-drain-task eligible" }, [
      el("span", { class: "auto-drain-row-index mono", text: String(index + 1) }),
      autoDrainTaskIdentity(task, workspace),
      el("span", { class: "auto-drain-row-label", text: AUTO_DRAIN_REASON_LABELS[reason] || "Unknown readiness reason" }),
      el("span", { class: "auto-drain-row-reason mono eligible", text: reason }),
    ]));
  });
  group.appendChild(list);
  return group;
}

function autoDrainSelectorChips(selectors) {
  const chips = el("div", { class: "auto-drain-chips" });
  const visible = selectors.slice(0, 3);
  for (const selector of visible) chips.appendChild(el("span", { class: "auto-drain-chip mono", text: selector, title: selector }));
  if (selectors.length > visible.length) {
    const more = el("details", { class: "auto-drain-chips-more" });
    more.appendChild(el("summary", { class: "auto-drain-chip mono", text: `+${selectors.length - visible.length} more` }));
    more.appendChild(el("div", { class: "auto-drain-chips" }, selectors.slice(visible.length).map((selector) =>
      el("span", { class: "auto-drain-chip mono", text: selector, title: selector }))));
    chips.appendChild(more);
  }
  return chips;
}

// Lock-blocked rows grouped by the task holding the lock, so one running task
// that holds five files reads as one holder with five files, not five rows
// each repeating "held by".
function autoDrainLockedGroup(tasks, workspace, occupancy) {
  const group = el("section", { class: "auto-drain-group" });
  group.appendChild(autoDrainGroupHeader("locked", "Blocked by a running task", tasks.length,
    tasks.length > 0 ? "Become eligible when the holder finishes" : ""));
  if (tasks.length === 0) return group;

  const slotPhase = new Map();
  for (const run of Array.isArray(occupancy?.runs) ? occupancy.runs : []) {
    for (const taskId of Array.isArray(run?.task_ids) ? run.task_ids : []) {
      if (run.phase) slotPhase.set(taskId, run.phase);
    }
  }

  // One row per task, filed under its first holder; further holders are
  // named on the row so the count of rows stays the count of tasks.
  const byHolder = new Map();
  for (const task of tasks) {
    const holder = autoDrainHolders(task)[0] ?? "";
    if (!byHolder.has(holder)) byHolder.set(holder, []);
    byHolder.get(holder).push(task);
  }

  for (const [holder, blocked] of [...byHolder.entries()].sort(([a], [b]) => a.localeCompare(b))) {
    const head = el("div", { class: "auto-drain-holder" });
    if (holder) {
      head.append(
        el("span", { text: "held by " }),
        autoDrainTaskLink(holder, workspace),
      );
      const phase = slotPhase.get(holder);
      head.appendChild(el("span", {
        class: `auto-drain-holder-phase mono ${phase ? "running" : ""}`,
        text: phase ? `${phase.replaceAll("_", " ")} · occupying a slot` : "not in an occupied slot",
      }));
    } else {
      head.appendChild(el("span", { text: "holder not supplied" }));
    }
    group.appendChild(head);
    const list = el("ul", { class: "auto-drain-rows" });
    for (const task of blocked) {
      const reason = autoDrainReason(task);
      const selectors = autoDrainConflictSelectors(task, holder || null);
      const detail = el("div", { class: "auto-drain-row-detail" });
      if (selectors.length > 0) detail.appendChild(autoDrainSelectorChips(selectors));
      const runIds = Array.isArray(task.run_ids) ? task.run_ids : [];
      if (runIds.length > 0) {
        detail.appendChild(el("div", { class: "auto-drain-row-note" }, [
          el("span", { text: "claimed by " }),
          ...runIds.flatMap((runId, index) => [index > 0 ? el("span", { text: ", " }) : null, autoDrainRunLink(runId, workspace)]),
        ]));
      }
      const otherHolders = autoDrainHolders(task).filter((other) => other !== holder);
      if (holder && otherHolders.length > 0) {
        detail.appendChild(el("div", { class: "auto-drain-row-note" }, [
          el("span", { text: "also blocked by " }),
          ...otherHolders.flatMap((other, index) => [index > 0 ? el("span", { text: ", " }) : null, autoDrainTaskLink(other, workspace)]),
        ]));
      }
      if (selectors.length === 0 && runIds.length === 0) {
        detail.appendChild(el("div", { class: "auto-drain-row-note", text: "Conflict details were not supplied." }));
      }
      list.appendChild(el("li", { class: "auto-drain-row auto-drain-row-locked auto-drain-task waiting" }, [
        autoDrainTaskIdentity(task, workspace),
        el("span", { class: "auto-drain-row-reason mono", text: reason, title: AUTO_DRAIN_REASON_LABELS[reason] || "Unknown readiness reason" }),
        detail,
      ]));
    }
    group.appendChild(list);
  }
  return group;
}

// Dependency-waiting rows collapsed to what they have in common: the tasks
// outside the group the chain bottoms out on, and the chain itself by depth.
// The full cards stay one click away so every server row is still shown.
function autoDrainDependencyGroup(tasks, allTasks, workspace) {
  const group = el("section", { class: "auto-drain-group" });
  const list = el("ul", { class: "auto-drain-task-list auto-drain-group-rows" });
  list.id = "auto-drain-dependency-rows";
  const toggle = el("button", { class: "auto-drain-group-toggle" });
  toggle.type = "button";
  toggle.setAttribute("aria-controls", list.id);
  const applyOpen = () => {
    list.hidden = !autoDrainDependencyRowsOpen;
    toggle.setAttribute("aria-expanded", autoDrainDependencyRowsOpen ? "true" : "false");
    toggle.textContent = autoDrainDependencyRowsOpen ? "Hide rows" : `Show all ${tasks.length}`;
  };
  toggle.addEventListener("click", () => {
    autoDrainDependencyRowsOpen = !autoDrainDependencyRowsOpen;
    applyOpen();
  });
  applyOpen();
  group.appendChild(autoDrainGroupHeader("dependency", "Waiting on dependencies", tasks.length,
    "Chained behind other backlog tasks", tasks.length > 0 ? toggle : null));
  if (tasks.length === 0) return group;

  const body = el("div", { class: "auto-drain-group-body" });
  const roots = autoDrainDependencyRoots(tasks, allTasks);
  const lead = el("p", { class: "auto-drain-chain-lead" });
  if (roots.size === 0) {
    lead.textContent = `All ${tasks.length} wait on each other; the server reported no dependency outside this group.`;
  } else {
    lead.append(el("span", { text: `All ${tasks.length} chain back to ` }));
    [...roots.entries()].forEach(([taskId, why], index) => {
      if (index > 0) lead.appendChild(el("span", { text: index === roots.size - 1 ? " and " : ", " }));
      lead.append(autoDrainTaskLink(taskId, workspace), el("span", { class: "auto-drain-chain-why", text: ` (${why})` }));
    });
    lead.appendChild(el("span", { text: ". Nothing here can start until that clears." }));
  }
  body.appendChild(lead);

  const depths = autoDrainDependencyDepths(tasks);
  const byDepth = new Map();
  for (const task of tasks) {
    const taskId = autoDrainTaskId(task);
    if (!taskId) continue;
    const depth = depths.get(taskId) ?? 0;
    if (!byDepth.has(depth)) byDepth.set(depth, []);
    byDepth.get(depth).push(taskId);
  }
  const chain = el("div", { class: "auto-drain-chain" });
  const levels = [...byDepth.keys()].sort((a, b) => a - b);
  if (roots.size > 0) {
    for (const taskId of roots.keys()) chain.appendChild(el("span", { class: "auto-drain-chain-node mono root" }, [autoDrainTaskLink(taskId, workspace)]));
    chain.appendChild(el("span", { class: "auto-drain-chain-arrow", text: "→" }));
  }
  levels.forEach((level, index) => {
    if (index > 0) chain.appendChild(el("span", { class: "auto-drain-chain-arrow", text: "→" }));
    const ids = byDepth.get(level).sort();
    chain.appendChild(el("span", { class: "auto-drain-chain-node mono" }, ids.flatMap((taskId, i) =>
      [i > 0 ? el("span", { class: "auto-drain-chain-sep", text: " · " }) : null, autoDrainTaskLink(taskId, workspace)])));
  });
  body.appendChild(chain);

  for (const task of tasks) list.appendChild(autoDrainTaskCard(task, workspace));
  body.appendChild(list);
  group.appendChild(body);
  return group;
}

function autoDrainOtherGroup(tasks, workspace) {
  const group = el("section", { class: "auto-drain-group" });
  group.appendChild(autoDrainGroupHeader("other", "Other reasons", tasks.length, "Every remaining server reason, with its evidence"));
  const list = el("ul", { class: "auto-drain-task-list" });
  for (const task of tasks) list.appendChild(autoDrainTaskCard(task, workspace));
  group.appendChild(list);
  return group;
}

function autoDrainReadinessList(payload, workspace) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  const section = el("section", { class: "auto-drain-readiness" });
  const heading = el("h3", { class: "auto-drain-readiness-title", text: "Task readiness" });
  heading.id = "auto-drain-readiness-title";
  section.setAttribute("aria-labelledby", heading.id);
  section.appendChild(heading);
  if (tasks.length === 0) {
    section.appendChild(el("div", {
      class: "empty-state auto-drain-empty",
      text: "No readiness rows were returned in this bounded snapshot. This does not establish that the workspace has no backlog tasks.",
    }));
    return section;
  }

  const groups = autoDrainGroups(tasks);
  section.appendChild(autoDrainEligibleGroup(groups.eligible, workspace));
  if (groups.locked.length > 0) section.appendChild(autoDrainLockedGroup(groups.locked, workspace, payload.capacity?.occupancy));
  if (groups.dependency.length > 0) section.appendChild(autoDrainDependencyGroup(groups.dependency, tasks, workspace));
  if (groups.other.length > 0) section.appendChild(autoDrainOtherGroup(groups.other, workspace));
  return section;
}

// Slot tiles: one per configured leaf slot, occupied ones naming the task(s)
// the run carries and its phase, so "2 / 5" is visible as which two.
function autoDrainSlots(capacity, workspace) {
  const occupancy = capacity.occupancy || {};
  const runs = Array.isArray(occupancy.runs) ? occupancy.runs : [];
  const active = Number(capacity.active_leaf_runs ?? occupancy.active_leaf_runs ?? runs.length);
  const max = Number(capacity.max_active_leaf_runs);
  const free = Number(capacity.free_slots ?? occupancy.free_slots);
  const phaseEntries = Object.entries(occupancy.phases || {}).filter(([, count]) => Number(count) > 0);
  const phaseSummary = phaseEntries.map(([phase, count]) => `${count} ${phase.replaceAll("_", "-")}`).join(", ");
  const section = el("section", { class: "auto-drain-slots" });
  section.appendChild(el("div", { class: "auto-drain-slots-head" }, [
    el("div", { class: "auto-drain-slots-title" }, [
      el("span", { class: "operation-field-label", text: "Slots" }),
      el("span", { class: "auto-drain-slots-count mono", text: Number.isFinite(max) ? `${active} / ${max} occupied` : `${active} occupied` }),
    ]),
    el("span", { class: "auto-drain-group-hint", text: [
      phaseSummary,
      capacity.limit_source ? `limit from ${String(capacity.limit_source).replaceAll("_", " ")}` : "",
    ].filter(Boolean).join(" · ") }),
  ]));
  const tiles = el("div", { class: "auto-drain-slot-grid" });
  if (runs.length === 0) {
    // Older servers roll occupancy up by phase only; one tile per counted
    // phase keeps the picture honest without inventing task ids.
    for (const [phase, count] of phaseEntries) {
      for (let index = 0; index < Number(count); index += 1) {
        tiles.appendChild(el("div", { class: `auto-drain-slot occupied ${phase}` }, [
          el("span", { class: "auto-drain-slot-tasks mono", text: "task not supplied" }),
          el("span", { class: "auto-drain-slot-phase mono", text: phase.replaceAll("_", " ") }),
        ]));
      }
    }
  }
  for (const run of runs) {
    const taskIds = Array.isArray(run?.task_ids) ? run.task_ids : [];
    const phase = run?.phase ? String(run.phase).replaceAll("_", " ") : "phase not supplied";
    const tile = el("div", { class: `auto-drain-slot occupied ${run?.phase || ""}` });
    const ids = el("div", { class: "auto-drain-slot-tasks" });
    if (taskIds.length === 0) ids.appendChild(run?.run_id ? autoDrainRunLink(run.run_id, workspace) : el("span", { class: "mono", text: "task not supplied" }));
    for (const taskId of taskIds) ids.appendChild(autoDrainTaskLink(taskId, workspace));
    tile.append(ids, el("span", { class: "auto-drain-slot-phase mono", text: phase }));
    tiles.appendChild(tile);
  }
  const freeTiles = Number.isFinite(free) ? Math.max(0, free) : 0;
  for (let index = 0; index < freeTiles; index += 1) {
    tiles.appendChild(el("div", { class: "auto-drain-slot free", text: "free" }));
  }
  if (tiles.childElementCount === 0) {
    tiles.appendChild(el("div", { class: "auto-drain-slot free", text: "Slot details were not supplied" }));
  }
  section.appendChild(tiles);
  return section;
}

function autoDrainStat(tone, label, value, note) {
  return el("div", { class: `auto-drain-stat ${tone}` }, [
    el("span", { class: "operation-field-label", text: label }),
    el("span", { class: "auto-drain-stat-value mono", text: value == null || value === "" ? "—" : String(value) }),
    el("span", { class: "auto-drain-stat-note", text: note }),
  ]);
}

function autoDrainSummary(payload, groups, counts) {
  const capacity = payload.capacity || {};
  const total = counts.eligible + counts.waiting;
  const free = Number(capacity.free_slots);
  const eligibleNote = !Number.isFinite(free) ? "free slots unknown"
    : counts.eligible === 0 ? "nothing to admit"
    : counts.eligible >= free ? "fills every free slot"
    : `leaves ${free - counts.eligible} slot${free - counts.eligible === 1 ? "" : "s"} free`;
  const holders = new Set(groups.locked.flatMap(autoDrainHolders));
  const roots = autoDrainDependencyRoots(groups.dependency, Array.isArray(payload.tasks) ? payload.tasks : []);
  const poolNote = capacity.candidate_pool_size == null
    ? "candidate pool not supplied"
    : `candidate pool ${capacity.candidate_pool_size}${capacity.candidate_pool_truncated ? " · truncated" : ""}`;
  return el("div", { class: "auto-drain-summary" }, [
    autoDrainStat("eligible", "Eligible now", counts.eligible, eligibleNote),
    autoDrainStat("locked", "Blocked by a running task", groups.locked.length,
      holders.size > 0 ? `held by ${holders.size} task${holders.size === 1 ? "" : "s"}` : "no holders reported"),
    autoDrainStat("dependency", "Waiting on deps", groups.dependency.length,
      roots.size > 0 ? `${roots.size === 1 ? "one chain, rooted at" : "rooted at"} ${[...roots.keys()].join(", ")}` : "no chain roots reported"),
    autoDrainStat("", "Waiting", counts.waiting, `of ${total} scanned · ${poolNote}`),
  ]);
}

function autoDrainStartButton(payload) {
  const key = "auto-drain:start";
  const reasons = autoDrainReasons(payload);
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: "operation-button primary auto-drain-start",
    text: pending ? "Starting…" : `Start ${autoDrainDuration} window`,
    title: reasons.submit || "Submit orbit.workflow.auto with this duration and concurrency",
  });
  button.type = "button";
  button.disabled = Boolean(reasons.submit) || pending || autoDrainComplete && Boolean(reasons.complete);
  button.addEventListener("click", async () => {
    if (pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const counts = autoDrainCounts(payload);
    const completeLine = autoDrainComplete
      ? `WARNING: ${AUTO_DRAIN_COMPLETE_WARNING}`
      : "Shipped tasks stay in review; a separate action completes them.";
    const confirmText = [
      `Start a bounded auto-delivery window in workspace "${workspace?.name || workspace?.id}"?`,
      `Duration: ${autoDrainDuration} · Concurrency: ${autoDrainConcurrency || "runtime default"}`,
      `Currently eligible: ${counts.eligible} · waiting: ${counts.waiting}`,
      "",
      completeLine,
    ].join("\n");
    if (!window.confirm(confirmText)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", `Starting a ${autoDrainDuration} auto-delivery window…`);
    renderAutoDrain(payload);
    try {
      const body = { for_duration: autoDrainDuration, complete: autoDrainComplete };
      if (autoDrainConcurrency) body.concurrency = Number(autoDrainConcurrency);
      const result = await postJson("/api/workflows/auto", body);
      const runId = result?.run_id ?? null;
      const state = result?.state ?? "submitted";
      const completion = result?.completion ?? "review";
      feedback("auto-drain-operation-feedback", "success", `Run ${runId ?? "(no run id)"} ${state} (completion: ${completion}).`);
      lastAutoDrainRun = runId ? { runId, state, completion, workspaceId: workspace?.id } : null;
      await fetchAndRenderAutoDrain();
    } catch (error) {
      feedback("auto-drain-operation-feedback", "error", `Auto-delivery window failed to start: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastAutoDrain) renderAutoDrain(lastAutoDrain);
    }
  });
  return button;
}

const AUTO_DRAIN_STOP_CONFIRM = "Stop new admissions for the active auto-delivery window? Already admitted workers keep running under their captured completion authority. This is not cancellation.";

// [ORB-12728] Counterpart to `orbit run auto --stop`: stops new admissions on
// the live coordinator without cancelling it or the workers it already
// admitted. Enabled only while readiness reports a live, not-yet-stopped
// window and the session may govern it; the title says which of those is
// missing otherwise.
function autoDrainStopButton(payload) {
  const key = "auto-drain:stop";
  const reasons = autoDrainReasons(payload);
  const live = autoDrainLiveWindow(payload);
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: "operation-button disable auto-drain-stop",
    text: pending ? "Stopping…" : "Stop admissions",
    title: reasons.stop || AUTO_DRAIN_STOP_CONFIRM,
  });
  button.type = "button";
  button.disabled = Boolean(reasons.stop) || pending;
  button.addEventListener("click", async () => {
    if (pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    if (!window.confirm(`${AUTO_DRAIN_STOP_CONFIRM}\n\nWindow: ${live.runId} in workspace "${workspace?.name || workspace?.id}"`)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", `Stopping admissions for ${live.runId}…`);
    renderAutoDrain(payload);
    try {
      const result = await postJson("/api/workflows/auto/stop", {});
      const coordinators = Array.isArray(result?.coordinators) ? result.coordinators : [];
      const remaining = coordinators.reduce((sum, change) => sum + (Array.isArray(change?.remaining_children) ? change.remaining_children.length : 0), 0);
      const changes = coordinators.map((change) => `${change?.run_id ?? "(no run id)"}: ${change?.outcome ?? "?"}`).join(", ");
      feedback("auto-drain-operation-feedback", "success", [
        `Admissions ${result?.outcome ?? "stop requested"}`,
        changes ? `(${changes})` : "",
        remaining > 0 ? `· ${remaining} admitted worker${remaining === 1 ? "" : "s"} still running.` : ".",
      ].filter(Boolean).join(" "));
      await fetchAndRenderAutoDrain();
    } catch (error) {
      feedback("auto-drain-operation-feedback", "error", `Stopping admissions failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastAutoDrain) renderAutoDrain(lastAutoDrain);
    }
  });
  return button;
}

// The live window row names the coordinator the stop button acts on, so the
// operator sees what they are stopping; it reads "stopped" once the flag is
// set rather than hiding the run.
function autoDrainLiveWindowRow(payload, workspace) {
  const live = autoDrainLiveWindow(payload);
  const row = el("div", { class: "auto-drain-live-window" });
  if (!live.runId) {
    row.appendChild(el("span", { text: "No live window." }));
    return row;
  }
  row.append(
    el("span", { text: live.admissionsStopped ? "Live window (admissions stopped):" : "Live window:" }),
    workspace ? autoDrainRunLink(live.runId, workspace) : el("span", { class: "mono", text: live.runId }),
  );
  return row;
}

// The window controls sit above the readiness groups: the operator reads the
// slot picture, then acts, instead of scrolling past every row to find the
// button. The duration picker is a segmented control over the same bounded
// list the confirm text quotes.
function autoDrainControls(payload, counts) {
  const reasons = autoDrainReasons(payload);
  const capacity = payload.capacity || {};
  const controls = el("section", { class: "auto-drain-controls" });

  const durationGroup = el("div", { class: "auto-drain-control" });
  durationGroup.appendChild(el("span", { class: "operation-field-label", text: "Duration" }));
  const segment = el("div", { class: "auto-drain-segment" });
  segment.setAttribute("role", "group");
  segment.setAttribute("aria-label", "Window duration");
  for (const value of AUTO_DRAIN_DURATIONS) {
    const option = el("button", { class: `auto-drain-segment-option${value === autoDrainDuration ? " selected" : ""}`, text: value });
    option.type = "button";
    option.setAttribute("aria-pressed", value === autoDrainDuration ? "true" : "false");
    option.addEventListener("click", () => {
      autoDrainDuration = value;
      renderAutoDrain(payload);
    });
    segment.appendChild(option);
  }
  durationGroup.appendChild(segment);

  const concurrencyGroup = el("div", { class: "auto-drain-control" });
  const concurrencyLabel = el("label", { class: "operation-field-label", text: "Concurrency" });
  concurrencyLabel.htmlFor = "auto-drain-concurrency";
  const concurrencyInput = el("input", { class: "operation-cadence auto-drain-concurrency", title: "Leaf-run concurrency (blank = runtime default)" });
  concurrencyInput.id = "auto-drain-concurrency";
  concurrencyInput.type = "number";
  concurrencyInput.min = "1";
  concurrencyInput.placeholder = Number.isFinite(Number(capacity.max_active_leaf_runs)) ? `Runtime default (${capacity.max_active_leaf_runs})` : "Runtime default";
  concurrencyInput.value = autoDrainConcurrency;
  concurrencyInput.addEventListener("change", () => {
    autoDrainConcurrency = concurrencyInput.value.trim();
  });
  concurrencyGroup.append(concurrencyLabel, concurrencyInput);

  const completeGroup = el("div", { class: "auto-drain-control" });
  completeGroup.appendChild(el("span", { class: "operation-field-label", text: "Completion" }));
  const completeLabel = el("label", { class: "auto-drain-complete", title: reasons.complete || AUTO_DRAIN_COMPLETE_WARNING });
  const completeCheckbox = el("input");
  completeCheckbox.type = "checkbox";
  completeCheckbox.checked = autoDrainComplete;
  completeCheckbox.disabled = Boolean(reasons.complete);
  completeCheckbox.addEventListener("change", () => {
    autoDrainComplete = completeCheckbox.checked;
    renderAutoDrain(payload);
  });
  completeLabel.append(completeCheckbox, el("span", { text: "Also mark shipped tasks done " }), el("span", { class: "auto-drain-muted", text: "(skip review)" }));
  completeGroup.appendChild(completeLabel);
  if (reasons.complete) completeGroup.appendChild(el("span", { class: "auto-drain-control-note", text: reasons.complete }));

  const free = Number(capacity.free_slots);
  const admits = Number.isFinite(free) ? Math.min(free, counts.eligible) : counts.eligible;
  const outcome = el("div", { class: "auto-drain-outcome" });
  outcome.append(
    el("span", { text: "Admits up to " }),
    el("strong", { text: `${admits} task${admits === 1 ? "" : "s"}` }),
    el("span", { text: Number.isFinite(free) ? " into " : "" }),
    Number.isFinite(free) ? el("strong", { text: `${free} free slot${free === 1 ? "" : "s"}` }) : null,
    el("br"),
    el("span", { text: "Completion " }),
    el("strong", { text: autoDrainComplete ? "done (skip review)" : "review" }),
    el("span", { text: " · concurrency " }),
    el("strong", { text: autoDrainConcurrency || "runtime default" }),
  );

  controls.appendChild(el("div", { class: "auto-drain-controls-row" }, [
    el("div", { class: "auto-drain-controls-fields" }, [durationGroup, concurrencyGroup, completeGroup]),
    el("div", { class: "auto-drain-controls-action" }, [
      autoDrainStartButton(payload),
      outcome,
      autoDrainStopButton(payload),
      autoDrainLiveWindowRow(payload, selectedWorkspace()),
    ]),
  ]));
  if (autoDrainComplete) {
    controls.appendChild(el("p", { class: "operation-control-note operation-mint-warning", text: AUTO_DRAIN_COMPLETE_WARNING }));
  }
  const total = counts.eligible + counts.waiting;
  const limitations = payload.snapshot?.limitations || "Snapshot only: eligibility can change immediately and does not guarantee a task will start.";
  const truncated = capacity.candidate_pool_truncated ? " The candidate pool was truncated before all available slots could be filled." : "";
  controls.appendChild(el("p", {
    class: "auto-drain-snapshot-note",
    title: limitations,
    text: `Snapshot only: nothing is reserved or started until you start a window. The server returned a bounded snapshot of ${total} task${total === 1 ? "" : "s"}; these counts are not a workspace total.${truncated} Proposed tasks are never drained automatically; promote a task to backlog first.`,
  }));
  return controls;
}

function renderAutoDrain(payload) {
  lastAutoDrain = payload;
  const body = $("auto-drain-body");
  if (!body) return;
  body.textContent = "";
  const reasons = autoDrainReasons(payload);
  const workspace = selectedWorkspace();
  if (reasons.submit) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: reasons.submit }));
    $("auto-drain-count").textContent = "read-only";
    setAutoDrainRailCount(null);
    return;
  }
  const counts = autoDrainCounts(payload);
  const capacity = payload.capacity || {};
  const groups = autoDrainGroups(Array.isArray(payload.tasks) ? payload.tasks : []);

  body.appendChild(autoDrainControls(payload, counts));
  if (lastAutoDrainRun && lastAutoDrainRun.workspaceId === workspace?.id) {
    const runLink = el("a", {
      class: "operation-control-note auto-drain-last-run",
      text: `Open run ${lastAutoDrainRun.runId} (${lastAutoDrainRun.state}, completion: ${lastAutoDrainRun.completion}) →`,
      title: "Open the submitted parent run",
    });
    runLink.href = "#";
    runLink.addEventListener("click", (event) => {
      event.preventDefault();
      navigateToRun(lastAutoDrainRun.runId, lastAutoDrainRun.workspaceId);
    });
    body.appendChild(runLink);
  }
  body.appendChild(autoDrainSlots(capacity, workspace));
  body.appendChild(autoDrainSummary(payload, groups, counts));
  body.appendChild(autoDrainReadinessList(payload, workspace));

  const slots = Number.isFinite(Number(capacity.max_active_leaf_runs))
    ? ` · ${capacity.active_leaf_runs ?? "—"}/${capacity.max_active_leaf_runs} slots`
    : "";
  $("auto-drain-count").textContent = `${counts.eligible} eligible${slots} · ${workspace?.name || workspace?.id}`;
  setAutoDrainRailCount(counts.eligible);
}

// The rail entry mirrors Tasks: it shows how many tasks are eligible now, so
// the Work group reads as "19 tasks, 3 ready to drain" from any other tab.
function setAutoDrainRailCount(eligible) {
  const node = $("rail-count-auto-drain");
  if (!node) return;
  node.textContent = Number.isFinite(Number(eligible)) && Number(eligible) > 0 ? String(eligible) : "";
}

function fetchAndRenderAutoDrain() {
  const workspace = selectedWorkspace();
  if (!workspace) {
    return requestPanel("auto-drain-body", "unselected", () => Promise.resolve({}), renderAutoDrain, "auto-drain-count");
  }
  const query = autoDrainConcurrency ? `?concurrency=${encodeURIComponent(autoDrainConcurrency)}` : "";
  return loadOperationPanel("auto-drain-body", `/api/workflows/auto/readiness${query}`, renderAutoDrain);
}

// ORB-11332: operation mode. The panel projects `orbit operation explain`
// (current preferences, the captured grant policy when one is active, caps,
// and limiting reasons) and offers the two governed grant controls.
// Enablement is deliberately not a dashboard action: a grant names a finite
// task set and explicit rights, which is an operator decision made from the
// CLI or MCP.
const OPERATION_MODE_CONTROLS = {
  stop: {
    label: "Stop grant",
    confirm: "Stop new admissions and promotion under this grant? Admitted work keeps its captured bounds, including completion. This is not cancellation.",
    enabled: (authority) => authority.admission === "open",
  },
  revoke: {
    label: "Revoke grant",
    confirm: "WARNING: revocation withdraws privileged actions, including completion, from work already admitted under this grant. Bound drains stop admitting. Continue?",
    enabled: (authority) => authority.status !== "revoked",
  },
};

function policyField(policy, name, label) {
  const entry = policy?.[name] || {};
  const value = entry.value ?? "—";
  const source = entry.source ? ` [${entry.source}]` : "";
  return field(label, `${value}${source}`);
}

function policyGrid(policy) {
  return el("div", { class: "operation-grid" }, [
    policyField(policy, "preset", "Preset"),
    policyField(policy, "preparation", "Preparation"),
    policyField(policy, "promotion", "Promotion"),
    policyField(policy, "completion", "Completion"),
    policyField(policy, "recovery", "Recovery"),
    policyField(policy, "leaf_ceiling", "Leaf ceiling"),
    policyField(policy, "review_policy", "Review policy"),
    policyField(policy, "review_crew", "Review crew (before-PR)"),
    policyField(policy, "review_reviewer_starts", "Reviewer starts / lineage"),
    policyField(policy, "review_repair_cycles", "Repair cycles / lineage"),
    policyField(policy, "review_minutes", "Review minutes / lineage"),
    policyField(policy, "delivery_cap", "Delivery cap"),
  ]);
}

function operationControlButton(payload, kind) {
  const control = OPERATION_MODE_CONTROLS[kind];
  const authority = payload.authority || {};
  const key = `operation:${kind}`;
  const pending = pendingOperations.has(key);
  const unauthorized = payload.controls_authorized === false;
  const button = el("button", {
    class: `operation-button ${kind === "revoke" ? "disable" : "secondary"}`,
    text: pending ? `${control.label}…` : control.label,
    title: unauthorized
      ? "Grant controls require an authorized operator session."
      : control.confirm,
  });
  button.type = "button";
  button.disabled = unauthorized || pending || !control.enabled(authority);
  button.addEventListener("click", async () => {
    if (pendingOperations.has(key)) return;
    if (!window.confirm(`${control.confirm}\n\nGrant: ${authority.grant_id} (revision ${authority.revision})`)) return;
    pendingOperations.add(key);
    feedback("operation-mode-operation-feedback", "pending", `${control.label} in progress…`);
    renderOperationMode(payload);
    try {
      const result = await postJson(`/api/operation/${kind}`, {
        grant_id: authority.grant_id,
        expected_revision: authority.revision,
      });
      feedback("operation-mode-operation-feedback", "success", `Grant ${result?.grant_id ?? authority.grant_id}: ${result?.outcome ?? kind} (revision ${result?.revision ?? "?"}).`);
      await fetchAndRenderOperationMode();
    } catch (error) {
      feedback("operation-mode-operation-feedback", "error", `${control.label} failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastOperationMode) renderOperationMode(lastOperationMode);
    }
  });
  return button;
}

function renderOperationMode(payload) {
  lastOperationMode = payload;
  const body = $("operation-mode-body");
  if (!body) return;
  body.textContent = "";
  const workspaceReason = workspaceReadOnlyReason();
  const workspace = selectedWorkspace();
  if (workspaceReason) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: workspaceReason }));
    $("operation-mode-count").textContent = "read-only";
    return;
  }
  const policy = payload.policy || {};
  const authority = payload.authority || {};
  const delivery = payload.delivery || {};
  const grantPolicy = authority.policy;
  body.append(
    el("div", { class: "operation-row-head" }, [
      operationIdentity(authority.grant_id || "No grant", authority.admission || "none"),
      el("div", { class: "operation-row-facts" }, [
        operationFact("Completion", `${delivery.effective_completion ?? "—"}${delivery.cap ? ` (cap: ${delivery.cap})` : ""}`),
        operationFact("Expires", authority.expires_at ? time(authority.expires_at) : "—"),
      ]),
    ]),
    operationDetails("operation-mode", [
      grantPolicy
        ? el("p", {
          class: "operation-control-note",
          text: "Active grant policy (captured at enablement; retuning preferences does not change it).",
        })
        : null,
      grantPolicy ? policyGrid(grantPolicy) : null,
      el("p", {
        class: "operation-control-note",
        text: grantPolicy
          ? "Current preferences (apply to a future grant only)."
          : "Current preferences.",
      }),
      policyGrid(policy),
      el("div", { class: "operation-grid" }, [
        field("Effective completion", `${delivery.effective_completion ?? "—"}${delivery.cap ? ` (cap: ${delivery.cap})` : ""}`),
        field("Grant", authority.grant_id ?? "none"),
        field("Admission", authority.admission ?? "none"),
        field("Rights", Array.isArray(authority.rights) && authority.rights.length ? authority.rights.join(", ") : "—"),
        field("Scope", Array.isArray(authority.task_ids) ? `${authority.task_ids.length} task(s)` : "—"),
        field("Expires", authority.expires_at ? time(authority.expires_at) : "—"),
      ]),
      el("p", {
        class: "operation-control-note",
        text: Array.isArray(payload.limiting_reasons) && payload.limiting_reasons.length
          ? `Limiting reasons: ${payload.limiting_reasons.join(", ")}`
          : "No limiting reasons.",
      }),
      el("p", {
        class: "operation-control-note",
        text: "Changing a preference activates nothing. Only an explicit grant (orbit operation enable) authorizes scoped automation, and no grant authorizes merge.",
      }),
      el("p", {
        class: "operation-control-note",
        text: "Review crew selects the reviewer for before-PR review only. After-landing review runs from its own delivery auto-task, which mints tasks with that definition's template crew.",
      }),
    ]),
  );
  if (authority.grant_id) {
    body.appendChild(el("div", { class: "operation-clock-actions" }, [
      operationControlButton(payload, "stop"),
      operationControlButton(payload, "revoke"),
    ]));
  }
  $("operation-mode-count").textContent = `${authority.grant_id ? authority.admission : "no grant"} · ${workspace?.name || workspace?.id}`;
}

export function fetchAndRenderOperationMode() {
  if (!selectedWorkspace()) {
    return requestPanel("operation-mode-body", "unselected", () => Promise.resolve({}), renderOperationMode, "operation-mode-count");
  }
  return loadOperationPanel("operation-mode-body", "/api/operation/explain", renderOperationMode);
}

function throwFirstPanelError(results) {
  const errors = results.filter(result => result.status === "rejected").map(result => result.reason);
  // Preserve transport classification even if a different panel also fails.
  if (errors.length) throw errors.find(error => error.networkFailure) || errors[0];
}

export async function fetchAndRenderOperations() {
  const routines = fetchJson("/api/routines");
  throwFirstPanelError(await Promise.allSettled([
    requestPanel("routines-body", "routines", () => routines, renderOperations, "routines-count"),
    requestPanel("clock-body", "clock", () => routines, renderClock, "clock-host"),
    fetchAndRenderAutoTasks(),
    fetchAndRenderJobs(),
  ]));
}

// The Auto-drain destination (Work → Auto-drain) carries the readiness panel
// and the Operation Mode grant that bounds it; both refresh together.
export async function fetchAndRenderAutoDrainPane() {
  throwFirstPanelError(await Promise.allSettled([
    fetchAndRenderAutoDrain(),
    fetchAndRenderOperationMode(),
  ]));
}
