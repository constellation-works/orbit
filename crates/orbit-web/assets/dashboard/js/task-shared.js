// Orbit dashboard task-domain shared state and helpers.
// The modules split out of tasks.js import this one, and it imports none of
// them, so they share state and re-render the list without import cycles.

import { clockStamp, el, getWorkspace, hostWriteRefusal, isAggregateView, isHttpUrl, runHref, withHost, withWorkspace } from './common.js';

export const $ = (id) => document.getElementById(id);

// Module-level state more than one task module reads or reassigns. An ES
// module cannot reassign an imported `let`, so each value is a property here.
export const taskState = {
  lastCrewPayload: { default_crew: null, crews: [], human_crews: [], viewer_human_crew: null },
  // The list row whose detail the side pane shows. One task at a time: the pane
  // beside the list replaces the inline accordion, so selecting another row
  // moves the pane to it and the list itself never reflows around a detail.
  selectedTaskId: null,
  taskActionNotice: null,
  pinnedExternalTask: null,
  // The task the Tasks hash names with `open=`: the one selected and still
  // shown, so a copied URL reopens it. `pendingOpenTaskId` is a hash-named task
  // that has not been shown yet; the next render selects it from the list or
  // pins it when the operator's filter hides it.
  openTaskId: null,
  pendingOpenTaskId: null,
  // `#tasks/<id>` shows the pane across the content width. `taskPageReturn`
  // remembers the list scroll when the page was entered from the list, so Back
  // lands on the same selection at the same place; `listRestore` applies that
  // on the next render. `taskPageError` is a page whose task could not be read.
  taskPageId: null,
  taskPageReturn: null,
  taskPageError: null,
  listRestore: null,
  focusPaneTitle: false,
  // A status change folded the pane: focus goes back to the row it came from.
  rowFocusAfterRender: null,
  // The last context a render ran with, for the keyboard and pane handlers that
  // are wired once.
  paneContext: null,
  // ORB-10874: per-task inline-edit feedback for the status/crew selects. Each
  // entry is `{ kind: 'pending'|'success'|'error', text, undo }`, where `undo`
  // (when present) is `{ previousValue, expiresAt }` — a bounded window in which
  // the prior value can still be restored with one click. Cleared automatically
  // once that window lapses (see scheduleFeedbackExpiry).
  statusFeedback: new Map(),
  crewFeedback: new Map(),
  // A status change made from a task's open detail folds that detail in: the row
  // has moved to its new status group. The change's feedback and undo then show
  // on the collapsed row, or in a notice above the list while the new status is
  // filtered out, until the feedback expires. Keyed by task id; the value is the
  // updated record, which the undo acts on when the list no longer holds the row.
  foldedStatusTasks: new Map(),
  // ORB-12235: the same feedback shape for the writable fields in the expanded
  // detail — the complexity select and the inline text editors. `fieldFeedback`
  // is keyed `<task id>:<field>` because one detail can hold several editors.
  complexityFeedback: new Map(),
  fieldFeedback: new Map(),
  // `open_decisions` on a task is compact: each request lists only its answer
  // ids. The label and one-line consequence that tell an operator what an id
  // does live on the decisions API, so the detail reads them from
  // `GET /api/decisions?task_id=` once per set of open requests and renders no
  // answer control until they are in hand.
  taskDecisionLoads: new Map(),
  // Operator records (pilot-admission / post-deploy): a refused record's typed
  // code and message, kept with the action like a refused recovery.
  operatorRecordRefusals: new Map(),
  // Guard duplicate clicks while a Ship request is pending. Once the server
  // accepts it, the server's 409 protects the live run; releasing this guard lets
  // a task returned to backlog be shipped again without reloading the page.
  shipInFlightTaskIds: new Set(),
  // ORB-12645: which comments the operator has opened, keyed
  // `<task id>#<index into task.comments>` so a refresh that rebuilds the detail
  // restores the thread the way it was left.
  expandedComments: new Set(),
  restoredCommentHash: null,
  // The comment a task link names (`#tasks?open=ID&comment=N`, `N` counted from
  // 1): `{ taskId, number }` while the route still names it, else null.
  commentTarget: null,
  // Tasks whose description the operator expanded; a rebuild keeps it open.
  expandedDescriptions: new Set(),
  // Tasks whose acceptance criteria the operator expanded; a rebuild keeps them open.
  expandedCriteria: new Set(),
};

// The task list renderer, registered by tasks.js at load. A task module that
// has to repaint the list calls `rerender` instead of importing tasks.js.
let taskRenderer = () => {};

export function setRenderer(fn) {
  taskRenderer = fn;
}

export function rerender(context) {
  taskRenderer(taskList(context), context);
}

export function taskList(context) {
  return context && typeof context.getTasks === "function" ? context.getTasks() : [];
}

export function searchQueryValue(context) {
  return context && typeof context.getSearchQuery === "function"
    ? context.getSearchQuery()
    : "";
}

export function activeStatusSet(context) {
  return context && typeof context.getActiveStatuses === "function"
    ? context.getActiveStatuses()
    : new Set();
}

export function setSearchQuery(context, value) {
  if (context && typeof context.setSearchQuery === "function") context.setSearchQuery(value);
}

export function setActiveStatuses(context, statuses) {
  if (context && typeof context.setActiveStatuses === "function") context.setActiveStatuses(statuses);
}

export function statusOrder(context) {
  return context && Array.isArray(context.statusOrder) ? context.statusOrder : [];
}

export function statusTransitions(task) {
  return Array.isArray(task && task.status_transitions)
    ? task.status_transitions.filter((transition) => transition && transition.status)
    : [];
}

export function statusTransition(task, targetStatus) {
  return statusTransitions(task).find((transition) => transition.status === targetStatus) || null;
}

export function fmtAbsTimeValue(context, value) {
  return context && typeof context.fmtAbsTime === "function"
    ? context.fmtAbsTime(value)
    : (value || "-");
}

// An absolute time in a task detail. The detail is kept while its task is
// unchanged, so the time is stamped for a UTC/Local switch to repaint.
export function absTimeNode(context, value, attrs = {}) {
  return clockStamp(el("span", { ...attrs, text: fmtAbsTimeValue(context, value) }), value);
}

export function refreshTasks(context) {
  return context && typeof context.refreshDashboard === "function"
    ? context.refreshDashboard()
    : Promise.resolve();
}

export function defaultActiveStatuses(context) {
  return context && Array.isArray(context.defaultActiveStatuses)
    ? context.defaultActiveStatuses
    : statusOrder(context);
}

export function tasksMeta(context) {
  return context && typeof context.getTasksMeta === "function" ? context.getTasksMeta() : null;
}

// ORB-10874: aggregate ("All workspaces") mode has no ambient workspace to
// scope a mutation to. A task fetched through /api/tasks/all carries its own
// `workspace_id`, so that's an explicit, workspace-qualified target; anything
// else is refused rather than silently applied to the wrong (or no) workspace.
export function canMutateTask(task) {
  return (!isAggregateView() || Boolean(task && task.workspace_id)) && !hostWriteRefusal();
}

// One sentence for every inline control that cannot write, so the refusal
// reads the same whichever control the operator hovers: a selected host that
// refuses forwarded writes, or the aggregate view.
export function aggregateRefusalTitle(label, action) {
  const host = hostWriteRefusal();
  if (host) return `${label} — ${host}`;
  return `${label} — select a specific workspace to ${action} in aggregate view`;
}

// A row's owner takes precedence over the page selection. Aggregate rows have
// no page selection, and selected views may still show a cross-workspace task.
export function taskWorkspacePath(task, path) {
  if (!task.workspace_id) return withWorkspace(path);
  const sep = path.includes("?") ? "&" : "?";
  return withHost(`${path}${sep}workspace=${encodeURIComponent(task.workspace_id)}`);
}

// The dashboard address of run `runId` in the task's own workspace, so an
// aggregate view or a task pinned from another workspace opens the run where it
// lives. A task with no known workspace falls back to the selected one.
export function taskRunHref(task, runId) {
  return runHref(runId, task && task.workspace_id ? { workspace: task.workspace_id } : {});
}

export function taskMutationPath(task, suffix = "") {
  return taskWorkspacePath(task, `/api/tasks/${encodeURIComponent(task.id)}${suffix}`);
}

export const GITHUB_PR_REF_SYSTEM = "github-pr";

// The delivery pull request a row links: the task's github-pr ref, when it
// carries an http(s) page. The server fills that page from the workspace's own
// origin remote for a ref recorded with only its number.
export function pullRequestLink(task) {
  const refs = Array.isArray(task.external_refs) ? task.external_refs : [];
  const ref = refs.find((candidate) => candidate && candidate.system === GITHUB_PR_REF_SYSTEM && isHttpUrl(candidate.url));
  return ref ? { number: String(ref.id || ""), url: ref.url } : null;
}

// A failed action is announced when it appears: the row's own controls are the
// only thing the operator is looking at, and a plain div is never read out.
export function actionErrorNode(text, remedy) {
  const node = el("div", { class: "action-error", text });
  node.setAttribute("role", "alert");
  if (remedy) node.appendChild(el("div", { class: "action-error-remedy", text: remedy }));
  return node;
}

export function plural(count, word) {
  return `${count} ${word}${count === 1 ? "" : "s"}`;
}

export function taskDispatchIdentity(task) {
  return `${task.workspace_id || getWorkspace() || ''}:${task.id}`;
}

export const MUTATION_UNDO_WINDOW_MS = 8000;

// `os:` tags route a task to a host of that OS, so they read apart from the
// labels around them.
export function isOsTag(tag) {
  return typeof tag === "string" && tag.toLowerCase().startsWith("os:");
}

// Chip words for each status. The five open statuses are the filter's
// primary chips; the closed ones read as quieter text until they are on.
export const STATUS_LABELS = {
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
