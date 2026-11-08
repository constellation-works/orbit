// Routine-definition and host clock operations [ORB-10875].

import { captureWorkspaceVisit, el, fetchJson, getWorkspace, isAggregateView, postJson, relativeTime } from './common.js';
import { conditionStateLabel, renderAutomation } from './automation.js';
import { closeEditors, editorObserved, isOpen, openEditor, routineDraft, routineTriggerEditor, scheduleEditButton } from './schedule-editor.js';

import {
  $,
  cadenceText,
  clockHm,
  clockServiceText,
  clockTickText,
  clockUnavailable,
  clockUnavailableReason,
  controlReason,
  cronDraftText,
  cronText,
  durationText,
  explainUnavailable,
  feedback,
  field,
  inactivePluginGroup,
  inactivePluginToggle,
  nextEvaluationText,
  operationCell,
  operationColumns,
  operationDetails,
  operationFact,
  operationGroup,
  operationSwitch,
  operationsState,
  outcomeDot,
  pendingOperations,
  registerOperationsReset,
  routineEventDriven,
  routineScheduleText,
  runLink,
  runOperation,
  selectedWorkspaceName,
  selectionSnapshot,
  time,
  whenCell
} from './operations-shared.js';

let routinesRenderOperations = null;
let routinesFetchOperations = null;

export function configureOperationsRoutines(callbacks) {
  routinesRenderOperations = callbacks.renderOperations;
  routinesFetchOperations = callbacks.fetchOperations;
}

let routineFailureRequest = 0;
// The operator's unapplied cadence choice, held outside the rebuilt <select>
// so a background refresh cannot revert it. Keyed by host (machine_name), like
// the clock itself: a draft made on one host never shows on another, while a
// workspace switch on the same host keeps it.
const pendingCadenceByHost = new Map();
let timelineResizeObserver = null;
let observedTimelineBody = null;
const routineList = { query: "", pausedOpen: false };

// ---------------------------------------------------------------------------
// Routines
registerOperationsReset(() => {
  closeEditors("routine");
  updateRoutineFailureBadge([]);
  Object.assign(routineList, { query: "", pausedOpen: false });
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
      render: () => { if (operationsState.lastOperations && routinesRenderOperations) routinesRenderOperations(operationsState.lastOperations); },
      request: () => postJson("/api/routines/toggle", {
        name: routine.name, source: routine.source, target: routine.target,
        machine_name: payload.machine_name, expected_enabled: routine.enabled, enabled: nextEnabled,
      }),
      refresh: () => routinesFetchOperations(),
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

export function jobIdFromTarget(target) {
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

const cronZoneFormatters = new Map();
const CRON_WEEKDAYS = { Sun: 0, Mon: 1, Tue: 2, Wed: 3, Thu: 4, Fri: 5, Sat: 6 };

// Wall-clock fields of an instant in the host's IANA zone, or null when this
// browser cannot resolve that zone. Seasonal offsets come from Intl, so a
// window crossing a DST change is read in the offset in force at each minute,
// not the host's current offset.
function cronLocalParts(at, zoneName) {
  if (!zoneName) return null;
  try {
    let formatter = cronZoneFormatters.get(zoneName);
    if (!formatter) {
      formatter = new Intl.DateTimeFormat("en-US", {
        timeZone: zoneName, hourCycle: "h23",
        year: "numeric", month: "numeric", day: "numeric",
        hour: "numeric", minute: "numeric", weekday: "short",
      });
      cronZoneFormatters.set(zoneName, formatter);
    }
    const parts = {};
    for (const part of formatter.formatToParts(new Date(at))) parts[part.type] = part.value;
    const weekday = CRON_WEEKDAYS[parts.weekday];
    if (weekday === undefined) return null;
    return {
      minute: Number(parts.minute), hour: Number(parts.hour) % 24,
      date: Number(parts.day), month: Number(parts.month), weekday,
    };
  } catch {
    return null;
  }
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
  const zoneName = operationsState.hostCronZone?.name || null;
  const zoned = zoneName ? cronLocalParts(first, zoneName) : null;
  const offset = Number(operationsState.hostCronZone?.offset_seconds);
  if (matchers.includes(null) || (!zoned && !Number.isFinite(offset) && fields.slice(1).some((f) => f !== "*"))) return [first];
  const [minute, hour, dom, month, dow] = matchers;
  const zoneOffset = Number.isFinite(offset) ? offset * 1000 : 0;
  const times = [first];
  for (let at = Math.floor(first / 60_000) * 60_000 + 60_000; at <= until; at += 60_000) {
    const local = zoned ? cronLocalParts(at, zoneName) : null;
    const fieldsAt = local || (() => {
      const shifted = new Date(at + zoneOffset);
      return {
        minute: shifted.getUTCMinutes(), hour: shifted.getUTCHours(),
        date: shifted.getUTCDate(), month: shifted.getUTCMonth() + 1, weekday: shifted.getUTCDay(),
      };
    })();
    if (!fieldsAt) continue;
    const weekday = fieldsAt.weekday;
    // Day-of-month and day-of-week combine with OR unless one is a wildcard.
    const days = fields[2] !== "*" && fields[4] !== "*"
      ? dom(fieldsAt.date) || dow(weekday) || (weekday === 0 && dow(7))
      : dom(fieldsAt.date) && (dow(weekday) || (weekday === 0 && dow(7)));
    if (minute(fieldsAt.minute) && hour(fieldsAt.hour) && month(fieldsAt.month) && days) times.push(at);
  }
  return times;
}

// Measure the real label boxes after rendering; their font metrics and track width vary by viewport.
function layoutRoutineTimelineLabels(track) {
  const width = track.clientWidth;
  if (width <= 0) return;

  const labels = Array.from(track.querySelectorAll(".operation-timeline-label"));
  for (const label of labels) label.style.maxWidth = `${width}px`;

  const placements = labels.map((label) => {
    const tick = label.closest(".operation-timeline-tick");
    const tickPosition = ((Number.parseFloat(tick.style.left) || 0) / 100) * width;
    const labelWidth = label.getBoundingClientRect().width;
    const center = Math.min(width - labelWidth / 2, Math.max(labelWidth / 2, tickPosition));
    return { label, labelWidth, labelHeight: label.getBoundingClientRect().height, tickPosition, center };
  }).sort((a, b) => a.center - b.center);

  const rows = [];
  for (const placement of placements) {
    const left = placement.center - placement.labelWidth / 2;
    const right = placement.center + placement.labelWidth / 2;
    let row = rows.findIndex((placed) => placed.intervals.every((interval) => right + 4 <= interval.left || interval.right + 4 <= left));
    if (row < 0) row = rows.push({ intervals: [], placements: [], height: 0 }) - 1;
    rows[row].intervals.push({ left, right });
    rows[row].placements.push(placement);
    rows[row].height = Math.max(rows[row].height, placement.labelHeight);
    placement.label.style.marginTop = "";
    placement.label.style.transform = `translateX(${placement.center - placement.tickPosition}px)`;
  }

  let rowTop = 0;
  for (const row of rows) {
    for (const placement of row.placements) placement.label.style.marginTop = `${rowTop}px`;
    rowTop += row.height + 4;
  }
  track.style.height = `${Math.max(39, 18 + rowTop)}px`;
  track.dataset.labelLayoutWidth = String(width);
}

function watchRoutineTimelineSize(track) {
  const body = $("routines-body");
  if (typeof ResizeObserver === "function" && body && observedTimelineBody !== body) {
    timelineResizeObserver ||= new ResizeObserver(() => {
      const current = body.querySelector(".operation-timeline-track");
      if (current) layoutRoutineTimelineLabels(current);
    });
    timelineResizeObserver.observe(body);
    observedTimelineBody = body;
  }
  if (typeof requestAnimationFrame === "function") requestAnimationFrame(() => layoutRoutineTimelineLabels(track));
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
  // Label rows are assigned from their rendered pixel widths after the track
  // enters the DOM. Repeated fires keep their tick marks but do not add labels.
  due.forEach(({ routine, at, repeat }) => {
    const pct = Math.min(100, Math.max(0, ((at - now) / horizon) * 100));
    const fires = routine.enabled && routine.effective !== false;
    const label = repeat ? null : el("span", { class: "operation-timeline-label mono", text: routine.name });
    const tick = el("div", { class: `operation-timeline-tick ${fires ? "fires" : "skipped"}` }, [
      el("span", { class: "operation-timeline-mark" }),
      label,
    ]);
    tick.style.left = `${pct}%`;
    tick.title = `${routine.name} · ${clockHm(at)}${fires ? "" : " · paused"}`;
    track.appendChild(tick);
  });
  watchRoutineTimelineSize(track);
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

export function scopedRoutines(payload) {
  const workspace = selectedWorkspaceName();
  return workspace ? (payload.routines || []).filter(routine => routine.source === workspace) : [];
}

export function updateRoutineFailureBadge(routines) {
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
export async function peekRoutineFailures(read = null) {
  if (isAggregateView() || !selectedWorkspaceName()) {
    updateRoutineFailureBadge([]);
    return;
  }
  const visit = captureWorkspaceVisit();
  const request = ++routineFailureRequest;
  try {
    const payload = await (read || fetchJson("/api/routines"));
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
      if (routinesRenderOperations) routinesRenderOperations(operationsState.lastOperations || payload, { force: true });
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
    cronPreview: (cron) => cronDraftText(cron, routine.cron || routine.trigger?.cron, routineNextAt(routine)),
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
      if (saved) routinesFetchOperations().catch(() => {});
      else if (operationsState.lastOperations && routinesRenderOperations) routinesRenderOperations(operationsState.lastOperations, { force: true });
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
    routine.automation?.condition?.stall ? el("span", { class: "operation-state blocked", text: "stalled" }) : null,
  ]);
  const condition = routine.automation?.condition;
  const cadence = routineEventDriven(routine)
    ? [
      el("span", { class: "operation-cell-main", text: routineScheduleText(routine) }),
      condition ? el("span", { class: "operation-cell-sub", text: conditionStateLabel(condition) }) : null,
    ]
    : (() => {
      const cadenceLabel = cronText(routine.cron, nextAt);
      return [
        el("span", { class: "operation-cell-main", text: cadenceLabel }),
        routine.cron && cadenceLabel !== routine.cron ? el("span", { class: "operation-cell-sub mono", text: routine.cron }) : null,
      ];
    })();
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


// List navigation is local operator state, retained when fresh status arrives.
// Only the list is filtered: health warnings and the timeline always use every routine.
function routineCatalogue(payload, routines, workspaceId) {
  const catalogue = el("div", { class: "routine-catalogue" });
  const search = el("input", { id: "routine-filter", type: "search" });
  search.dataset.routineFocus = "filter";
  search.placeholder = "Name, job or description";
  search.value = routineList.query;
  const label = el("label", { text: "Filter routines" });
  label.htmlFor = search.id;
  const clear = el("button", { class: "operation-button", type: "button", text: "Clear" });
  clear.dataset.routineFocus = "clear";
  const count = el("span", { class: "routine-list-count", role: "status" });
  const groups = el("div", { id: "routine-list-groups" });
  search.setAttribute("aria-controls", groups.id);
  catalogue.append(el("div", { class: "routine-list-toolbar" }, [label, search, clear, count]), routineTimeline(routines), groups);

  // Filtering is refused only while an editor is on screen: an open
  // editor whose routine is filtered out has nothing to
  // close here, so the controls stay usable to navigate back to it. Every
  // repaint re-derives the state, so filtering back to an open editor
  // disables the controls again.
  // Focus the first enabled, visible control of the open editor. A cron-only
  // editor disables its Trigger select, and a pending save disables every
  // control, so with none available focus the editor itself, not the page.
  // A filter keystroke must focus only the panel: later query characters must
  // not type into a cron input or activate a Trigger select's type-ahead.
  const focusEditor = ({ focusControl = true } = {}) => {
    const editor = groups.querySelector(".schedule-editor");
    if (!editor) return;
    const control = [...editor.querySelectorAll("input, select, button")]
      .find((node) => !node.disabled && node.checkVisibility());
    if (focusControl && control) control.focus();
    else {
      editor.tabIndex = -1;
      editor.focus();
    }
  };
  const paintGroups = () => {
    groups.textContent = "";
    const query = routineList.query.trim().toLocaleLowerCase();
    const matches = routines.filter(routine => [routine.name, routine.target, routine.description]
      .some(value => String(value || "").toLocaleLowerCase().includes(query)));
    count.textContent = query ? `${matches.length} of ${routines.length} routines match` : `${routines.length} routines`;
    for (const [kind, title, hint] of [
      ["active", "Active", "enabled and delivered by the host clock"],
      ["paused", "Paused", "slots are skipped, not queued"],
    ]) {
      const all = routines.filter(routine => Boolean(routine.enabled) === (kind === "active"));
      if (!all.length) continue;
      const filtered = matches.filter(routine => Boolean(routine.enabled) === (kind === "active"));
      const group = operationGroup(kind, title, query ? `${filtered.length} of ${all.length}` : all.length, hint);
      const states = [
        ["blocked", all.filter(routine => routineState(routine) === "blocked").length],
        ["stalled", all.filter(routine => routine.automation?.condition?.stall).length],
        ["failing repeatedly", failingRoutines(all).length],
      ].filter(([, total]) => total);
      if (states.length) group.firstChild.appendChild(el("span", {
        class: "routine-group-warning", text: `${query ? `Across all ${all.length}: ` : ""}${states.map(([state, total]) => `${total} ${state}`).join(" · ")}`,
      }));
      let container = group;
      if (kind === "paused") {
        container = el("details", { class: "operation-group routine-paused" });
        container.open = routineList.pausedOpen;
        const summary = el("summary", { class: "operation-group-head" });
        summary.dataset.routineFocus = "paused";
        summary.append(...group.firstChild.childNodes);
        container.appendChild(summary);
        container.addEventListener("toggle", () => {
          if (container.isConnected) routineList.pausedOpen = container.open;
        });
      }
      if (filtered.length) {
        container.append(operationColumns(["", "Routine", "Cadence", "Next fire", "Last run"]));
        for (const routine of filtered) container.appendChild(routineRow(payload, routine, workspaceId));
      } else {
        container.appendChild(el("p", { class: "routine-list-empty", text: `No ${kind} routines match. Clear or change the filter.` }));
      }
      groups.appendChild(container);
    }
    const editing = Boolean(groups.querySelector(".schedule-editor"));
    search.disabled = editing;
    search.title = editing ? "Close the trigger editor to filter routines." : "";
    clear.disabled = editing || !routineList.query;
  };
  search.addEventListener("input", () => {
    routineList.query = search.value;
    if (routineList.query.trim()) routineList.pausedOpen = true;
    const typing = document.activeElement === search;
    paintGroups();
    // Revealing an editor locks the filter mid-query. Park focus on its panel
    // until the operator deliberately tabs or clicks into an editing control.
    if (typing && search.disabled) focusEditor({ focusControl: false });
  });
  clear.addEventListener("click", () => {
    search.value = "";
    search.dispatchEvent(new Event("input"));
    if (search.disabled) focusEditor();
    else search.focus();
  });
  paintGroups();
  return catalogue;
}

export function renderRoutineRows(payload, body, { workspace, workspaceId, routines, inactive, ownerOnly }) {
  const focused = body.contains(document.activeElement) ? document.activeElement?.dataset?.routineFocus : null;
  const selection = focused === "filter" ? [document.activeElement.selectionStart, document.activeElement.selectionEnd] : null;
  body.textContent = "";
  if (!workspace) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: `All-workspace mode is read-only. Select one workspace; this machine is already resolved as ${payload.machine_name}.` }));
  }
  if (routines.length === 0 && inactive.length === 0 && ownerOnly.length === 0) {
    body.appendChild(el("div", { class: "empty-state", text: workspace ? "No routines are defined by this workspace." : "Select a workspace to list its routines." }));
  } else if (routines.length) {
    const warning = routineFailureSummary(routines);
    if (warning) body.appendChild(warning);
    body.appendChild(routineCatalogue(payload, routines, workspaceId));
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
  if (focused) {
    const control = body.querySelector(`[data-routine-focus="${focused}"]`);
    control?.focus({ preventScroll: true });
    if (selection) control?.setSelectionRange(...selection);
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
      refresh: () => routinesFetchOperations(),
      success: (result) => `${result.message} on ${payload.machine_name}.`,
    });
  });
  return explainUnavailable(button, reason);
}

// The clock is one host-wide fact that governs every routine, so it reads as
// a status bar above the list rather than a card beside it.
export function renderClock(payload) {
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
  // it clears once its own host reports that value as configured, whoever
  // applied it. Another host's draft is neither shown nor applied here.
  const cadenceHost = payload.machine_name || "";
  let pendingCadenceSeconds = pendingCadenceByHost.get(cadenceHost) ?? null;
  if (pendingCadenceSeconds === clock.configured_cadence_seconds) {
    pendingCadenceByHost.delete(cadenceHost);
    pendingCadenceSeconds = null;
  }
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
    pendingCadenceByHost.set(cadenceHost, pendingCadenceSeconds);
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
      refresh: () => routinesFetchOperations(),
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
