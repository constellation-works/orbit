// Orbit dashboard task-domain rendering and actions.
// Pure vanilla JS, split into ES modules with no build step.

import { captureWorkspaceVisit, findRegisteredHost, getHost, getRegisteredHosts, getServingHost, getWorkspace, onWorkspaceChange, panelCanRender, el, infoHint, fetchJson, patchJson, postJson, syncNodes, teardownNode, isAggregateView, isHttpUrl, withHost, withWorkspace, hostWriteRefusal, makeToggleRow, makeDisclosure, makeRowDisclosure, enableRovingRows, makeCopyButton, copyText, copyWithFeedback, runHref, bindTaskLink, clockStamp, formatClock, formatDate, formatDayHeading, onClockZoneChange, statusPill, submissionNoticeLines, stateCell, fmtDuration, elapsedDurationInfo } from './common.js';
import { renderMarkdown, renderMarkdownInline } from './markdown.js';
import { buildInlineFieldEditor } from './field-editor.js';
import { DRAIN_POOLS, backlogReadiness, drainReadinessEntry, drainReadinessFor, drainReadinessView, drainWaitSignature } from './drain-waits.js';
import { elapsedLabel, onTaskRuntimeChange, runStartedAt, taskContextContenders, taskLockFiles } from './task-runtime.js';
import { buildAttentionCallout, buildDeliveryPanel, buildDeliveryProgress, buildExecutionProvenance, formatExecutionLocation, claimAttention, claimedReviewApproval, deliverySteps, handoffApprovalRequest, invalidateDistributedConsole, lastKnownClaims, registeredMachineHost } from './distributed.js';

const $ = (id) => document.getElementById(id);

let lastCrewPayload = { default_crew: null, crews: [] };
// The list row whose detail the side pane shows. One task at a time: the pane
// beside the list replaces the inline accordion, so selecting another row
// moves the pane to it and the list itself never reflows around a detail.
let selectedTaskId = null;
let taskActionNotice = null;
let pinnedExternalTask = null;
// The task the Tasks hash names with `open=`: the one selected and still
// shown, so a copied URL reopens it. `pendingOpenTaskId` is a hash-named task
// that has not been shown yet; the next render selects it from the list or
// pins it when the operator's filter hides it.
let openTaskId = null;
let pendingOpenTaskId = null;
let openTaskGeneration = 0;
// `#tasks/<id>` shows the pane across the content width. `taskPageReturn`
// remembers the list scroll when the page was entered from the list, so Back
// lands on the same selection at the same place; `listRestore` applies that
// on the next render. `taskPageError` is a page whose task could not be read.
let taskPageId = null;
let taskPageReturn = null;
let taskPageError = null;
let listRestore = null;
let returningFromPage = false;
let focusPaneTitle = false;
// A status change folded the pane: focus goes back to the row it came from.
let rowFocusAfterRender = null;
// The pane's tab is a page-wide choice: moving the selection keeps it.
let activeDetailTab = "overview";
// The Runs tab's read per task (`GET /api/job-runs?task_id=`), kept so a
// rebuilt detail repaints the last answer while a fresh read is in flight.
let taskRunLoads = new Map();
const TASK_RUNS_LIMIT = 20;
const TASK_RUNS_FRESH_MS = 15000;
// The last context a render ran with, for the keyboard and pane handlers that
// are wired once.
let paneContext = null;
let searchDebounce = null;
// ORB-10874: per-task inline-edit feedback for the status/crew selects. Each
// entry is `{ kind: 'pending'|'success'|'error', text, undo }`, where `undo`
// (when present) is `{ previousValue, expiresAt }` — a bounded window in which
// the prior value can still be restored with one click. Cleared automatically
// once that window lapses (see scheduleFeedbackExpiry).
let statusFeedback = new Map();
let crewFeedback = new Map();
// A status change made from a task's open detail folds that detail in: the row
// has moved to its new status group. The change's feedback and undo then show
// on the collapsed row, or in a notice above the list while the new status is
// filtered out, until the feedback expires. Keyed by task id; the value is the
// updated record, which the undo acts on when the list no longer holds the row.
let foldedStatusTasks = new Map();
// ORB-12235: the same feedback shape for the writable fields in the expanded
// detail — the complexity select and the inline text editors. `fieldFeedback`
// is keyed `<task id>:<field>` because one detail can hold several editors.
let complexityFeedback = new Map();
let fieldFeedback = new Map();
const MUTATION_UNDO_WINDOW_MS = 8000;
// A saved text field has no undo (restoring prose would need a second write of
// its own), so its note only has to stay long enough to be read.
const FIELD_SAVED_NOTICE_MS = 4000;
// DANI-10391: a list row is a summary (`projection: "summary"`). It carries what
// the collapsed row renders and filters on, plus counts, but the bodies, the
// review gate, and the evidence a governed status transition needs live behind
// GET /api/tasks/:id. `taskDetails` keeps the last full projection per task so
// an open detail repaints from it across the 30 s refresh instead of flashing a
// placeholder; `taskDetailLoads` records one read per list payload, keyed by
// the summary object itself (every list fetch produces new objects), so a
// render never issues a second read for the same row and a failed read is not
// retried until the list refreshes or the row is reopened.
let taskDetails = new Map();
let taskDetailLoads = new Map();
// Drain readiness for an open proposed or backlog task: why the drain is or is
// not taking it, read per task (`GET /api/workflows/auto/readiness?task=<id>`)
// so the Readiness card answers without the Drain card's snapshot and for a
// task past its bound. One read per summary object, like `taskDetailLoads`; a
// read in flight keeps the last answer on screen.
const DRAIN_READINESS_STATUSES = new Set(["proposed", "backlog"]);
let taskDrainLoads = new Map();
const reviewApprovalsInFlight = new Set();
// ORB-12645: which comments the operator has opened, and which are showing
// their Markdown source instead of the rendered body. Both are keyed
// `<task id>#<index into task.comments>` so a refresh that rebuilds the detail
// restores the thread the way it was left. The thread order is a page-wide
// preference, so it persists like the other dashboard presentation prefs.
let expandedComments = new Set();
let rawComments = new Set();
let commentPrefs = loadCommentPrefs();
let restoredCommentHash = null;

onWorkspaceChange(() => {
  invalidateDistributedConsole();
  pinnedExternalTask = null;
  taskActionNotice = null;
  quickActionState.clear();
  selectedTaskId = null;
  taskRunLoads.clear();
  // The initial workspace selection also lands here; a task the URL named but
  // no render has shown yet must survive it.
  if (!pendingOpenTaskId && openTaskId) {
    taskPageId = null;
    taskPageReturn = null;
    setOpenTask(null);
  }
  statusFeedback.clear();
  foldedStatusTasks.clear();
  crewFeedback.clear();
  complexityFeedback.clear();
  fieldFeedback.clear();
  taskDetails.clear();
  taskDetailLoads.clear();
  taskDrainLoads.clear();
  expandedComments.clear();
  rawComments.clear();
  restoredCommentHash = null;
});

// Guard duplicate clicks while a Ship request is pending. Once the server
// accepts it, the server's 409 protects the live run; releasing this guard lets
// a task returned to backlog be shipped again without reloading the page.
const shipInFlightTaskIds = new Set();
// Run pilot: the pilot run this panel submitted for a task, keyed like the ship
// guard, while it is live. Absent after the run settles or a reload, when the
// row's own `readiness.preparing` marks a held task.
const pilotRunsByTask = new Map();
// A refused Run pilot's message, kept with the action so a refresh that rebuilds
// the panel does not drop it before the operator has read it.
const pilotRefusals = new Map();
const PILOT_REFUSAL_MS = 20000;
const PILOT_POLL_MS = 3000;
const PILOT_POLL_LIMIT = 600;
const PILOT_TERMINAL_STATES = new Set(["success", "failed", "timeout", "cancelled", "interrupted", "held"]);
// Triage: the escalation triage run this panel submitted for a task, keyed
// like the pilot runs, while it is live. Absent after the run settles or a
// reload, when the detail's own `escalation_triage` readout names the run.
const triageRunsByTask = new Map();
// A refused Triage submission's message, kept with the action like a refused
// Run pilot so a refresh that rebuilds the panel does not drop it.
const triageRefusals = new Map();
const TRIAGE_REFUSAL_MS = 20000;
// Fix PR: whether the server would start an out-of-run auto-fix of a review
// task's pull request, keyed like the ship guard. Read once when the panel
// shows the task and again after FIX_PR_READINESS_MS or a start, so a list
// refresh never fans out forge reads.
const fixPrReadiness = new Map();
// A Fix PR start's outcome ({ ok, text }), kept with the action like a refused
// Run pilot so the panel's re-read after the start does not drop it.
const fixPrFeedback = new Map();
const FIX_PR_READINESS_MS = 60000;
const FIX_PR_FEEDBACK_MS = 20000;
// Row shortcuts (Approve on proposed or review tasks, Ship on backlog): the
// pending or failed state of each, keyed by task id, so a refresh repaints it.
let quickActionState = new Map();

function taskDispatchIdentity(task) {
  return `${task.workspace_id || getWorkspace() || ''}:${task.id}`;
}

// Groups read in the order a person has to act on them: tasks waiting on a
// human decision first, then running work, then the queue. The chips keep the
// lifecycle order; only the grouped list is reordered.
const GROUP_ORDER = ["proposed", "review", "blocked", "in-progress", "backlog", "someday", "done", "rejected", "archived"];
const GROUP_LABELS = {
  proposed: "Awaiting approval",
  review: "Ready for review",
  blocked: "Blocked",
  "in-progress": "In progress",
  backlog: "Backlog",
  someday: "Someday",
  done: "Done",
  rejected: "Rejected",
  archived: "Archived",
};
const GROUP_HINTS = {
  proposed: "Proposed tasks never run until they are approved",
  review: "Review the pull request, then approve to close",
  blocked: "Waiting on something the run cannot resolve",
  "in-progress": "Running now",
  backlog: "Approved; Ship or a drain window picks them up once nothing holds them",
};

// Groups whose rows carry where their run is and how long it has been going.
const RUN_GROUPS = new Set(["in-progress", "blocked"]);

// Chip words for each status. The five open statuses are the filter's
// primary chips; the closed ones read as quieter text until they are on.
const STATUS_LABELS = {
  "in-progress": "In progress",
  review: "Review",
  blocked: "Blocked",
  proposed: "Proposed",
  backlog: "Backlog",
  someday: "Someday",
  done: "Done",
  rejected: "Rejected",
  archived: "Archived",
};

// How an empty open group reads in the "Nothing …" line above the cards.
const EMPTY_GROUP_PHRASES = {
  blocked: "blocked",
  "in-progress": "in progress",
  review: "in review",
  proposed: "proposed",
  backlog: "in the backlog",
};

function taskList(context) {
  return context && typeof context.getTasks === "function" ? context.getTasks() : [];
}

function searchQueryValue(context) {
  return context && typeof context.getSearchQuery === "function"
    ? context.getSearchQuery()
    : "";
}

function activeStatusSet(context) {
  return context && typeof context.getActiveStatuses === "function"
    ? context.getActiveStatuses()
    : new Set();
}

function setSearchQuery(context, value) {
  if (context && typeof context.setSearchQuery === "function") context.setSearchQuery(value);
}

function setActiveStatuses(context, statuses) {
  if (context && typeof context.setActiveStatuses === "function") context.setActiveStatuses(statuses);
}

function statusOrder(context) {
  return context && Array.isArray(context.statusOrder) ? context.statusOrder : [];
}

function statusTransitions(task) {
  return Array.isArray(task && task.status_transitions)
    ? task.status_transitions.filter((transition) => transition && transition.status)
    : [];
}

function statusTransition(task, targetStatus) {
  return statusTransitions(task).find((transition) => transition.status === targetStatus) || null;
}

function fmtAbsTimeValue(context, value) {
  return context && typeof context.fmtAbsTime === "function"
    ? context.fmtAbsTime(value)
    : (value || "-");
}

// An absolute time in a task detail. The detail is kept while its task is
// unchanged, so the time is stamped for a UTC/Local switch to repaint.
function absTimeNode(context, value, attrs = {}) {
  return clockStamp(el("span", { ...attrs, text: fmtAbsTimeValue(context, value) }), value);
}

function refreshTasks(context) {
  return context && typeof context.refreshDashboard === "function"
    ? context.refreshDashboard()
    : Promise.resolve();
}

function defaultActiveStatuses(context) {
  return context && Array.isArray(context.defaultActiveStatuses)
    ? context.defaultActiveStatuses
    : statusOrder(context);
}

function tasksMeta(context) {
  return context && typeof context.getTasksMeta === "function" ? context.getTasksMeta() : null;
}

// Explicit page range and matching total instead of the ambiguous `N/50`
// shorthand, which could mean either a total or an unreachable hard cap.
export function formatTaskCount(filteredCount, fetchedCount, meta) {
  if (meta && Number.isFinite(meta.total)) {
    const offset = Number.isFinite(meta.offset) ? meta.offset : 0;
    if (fetchedCount === 0) return `0 of ${meta.total}`;
    const range = `${offset + 1}–${offset + fetchedCount} of ${meta.total}`;
    return filteredCount === fetchedCount ? range : `${filteredCount} shown · page ${range}`;
  }
  return filteredCount === fetchedCount
    ? `${fetchedCount} shown`
    : `${filteredCount} shown of ${fetchedCount} fetched`;
}

// ORB-10874: aggregate ("All workspaces") mode has no ambient workspace to
// scope a mutation to. A task fetched through /api/tasks/all carries its own
// `workspace_id`, so that's an explicit, workspace-qualified target; anything
// else is refused rather than silently applied to the wrong (or no) workspace.
function canMutateTask(task) {
  return (!isAggregateView() || Boolean(task && task.workspace_id)) && !hostWriteRefusal();
}

// One sentence for every inline control that cannot write, so the refusal
// reads the same whichever control the operator hovers: a selected host that
// refuses forwarded writes, or the aggregate view.
function aggregateRefusalTitle(label, action) {
  const host = hostWriteRefusal();
  if (host) return `${label} — ${host}`;
  return `${label} — select a specific workspace to ${action} in aggregate view`;
}

// A row's owner takes precedence over the page selection. Aggregate rows have
// no page selection, and selected views may still show a cross-workspace task.
function taskWorkspacePath(task, path) {
  if (!task.workspace_id) return withWorkspace(path);
  const sep = path.includes("?") ? "&" : "?";
  return withHost(`${path}${sep}workspace=${encodeURIComponent(task.workspace_id)}`);
}

function taskMutationPath(task, suffix = "") {
  return taskWorkspacePath(task, `/api/tasks/${encodeURIComponent(task.id)}${suffix}`);
}

function scheduleFeedbackExpiry(map, key, context, delay) {
  const scheduledEntry = map.get(key);
  setTimeout(() => {
    if (map.get(key) === scheduledEntry && scheduledEntry && scheduledEntry.kind !== "pending") {
      map.delete(key);
      renderTasks(taskList(context), context);
    }
  }, delay);
}

// Renders the inline pending/success/error text next to a status or crew
// select, plus a bounded "undo" button while `entry.undo` is still live. The
// wrapper is a live region so screen-reader users get the same feedback a
// sighted user reads from the text/color change.
function buildMutationFeedback(entry, onUndo = () => {}) {
  if (!entry) return null;
  const wrap = el("span", { class: `mutation-feedback ${entry.kind}`, text: entry.text });
  wrap.setAttribute("role", "status");
  wrap.setAttribute("aria-live", entry.kind === "error" ? "assertive" : "polite");
  if (entry.undo) {
    const undoBtn = el("button", { class: "mutation-undo", text: "undo" });
    undoBtn.type = "button";
    undoBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      onUndo();
    });
    wrap.appendChild(undoBtn);
  }
  return wrap;
}

export function normalizeCrewPayload(payload) {
  const crews = Array.isArray(payload && payload.crews)
    ? payload.crews
      .filter((crew) => crew && crew.name)
      .map((crew) => ({
        name: String(crew.name),
        model: crew.model == null ? "" : String(crew.model),
        is_default: Boolean(crew.is_default),
      }))
    : [];
  crews.sort((a, b) => a.name.localeCompare(b.name));
  return {
    default_crew: payload && payload.default_crew ? String(payload.default_crew) : null,
    crews,
  };
}

export function cacheCrewPayload(payload) {
  lastCrewPayload = normalizeCrewPayload(payload);
  return lastCrewPayload;
}

export function hasCrewOptions() {
  return Array.isArray(lastCrewPayload.crews) && lastCrewPayload.crews.length > 0;
}

function crewOptionsSignature() {
  return JSON.stringify(lastCrewPayload);
}

// A stable string for a task's current mutation-feedback entry (or "" when
// none), so a syncNodes row diff picks up pending/success/error/undo
// transitions the same way it picks up any other task field change.
function feedbackSignature(map, taskId) {
  const entry = map.get(taskId);
  if (!entry) return "";
  return `${entry.kind}:${entry.text}:${entry.undo ? entry.undo.expiresAt : ""}`;
}

// The detail's own controls — status, complexity, crew and each field editor —
// keep their feedback outside the task object, so the detail's diff hash has to
// see it the way the row's hash sees the status/crew entries. Without this a
// saved or failed note would never paint.
function detailFeedbackSignature(taskId) {
  const parts = [feedbackSignature(statusFeedback, taskId), feedbackSignature(complexityFeedback, taskId), feedbackSignature(crewFeedback, taskId)];
  for (const field of Object.keys(TASK_FIELD_EDITORS)) {
    parts.push(feedbackSignature(fieldFeedback, fieldFeedbackKey(taskId, field)));
  }
  return parts.join("|");
}

function explicitCrewValue(task) {
  return task && task.crew ? String(task.crew) : "";
}

function isConfiguredCrewValue(value) {
  const name = value ? String(value) : "";
  return Boolean(name) && lastCrewPayload.crews.some((crew) => crew.name === name);
}

function hasStaleExplicitCrew(task) {
  const currentValue = explicitCrewValue(task);
  return Boolean(currentValue) && !isConfiguredCrewValue(currentValue);
}

function resolvedCrewName(task) {
  if (lastCrewPayload.default_crew) return lastCrewPayload.default_crew;
  if (!explicitCrewValue(task) && task && task.resolved_crew) {
    return String(task.resolved_crew);
  }
  return "workspace";
}

function defaultCrewOptionText(task) {
  return `default: ${resolvedCrewName(task)}`;
}

function crewOptionTitle(crew) {
  return `model=${crew.model || "-"}`;
}

function isTaskSummary(task) {
  return Boolean(task) && task.projection === "summary";
}

// The aggregate view's rows carry their owning workspace, which the detail and
// mutation endpoints do not echo back. A full projection replacing a row keeps
// those fields, or the row would lose its badge and, with it, its mutation
// target (canMutateTask) after the first edit.
const LIST_ONLY_TASK_FIELDS = ["workspace_id", "workspace_name", "workspace_root"];

function mergeTaskRecord(previous, updated) {
  if (!previous || !updated) return updated;
  const merged = { ...updated };
  for (const key of LIST_ONLY_TASK_FIELDS) {
    if (merged[key] == null && previous[key] != null) merged[key] = previous[key];
  }
  return merged;
}

function applyUpdatedTask(updatedTask, context) {
  if (!updatedTask || !updatedTask.id) return;
  const previous = taskList(context).find((task) => task && task.id === updatedTask.id);
  const merged = mergeTaskRecord(previous, updatedTask);
  if (!isTaskSummary(merged)) taskDetails.set(merged.id, merged);
  if (context && typeof context.replaceTask === "function") {
    context.replaceTask(merged);
  }
  if (pinnedExternalTask && pinnedExternalTask.task && pinnedExternalTask.task.id === merged.id) {
    pinnedExternalTask.task = merged;
  }
  return merged;
}

function currentCrewTask(task, context) {
  if (!task || !task.id) return task;
  return taskList(context).find((current) => current && current.id === task.id) ||
    (pinnedExternalTask && pinnedExternalTask.task && pinnedExternalTask.task.id === task.id
      ? pinnedExternalTask.task
      : null) ||
    taskDetails.get(task.id) ||
    task;
}

// Read the full projection behind a summary row. One read per summary object:
// a repeat call for the same row returns the same promise, settled or not, so
// renders and a status change racing on the same row share it.
function loadTaskDetail(task, context) {
  const existing = taskDetailLoads.get(task.id);
  if (existing && existing.source === task) return existing.promise;
  const load = { source: task, pending: true, error: null, promise: null };
  if (isAggregateView() && !task.workspace_id) {
    load.pending = false;
    load.error = "Task workspace is unknown";
    load.promise = Promise.reject(new Error(load.error));
    taskDetailLoads.set(task.id, load);
    return load.promise;
  }
  load.promise = fetchJson(taskMutationPath(task)).then(
    (full) => {
      load.pending = false;
      if (taskDetailLoads.get(task.id) === load) {
        applyUpdatedTask(full, context);
        renderTasks(taskList(context), context);
      }
      return full;
    },
    (error) => {
      load.pending = false;
      load.error = error.message || String(error);
      if (taskDetailLoads.get(task.id) === load) renderTasks(taskList(context), context);
      throw error;
    },
  );
  taskDetailLoads.set(task.id, load);
  return load.promise;
}

// What an expanded row can render right now: the full projection when the row
// is one or the last one fetched, plus whether a fresh read is pending or
// failed. Starts the read for a summary row that has none yet.
function taskDetailState(task, context) {
  if (!isTaskSummary(task)) return { task, pending: false, error: null };
  const load = taskDetailLoads.get(task.id);
  if (!load || load.source !== task) {
    loadTaskDetail(task, context).catch(() => {});
  }
  const current = taskDetailLoads.get(task.id);
  return {
    task: taskDetails.get(task.id) || null,
    pending: Boolean(current && current.pending),
    error: current ? current.error : null,
  };
}

// Drop the recorded read so the next render issues a fresh one.
function forgetTaskDetailLoad(taskId) {
  taskDetailLoads.delete(taskId);
  taskDrainLoads.delete(taskId);
}

function loadTaskDrainReadiness(task, context) {
  const previous = taskDrainLoads.get(task.id);
  const load = { source: task, pending: true, error: null, entry: previous ? previous.entry : null };
  taskDrainLoads.set(task.id, load);
  if (isAggregateView() && !task.workspace_id) {
    load.pending = false;
    load.entry = null;
    load.error = "Task workspace is unknown";
    return;
  }
  fetchJson(taskWorkspacePath(task, `/api/workflows/auto/readiness?task=${encodeURIComponent(task.id)}`)).then(
    (payload) => {
      const entry = Array.isArray(payload?.tasks) ? payload.tasks.find((item) => item?.task_id === task.id) : null;
      load.entry = entry || null;
      load.error = entry ? null : "The readiness snapshot does not mention this task";
    },
    (error) => {
      load.entry = null;
      load.error = error.message || String(error);
    },
  ).finally(() => {
    load.pending = false;
    if (taskDrainLoads.get(task.id) === load) renderTasks(taskList(context), context);
  });
}

// What the Readiness card knows of the drain for an open task: its own read's
// entry, else the Drain card's snapshot entry, and whether a read is pending
// or failed. Null for a status the drain does not consider.
function taskDrainState(task, context) {
  if (!DRAIN_READINESS_STATUSES.has(task.status)) return null;
  const load = taskDrainLoads.get(task.id);
  if (!load || load.source !== task) loadTaskDrainReadiness(task, context);
  const current = taskDrainLoads.get(task.id);
  return {
    entry: current.entry || drainReadinessEntry(task.id),
    pending: current.pending,
    error: current.error,
  };
}

// An answer diffs on what it says; until one arrives, on the read's state.
function taskDrainSignature(drain) {
  if (!drain) return "";
  return drain.entry ? JSON.stringify(drain.entry) : `${drain.pending}-${drain.error || ""}`;
}

function stopRowInteraction(node) {
  for (const eventName of ["pointerdown", "mousedown", "click", "keydown"]) {
    node.addEventListener(eventName, (event) => event.stopPropagation());
  }
}

// The "not ready" chip narrows the loaded page to proposed and backlog tasks
// with a blocking readiness gap. Readiness is derived per task rather than
// stored, so it filters here instead of in the server's page query.
let notReadyOnly = false;
let pilotWarningsOnly = false;
export function taskPilotWarningsFilter() { return pilotWarningsOnly; }

function filterTasks(tasks, context) {
  const q = searchQueryValue(context);
  const activeStatuses = activeStatusSet(context);
  return tasks.filter((t) => {
    if (!activeStatuses.has(t.status)) return false;
    if (notReadyOnly && !isNotReady(t)) return false;
    if (pilotWarningsOnly && !pilotWarnings(t).length) return false;
    if (!q) return true;
    return (
      (t.id && t.id.toLowerCase().includes(q)) ||
      (t.title && t.title.toLowerCase().includes(q))
    );
  });
}

// Who made and carried the task, recorded by the pipeline and so read-only.
const TASK_PEOPLE_FIELDS = [
  ["created_by", "Created by"],
  ["planned_by", "Planned by"],
  ["implemented_by", "Implemented by"],
  ["orchestrator", "Orchestrator"],
];

const RELATION_GROUPS = [
  ["blocked_by", "blocked by"],
  ["child_of", "child of"],
  ["spawned_from", "spawned from"],
  ["regression_from", "regression from"],
  ["supersedes", "supersedes"],
  ["related_to", "related to"],
];
const RELATION_GROUP_LABELS = new Map(RELATION_GROUPS);

function relationTypeKey(value) {
  if (!value) return "";
  return String(value)
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/-/g, "_")
    .toLowerCase();
}

export async function copyTaskIdWithNotice(taskId, context) {
  const copied = await copyText(taskId);
  taskActionNotice = `${taskId} is not in the filtered task list; ${copied ? "copied ID" : "could not copy the ID"}`;
  renderTasks(taskList(context), context);
}

function findTaskRow(taskId) {
  return Array.from(document.querySelectorAll("#tasks-body .row"))
    .find((row) => row.dataset.key === `task-${taskId}`) || null;
}

export function openVisibleTask(taskId, context) {
  const visible = filterTasks(taskList(context), context).some((task) => task.id === taskId);
  if (!visible) {
    copyTaskIdWithNotice(taskId, context);
    return;
  }
  selectTask(taskId, context);
  revealTaskRow(taskId);
}

// Bring a row into view without moving a list that already shows it: a
// deep link centres its row, a selection only scrolls as far as it must.
function revealTaskRow(taskId, block = "nearest") {
  requestAnimationFrame(() => {
    const row = findTaskRow(taskId);
    if (!row) return;
    row.scrollIntoView({ behavior: "smooth", block });
    row.classList.add("data-changed");
    setTimeout(() => row.classList.remove("data-changed"), 1200);
  });
}

// Select a list row: the pane moves to it. The clicked row stays where the
// pointer left it, even when opening the pane narrows the list and its rows
// rewrap, so selection never costs the operator their place.
function selectTask(taskId, context, { anchor = null } = {}) {
  const top = anchor && typeof anchor.getBoundingClientRect === "function" ? anchor.getBoundingClientRect().top : null;
  if (pinnedExternalTask && pinnedExternalTask.id !== taskId) pinnedExternalTask = null;
  selectedTaskId = taskId;
  setOpenTask(taskId);
  renderTasks(taskList(context), context);
  keepRowInPlace(taskId, top);
}

function keepRowInPlace(taskId, top) {
  const body = $("tasks-body");
  const row = top == null ? null : findTaskRow(taskId);
  if (!body || !row) return;
  const delta = row.getBoundingClientRect().top - top;
  if (Math.abs(delta) >= 1) body.scrollTop += delta;
}

// Close the pane. Focus returns to the row that opened it when it was in the
// pane (or nowhere), so the keyboard resumes where the operator left the list.
function closeTaskPane(context, { forget = false } = {}) {
  if (taskPageId) {
    leaveTaskPage(context);
    return;
  }
  const taskId = shownTaskId();
  if (!taskId) return;
  const pane = $("task-detail-pane");
  const active = document.activeElement;
  const focusWasInPane = !active || active === document.body || Boolean(pane && pane.contains && pane.contains(active));
  const row = findTaskRow(taskId);
  const top = row && typeof row.getBoundingClientRect === "function" ? row.getBoundingClientRect().top : null;
  if (pinnedExternalTask && pinnedExternalTask.id === taskId) pinnedExternalTask = null;
  if (selectedTaskId === taskId) selectedTaskId = null;
  if (openTaskId === taskId) setOpenTask(null);
  // Reopening re-reads the detail, which is also the retry for a read that
  // failed.
  if (forget) forgetTaskDetailLoad(taskId);
  renderTasks(taskList(context), context);
  keepRowInPlace(taskId, top);
  if (focusWasInPane) focusTaskRow(taskId);
}

function focusTaskRow(taskId) {
  const button = findTaskRow(taskId)?.querySelector?.("[data-row-focus]");
  if (button && typeof button.focus === "function") button.focus({ preventScroll: true });
}

// The task the pane holds right now: the pinned resolver result, else the
// selected row.
function shownTaskId() {
  return (pinnedExternalTask && pinnedExternalTask.id) || selectedTaskId || null;
}

// Enter the full-page view of a task. The list behind it keeps its state; the
// scroll is remembered because a hidden list loses it.
function openTaskPage(taskId, context) {
  const body = $("tasks-body");
  taskPageReturn = { id: taskId, scrollTop: body ? body.scrollTop : 0 };
  taskPageId = taskId;
  taskPageError = null;
  focusPaneTitle = true;
  if (openTaskId !== taskId) setOpenTask(taskId);
  renderTasks(taskList(context), context);
  window.location.hash = `#tasks/${encodeURIComponent(taskId)}`;
}

// Leave the page state; the next render restores the list where it was.
function exitTaskPageState() {
  if (!taskPageId) return;
  const back = taskPageReturn && taskPageReturn.id === taskPageId ? taskPageReturn : null;
  listRestore = { id: taskPageId, scrollTop: back ? back.scrollTop : null };
  taskPageId = null;
  taskPageReturn = null;
  taskPageError = null;
}

// Back to the list at the same selection. A page entered from the list goes
// back through history, so the browser's own Back agrees with this control; a
// page reached by a link has no list entry behind it and opens one.
function leaveTaskPage(context) {
  const taskId = taskPageId;
  if (!taskId) return;
  const fromList = Boolean(taskPageReturn && taskPageReturn.id === taskId);
  exitTaskPageState();
  if (!pinnedExternalTask && filterTasks(taskList(context), context).some((task) => task.id === taskId)) {
    selectedTaskId = taskId;
  }
  returningFromPage = true;
  renderTasks(taskList(context), context);
  if (fromList && typeof window.history?.back === "function") window.history.back();
  else window.location.hash = buildTasksHash(context);
}

// Task ids in a hash are untrusted input: keep only id-shaped values.
function parseOpenTaskId(value) {
  const id = (value || "").trim();
  return /^[\w.-]{1,64}$/.test(id) ? id : null;
}

// Mirror the open task into the current `#tasks` entry. The entry is replaced,
// not pushed, so toggling a row never adds a Back step.
function writeOpenTaskToUrl(taskId) {
  const hash = window.location.hash || "";
  const split = hash.indexOf("?");
  const route = split < 0 ? hash : hash.slice(0, split);
  if (route !== "#tasks" || typeof window.history?.replaceState !== "function") return;
  const params = new URLSearchParams(split < 0 ? "" : hash.slice(split + 1));
  if (taskId) params.set("open", taskId);
  else params.delete("open");
  const query = params.toString();
  const next = query ? `#tasks?${query}` : "#tasks";
  if (next !== hash) window.history.replaceState(null, "", next);
}

function setOpenTask(taskId) {
  openTaskGeneration += 1;
  openTaskId = taskId;
  writeOpenTaskToUrl(taskId);
}

// Every path that closes a task detail goes through here so the URL stops
// naming a task the page no longer shows. A full-page task stays: the page is
// that task, and its actions repaint it in place.
function collapseTask(taskId) {
  if (taskPageId === taskId) return;
  if (selectedTaskId === taskId) selectedTaskId = null;
  if (openTaskId === taskId) setOpenTask(null);
}

// Show the task a hash names, once the list that may contain it has rendered.
// A task the filter or page hides is fetched and pinned instead, so following
// a link never rewrites the saved status or search filter.
function resolvePendingOpenTask(tasks, context) {
  const taskId = pendingOpenTaskId;
  if (!taskId) return;
  pendingOpenTaskId = null;
  if (filterTasks(tasks, context).some((task) => task.id === taskId)) {
    // A route replay for the row already selected leaves the list where it
    // is, and coming back from the full page restores the list's own scroll.
    const settled = selectedTaskId === taskId || (listRestore && listRestore.id === taskId);
    selectedTaskId = taskId;
    pinnedExternalTask = null;
    if (!settled) revealTaskRow(taskId, "center");
  } else if (!(pinnedExternalTask && pinnedExternalTask.id === taskId)) {
    pinOpenTask(taskId, context);
  }
}

async function pinOpenTask(taskId, context) {
  const visit = captureWorkspaceVisit();
  const generation = openTaskGeneration;
  const superseded = () => !visit.isCurrent() || openTaskId !== taskId || generation !== openTaskGeneration;
  try {
    const task = await fetchJson(`/api/tasks/${encodeURIComponent(taskId)}`);
    if (superseded()) return;
    if (!task || task.id !== taskId) throw new Error(`${taskId} not found`);
    pinnedExternalTask = { task, id: taskId };
  } catch (error) {
    if (superseded()) return;
    taskActionNotice = error && error.status === 404
      ? `${taskId} not found`
      : `Could not open ${taskId}: ${(error && error.message) || error}`;
    // A full page has no list on screen to carry the notice.
    if (taskPageId === taskId) taskPageError = taskActionNotice;
  }
  renderTasks(taskList(context), context);
}

/* Global ID resolver support (ORB-00211): allow rendering detail for a task that
   is outside the filtered list (hidden status, search, or truncated fetch)
   without mutating DASHBOARD_TASK_STATUSES. The pinned detail shows in the
   side pane, marked as outside the list; auto-clears once the filtered list
   actually contains the task. */
export function setPinnedExternalTask(task, context) {
  if (!task || !task.id) return;
  cancelTaskSearchDebounce();
  if (selectedTaskId && selectedTaskId !== task.id) selectedTaskId = null;
  pinnedExternalTask = { task, id: task.id };
  pendingOpenTaskId = null;
  setOpenTask(task.id);
  // The resolver has cleared the text query; retire its URL and page too.
  window.history?.replaceState?.(null, "", buildTasksHash(context));
  if (context?.resetTaskPagination) context.resetTaskPagination();
}

export function clearPinnedExternalTask(context) {
  if (pinnedExternalTask && openTaskId === pinnedExternalTask.id) setOpenTask(null);
  pinnedExternalTask = null;
  if (context) renderTasks(taskList(context), context);
}

// Clearing also retires hash-driven reads, so a delayed response cannot restore
// a dismissed result, even after another lookup for the same task.
export function cancelTaskSearchDebounce() {
  if (searchDebounce) clearTimeout(searchDebounce);
  searchDebounce = null;
}

export function clearTaskSearch(context) {
  cancelTaskSearchDebounce();
  if (context?.cancelTaskLookup) context.cancelTaskLookup();
  selectedTaskId = null;
  taskPageId = null;
  taskPageReturn = null;
  pendingOpenTaskId = null;
  pinnedExternalTask = null;
  taskActionNotice = null;
  setSearchQuery(context, "");
  setOpenTask(null);
  window.history?.replaceState?.(null, "", buildTasksHash(context));
  if (context?.resetTaskPagination) context.resetTaskPagination();
  syncTaskControls(context);
  renderTasks(taskList(context), context);
  refreshTasks(context).catch((error) => console.error("Failed to clear task search", error));
}

// Per-status counts for the chips, from the rows this page holds. They are
// only stated when the page holds every matching task: a paginated list would
// count one page, and a status that is off was never fetched.
function statusCounts(context) {
  if (!panelCanRender("tasks-body")) return null;
  const meta = tasksMeta(context);
  if (meta && (meta.next_cursor || (Number.isFinite(meta.offset) && meta.offset > 0))) return null;
  const counts = new Map();
  for (const task of taskList(context)) counts.set(task.status, (counts.get(task.status) || 0) + 1);
  return counts;
}

function refreshChips(context) {
  const container = $("task-filter");
  if (!container) return;
  const activeStatuses = activeStatusSet(context);
  const allOn = activeStatuses.size === statusOrder(context).length;
  const counts = statusCounts(context);
  for (const chip of container.querySelectorAll(".chip")) {
    const status = chip.dataset.status;
    const isAll = chip.dataset.role === "all";
    const on = chip.dataset.role === "not-ready"
      ? notReadyOnly
      : chip.dataset.role === "pilot-warnings" ? pilotWarningsOnly
      : isAll ? allOn : activeStatuses.has(status);
    chip.classList.toggle("active", on);
    // ORB-10874: chip state must not rely on the active/inactive color
    // difference alone — aria-pressed exposes it to assistive tech too.
    chip.setAttribute("aria-pressed", on ? "true" : "false");
    const count = chip.querySelector(".chip-count");
    if (count) count.textContent = status && on && counts ? String(counts.get(status) || 0) : "";
  }
}

// ORB-10874: the status filter and search query are represented in the tasks
// hash (`#tasks?status=a,b&q=...`), mirroring the audit tab's buildAuditHash/
// applyAuditHashQuery pair, so a reload or the browser's back/forward button
// restores the same filtered view instead of resetting to the default.
export function buildTasksHash(context) {
  // The full-page view is its own route; the list filter waits behind it.
  if (taskPageId) return `#tasks/${encodeURIComponent(taskPageId)}`;
  return buildTasksListHash(context);
}

function buildTasksListHash(context) {
  const sp = new URLSearchParams();
  const order = statusOrder(context);
  const activeStatuses = activeStatusSet(context);
  const selected = order.filter((s) => activeStatuses.has(s));
  if (selected.length === order.length) {
    // An explicit all-status selection must not collapse to the omitted
    // status query, which means the default set and excludes someday.
    sp.set("status", "all");
  } else {
    sp.set("status", selected.length > 0 ? selected.join(",") : "none");
  }
  if (notReadyOnly) sp.set("ready", "not");
  if (pilotWarningsOnly) sp.set("pilot", "warnings");
  const q = searchQueryValue(context);
  if (q) sp.set("q", q);
  if (openTaskId) sp.set("open", openTaskId);
  const qs = sp.toString();
  return qs ? `#tasks?${qs}` : "#tasks";
}

// `page` is the `#tasks/<id>` route segment: that task's full-page view. It
// names only the task, so the list filter behind it is left as it was.
export function applyTasksHashQuery(query, context, page = null) {
  cancelTaskSearchDebounce();
  const pageId = parseOpenTaskId(page);
  // A router replay of the route we just wrote is still the same lookup.
  // Retire it only when navigation actually changes the represented state.
  const current = buildTasksHash(context);
  const replay = pageId
    ? current === `#tasks/${encodeURIComponent(pageId)}`
    : query.toString() === (current.split("?")[1] || "");
  if (!replay && context?.cancelTaskLookup) context.cancelTaskLookup();
  const open = pageId || parseOpenTaskId(query.get("open"));
  if (openTaskId !== open) {
    if (selectedTaskId && selectedTaskId !== open) selectedTaskId = null;
    pinnedExternalTask = null;
    taskActionNotice = null;
  }
  openTaskGeneration += 1;
  openTaskId = open;
  pendingOpenTaskId = open;
  if (pageId) {
    if (taskPageId !== pageId) taskPageError = null;
    taskPageId = pageId;
    return;
  }
  // Back from the full page (this pane's control or the browser's) returns to
  // the list as it was: the same filter, page and scroll.
  const fromPage = Boolean(taskPageId) || returningFromPage;
  returningFromPage = false;
  exitTaskPageState();
  if (fromPage && query.toString() === (buildTasksListHash(context).split("?")[1] || "")) return;
  // A link that only names a task (`#tasks?open=ID`) opens it under whatever
  // filter the operator already has; reading it as "no filter" would reset
  // the chips and search to their defaults.
  if (open && !["status", "q", "ready", "pilot"].some((key) => query.has(key))) return;
  // A replay names the list already shown. Back fires popstate and then
  // hashchange for the address the router just normalized; the second must
  // not send the list to its first page and top.
  if (!replay && context && typeof context.resetTaskPagination === "function") {
    context.resetTaskPagination();
  }
  const order = statusOrder(context);
  const statusParam = query.get("status");
  if (statusParam == null) {
    setActiveStatuses(context, new Set(defaultActiveStatuses(context)));
  } else if (statusParam === "none") {
    setActiveStatuses(context, new Set());
  } else if (statusParam === "all") {
    setActiveStatuses(context, new Set(order));
  } else {
    const wanted = new Set(statusParam.split(",").map((s) => s.trim()).filter(Boolean));
    setActiveStatuses(context, new Set(order.filter((s) => wanted.has(s))));
  }
  notReadyOnly = query.get("ready") === "not";
  pilotWarningsOnly = query.get("pilot") === "warnings";
  setSearchQuery(context, (query.get("q") || "").trim().toLowerCase());
}

// Re-syncs the search box and chip states from context after the hash (not a
// direct user interaction with these controls) changed them — e.g. on load,
// or after a back/forward navigation.
export function syncTaskControls(context) {
  const search = $("task-search");
  if (search) {
    const q = searchQueryValue(context);
    if (search.value !== q) search.value = q;
  }
  refreshChips(context);
}

function navigateTasksHash(context) {
  if (context && typeof context.resetTaskPagination === "function") {
    context.resetTaskPagination();
  }
  const hash = buildTasksHash(context);
  if (window.location.hash !== hash) {
    window.location.hash = hash;
  } else {
    refreshChips(context);
    refreshTasks(context).catch((error) => console.error("Failed to reset task pages", error));
  }
}

export function renderTaskPagination(context) {
  const previous = $("tasks-previous");
  const next = $("tasks-next");
  const status = $("tasks-page-status");
  if (!previous || !next || !status) return;
  const state = context && typeof context.getTaskPagination === "function"
    ? context.getTaskPagination()
    : {};
  previous.disabled = Boolean(state.loading) || !state.canPrevious;
  next.disabled = Boolean(state.loading) || !state.canNext;
  // One page needs no pager; it comes back as soon as there is somewhere to go
  // or something to report.
  const pager = previous.closest ? previous.closest(".task-pagination") : null;
  if (pager) pager.hidden = !state.canPrevious && !state.canNext && !state.loading && !state.error;
  status.textContent = state.error
    ? `Page failed: ${state.error}`
    : state.loading ? "Loading task page…" : "";
  status.className = state.error ? "error" : "";
  if (previous.dataset.wired !== "true") {
    previous.dataset.wired = "true";
    previous.addEventListener("click", () => context.navigateTaskPage("previous"));
    next.addEventListener("click", () => context.navigateTaskPage("next"));
  }
}

export function buildChips(context) {
  const container = $("task-filter");
  container.innerHTML = "";
  const chip = (text, attrs, quiet, title) => {
    const node = el("button", { class: `chip${quiet ? " chip-quiet" : ""}`, type: "button", title }, [
      el("span", { class: "chip-label", text }),
      ...(attrs.status ? [el("span", { class: "chip-count" })] : []),
    ]);
    Object.assign(node.dataset, attrs);
    return node;
  };
  // The open statuses lead as chips with a dot and a count; the closed ones,
  // All and Not ready wait behind More, a disclosure. A quiet chip that is on
  // stays in view while More is closed, so the filter the list obeys is
  // always on screen.
  const defaults = new Set(defaultActiveStatuses(context));
  const primary = el("div", { class: "chip-group" });
  const secondary = el("div", { class: "chip-group chip-group-quiet", id: "task-filter-more" });
  for (const status of statusOrder(context)) {
    const node = chip(STATUS_LABELS[status] || status, { status }, !defaults.has(status));
    node.addEventListener("click", () => {
      const activeStatuses = activeStatusSet(context);
      if (activeStatuses.has(status)) {
        activeStatuses.delete(status);
      } else {
        activeStatuses.add(status);
      }
      navigateTasksHash(context);
    });
    (defaults.has(status) ? primary : secondary).appendChild(node);
  }
  const allChip = chip("All", { role: "all" }, true);
  allChip.addEventListener("click", () => {
    setActiveStatuses(context, new Set(statusOrder(context)));
    navigateTasksHash(context);
  });
  const notReady = chip("Not ready", { role: "not-ready" }, true, "Only proposed and backlog tasks with a gap that blocks automation");
  notReady.classList.add("readiness-filter");
  notReady.addEventListener("click", () => {
    notReadyOnly = !notReadyOnly;
    navigateTasksHash(context);
  });
  const pilotFilter = chip("Pilot warnings", { role: "pilot-warnings" }, true, "Proposed tasks held by their latest pilot findings");
  pilotFilter.addEventListener("click", () => {
    pilotWarningsOnly = !pilotWarningsOnly;
    navigateTasksHash(context);
  });
  secondary.append(allChip, notReady, pilotFilter);
  const more = el("button", {
    class: "chip-more",
    type: "button",
    "aria-controls": "task-filter-more",
    "aria-expanded": "false",
    title: "Closed statuses, All, Not ready and Pilot warnings",
  }, [el("span", { text: "More" })]);
  more.addEventListener("click", () => {
    const open = more.getAttribute("aria-expanded") !== "true";
    more.setAttribute("aria-expanded", String(open));
    secondary.classList.toggle("open", open);
  });
  container.append(primary, more, secondary);
  refreshChips(context);
}

export function wireSearch(context) {
  const input = $("task-search");
  input.addEventListener("input", (e) => {
    setSearchQuery(context, e.target.value.trim().toLowerCase());
    cancelTaskSearchDebounce();
    searchDebounce = setTimeout(() => {
      searchDebounce = null;
      navigateTasksHash(context);
    }, 250);
  });
  // ORB-14495: `/` jumps to the task search while the Tasks view shows it. A
  // field that takes typing keeps the key as a character.
  document.addEventListener("keydown", (event) => {
    if (event.key !== "/" || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    const target = event.target;
    if (target && (target.isContentEditable || (target.closest && target.closest("input, textarea, select")))) return;
    if (input.getClientRects().length === 0) return;
    event.preventDefault();
    input.focus();
    input.select();
  });
}

// ORB-10874: a compact, always-visible restatement of the filters currently
// narrowing the list — the chips/search box already encode this, but not in a
// form a screen reader announces or a glance confirms without reading each
// control. Rendered next to the count so "why am I seeing this many" is
// answered in the same place as "how many is this".
function renderFilterSummary(context) {
  const node = $("task-filter-summary");
  if (!node) return;
  const order = statusOrder(context);
  const activeStatuses = activeStatusSet(context);
  const parts = [];
  if (activeStatuses.size === 0) {
    parts.push("status: none selected");
  } else if (activeStatuses.size < order.length) {
    parts.push(`status: ${order.filter((s) => activeStatuses.has(s)).join(", ")}`);
  }
  if (notReadyOnly) parts.push("not ready");
  if (pilotWarningsOnly) parts.push("pilot warnings");
  const q = searchQueryValue(context);
  if (q) parts.push(`search: "${q}"`);
  node.textContent = parts.length > 0 ? `Filtering by ${parts.join(" · ")}` : "Showing all statuses";
}

// `os:` tags route a task to a host of that OS, so they read apart from the
// labels around them.
function isOsTag(tag) {
  return typeof tag === "string" && tag.toLowerCase().startsWith("os:");
}

function buildTagRow(tags) {
  const wrap = el("div", { class: "detail-tag-row" });
  for (const tag of tags) {
    wrap.appendChild(isOsTag(tag)
      ? el("span", { class: "chip os-chip", text: tag, title: `Admitted only on a host running ${tag.slice(3)}` })
      : el("span", { class: "chip", text: tag }));
  }
  return wrap;
}

// The task row's OS chips, from the parsed requirement the server attaches;
// a tag outside the namespace reads as the wait it causes.
function buildOsBadges(task) {
  const requirement = task.os_requirement;
  if (!requirement || typeof requirement !== "object") return [];
  const invalid = Array.isArray(requirement.invalid) ? requirement.invalid : [];
  const anyOf = Array.isArray(requirement.any_of) ? requirement.any_of : [];
  if (["unresolved", "conflict"].includes(requirement.state)) {
    return [el("span", { class: "os-badge os-badge-invalid", text: requirement.state, title: requirement.describe })];
  }
  return [
    ...anyOf.map((os) => el("span", { class: "os-badge mono", text: os === "macos" ? "macOS" : os, title: `Runs on ${requirement.describe || os}` })),
    ...invalid.map((tag) => el("span", { class: "os-badge os-badge-invalid mono", text: tag, title: "Unsupported OS tag: no host runs this task until it is retagged" })),
  ];
}

// Readiness: what a proposed or backlog task still lacks before automation
// approves or admits it. The server attaches it to those statuses only, with
// `preparing` set while a task-pilot run holds the task.
function readinessOf(task) {
  const readiness = task && task.readiness;
  return readiness && typeof readiness === "object" ? readiness : null;
}

// Blocking gaps first. The sort is stable, so server order holds within a severity.
function readinessGaps(readiness) {
  const gaps = Array.isArray(readiness.gaps) ? readiness.gaps.filter((gap) => gap && gap.code) : [];
  return gaps.sort((a, b) => Number(b.severity === "blocking") - Number(a.severity === "blocking"));
}

function isNotReady(task) {
  const readiness = readinessOf(task);
  return Boolean(readiness) && readiness.ready === false;
}

function readinessSignature(task) {
  const readiness = readinessOf(task);
  if (!readiness) return "";
  const gaps = readinessGaps(readiness).map((gap) => `${gap.code}/${gap.severity}`).join(",");
  return `${readiness.ready}:${readiness.preparing ? "p" : ""}:${gaps}`;
}

// The row chip, in the os-badge slot: `preparing` while a pilot run holds the
// task, amber when a gap blocks automation, muted when every gap is advisory,
// and nothing for a task that lacks nothing. The tooltip lists each gap.
function buildReadinessBadge(task) {
  const readiness = readinessOf(task);
  if (!readiness) return [];
  if (readiness.preparing) {
    return [el("span", {
      class: "readiness-badge readiness-preparing mono",
      text: "preparing",
      title: "A task-pilot run is preparing this task; its readiness settles when the run does",
    })];
  }
  const gaps = readinessGaps(readiness);
  if (gaps.length === 0) return [];
  const blocking = readiness.ready === false;
  return [el("span", {
    class: `readiness-badge ${blocking ? "readiness-blocking" : "readiness-advisory"} mono`,
    text: `${blocking ? "not ready" : "advisory"} · ${gaps.length}`,
    title: gaps.map((gap) => `${gap.severity}: ${gap.message}`).join("\n"),
  })];
}

// Core supplies only the findings that hold promotion; clients do not
// reinterpret pilot assessments or read the drain's latest-pass snapshot.
function pilotWarnings(task) {
  return Array.isArray(task.pilot_warnings) ? task.pilot_warnings : [];
}

function buildPilotWarningBadge(task) {
  const count = pilotWarnings(task).length;
  return count ? [el("span", {
    class: "chip pilot-warning-chip",
    role: "img",
    "aria-label": `Pilot warnings: ${count} holding findings`,
  }, [el("span", { text: "⚠", "aria-hidden": "true" }), ` pilot warnings · ${count}`])] : [];
}

function deliveryAttentionLabel(attention) {
  if (!attention) return null;
  if (attention.blocker === "failed_checks") return "CI failed";
  if (attention.blocker === "conflict") return "conflict";
  if (attention.blocker) return String(attention.blocker).replaceAll("_", " ");
  if (attention.run_state) return "run failed";
  return "delivery stuck";
}

function deliveryAttentionTitle(task, attention) {
  const parts = [];
  if (attention.failed_step_id) parts.push(`failed step ${attention.failed_step_id}`);
  if (attention.blocker) parts.push(`blocker ${attention.blocker}`);
  if (attention.head_sha) parts.push(`head ${attention.head_sha}`);
  if (attention.attempt != null && attention.budget != null) parts.push(`auto-fix attempt ${attention.attempt} of ${attention.budget}`);
  if (attention.run_state) parts.push(`delivery run ${attention.run_state}`);
  if (attention.repairing) parts.push("a live run is repairing the pull request");
  if (attention.as_of) parts.push(`as of ${attention.as_of}`);
  return parts.length ? `${task.status} · ${deliveryAttentionLabel(attention)}: ${parts.join(", ")}` : `${task.status} · ${deliveryAttentionLabel(attention)}`;
}

function buildDeliveryAttentionBadge(task) {
  const attention = task.delivery_attention;
  if (!attention) return [];
  const label = deliveryAttentionLabel(attention);
  return [el("span", {
    class: "delivery-attention-badge",
    role: "img",
    "aria-label": deliveryAttentionTitle(task, attention),
    title: deliveryAttentionTitle(task, attention),
    text: `${task.status} · ${label}`,
  })];
}

function buildDeliveryAttentionBanner(task) {
  const attention = task.delivery_attention;
  if (!attention) return null;
  const lines = [];
  if (attention.failed_step_id) lines.push(`Failed step: ${attention.failed_step_id}`);
  if (attention.blocker) lines.push(`Blocker: ${attention.blocker}`);
  if (attention.head_sha) lines.push(`Observed head: ${attention.head_sha}`);
  if (attention.attempt != null && attention.budget != null) lines.push(`Auto-fix attempt ${attention.attempt} of ${attention.budget}`);
  if (attention.run_state) lines.push(`Delivery run: ${attention.run_state}`);
  lines.push(attention.repairing ? "A live run is watching or repairing the pull request." : "No live run is repairing the pull request.");
  if (attention.as_of) lines.push(`As of ${attention.as_of}`);
  const actions = el("div", { class: "task-attention-actions" });
  if (attention.run_id) {
    if (task.job_run_navigable !== false) {
      const run = el("a", { class: "attention-action", text: "View run" });
      run.href = runHref(attention.run_id, task.workspace_id ? { workspace: task.workspace_id } : {});
      run.addEventListener("click", (event) => event.stopPropagation());
      actions.appendChild(run);
    } else {
      actions.appendChild(el("span", { class: "attention-action", text: `Run ${attention.run_id}` }));
    }
  }
  const pullRequest = pullRequestLink(task);
  if (pullRequest) {
    const pr = el("a", { class: "attention-action", text: pullRequest.number ? `PR #${pullRequest.number}` : "Pull request" });
    pr.href = pullRequest.url;
    pr.rel = "noopener noreferrer";
    pr.target = "_blank";
    pr.addEventListener("click", (event) => event.stopPropagation());
    actions.appendChild(pr);
  } else if (attention.pr_number) {
    actions.appendChild(el("span", { class: "attention-action", text: `PR #${attention.pr_number}` }));
  }
  return el("section", { class: "task-attention warning delivery-attention", "aria-label": "Delivery needs attention" }, [
    el("span", { class: "task-attention-icon", "aria-hidden": "true" }),
    el("div", { class: "task-attention-content" }, [
      el("h3", { class: "task-attention-title", text: `Delivery needs attention · ${deliveryAttentionLabel(attention)}` }),
      ...lines.map((line) => el("p", { class: "task-attention-body", text: line })),
      actions.children.length ? actions : null,
    ]),
  ]);
}

function buildPilotWarnings(task) {
  const findings = pilotWarnings(task);
  if (!findings.length) return null;
  const labels = {
    utility_warnings: "Utility", surface_warnings: "Surface",
    validation_tool_warnings: "Validation tool", blocked_by: "Prerequisite",
    adr_conflicts: "Conflict",
  };
  return el("section", { class: "task-attention warning pilot-warnings", "aria-label": "Pilot warnings" }, [
    el("span", { class: "task-attention-icon", "aria-hidden": "true" }),
    el("div", { class: "task-attention-content" }, [
      el("h3", { class: "task-attention-title", text: "Pilot warnings" }),
      el("p", { class: "task-attention-body", text: "The drain holds this task until an operator approves it or edits it so the next pilot pass is clean." }),
      el("ul", { class: "pilot-warning-findings" }, findings.map(finding => el("li", {}, [
        el("strong", { text: `${labels[finding.kind] || String(finding.kind).replaceAll("_", " ")}: ` }),
        el("span", { text: finding.text }),
      ]))),
    ]),
  ]);
}

// The readiness dot beside a backlog task's ID: the pool colour, its words as
// the dot's label and the full wait in the tooltip. A row the snapshot does
// not mention keeps a hidden dot and says nothing.
function buildReadinessDot(task) {
  const readiness = drainReadinessFor(task.id);
  if (!readiness) return el("span", { class: "ready-dot none", "aria-hidden": "true" });
  const dot = el("span", { class: "ready-dot", role: "img", "aria-label": readiness.label, title: readiness.title });
  dot.dataset.pool = readiness.pool;
  return dot;
}

// A backlog row's second line after its ID: the readiness dot and why the
// task is or is not starting, in the drain snapshot's own words. The dot's
// label already says it to assistive technology, so the visible words are
// hidden from it rather than read twice.
function buildRowReason(task) {
  const dot = buildReadinessDot(task);
  const readiness = drainReadinessFor(task.id);
  if (!readiness) return el("span", { class: "row-reason none" }, [dot]);
  const words = readiness.badge ? readiness.badge.text : readiness.label;
  const text = `${words.charAt(0).toUpperCase()}${words.slice(1)}`.replace(/\bmacos\b/g, "macOS");
  return el("span", { class: "row-reason" }, [
    dot,
    el("span", { class: "row-reason-text", text, title: readiness.title, "aria-hidden": "true" }),
  ]);
}

// `[source] Rest of the title`: the bracketed source prefixes become tag chips
// beside the title (`[auto-task]` reads as an "auto-task" chip), so the title
// starts with the problem it names.
function splitTitlePrefix(title) {
  const match = /^((?:\[[^\]]+\]\s*)+)(.*)$/.exec(String(title || ""));
  if (!match || !match[2]) return [[], String(title || "")];
  const labels = [...match[1].matchAll(/\[([^\]]+)\]/g)].map((part) => part[1].trim()).filter(Boolean);
  return [labels, match[2]];
}

// Where a running or blocked task's run is and what it holds: the host, the
// time since the run started (known for runs this host records), and the files
// the task has locked, each file in the tooltip.
function runCellParts(task) {
  const machine = task.job_run_machine && task.job_run_machine.machine_name;
  const host = machine || (task.job_run_id ? getHost() || getServingHost()?.name || "this host" : "");
  const started = task.status === "in-progress" ? runStartedAt(task.job_run_id) : null;
  const files = taskLockFiles(task.id);
  return {
    host,
    remote: Boolean(machine) && machine !== (getHost() || getServingHost()?.name),
    elapsed: started != null ? elapsedLabel(Date.now() - started) : "",
    files,
  };
}

function buildRunCell(task) {
  const { host, elapsed, files } = runCellParts(task);
  const sub = [elapsed, files.length ? plural(files.length, "file") : ""].filter(Boolean);
  const title = [
    host ? `Run on ${host}` : "No run recorded",
    elapsed ? `running for ${elapsed}` : "",
    ...files,
  ].filter(Boolean).join("\n");
  return el("span", { class: "run-cell", title }, [
    el("span", { class: `run-host${host ? "" : " none"}`, text: host || "no run" }),
    ...sub.map((text) => el("span", { class: "run-sub", text })),
  ]);
}

function runCellSignature(task) {
  const { host, elapsed, files } = runCellParts(task);
  return JSON.stringify([host, elapsed, files]);
}

// A task id in the Readiness card: a link to that task in the same workspace,
// which opens it even when the Tasks filter hides it.
function readinessTaskLink(taskId, workspace) {
  const link = el("a", { class: "relation-target mono", text: taskId, title: `Open ${taskId}` });
  return bindTaskLink(link, taskId, workspace ? { workspace } : {});
}

// The Readiness card leads with the pool, then links every reported blocker.
// Without an entry it says whether a read is pending, failed or found nothing,
// and never claims a pool.
function buildDrainReadiness(drain, workspace) {
  const entry = drain && drain.entry;
  if (!entry) {
    const text = drain && drain.pending
      ? "Checking whether the drain can take this task…"
      : `Drain readiness unknown${drain && drain.error ? `: ${drain.error}` : ""}.`;
    return el("div", { class: "readiness-drain readiness-note", text });
  }
  const view = drainReadinessView(entry);
  const badge = view.badge;
  const dot = el("span", { class: "ready-dot", "aria-hidden": "true" });
  dot.dataset.pool = view.pool;
  const head = el("div", { class: "readiness-drain-head" }, [
    dot,
    el("strong", { text: view.poolLabel }),
    ...(badge && view.pool !== "locks" ? [el("span", { class: "readiness-drain-reason", text: `· ${badge.text}` })] : []),
  ]);
  const wrap = el("div", { class: "readiness-drain" }, [head]);
  wrap.dataset.pool = view.pool;
  const line = (label, children) => el("div", { class: "readiness-drain-line" }, [
    el("span", { class: "label", text: label }),
    el("span", { class: "readiness-drain-value" }, children),
  ]);
  if (!badge) {
    wrap.appendChild(el("div", { class: "readiness-fix", text: view.title }));
    return wrap;
  }
  if (badge.holders.length) {
    wrap.appendChild(line("Held by", badge.holders.map(id => readinessTaskLink(id, workspace))));
  } else if (view.pool === "locks" && !badge.runIds.length) {
    wrap.appendChild(line("Held by", [el("span", { text: "Lock holder unavailable" })]));
  }
  if (badge.runIds.length) {
    wrap.appendChild(line("Live run", badge.runIds.map(runId => {
      const run = el("a", { class: "mono", text: runId });
      run.href = runHref(runId, workspace ? { workspace } : {});
      return run;
    })));
  }
  if (badge.dependencies.length > 0) {
    wrap.appendChild(line("Depends on", badge.dependencies.map(({ taskId, status }) =>
      el("span", { class: "readiness-dependency" }, [
        readinessTaskLink(taskId, workspace),
        status ? el("span", { class: "readiness-fix", text: status }) : null,
      ]))));
  }
  if (badge.detail) wrap.appendChild(el("div", { class: "readiness-message", text: badge.detail }));
  return wrap;
}

// The detail's Readiness card: why the drain is or is not taking the task,
// then each preparation gap with its severity and its fix. "Ready" is claimed
// only for a task the drain reports eligible with nothing missing.
function buildReadinessBlock(readiness, drain, workspace) {
  const wrap = el("div", { class: "value readiness-list" });
  if (drain) wrap.appendChild(buildDrainReadiness(drain, workspace));
  if (!readiness) return wrap;
  if (readiness.preparing) {
    wrap.appendChild(el("div", {
      class: "readiness-note",
      text: "A task-pilot run is preparing this task; these gaps settle when it finishes.",
    }));
  }
  const gaps = readinessGaps(readiness);
  if (gaps.length === 0) {
    const eligible = drain && drain.entry && drain.entry.eligible === true;
    wrap.appendChild(el("div", { class: "readiness-note", text: eligible ? "Ready: nothing is missing." : "No preparation gaps." }));
    return wrap;
  }
  wrap.appendChild(el("span", { class: "readiness-gap-count", text: `${gaps.length} ${gaps.length === 1 ? "gap" : "gaps"}` }));
  for (const gap of gaps) {
    const severity = gap.severity === "blocking" ? "blocking" : "advisory";
    wrap.appendChild(el("div", { class: "readiness-gap" }, [
      el("span", { class: `readiness-badge readiness-${severity} mono`, text: severity }),
      el("div", { class: "readiness-gap-body" }, [
        el("div", { class: "readiness-message", text: gap.message || gap.code }),
        ...(gap.fix ? [el("div", { class: "readiness-fix", text: `Fix: ${gap.fix}` })] : []),
      ]),
    ]));
  }
  return wrap;
}

const GITHUB_PR_REF_SYSTEM = "github-pr";

// The delivery pull request a row links: the task's github-pr ref, when it
// carries an http(s) page. The server fills that page from the workspace's own
// origin remote for a ref recorded with only its number.
function pullRequestLink(task) {
  const refs = Array.isArray(task.external_refs) ? task.external_refs : [];
  const ref = refs.find((candidate) => candidate && candidate.system === GITHUB_PR_REF_SYSTEM && isHttpUrl(candidate.url));
  return ref ? { number: String(ref.id || ""), url: ref.url } : null;
}

function buildExternalRefs(refs) {
  const wrap = el("div");
  for (const ref of refs) {
    const label = ref.system === GITHUB_PR_REF_SYSTEM
      ? `PR #${ref.id || ""}`
      : `${ref.system || "external"}:${ref.id || ""}`;
    const line = el("div", { class: "external-ref-line" });
    // Task records are agent-writable, so a non-http(s) URL stays text.
    if (isHttpUrl(ref.url)) {
      const link = el("a", { text: label });
      link.href = ref.url;
      link.rel = "noopener noreferrer";
      link.target = "_blank";
      line.appendChild(link);
    } else {
      line.textContent = label;
    }
    wrap.appendChild(line);
  }
  return wrap;
}

// The typed relations in RELATION_GROUPS order, one entry per related task,
// each carrying the label that says how it relates.
function relationTargets(relations) {
  const byType = new Map(RELATION_GROUPS.map(([key]) => [key, []]));
  for (const relation of relations) {
    const key = relationTypeKey(relation.relation_type || relation.type);
    const target = relation.target == null ? "" : String(relation.target);
    if (!RELATION_GROUP_LABELS.has(key) || !target) continue;
    byType.get(key).push(target);
  }
  return RELATION_GROUPS.flatMap(([key, label]) =>
    byType.get(key).map((target) => ({ key, label, target })));
}

// ORB-11333: the settled before-PR review gate. Every value comes from the
// certificate the gate wrote; nothing here is inferred from status or tags.
function buildReviewGate(review) {
  const wrap = el("div", { class: "review-gate" });
  if (review.unreadable) {
    wrap.appendChild(el("div", { text: `review evidence unreadable: ${review.unreadable}` }));
    return wrap;
  }
  const reviewer = review.reviewer || {};
  const consumed = review.consumed || {};
  const budget = review.budget || {};
  const lines = [
    `verdict: ${review.verdict}${review.assurance ? ` (${review.assurance})` : ""}`,
    `reviewer: ${reviewer.crew ?? "—"} · ${reviewer.provider ?? "—"} / ${reviewer.model ?? "—"}${reviewer.same_model_as_implementer ? " · same model as implementer" : ""}`,
    `base ${review.base?.commit ?? "—"} → reviewed ${review.reviewed_candidate?.commit ?? "—"} → final ${review.final_candidate?.commit ?? "—"}`,
    `reviewer commit: ${Array.isArray(review.repair_commits) && review.repair_commits.length ? review.repair_commits.map((c) => `${c.commit.slice(0, 12)} by ${c.author}`).join(", ") : "none"}`,
    `findings: ${Array.isArray(review.findings) ? review.findings.length : 0} · validation: ${Array.isArray(review.validation) ? review.validation.length : 0} record(s), complete: ${review.validation_complete ? "yes" : "no"}`,
    `reviewer runtime: ${consumed.seconds ?? 0}s of ${budget.minutes ?? "?"} min`,
  ];
  if (Array.isArray(review.validation_limitations) && review.validation_limitations.length) {
    lines.push(`not established: ${review.validation_limitations.join("; ")}`);
  }
  if (Array.isArray(review.retained_obligations) && review.retained_obligations.length) {
    lines.push(`retained from earlier report revisions: ${review.retained_obligations.map((o) => `${o.validation?.command ?? "?"} ${o.validation?.outcome ?? "?"}`).join(", ")}`);
  }
  if (review.escalation) lines.push(`escalation: ${review.escalation}`);
  if (Array.isArray(review.landings) && review.landings.length) {
    for (const landing of review.landings) {
      lines.push(`landing ${landing.landed?.commit ?? "?"}: ${landing.transformation} · ${landing.covered ? "covered" : `uncovered (${landing.reason ?? "unknown"})`}`);
    }
  }
  if (Array.isArray(review.stale_reasons) && review.stale_reasons.length) {
    lines.push(`stale gate reasons: ${review.stale_reasons.join(", ")}`);
  }
  for (const line of lines) wrap.appendChild(el("div", { text: line }));
  return wrap;
}

function buildOwedCriteria(owed) {
  const wrap = el("div", { class: "owed-criteria" });
  for (const entry of owed) {
    const parts = [
      `${entry.criterion}. [${entry.state}] on ${entry.host}: ${entry.command} (target ${entry.target})`,
    ];
    if (entry.candidate_evidence) parts.push(`candidate: ${entry.candidate_evidence}`);
    if (entry.evidence) parts.push(`recorded by ${entry.recorded_by ?? "—"}: ${entry.evidence}`);
    wrap.appendChild(el("div", { text: parts.join(" · ") }));
  }
  return wrap;
}

function fmtSize(bytes) {
  const value = Number(bytes);
  if (!Number.isFinite(value) || value < 0) return "0 bytes";
  if (value < 1024) return `${value} bytes`;
  const kb = value / 1024;
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
  const mb = kb / 1024;
  return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
}

function artifactUrl(task, path) {
  const encodedPath = String(path)
    .split("/")
    .map((part) => encodeURIComponent(part))
    .join("/");
  return taskMutationPath(task, `/artifacts/${encodedPath}`);
}

function artifactMediaType(artifact, response) {
  return String(
    response.headers.get("content-type") || artifact.media_type || "application/octet-stream",
  ).split(";")[0].trim().toLowerCase();
}

function renderArtifactText(mediaType, text) {
  const rendered = mediaType === "text/markdown" ? renderMarkdown(text) : null;
  if (rendered !== null) {
    const view = el("div", { class: "markdown-body" });
    view.innerHTML = rendered;
    return view;
  }
  return el("pre", { text });
}

// The raster formats the server will serve inline. This mirrors the artifact
// policy in orbit-types (`inline_safe_artifact_media_type`), which the artifact
// route enforces with `nosniff`: SVG and HTML are viewable formats that also
// host script, so they are deliberately absent and fall through to download.
const INLINE_IMAGE_MEDIA_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
const ARTIFACT_FETCH_TIMEOUT_MS = 30000;

function artifactFileName(path) {
  return String(path).split("/").pop() || String(path);
}

function buildArtifactDownloadLink(artifact, objectUrl, text) {
  const link = el("a", { text: text || `Download ${artifact.path}` });
  link.href = objectUrl;
  link.download = artifactFileName(artifact.path);
  return link;
}

// A blob URL pins its bytes until revoked. `syncNodes` tears down a detail it
// replaces, so the preview releases them when the task detail goes away.
function releaseOnTeardown(node, objectUrl) {
  node.teardown = () => URL.revokeObjectURL(objectUrl);
  return node;
}

function buildArtifactImage(artifact, blob) {
  const objectUrl = URL.createObjectURL(blob);
  const figure = releaseOnTeardown(el("div", { class: "artifact-image-view" }), objectUrl);
  const image = el("img", { class: "artifact-image" });
  image.src = objectUrl;
  // The path is the only description the artifact carries, so it is a more
  // useful alt text than a generic label for anyone reading without the image.
  image.alt = String(artifact.path);
  image.loading = "lazy";
  // A stored artifact can be truncated or mislabeled. When the browser cannot
  // decode it, say so and still offer the bytes rather than leaving a broken
  // image icon behind.
  image.addEventListener("error", () => {
    figure.replaceChildren(
      el("div", {
        class: "artifact-error",
        text: `Unable to display ${artifact.path}: the image could not be decoded.`,
      }),
      buildArtifactDownloadLink(artifact, objectUrl),
    );
  });
  const open = el("a", { class: "artifact-open", text: "Open" });
  open.href = objectUrl;
  open.target = "_blank";
  open.rel = "noopener";
  const actions = el("div", { class: "artifact-actions" });
  actions.appendChild(open);
  actions.appendChild(buildArtifactDownloadLink(artifact, objectUrl, "Download"));
  figure.appendChild(image);
  figure.appendChild(actions);
  return figure;
}

function buildArtifactPreview(artifact, response) {
  const mediaType = artifactMediaType(artifact, response);
  if (INLINE_IMAGE_MEDIA_TYPES.has(mediaType)) {
    return response.blob().then((blob) => buildArtifactImage(artifact, blob));
  }
  if (
    mediaType === "text/markdown" ||
    mediaType === "application/json" ||
    mediaType.endsWith("/yaml") ||
    mediaType.endsWith("+yaml") ||
    mediaType.startsWith("text/")
  ) {
    return response.text().then((text) => renderArtifactText(mediaType, text));
  }
  return response.blob().then((blob) => {
    const objectUrl = URL.createObjectURL(blob);
    return releaseOnTeardown(buildArtifactDownloadLink(artifact, objectUrl), objectUrl);
  });
}

export function buildArtifacts(task) {
  const wrap = el("div", { class: "artifacts" });
  for (const [index, artifact] of task.artifacts.entries()) {
    const path = String(artifact.path || "");
    const mediaType = String(artifact.media_type || "application/octet-stream");
    const row = el("div", {
      class: "artifact-row",
      text: `${path} · ${mediaType} · ${fmtSize(artifact.size_bytes)}`,
    });
    const preview = el("div", { class: "artifact-preview" });
    // Artifact paths are free-form, so the position in the list is what makes a
    // stable id the row's `aria-controls` can point at.
    preview.id = `artifact-preview-${task.id}-${index}`;
    preview.hidden = true;

    // Disclosure lives on the preview's `hidden` flag; keeping the two in one
    // setter is what stops the announced state from drifting from the visible one.
    const revealPreview = (visible) => {
      preview.hidden = !visible;
      row.setAttribute("aria-expanded", String(visible));
    };

    // The preview owns its read. Retiring the task detail aborts a read in
    // flight, and a preview that resolves anyway is released at once: teardown
    // has already passed over this subtree, so a blob URL mounted into it later
    // would never be revoked.
    let request = null;
    let retired = false;
    preview.teardown = () => {
      retired = true;
      if (request) request.abort();
    };

    makeToggleRow(row, {
      expanded: false,
      controls: preview.id,
      onToggle: async (e) => {
        e.stopPropagation();
        if (preview.dataset.loaded === "true" || request) {
          revealPreview(preview.hidden);
          return;
        }
        revealPreview(true);
        preview.textContent = "loading...";
        // Same 30s bound as fetchJson, covering the body read too, so a
        // stalled artifact read ends in an error instead of "loading..." forever.
        const controller = new AbortController();
        request = controller;
        const timeout = setTimeout(() => controller.abort(), ARTIFACT_FETCH_TIMEOUT_MS);
        try {
          if (isAggregateView() && !task.workspace_id) throw new Error("task workspace is unknown");
          const response = await fetch(artifactUrl(task, path), { signal: controller.signal });
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          const view = await buildArtifactPreview(artifact, response);
          if (retired) {
            teardownNode(view);
            return;
          }
          preview.replaceChildren(view);
          preview.dataset.loaded = "true";
        } catch (error) {
          if (retired) return;
          const reason = error && error.name === "AbortError" ? "request timed out after 30 seconds" : error.message;
          preview.textContent = `Unable to load ${path}: ${reason}`;
        } finally {
          clearTimeout(timeout);
          request = null;
        }
      },
    });
    wrap.appendChild(row);
    wrap.appendChild(preview);
  }
  return wrap;
}

/* ORB-12235: the task fields the expanded detail can edit in place. Each entry
   reads the task's current value as editor text, renders the read-only view,
   and turns the edited text back into a PATCH body carrying that one field —
   never a whole task, which would clobber whatever an agent wrote concurrently.
   `complexity` is not here: it is a fixed set of choices, so it gets a select
   (buildComplexityUpdateControl) rather than a text editor. */
const TASK_FIELD_EDITORS = {
  description: {
    label: "description",
    toText: (task) => task.description || "",
    renderView: (task) =>
      task.description && task.description.trim()
        ? markdownView(task.description)
        : emptyFieldView("no description"),
    toPayload: (text) => ({ description: text }),
    placeholder: "Markdown description",
  },
  acceptance_criteria: {
    label: "acceptance criteria",
    toText: (task) => linesToText(task.acceptance_criteria),
    renderView: (task) =>
      Array.isArray(task.acceptance_criteria) && task.acceptance_criteria.length > 0
        ? buildCriteriaList(task.acceptance_criteria)
        : emptyFieldView("no acceptance criteria"),
    toPayload: (text) => ({ acceptance_criteria: textToLines(text) }),
    hint: "One criterion per line.",
  },
  tags: {
    label: "tags",
    multiline: false,
    toText: (task) => (Array.isArray(task.tags) ? task.tags.join(", ") : ""),
    renderView: (task) =>
      Array.isArray(task.tags) && task.tags.length > 0
        ? buildTagRow(task.tags)
        : emptyFieldView("no tags"),
    toPayload: (text) => ({ tags: splitTagText(text) }),
    placeholder: "comma, separated, tags",
  },
  context_files: {
    label: "context files",
    toText: (task) => linesToText(task.context_files),
    // Keep the empty state in the same refreshable list as populated
    // selectors, so a background refresh can replace it without rebuilding an
    // open detail that contains another field's draft.
    renderView: (task) => buildFileList(task),
    // The server refuses a selector that names nothing unless the caller says
    // the task is about to create it, so that escape is the editor's own
    // checkbox rather than a second, silently different code path.
    toPayload: (text, options) => {
      const payload = { context_files: textToLines(text) };
      if (options && options.allow_missing_context) payload.allow_missing_context = true;
      return payload;
    },
    hint: "One selector per line (file:…, dir:…, symbol:…).",
    toggle: {
      key: "allow_missing_context",
      label: "allow missing context",
      title: "Accept a selector naming a target this task will create",
    },
  },
};

function markdownView(text) {
  const view = el("div", { class: "markdown-body" });
  const rendered = renderMarkdown(text);
  if (rendered !== null) {
    view.innerHTML = rendered;
  } else {
    view.textContent = text;
  }
  return view;
}

function emptyFieldView(text) {
  return el("div", { class: "field-empty", text });
}

function taskMarkdownPanel(content) {
  return el("div", { class: "task-markdown-panel" }, [content]);
}

// Numbered, because criteria are cited by number ("criterion 3 is met").
function buildCriteriaList(criteria) {
  const ul = el("ol", { class: "ac-list" });
  for (const ac of criteria) {
    const rendered = renderMarkdownInline(ac);
    if (rendered !== null) {
      const li = el("li");
      li.innerHTML = rendered;
      ul.appendChild(li);
    } else {
      ul.appendChild(el("li", { text: ac }));
    }
  }
  return ul;
}

const CF_VIEW_KEY = "orbit.contextFiles.view";
const CF_COLLAPSED_KEY = "orbit.contextFiles.collapsed";
const CF_ROOT_FILES_KEY = "\u0000project-root-files";

function cfStoreGet(key) {
  try { return globalThis.localStorage ? globalThis.localStorage.getItem(key) : null; } catch (_) { return null; }
}
function cfStoreSet(key, value) {
  try { if (globalThis.localStorage) globalThis.localStorage.setItem(key, value); } catch (_) { /* per-viewer convenience only */ }
}
function cfViewMode() { return cfStoreGet(CF_VIEW_KEY) === "flat" ? "flat" : "grouped"; }
function cfCollapsed() {
  try { const v = JSON.parse(cfStoreGet(CF_COLLAPSED_KEY) || "{}"); return v && typeof v === "object" ? v : {}; } catch (_) { return {}; }
}
function cfSetCollapsed(key, collapsed) {
  const next = { ...cfCollapsed() };
  if (collapsed) next[key] = true; else delete next[key];
  cfStoreSet(CF_COLLAPSED_KEY, JSON.stringify(next));
}

function buildFileList(task) {
  const wrap = el("div", { class: "context-files-view" });
  const summary = el("span", { class: "cf-summary" });
  const groupedBtn = el("button", { class: "cf-toggle-btn", type: "button", text: "By folder" });
  const flatBtn = el("button", { class: "cf-toggle-btn", type: "button", text: "Flat paths" });
  groupedBtn.setAttribute("aria-pressed", "true");
  flatBtn.setAttribute("aria-pressed", "false");
  const toggle = el("div", { class: "cf-toggle", role: "group", "aria-label": "Context files view" }, [groupedBtn, flatBtn]);
  const copyAll = el("button", { class: "cf-copy-all", type: "button", text: "Copy selectors" });
  copyAll.addEventListener("click", () => {
    const t = ul.contentionTask;
    const paths = t && Array.isArray(t.context_files) ? t.context_files : [];
    if (paths.length) copyWithFeedback(copyAll, paths.join("\n"));
  });
  const header = el("div", { class: "cf-header" }, [summary, toggle, copyAll]);
  const banner = el("div", { class: "cf-banner", role: "status" });
  const ul = el("ul", { class: "file-list mono" });
  ul.contentionTask = task;
  ul.cfRefs = { wrap, summary, banner, header, groupedBtn, flatBtn };
  groupedBtn.addEventListener("click", () => { cfStoreSet(CF_VIEW_KEY, "grouped"); refreshFileList(ul); });
  flatBtn.addEventListener("click", () => { cfStoreSet(CF_VIEW_KEY, "flat"); refreshFileList(ul); });
  wrap.appendChild(header);
  wrap.appendChild(banner);
  wrap.appendChild(ul);
  refreshFileList(ul);
  return wrap;
}

function parseContextSelector(selector) {
  const match = /^([a-z]+):(.*)$/.exec(selector);
  if (!match) return { kind: "file", target: selector, cut: 0, base: selector, dir: "" };
  const [, kind, target] = match;
  let cut = 0;
  if (kind === "file") {
    cut = target.lastIndexOf("/") + 1;
  } else if (kind === "dir") {
    const trimmed = target.endsWith("/") ? target.slice(0, -1) : target;
    cut = trimmed.lastIndexOf("/") + 1;
  }
  return {
    kind,
    target,
    cut,
    base: target.slice(cut),
    dir: target.slice(0, cut),
  };
}

function formatDirectory(dirPath) {
  const span = el("span", { class: "selector-dim" });
  if (!dirPath) return span;
  const parts = dirPath.split("/");
  for (let i = 0; i < parts.length; i++) {
    if (i < parts.length - 1) {
      const seg = `${parts[i]}/`;
      if (i === 0) {
        span.appendChild(el("span", { class: "selector-root", text: seg }));
      } else {
        span.appendChild(document.createTextNode(seg));
      }
      span.appendChild(el("wbr"));
    } else if (parts[i]) {
      span.appendChild(document.createTextNode(parts[i]));
    }
  }
  return span;
}

function cfPathForSelector(p) {
  let target = p.target || "";
  if (p.kind === "symbol") {
    let gt = target.split("#")[0];
    const ci = gt.indexOf("::");
    if (ci > 0) gt = gt.slice(0, ci);
    target = gt;
  }
  return target;
}

function cfGroups(entries) {
  const normalized = entries.map((e) => {
    const entry = typeof e === "string" ? { selector: e, p: parseContextSelector(e), matches: [] } : e;
    const target = cfPathForSelector(entry.p);
    const segs = target.split("/").filter(Boolean);
    const isDirect = entry.p.kind === "dir" ? segs.length <= 1 : segs.length <= 2;
    const topKey = segs.length <= 1 ? CF_ROOT_FILES_KEY : segs[0];
    return { entry, target, segs, isDirect, topKey };
  });

  const byTop = new Map();
  for (const item of normalized) {
    let list = byTop.get(item.topKey);
    if (!list) { list = []; byTop.set(item.topKey, list); }
    list.push(item);
  }

  const groups = [];
  const byKey = new Map();

  for (const [topKey, items] of byTop) {
    const shouldSplit = topKey !== CF_ROOT_FILES_KEY && !items.some((it) => it.isDirect);
    for (const it of items) {
      const { entry, segs, target } = it;
      let g;
      if (topKey === CF_ROOT_FILES_KEY) {
        g = { key: CF_ROOT_FILES_KEY, prefix: "", name: "project root", subDir: "", displayBase: entry.p.kind === "symbol" ? entry.p.target : (entry.p.base || target) };
      } else if (shouldSplit) {
        const child = segs[1] || "";
        const rest = segs.slice(2).join("/");
        const idx = rest.lastIndexOf("/");
        g = {
          key: `${topKey}/${child}`,
          prefix: `${topKey}/`,
          name: child,
          subDir: idx >= 0 ? `${rest.slice(0, idx + 1)}` : "",
          displayBase: entry.p.kind === "symbol" ? entry.p.target : (entry.p.base || segs[segs.length - 1] || target),
        };
      } else {
        const rest = segs.slice(1).join("/");
        const idx = rest.lastIndexOf("/");
        g = {
          key: topKey,
          prefix: "",
          name: topKey,
          subDir: idx >= 0 ? `${rest.slice(0, idx + 1)}` : "",
          displayBase: entry.p.kind === "symbol" ? entry.p.target : (entry.p.base || segs[segs.length - 1] || target),
        };
      }
      entry.groupKey = g.key;
      entry.subDir = g.subDir;
      entry.displayBase = g.displayBase;
      let group = byKey.get(g.key);
      if (!group) {
        group = { ...g, dirs: new Map() };
        byKey.set(g.key, group);
        groups.push(group);
      }
      if (!group.dirs.has(g.subDir)) group.dirs.set(g.subDir, []);
      group.dirs.get(g.subDir).push(entry);
    }
  }

  for (const g of groups) g.count = [...g.dirs.values()].reduce((n, a) => n + a.length, 0);
  return groups;
}

function cfSummaryText(total, groups) {
  const fileLabel = `${total} file${total === 1 ? "" : "s"}`;
  if (!groups || groups.length === 0) return fileLabel;
  if (groups.length <= 3) {
    const parts = groups.map((g) => g.key === CF_ROOT_FILES_KEY ? "the project root" : g.key);
    return `${fileLabel} in ${parts.join(" and ")}`;
  }
  return `${fileLabel} in ${groups.length} folders`;
}

function cfHolderLabel(holder) { return `${holder.id} · ${String(holder.status || "").replaceAll("-", " ")}`; }

function makeContextRow(entry, task, { flat = false } = {}) {
  const { selector, p, matches } = entry;
  const state = matches === null ? "Contention unknown: current workspace data unavailable"
    : matches && matches.length ? task.status === "backlog" ? "Contended context" : "Active context contention" : "";
  const children = [];
  if (p.kind && p.kind !== "file") {
    const kindNode = el("span", { class: "selector-kind", text: `${p.kind}:` });
    kindNode.dataset.kind = p.kind;
    children.push(kindNode);
  }
  if (flat && p.kind !== "symbol") children.push(formatDirectory(p.dir || (p.target.includes("/") ? p.target.slice(0, p.target.lastIndexOf("/") + 1) : "")));
  children.push(el("span", { class: "selector-base", text: flat ? (p.base || p.target) : entry.displayBase }));
  const link = el("a", { class: "context-selector", title: [selector, state].filter(Boolean).join(" · ") }, children);
  bindTaskLink(link, task.id, task.workspace_id ? { workspace: task.workspace_id } : {});
  if (state) link.setAttribute("aria-label", `${selector}: ${state}`);
  const rowChildren = [link];
  if (matches && matches.length) {
    rowChildren.push(el("span", { class: "cf-pill cf-pill-contended", text: `held by ${matches.map(cfHolderLabel).join(", ")}` }));
  } else if (matches === null && (task.status === "backlog" || task.status === "in-progress")) {
    rowChildren.push(el("span", { class: "cf-pill cf-pill-unknown", text: "contention unknown" }));
  }
  const copyBtn = el("button", { class: "cf-copy", type: "button", title: "Copy selector" }, [el("span", { text: "⧉", "aria-hidden": "true" })]);
  copyBtn.setAttribute("aria-label", `Copy ${selector}`);
  copyBtn.addEventListener("click", (event) => { event.stopPropagation(); copyText(selector); });
  rowChildren.push(copyBtn);
  const li = el("li", { class: "cf-row" }, rowChildren);
  if (matches === null) li.classList.add("context-contention-unknown");
  if (matches && matches.length) li.classList.add(task.status === "backlog" ? "context-contended-backlog" : "context-contended-active");
  return li;
}

function refreshFileList(ul) {
  const task = ul.contentionTask;
  if (!task) return;
  const applicable = task.status === "backlog" || task.status === "in-progress";
  const rawPaths = Array.isArray(task.context_files) ? task.context_files : [];
  const entries = rawPaths.map((selector) => {
    const p = parseContextSelector(selector);
    return { selector, p, matches: applicable ? taskContextContenders(task, selector) : [] };
  });
  const groups = cfGroups(entries);
  const view = cfViewMode();
  const refs = ul.cfRefs;
  if (refs) {
    refs.header.hidden = entries.length === 0;
    refs.summary.textContent = cfSummaryText(entries.length, groups);
    refs.groupedBtn.setAttribute("aria-pressed", String(view === "grouped"));
    refs.flatBtn.setAttribute("aria-pressed", String(view === "flat"));
    refs.groupedBtn.classList.toggle("is-on", view === "grouped");
    refs.flatBtn.classList.toggle("is-on", view === "flat");
    const contended = entries.filter((e) => e.matches && e.matches.length);
    if (contended.length) {
      const holders = [...new Set(contended.flatMap((e) => e.matches.map((h) => h.id)))];
      refs.banner.hidden = false;
      refs.banner.textContent = `${contended.length} file${contended.length === 1 ? "" : "s"} overlap${contended.length === 1 ? "s" : ""} an in-progress task: held by ${holders.join(", ")}`;
    } else { refs.banner.hidden = true; refs.banner.textContent = ""; }
  }
  ul.classList.toggle("cf-flat", view === "flat");
  ul.classList.toggle("cf-grouped", view !== "flat");
  if (entries.length === 0) { ul.replaceChildren(el("li", { class: "field-empty", text: "no context files" })); return; }
  if (view === "flat") { ul.replaceChildren(...entries.map((e) => makeContextRow(e, task, { flat: true }))); return; }
  const collapsed = cfCollapsed();
  const nodes = groups.map((group) => {
    const isCollapsed = Boolean(collapsed[group.key]);
    const head = el("button", { class: "cf-group-head", type: "button" }, [
      el("span", { class: "cf-chevron", text: "›", "aria-hidden": "true" }),
      ...(group.prefix ? [el("span", { class: "cf-group-prefix", text: group.prefix })] : []),
      el("span", { class: "cf-group-name", text: group.name }),
      el("span", { class: "cf-group-count", text: `${group.count} file${group.count === 1 ? "" : "s"}` }),
    ]);
    head.setAttribute("aria-expanded", String(!isCollapsed));
    const body = el("div", { class: "cf-group-body" });
    body.hidden = isCollapsed;
    for (const [dir, dirEntries] of group.dirs) {
      if (dir) body.appendChild(el("div", { class: "cf-dir", text: dir }));
      const inner = el("ul", { class: "cf-files" });
      for (const e of dirEntries) inner.appendChild(makeContextRow(e, task));
      body.appendChild(inner);
    }
    head.addEventListener("click", () => {
      const nextCollapsed = !body.hidden;
      body.hidden = nextCollapsed;
      head.setAttribute("aria-expanded", String(!nextCollapsed));
      cfSetCollapsed(group.key, nextCollapsed);
    });
    const li = el("li", { class: "cf-group" }, [head, body]);
    return li;
  });
  ul.replaceChildren(...nodes);
}

// Update only the displayed selectors, including details preserved for an
// unsaved editor/comment. Never replace an editor or discard its draft.
onTaskRuntimeChange(() => {
  for (const ul of document.querySelectorAll(".file-list")) {
    if (ul.contentionTask) refreshFileList(ul);
  }
});

function linesToText(values) {
  return Array.isArray(values) ? values.join("\n") : "";
}

// A blank line is how an operator separates entries while typing, not an empty
// criterion or selector, so it never reaches the server.
function textToLines(text) {
  return String(text)
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}

function splitTagText(text) {
  return String(text)
    .split(/[,\n]/)
    .map((tag) => tag.trim())
    .filter(Boolean);
}

function fieldFeedbackKey(taskId, field) {
  return `${taskId}:${field}`;
}

function taskFieldHasValue(task, spec) {
  return spec.toText(task).trim() !== "";
}

/* ORB-11655: a detail holding an open editor is reused verbatim by the next
   background refresh, the same way an open comment form is, so the operator's
   unsaved text survives the poll. The open editors are tracked on the node
   itself because that node is the thing being kept, and under their own dataset
   key so closing one cannot cancel a comment draft's claim on the same node. */
function setDetailEditing(detail, field, open) {
  if (!detail) return;
  const fields = new Set((detail.dataset.editing || "").split(",").filter(Boolean));
  if (open) fields.add(field);
  else fields.delete(field);
  if (fields.size > 0) detail.dataset.editing = [...fields].join(",");
  else delete detail.dataset.editing;
}

function buildTaskFieldEditor(task, field, detail, context, editSlot = null) {
  const visit = captureWorkspaceVisit();
  const spec = TASK_FIELD_EDITORS[field];
  const mutationPath = visit.path(taskMutationPath(task));
  const mutable = canMutateTask(task);
  const editTitle = `Edit ${spec.label} for ${task.id}`;
  const wrap = el("div", { class: "field-editor-cell" });
  wrap.appendChild(
    buildInlineFieldEditor({
      label: spec.label,
      value: spec.toText(task),
      renderView: () => spec.renderView(task),
      multiline: spec.multiline !== false,
      placeholder: spec.placeholder || "",
      hint: spec.hint || "",
      toggle: spec.toggle || null,
      editable: mutable,
      editTitle,
      disabledTitle: aggregateRefusalTitle(editTitle, `edit ${spec.label}`),
      save: (text, options) => patchJson(mutationPath, spec.toPayload(text, options)),
      onSaved: (updatedTask) => {
        if (visit.isCurrent()) completeFieldSave(task.id, field, updatedTask, context);
      },
      onEditingChange: (open) => setDetailEditing(detail, field, open),
      editSlot,
    }),
  );
  const feedback = fieldFeedback.get(fieldFeedbackKey(task.id, field));
  const feedbackNode = buildMutationFeedback(feedback);
  if (feedbackNode) wrap.appendChild(feedbackNode);
  return wrap;
}

function completeFieldSave(taskId, field, updatedTask, context) {
  applyUpdatedTask(updatedTask, context);
  const key = fieldFeedbackKey(taskId, field);
  fieldFeedback.set(key, { kind: "success", text: `${TASK_FIELD_EDITORS[field].label} saved` });
  renderTasks(taskList(context), context);
  scheduleFeedbackExpiry(fieldFeedback, key, context, FIELD_SAVED_NOTICE_MS);
}

/* ORB-12235: complexity gates dispatch — an unassessed task is withheld from
   implementation — and until now only the CLI could set it. `unassessed` is
   deliberately not an option: the update endpoint rejects it, so offering it
   would only produce a 400. A task that currently carries it (or any value the
   server later adds) keeps a disabled placeholder so the select still shows
   what is stored. */
const TASK_COMPLEXITY_OPTIONS = ["low", "medium", "hard", "xhard"];

function buildComplexityUpdateControl(task, context) {
  const cell = el("div", { class: "complexity-cell" });
  const mutable = canMutateTask(task);
  const feedback = complexityFeedback.get(task.id);
  const label = `Update complexity for ${task.id}`;
  const select = el("select", {
    class: "task-complexity-select mono",
    title: mutable ? label : aggregateRefusalTitle(label, "change complexity"),
  });
  select.setAttribute("aria-label", label);

  const currentValue = assessedComplexity(task);
  if (!currentValue) {
    const placeholder = el("option", { text: task.complexity ? String(task.complexity) : "unassessed" });
    placeholder.value = "";
    placeholder.disabled = true;
    select.appendChild(placeholder);
  }
  for (const value of TASK_COMPLEXITY_OPTIONS) {
    const option = el("option", { text: value });
    option.value = value;
    select.appendChild(option);
  }
  select.value = currentValue;
  if (!mutable || (feedback && feedback.kind === "pending")) select.disabled = true;

  select.addEventListener("change", (event) => {
    event.stopPropagation();
    applyTaskComplexityChange(task, select.value, context);
  });
  cell.appendChild(select);
  const feedbackNode = buildMutationFeedback(feedback, () => {
    if (feedback && feedback.undo) {
      applyTaskComplexityChange(task, feedback.undo.previousValue, context);
    }
  });
  if (feedbackNode) cell.appendChild(feedbackNode);
  return cell;
}

function assessedComplexity(task) {
  const value = task && task.complexity ? String(task.complexity) : "";
  return TASK_COMPLEXITY_OPTIONS.includes(value) ? value : "";
}

async function applyTaskComplexityChange(task, nextValue, context) {
  const visit = captureWorkspaceVisit();
  const mutationPath = visit.path(taskMutationPath(task));
  const previousValue = assessedComplexity(task);
  if (!nextValue || nextValue === previousValue || !canMutateTask(task)) return;
  complexityFeedback.set(task.id, { kind: "pending", text: "saving…" });
  renderTasks(taskList(context), context);
  try {
    const updatedTask = await patchJson(mutationPath, { complexity: nextValue });
    if (!visit.isCurrent()) return;
    applyUpdatedTask(updatedTask, context);
    complexityFeedback.set(task.id, {
      kind: "success",
      text: "complexity saved",
      // Undo is offered only back to an assessed value; the endpoint refuses
      // `unassessed`, so a task that arrived unassessed has nothing to restore.
      undo: previousValue
        ? { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS }
        : undefined,
    });
  } catch (error) {
    if (!visit.isCurrent()) return;
    complexityFeedback.set(task.id, {
      kind: "error",
      text: `complexity update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  renderTasks(taskList(context), context);
  scheduleFeedbackExpiry(complexityFeedback, task.id, context, MUTATION_UNDO_WINDOW_MS + 500);
}

/* ORB-12645: the comment thread.

   Agents write spec-length Markdown here, so a comment is a document rather
   than a log line: the thread spans the full detail width below the two field
   columns, every body renders through markdown.js (the same sanitization as
   any other dashboard-authored body), and anything taller than a screenful is
   capped until the operator opens it.

   Expansion, the raw/rendered toggle and the thread order are operator state
   rather than task data, so they live in this module — keyed
   `<task id>#<index into task.comments>` — and survive the 30 s refresh that
   rebuilds the detail node. */
const COMMENT_COLLAPSE_HEIGHT_PX = 300;
// A rendered body is 13px sans at line-height 1.65 (~22px a line), and the
// ~72ch measure takes roughly 96 source columns before it wraps.
const COMMENT_LINE_HEIGHT_PX = 22;
const COMMENT_WRAP_COLUMNS = 96;
// Below this an outline would only repeat what the body already shows.
const COMMENT_OUTLINE_MIN_SECTIONS = 3;
const COMMENT_PREFS_KEY = "orbit.dashboard.comments";
// `by` on an agent-written comment names the crew or the agent family that
// wrote it. A human note carries the operator identity the API sanitizes to
// (ORB-10444), which is never one of these.
const AGENT_COMMENT_AUTHORS = new Set(["codex", "claude", "gemini", "grok", "agent", "system", "orbit"]);

function loadCommentPrefs() {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(COMMENT_PREFS_KEY) || "{}");
    return {
      newestFirst: parsed.newestFirst === true,
      collapsed: parsed.collapsed === true,
    };
  } catch (_) {
    return { newestFirst: false, collapsed: false };
  }
}

function saveCommentPrefs() {
  try {
    window.localStorage.setItem(COMMENT_PREFS_KEY, JSON.stringify(commentPrefs));
  } catch (_) {
    /* localStorage unavailable (private mode, quota) — thread order is a
       presentation preference, so losing it costs nothing */
  }
}

function commentSourceLines(message) {
  return String(message ?? "").split("\n");
}

// The `##` sections of a body: the collapsed card lists them instead of the
// text it is hiding, and three or more of them earn an outline.
function commentSections(message) {
  const titles = [];
  let fenced = false;
  for (const line of commentSourceLines(message)) {
    if (/^\s*(```|~~~)/.test(line)) {
      fenced = !fenced;
      continue;
    }
    if (fenced) continue;
    const heading = /^##\s+(.*?)\s*#*$/.exec(line);
    if (heading && heading[1].trim()) titles.push(heading[1].trim());
  }
  return titles;
}

/* Height has to be known while the card is still detached — the detail is
   built off-document and measuring it would mean painting the thread a frame
   late, after a reflow — so it is estimated from the source instead: one row
   a line, plus a row for every wrap. */
function estimatedCommentHeight(message) {
  let rows = 0;
  for (const line of commentSourceLines(message)) {
    rows += Math.max(1, Math.ceil(line.length / COMMENT_WRAP_COLUMNS));
  }
  return rows * COMMENT_LINE_HEIGHT_PX;
}

function commentIsLong(message) {
  return estimatedCommentHeight(message) > COMMENT_COLLAPSE_HEIGHT_PX;
}

function commentWordCount(message) {
  return String(message ?? "").split(/\s+/).filter(Boolean).length;
}

function commentMessage(comment) {
  return comment && typeof comment.message === "string" ? comment.message : "";
}

// The audit body of a pilot receipt: the whole message when the comment
// carries its typed operation id.
function pilotReceiptBody(comment) {
  const message = commentMessage(comment);
  if (comment.operation_id == null) return legacyPilotReceiptBody(message);
  return /^[a-fA-F0-9]{64}$/.test(comment.operation_id) ? message : null;
}

// Compatibility reader for receipts written before the typed operation id:
// an `operation_id=<id>` first line ahead of the audit body.
function legacyPilotReceiptBody(message) {
  const newline = message.indexOf("\n");
  if (newline < 0 || !/^operation_id=[a-fA-F0-9]{64}\r?$/.test(message.slice(0, newline))) return null;
  return message.slice(newline + 1);
}

// A pilot receipt is persisted data: summarize only a recognized assessment,
// and keep the original message for raw/copy and for unrecognized comments.
function pilotAssessment(comment) {
  if (!comment || comment.by !== "task-pilot") return null;
  const body = pilotReceiptBody(comment);
  if (body == null) return null;
  try {
    const receipt = JSON.parse(body);
    const assessment = receipt && receipt.assessment;
    const fields = ["disposition", "confidence", "recommended_crew", "recommended_complexity", "assessment_rationale"];
    if (!assessment || !fields.every(field => typeof assessment[field] === "string" && assessment[field].trim())) return null;
    return { receipt, assessment };
  } catch {
    return null;
  }
}

function pilotAssessmentView({ receipt, assessment }) {
  const view = el("div", { class: "pilot-assessment" });
  const fields = el("dl", { class: "pilot-assessment-fields" });
  const addField = (label, value) => {
    fields.appendChild(el("dt", { text: label }));
    fields.appendChild(el("dd", { text: value }));
  };
  const recommendation = (field, before) => {
    const after = assessment[field];
    return typeof before === "string" && before && before !== after ? `${before} → ${after}` : after;
  };
  addField("Disposition", assessment.disposition);
  addField("Confidence", assessment.confidence);
  addField("Recommended crew", recommendation("recommended_crew", receipt.crew_before));
  addField("Recommended complexity", recommendation("recommended_complexity", receipt.complexity_before));
  addField("Rationale", assessment.assessment_rationale);
  view.appendChild(fields);
  for (const [field, label] of [["evidence_gaps", "Evidence gaps"], ["reassessment_triggers", "Reassessment triggers"], ["blocked_by", "Blocked by"]]) {
    const items = Array.isArray(assessment[field]) ? assessment[field].filter(item => typeof item === "string" && item.trim()) : [];
    if (items.length === 0) continue;
    view.appendChild(el("div", { class: "pilot-assessment-label", text: label }));
    view.appendChild(el("ul", {}, items.map(text => el("li", { text }))));
  }
  for (const [field, label] of [["duplicate_of", "Duplicate of"], ["already_landed", "Already landed"]]) {
    const finding = assessment[field];
    if (finding == null) continue;
    const text = typeof finding === "string" ? finding : finding && typeof finding === "object"
      ? [finding.task_id, finding.commit, finding.evidence].filter(value => typeof value === "string" && value.trim()).join(" · ")
      : "";
    if (text) addField(label, text);
  }
  return view;
}

function commentKey(taskId, index) {
  return `${taskId}#${index}`;
}

// Stable across a reorder and across the refresh: the position in
// `task.comments`, never the position on screen.
function commentAnchorId(taskId, index) {
  return `comment-${taskId}-${index + 1}`;
}

function commentInitial(name) {
  const match = /[a-z0-9]/i.exec(String(name ?? ""));
  return match ? match[0].toUpperCase() : "?";
}

function isAgentComment(name, task) {
  const value = String(name ?? "").trim();
  if (!value) return false;
  if (AGENT_COMMENT_AUTHORS.has(value.toLowerCase())) return true;
  if (isConfiguredCrewValue(value)) return true;
  const resolved = task && task.resolved_crew ? String(task.resolved_crew) : "";
  return value === explicitCrewValue(task) || value === resolved;
}

// The task context carries absolute time only, and a thread also wants the
// "how long ago" a reader scans for, on the scale the rest of the dashboard
// uses.
function commentAge(value) {
  const at = value ? new Date(value) : null;
  if (!at || Number.isNaN(at.getTime())) return "";
  const seconds = (Date.now() - at.getTime()) / 1000;
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

function commentActionButton(label, title) {
  const button = el("button", { class: "comment-action", text: label, title });
  button.type = "button";
  return button;
}

function commentPermalink(anchorId) {
  const href = window.location && window.location.href ? String(window.location.href) : "";
  return `${href.split("#")[0]}#${anchorId}`;
}

function renderedCommentHeadings(view) {
  return typeof view.querySelectorAll === "function" ? Array.from(view.querySelectorAll("h2")) : [];
}

/* The outline follows the reader: whichever heading last crossed the top of
   the viewport owns the highlight. Observed rather than polled on scroll, and
   only where the browser supports it — without an observer the outline is
   still a working set of links. */
function trackCommentOutline(headings, links) {
  if (typeof IntersectionObserver !== "function" || headings.length === 0) return null;
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const index = headings.indexOf(entry.target);
        if (index < 0) continue;
        highlightCommentOutline(links, index);
      }
    },
    { rootMargin: "0px 0px -70% 0px" },
  );
  for (const heading of headings) observer.observe(heading);
  return observer;
}

function highlightCommentOutline(links, index) {
  links.forEach((link, position) => {
    link.className = position === index ? "comment-outline-link active" : "comment-outline-link";
  });
}

function buildCommentOutline(headings, titles) {
  const outline = el("nav", { class: "comment-outline" });
  outline.appendChild(el("div", { class: "comment-outline-title", text: "In this comment" }));
  const links = [];
  titles.forEach((title, index) => {
    const target = headings[index] || null;
    const link = el("button", { class: "comment-outline-link", text: title });
    link.type = "button";
    link.addEventListener("click", (event) => {
      event.stopPropagation();
      if (target && typeof target.scrollIntoView === "function") {
        target.scrollIntoView({ behavior: "smooth", block: "start" });
      }
      highlightCommentOutline(links, index);
    });
    links.push(link);
    outline.appendChild(link);
  });
  outline.links = links;
  return outline;
}

function disconnectCommentCards(container) {
  if (!container) return;
  for (const child of container.children || []) {
    if (typeof child.disconnectOutline === "function") {
      child.disconnectOutline();
    }
    disconnectCommentCards(child);
  }
}

function buildCommentCard(task, comment, index, context) {
  const key = commentKey(task.id, index);
  const message = commentMessage(comment);
  const writer = comment && comment.by ? String(comment.by) : "?";
  const pilot = pilotAssessment(comment);
  const titles = pilot ? [] : commentSections(message);
  const long = !pilot && commentIsLong(message);

  const card = el("article", { class: "comment-card" });
  card.id = commentAnchorId(task.id, index);
  card.dataset.commentKey = key;

  const rawToggle = commentActionButton("raw", "Show this comment's original text");
  const copy = commentActionButton("copy", "Copy this comment's original text");
  const permalink = commentActionButton("#", "Scroll to this comment and copy a link to it");
  permalink.className = "comment-action permalink";
  const actions = el("span", { class: "comment-actions" }, [rawToggle, copy, permalink]);

  const size = long
    ? el("span", {
        class: "comment-size",
        text: titles.length > 0
          ? `${commentWordCount(message)} words · ${titles.length} sections`
          : `${commentWordCount(message)} words`,
      })
    : null;
  const head = el("div", { class: "comment-head" }, [
    el("span", { class: "comment-initial", text: commentInitial(writer) }),
    el("span", {
      class: isAgentComment(writer, task) ? "comment-by agent" : "comment-by",
      text: writer,
    }),
    isAgentComment(writer, task) ? el("span", { class: "comment-agent-pill", text: "agent" }) : null,
    absTimeNode(context, comment && comment.at, { class: "comment-at" }),
    clockStamp(
      el("span", { class: "comment-age", text: commentAge(comment && comment.at), title: fmtAbsTimeValue(context, comment && comment.at) }),
      comment && comment.at,
      { title: true },
    ),
    size,
    actions,
  ]);

  const view = pilot ? pilotAssessmentView(pilot) : markdownView(message);
  view.className = "markdown-body comment-body";
  const headings = renderedCommentHeadings(view);
  const raw = el("pre", { class: "comment-raw", text: message });
  raw.style.display = "none";
  const bodies = el("div", { class: "comment-bodies" }, [view, raw]);
  // The outline is built from the rendered headings so its entries and its
  // scroll targets are the same nodes; the parsed `##` titles stand in only
  // where no renderer is available and the body fell back to plain text.
  const outlineTitles = headings.length > 0 ? headings.map((h) => String(h.textContent || "")) : titles;
  const outline = outlineTitles.length >= COMMENT_OUTLINE_MIN_SECTIONS
    ? buildCommentOutline(headings, outlineTitles)
    : null;
  const layout = el("div", { class: outline ? "comment-layout outlined" : "comment-layout" }, [outline, bodies]);

  let outlineObserver = null;
  const startTracking = () => {
    if (outlineObserver || !outline || headings.length === 0) return;
    outlineObserver = trackCommentOutline(headings, outline.links);
  };
  const stopTracking = () => {
    if (outlineObserver) {
      outlineObserver.disconnect();
      outlineObserver = null;
    }
  };
  card.disconnectOutline = stopTracking;
  card.teardown = stopTracking;

  const summary = el("div", {
    class: "comment-summary",
    text: titles.length > 0 ? `sections: ${titles.join(" · ")}` : `${commentWordCount(message)} words`,
  });
  const toggle = el("button", { class: "comment-toggle" });
  toggle.type = "button";
  const foot = el("div", { class: "comment-foot" }, [summary, toggle]);

  const apply = () => {
    const expanded = !long || expandedComments.has(key);
    const showsRaw = rawComments.has(key);
    card.className = [
      "comment-card",
      long ? "long" : "",
      long && !expanded ? "collapsed" : "",
      long && expanded ? "expanded" : "",
      showsRaw ? "raw" : "",
    ].filter(Boolean).join(" ");
    view.style.display = showsRaw ? "none" : "";
    raw.style.display = showsRaw ? "" : "none";
    if (outline) outline.style.display = showsRaw || !expanded ? "none" : "";
    rawToggle.textContent = showsRaw ? "rendered" : "raw";
    rawToggle.setAttribute("aria-pressed", String(showsRaw));
    toggle.textContent = expanded ? "Collapse" : "Show full comment";
    // A short comment has nothing to disclose, so its card keeps the density
    // of the line it replaced: no footer, no empty row under the body.
    foot.style.display = long ? "" : "none";
    summary.style.display = long && !expanded ? "" : "none";
    // The disclosure state belongs to the control that changes it, not to the
    // card, which is an article rather than a widget.
    toggle.setAttribute("aria-expanded", String(expanded));
    if (!expanded || showsRaw) {
      stopTracking();
    } else {
      startTracking();
    }
  };

  toggle.addEventListener("click", (event) => {
    event.stopPropagation();
    if (expandedComments.has(key)) expandedComments.delete(key);
    else expandedComments.add(key);
    apply();
  });
  rawToggle.addEventListener("click", (event) => {
    event.stopPropagation();
    if (rawComments.has(key)) rawComments.delete(key);
    else rawComments.add(key);
    apply();
  });
  copy.addEventListener("click", (event) => {
    event.stopPropagation();
    copyWithFeedback(copy, message);
  });
  permalink.addEventListener("click", (event) => {
    event.stopPropagation();
    if (typeof card.scrollIntoView === "function") {
      card.scrollIntoView({ behavior: "smooth", block: "start" });
    }
    copyWithFeedback(permalink, commentPermalink(card.id));
  });

  card.appendChild(head);
  card.appendChild(layout);
  card.appendChild(foot);
  apply();
  return card;
}

function getCommentHash() {
  if (typeof window === "undefined" || !window.location || !window.location.hash) return "";
  const raw = String(window.location.hash);
  return raw.startsWith("#") ? raw : `#${raw}`;
}

function buildCommentsPanel(task, context) {
  let isCollapsed = commentPrefs.collapsed === true;
  const currentHash = getCommentHash();
  if (currentHash.startsWith(`#comment-${task.id}-`) && restoredCommentHash !== currentHash) {
    isCollapsed = false;
  }
  let classes = "field-block collapsible comments-panel";
  if (isCollapsed) classes += " collapsed";
  const panel = el("div", { class: classes });
  renderCommentsPanel(panel, task, context);
  return panel;
}

function findCommentCard(id) {
  if (typeof document.getElementById === "function") {
    const node = document.getElementById(id);
    if (node && (node.id === id || String(node.className).includes("comment-card"))) return node;
  }
  const root = (typeof document.getElementById === "function" ? document.getElementById("task-detail-body") : null) || (typeof document !== "undefined" ? document.body : null);
  if (!root) return null;
  const queue = [root];
  while (queue.length > 0) {
    const curr = queue.shift();
    if (curr.id === id) return curr;
    if (curr.children) {
      for (const child of curr.children) queue.push(child);
    }
  }
  return null;
}

function findCommentsPanel(node) {
  let curr = node ? node.parentNode : null;
  while (curr) {
    if (curr.classList && typeof curr.classList.contains === "function" && curr.classList.contains("comments-panel")) {
      return curr;
    }
    if (String(curr.className || "").split(/\s+/).includes("comments-panel")) {
      return curr;
    }
    curr = curr.parentNode;
  }
  return null;
}

export function scrollToComment(hash = getCommentHash()) {
  if (!hash) return false;
  const rawId = String(hash).replace(/^#/, "");
  if (!rawId.startsWith("comment-")) return false;
  const target = findCommentCard(rawId);
  if (!target) return false;
  // The thread lives on the detail's Comments tab.
  for (let node = target.parentNode; node; node = node.parentNode) {
    if (typeof node.activateTab === "function") {
      node.activateTab("comments");
      break;
    }
  }
  const panel = findCommentsPanel(target);
  if (panel) {
    const isCollapsed = panel.classList && typeof panel.classList.contains === "function"
      ? panel.classList.contains("collapsed")
      : String(panel.className || "").split(/\s+/).includes("collapsed");
    if (isCollapsed) {
      if (panel.classList && typeof panel.classList.remove === "function") {
        panel.classList.remove("collapsed");
      }
      panel.className = String(panel.className || "")
        .split(/\s+/)
        .filter((c) => c !== "collapsed")
        .join(" ");
      const toggle = panel.querySelector ? panel.querySelector("h4 .field-toggle") : null;
      if (toggle && typeof toggle.setAttribute === "function") {
        toggle.setAttribute("aria-expanded", "true");
      }
      commentPrefs = { ...commentPrefs, collapsed: false };
      saveCommentPrefs();
    }
  }
  if (typeof target.scrollIntoView === "function") {
    target.scrollIntoView({ behavior: "smooth", block: "start" });
  }
  const currentHash = getCommentHash();
  if (currentHash && (hash === currentHash || rawId === currentHash.replace(/^#/, ""))) {
    restoredCommentHash = currentHash;
  }
  return true;
}

if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
  window.addEventListener("hashchange", () => {
    const commentHash = getCommentHash();
    if (commentHash.startsWith("#comment-")) {
      if (scrollToComment(commentHash)) {
        restoredCommentHash = commentHash;
      } else {
        restoredCommentHash = null;
      }
    } else {
      restoredCommentHash = null;
    }
  });
}

/* Rendered in place rather than through renderTasks: the thread's order and
   each card's disclosure are this panel's own state, and the detail node is
   diffed on the task payload, which none of them are part of. */
function renderCommentsPanel(panel, task, context) {
  disconnectCommentCards(panel);
  const isCollapsed = panel.classList && typeof panel.classList.contains === "function"
    ? panel.classList.contains("collapsed")
    : String(panel.className || "").split(/\s+/).includes("collapsed");
  const comments = Array.isArray(task.comments) ? task.comments : [];
  const actions = el("span", { class: "field-actions" });
  const order = el("button", {
    class: "comment-pref",
    text: commentPrefs.newestFirst ? "newest first" : "oldest first",
    title: "Toggle the order this thread is listed in",
  });
  order.type = "button";
  order.addEventListener("click", (event) => {
    event.stopPropagation();
    commentPrefs = { ...commentPrefs, newestFirst: !commentPrefs.newestFirst };
    saveCommentPrefs();
    renderCommentsPanel(panel, task, context);
  });
  actions.appendChild(order);
  if (comments.some((comment) => commentIsLong(commentMessage(comment)))) {
    const collapseAll = el("button", {
      class: "comment-pref",
      text: "collapse all",
      title: "Collapse every expanded comment on this task",
    });
    collapseAll.type = "button";
    collapseAll.addEventListener("click", (event) => {
      event.stopPropagation();
      for (let index = 0; index < comments.length; index++) {
        expandedComments.delete(commentKey(task.id, index));
      }
      renderCommentsPanel(panel, task, context);
    });
    actions.appendChild(collapseAll);
  }
  // The header also holds the order and collapse-all buttons, so the title and
  // count are the disclosure button rather than the whole header.
  const toggle = el("button", { class: "field-toggle" }, [
    el("span", { class: "field-title", text: "Comments" }),
    el("span", { class: "field-count", text: String(comments.length) }),
  ]);
  const head = el("h4", { class: "section-title" }, [toggle, actions]);
  makeDisclosure(head, toggle, {
    expanded: !isCollapsed,
    onToggle: (event) => {
      if (event && typeof event.stopPropagation === "function") {
        event.stopPropagation();
      }
      const hasToggle = panel.classList && typeof panel.classList.toggle === "function";
      const nowCollapsed = hasToggle
        ? panel.classList.toggle("collapsed")
        : !isCollapsed;
      if (!hasToggle || nowCollapsed === undefined) {
        const set = new Set(String(panel.className || "").split(/\s+/).filter(Boolean));
        const next = !set.has("collapsed");
        if (next) set.add("collapsed");
        else set.delete("collapsed");
        panel.className = Array.from(set).join(" ");
      }
      toggle.setAttribute("aria-expanded", String(!nowCollapsed));
      commentPrefs = { ...commentPrefs, collapsed: nowCollapsed };
      saveCommentPrefs();
    },
  });
  const thread = el("div", { class: "comment-thread" });
  const ordered = comments.map((comment, index) => ({ comment, index }));
  if (commentPrefs.newestFirst) ordered.reverse();
  for (const entry of ordered) {
    thread.appendChild(buildCommentCard(task, entry.comment, entry.index, context));
  }
  panel.replaceChildren(head, thread);
}

// Descriptions longer than this open clamped behind "Show full description".
// Estimated from the source, like a long comment, because the detail is built
// off-document and cannot be measured before it paints.
const LONG_DESCRIPTION_CHARS = 700;
const LONG_DESCRIPTION_LINES = 12;
// Tasks whose description the operator expanded; a rebuild keeps it open.
const expandedDescriptions = new Set();
// Tasks whose acceptance criteria the operator expanded; a rebuild keeps them open.
const expandedCriteria = new Set();

function isLongDescription(text) {
  const value = String(text || "");
  return value.length > LONG_DESCRIPTION_CHARS || value.split("\n").length > LONG_DESCRIPTION_LINES;
}

function isLongCriteria(criteria) {
  return isLongDescription(linesToText(criteria));
}

// The attention callout's Retry landing goes to the claim panel's own control,
// which asks for the reason and sends the request; nothing is sent from here.
function goToLandingRetry(detail, claim) {
  if (typeof detail.activateTab === "function") detail.activateTab("delivery");
  const panel = Array.from(detail.querySelectorAll(".claim-panel"))
    .find((node) => node.getAttribute("data-claim-id") === String((claim && claim.claim_id) || ""));
  const target = panel && (panel.querySelector('[data-reason-for~="retry-handoff"]') || panel.querySelector(".claim-actions"));
  if (!target) return;
  if (typeof target.scrollIntoView === "function") target.scrollIntoView({ block: "center", behavior: "smooth" });
  if (target.tagName === "INPUT") target.focus({ preventScroll: true });
}

// Core selects the latest block or escalation and supplies readable evidence.
// Render it as text: diagnoses and operator actions are untrusted task content.
function buildTaskBlockCallout(task, context, detail) {
  if (task.status !== "blocked") return null;
  const block = task.block || {};
  const headingId = `block-heading-${task.id}`;
  const content = el("div", { class: "task-attention-content" }, [
    el("h3", { class: "task-attention-title", id: headingId, text: "Blocked · needs your attention" }),
    el("p", {
      class: "task-attention-body task-block-reason",
      text: block.reason || "No block reason was recorded.",
    }),
  ]);
  const time = el("p", { class: "task-block-time" });
  if (block.at) {
    time.append("Blocked at ", absTimeNode(context, block.at));
  } else {
    time.textContent = "Block time not recorded.";
  }
  content.appendChild(time);
  const next = block.next_action || (task.next_step && task.next_step.resume_command
    ? "Review the failed run, resolve the cause, then resume the run or move this task back to backlog."
    : "Review the task and its evidence, resolve the cause, then move this task back to backlog when it is ready to retry.");
  content.appendChild(el("p", { class: "task-attention-body task-block-next", text: `Next: ${next}` }));
  const actions = el("div", { class: "task-attention-actions" });
  if (Number.isInteger(block.comment_index) && Array.isArray(task.comments) && task.comments[block.comment_index]) {
    const read = el("button", { class: "attention-action primary", type: "button", text: "Read escalation" });
    read.addEventListener("click", () => {
      detail.activateTab("comments");
      const id = commentAnchorId(task.id, block.comment_index);
      expandedComments.add(commentKey(task.id, block.comment_index));
      const panel = detail.querySelector(".comments-panel");
      if (panel) renderCommentsPanel(panel, task, context);
      scrollToComment(`#${id}`);
    });
    actions.appendChild(read);
  }
  if (block.run_id) {
    const run = el("a", { class: "attention-action", text: "View run" });
    run.href = runHref(block.run_id, task.workspace_id ? { workspace: task.workspace_id } : {});
    actions.appendChild(run);
  }
  if (!actions.children.length) {
    const history = el("button", { class: "attention-action", type: "button", text: "View history" });
    history.addEventListener("click", () => detail.activateTab("history"));
    actions.appendChild(history);
  }
  content.appendChild(actions);
  return el("section", { class: "task-attention warning task-block-callout", "aria-labelledby": headingId }, [
    el("span", { class: "task-attention-icon", "aria-hidden": "true" }), content,
  ]);
}

function buildTaskDetail(task, context, drain = null) {
  const detail = el("div", { class: "row-detail task-detail" });
  detail.addEventListener("click", (e) => e.stopPropagation());
  detail.teardown = () => disconnectCommentCards(detail);

  const leftCol = el("div", { class: "detail-main" });
  const rightCol = el("div", { class: "detail-side" });

  // The h4 carries the title in its own span so a header can also hold an
  // action (the field's edit button) without the two reading as one label.
  const addField = (parent, title, child, collapsible = false, collapsed = false, header = {}) => {
    let classes = "field-block";
    if (collapsible) classes += " collapsible";
    if (collapsed) classes += " collapsed";
    if (header.class) classes += ` ${header.class}`;
    const block = el("div", { class: classes });
    const heading = [el("span", { class: "field-title", text: title })];
    if (header.count != null) heading.push(el("span", { class: "field-count", text: String(header.count) }));
    // A collapsible header that also holds actions keeps its title and count
    // in one disclosure button, as the comments thread does, so its actions
    // stay controls of their own rather than parts of a toggle.
    const toggle = collapsible && header.actions ? el("button", { class: "field-toggle" }, heading) : null;
    const h4 = toggle
      ? el("h4", { class: "section-title" }, [toggle, header.actions])
      : el("h4", { class: "section-title" }, header.actions ? [...heading, header.actions] : heading);
    if (collapsible) {
      const setOpen = (open) => {
        block.classList.toggle("collapsed", !open);
        (toggle || h4).setAttribute("aria-expanded", String(open));
        if (header.onToggle) header.onToggle(open);
      };
      const onToggle = (e) => {
        e.stopPropagation();
        if (toggle && header.actions.contains(e.target)) return;
        setOpen(block.classList.contains("collapsed"));
      };
      if (toggle) {
        makeDisclosure(h4, toggle, { expanded: !collapsed, onToggle });
        // Acting on the section (its Edit) opens it, so the editor is never
        // hidden behind a collapsed header.
        header.actions.addEventListener("click", () => {
          if (block.classList.contains("collapsed")) setOpen(true);
        }, true);
      } else {
        makeToggleRow(h4, { expanded: !collapsed, onToggle });
      }
    }
    block.appendChild(h4);
    block.appendChild(child);
    if (parent) parent.appendChild(block);
    return block;
  };
  const actionSlot = () => el("span", { class: "field-actions" });

  // An editable field is shown even when it is empty — adding a missing
  // description is exactly the edit an operator comes here for — but only where
  // editing is actually allowed, so the read-only aggregate view stays as terse
  // as it was.
  const editor = (field, editSlot = null) => buildTaskFieldEditor(task, field, detail, context, editSlot);
  const showsEditor = (field) =>
    canMutateTask(task) || taskFieldHasValue(task, TASK_FIELD_EDITORS[field]);

  // What needs the operator, and how far the delivery has come, sit above the
  // tabs. Both come from the claim read the Delivery block makes, which lands
  // after the detail is built; until then the last read for this task (if any)
  // paints them, so a rebuilt detail neither flashes nor shifts. They repaint
  // only when what they show changes.
  const pullRequest = pullRequestLink(task);
  const attentionSlot = el("div", { class: "task-attention-slot" });
  const progressSlot = el("div", { class: "delivery-progress-slot" });
  let deliverySignature = null;
  const paintDelivery = (claims) => {
    const attention = claimAttention(claims);
    const steps = deliverySteps(claims);
    const signature = JSON.stringify([
      attention ? [attention.kind, attention.headline, attention.body, attention.retry, attention.claim.claim_id, attention.evidence && attention.evidence.raw] : null,
      steps,
    ]);
    if (signature === deliverySignature) return;
    deliverySignature = signature;
    const callout = buildAttentionCallout(attention, { pullRequest, onRetry: (claim) => goToLandingRetry(detail, claim) });
    attentionSlot.replaceChildren(...(callout ? [callout] : []));
    const progress = buildDeliveryProgress(steps);
    progressSlot.replaceChildren(...(progress ? [progress] : []));
  };
  const known = lastKnownClaims(task.id);
  if (known) paintDelivery(known);

  if (showsEditor("acceptance_criteria")) {
    const count = Array.isArray(task.acceptance_criteria) ? task.acceptance_criteria.length : 0;
    const actions = actionSlot();
    const block = addField(leftCol, "Acceptance criteria", editor("acceptance_criteria", actions), false, false, {
      actions,
      class: "criteria-block",
      ...(count ? { count } : {}),
    });
    if (isLongCriteria(task.acceptance_criteria)) {
      const open = expandedCriteria.has(task.id);
      if (!open) block.classList.add("clamped");
      const toggle = el("button", {
        class: "ac-toggle",
        type: "button",
        text: open ? "Show less" : "Show full AC",
        "aria-expanded": String(open),
      });
      toggle.addEventListener("click", (event) => {
        event.stopPropagation();
        const nowOpen = block.classList.contains("clamped");
        block.classList.toggle("clamped", !nowOpen);
        if (nowOpen) expandedCriteria.add(task.id);
        else expandedCriteria.delete(task.id);
        toggle.setAttribute("aria-expanded", String(nowOpen));
        toggle.textContent = nowOpen ? "Show less" : "Show full AC";
      });
      block.appendChild(toggle);
    }
  }

  if (showsEditor("description")) {
    const actions = actionSlot();
    const block = addField(leftCol, "Description", taskMarkdownPanel(editor("description", actions)), false, false, { actions, class: "description-block" });
    if (isLongDescription(task.description)) {
      const open = expandedDescriptions.has(task.id);
      if (!open) block.classList.add("clamped");
      const toggle = el("button", {
        class: "description-toggle",
        type: "button",
        text: open ? "Show less" : "Show full description",
        "aria-expanded": String(open),
      });
      toggle.addEventListener("click", (event) => {
        event.stopPropagation();
        const nowOpen = block.classList.contains("clamped");
        block.classList.toggle("clamped", !nowOpen);
        if (nowOpen) expandedDescriptions.add(task.id);
        else expandedDescriptions.delete(task.id);
        toggle.setAttribute("aria-expanded", String(nowOpen));
        toggle.textContent = nowOpen ? "Show less" : "Show full description";
      });
      block.appendChild(toggle);
    }
  }

  // Criteria the delivery owes until the deployed build is measured; an
  // operator records each with a `post-deploy: met|failed <N>` comment.
  // The folded sections read as one group of disclosure rows.
  const disclosures = el("div", { class: "detail-disclosures" });
  if (Array.isArray(task.owed_criteria) && task.owed_criteria.length) {
    addField(disclosures, "Owed after deploy", buildOwedCriteria(task.owed_criteria), true, false, {
      count: task.owed_criteria.length,
    });
  }

  if (task.plan && task.plan.trim()) {
    addField(disclosures, "Plan", taskMarkdownPanel(markdownView(task.plan)), true, true);
  }

  if (task.execution_summary && task.execution_summary.trim()) {
    addField(disclosures, "Execution summary", taskMarkdownPanel(markdownView(task.execution_summary)), true, true);
  }

  if (task.review && typeof task.review === "object") {
    addField(disclosures, "Review gate", buildReviewGate(task.review), true, true);
  }
  if (disclosures.children.length) leftCol.appendChild(disclosures);

  // The task's state, readiness, delivery and relations sit together above
  // the tabs; the side column holds the graph around it.
  const metaBlock = buildTaskMeta(task, context, { editor, showsEditor, drain });

  // The delivery card already links the pull request; the rest stay here.
  const otherRefs = (Array.isArray(task.external_refs) ? task.external_refs : []).filter((ref) =>
    !(pullRequest && ref && ref.system === GITHUB_PR_REF_SYSTEM && ref.url === pullRequest.url));
  if (otherRefs.length > 0) {
    addField(rightCol, "External refs", buildExternalRefs(otherRefs));
  }

  // The overview leads; context, history, runs, artifacts and the thread each
  // have a tab of their own, so the pane shows one subject at a time.
  const history = buildTaskHistory(task, context);
  const artifacts = Array.isArray(task.artifacts) ? task.artifacts.length : 0;
  const comments = Array.isArray(task.comments) ? task.comments.length : 0;
  const runs = el("div", { class: "task-runs", id: `detail-${task.id}-runs` });
  const lifecycleActions = buildActionsRow(task, detail, context, { includeComment: false });
  const commentActions = buildActionsRow(task, detail, context, { includeLifecycle: false });
  const overview = el("div", { class: "task-overview-content" }, [
    el("div", { class: "split-layout detail-overview" }, [leftCol, rightCol.children.length ? rightCol : null]),
  ]);
  // Keep the wrapper when Reject replaces the action row with its form.
  // It belongs to the detail scroll viewport, outside the tab panels.
  const actionFooter = lifecycleActions
    ? el("div", { class: "task-action-footer", role: "region", "aria-label": "Task actions" }, [lifecycleActions])
    : null;
  const commentsContent = el("div", { class: "comments-tab-content" }, [
    comments > 0 ? buildCommentsPanel(task, context) : el("p", { class: "field-empty", text: "No comments yet." }),
    commentActions,
  ]);
  // ORB-12516: distributed claim provenance and the owner's handoff actions.
  // The tab appears only once a read finds a claim this workspace holds for
  // the task. The last read decides whether it shows before the panel's own read.
  const delivery = el("div", { class: "delivery-tab" });
  const sections = buildDetailTabs(task, detail, [
    { name: "overview", label: "Overview", content: overview },
    {
      name: "context_files",
      label: "Context files",
      count: Array.isArray(task.context_files) ? task.context_files.length : 0,
      showZeroCount: true,
      available: showsEditor("context_files"),
      content: showsEditor("context_files") ? el("div", { class: "field-block context-files-block" }, [editor("context_files")]) : null,
    },
    { name: "delivery", label: "Delivery", content: delivery, available: Boolean(known && known.length) },
    { name: "history", label: "History", count: history.count, content: history.node },
    { name: "runs", label: "Runs", count: taskRunCount(task), content: runs },
    {
      name: "artifacts",
      label: "Artifacts",
      count: artifacts,
      content: artifacts > 0 ? buildArtifacts(task) : el("p", { class: "field-empty", text: "No artifacts attached." }),
    },
    {
      name: "comments",
      label: "Comments",
      count: comments,
      content: commentsContent,
    },
  ], context);
  // Everything above the tab content (state, callouts, delivery progress and
  // the tab strip) shares one band, so the strip reads apart from the content.
  const [tabStrip, ...tabPanels] = sections;
  const band = el("div", { class: "detail-band" });
  band.appendChild(metaBlock);
  const blockCallout = buildTaskBlockCallout(task, context, detail);
  if (blockCallout) band.appendChild(blockCallout);
  const pilotCallout = buildPilotWarnings(task);
  if (pilotCallout) band.appendChild(pilotCallout);
  const deliveryAttentionBanner = buildDeliveryAttentionBanner(task);
  if (deliveryAttentionBanner) band.appendChild(deliveryAttentionBanner);
  band.appendChild(attentionSlot);
  band.appendChild(progressSlot);
  band.appendChild(tabStrip);
  detail.appendChild(band);
  for (const panel of tabPanels) detail.appendChild(panel);
  if (actionFooter) detail.appendChild(actionFooter);
  delivery.appendChild(buildDeliveryPanel(task.id, {
    onTaskChanged: () => refreshTasks(context),
    onClaims: paintDelivery,
    onContent: (rendered) => detail.setTabAvailable("delivery", rendered),
    formatTime: (value) => fmtAbsTimeValue(context, value),
    pullRequest,
  }));
  renderTaskRuns(runs, task, context);
  // Activated once the panels exist: the Runs tab reads its runs then.
  detail.activateTab(activeDetailTab);

  return detail;
}

// The detail's head, above the tabs: what the task is and where it stands.
// The levers an operator sets (status, complexity, crew, host, tags) share
// one row of chips; the lifecycle strip places the status on the way to done;
// the delivery card gathers the pull request, the run, the hands that carried
// the task and its relations to other tasks. The pane head carries only the
// id and title, so nothing here is repeated there, and the timestamps close
// the Overview.
function buildTaskMeta(task, context, { editor, showsEditor, drain }) {
  const levers = el("div", { class: "task-levers" });
  levers.appendChild(buildStatusUpdateControl(task, context));
  levers.appendChild(buildComplexityUpdateControl(task, context));
  if (canMutateTask(task) && !isAggregateView()) {
    const control = buildCrewUpdateControl(task, context, { withChip: false });
    control.classList.add("task-chip-crew");
    const select = control.querySelector("select");
    select.removeAttribute("aria-label");
    select.id = `detail-${task.id}-crew`;
    const label = el("label", { text: "Crew" });
    label.htmlFor = select.id;
    const chip = el("span", { class: "task-chip task-crew-control" }, [label, select]);
    control.prepend(chip);
    levers.appendChild(control);
  } else {
    const crew = explicitCrewValue(task);
    levers.appendChild(el("span", { class: "task-chip task-chip-crew", title: "Crew" }, [
      el("span", { text: "Crew: " }),
      ...(crew ? [crew] : [resolvedCrewName(task), el("span", { class: "fact-note", text: " · default" })]),
    ]));
  }
  levers.appendChild(el("span", { class: "task-chip task-chip-host", title: "Runs on" }, [
    el("span", { class: "visually-hidden", text: "Runs on: " }),
    buildOsReadout(task),
  ]));
  if (showsEditor("tags")) levers.appendChild(el("div", { class: "task-levers-tags" }, [editor("tags")]));

  const head = el("div", { class: "task-meta-block" }, [levers]);
  const lifecycle = buildLifecycleStrip(task.status);
  if (lifecycle) head.appendChild(lifecycle);
  const delivery = buildDeliveryCard(task, drain, context);
  if (delivery) head.appendChild(delivery);
  return head;
}

// The statuses a task passes through on its way to done. Any other status
// (blocked, rejected, archived, someday) sits off this line and shows no strip.
const TASK_LIFECYCLE = [
  ["proposed", "Proposed"],
  ["backlog", "Backlog"],
  ["in-progress", "In progress"],
  ["review", "Review"],
  ["done", "Done"],
];

function buildLifecycleStrip(status) {
  const at = TASK_LIFECYCLE.findIndex(([key]) => key === status);
  if (at < 0) return null;
  const list = el("ol", { class: "task-lifecycle", "aria-label": "Lifecycle" });
  TASK_LIFECYCLE.forEach(([key, label], index) => {
    const state = index < at ? "past" : index === at ? "current" : "future";
    const step = el("li", { class: `task-lifecycle-step ${state}` }, [
      el("span", { class: "task-lifecycle-bar" }),
      el("span", { class: "task-lifecycle-label", text: label }),
    ]);
    if (state === "current") {
      step.dataset.status = key;
      step.setAttribute("aria-current", "step");
    }
    list.appendChild(step);
  });
  return list;
}

// The verb each recorded hand did, in the order the task met them.
const TASK_HAND_VERBS = {
  created_by: "created",
  planned_by: "planned",
  implemented_by: "implemented",
  orchestrator: "orchestrated",
};
const AGENT_FAMILIES = ["claude", "codex", "gemini", "grok"];

function buildHandsChain(task) {
  const hands = TASK_PEOPLE_FIELDS
    .filter(([key]) => task[key] != null && task[key] !== "")
    .map(([key, label]) => {
      const name = String(task[key]);
      const family = AGENT_FAMILIES.find((agent) => name.toLowerCase().includes(agent));
      const hand = el("span", { class: "task-hand", title: `${label} ${name}` }, [
        el("span", { class: "task-hand-name", text: name }),
        el("span", { class: "task-hand-verb", text: ` ${TASK_HAND_VERBS[key]}` }),
      ]);
      if (family) hand.dataset.agent = family;
      return hand;
    });
  if (!hands.length) return null;
  const chain = hands.flatMap((hand, index) =>
    index ? [el("span", { class: "task-hand-sep", "aria-hidden": "true" }), hand] : [hand]);
  return el("span", { class: "value task-hands" }, chain);
}

function buildDeliveryCard(task, drain, context) {
  const row = (kind, label, value) =>
    el("div", { class: `property-row fact task-delivery-row ${kind}` }, [el("span", { class: "label", text: label }), value]);
  const rows = [];
  const pullRequest = pullRequestLink(task);
  if (pullRequest || task.pr_number) {
    const value = el("span", { class: "value" });
    if (pullRequest) {
      const link = el("a", { class: "mono task-delivery-pr", text: pullRequest.number ? `#${pullRequest.number}` : "Pull request" });
      link.href = pullRequest.url;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      value.appendChild(link);
    } else {
      value.appendChild(el("span", { class: "mono", text: `#${task.pr_number}` }));
    }
    if (task.pr_status) value.appendChild(el("span", { class: "fact-note", text: ` · ${task.pr_status}` }));
    rows.push(row("pr", "Pull request", value));
  }
  if (task.job_run_id != null && task.job_run_id !== "") rows.push(row("run", "Run", buildRunValue(task)));
  const hands = buildHandsChain(task);
  if (hands) rows.push(row("hands", "Handled by", hands));
  // One row per typed relation; its id opens the related task on click.
  const relations = relationTargets(Array.isArray(task.relations) ? task.relations : []);
  for (const { key, label, target } of relations) {
    const open = el("button", { class: "mono task-delivery-relation", type: "button", text: target, title: `Open ${target}` });
    open.addEventListener("click", (e) => {
      e.stopPropagation();
      openVisibleTask(target, context);
    });
    const relationRow = row("relation", label, el("span", { class: "value" }, [open]));
    relationRow.dataset.relation = key;
    rows.push(relationRow);
  }
  const readiness = readinessOf(task);
  if (readiness || drain) rows.push(row("readiness", "Readiness", buildReadinessBlock(readiness, drain, task.workspace_id)));
  // ORB-00037: in the aggregate ("All workspaces") view each task carries its
  // owning workspace's filesystem location (home-abbreviated to ~ server-side);
  // show it in full here since the row only has room for the short name badge.
  if (task.workspace_root) {
    rows.push(row("location", "Location", el("span", { class: "value ws-location mono", text: task.workspace_root, title: task.workspace_root })));
  }
  return rows.length ? el("div", { class: "task-delivery-card" }, rows) : null;
}

// Where the task may run, as a word or two: "any", or the operating systems
// (macOS, Linux). The full sentence — which crew or tags decided it — is the
// readout's hint.
const OS_NAMES = { macos: "macOS", linux: "Linux", windows: "Windows" };

function osReadoutText(task) {
  const requirement = task.os_requirement;
  if (!requirement) return "any";
  if (["unresolved", "conflict"].includes(requirement.state)) return requirement.state;
  const anyOf = Array.isArray(requirement.any_of) ? requirement.any_of : [];
  return anyOf.length ? anyOf.map((os) => OS_NAMES[os] || os).join(" or ") : "any";
}

function osReadoutNote(task) {
  return task.os_requirement?.describe || (task.crew ? "any host" : "any host from OS tags (crew unassigned)");
}

function buildOsReadout(task) {
  const readout = el("span", { class: "value" }, [el("span", { class: "task-os-readout", text: osReadoutText(task) })]);
  readout.appendChild(infoHint(osReadoutNote(task)));
  return readout;
}

// ORB-12516: a pulled task's run lives in the executing machine's own job
// store, so the id alone is a dangling reference. When the recorded execution
// machine is this one the link still resolves; otherwise name the machine to
// inspect rather than linking somewhere it is not.
function buildRunValue(task) {
  const runId = String(task.job_run_id);
  const machine = task.job_run_machine;
  const known = Boolean(machine && machine.machine_id);
  const value = el("span", { class: "value run-value" });
  if (task.job_run_navigable !== false) {
    const link = el("a", { class: "mono", text: runId });
    link.href = runHref(runId);
    value.appendChild(link);
  } else {
    value.appendChild(el("span", { class: "mono", text: runId }));
  }
  // "on <machine>" follows the id on its line; what the id cannot do here (no
  // owner-local run) and an unrecorded machine are the hint.
  if (known) {
    // The space keeps the machine from reading as part of the run id.
    value.appendChild(document.createTextNode(" "));
    value.appendChild(buildExecutionProvenance(
      { known: true, machine_id: machine.machine_id, machine_name: machine.machine_name || null },
      { runId, workspace: task.workspace_id },
    ));
  }
  const notes = [];
  if (task.job_run_navigable === false) notes.push("No owner-local run for this id — inspect it on its execution machine.");
  if (!known) notes.push(`Execution machine ${formatExecutionLocation(null)}.`);
  if (notes.length) value.appendChild(infoHint(notes.join("\n")));
  return value;
}

// ORB-17931: the History tab as a timeline. Entries are grouped under a day
// heading, each row names its actor, a sentence built from the entry's fields
// and the raw event name (so it can still be grepped). A note that parses as
// JSON becomes a key/value details panel behind a disclosure instead of
// raw text. Display only: the entries are the API's, unchanged.
const HISTORY_CATEGORIES = [
  { key: "lifecycle", label: "Lifecycle" },
  { key: "context", label: "Context" },
  { key: "crew", label: "Crew" },
  { key: "run", label: "Runs" },
  { key: "warning", label: "Warnings" },
  { key: "other", label: "Other" },
];

const HISTORY_EVENT_CATEGORY = {
  created: "lifecycle",
  status_changed: "lifecycle",
  started: "lifecycle",
  proposal_approved: "lifecycle",
  review_approved: "lifecycle",
  operator_requeued: "lifecycle",
  context_creation_authorized: "context",
  context_files_pruned: "context",
  context_files_restored: "context",
  context_files_widened: "context",
  crew_assigned: "crew",
  crew_redrawn: "crew",
  claimed: "run",
  run_finished: "run",
  pull_request: "run",
  landing_completed: "run",
  transient_infra_requeued: "warning",
  escalated: "warning",
};

// Two entries of one event with the same payload recorded this close together
// are one write seen twice (the task's owner and the system both log a grant).
const HISTORY_MERGE_WINDOW_MS = 5000;

function historyText(text) {
  return document.createTextNode(text);
}

// The note as text, and as a JSON object or array when it is one.
function parseHistoryNote(note) {
  const text = typeof note === "string" ? note.trim() : "";
  if (text && (text[0] === "{" || text[0] === "[")) {
    try {
      const json = JSON.parse(text);
      if (json && typeof json === "object") return { text, json };
    } catch (_) {
      // Not JSON after all; the note reads as text.
    }
  }
  return { text, json: null };
}

function historyStatusFlow(entry) {
  if (!entry.from_status || !entry.to_status) return null;
  return el("span", { class: "history-flow" }, [
    statusPill(entry.from_status),
    el("span", { class: "history-arrow", text: "→", "aria-label": "to" }),
    statusPill(entry.to_status),
  ]);
}

// "assigned crew `opus` from pool:hard": the crew and, when named, its pool.
// The wording of a note is not a contract, so a note that does not read this
// way stays visible as text.
function historyCrewParts(verb, text) {
  const match = /crew\s+`?([\w.:-]+)`?(?:\s+from\s+`?([\w.:/-]+)`?)?/i.exec(text);
  if (!match) return { parts: [`${verb} a crew`] };
  const parts = [`${verb} crew `, el("strong", { text: match[1] })];
  if (match[2]) parts.push(` from ${match[2]}`);
  const rest = text.replace(match[0], "").replace(/[\s.]/g, "");
  return rest ? { parts, note: text } : { parts, note: "" };
}

function historyPullRequestParts(text) {
  const number = /\/pull\/(\d+)|#(\d+)/.exec(text);
  if (!number) return { parts: ["opened a pull request"] };
  const label = `#${number[1] || number[2]}`;
  const url = /https?:\/\/\S+/.exec(text);
  const target = url && isHttpUrl(url[0]) ? el("a", { text: label }) : el("strong", { text: label });
  if (url && isHttpUrl(url[0])) {
    target.href = url[0];
    target.target = "_blank";
    target.rel = "noopener noreferrer";
  }
  const rest = text.replace(url ? url[0] : number[0], "").replace(/[\s.#\d]/g, "");
  return { parts: ["opened pull request ", target], note: rest ? text : "" };
}

function historyCountLabel(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

// The structured payload of an entry: a JSON note, else its typed decision.
function historyStructured(entry, parsed) {
  if (parsed.json) return parsed.json;
  const decision = entry.decision;
  return decision && typeof decision === "object" ? decision : null;
}

function historyJsonSummary(data) {
  if (Array.isArray(data)) return historyCountLabel(data.length, "item");
  const pairs = Object.entries(data)
    .filter(([, value]) => value == null || typeof value !== "object")
    .slice(0, 2)
    .map(([key, value]) => {
      const text = String(value);
      return `${key}: ${text.length > 40 ? `${text.slice(0, 40)}…` : text}`;
    });
  return pairs.length ? pairs.join(" · ") : historyCountLabel(Object.keys(data).length, "field");
}

// The sentence after the actor, as `{ parts, note, fallback }`. `note` is the
// text still to show under the sentence; absent, the note's own text shows.
function describeHistoryEntry(entry, parsed, data) {
  const text = parsed.json ? "" : parsed.text;
  switch (entry.event) {
    case "created":
      return { parts: ["created the task"] };
    case "status_changed": {
      const flow = historyStatusFlow(entry);
      return flow ? { parts: ["changed status ", flow] } : { parts: ["changed status"] };
    }
    case "started": {
      const flow = historyStatusFlow(entry);
      return flow ? { parts: ["started work ", flow] } : { parts: ["started work"] };
    }
    case "claimed":
      return { parts: ["claimed the task"] };
    case "crew_assigned":
      return historyCrewParts("assigned", text);
    case "crew_redrawn":
      return historyCrewParts("redrew", text);
    case "proposal_approved":
      return { parts: ["approved the proposal"] };
    case "review_approved":
      return { parts: ["approved the review"] };
    case "context_creation_authorized": {
      if (!data || !Array.isArray(data.selectors)) return { parts: ["authorized context creation"] };
      if (data.selectors.length === 0) return { parts: ["revoked context creation authorization"] };
      const parts = ["authorized context: ", el("strong", { text: historyCountLabel(data.selectors.length, "selector") })];
      if (data.generation) parts.push(` · generation ${data.generation}`);
      return { parts, summarized: true };
    }
    case "context_files_pruned":
    case "context_files_restored":
    case "context_files_widened": {
      const verb = entry.event.slice("context_files_".length);
      const selectors = data && Array.isArray(data.selectors) ? data.selectors : null;
      if (!selectors) return { parts: [`${verb} context files`] };
      return { parts: [`${verb} context: `, el("strong", { text: historyCountLabel(selectors.length, "selector") })], summarized: true };
    }
    case "run_finished":
      return { parts: ["finished a run"] };
    case "pull_request":
      return historyPullRequestParts(text);
    case "landing_completed":
      return { parts: ["completed landing"] };
    case "operator_requeued":
      return { parts: ["requeued the task"] };
    case "transient_infra_requeued":
      return { parts: ["requeued the task after a transient infrastructure failure"] };
    case "escalated":
      return { parts: ["escalated the task"] };
    default:
      return { parts: [entry.event || "(no event)"], fallback: true };
  }
}

// Consecutive entries of one event carrying an identical payload, recorded by
// different actors within a few seconds, are one group.
function groupHistoryEntries(entries) {
  const groups = [];
  for (const item of entries) {
    const group = groups[groups.length - 1];
    const last = group && group.items[group.items.length - 1];
    const same = last
      && last.entry.event === item.entry.event
      && last.entry.from_status === item.entry.from_status
      && last.entry.to_status === item.entry.to_status
      && item.parsed.text !== ""
      && last.parsed.text === item.parsed.text
      && !group.items.some((member) => member.entry.by === item.entry.by)
      && Math.abs(item.time - last.time) <= HISTORY_MERGE_WINDOW_MS;
    if (same) group.items.push(item);
    else groups.push({ items: [item], category: item.category });
  }
  return groups;
}

function historyValueNode(value) {
  if (Array.isArray(value)) {
    return el("div", { class: "history-kv-list" }, value.map((item) =>
      el("div", { class: "history-kv-item", text: typeof item === "string" ? item : JSON.stringify(item) })));
  }
  if (value && typeof value === "object") return el("span", { text: JSON.stringify(value) });
  return el("span", { text: value == null ? "null" : String(value) });
}

function buildHistoryDetails(group, data, context, id) {
  const first = group.items[0];
  const list = el("dl", { class: "history-kv" });
  const add = (key, valueNode) => {
    list.appendChild(el("dt", { text: key }));
    list.appendChild(el("dd", {}, [valueNode]));
  };
  if (data && !Array.isArray(data)) {
    for (const [key, value] of Object.entries(data)) add(key, historyValueNode(value));
  } else if (data) {
    add("items", historyValueNode(data));
  }
  add(group.items.length > 1 ? `recorded ×${group.items.length}` : "recorded", el("div", { class: "history-kv-list" },
    group.items.map((item) => el("div", { class: "history-kv-item" }, [
      el("strong", { text: item.entry.by || "?" }),
      historyText(" · "),
      absTimeNode(context, item.entry.at),
    ]))));
  const details = el("div", { class: "history-details", id }, [list]);
  if (data) {
    const raw = first.parsed.json ? first.parsed.text : JSON.stringify(data);
    const copy = el("button", { class: "history-copy", text: "Copy raw JSON", title: "Copy the recorded payload" });
    copy.type = "button";
    copy.addEventListener("click", (event) => {
      event.stopPropagation();
      copyWithFeedback(copy, raw);
    });
    details.appendChild(el("div", { class: "history-details-actions" }, [copy]));
  }
  return details;
}

function buildHistoryRow(task, context, group, expandedKeys) {
  const first = group.items[0];
  const entry = first.entry;
  const data = historyStructured(entry, first.parsed);
  const described = describeHistoryEntry(entry, first.parsed, data);
  const category = HISTORY_CATEGORIES.find((candidate) => candidate.key === group.category);

  const sentence = el("span", { class: "history-sentence" }, [
    el("span", { class: "actor", text: entry.by || "?" }),
    historyText(described.fallback ? ": " : " "),
  ]);
  for (const part of described.parts) sentence.appendChild(typeof part === "string" ? historyText(part) : part);
  if (group.items.length > 1) {
    sentence.appendChild(el("span", {
      class: "history-merge",
      text: `×${group.items.length}`,
      title: `Recorded ${group.items.length} times by ${group.items.map((item) => item.entry.by || "?").join(", ")}`,
    }));
  }

  // A payload shows as a short summary; its fields wait in the details panel.
  const body = el("div", { class: "history-main" }, [sentence]);
  const note = described.note !== undefined ? described.note : (data ? "" : first.parsed.text);
  if (data && !described.summarized) {
    body.appendChild(el("span", { class: "history-summary", text: historyJsonSummary(data) }));
  }
  if (note) body.appendChild(el("span", { class: "history-note", text: note }));

  const head = el("div", { class: "history-head" }, [
    body,
    el("span", { class: "history-event", text: entry.event || "?" }),
  ]);
  const row = el("div", { class: "history-line" });
  row.dataset.category = group.category;
  const time = el("span", { class: "history-time", text: formatClock(entry.at, { zone: false }), title: fmtAbsTimeValue(context, entry.at) });
  const node = el("span", { class: "history-node", title: category ? category.label : "" }, [el("span", { class: "history-dot" })]);
  node.setAttribute("aria-hidden", "true");
  row.append(time, node);

  const content = el("div", { class: "history-content" }, [head]);
  row.appendChild(content);

  const expandable = Boolean(data) || group.items.length > 1;
  if (expandable) {
    const key = `${task.id}:${first.index}`;
    const id = `history-${task.id}-${first.index}`.replace(/[^\w-]/g, "-");
    const details = buildHistoryDetails(group, data, context, id);
    const toggle = el("button", { class: "history-toggle", text: "Details" });
    const setOpen = (open) => {
      details.hidden = !open;
      toggle.setAttribute("aria-expanded", String(open));
      row.classList.toggle("expanded", open);
      if (open) expandedKeys.add(key);
      else expandedKeys.delete(key);
    };
    makeDisclosure(head, toggle, {
      expanded: false,
      controls: id,
      onToggle: (event) => {
        // A click on text being selected, or on a link, is not a toggle.
        if (event && event.target && event.target.closest && event.target.closest("a")) return;
        setOpen(details.hidden);
      },
    });
    body.appendChild(toggle);
    content.appendChild(details);
    setOpen(expandedKeys.has(key));
  }
  return row;
}

// Every meaningful history entry as a timeline, newest first by default.
function buildTaskHistory(task, context) {
  // ORB-10311: legacy bare `commented` stubs are comment noise; the thread has
  // its own tab.
  const meaningful = Array.isArray(task.history) ? task.history.filter((h) => h && h.event !== "commented") : [];
  if (meaningful.length === 0) {
    return { count: 0, node: el("p", { class: "field-empty", text: "No history recorded." }) };
  }
  const items = meaningful.map((entry, index) => ({
    entry,
    index,
    time: Date.parse(entry.at),
    parsed: parseHistoryNote(entry.note),
    category: HISTORY_EVENT_CATEGORY[entry.event] || "other",
  }));
  // Recorded order is chronological; an unreadable time keeps the recorded
  // order rather than guessing.
  if (items.every((item) => Number.isFinite(item.time))) items.sort((a, b) => a.time - b.time || a.index - b.index);
  const groups = groupHistoryEntries(items);
  const counts = { all: items.length };
  for (const item of items) counts[item.category] = (counts[item.category] || 0) + 1;

  const view = { filter: "all", newestFirst: true };
  const expandedKeys = new Set();
  const timeline = el("div", { class: "history-timeline" });
  const chips = [];
  const orders = [];

  const render = () => {
    const shown = groups.filter((group) => view.filter === "all" || group.category === view.filter);
    if (view.newestFirst) shown.reverse();
    const days = [];
    for (const group of shown) {
      const at = group.items[0].entry.at;
      const day = formatDate(at);
      let section = days[days.length - 1];
      if (!section || section.day !== day) {
        section = { day, heading: formatDayHeading(at), rows: [] };
        days.push(section);
      }
      section.rows.push(group);
    }
    timeline.replaceChildren(...days.map((section) => el("div", { class: "history-day" }, [
      el("div", { class: "history-day-head", text: section.heading }),
      ...section.rows.map((group) => buildHistoryRow(task, context, group, expandedKeys)),
    ])));
  };

  const choose = (buttons, activeOf) => {
    for (const button of buttons) button.setAttribute("aria-pressed", String(activeOf(button)));
  };
  const chip = (key, label) => {
    const button = el("button", { class: "history-chip" }, [
      historyText(label),
      el("span", { class: "history-chip-count", text: String(counts[key] || 0) }),
    ]);
    button.type = "button";
    button.dataset.filter = key;
    button.addEventListener("click", () => {
      view.filter = key;
      choose(chips, (candidate) => candidate.dataset.filter === view.filter);
      render();
    });
    chips.push(button);
    return button;
  };
  const order = (newestFirst, label) => {
    const button = el("button", { class: "history-order", text: label });
    button.type = "button";
    button.dataset.order = newestFirst ? "newest" : "oldest";
    button.addEventListener("click", () => {
      view.newestFirst = newestFirst;
      choose(orders, (candidate) => candidate.dataset.order === (view.newestFirst ? "newest" : "oldest"));
      render();
    });
    orders.push(button);
    return button;
  };

  const filters = el("div", { class: "history-filters", role: "group", "aria-label": "Filter history by category" }, [
    chip("all", "All"),
    ...HISTORY_CATEGORIES.filter((category) => counts[category.key]).map((category) => chip(category.key, category.label)),
  ]);
  const sort = el("div", { class: "history-order-toggle", role: "group", "aria-label": "Order" }, [
    order(true, "Newest"),
    order(false, "Oldest"),
  ]);
  choose(chips, (candidate) => candidate.dataset.filter === view.filter);
  choose(orders, (candidate) => candidate.dataset.order === "newest");

  const wrap = el("div", { class: "history-list" }, [
    el("div", { class: "history-toolbar" }, [filters, sort]),
    timeline,
  ]);
  render();
  // Day headings and times follow the dashboard clock: repaint on a switch.
  const stopClock = onClockZoneChange(render);
  wrap.teardown = stopClock;
  return { count: meaningful.length, node: wrap };
}

const DETAIL_TAB_NAMES = ["overview", "context_files", "delivery", "history", "runs", "artifacts", "comments"];

// The detail's tab strip and its panels. Every panel is built with the detail
// and switching only toggles which one shows, so a draft in one tab survives
// a visit to another. Arrow keys move between tabs (automatic activation).
// A tab marked `available: false` (such as Delivery without a claim) is left out of
// the strip until `detail.setTabAvailable` adds it.
function buildDetailTabs(task, detail, tabs, context) {
  const base = `detail-${task.id}`;
  const list = el("div", { class: "detail-tabs", role: "tablist", "aria-label": "Task detail sections" });
  const buttons = new Map();
  const panels = new Map();
  const available = new Set(tabs.filter((tab) => tab.available !== false).map((tab) => tab.name));
  const runsNode = tabs.find((tab) => tab.name === "runs")?.content || null;
  let current = null;
  // A remembered tab that is not in the strip yet (Delivery before its read
  // lands) shows Overview and opens once it appears, unless the operator
  // picks a tab first.
  let waiting = null;
  const activate = (name, { focus = false } = {}) => {
    const next = panels.has(name) && available.has(name) ? name : "overview";
    waiting = next === name ? null : name;
    current = next;
    if (next === name) activeDetailTab = next;
    for (const [key, button] of buttons) {
      const on = key === next;
      button.setAttribute("aria-selected", String(on));
      button.tabIndex = on ? 0 : -1;
      button.classList.toggle("active", on);
      panels.get(key).hidden = !on;
    }
    const footer = detail.querySelector(".task-action-footer");
    if (footer) footer.hidden = next !== "overview";
    if (focus) buttons.get(next).focus();
    if (next === "runs") loadTaskRuns(task, context, runsNode);
  };
  for (const tab of tabs) {
    const button = el("button", {
      class: "detail-tab",
      type: "button",
      role: "tab",
      id: `${base}-tab-${tab.name}`,
      "aria-controls": `${base}-panel-${tab.name}`,
    }, [
      el("span", { class: "detail-tab-label", text: tab.label }),
      el("span", { class: "detail-tab-count", text: tab.count || tab.showZeroCount ? String(tab.count) : "" }),
    ]);
    button.dataset.tab = tab.name;
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      activate(tab.name);
    });
    buttons.set(tab.name, button);
    if (available.has(tab.name)) list.appendChild(button);
    const panel = el("div", {
      class: "detail-tabpanel",
      role: "tabpanel",
      id: `${base}-panel-${tab.name}`,
      "aria-labelledby": button.id,
    }, [tab.content]);
    panel.dataset.tabPanel = tab.name;
    panels.set(tab.name, panel);
  }
  list.addEventListener("keydown", (event) => {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const names = tabs.map((tab) => tab.name).filter((name) => available.has(name));
    const at = Math.max(0, names.indexOf(current));
    let next = null;
    if (event.key === "ArrowRight") next = names[(at + 1) % names.length];
    else if (event.key === "ArrowLeft") next = names[(at - 1 + names.length) % names.length];
    else if (event.key === "Home") next = names[0];
    else if (event.key === "End") next = names[names.length - 1];
    if (!next) return;
    event.preventDefault();
    activate(next, { focus: true });
  });
  detail.activateTab = (name, opts) => activate(DETAIL_TAB_NAMES.includes(name) ? name : "overview", opts);
  detail.setTabAvailable = (name, on) => {
    const button = buttons.get(name);
    if (!button || available.has(name) === Boolean(on)) return;
    if (on) {
      available.add(name);
      const after = tabs.slice(tabs.findIndex((tab) => tab.name === name) + 1).find((tab) => available.has(tab.name));
      list.insertBefore(button, after ? buttons.get(after.name) : null);
      if (waiting === name) activate(name);
    } else {
      available.delete(name);
      if (button.parentNode) button.parentNode.removeChild(button);
      if (current === name) activate(name);
    }
  };
  return [list, ...panels.values()];
}

// The Runs tab: the runs that carried this task, newest first, from the job
// store's task filter. Read when the tab shows, at most once per freshness
// window, and painted in place so a read never rebuilds the detail.
function taskRunCount(task) {
  const load = taskRunLoads.get(task.id);
  return load && load.items ? load.total : null;
}

function loadTaskRuns(task, context, fallback = null) {
  const previous = taskRunLoads.get(task.id);
  const target = () => document.getElementById(`detail-${task.id}-runs`) || fallback;
  if (previous && (previous.pending || Date.now() - previous.at < TASK_RUNS_FRESH_MS)) return;
  if (isAggregateView() && !task.workspace_id) {
    taskRunLoads.set(task.id, { pending: false, error: "Task workspace is unknown", items: null, total: null, at: Date.now() });
    renderTaskRuns(target(), task, context);
    return;
  }
  const visit = captureWorkspaceVisit();
  const load = { pending: true, error: null, items: previous ? previous.items : null, total: previous ? previous.total : null, at: Date.now() };
  taskRunLoads.set(task.id, load);
  renderTaskRuns(target(), task, context);
  fetchJson(taskWorkspacePath(task, `/api/job-runs?task_id=${encodeURIComponent(task.id)}&limit=${TASK_RUNS_LIMIT}`)).then(
    (payload) => {
      load.items = Array.isArray(payload && payload.items) ? payload.items : [];
      load.total = Number.isFinite(payload && payload.total) ? payload.total : load.items.length;
      load.error = null;
    },
    (error) => {
      load.error = (error && error.message) || String(error);
    },
  ).finally(() => {
    load.pending = false;
    load.at = Date.now();
    if (!visit.isCurrent() || taskRunLoads.get(task.id) !== load) return;
    renderTaskRuns(target(), task, context);
  });
}

function renderTaskRuns(node, task, context) {
  if (!node) return;
  const load = taskRunLoads.get(task.id);
  const children = [];
  if (!load || (load.pending && !load.items && !load.error)) {
    const note = el("p", { class: "panel-placeholder", text: "Loading runs…", role: "status", "aria-live": "polite" });
    children.push(note);
  } else {
    if (load.error) {
      const retry = el("button", { class: "action", type: "button", text: "Retry" });
      retry.addEventListener("click", (event) => {
        event.stopPropagation();
        taskRunLoads.delete(task.id);
        loadTaskRuns(task, context, node);
      });
      children.push(el("div", { class: "action-error", text: `Could not load runs: ${load.error}`, role: "alert" }), retry);
    }
    if (load.items && load.items.length === 0) {
      children.push(el("p", { class: "field-empty", text: "No runs recorded for this task." }));
    } else if (load.items) {
      const list = el("ul", { class: "task-run-list" });
      for (const run of load.items) list.appendChild(buildTaskRun(run, task, context));
      children.push(list);
      if (load.total > load.items.length) {
        children.push(el("p", { class: "task-runs-more", text: `Latest ${load.items.length} of ${load.total} runs.` }));
      }
    }
  }
  node.replaceChildren(...children);
  const count = document.getElementById(`detail-${task.id}-tab-runs`)?.querySelector?.(".detail-tab-count");
  if (count) count.textContent = load && load.items ? String(load.total) : "";
}

function buildTaskRun(run, task, context) {
  const runId = String(run.run_id || "");
  const link = el("a", { class: "mono task-run-id", text: runId });
  link.href = runHref(runId, task.workspace_id ? { workspace: task.workspace_id } : {});
  const when = run.started_at || run.scheduled_at || run.finished_at;
  const { durationMs } = elapsedDurationInfo(run);
  return el("li", { class: "task-run" }, [
    el("div", { class: "task-run-head" }, [link, stateCell(run.state || "unknown")]),
    el("div", { class: "task-run-meta" }, [
      run.job_id ? el("span", { class: "task-run-job", text: run.job_id }) : null,
      when ? absTimeNode(context, when) : null,
      durationMs != null ? el("span", { text: fmtDuration(durationMs) }) : null,
    ]),
    run.error_message ? el("div", { class: "task-run-error", text: run.error_message }) : null,
  ]);
}

// The expanded row while its first detail read is pending or has failed. A
// failed read offers a retry in place; the row's own toggle is the other one.
function buildTaskDetailPlaceholder(task, state, context) {
  const detail = el("div", { class: "row-detail" });
  detail.addEventListener("click", (e) => e.stopPropagation());
  const note = el("div", {
    class: state.error ? "panel-placeholder action-error" : "panel-placeholder",
    text: state.error
      ? `Unable to load ${task.id}: ${state.error}`
      : `Loading ${task.id}…`,
  });
  note.setAttribute("role", "status");
  note.setAttribute("aria-live", "polite");
  detail.appendChild(note);
  if (state.error) {
    const retry = el("button", { class: "action", text: "Retry" });
    retry.type = "button";
    retry.addEventListener("click", (e) => {
      e.stopPropagation();
      forgetTaskDetailLoad(task.id);
      renderTasks(taskList(context), context);
    });
    detail.appendChild(el("div", { class: "actions" }, [retry]));
  }
  return detail;
}

const APPROVE_STATUSES = new Set(["proposed", "review"]);
const REJECT_STATUSES = new Set(["proposed", "review", "backlog"]);
// Ship dispatches a task through the pipeline, which admits it out of backlog —
// so backlog is the only status where the control means anything.
const SHIP_STATUSES = new Set(["backlog"]);
// Rows whose quick-action cell links the delivery pull request, when the task
// has one and the cell holds nothing else.
const PULL_REQUEST_LINK_STATUSES = new Set(["review", "in-progress"]);
// Group heading for the status targets the lifecycle table refuses. Choosing one
// is the operator override recorded in task history as `forced`.
const FORCED_STATUS_GROUP_LABEL = "force (off-table)";

function buildActionsRow(task, detail, context, { includeLifecycle = true, includeComment = true } = {}) {
  const kind = includeComment ? "comment-actions" : "lifecycle-actions";
  const actions = el("div", { class: `actions task-action-row ${kind}` });
  const mutable = canMutateTask(task);
  if (includeLifecycle && SHIP_STATUSES.has(task.status)) {
    const shipped = shipInFlightTaskIds.has(taskDispatchIdentity(task));
    const btn = el("button", {
      class: "action ship",
      text: shipped ? "shipping" : "ship",
      title: shipped
        ? "A ship run is already in flight for this task"
        : "Dispatch this task through the pipeline with its own crew",
    });
    btn.disabled = shipped || !mutable;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      shipTask(task, detail, btn, context);
    });
    actions.appendChild(btn);
  }
  if (includeComment) {
    const btn = el("button", { class: "action comment", text: "comment" });
    btn.disabled = !mutable;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      showCommentForm(task, detail, actions, context);
    });
    actions.appendChild(btn);
  }
  if (includeLifecycle && APPROVE_STATUSES.has(task.status)) {
    const btn = el("button", { class: "action approve", text: "approve" });
    btn.disabled = !mutable;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (task.status === "review") approveReviewTask(task, detail, btn, context);
      else runAction(task, "approve", detail, null, btn, context);
    });
    actions.appendChild(btn);
  }
  if (includeLifecycle && REJECT_STATUSES.has(task.status)) {
    const btn = el("button", { class: "action reject", text: "reject" });
    btn.disabled = !mutable;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      showRejectForm(task, detail, actions, context);
    });
    actions.appendChild(btn);
  }
  if (includeLifecycle && task.status === "review" && taskPullRequestNumber(task)) appendFixPrControls(actions, task, detail, context, mutable);
  if (includeLifecycle && task.status === "proposed") appendPilotControls(actions, task, detail, context, mutable);
  if (includeLifecycle && escalationTriage(task)) appendTriageControls(actions, task, detail, context, mutable);
  if (includeLifecycle && task.status !== "archived") {
    const btn = el("button", { class: "action archive", text: "archive" });
    btn.disabled = !mutable;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (window.confirm(`Archive task ${task.id}?`)) {
        runAction(task, "archive", detail, null, btn, context);
      }
    });
    actions.appendChild(btn);
  }
  if (includeLifecycle || includeComment) {
    const refusal = hostWriteRefusal();
    if (refusal) actions.appendChild(el("span", { class: "host-read-only-note", text: refusal }));
  }
  return actions.children.length ? actions : null;
}

function buildStatusUpdateControl(task, context) {
  const cell = el("span", { class: "status-cell" });
  cell.dataset.status = task.status || "";
  const targets = statusTransitions(task).map((transition) => transition.status);
  // ORB-12445: an operator may move a task to any status from any status, the
  // same override the bare CLI spells `--force`. The governed targets keep the
  // plain list; everything else the lifecycle table refuses is offered under a
  // marked group so the two are never confused, and a terminal task is no
  // longer a dead control.
  const forcedTargets = statusOrder(context).filter(
    (status) => status && status !== task.status && !targets.includes(status),
  );
  const mutable = canMutateTask(task);
  const feedback = statusFeedback.get(task.id);
  const label = `Update status for ${task.id}`;
  const select = el("select", {
    class: "task-status-select mono",
    title: mutable ? label : aggregateRefusalTitle(label, "change status"),
  });
  select.setAttribute("aria-label", label);
  const placeholder = el("option", { text: task.status || "status" });
  placeholder.value = "";
  placeholder.disabled = true;
  placeholder.selected = true;
  placeholder.hidden = true;
  select.appendChild(placeholder);

  for (const status of targets) {
    const option = el("option", { text: status });
    option.value = status;
    select.appendChild(option);
  }

  if (forcedTargets.length > 0) {
    const group = el("optgroup", {
      title: `Off-table targets — forcing one overrides the lifecycle table for ${task.id}`,
    });
    group.setAttribute("label", FORCED_STATUS_GROUP_LABEL);
    for (const status of forcedTargets) {
      const option = el("option", { text: `${status} ⚠` });
      option.value = status;
      group.appendChild(option);
    }
    select.appendChild(group);
  }

  if (
    targets.length + forcedTargets.length === 0 ||
    !mutable ||
    (feedback && feedback.kind === "pending")
  ) {
    select.disabled = true;
  }

  stopRowInteraction(cell);
  stopRowInteraction(select);
  select.addEventListener("change", (event) => {
    event.stopPropagation();
    const targetStatus = select.value;
    if (!targetStatus) return;
    applyTaskStatusChange(task, targetStatus, context);
  });
  cell.appendChild(select);
  const chip = el("span", { class: "task-select-chip", text: task.status || "status" });
  chip.setAttribute("aria-hidden", "true");
  cell.appendChild(chip);
  const feedbackNode = buildMutationFeedback(feedback, () => {
    if (feedback && feedback.undo) applyTaskStatusChange(task, feedback.undo.previousValue, context);
  });
  if (feedbackNode) cell.appendChild(feedbackNode);
  return cell;
}

// The list row overlays a chip on its select; the detail styles the select
// itself inside a visible field label and needs no duplicate value.
function buildCrewUpdateControl(task, context, { withChip = true } = {}) {
  const cell = el("span", { class: "crew-cell" });
  if (isAggregateView()) {
    // The aggregate task list has no single crew registry to validate against.
    // Keep the task's own crew visible without marking it missing or offering
    // a workspace-scoped edit control.
    cell.textContent = explicitCrewValue(task) || resolvedCrewName(task);
    cell.title = "Crew settings are workspace-specific; select a workspace to edit.";
    return cell;
  }

  const mutable = canMutateTask(task);
  const feedback = crewFeedback.get(task.id);
  const label = `Update crew for ${task.id}`;
  const select = el("select", {
    class: "task-crew-select mono",
    title: mutable ? label : aggregateRefusalTitle(label, "change crew"),
  });
  select.setAttribute("aria-label", label);
  const currentValue = explicitCrewValue(task);
  const staleCurrentValue = hasStaleExplicitCrew(task);
  select.dataset.currentValue = currentValue;
  select.dataset.staleCurrentValue = staleCurrentValue ? currentValue : "";

  const defaultOption = el("option", {
    text: defaultCrewOptionText(task),
  });
  defaultOption.value = "";
  select.appendChild(defaultOption);

  const crews = Array.isArray(lastCrewPayload.crews) ? lastCrewPayload.crews : [];
  for (const crew of crews) {
    const option = el("option", {
      text: crew.name,
      title: crewOptionTitle(crew),
    });
    option.value = crew.name;
    select.appendChild(option);
  }

  if (staleCurrentValue) {
    const option = el("option", {
      text: `${currentValue} (missing)`,
      title: "Configured crew no longer found",
    });
    option.value = currentValue;
    select.appendChild(option);
  }

  if (crews.length === 0) {
    select.disabled = true;
    defaultOption.textContent = "crew unavailable";
  }
  if (!mutable || (feedback && feedback.kind === "pending")) {
    select.disabled = true;
  }

  select.value = currentValue;
  stopRowInteraction(cell);
  stopRowInteraction(select);
  select.addEventListener("change", (event) => {
    event.stopPropagation();
    applyTaskCrewChange(task, select.value || "", context);
  });

  cell.appendChild(select);
  const crewLabel = staleCurrentValue ? `${currentValue} (missing)` : currentValue || defaultOption.textContent;
  select.title += `: ${crewLabel}`;
  if (withChip) {
    const chip = el("span", {
      class: "task-select-chip",
      text: crews.length === 0 ? "unavailable" : staleCurrentValue
        ? `${currentValue} (missing)`
        : currentValue || resolvedCrewName(task),
    });
    chip.setAttribute("aria-hidden", "true");
    cell.appendChild(chip);
  }
  const feedbackNode = buildMutationFeedback(feedback, () => {
    if (feedback && feedback.undo) applyTaskCrewChange(task, feedback.undo.previousValue, context);
  });
  if (feedbackNode) cell.appendChild(feedbackNode);
  return cell;
}

async function applyTaskStatusChange(task, nextStatus, context) {
  // A transition can first read the full record for its evidence requirement.
  // Leaving that workspace abandons the interaction before any write, and a
  // later mutation result must not be applied to another workspace visit.
  const visit = captureWorkspaceVisit();
  const mutationPath = visit.path(taskMutationPath(task));
  if (!nextStatus || nextStatus === task.status || !canMutateTask(task)) return;
  let transition = statusTransition(task, nextStatus);
  // A target the projection did not offer is an override, not a mistake: the
  // operator gets one confirm naming the move, and the PATCH carries `force`
  // so the server applies it and records the `forced` history event. A target
  // that is not a lifecycle status at all stays a refusal.
  const forced = !transition;
  // A refusal before any write. Beside an open detail's control it stays until
  // the next change; on a folded row or notice it expires like the feedback it
  // replaced, so a refused undo does not linger there.
  const refuse = (text) => {
    statusFeedback.set(task.id, { kind: "error", text });
    renderTasks(taskList(context), context);
    if (foldedStatusTasks.has(task.id) && selectedTaskId !== task.id) {
      scheduleFeedbackExpiry(statusFeedback, task.id, context, MUTATION_UNDO_WINDOW_MS + 500);
    }
  };
  // A summary row lists the governed targets but not what each one requires —
  // the `done` requirement depends on the task's job run, which only the
  // detail endpoint reads. Ask it before deciding whether to prompt.
  if (!forced && !("required_field" in transition)) {
    statusFeedback.set(task.id, { kind: "pending", text: "checking…" });
    renderTasks(taskList(context), context);
    let detail;
    try {
      detail = await loadTaskDetail(task, context);
    } catch (error) {
      if (!visit.isCurrent()) return;
      refuse(`status update failed: ${error.message || String(error)}`);
      return;
    }
    if (!visit.isCurrent()) return;
    transition = detail.status === task.status ? statusTransition(detail, nextStatus) : null;
    if (!transition) {
      refuse(`status update unavailable: ${task.id} changed underneath this page; refresh`);
      return;
    }
    task = mergeTaskRecord(task, detail);
  }
  if (forced && !statusOrder(context).includes(nextStatus)) {
    refuse(`status update unavailable: ${task.status} cannot move to ${nextStatus}`);
    return;
  }
  if (forced && !confirmForcedStatusChange(task, nextStatus)) {
    refuse(typeof window.confirm === "function"
      ? "status update cancelled"
      : `status update unavailable: forcing ${task.status} → ${nextStatus} needs a confirmation`);
    return;
  }

  const payload = forced ? { status: nextStatus, force: true } : { status: nextStatus };
  if (!forced && transition.required_field) {
    const evidence = collectStatusTransitionEvidence(task, nextStatus, transition.required_field);
    if (!evidence) {
      refuse(statusTransitionEvidenceUnavailable(transition.required_field));
      return;
    }
    payload[transition.required_field] = evidence;
  }

  const previousValue = task.status;
  statusFeedback.set(task.id, { kind: "pending", text: "saving…" });
  renderTasks(taskList(context), context);
  try {
    const updatedTask = await patchJson(mutationPath, payload);
    if (!visit.isCurrent()) return;
    const merged = applyUpdatedTask(updatedTask, context) || task;
    const feedback = {
      kind: "success",
      text: forced ? "status forced" : "status saved",
    };
    const reverse = statusTransition(updatedTask, previousValue);
    if (reverse && !reverse.required_field) {
      feedback.undo = { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS };
    }
    statusFeedback.set(task.id, feedback);
    // Fold the open detail in; an undo from a folded row keeps it folded. A
    // pinned detail is the only view of a filtered-out task, so it stays open.
    // A full page is that task's own view, so it stays too.
    const pinned = pinnedExternalTask && pinnedExternalTask.id === task.id;
    if (!pinned && taskPageId !== task.id && (selectedTaskId === task.id || foldedStatusTasks.has(task.id))) {
      if (selectedTaskId === task.id) rowFocusAfterRender = task.id;
      collapseTask(task.id);
      foldedStatusTasks.set(task.id, merged);
    }
  } catch (error) {
    if (!visit.isCurrent()) return;
    statusFeedback.set(task.id, {
      kind: "error",
      text: `status update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  renderTasks(taskList(context), context);
  scheduleFeedbackExpiry(statusFeedback, task.id, context, MUTATION_UNDO_WINDOW_MS + 500);
}

// One confirm, naming the exact move, before the override leaves the browser.
// A host without `confirm` cannot obtain it, so the forced write is refused
// rather than sent unconfirmed.
function confirmForcedStatusChange(task, nextStatus) {
  if (typeof window.confirm !== "function") return false;
  return window.confirm(
    `Force ${task.id} from ${task.status} to ${nextStatus}? This overrides the lifecycle ` +
      `table and is recorded in task history as a forced change.`,
  );
}

function collectStatusTransitionEvidence(task, nextStatus, requiredField) {
  if (typeof window.prompt !== "function") return null;
  const label = requiredField === "plan" ? "execution plan" : "completion summary";
  const currentValue = requiredField === "plan" ? task.plan : task.execution_summary;
  const value = window.prompt(
    `A non-empty ${label} is required before moving ${task.id} to ${nextStatus}.`,
    currentValue || "",
  );
  return value && value.trim() ? value.trim() : null;
}

function statusTransitionEvidenceUnavailable(requiredField) {
  const label = requiredField === "plan" ? "execution plan" : "completion summary";
  return `status update unavailable: a non-empty ${label} is required`;
}

async function applyTaskCrewChange(task, nextValue, context) {
  const currentTask = currentCrewTask(task, context);
  const visit = captureWorkspaceVisit();
  const mutationPath = visit.path(taskMutationPath(currentTask));
  const previousValue = explicitCrewValue(currentTask);
  if (nextValue === previousValue || !canMutateTask(currentTask)) return;
  crewFeedback.set(currentTask.id, { kind: "pending", text: "saving…" });
  renderTasks(taskList(context), context);
  try {
    const updatedTask = await patchJson(mutationPath, { crew: nextValue || null });
    if (!visit.isCurrent()) return;
    applyUpdatedTask(updatedTask, context);
    crewFeedback.set(currentTask.id, {
      kind: "success",
      text: "crew saved",
      undo: { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS },
    });
  } catch (error) {
    if (!visit.isCurrent()) return;
    crewFeedback.set(currentTask.id, {
      kind: "error",
      text: `crew update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  renderTasks(taskList(context), context);
  scheduleFeedbackExpiry(crewFeedback, currentTask.id, context, MUTATION_UNDO_WINDOW_MS + 500);
}

// A failed action is announced when it appears: the row's own controls are the
// only thing the operator is looking at, and a plain div is never read out.
function actionErrorNode(text, remedy) {
  const node = el("div", { class: "action-error", text });
  node.setAttribute("role", "alert");
  if (remedy) node.appendChild(el("div", { class: "action-error-remedy", text: remedy }));
  return node;
}

// Closing a form swaps the actions row back in, which drops focus to the page.
// Put it back on the control that opened the form.
function restoreActionFocus(actions, selector) {
  const opener = actions.querySelector?.(selector);
  if (opener && typeof opener.focus === "function") opener.focus();
}

// Why Run pilot is unavailable for this task, or "" when it is offered. A pilot
// run already holding the task is the server's refusal too, so the reason here
// only saves the round trip.
function pilotDisabledReason(task, mutable, running) {
  if (!mutable) return aggregateRefusalTitle("Run pilot", "run a pilot");
  if (running) return `A pilot run (${running.runId}) is already assessing this task`;
  const readiness = readinessOf(task);
  if (readiness && readiness.preparing) return "A task-pilot run is already preparing this task";
  return "";
}

// Run pilot on a proposed task: one explicit task-pilot run for this task. It
// refreshes the task's selectors and complexity; it never approves or ships it.
function appendPilotControls(actions, task, detail, context, mutable) {
  const running = pilotRunsByTask.get(taskDispatchIdentity(task));
  const reason = pilotDisabledReason(task, mutable, running);
  const btn = el("button", {
    class: "action pilot",
    text: running ? "piloting…" : "run pilot",
    title: reason || "Assess this task with a task-pilot run: it re-checks selectors and complexity and never approves or dispatches the task",
  });
  btn.disabled = Boolean(reason);
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    runPilot(task, detail, btn, context);
  });
  actions.appendChild(btn);
  if (running) {
    const link = el("a", { class: "task-quick-link pilot-run-link mono", text: running.runId, title: `Open pilot run ${running.runId}` });
    link.href = runHref(running.runId, task.workspace_id ? { workspace: task.workspace_id } : {});
    actions.appendChild(link);
  }
  const refusal = pilotRefusals.get(taskDispatchIdentity(task));
  if (refusal) {
    const note = actionErrorNode(refusal);
    note.style.flexBasis = "100%";
    actions.appendChild(note);
  }
}

function pilotSignature(task) {
  const key = taskDispatchIdentity(task);
  const running = pilotRunsByTask.get(key);
  return `${running ? running.runId : ""}|${pilotRefusals.get(key) || ""}`;
}

async function runPilot(task, detail, btnNode, context) {
  const visit = captureWorkspaceVisit();
  const key = taskDispatchIdentity(task);
  if (!canMutateTask(task) || pilotRunsByTask.has(key)) return;
  const requestPath = visit.path(taskMutationPath(task, "/pilot"));
  pilotRefusals.delete(key);
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  for (const b of detail.querySelectorAll(".action")) b.disabled = true;
  btnNode.innerHTML = `<span class="spinner"></span>wait`;
  let result;
  try {
    result = await postJson(requestPath, {});
  } catch (error) {
    if (!visit.isCurrent()) return;
    const message = `run pilot failed: ${error.message || String(error)}`;
    pilotRefusals.set(key, message);
    renderTasks(taskList(context), context);
    setTimeout(() => {
      if (pilotRefusals.get(key) !== message) return;
      pilotRefusals.delete(key);
      renderTasks(taskList(context), context);
    }, PILOT_REFUSAL_MS);
    return;
  }
  const runId = result && result.run_id ? String(result.run_id) : "";
  if (!visit.isCurrent()) return;
  if (runId) pilotRunsByTask.set(key, { runId });
  taskActionNotice = `${task.id}: pilot run ${runId || "(no run id)"} ${result && result.state ? result.state : "submitted"}`;
  renderTasks(taskList(context), context);
  if (runId) watchPilotRun(task, key, runId, visit, context);
}

// Follow the submitted run until it settles, then refresh: the task then shows
// the new `task_pilot_applied` entry or the pilot's warnings. A run that cannot
// be read any more is dropped; `readiness.preparing` takes over for a live one.
async function watchPilotRun(task, key, runId, visit, context) {
  const path = visit.path(taskWorkspacePath(task, `/api/runs/${encodeURIComponent(runId)}`));
  let state = "";
  for (let polls = 0; polls < PILOT_POLL_LIMIT; polls += 1) {
    await new Promise((resolve) => setTimeout(resolve, PILOT_POLL_MS));
    if (!visit.isCurrent()) return;
    try {
      // The run detail is `{ run, steps }`.
      const detail = await fetchJson(path);
      const run = detail && detail.run ? detail.run : detail;
      state = run && run.state ? String(run.state) : "";
    } catch (_) {
      break;
    }
    if (PILOT_TERMINAL_STATES.has(state)) break;
  }
  pilotRunsByTask.delete(key);
  if (!visit.isCurrent()) return;
  if (state) taskActionNotice = `${task.id}: pilot run ${runId} ${state}`;
  await refreshTasks(context);
}

// The detail's escalation triage readout, or null when no Triage action is
// shown: the task is neither held by pilot warnings nor blocked. The server
// derives it from the pipeline's own candidacy, so the panel enables the
// action exactly when the endpoint would submit.
function escalationTriage(task) {
  const triage = task && task.escalation_triage;
  return triage && typeof triage === "object" ? triage : null;
}

// Why Triage is unavailable for this task, or "" when it is offered. The
// readout's reason is the server's refusal too, so this only saves the
// round trip.
function triageDisabledReason(task, mutable, running) {
  if (!mutable) return aggregateRefusalTitle("Triage", "triage a task");
  if (running) return `A triage run (${running.runId}) is already triaging this task`;
  const triage = escalationTriage(task);
  if (!triage) return "Escalation triage is not available for this task";
  if (triage.available) return "";
  const detail = triage.detail || triage.reason || "it does not qualify now";
  return `Triage unavailable — ${detail}`;
}

// Triage on a held proposed task or a blocked task: one explicit
// escalation triage run for this task. It only proposes a decision or names
// the human action (a blocked task may be requeued by the pipeline's own
// bounded rule); it never approves, promotes or edits the task itself.
function appendTriageControls(actions, task, detail, context, mutable) {
  const triage = escalationTriage(task);
  const running = triageRunsByTask.get(taskDispatchIdentity(task))
    || (triage && triage.run_id ? { runId: String(triage.run_id) } : null);
  const reason = triageDisabledReason(task, mutable, running);
  const subjectWords = triage && triage.subject === "block"
    ? "Triage this blocked task: a hard-tier triager requeues it within the final-recovery bound, proposes a decision, or names the human action. Nothing else on the task changes"
    : "Triage this held pilot assessment: a hard-tier triager proposes a decision or names the human action that settles the hold. It never approves, promotes or edits the task";
  const btn = el("button", {
    class: "action triage",
    text: running ? "triaging…" : "triage",
    title: reason || subjectWords,
  });
  btn.disabled = Boolean(reason);
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    runTriage(task, detail, btn, context);
  });
  actions.appendChild(btn);
  if (running) {
    const link = el("a", { class: "task-quick-link triage-run-link mono", text: running.runId, title: `Open triage run ${running.runId}` });
    link.href = runHref(running.runId, task.workspace_id ? { workspace: task.workspace_id } : {});
    actions.appendChild(link);
  }
  const refusal = triageRefusals.get(taskDispatchIdentity(task));
  if (refusal) {
    const note = actionErrorNode(refusal);
    note.style.flexBasis = "100%";
    actions.appendChild(note);
  }
}

function triageSignature(task) {
  const key = taskDispatchIdentity(task);
  const running = triageRunsByTask.get(key);
  return `${running ? running.runId : ""}|${triageRefusals.get(key) || ""}`;
}

async function runTriage(task, detail, btnNode, context) {
  const visit = captureWorkspaceVisit();
  const key = taskDispatchIdentity(task);
  if (!canMutateTask(task) || triageRunsByTask.has(key)) return;
  const requestPath = visit.path(taskMutationPath(task, "/triage"));
  triageRefusals.delete(key);
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  for (const b of detail.querySelectorAll(".action")) b.disabled = true;
  btnNode.innerHTML = `<span class="spinner"></span>wait`;
  let result;
  try {
    result = await postJson(requestPath, {});
  } catch (error) {
    if (!visit.isCurrent()) return;
    const message = `triage failed: ${error.message || String(error)}`;
    triageRefusals.set(key, message);
    renderTasks(taskList(context), context);
    setTimeout(() => {
      if (triageRefusals.get(key) !== message) return;
      triageRefusals.delete(key);
      renderTasks(taskList(context), context);
    }, TRIAGE_REFUSAL_MS);
    return;
  }
  const runId = result && result.run_id ? String(result.run_id) : "";
  if (!visit.isCurrent()) return;
  if (runId) triageRunsByTask.set(key, { runId });
  taskActionNotice = `${task.id}: triage run ${runId || "(no run id)"} ${result && result.state ? result.state : "submitted"}`;
  renderTasks(taskList(context), context);
  if (runId) watchTriageRun(task, key, runId, visit, context);
}

// Follow the submitted run until it settles, then refresh: the task then
// shows the triage comment the run recorded. A run that cannot be read any
// more is dropped; the readout's own run link takes over for a live one.
async function watchTriageRun(task, key, runId, visit, context) {
  const path = visit.path(taskWorkspacePath(task, `/api/runs/${encodeURIComponent(runId)}`));
  let state = "";
  for (let polls = 0; polls < PILOT_POLL_LIMIT; polls += 1) {
    await new Promise((resolve) => setTimeout(resolve, PILOT_POLL_MS));
    if (!visit.isCurrent()) return;
    try {
      // The run detail is `{ run, steps }`.
      const detail = await fetchJson(path);
      const run = detail && detail.run ? detail.run : detail;
      state = run && run.state ? String(run.state) : "";
    } catch (_) {
      break;
    }
    if (PILOT_TERMINAL_STATES.has(state)) break;
  }
  triageRunsByTask.delete(key);
  if (!visit.isCurrent()) return;
  if (state) taskActionNotice = `${task.id}: triage run ${runId} ${state}`;
  await refreshTasks(context);
}

// The pull request number a task records, linked or not.
function taskPullRequestNumber(task) {
  const refs = Array.isArray(task.external_refs) ? task.external_refs : [];
  const ref = refs.find((candidate) => candidate && candidate.system === GITHUB_PR_REF_SYSTEM && candidate.id);
  if (ref) return String(ref.id);
  return task.pr_number ? String(task.pr_number) : "";
}

// Fix PR refusals in the operator's words; the server's detail follows.
const FIX_PR_REASONS = {
  not_authorized: "needs an operator session with auto-fix authority",
  no_blocker: "nothing to repair: the pull request is mergeable and no required check failed",
  mergeability_unknown: "GitHub is still calculating whether the pull request is mergeable; try again shortly",
  budget_exhausted: "the pull request's auto-fix repair budget is spent",
  already_repaired: "a repair already ran for this head and blocker",
  already_started: "a repair was already started for this head and blocker",
  live_run: "a run is already delivering or repairing this task",
  pull_request_not_open: "the pull request is no longer open",
  not_orbit_pull_request: "the pull request was not opened by an Orbit delivery on this task's branch",
  no_delivery_leaf: "the task has no delivery run of its own to hand the pull request back to",
  leaf_not_stopped: "the task's newest delivery did not stop with its pull request unfinished",
  operator_hold: "an operator holds the task",
  opted_out: "the task is tagged no-auto-approve; a person decides its delivery",
  authority: "its delivery was started with completion authority this session lacks",
  write_free: "this dashboard is read-only",
  replica_checkout: "this checkout is a replica; repair the pull request on its owner",
  claimed_execution: "the task's delivery runs on another machine",
  resource_throttled: "host resource pressure holds new runs",
  forge_unreadable: "GitHub could not be read",
  not_resumable: "its delivery cannot be resumed at its completion step",
};

function fixPrReasonText(reason, detail) {
  const words = FIX_PR_REASONS[reason];
  if (words && detail) return `${words} (${detail})`;
  return words || detail || reason || "unavailable";
}

// The readiness entry for the task's pull request, fetching it when it is
// missing, stale or for another pull request. Never more than one read in
// flight per task.
function fixPrState(task, context) {
  const key = taskDispatchIdentity(task);
  const prNumber = taskPullRequestNumber(task);
  const entry = fixPrReadiness.get(key);
  const fresh = entry && entry.prNumber === prNumber && (entry.pending || Date.now() - entry.at < FIX_PR_READINESS_MS);
  if (!fresh) loadFixPrReadiness(task, key, prNumber, context);
  return fixPrReadiness.get(key);
}

async function loadFixPrReadiness(task, key, prNumber, context) {
  const visit = captureWorkspaceVisit();
  const entry = { pending: true, at: Date.now(), prNumber, data: null, error: "" };
  fixPrReadiness.set(key, entry);
  try {
    entry.data = await fetchJson(visit.path(taskMutationPath(task, "/auto-fix")));
  } catch (error) {
    entry.error = error.message || String(error);
  }
  entry.pending = false;
  entry.at = Date.now();
  if (fixPrReadiness.get(key) !== entry || !visit.isCurrent()) return;
  renderTasks(taskList(context), context);
}

// Why Fix PR is disabled, or "" when it is offered.
function fixPrDisabledReason(task, mutable, entry) {
  if (!mutable) return aggregateRefusalTitle("Fix PR", "repair a pull request");
  if (!entry || entry.pending) return "Fix PR — checking the pull request…";
  if (entry.error) return `Fix PR — could not check the pull request: ${entry.error}`;
  const data = entry.data || {};
  if (data.available) return "";
  return `Fix PR unavailable — ${fixPrReasonText(data.reason, data.detail)}`;
}

// Fix PR on a review task: hand its conflicting or red pull request back to
// its own delivery for an auto-fix, as an --auto-fix drain would, without one.
function appendFixPrControls(actions, task, detail, context, mutable) {
  const entry = fixPrState(task, context);
  const reason = fixPrDisabledReason(task, mutable, entry);
  const data = entry && entry.data ? entry.data : {};
  const blocker = data.blocker === "conflict" ? "its merge conflict" : "its failed required checks";
  const btn = el("button", {
    class: "action fix-pr",
    text: "fix PR",
    title: reason || `Hand pull request #${data.pr_number || taskPullRequestNumber(task)} back to its delivery to repair ${blocker}. The repair pushes to the same branch and never merges a pull request its delivery was not allowed to merge`,
  });
  btn.disabled = Boolean(reason);
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    runFixPr(task, detail, btn, context, data);
  });
  actions.appendChild(btn);
  const feedback = fixPrFeedback.get(taskDispatchIdentity(task));
  if (feedback) {
    const note = feedback.ok
      ? el("div", { class: "task-action-notice", text: feedback.text, role: "status" })
      : actionErrorNode(feedback.text);
    note.style.flexBasis = "100%";
    actions.appendChild(note);
  }
}

function fixPrSignature(task) {
  const key = taskDispatchIdentity(task);
  const entry = fixPrReadiness.get(key);
  const state = entry ? `${entry.pending}|${entry.error}|${JSON.stringify(entry.data)}` : "";
  const feedback = fixPrFeedback.get(key);
  return `${state}|${feedback ? feedback.text : ""}`;
}

function showFixPrFeedback(key, feedback, context) {
  fixPrFeedback.set(key, feedback);
  renderTasks(taskList(context), context);
  setTimeout(() => {
    if (fixPrFeedback.get(key) !== feedback) return;
    fixPrFeedback.delete(key);
    renderTasks(taskList(context), context);
  }, FIX_PR_FEEDBACK_MS);
}

async function runFixPr(task, detail, btnNode, context, readiness) {
  const visit = captureWorkspaceVisit();
  const key = taskDispatchIdentity(task);
  if (!canMutateTask(task)) return;
  const number = readiness.pr_number || taskPullRequestNumber(task);
  if (!window.confirm(`Hand pull request #${number} back to its delivery for an auto-fix? A repair agent pushes to the same branch; the pull request is merged only if its delivery was started to complete it.`)) return;
  const requestPath = visit.path(taskMutationPath(task, "/auto-fix"));
  fixPrFeedback.delete(key);
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  for (const b of detail.querySelectorAll(".action")) b.disabled = true;
  btnNode.innerHTML = `<span class="spinner"></span>wait`;
  let result;
  try {
    result = await postJson(requestPath, {});
  } catch (error) {
    if (!visit.isCurrent()) return;
    fixPrReadiness.delete(key);
    showFixPrFeedback(key, { ok: false, text: `fix PR refused: ${error.message || String(error)}` }, context);
    return;
  }
  if (!visit.isCurrent()) return;
  fixPrReadiness.delete(key);
  const runId = result && result.resumed_run_id ? String(result.resumed_run_id) : "";
  showFixPrFeedback(key, { ok: true, text: `Repair of pull request #${number} started${runId ? ` in run ${runId}` : ""}` }, context);
  await refreshTasks(context);
}

/* ORB-10444: one-click Ship. The dispatch carries only the task id — the
   pipeline resolves the crew from the task's own record and the mode from the
   workspace's configured default — so there is deliberately no crew picker and
   no PR/local toggle here. The resulting run id (or the server's error) is
   surfaced so the operator can see the click took effect. */
async function shipTask(task, detail, btnNode, context) {
  const visit = captureWorkspaceVisit();
  const dispatchKey = taskDispatchIdentity(task);
  const requestPath = visit.path(taskWorkspacePath(task, "/api/workflows/ship"));
  if (!canMutateTask(task)) return;
  if (shipInFlightTaskIds.has(taskDispatchIdentity(task))) return;
  shipInFlightTaskIds.add(dispatchKey);
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  for (const b of detail.querySelectorAll(".action")) b.disabled = true;
  const oldText = btnNode.textContent;
  btnNode.innerHTML = `<span class="spinner"></span>wait`;
  let dispatched = false;
  try {
    const result = await postJson(requestPath, { task_ids: [task.id] });
    shipInFlightTaskIds.delete(dispatchKey);
    dispatched = true;
    if (!visit.isCurrent()) return;
    const runId = result && result.run_id ? result.run_id : "(no run id)";
    const state = result && result.state ? result.state : "submitted";
    taskActionNotice = [`${task.id}: ship run ${runId} ${state}`, ...submissionNoticeLines(result)].join("\n");
    collapseTask(task.id);
    await refreshTasks(context);
  } catch (error) {
    if (!dispatched) shipInFlightTaskIds.delete(dispatchKey);
    if (!visit.isCurrent()) return;
    // A refresh may have replaced the detail while this request was pending.
    renderTasks(taskList(context), context);
    detail = $("detail-" + task.id) || detail;
    btnNode = detail.querySelector(".action.ship") || btnNode;
    for (const b of detail.querySelectorAll(".action")) b.disabled = false;
    btnNode.textContent = oldText;
    detail.prepend(actionErrorNode(dispatched
      ? `Ship was accepted, but the view could not refresh: ${error.message || String(error)}. Use Refresh to update it.`
      : `ship failed: ${error.message || String(error)}`));
  }
}

/* ORB-10444: human comments on a task. The write goes to the task's existing
   review-thread structure via POST /api/tasks/<id>/comments, which records a
   human identity rather than the server process's ambient one.

   ORB-12645: the draft is Markdown and the thread renders it, so the composer
   says so — in the placeholder, in a hint line naming the marks that carry,
   and in a preview that runs the draft through the same renderMarkdown() the
   posted comment will go through. The write itself is unchanged. */
function showCommentForm(task, detail, actions, context) {
  const form = el("div", { class: "comment-form" });
  form.addEventListener("click", (e) => e.stopPropagation());
  detail.dataset.draft = "comment";
  const ta = el("textarea");
  ta.id = `comment-draft-${task.id}`;
  ta.placeholder = "Markdown is rendered — ## heading, - list, `code`, **bold**";
  const label = el("label", { class: "comment-form-label", text: "Add a comment" });
  label.htmlFor = ta.id;
  const hint = el("div", {
    class: "comment-form-hint",
    text: "Markdown is rendered: **bold** _italic_ `code` ## heading - list",
  });
  const preview = el("div", { class: "comment-preview markdown-body" });
  preview.style.display = "none";
  const buttons = el("div", { class: "actions" });
  const previewToggle = el("button", { class: "action preview", text: "preview" });
  const submit = el("button", { class: "action comment", text: "post" });
  const cancel = el("button", { class: "action cancel", text: "cancel" });
  previewToggle.addEventListener("click", (e) => {
    e.stopPropagation();
    if (preview.style.display !== "none") {
      preview.style.display = "none";
      previewToggle.textContent = "preview";
      return;
    }
    const draft = ta.value.trim();
    const rendered = draft ? renderMarkdown(draft) : null;
    if (rendered !== null) preview.innerHTML = rendered;
    else preview.textContent = draft || "Nothing to preview yet.";
    preview.style.display = "";
    previewToggle.textContent = "hide preview";
  });
  submit.addEventListener("click", async (e) => {
    e.stopPropagation();
    if (!canMutateTask(task)) return;
    const message = ta.value.trim();
    if (!message) {
      ta.focus();
      return;
    }
    const prior = detail.querySelector(".action-error");
    if (prior) prior.remove();
    submit.disabled = true;
    cancel.disabled = true;
    const visit = captureWorkspaceVisit();
    const requestPath = visit.path(taskMutationPath(task, "/comments"));
    let posted = false;
    try {
      await postJson(requestPath, { message });
      posted = true;
      if (!visit.isCurrent()) return;
      // The draft is spent: let the next render rebuild the detail so the
      // posted comment appears.
      delete detail.dataset.draft;
      await refreshTasks(context);
    } catch (error) {
      if (!visit.isCurrent()) return;
      submit.disabled = posted;
      cancel.disabled = false;
      detail.prepend(actionErrorNode(posted
        ? `Comment was posted, but the view could not refresh: ${error.message || String(error)}. Use Refresh to update it.`
        : `comment failed: ${error.message || String(error)}`));
    }
  });
  cancel.addEventListener("click", (e) => {
    e.stopPropagation();
    delete detail.dataset.draft;
    form.replaceWith(actions);
    restoreActionFocus(actions, ".action.comment");
  });
  buttons.appendChild(previewToggle);
  buttons.appendChild(submit);
  buttons.appendChild(cancel);
  form.appendChild(label);
  form.appendChild(ta);
  form.appendChild(hint);
  form.appendChild(preview);
  form.appendChild(buttons);
  actions.replaceWith(form);
  ta.focus();
}

function showRejectForm(task, detail, actions, context) {
  const form = el("div", { class: "reject-form" });
  form.addEventListener("click", (e) => e.stopPropagation());
  detail.dataset.draft = "reject";
  const ta = el("textarea");
  ta.placeholder = "reason for rejection";
  const buttons = el("div", { class: "actions" });
  const submit = el("button", { class: "action reject", text: "submit" });
  const cancel = el("button", { class: "action cancel", text: "cancel" });
  submit.addEventListener("click", (e) => {
    e.stopPropagation();
    const note = ta.value.trim();
    if (!note) {
      ta.focus();
      return;
    }
    runAction(task, "reject", detail, { note }, submit, context);
  });
  cancel.addEventListener("click", (e) => {
    e.stopPropagation();
    delete detail.dataset.draft;
    form.replaceWith(actions);
    restoreActionFocus(actions, ".action.reject");
  });
  buttons.appendChild(submit);
  buttons.appendChild(cancel);
  form.appendChild(ta);
  form.appendChild(buttons);
  actions.replaceWith(form);
  ta.focus();
}

async function runAction(task, kind, detail, body, btnNode, context, opts = {}) {
  const visit = opts.visit || captureWorkspaceVisit();
  if (!visit.isCurrent()) return;
  const requestPath = visit.path(opts.path ? taskWorkspacePath(task, opts.path) : taskMutationPath(task, `/${kind}`));
  if (!canMutateTask(task)) return;
  // Disable action controls while in flight to prevent double-clicks.
  for (const b of detail.querySelectorAll(".action")) b.disabled = true;
  let oldText = "";
  if (btnNode && btnNode.tagName === "BUTTON") {
    oldText = btnNode.textContent;
    btnNode.innerHTML = `<span class="spinner"></span>wait`;
  }
  // Clear any prior error
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  let accepted = false;
  try {
    const res = await fetch(requestPath, {
      method: opts.method || "POST",
      headers: body ? { "content-type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    if (!res.ok) {
      let msg = `${kind} failed: HTTP ${res.status}`;
      let remedy = null;
      try {
        const errBody = await res.json();
        if (errBody && errBody.error) msg = `${kind} failed: ${errBody.error}`;
        // A typed refusal (the handoff approval's stale or uncertain-merge
        // conflicts) names what to do next; dropping it leaves the operator
        // with a refusal and no way forward.
        if (errBody && typeof errBody.remedy === "string" && errBody.remedy) remedy = errBody.remedy;
      } catch (_) {
        /* keep generic msg */
      }
      throw Object.assign(new Error(msg), remedy ? { remedy } : {});
    }
    accepted = true;
    if (!visit.isCurrent()) return;
    if (opts.collapseOnSuccess !== false) collapseTask(task.id);
    if (opts.successNotice) taskActionNotice = opts.successNotice;
    await refreshTasks(context);
  } catch (err) {
    if (!visit.isCurrent()) return;
    for (const b of detail.querySelectorAll(".action")) b.disabled = accepted;
    if (btnNode && btnNode.tagName === "BUTTON") btnNode.textContent = oldText;
    if (!accepted && opts.onFailure) opts.onFailure();
    detail.prepend(actionErrorNode(accepted
      ? `${kind} was accepted, but the view could not refresh: ${err.message || String(err)}. Use Refresh to update it.`
      : String(err.message || err), err && err.remedy));
  }
}

/// Approve a review task. A task delivered by a distributed claim is approved
/// through its handoff, which records completion authority for the exact
/// candidate and starts the owner's landing job; a plain status write would
/// be refused while the claim protects the task.
async function approveReviewTask(task, detail, btnNode, context) {
  const visit = captureWorkspaceVisit();
  const admission = `${visit.revision}:${task.id}`;
  if (reviewApprovalsInFlight.has(admission)) return;
  reviewApprovalsInFlight.add(admission);
  btnNode.disabled = true;
  let claimed = null;
  try {
    claimed = await claimedReviewApproval(task.id);
  } catch (_) {
    claimed = null; // No readable claim state: the ordinary path reports the owner's answer.
  } finally {
    reviewApprovalsInFlight.delete(admission);
    btnNode.disabled = false;
  }
  // Admission is asynchronous; a task detail from an earlier workspace visit
  // must not approve a same-named task through the new ambient workspace.
  if (!visit.isCurrent()) return;
  if (!claimed) return runAction(task, "approve", detail, null, btnNode, context, { visit });
  if (claimed.refusal) {
    const prior = detail.querySelector(".action-error");
    if (prior) prior.remove();
    detail.prepend(actionErrorNode(`approve: ${claimed.refusal}`));
    return undefined;
  }
  const request = handoffApprovalRequest(claimed.claim);
  invalidateDistributedConsole();
  return runAction(task, "approve", detail, request.body, btnNode, context, {
    visit,
    path: request.path,
    successNotice: `${task.id}: handoff approved; the owner landing job completes it`,
  });
}

// The feedback of a status change whose detail folded in, with its undo. Null
// while the detail is open again (it shows the feedback itself) or once the
// feedback has expired.
function buildFoldedStatusFeedback(taskId, context) {
  if (!foldedStatusTasks.has(taskId) || selectedTaskId === taskId) return null;
  const entry = statusFeedback.get(taskId);
  return buildMutationFeedback(entry, () => {
    if (!entry || !entry.undo) return;
    const task = taskList(context).find((t) => t && t.id === taskId) || foldedStatusTasks.get(taskId);
    applyTaskStatusChange(task, entry.undo.previousValue, context);
  });
}

function foldedStatusSignature(taskId) {
  return foldedStatusTasks.has(taskId) && selectedTaskId !== taskId ? feedbackSignature(statusFeedback, taskId) : "";
}

// Folded status feedback for a task with no row on screen, usually because its
// new status is filtered out, so the undo stays reachable above the list. A
// pinned detail shows its own.
function buildFoldedStatusNotices(rowIds, context) {
  const notices = [];
  for (const [taskId, task] of foldedStatusTasks) {
    if (rowIds.has(taskId) || (pinnedExternalTask && pinnedExternalTask.id === taskId)) continue;
    const feedback = buildFoldedStatusFeedback(taskId, context);
    if (!feedback) continue;
    const notice = el("div", { class: "task-status-notice" }, [
      el("span", { class: "mono", text: `${taskId} → ${task.status}` }),
      feedback,
    ]);
    notice.dataset.key = `status-notice-${taskId}`;
    notice.dataset.hash = `${task.status}-${foldedStatusSignature(taskId)}`;
    notices.push(notice);
  }
  return notices;
}

function takeTaskActionNotice() {
  if (!taskActionNotice) return null;
  const notice = el("div", { class: "task-action-notice", text: taskActionNotice });
  notice.dataset.key = "task-action-notice";
  notice.dataset.hash = taskActionNotice;
  notice.setAttribute("role", "status");
  notice.setAttribute("aria-live", "polite");
  taskActionNotice = null;
  return notice;
}

// A detail kept for an open draft can refresh its read-only readiness and
// selector displays without touching the editor holding that draft.
function refreshDraftDetails(wrap, task, context, readinessTask = task) {
  for (const readout of wrap.querySelectorAll(".task-os-readout")) {
    readout.textContent = osReadoutText(task);
    const note = readout.parentNode && readout.parentNode.querySelector(".info-hint-text");
    if (note) note.textContent = osReadoutNote(task);
  }
  refreshDraftCrewControl(wrap, task, context);
  if (Array.isArray(readinessTask.context_files)) {
    const count = wrap.querySelector(`#detail-${task.id}-tab-context_files .detail-tab-count`);
    if (count) count.textContent = String(readinessTask.context_files.length);
    for (const ul of wrap.querySelectorAll(".file-list")) {
      if (!ul.contentionTask) continue;
      ul.contentionTask = readinessTask;
      refreshFileList(ul);
    }
  }
  const current = wrap.querySelector(".readiness-list");
  if (!current) return;
  const drain = taskDrainState(task, context);
  if (!drain) return;
  const signature = taskDrainSignature(drain);
  if (current.dataset.drainSignature === signature) return;
  const replacement = buildReadinessBlock(readinessOf(readinessTask), drain, readinessTask.workspace_id);
  replacement.dataset.drainSignature = signature;
  const parent = current.parentNode;
  if (!parent) return;
  parent.insertBefore(replacement, current);
  parent.removeChild(current);
}

function refreshDraftCrewControl(wrap, task, context) {
  const control = wrap.querySelector(".task-meta-block .crew-cell.task-chip-crew");
  const select = control && control.querySelector("select");
  if (!select) return;

  const refreshed = buildCrewUpdateControl(task, context, { withChip: false });
  const refreshedSelect = refreshed.querySelector("select");
  if (!refreshedSelect) return;

  const value = refreshedSelect.value;
  select.replaceChildren(...refreshedSelect.children);
  select.value = value;
  select.disabled = refreshedSelect.disabled;
  select.title = refreshedSelect.title;
  select.dataset.currentValue = refreshedSelect.dataset.currentValue;
  select.dataset.staleCurrentValue = refreshedSelect.dataset.staleCurrentValue;

  const oldFeedback = control.querySelector(":scope > .mutation-feedback");
  if (oldFeedback) oldFeedback.remove();
  const newFeedback = refreshed.querySelector(":scope > .mutation-feedback");
  if (newFeedback) control.appendChild(newFeedback);
}

// The pinned resolver result shares the normal detail's crew control. Only
// the dock needs the outside-the-list note; full-page CSS hides that box.
function buildPinnedTask(ptask, context) {
  const row = el("div", { class: "pinned-external" }, [
    el("span", { class: "pinned-note", text: `Not in the current list · ${ptask.status}` }),
  ]);
  stopRowInteraction(row);
  row.dataset.hash = `${ptask.id}-${ptask.title}-${ptask.status}-${ptask.crew || ""}-${ptask.resolved_crew || ""}-${crewOptionsSignature()}-${feedbackSignature(crewFeedback, ptask.id)}`;

  const drain = taskDrainState(ptask, context);
  const detail = buildTaskDetail(ptask, context, drain);
  const readiness = detail.querySelector(".readiness-list");
  if (readiness) readiness.dataset.drainSignature = taskDrainSignature(drain);

  const wrap = el("div", { class: "pinned-task-wrap" }, [row, detail]);
  wrap.teardown = () => disconnectCommentCards(wrap);
  wrap.dataset.key = `pinned-${ptask.id}`;
  wrap.dataset.hash = `${row.dataset.hash}-${JSON.stringify(ptask)}-${taskDrainSignature(drain)}-${detailFeedbackSignature(ptask.id)}`;
  return wrap;
}

/* ORB-11655: a comment or reject form, or an open field editor (ORB-12235),
   holds text the operator is still typing, so the 30 s refresh must not rebuild
   the node that contains it — not even when the task itself changed. Collect the
   live top-level nodes holding one, keyed the way syncNodes keys them, and reuse
   them verbatim. The detail resumes tracking task data as soon as it closes.
   The nested lookups are for the pinned-task wrapper, whose draft marker sits on
   the detail inside it rather than on the keyed node itself. */
function openDraftNodes(body) {
  const drafts = new Map();
  for (const node of Array.from(body.children)) {
    if (!node.dataset.key) continue;
    const holdsDraft =
      node.dataset.draft ||
      node.dataset.editing ||
      node.querySelector?.("[data-draft]") ||
      node.querySelector?.("[data-editing]");
    if (holdsDraft) drafts.set(node.dataset.key, node);
  }
  return drafts;
}

function quickActionSignature(task) {
  const state = quickActionState.get(task.id);
  const machine = task.job_run_machine;
  const host = machine && machine.machine_id ? `${machine.machine_id}:${machine.machine_name || ""}` : "";
  const navigable = task.job_run_navigable === false ? "0" : "1";
  const pullRequest = pullRequestLink(task);
  return `${task.job_run_id || ""}-${navigable}-${host}-${pullRequest ? pullRequest.url : ""}-${shipInFlightTaskIds.has(taskDispatchIdentity(task))}-${state ? `${state.kind}:${state.text}` : ""}`;
}

function summaryExecutionLocation(machine) {
  if (!machine || !machine.machine_id) return { known: false };
  return {
    known: true,
    machine_id: machine.machine_id,
    machine_name: machine.machine_name || null,
  };
}

// The one decision a row is waiting on, one click from the list: Approve a
// proposed or review task, Ship a backlog one, open the run of one in progress.
// They hit the same endpoints as the detail's actions; the detail keeps the full set.
// True when buildQuickAction renders something for the task. The list drops the
// action column only when no visible row would fill it, so this must agree with
// buildQuickAction.
function hasQuickAction(task) {
  if (task.status === "in-progress" && task.job_run_id) {
    if (task.job_run_navigable !== false) return true;
    if (summaryExecutionLocation(task.job_run_machine).known) return true;
  }
  if (PULL_REQUEST_LINK_STATUSES.has(task.status) && pullRequestLink(task)) return true;
  return APPROVE_STATUSES.has(task.status) || SHIP_STATUSES.has(task.status);
}

function canRunQuickAction(task) {
  // Review admission reads claim state in the selected workspace. Aggregate
  // review rows need that selection even when their ordinary writes are scoped.
  return canMutateTask(task) && (task.status !== "review" || !isAggregateView());
}

// A run recorded on another machine. A registered host opens the run there; an
// unregistered one has no dashboard to open, so its "View run" is dimmed text
// that names the machine in its title.
function buildRemoteRunAction(task, location) {
  const runId = task.job_run_id;
  const host = registeredMachineHost(location);
  if (host) {
    const link = el("a", { class: "task-quick-link remote-run-link", text: "View run", title: `Open run ${runId} on ${host.name}` });
    link.href = runHref(runId, { host: host.name, workspace: task.workspace_id || getWorkspace() });
    link.addEventListener("click", (event) => event.stopPropagation());
    return link;
  }
  const machine = location.machine_name || location.machine_id;
  return el("span", {
    class: "task-quick-link unlinked",
    text: "View run",
    title: `Run ${runId} ran on ${machine}, which is not a registered host`,
  });
}

function buildQuickAction(task, context) {
  const cell = el("span", { class: "task-quick-cell" });
  stopRowInteraction(cell);
  const state = quickActionState.get(task.id);
  if (task.status === "in-progress" && task.job_run_id) {
    // A missing flag stays a link: that is the historical unrecorded-host row.
    // An explicit false means the run was recorded on another machine, so open
    // it on that host rather than this host's job store.
    if (task.job_run_navigable !== false) {
      const link = el("a", { class: "task-quick-link", text: "View run", title: `Open run ${task.job_run_id}` });
      link.href = runHref(task.job_run_id);
      link.addEventListener("click", (event) => event.stopPropagation());
      cell.appendChild(link);
      return cell;
    }
    const location = summaryExecutionLocation(task.job_run_machine);
    if (location.known) {
      cell.appendChild(buildRemoteRunAction(task, location));
      return cell;
    }
  }
  const pullRequest = PULL_REQUEST_LINK_STATUSES.has(task.status) ? pullRequestLink(task) : null;
  if (pullRequest) {
    const link = el("a", { class: "task-quick-link", text: `PR #${pullRequest.number}`, title: `Open pull request #${pullRequest.number}` });
    link.href = pullRequest.url;
    link.rel = "noopener noreferrer";
    link.target = "_blank";
    link.addEventListener("click", (event) => event.stopPropagation());
    cell.appendChild(link);
    return cell;
  }
  let spec = null;
  if (task.status === "proposed") {
    spec = { kind: "approve", text: "Approve", title: `Approve ${task.id}: move it to backlog so it can run` };
  } else if (task.status === "review") {
    spec = { kind: "approve", text: "Approve", title: `Approve ${task.id}: close it as done` };
  } else if (SHIP_STATUSES.has(task.status)) {
    spec = { kind: "ship", text: "Ship", title: `Ship ${task.id}: dispatch it through the pipeline with its own crew` };
  }
  if (!spec) return cell;
  const pending = (state && state.kind === "pending") || (spec.kind === "ship" && shipInFlightTaskIds.has(taskDispatchIdentity(task)));
  // Ship reads in the accent while nothing holds the task, and quietly while
  // the drain snapshot says it waits; it ships either way.
  const ready = spec.kind === "ship" && drainReadinessFor(task.id)?.pool === "eligible";
  // Pending keeps the idle label so the button keeps its idle width: the
  // longer "Approving…"/"Shipping…" labels overflow the 73px desktop action
  // column into the crew control. The pending state is named for assistive
  // technology and shown by the spinner in tasks.css.
  const pendingLabel = spec.kind === "ship" ? "Shipping…" : "Approving…";
  const btn = el("button", {
    class: `task-quick ${spec.kind}${ready ? " ready" : ""}${pending ? " pending" : ""}`,
    text: spec.text,
    title: pending ? pendingLabel : (canRunQuickAction(task) ? spec.title : aggregateRefusalTitle(spec.title, spec.kind)),
    ...(pending ? { "aria-label": pendingLabel, "aria-busy": "true" } : {}),
  });
  btn.type = "button";
  btn.disabled = pending || !canRunQuickAction(task);
  btn.addEventListener("click", (event) => {
    event.stopPropagation();
    runQuickAction(task, spec.kind, context);
  });
  cell.appendChild(btn);
  return cell;
}

function buildQuickActionError(task) {
  const state = quickActionState.get(task.id);
  if (!state || state.kind !== "error") return null;

  const text = state.text;
  const runMatch = text.match(/\b(jrun-[a-zA-Z0-9_\-\.]+)\b/);
  let wrap;
  if (runMatch) {
    const runId = runMatch[1];
    const idx = text.indexOf(runId);
    const before = text.slice(0, idx);
    const after = text.slice(idx + runId.length);
    const link = el("a", { class: "task-quick-error-link", text: runId, title: `Open run ${runId}` });
    link.href = runHref(runId);
    link.addEventListener("click", (e) => e.stopPropagation());
    wrap = el("div", { class: "task-quick-error", title: "" }, [
      before,
      link,
      after,
    ]);
  } else {
    wrap = el("div", { class: "task-quick-error", text, title: "" });
  }
  wrap.setAttribute("role", "status");
  wrap.setAttribute("aria-live", "assertive");
  stopRowInteraction(wrap);
  return wrap;
}

async function runQuickAction(task, kind, context) {
  const visit = captureWorkspaceVisit();
  const dispatchKey = taskDispatchIdentity(task);
  const requestPath = visit.path(kind === "ship" ? taskWorkspacePath(task, "/api/workflows/ship") : taskMutationPath(task, "/approve"));
  if (!canRunQuickAction(task)) return;
  if (quickActionState.get(task.id)?.kind === "pending") return;
  if (kind === "ship" && shipInFlightTaskIds.has(taskDispatchIdentity(task))) return;
  quickActionState.set(task.id, { kind: "pending", text: kind });
  if (kind === "ship") shipInFlightTaskIds.add(dispatchKey);
  renderTasks(taskList(context), context);
  let dispatched = false;
  try {
    if (kind === "ship") {
      const result = await postJson(requestPath, { task_ids: [task.id] });
      shipInFlightTaskIds.delete(dispatchKey);
      dispatched = true;
      if (!visit.isCurrent()) return;
      const runId = result && result.run_id ? result.run_id : "(no run id)";
      const state = result && result.state ? result.state : "submitted";
      taskActionNotice = [`${task.id}: ship run ${runId} ${state}`, ...submissionNoticeLines(result)].join("\n");
    } else {
      let claimed = null;
      if (task.status === "review") {
        try {
          claimed = await claimedReviewApproval(task.id);
        } catch (_) {
          // As in the detail panel, an unreadable claim leaves the owner to decide.
        }
        if (!visit.isCurrent()) return;
        if (claimed?.refusal) throw new Error(claimed.refusal);
      }
      if (claimed?.claim) {
        const request = handoffApprovalRequest(claimed.claim);
        invalidateDistributedConsole();
        await postJson(visit.path(request.path), request.body);
      } else {
        await postJson(requestPath, {});
      }
      dispatched = true;
      if (!visit.isCurrent()) return;
      taskActionNotice = claimed?.claim
        ? `${task.id}: handoff approved; the owner landing job completes it`
        : `${task.id} approved and moved to ${task.status === "review" ? "done" : "backlog"}`;
    }
    quickActionState.delete(task.id);
    await refreshTasks(context);
  } catch (error) {
    if (kind === "ship" && !dispatched) shipInFlightTaskIds.delete(dispatchKey);
    if (!visit.isCurrent()) return;
    quickActionState.set(task.id, { kind: "error", text: dispatched
      ? `${kind === "ship" ? "Ship" : "Approval"} was accepted, but the view could not refresh: ${error.message || String(error)}. Use Refresh to update it.`
      : `${kind} failed: ${error.message || String(error)}` });
    renderTasks(taskList(context), context);
  }
}

// ORB-15216: a pull replica holds no tasks of its own; they live on its owner
// host. The serving host's `/api/hosts` row lists each checkout's role, so this
// reads what the host picker already has. A remote host's row lists checkouts
// only after a probe, so replicas on a remote host are not named here.
function replicaOwner() {
  const workspaceId = getWorkspace();
  if (!workspaceId) return null;
  const row = getHost() ? findRegisteredHost(getHost()) : getRegisteredHosts().find((host) => host.local);
  const checkout = (row?.workspaces || []).find((workspace) => workspace.id === workspaceId);
  if (checkout?.role !== "replica") return null;
  const owner = findRegisteredHost(checkout.owner_machine_id);
  return {
    name: owner ? owner.name : checkout.owner_machine_id || "an unknown host",
    // Only a registered remote host has an entry in the picker to switch to.
    switchable: Boolean(owner && !owner.local),
  };
}

function showOnOwnerButton(name) {
  const button = el("button", { class: "ghost", type: "button", text: `Show on ${name}` });
  button.addEventListener("click", () => {
    const picker = document.getElementById("host-select");
    if (!picker) return;
    picker.value = name;
    picker.dispatchEvent(new Event("change"));
  });
  return button;
}

// The Backlog card can narrow its rows to one readiness pool. In-memory only:
// it is a glance filter, cleared when the workspace changes.
let readyPoolFilter = null;
onWorkspaceChange(() => { readyPoolFilter = null; });

function plural(count, word) {
  return `${count} ${word}${count === 1 ? "" : "s"}`;
}

// The In progress card's head: where the runs are and how many files they
// hold, from the rows themselves. Falls back to the plain hint when no row
// names a host.
function runningHint(group) {
  const hosts = new Map();
  const files = new Set();
  for (const task of group) {
    const { host } = runCellParts(task);
    if (host) hosts.set(host, (hosts.get(host) || 0) + 1);
    for (const file of taskLockFiles(task.id)) files.add(file);
  }
  const parts = [...hosts].map(([host, count]) => `${count} on ${host}`);
  if (files.size > 0) parts.push(`${plural(files.size, "file")} locked`);
  return parts.length > 0 ? parts.join(" · ") : GROUP_HINTS["in-progress"];
}

function buildGroupHeader(status, group) {
  const hint = status === "in-progress" ? runningHint(group) : GROUP_HINTS[status];
  const header = el("div", { class: "group-header" }, [
    el("h3", { class: "group-title" }, [
      el("span", { class: "group-dot", "aria-hidden": "true" }),
      el("span", { class: "group-label", text: GROUP_LABELS[status] || status }),
      el("span", { class: "group-count", text: `${group.length}` }),
    ]),
    hint ? el("span", { class: "group-hint", text: hint }) : null,
  ]);
  header.dataset.status = status;
  header.dataset.key = `header-${status}`;
  header.dataset.hash = `${status}-${group.length}-${hint || ""}`;
  return header;
}

// The legend's short word for each readiness pool; its title says it in full.
const READY_POOL_WORDS = { eligible: "eligible", locks: "locks", capacity: "capacity", other: "other" };

// The Backlog card's readiness strip: one slim bar split by pool, a legend
// whose entries filter the card to that pool, and a link to the Drain view that
// explains each wait. Absent until the drain snapshot arrives, so the card
// never claims a readiness it does not know.
function buildBacklogReadiness(group, context) {
  const { known, counts } = backlogReadiness(group);
  if (!known) return null;
  const pools = DRAIN_POOLS.filter(({ key }) => counts[key] > 0);
  const described = DRAIN_POOLS.map(({ key, label }) => `${counts[key]} ${label.toLowerCase()}`);
  if (counts.unknown > 0) described.push(`${counts.unknown} not in the drain snapshot`);
  const bar = el("div", { class: "ready-bar", role: "img", "aria-label": `Backlog readiness: ${described.join(", ")}` }, [
    ...pools.map(({ key }) => {
      const segment = el("span", { class: "ready-bar-segment", style: { flexGrow: String(counts[key]) } });
      segment.dataset.pool = key;
      return segment;
    }),
    counts.unknown > 0 ? el("span", { class: "ready-bar-segment unknown", style: { flexGrow: String(counts.unknown) } }) : null,
  ]);
  if (readyPoolFilter && counts[readyPoolFilter] === 0) readyPoolFilter = null;
  const legend = el("div", { class: "ready-legend", role: "group", "aria-label": "Filter the backlog by readiness" }, [
    ...DRAIN_POOLS.map(({ key, label }) => {
      const on = readyPoolFilter === key;
      const button = el("button", {
        class: `ready-filter${on ? " active" : ""}`,
        type: "button",
        "aria-pressed": on ? "true" : "false",
        title: on ? "Show every backlog task" : `Show only tasks ${label.toLowerCase()}`,
      }, [
        el("span", { class: "ready-filter-dot", "aria-hidden": "true" }),
        el("strong", { class: "ready-filter-count", text: String(counts[key]) }),
        el("span", { text: READY_POOL_WORDS[key] || label.toLowerCase() }),
      ]);
      button.dataset.pool = key;
      button.disabled = counts[key] === 0 && !on;
      button.addEventListener("click", () => {
        readyPoolFilter = on ? null : key;
        renderTasks(taskList(context), context);
      });
      return button;
    }),
    counts.unknown > 0 ? el("span", { class: "ready-unknown", text: `${counts.unknown} not in the drain snapshot` }) : null,
  ]);
  const why = el("a", { class: "ready-why", text: "Why? See Drain", title: "Open the Drain view: what each backlog task waits on" });
  why.href = "#drain";
  const strip = el("div", { class: "group-readiness" }, [bar, legend, why]);
  strip.dataset.key = "readiness-backlog";
  strip.dataset.hash = `${DRAIN_POOLS.map(({ key }) => counts[key]).join(",")},${counts.unknown}-${readyPoolFilter || ""}`;
  return strip;
}

// A card whose rows the readiness filter hid entirely still says so.
function buildFilteredEmpty(status) {
  const note = el("div", { class: "group-empty", text: `No ${(GROUP_LABELS[status] || status).toLowerCase()} tasks match this readiness filter.` });
  note.dataset.key = `empty-${status}`;
  note.dataset.hash = `empty-${status}`;
  return note;
}

// "Nothing in review or proposed." for open statuses that are on but hold no
// task. Only when the page holds every matching task and nothing else narrows
// it, so an empty card is never inferred from a partial list.
function buildEmptyGroupsNote(groups, context) {
  if (searchQueryValue(context) || notReadyOnly || !statusCounts(context)) return null;
  const active = activeStatusSet(context);
  const empty = GROUP_ORDER.filter((status) => EMPTY_GROUP_PHRASES[status] && active.has(status) && !groups.has(status));
  if (empty.length === 0 || groups.size === 0) return null;
  const phrases = empty.map((status) => EMPTY_GROUP_PHRASES[status]);
  const text = `Nothing ${phrases.length > 1 ? `${phrases.slice(0, -1).join(", ")} or ${phrases.at(-1)}` : phrases[0]}.`;
  const note = el("p", { class: "groups-empty-note", text });
  note.dataset.key = "groups-empty-note";
  note.dataset.hash = text;
  return note;
}

export function renderTasks(tasks, context) {
  if (!panelCanRender("tasks-body")) return;
  const body = $("tasks-body");
  if (!body) return;
  paneContext = context;
  // j/k move like Down/Up, and while the pane is open it follows the row the
  // keyboard lands on.
  enableRovingRows(body, { letterKeys: true, onMove: followRovingSelection });

  // ORB-00030: in the aggregate ("All workspaces") view each task carries its
  // owning workspace; show it as a badge in the Title cell. Detected from the
  // data so no extra plumbing is needed (single-workspace lists lack the field).
  const aggregate = Array.isArray(tasks) && tasks.some((t) => t && t.workspace_name);

  // Auto-clear a pinned jump only when the task is actually in the filtered
  // list. Matching status/search chips is not enough: a truncated fetch can
  // omit the jumped task even when its chip is on.
  if (pinnedExternalTask && pinnedExternalTask.task) {
    const p = pinnedExternalTask.task;
    if (filterTasks(tasks, context).some((task) => task.id === p.id)) {
      pinnedExternalTask = null;
      if (openTaskId === p.id) selectedTaskId = p.id;
    }
  }
  resolvePendingOpenTask(tasks, context);
  for (const taskId of foldedStatusTasks.keys()) {
    if (!statusFeedback.has(taskId)) foldedStatusTasks.delete(taskId);
  }

  // Collected as a plain array rather than a document fragment: a fragment
  // would detach every reused node from the panel. syncNodes leaves a node it
  // already holds in place, which is also what keeps the list still: the
  // detail lives in the pane, so a selection rebuilds nothing here.
  const nodes = [];
  // DANI-10396: the row hash is cheap to compute from task fields alone, so it
  // is checked against the previous render's node *before* building
  // buildStatusUpdateControl/buildCrewUpdateControl — syncNodes would discard
  // that DOM anyway on an unchanged hash, but only after paying to construct it.
  const existingRowNodes = new Map();
  for (const node of body.children) {
    if (node.dataset.key) existingRowNodes.set(node.dataset.key, node);
  }
  const notice = takeTaskActionNotice();

  const filtered = filterTasks(tasks, context);
  // Rows keep a fixed-width action column so actions line up between groups;
  // when no visible row has an action (a Done-only view) the whole column is
  // dead space, so the list gives it back as one.
  body.dataset.quickActions = filtered.some(hasQuickAction) ? "some" : "none";
  $("tasks-count").textContent = formatTaskCount(filtered.length, tasks.length, tasksMeta(context));
  renderTaskPagination(context);
  // ORB-10972: the rail shows the matching total the panel header reports, so
  // the Tasks entry reads correctly from any other tab. The rows on this page
  // are not that total once the list is paginated (ORB-14701).
  const railCount = document.getElementById("rail-count-tasks");
  if (railCount) {
    const meta = tasksMeta(context);
    const matching = meta && Number.isFinite(meta.total) ? meta.total : filtered.length;
    railCount.textContent = String(matching);
    railCount.title = "Tasks matching the current status filter";
  }
  renderFilterSummary(context);
  refreshChips(context);
  if (filtered.length === 0) {
    const replica = tasks.length === 0 ? replicaOwner() : null;
    const defaultText = tasks.length === 0 ? "No tasks available." : "No tasks match filter.";
    const emptyState = el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: replica ? `This checkout is a pull replica of ${replica.name}; its tasks live on the owner.` : defaultText }),
      replica?.switchable ? showOnOwnerButton(replica.name) : null,
    ]);
    syncNodes(body, [notice, ...buildFoldedStatusNotices(new Set(), context), emptyState].filter(Boolean));
    renderTaskPane(filtered, context);
    finishTaskRender(body);
    return;
  }
  const groups = new Map();
  if (notice) nodes.push(notice);
  // Filled in once the rows are known: a notice is only for a task with no row.
  const noticeIndex = nodes.length;
  const rowIds = new Set();

  for (const t of filtered) {
    if (!groups.has(t.status)) groups.set(t.status, []);
    groups.get(t.status).push(t);
  }
  const ordered = GROUP_ORDER.filter((s) => groups.has(s)).concat(
    [...groups.keys()].filter((s) => !GROUP_ORDER.includes(s)),
  );
  const emptyNote = buildEmptyGroupsNote(groups, context);
  if (emptyNote) nodes.push(emptyNote);
  for (const status of ordered) {
    const group = groups.get(status);
    // Each status is one card: its head, the backlog's readiness strip, then
    // the rows. The nodes stay siblings in the list so row reuse and keyboard
    // roving work across groups; CSS draws the card from the first node of
    // each run to the one marked `group-end`.
    const groupNodes = [buildGroupHeader(status, group)];
    let rows = group;
    if (status === "backlog") {
      const strip = buildBacklogReadiness(group, context);
      if (strip) groupNodes.push(strip);
      if (readyPoolFilter && strip) rows = group.filter((t) => drainReadinessFor(t.id)?.pool === readyPoolFilter);
    }
    for (const t of rows) {
      const rowKey = `task-${t.id}`;
      const run = RUN_GROUPS.has(t.status);
      // Basic hash based on row presentation parameters (selection is painted
      // apart, by syncRowSelection)
      const rowHash = `${JSON.stringify(t.pilot_warnings || [])}-${JSON.stringify(t.delivery_attention || null)}-${t.id}-${t.title}-${t.status}-${JSON.stringify(t.os_requirement || null)}-${(t.tags || []).filter(isOsTag).join(",")}-${readinessSignature(t)}-${t.status === "backlog" ? drainWaitSignature(t.id) : ""}-${run ? runCellSignature(t) : ""}-${t.crew || ""}-${t.resolved_crew || ""}-${t.workspace_id || ""}-${crewOptionsSignature()}-${feedbackSignature(crewFeedback, t.id)}-${quickActionSignature(t)}-${foldedStatusSignature(t.id)}`;
      const existingRow = existingRowNodes.get(rowKey);
      let row = existingRow && existingRow.dataset.hash === rowHash ? existingRow : null;
      if (!row) {
        // Two lines beside the crew and the action: the title with its tag
        // chips, then the ID and what the task is waiting on or where its run
        // is. The ID line comes first in the DOM so Tab goes title → crew →
        // action; the grid draws it under the title.
        const meta = el("span", { class: "row-meta" }, [
          makeCopyButton(t.id, { class: "id mono", title: "Copy task ID" }),
          t.status === "backlog" ? buildRowReason(t) : null,
          run ? buildRunCell(t) : null,
        ]);
        const [prefixes, rest] = splitTitlePrefix(t.title);
        const tags = [
          aggregate && t.workspace_name
            ? el("span", { class: "ws-badge mono", text: t.workspace_name, title: `Workspace: ${t.workspace_name}` })
            : null,
          ...buildOsBadges(t),
          ...buildReadinessBadge(t),
          ...buildPilotWarningBadge(t),
          ...buildDeliveryAttentionBadge(t),
          ...prefixes.map((label) => el("span", { class: "title-tag", text: label, title: `[${label}]` })),
        ].filter(Boolean);
        // The title is the row's disclosure: the row holds the copy-id button,
        // the crew select and the quick action, so it cannot be a button itself.
        const titleCell = el("button", { class: "title" }, [rest]);
        const titleLine = el("span", { class: "title-line" }, [
          titleCell,
          tags.length ? el("span", { class: "title-tags" }, tags) : null,
        ]);
        const quickError = buildQuickActionError(t);
        const statusFeedbackNode = buildFoldedStatusFeedback(t.id, context);
        const statusNote = statusFeedbackNode ? el("div", { class: "task-status-note" }, [statusFeedbackNode]) : null;
        if (statusNote) stopRowInteraction(statusNote);
        row = el("div", { class: `row${quickError ? " has-quick-error" : ""}${statusNote ? " has-status-note" : ""}`, title: t.title }, [
          meta,
          titleLine,
          buildCrewUpdateControl(t, context),
          buildQuickAction(t, context),
          ...(quickError ? [quickError] : []),
          statusNote,
        ]);
        row.dataset.key = rowKey;
        row.dataset.hash = rowHash;
        // Selection is painted after the sync (syncRowSelection), so moving it
        // rebuilds no row and the focused one keeps its node.
        makeRowDisclosure(row, titleCell, {
          expanded: false,
          controls: "task-detail-pane",
          onToggle: () => {
            if (selectedTaskId === t.id && !pinnedExternalTask) closeTaskPane(context, { forget: true });
            else selectTask(t.id, context, { anchor: row });
          },
        });
      }
      groupNodes.push(row);
      rowIds.add(t.id);
    }
    if (rows.length === 0) groupNodes.push(buildFilteredEmpty(status));
    // A card whose rows carry no action (Blocked) gives the action column
    // back, so its crew sits at the card's right edge.
    const noQuick = !group.some(hasQuickAction);
    groupNodes.forEach((node, index) => {
      node.dataset.group = status;
      node.classList.toggle("no-quick", noQuick);
      node.classList.toggle("group-end", index === groupNodes.length - 1);
    });
    nodes.push(...groupNodes);
  }
  nodes.splice(noticeIndex, 0, ...buildFoldedStatusNotices(rowIds, context));
  syncNodes(body, nodes);
  syncRowSelection(body);
  renderTaskPane(filtered, context);
  finishTaskRender(body);
  const commentHash = getCommentHash();
  if (commentHash.startsWith("#comment-")) {
    if (restoredCommentHash !== commentHash) {
      if (scrollToComment(commentHash)) {
        restoredCommentHash = commentHash;
      }
    }
  } else {
    restoredCommentHash = null;
  }
}

// The selected row reads as selected without being rebuilt: its class and
// its disclosure state are set on the node the list already holds.
function syncRowSelection(body) {
  const selected = !pinnedExternalTask && selectedTaskId ? `task-${selectedTaskId}` : null;
  for (const row of body.querySelectorAll(".row")) {
    if (!row.dataset.key || !row.dataset.key.startsWith("task-")) continue;
    const on = row.dataset.key === selected;
    row.classList.toggle("expanded", on);
    const button = row.querySelector("[data-row-focus]");
    if (button) button.setAttribute("aria-expanded", String(on));
  }
}

// Work a render leaves for after both the list and the pane are in place:
// the list scroll a full page saved, and focus a closed pane hands back.
function finishTaskRender(body) {
  const active = document.activeElement;
  const pane = $("task-detail-pane");
  const focusLost = !active || active === document.body;
  if (listRestore && !taskPageId) {
    const restore = listRestore;
    listRestore = null;
    if (restore.scrollTop != null) body.scrollTop = restore.scrollTop;
    else revealTaskRow(restore.id, "center");
    if (focusLost || (pane && pane.contains && pane.contains(active))) focusTaskRow(restore.id);
  }
  if (rowFocusAfterRender) {
    const taskId = rowFocusAfterRender;
    rowFocusAfterRender = null;
    if (focusLost) focusTaskRow(taskId);
  }
}

// The pane beside the list: a head naming the task with its close, back and
// full-page controls, then the task's detail. A pinned resolver result shows
// here too, marked as outside the list.
function renderTaskPane(filtered, context) {
  const pane = $("task-detail-pane");
  const paneBody = $("task-detail-body");
  if (!pane || !paneBody) return;
  const ptask = pinnedExternalTask && pinnedExternalTask.task ? pinnedExternalTask.task : null;
  const listTask = !ptask && selectedTaskId ? filtered.find((task) => task.id === selectedTaskId) || null : null;
  const shown = ptask || listTask;
  const existing = new Map();
  for (const node of paneBody.children) {
    if (node.dataset.key) existing.set(node.dataset.key, node);
  }
  const drafts = openDraftNodes(paneBody);
  const nodes = [];
  if (shown) {
    nodes.push(buildPaneHead(shown, Boolean(ptask), context));
    if (ptask) {
      const draft = drafts.get(`pinned-${ptask.id}`);
      if (draft) refreshDraftDetails(draft, ptask, context);
      nodes.push(draft || buildPinnedTask(ptask, context));
    } else {
      nodes.push(buildListTaskDetail(listTask, drafts, existing, context));
    }
  } else if (taskPageId) {
    nodes.push(buildTaskPagePlaceholder(context));
  }
  syncNodes(paneBody, nodes);
  const open = nodes.length > 0;
  pane.hidden = !open;
  const main = $("tasks-main");
  if (main) {
    main.classList.toggle("task-pane-open", open);
    main.classList.toggle("task-page-mode", open && Boolean(taskPageId));
  }
  syncPaneChrome(open);
  if (focusPaneTitle && open) {
    focusPaneTitle = false;
    focusPaneHeading();
  }
}

function focusPaneHeading() {
  const title = $("task-detail-body")?.querySelector?.(".task-pane-title");
  if (title && typeof title.focus === "function") title.focus({ preventScroll: true });
}

// A list task's detail node, kept across renders while its payload and
// feedback are unchanged and kept verbatim while it holds a draft.
function buildListTaskDetail(t, drafts, existing, context) {
  const key = `detail-${t.id}`;
  const state = taskDetailState(t, context);
  const draft = drafts.get(key);
  if (draft) {
    refreshDraftDetails(draft, t, context, state.task || t);
    return draft;
  }
  const drain = taskDrainState(t, context);
  // Refresh expired Fix PR readiness before the reuse decision: a pull
  // request's checks and mergeability change on GitHub without any task
  // field changing, so an unchanged detail would otherwise keep a stale
  // button forever. fixPrState starts at most one read and only when the
  // cached entry is missing, stale or for another pull request; its pending
  // flag then changes fixPrSignature below and the detail rebuilds.
  const shownTask = state.task;
  if (shownTask && shownTask.status === "review" && taskPullRequestNumber(shownTask)) fixPrState(shownTask, context);
  // Diff by full task object stringified, plus the feedback the detail's own
  // controls render (the row hash only covers the crew). A placeholder diffs on
  // its read state instead; a detail rendered from the cached projection must
  // not be rebuilt just because a refresh has a read in flight.
  const readState = state.task ? "" : `${state.pending}-${state.error || ""}`;
  const baseHash = `${JSON.stringify(state.task || t)}-${readState}-${crewOptionsSignature()}-${detailFeedbackSignature(t.id)}-${shipInFlightTaskIds.has(taskDispatchIdentity(t))}-${pilotSignature(t)}-${triageSignature(t)}-${fixPrSignature(t)}`;
  const detailHash = `${baseHash}-${taskDrainSignature(drain)}`;
  const existingDetail = existing.get(key);
  if (existingDetail && existingDetail.dataset.hash === detailHash) return existingDetail;
  // Only the drain readiness changed (its read landed after the detail was
  // shown): repaint that card in place, so the detail — and whatever control
  // in it has focus — is not rebuilt under the operator.
  if (state.task && existingDetail && existingDetail.dataset.baseHash === baseHash && existingDetail.querySelector(".readiness-list")) {
    refreshDraftDetails(existingDetail, t, context, state.task);
    existingDetail.dataset.hash = detailHash;
    return existingDetail;
  }
  const detail = state.task
    ? buildTaskDetail(state.task, context, drain)
    : buildTaskDetailPlaceholder(t, state, context);
  const readiness = detail.querySelector(".readiness-list");
  if (readiness) readiness.dataset.drainSignature = taskDrainSignature(drain);
  detail.dataset.key = key;
  // The row's disclosure and the tabs point here, so the detail needs a real id.
  detail.id = key;
  detail.dataset.hash = detailHash;
  detail.dataset.baseHash = baseHash;
  return detail;
}

function buildPaneHead(task, pinned, context) {
  const page = taskPageId === task.id;
  const back = el("button", {
    class: "action task-pane-back",
    type: "button",
    text: page ? "Back to list" : "Back",
    title: page ? "Back to the task list at this task (Esc)" : "Back to the task list (Esc)",
  });
  back.addEventListener("click", () => closeTaskPane(context, { forget: true }));
  // Icon buttons: the glyph is CSS, the accessible name is the label.
  const full = page ? null : el("button", {
    class: "task-pane-icon task-pane-full",
    type: "button",
    title: "Open full page (Enter on its row)",
    "aria-label": "Open full page",
  });
  if (full) full.addEventListener("click", () => openTaskPage(task.id, context));
  const close = page ? null : el("button", {
    class: "task-pane-icon task-pane-close",
    type: "button",
    title: "Close (Esc)",
    "aria-label": "Close task detail",
  });
  if (close) close.addEventListener("click", () => closeTaskPane(context, { forget: true }));
  const key = `pane-head-${task.id}`;
  // The head carries only the id and title; status, crew, run and dates live in
  // the detail's Details section, which edits them.
  const baseHash = `${task.id}-${task.title}-${pinned}-${page}`;
  const title = el("h2", { class: "task-pane-title", id: "task-pane-title", text: task.title || task.id });
  title.tabIndex = -1;
  const id = makeCopyButton(task.id, { class: "id mono", title: "Copy task ID" });
  const spacer = el("span", { class: "task-pane-spacer" });
  const head = el("div", { class: "task-pane-head" }, [
    el("div", { class: "task-pane-bar" }, page
      ? [id, spacer, back]
      : [back, id, spacer, full, close]),
    title,
  ]);
  head.dataset.key = key;
  head.dataset.hash = baseHash;
  head.dataset.baseHash = baseHash;
  return head;
}

// A full page whose task is still being read, or could not be.
function buildTaskPagePlaceholder(context) {
  const back = el("button", { class: "action task-pane-back", type: "button", text: "Back to list" });
  back.addEventListener("click", () => closeTaskPane(context));
  const id = makeCopyButton(taskPageId, { class: "id mono", title: "Copy task ID" });
  const note = el("p", {
    class: taskPageError ? "panel-placeholder action-error" : "panel-placeholder",
    text: taskPageError || `Loading ${taskPageId}…`,
    role: "status",
    "aria-live": "polite",
  });
  const title = el("h2", { class: "task-pane-title", id: "task-pane-title", text: taskPageId });
  title.tabIndex = -1;
  const bar = el("div", { class: "task-pane-bar" }, [id, el("span", { class: "task-pane-spacer" }), back]);
  const node = el("div", { class: "task-pane-head" }, [bar, title, note]);
  node.dataset.key = `pane-page-${taskPageId}`;
  node.dataset.hash = taskPageError || "loading";
  return node;
}

// ---------------------------------------------------------------------------
// The pane's chrome: keyboard, the phone sheet, and the resizable width.

const PANE_WIDTH_KEY = "orbit.dashboard.taskPaneWidth";
const PANE_WIDTH_DEFAULT = 42;
const PANE_WIDTH_MIN = 25;
const PANE_WIDTH_MAX = 70;
const PHONE_SHEET_QUERY = "(max-width: 760px)";
let paneWidth = PANE_WIDTH_DEFAULT;
let sheetOpen = false;
let sheetInert = [];

function isEditableTarget(target) {
  return Boolean(target && (target.isContentEditable || (target.closest && target.closest("input, textarea, select"))));
}

function taskIdOfRow(row) {
  const key = row && row.dataset ? row.dataset.key || "" : "";
  return key.startsWith("task-") ? key.slice(5) : null;
}

// Roving focus moved to another row: while the pane shows a list task, it
// follows. A pinned result or a full page is not a list selection.
function followRovingSelection(button) {
  if (!selectedTaskId || pinnedExternalTask || taskPageId || !paneContext) return;
  const taskId = taskIdOfRow(button.closest ? button.closest(".row") : null);
  if (taskId && taskId !== selectedTaskId) selectTask(taskId, paneContext);
}

// j/k from inside the pane: select the next or previous row and keep the
// keyboard in the pane, on the new task's heading.
function moveTaskSelection(step) {
  const body = $("tasks-body");
  if (!body || !paneContext) return false;
  const buttons = Array.from(body.querySelectorAll("[data-row-focus]"))
    .filter((button) => typeof button.getClientRects !== "function" || button.getClientRects().length > 0);
  const at = buttons.findIndex((button) => taskIdOfRow(button.closest(".row")) === selectedTaskId);
  const next = at < 0 ? buttons[0] : buttons[at + step];
  const taskId = next ? taskIdOfRow(next.closest(".row")) : null;
  if (!taskId) return false;
  selectTask(taskId, paneContext);
  revealTaskRow(taskId);
  focusPaneHeading();
  return true;
}

function phoneSheet() {
  try {
    return typeof window.matchMedia === "function" && window.matchMedia(PHONE_SHEET_QUERY).matches;
  } catch (_) {
    return false;
  }
}

// At phone width the open pane is a full-screen sheet: a modal dialog whose
// background is inert until Back closes it.
function syncPaneChrome(open) {
  const pane = $("task-detail-pane");
  if (!pane) return;
  const sheet = open && phoneSheet() && !pane.closest?.(".tab-pane:not(.active)");
  if (sheet === sheetOpen) return;
  sheetOpen = sheet;
  if (sheet) {
    pane.setAttribute("role", "dialog");
    pane.setAttribute("aria-modal", "true");
    pane.setAttribute("aria-labelledby", "task-pane-title");
  } else if (typeof pane.removeAttribute === "function") {
    pane.removeAttribute("role");
    pane.removeAttribute("aria-modal");
    pane.removeAttribute("aria-labelledby");
  }
  setSheetInert(sheet ? pane : null);
  if (sheet) focusPaneHeading();
}

function setSheetInert(pane) {
  for (const node of sheetInert) node.inert = false;
  sheetInert = [];
  for (let node = pane; node && node.parentElement && node !== document.body; node = node.parentElement) {
    for (const sibling of node.parentElement.children) {
      if (sibling === node || sibling.inert || sibling.tagName === "SCRIPT") continue;
      sibling.inert = true;
      sheetInert.push(sibling);
    }
  }
}

function clampPaneWidth(value) {
  return Math.min(PANE_WIDTH_MAX, Math.max(PANE_WIDTH_MIN, value));
}

// The width is a per-viewer presentation preference: storage that is missing
// or refuses (private mode, quota) costs only the remembered width.
function loadPaneWidth() {
  try {
    const raw = window.localStorage.getItem(PANE_WIDTH_KEY);
    const value = raw == null ? NaN : Number(raw);
    return Number.isFinite(value) ? clampPaneWidth(value) : PANE_WIDTH_DEFAULT;
  } catch (_) {
    return PANE_WIDTH_DEFAULT;
  }
}

function savePaneWidth() {
  try {
    window.localStorage.setItem(PANE_WIDTH_KEY, String(paneWidth));
  } catch (_) {
    /* the width still applies for this page */
  }
}

function applyPaneWidth(value) {
  paneWidth = Math.round(clampPaneWidth(value) * 10) / 10;
  const main = $("tasks-main");
  if (main) main.style.setProperty("--task-pane-width", `${paneWidth}%`);
  const resizer = $("task-pane-resizer");
  if (resizer) {
    resizer.setAttribute("aria-valuenow", String(Math.round(paneWidth)));
    resizer.setAttribute("aria-valuetext", `Detail pane ${Math.round(paneWidth)}% of the width`);
  }
}

// The separator between the list and the pane: drag it, or focus it and use
// the arrow keys (Shift for larger steps), Home and End. A double click
// restores the default width.
function wirePaneResizer() {
  const resizer = $("task-pane-resizer");
  const main = $("tasks-main");
  if (!resizer || !main || resizer.dataset.wired === "true") return;
  resizer.dataset.wired = "true";
  let dragging = null;
  const widthAt = (clientX) => {
    const rect = main.getBoundingClientRect();
    const style = window.getComputedStyle(main);
    const left = rect.left + (parseFloat(style.paddingLeft) || 0);
    const right = rect.right - (parseFloat(style.paddingRight) || 0);
    return right > left ? ((right - clientX) / (right - left)) * 100 : paneWidth;
  };
  resizer.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    dragging = event.pointerId;
    if (typeof resizer.setPointerCapture === "function") resizer.setPointerCapture(event.pointerId);
    main.classList.add("task-pane-resizing");
  });
  resizer.addEventListener("pointermove", (event) => {
    if (dragging === event.pointerId) applyPaneWidth(widthAt(event.clientX));
  });
  const finish = (event) => {
    if (dragging !== event.pointerId) return;
    dragging = null;
    main.classList.remove("task-pane-resizing");
    savePaneWidth();
  };
  resizer.addEventListener("pointerup", finish);
  resizer.addEventListener("pointercancel", finish);
  resizer.addEventListener("dblclick", () => {
    applyPaneWidth(PANE_WIDTH_DEFAULT);
    savePaneWidth();
  });
  resizer.addEventListener("keydown", (event) => {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const step = event.shiftKey ? 10 : 2;
    let next = null;
    if (event.key === "ArrowLeft") next = paneWidth + step;
    else if (event.key === "ArrowRight") next = paneWidth - step;
    else if (event.key === "Home") next = PANE_WIDTH_MIN;
    else if (event.key === "End") next = PANE_WIDTH_MAX;
    if (next == null) return;
    event.preventDefault();
    applyPaneWidth(next);
    savePaneWidth();
  });
}

// Wired once at startup. Enter on the selected row opens its full page, Esc
// closes the pane (or leaves the full page), and j/k inside the pane move the
// selection. Keys typed into a field stay with the field.
export function wireTaskPane(context) {
  paneContext = paneContext || context;
  applyPaneWidth(loadPaneWidth());
  wirePaneResizer();
  const body = $("tasks-body");
  const pane = $("task-detail-pane");
  if (!body || !pane) return;
  body.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const target = event.target;
    if (!target || !target.matches || !target.matches("[data-row-focus]")) return;
    const taskId = taskIdOfRow(target.closest(".row"));
    if (!taskId || taskId !== selectedTaskId || pinnedExternalTask) return;
    event.preventDefault();
    openTaskPage(taskId, paneContext || context);
  });
  pane.addEventListener("keydown", (event) => {
    if ((event.key !== "j" && event.key !== "k") || event.altKey || event.ctrlKey || event.metaKey) return;
    if (taskPageId || pinnedExternalTask || isEditableTarget(event.target)) return;
    if (moveTaskSelection(event.key === "j" ? 1 : -1)) event.preventDefault();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    if (pane.hidden || pane.getClientRects().length === 0 || isEditableTarget(event.target)) return;
    // The composer and other dialogs of the page keep their own Esc.
    if (event.target && event.target.closest && event.target.closest("[role=dialog]:not(#task-detail-pane), #task-create-panel")) return;
    event.preventDefault();
    closeTaskPane(paneContext || context, { forget: true });
  });
  // The sheet follows the viewport and is released whenever Tasks is not the
  // destination on screen.
  try {
    window.matchMedia(PHONE_SHEET_QUERY).addEventListener("change", () => syncPaneChrome(!pane.hidden));
  } catch (_) {
    /* no media queries: the pane stays a side pane */
  }
  // After the router has switched destinations for the same hashchange.
  window.addEventListener("hashchange", () => setTimeout(() => syncPaneChrome(!pane.hidden), 0));
}
