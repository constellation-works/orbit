// Routine-definition, host clock, and auto-task operations [ORB-10875, ORB-10876].

import { captureWorkspaceVisit, requestPanel, describePullSettlements, copyText, detailsPanel, el, fetchJson, formatClock, formatDateTime, getWorkspace, getWorkspaceRevision, isAggregateView, onWorkspaceChange, postJson, statusPill, fmtDuration } from './common.js';
import { navigateToRun, setActiveTab } from './router.js';
import { renderAutomation } from './automation.js';
import { cpuLoadMultiple } from './host-resources.js';

const $ = (id) => document.getElementById(id);
const pendingOperations = new Set();
const UNCONDITIONAL_MINT_WARNING = "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.";
const AUTO_DRAIN_DURATION_SECONDS = { "15m": 900, "30m": 1800, "1h": 3600, "2h": 7200, "4h": 14400, "8h": 28800 };
const AUTO_DRAIN_DURATIONS = Object.keys(AUTO_DRAIN_DURATION_SECONDS);
const AUTO_DRAIN_APPROVE_RULE = "Approval needs context files and an assessed complexity (or the no-diff-expected tag) plus a clean task-pilot verification. Tasks tagged no-auto-approve are skipped.";
const AUTO_DRAIN_APPROVE_WARNING = "The window will approve qualifying proposed tasks, including ones filed while it runs.";
const AUTO_DRAIN_COMPLETE_WARNING = "Also marks every task this window ships as done (review -> done), not only the ones eligible right now.";
let lastOperations = null;
let lastAutoTasks = null;
let lastAutoDrain = null;
let announcedDrainState = null;
let autoDrainDuration = "1h";
let autoDrainConcurrency = "";
let autoDrainComplete = false;
let autoDrainApproveProposed = false;
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

export function initOperations(nextContext) {
  context = nextContext;
  unsubscribeWorkspace?.();
  unsubscribeWorkspace = onWorkspaceChange(() => {
    lastOperations = lastAutoTasks = lastAutoDrain = lastJobs = null;
    announcedDrainState = null;
    updateDrainIndicators("idle", "idle");
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
      const link = el("a", { class: "mono operation-run-link operation-task-link operation-link", text: ` Open ${result.task_id} →` });
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

function runLink(runId, workspaceId, text = runId) {
  const link = el("a", { class: "mono operation-run-link", text, title: `Open run ${runId}` });
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
  const routines = workspace
    ? (payload.routines || []).filter((routine) => routine.source === workspace)
    : [];
  const inactive = workspace
    ? (payload.retired || []).filter((routine) => routine.plugin_inactive && routine.source === workspace)
    : [];
  // A replica schedules only worktree GC; its owner schedules the rest.
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
      "fires", "Owner only", "this replica schedules only worktree GC · enable these on the owner machine",
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
  const link = el("a", { class: "mono operation-run-link operation-task-link operation-link", text: taskId, title: `Open ${taskId}` });
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
        : [mintSource, relativeTime(definition.last_evaluation?.last_fired_at)].filter(Boolean).join(" · "),
      title: definition.last_evaluation?.last_fired_at ? time(definition.last_evaluation.last_fired_at) : "" }),
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
  if (!scheduler) {
    note.appendChild(el("span", { text: "Definitions live in .orbit/auto_tasks/ and are evaluated on the host sweep clock. Toggle writes the definition's enabled field; Mint now bypasses schedule, enabled and dedupe." }));
    return;
  }
  const link = el("a", { class: "operation-run-link operation-routine-link operation-link", text: scheduler.name });
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
  if (inactive.length) {
    body.appendChild(inactivePluginGroup(inactive.map((definition) => ({ name: definition.name, detail: definition.schedule_summary, reason: definition.skipped_reason })), "minted"));
  }
  if (workspace && !workspaceReason) {
    const toggle = inactivePluginToggle(payload.inactive_plugin_count || 0);
    if (toggle) body.appendChild(toggle);
  }
  syncAutoTaskSchedulerNote();
  const count = $("auto-tasks-count");
  const enabledCount = definitions.filter((definition) => definition.enabled).length;
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
  lastJobs = payload;
  if (payload.routines) {
    lastOperations = payload.routines;
    noteCronZone(lastOperations);
  }
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
    body.appendChild(el("p", { class: "operation-control-note", text: `Catalogue projected from routine targets and the last ${JOB_RUN_LIMIT} runs.` }));
  }
  $("jobs-count").textContent = `${catalog.size} job${catalog.size === 1 ? "" : "s"} · ${running.length} running · ${workspace.name}`;
  setRailSubtabCount("rail-count-ops-jobs", running.length ? `${running.length} running` : "");
}

function fetchAndRenderJobs(routinesRequest = null) {
  if (!selectedWorkspace()) {
    return requestPanel("jobs-body", "unselected", () => Promise.resolve({ runs: [] }), renderJobs, "jobs-count");
  }
  return requestPanel("jobs-body", "jobs",
    () => Promise.all([routinesRequest || Promise.resolve(lastOperations), fetchJson(`/api/job-runs?limit=${JOB_RUN_LIMIT}`)])
      .then(([routines, runs]) => ({ routines, runs })),
    renderJobs, "jobs-count");
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
// ORB-12898: the window is a compact card at the top of the Tasks dock's
// Drain mode, above Locked files. It keeps only what an operator acts on:
// the live window, the three settings, Start/Stop, the slot line, two counts,
// and which tasks are waiting on a running one.

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

function autoDrainReasons(payload) {
  const workspaceReason = workspaceReadOnlyReason();
  const live = autoDrainLiveWindow(payload);
  return {
    submit: workspaceReason,
    complete: workspaceReason || (payload.controls_authorized === false
      ? "Automatic completion requires an authorized operator session; the window can still start with default review completion."
      : ""),
    // A pull replica cannot approve (`--pull` conflicts with
    // `--approve-proposed`): the owner's window does that.
    approve: workspaceReason || (payload.replica === true
      ? "A pull replica cannot approve proposed tasks; start the window on the owner machine."
      : payload.controls_authorized === false
        ? "Approving proposed tasks requires an authorized operator session; the window can still start and leave them for you."
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

// Readiness reasons that mean "waiting on another task or run": a context lock, a
// grouped member, a same-wave deferral, or a live child's claim.
const AUTO_DRAIN_LOCK_REASONS = new Set(["context_lock_conflict", "group_member_conflict", "conflict_deferred", "claimed_by_live_child"]);
const AUTO_DRAIN_BLOCKED_ROWS = 3;

function autoDrainTaskId(task) {
  return typeof task.task_id === "string" && task.task_id.trim() ? task.task_id : null;
}

function autoDrainReason(task) {
  return typeof task.reason === "string" && task.reason.trim() ? task.reason : "unknown";
}

function autoDrainBlocked(payload) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  return tasks.filter((task) => task.eligible !== true && AUTO_DRAIN_LOCK_REASONS.has(autoDrainReason(task)));
}

// Readiness reasons that mean "waiting on slots or host pressure": a full
// workspace, a throttle, a scheduled shutdown or stopped admissions.
const AUTO_DRAIN_CAPACITY_REASONS = new Set(["capacity_saturated", "resource_throttled", "host_shutdown_scheduled", "admissions_stopped", "cpu_light_budget_full"]);

// Every readiness task lands in exactly one pool group, so the group counts add
// up to the backlog total. Each group keeps its per-reason counts for the
// stat's tooltip. The word "blocked" is left to the task status.
const AUTO_DRAIN_POOL_GROUPS = [
  { key: "eligible", label: "Pool: eligible" },
  { key: "locks", label: "Pool: waiting on locks" },
  { key: "capacity", label: "Pool: waiting on capacity" },
  { key: "other", label: "Pool: waiting, other" },
];

function autoDrainPool(payload) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  const groups = Object.fromEntries(AUTO_DRAIN_POOL_GROUPS.map(({ key, label }) => [key, { key, label, count: 0, reasons: new Map() }]));
  for (const task of tasks) {
    const reason = autoDrainReason(task);
    const key = task.eligible === true ? "eligible"
      : AUTO_DRAIN_LOCK_REASONS.has(reason) ? "locks"
      : AUTO_DRAIN_CAPACITY_REASONS.has(reason) ? "capacity" : "other";
    groups[key].count += 1;
    groups[key].reasons.set(reason, (groups[key].reasons.get(reason) || 0) + 1);
  }
  return AUTO_DRAIN_POOL_GROUPS.map(({ key }) => groups[key]);
}

// Holder ids for a lock-blocked row. Context locks report
// `conflicts[].locking_task_id`; same-wave deferrals report
// `conflicts[].blocking_task_id` plus `blocking_task_ids`; live-child claims
// report only `run_ids`, which have no task holder.
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

// `file:crates/a/b/c.rs` → `…/b/c.rs`: the dock is 336px, so the lock line
// keeps the part of the path that tells files apart; the title has the rest.
function autoDrainShortSelector(selector) {
  const parts = String(selector).replace(/^[a-z]+:/, "").split("/");
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : parts.join("/");
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
  const indicatorPhase = aggregate ? "per-workspace" : phase;
  const indicatorLabel = aggregate ? "Per-workspace drain status" : label;
  const tab = document.querySelector?.('#dock-mode-toggle [data-mode="drain"]');
  const tabState = $("dock-drain-state");
  if (tab) {
    tab.dataset.drainState = indicatorPhase;
    tab.setAttribute("aria-label", `Drain: ${indicatorLabel}`);
    tab.title = indicatorLabel;
  }
  if (tabState) tabState.textContent = aggregate ? "per workspace" : phase === "winding_down" ? label : "";
  const global = $("global-drain-state");
  if (global) {
    global.hidden = phase === "idle" && !aggregate;
    global.dataset.drainState = indicatorPhase;
    global.setAttribute("aria-label", aggregate
      ? "Drain status is per workspace. Select a workspace to inspect its live status."
      : `${label}. Open Drain card`);
    const text = global.querySelector?.('.global-drain-label');
    if (text) text.textContent = indicatorLabel;
    if (!global.dataset.wired) {
      global.addEventListener("click", () => setActiveTab("auto-drain"));
      global.dataset.wired = "true";
    }
  }
}

// Header row: the state dot and the live window on the right. The run link is
// the short id with the full one in its title, and opens like any run link.
function renderAutoDrainHead(payload) {
  const live = autoDrainLiveWindow(payload);
  const capacity = payload.capacity || {};
  const phase = capacity.drain_phase || (live.runId && !live.admissionsStopped ? "draining" : "idle");
  const running = Number(capacity.running_admitted_workers) || 0;
  const label = phase === "draining" ? "Draining"
    : phase === "winding_down" ? "Winding down" : "idle";
  updateDrainIndicators(phase, label);
  const card = $("auto-drain-panel");
  if (card) card.dataset.drainState = phase;
  const dot = $("auto-drain-dot");
  if (dot) dot.className = `drain-dot ${phase}`;
  const head = $("auto-drain-live");
  if (!head) return;
  head.textContent = "";
  if (phase === "idle" && live.pullRunId && !workspaceReadOnlyReason()) {
    head.appendChild(el("strong", { class: "drain-state-label", text: live.pullAdmissionsStopped ? "Pull drain · admissions stopped" : "Pull drain" }));
    const detail = el("span", { class: "drain-live-detail" });
    detail.appendChild(runLink(live.pullRunId, selectedWorkspace()?.id, autoDrainShortRunId(live.pullRunId)));
    head.appendChild(detail);
  } else if (phase === "idle") {
    head.appendChild(el("span", { class: "drain-idle", text: workspaceReadOnlyReason() ? "read-only" : "idle" }));
  } else {
    head.appendChild(el("strong", { class: "drain-state-label", text: label }));
    const runId = capacity.drain_status_run_id || live.runId;
    const detail = el("span", { class: "drain-live-detail" });
    if (runId) detail.appendChild(runLink(runId, selectedWorkspace()?.id, autoDrainShortRunId(runId)));
    if (phase === "draining") {
      const left = autoDrainTimeLeft(capacity.ends_at);
      if (left) detail.appendChild(el("span", { class: "drain-left", text: ` · ${left}` }));
    }
    head.appendChild(detail);
    if (capacity.admitted_workers != null) head.appendChild(el("span", {
      class: "drain-window-count",
      text: `This window: ${running} running of ${capacity.admitted_workers} admitted`,
    }));
    const approvals = autoDrainApprovalsLine(payload.approvals);
    if (approvals) head.appendChild(approvals);
  }
  const stateKey = `${phase}:${phase === "winding_down" ? running : ""}`;
  if (announcedDrainState !== null && announcedDrainState !== stateKey && !holdDrainAnnouncement) {
    feedback("auto-drain-operation-feedback", "", `Auto-drain ${label}${phase === "winding_down" ? ` · this window: ${running} still running` : ""}.`);
  }
  announcedDrainState = stateKey;
}

// Whether the live window was started with approve-proposed, and what its
// passes have done so far. The payload's `approvals` reports the status
// drain's own input: `approved_total` counts the whole window, while the
// held figures describe the latest pass, with each task's reason in the title.
function autoDrainApprovalsLine(approvals) {
  if (!approvals || approvals.enabled !== true) return null;
  const approved = Number(approvals.approved_total);
  const held = Number(approvals.held_total);
  const parts = ["Approving proposed tasks"];
  if (Number.isFinite(approved)) parts.push(`${approved} approved`);
  if (Number.isFinite(held)) parts.push(`${held} held`);
  const reasons = Object.entries(approvals.held_by_reason || {})
    .map(([reason, count]) => `${count} × ${(reason || "unclassified").replaceAll("_", " ")}`);
  const heldTasks = (Array.isArray(approvals.held) ? approvals.held : [])
    .map((task) => `${task.task_id}: ${(task.reason || "unclassified").replaceAll("_", " ")}`);
  const title = [
    AUTO_DRAIN_APPROVE_RULE,
    reasons.length ? `Held in the latest pass: ${reasons.join(", ")}.` : "",
    heldTasks.join("\n"),
  ].filter(Boolean).join("\n");
  return el("span", { class: "drain-window-count drain-approvals", text: parts.join(" · "), title });
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
  return el("div", { class: "drain-field" }, [
    label,
    el("div", { class: "drain-stepper" }, [step(-1, "Decrease"), input, step(1, "Increase")]),
  ]);
}

// Completion is a choice between two outcomes, so it reads as two named
// options rather than a checkbox whose label describes its current state.
// "Mark done" turns amber, in place of a separate warning banner.
function autoDrainCompletionControl(payload, reasons) {
  const group = el("div", { class: "drain-completion" });
  group.setAttribute("role", "radiogroup");
  group.setAttribute("aria-labelledby", "auto-drain-completion-label");
  const option = (value, text, title) => {
    const input = el("input");
    input.type = "radio";
    input.name = "auto-drain-completion";
    input.value = value;
    input.checked = autoDrainComplete === (value === "done");
    input.disabled = value === "done" && Boolean(reasons.complete);
    input.dataset.drainFocus = `complete-${value}`;
    input.addEventListener("change", () => {
      if (!input.checked) return;
      autoDrainComplete = value === "done";
      renderAutoDrain(payload);
    });
    return el("label", {
      class: `drain-completion-option${input.checked ? " selected" : ""}${value === "done" ? " done" : ""}`,
      title: input.disabled ? reasons.complete : title,
    }, [input, el("span", { text })]);
  };
  group.append(
    option("review", "Stop at review", "Shipped tasks stay in review; a separate action completes them."),
    option("done", "Mark done", AUTO_DRAIN_COMPLETE_WARNING),
  );
  const label = el("span", { class: "drain-field-label", text: "When a task finishes" });
  label.id = "auto-drain-completion-label";
  return el("div", { class: "drain-field drain-field-complete" }, [label, group]);
}

// Approving proposed tasks is a second two-option choice beside completion,
// in the same segmented style; the amber option is the one that acts on work
// nobody has looked at yet. Its reason is visible text, not only a tooltip.
function autoDrainApproveControl(payload, reasons) {
  const group = el("div", { class: "drain-completion" });
  group.setAttribute("role", "radiogroup");
  group.setAttribute("aria-labelledby", "auto-drain-approve-label");
  const option = (value, text, title) => {
    const input = el("input");
    input.type = "radio";
    input.name = "auto-drain-approve";
    input.value = value;
    input.checked = autoDrainApproveProposed === (value === "approve");
    input.disabled = value === "approve" && Boolean(reasons.approve);
    input.dataset.drainFocus = `approve-${value}`;
    input.addEventListener("change", () => {
      if (!input.checked) return;
      autoDrainApproveProposed = value === "approve";
      renderAutoDrain(payload);
    });
    return el("label", {
      class: `drain-completion-option${input.checked ? " selected" : ""}${value === "approve" ? " done" : ""}`,
      title: input.disabled ? reasons.approve : title,
    }, [input, el("span", { text })]);
  };
  group.append(
    option("leave", "Leave for me", "Proposed tasks stay proposed until you approve them."),
    option("approve", "Approve qualifying", AUTO_DRAIN_APPROVE_RULE),
  );
  const label = el("span", { class: "drain-field-label", text: "Proposed tasks" });
  label.id = "auto-drain-approve-label";
  const field = el("div", { class: "drain-field drain-field-approve" }, [label, group]);
  if (reasons.approve && !reasons.submit) {
    field.appendChild(el("div", { class: "operation-control-note drain-stop-note disabled-reason", text: reasons.approve }));
  }
  return field;
}

// Why Start is off, in one place so typing in the concurrency field can
// re-evaluate it without a rebuild.
function autoDrainStartBlocked(payload) {
  const reasons = autoDrainReasons(payload);
  return Boolean(reasons.submit)
    || pendingOperations.has("auto-drain:start")
    || !autoDrainConcurrencyValid()
    || (autoDrainComplete && Boolean(reasons.complete))
    || (autoDrainApproveProposed && Boolean(reasons.approve));
}

function autoDrainStartButton(payload, form) {
  const key = "auto-drain:start";
  const reasons = autoDrainReasons(payload);
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: `operation-button drain-start${payload.capacity?.drain_phase === "draining" ? "" : " primary"}`,
    text: pending ? "Starting…" : `${payload.capacity?.drain_phase === "draining" ? "Start another" : "Start"} ${autoDrainDuration} window`,
    title: reasons.submit || "Submit orbit.workflow.auto with this duration and concurrency",
  });
  button.type = "button";
  button.dataset.drainFocus = "start";
  button.disabled = autoDrainStartBlocked(payload);
  form.start = button;
  form.syncConcurrency?.();
  button.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    if (pendingOperations.has(key) || !autoDrainConcurrencyValid()) return;
    const workspace = selectedWorkspace();
    const counts = autoDrainCounts(payload);
    const duration = autoDrainDuration;
    const completeLine = autoDrainComplete
      ? `WARNING: ${AUTO_DRAIN_COMPLETE_WARNING}`
      : "Shipped tasks stay in review; a separate action completes them.";
    const approveProposed = autoDrainApproveProposed;
    const confirmText = [
      `Start a bounded auto-delivery window in workspace "${workspace?.name || workspace?.id}"?`,
      `Duration: ${duration} · Concurrency: ${autoDrainConcurrency || "runtime default"}`,
      `Currently eligible: ${counts.eligible} · waiting: ${counts.waiting}`,
      "",
      completeLine,
      ...(approveProposed ? [`${AUTO_DRAIN_APPROVE_WARNING} ${AUTO_DRAIN_APPROVE_RULE}`] : []),
    ].join("\n");
    if (!window.confirm(confirmText)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", `Starting a ${duration} auto-delivery window…`);
    renderAutoDrain(payload);
    try {
      const body = { for_duration: duration, complete: autoDrainComplete, approve_proposed: approveProposed };
      if (autoDrainConcurrency) body.concurrency = Number(autoDrainConcurrency);
      const result = await postJson(visit.path("/api/workflows/auto"), body);
      if (!visit.isCurrent()) return;
      const runId = result?.run_id ?? null;
      const state = result?.state ?? "submitted";
      const completion = result?.completion ?? "review";
      feedback("auto-drain-operation-feedback", "success", `Run ${runId ?? "(no run id)"} ${state} (completion: ${completion}).${result?.approve_proposed === true ? " Approving qualifying proposed tasks." : ""}`);
      await refreshDrainAfterAction();
    } catch (error) {
      if (visit.isCurrent()) feedback("auto-drain-operation-feedback", "error", `Auto-delivery window failed to start: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastAutoDrain) renderAutoDrain(lastAutoDrain);
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
const AUTO_DRAIN_SETTLE_CONFIRM = "Deliver the settlements this workspace has recorded but not yet delivered? No auto-delivery window needs to be live. Nothing is cancelled; running workers are left alone.";

// [ORB-12728] Counterpart to `orbit run auto --stop`: stops new admissions on
// the live coordinator without cancelling it or the workers it already
// admitted, then runs the settle-only pass that delivers recorded settlements
// [ORB-13663]. That pass needs no live window, so with none (or with
// admissions already stopped) the button stays usable as "Settle pending" —
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
      ? "Stop starting new tasks in this window. Tasks already started keep running; send any completed results to their owner."
      : "Stop starting new tasks on this replica. Tasks already started keep running; send any completed results to their owner.";
    return { label: "Stop", busy: "Stopping…", aria: "Stop admissions", confirm, description };
  }
  const stoppedBy = (stop) => (stop?.actor ? ` (by ${stop.actor})` : "");
  const stopped = [];
  if (live.runId) stopped.push(`${live.runId}${stoppedBy(live.stop)}`);
  if (live.pullRunId) stopped.push(`pull drain ${live.pullRunId}${stoppedBy(live.pullStop)}`);
  const already = stopped.length
    ? `Admissions are already stopped for ${stopped.join(" and ")}. `
    : "No auto-delivery window is live. ";
  return { label: "Settle pending", busy: "Settling…", aria: "Settle pending settlements", confirm: AUTO_DRAIN_SETTLE_CONFIRM, description: `${already}Deliver settlements recorded for finished or cancelled drains.` };
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
    class: "operation-button drain-stop",
    text: pending ? copy.busy : copy.label,
    title: reasons.stop || copy.description,
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
    feedback("auto-drain-operation-feedback", "pending", autoDrainStopMode(payload) === "stop" ? `Stopping admissions for ${targets.join(" and ")}…` : "Delivering recorded settlements…");
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
  const plural = (n, word) => `${n} ${word}${n === 1 ? "" : "s"}`;
  let summary;
  if (capacity.host_shutdown) {
    summary = "New tasks cannot start while a host shutdown is scheduled.";
  } else if (capacity.resource_throttle) {
    const resources = Array.isArray(capacity.resource_throttle.resources)
      ? capacity.resource_throttle.resources.map(item => item.resource).join(", ") : "";
    summary = `New tasks cannot start until the host resource throttle clears${resources ? ` (${resources})` : ""}.`;
  } else if (capacity.admissions_stopped === true) {
    summary = "This window has stopped starting new tasks. Tasks already admitted keep running.";
  } else if (Number.isFinite(free) && free <= 0 && known && busy >= limit) {
    const toClear = busy - limit + 1;
    summary = `The workspace leaf limit is reached; ${plural(toClear, "occupied slot")} must clear before another task can start.`;
  } else if (Number.isFinite(free) && free <= 0) {
    summary = "No admissions are available in this readiness snapshot.";
  } else if (counts.eligible === 0) {
    if (blocked > 0) {
      summary = `No tasks are eligible yet; ${plural(blocked, "pool task")} ${blocked === 1 ? "waits" : "wait"} on task locks or live claims.`;
    } else {
      summary = counts.waiting > 0
      ? `Nothing is eligible yet; ${plural(counts.waiting, "task")} ${counts.waiting === 1 ? "waits" : "wait"} in the pool.`
      : "Nothing is waiting in the backlog.";
    }
  } else {
    summary = `A window started now admits up to ${plural(admits, "task")}.`;
  }
  const head = el("div", { class: "drain-capacity-head" }, [
    el("span", { class: "drain-capacity-count", text: known ? `Workspace: ${busy} of ${limit} leaf slots in use` : "Workspace: capacity unknown" }),
    el("span", {
      class: `drain-capacity-free${Number.isFinite(free) && free <= 0 ? " full" : ""}`,
      text: Number.isFinite(free) ? `${plural(Math.max(0, free), "free slot")}` : "",
    }),
  ]);
  const bar = el("div", { class: "drain-capacity-bar" });
  bar.setAttribute("role", "img");
  bar.setAttribute("aria-label", known ? `Workspace: ${busy} of ${limit} leaf slots in use` : "Workspace: capacity unknown");
  if (known) {
    const within = Math.min(busy, limit);
    const over = Math.max(0, busy - limit);
    const scale = Math.max(limit, busy);
    bar.append(
      el("span", { class: "drain-capacity-used", style: { width: `${(within / scale) * 100}%` } }),
      el("span", { class: "drain-capacity-over", style: { width: `${(over / scale) * 100}%` } }),
    );
  }
  const result = el("div", { class: "drain-capacity" }, [head, bar, el("p", { class: "drain-slots", text: summary })]);
  const pipelineCounts = Object.entries(capacity.leaf_occupancy_by_pipeline || {})
    .filter(([, count]) => Number(count) > 0);
  const pipelines = pipelineCounts.map(([pipeline, count]) => `${pipeline}: ${plural(count, "slot")}`);
  // Legacy wrappers can occupy slots without appearing in the pipeline map.
  const other = busy - pipelineCounts.reduce((sum, [, count]) => sum + Number(count), 0);
  if (pipelines.length && other > 0) pipelines.push(`other: ${plural(other, "slot")}`);
  if (pipelines.length) result.appendChild(el("p", { class: "drain-slots drain-pipeline-occupancy", text: `Workspace slots by pipeline: ${pipelines.join("; ")}.` }));
  return result;
}

function autoDrainStat(tone, label, value, detail = "") {
  return el("div", { class: `drain-stat ${tone}${value > 0 ? " nonzero" : ""}` }, [
    el("span", { class: "drain-stat-label", title: detail ? `${label} (${detail})` : label }, [el("span", { class: "drain-stat-dot" }), el("span", { text: label })]),
    el("span", { class: "drain-stat-value mono", text: String(value) }),
  ]);
}

// One line per pool task waiting on another task or run: who waits on whom, the holder's
// slot phase, and the lock between them. Capped so the card stays a card;
// Locked files below carries the full per-task lock picture.
function autoDrainBlockedList(tasks, occupancy, workspace) {
  const phases = new Map();
  for (const run of Array.isArray(occupancy?.runs) ? occupancy.runs : []) {
    for (const taskId of Array.isArray(run?.task_ids) ? run.task_ids : []) {
      if (run.phase) phases.set(taskId, String(run.phase).replaceAll("_", " "));
    }
  }
  const list = el("ul", { class: "drain-blocked" });
  list.setAttribute("aria-label", "Pool tasks waiting on locks or live claims");
  for (const task of tasks.slice(0, AUTO_DRAIN_BLOCKED_ROWS)) {
    const taskId = autoDrainTaskId(task);
    const holder = autoDrainHolders(task)[0] || null;
    const runIds = Array.isArray(task.run_ids) ? task.run_ids : [];
    const on = holder
      ? taskLink(holder, workspace?.id)
      : runIds[0]
        ? runLink(runIds[0], workspace?.id, autoDrainShortRunId(runIds[0]))
        : el("span", { text: "holder not supplied" });
    const state = holder ? phases.get(holder) || "no slot" : runIds[0] ? "live run" : "";
    const selectors = autoDrainConflictSelectors(task, holder);
    const lock = selectors.length > 0
      ? `lock · ${autoDrainShortSelector(selectors[0])}${selectors.length > 1 ? ` +${selectors.length - 1}` : ""}`
      : autoDrainReason(task).replaceAll("_", " ");
    list.appendChild(el("li", { class: "drain-blocked-row" }, [
      el("div", { class: "drain-blocked-line" }, [
        el("span", { class: "drain-blocked-who mono" }, [
          taskId ? taskLink(taskId, workspace?.id) : el("span", { text: "task not supplied" }),
          el("span", { class: "drain-muted", text: " waits on " }),
          on,
        ]),
        el("span", { class: `drain-blocked-state mono${phases.has(holder) ? " running" : ""}`, text: state }),
      ]),
      el("div", { class: "drain-blocked-lock mono", text: lock, title: selectors.join("\n") || autoDrainReason(task) }),
    ]));
  }
  if (tasks.length > AUTO_DRAIN_BLOCKED_ROWS) {
    list.appendChild(el("li", { class: "drain-blocked-more mono", text: `+${tasks.length - AUTO_DRAIN_BLOCKED_ROWS} more` }));
  }
  return list;
}

// A task whose `os:` tags this host's OS does not satisfy waits for a host of
// its own (a pull-drain follower on that OS, say). Readiness names the wait,
// and the card lists it so an idle drain does not read as an empty backlog.
function autoDrainHostWaits(payload) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  return tasks.filter((task) => task.eligible !== true && autoDrainReason(task) === "host_os_mismatch");
}

function autoDrainHostWaitList(tasks, workspace) {
  const list = el("ul", { class: "drain-blocked drain-host-waits" });
  list.setAttribute("aria-label", "Tasks waiting for a host of another OS");
  for (const task of tasks.slice(0, AUTO_DRAIN_BLOCKED_ROWS)) {
    const taskId = autoDrainTaskId(task);
    list.appendChild(el("li", { class: "drain-blocked-row" }, [
      el("div", { class: "drain-blocked-line" }, [
        el("span", { class: "drain-blocked-who mono" }, [
          taskId ? taskLink(taskId, workspace?.id) : el("span", { text: "task not supplied" }),
          el("span", { class: "drain-muted", text: ` ${typeof task.detail === "string" && task.detail ? task.detail : "waits for a host of another OS"}` }),
        ]),
      ]),
    ]));
  }
  if (tasks.length > AUTO_DRAIN_BLOCKED_ROWS) {
    list.appendChild(el("li", { class: "drain-blocked-more mono", text: `+${tasks.length - AUTO_DRAIN_BLOCKED_ROWS} more` }));
  }
  return list;
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

// Sustained host pressure holds every new admission until it clears, so the
// card names the resource, value, threshold and since-when instead of leaving
// an idle drain to read as an empty backlog.
function autoDrainThrottleNote(capacity) {
  const resources = capacity?.resource_throttle?.resources;
  if (!Array.isArray(resources) || resources.length === 0) return null;
  const held = resources
    .map(item => `${throttleReading(item.resource, item.percent)} (throttled at ≥ ${throttleReading(item.resource, item.high_percent, true)} since ${time(item.since)}; resumes below ${throttleReading(item.resource, item.resume_percent, true)})`)
    .join("; ");
  const note = el("p", { class: "operation-control-note drain-throttle-note", text: `Admissions throttled: ${held}. Running tasks are not touched.` });
  note.setAttribute("role", "status");
  return note;
}

function renderAutoDrain(payload) {
  lastAutoDrain = payload;
  const body = $("auto-drain-body");
  if (!body) return;
  // A poll or a control click rebuilds the card; keep keyboard focus on the
  // control that had it.
  const focusKey = document.activeElement?.dataset?.drainFocus;
  body.textContent = "";
  renderAutoDrainHead(payload);
  const throttle = autoDrainThrottleNote(payload.capacity);
  if (throttle) body.appendChild(throttle);
  const reasons = autoDrainReasons(payload);
  if (reasons.submit) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: reasons.submit }));
    return;
  }
  const counts = autoDrainCounts(payload);
  const blocked = autoDrainBlocked(payload);
  // Read top to bottom: what the queue looks like now, then the window you
  // could start against it.
  body.append(
    autoDrainCapacity(payload.capacity || {}, counts, blocked.length),
    el("div", { class: "drain-stats" }, autoDrainPool(payload).map(group => autoDrainStat(
      group.key, group.label, group.count,
      group.key === "eligible" ? "" : [...group.reasons].map(([reason, n]) => `${reason} ${n}`).join(", "),
    ))),
  );
  if (blocked.length > 0) body.appendChild(autoDrainBlockedList(blocked, payload.capacity?.occupancy, selectedWorkspace()));
  const hostWaits = autoDrainHostWaits(payload);
  if (hostWaits.length > 0) body.appendChild(autoDrainHostWaitList(hostWaits, selectedWorkspace()));
  const durationLabel = el("span", { class: "drain-field-label", text: "Window length" });
  // The concurrency field and the Start button are built apart but depend on
  // each other; they meet here.
  const form = {};
  body.append(
    el("div", { class: "drain-form" }, [
      durationLabel,
      autoDrainDurationControl(payload),
      el("div", { class: "drain-settings" }, [autoDrainConcurrencyControl(payload, form), autoDrainCompletionControl(payload, reasons), autoDrainApproveControl(payload, reasons)]),
      form.problem,
      el("div", { class: "drain-actions" }, [autoDrainStartButton(payload, form), autoDrainStopButton(payload)]),
      autoDrainStopNote(payload),
    ]),
  );
  if (focusKey) body.querySelector?.(`[data-drain-focus="${focusKey}"]`)?.focus();
}

function fetchAndRenderAutoDrain() {
  const workspace = selectedWorkspace();
  if (!workspace) {
    return requestPanel("auto-drain-body", "unselected", () => Promise.resolve({}), renderAutoDrain, "auto-drain-live");
  }
  const query = autoDrainConcurrency && autoDrainConcurrencyValid() ? `?concurrency=${encodeURIComponent(autoDrainConcurrency)}` : "";
  const scope = "/api/workflows/auto/readiness";
  const path = `${scope}${query}`;
  return requestPanel("auto-drain-body", scope, () => fetchJson(path), renderAutoDrain, "auto-drain-live");
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
  if (subtab === "jobs") {
    await fetchAndRenderJobs(routines);
    return;
  }
  throwFirstPanelError(await Promise.allSettled([
    requestPanel("routines-body", "routines", () => routines, renderOperations, "routines-count"),
    requestPanel("clock-body", "clock", () => routines, renderClock, "clock-host"),
  ]));
}

// The Tasks dock's Drain card refreshes with the Tasks tab.
export async function fetchAndRenderAutoDrainPane() {
  await fetchAndRenderAutoDrain();
}
