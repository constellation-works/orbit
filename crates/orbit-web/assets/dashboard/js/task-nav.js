// Orbit dashboard task list navigation: row selection, the task page, pins, the hash, filters.

import { captureWorkspaceVisit, getSelectedWorkspaces, getWorkspace, isWorkspaceListReady, onWorkspaceChange, onWorkspaceNamesChange, panelCanRender, persistScopeToUrl, rememberWorkspace, setSelectedWorkspaces, setWorkspace, el, fetchJson, isAggregateView, copyText, workspaceCatalogEntries, workspaceColor, workspaceDisplayName } from './common.js';
import { $, taskState, onDecisionAnswered, rerender, taskList, activeStatusSet, setActiveStatuses, statusOrder, refreshTasks, defaultActiveStatuses, tasksMeta, aggregateRefusalTitle, taskMutationPath, taskWorkspacePath, taskDispatchIdentity, isSelectedTask, STATUS_LABELS } from './task-shared.js';
import { applyUpdatedTask, forgetTaskDetailLoad, mergeTaskRecord, taskDetailSnapshot } from './task-data.js';
import * as taskReadiness from './task-readiness.js';
import { humanCrewFor, isOpenTask, isMyWorkTask, loadAggregateCrews, myWorkCrewNames } from './task-crew.js';
import { recordRoutedHref } from './router.js';
import { quickFilterActive, quickFilterMatches } from './task-quick-filter.js';

// Advances whenever `taskState.openTaskId` changes, so a pin read for a task
// the hash no longer names is dropped.
let openTaskGeneration = 0;
let returningFromPage = false;

export function stopRowInteraction(node) {
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
// "My work" keeps tasks assigned to the viewer's human crew (every human crew
// when the server could not tell whose browser this is): operator work no
// drain picks up. The chip shows only when the workspace has a human crew.
let myWorkOnly = false;
// "Waiting on you": open human-crew tasks plus tasks with open decisions.
let waitingOnly = false;
let waitingDecisionTaskIds = null;
let waitingFetchAt = 0;
let waitingFetchPending = false;
// Advances whenever the cached ids stop being trustworthy (a workspace switch or
// an answered decision), so a read that began before it is dropped and redone.
let waitingGeneration = 0;
// Whether a filter only the browser applies (Not ready, My work, Waiting on
// you) narrows the loaded rows. The server's section totals do not count it.
export function browserTaskFilterActive() { return notReadyOnly || myWorkOnly || waitingOnly; }
export function taskWaitsOnYou(t) {
  if (!isOpenTask(t)) return false;
  if (Array.isArray(t.open_decisions) && t.open_decisions.length) return true;
  if (waitingDecisionTaskIds && waitingDecisionTaskIds.has(t.id)) return true;
  return Boolean(humanCrewFor(t));
}
function waitingCountFromTasks(context) {
  return taskList(context).filter(taskWaitsOnYou).length;
}
// Derived from the live task list on every paint, never cached at fetch time: a
// count stored when /api/decisions answered went stale for the 15 s throttle
// whenever the task list changed after it (a human-crew task arriving in a
// later poll read as 0). Open decisions for tasks outside the loaded list still
// count, by id. A cached id for a task the list shows counts only while that task
// is open, so a failed read that keeps the ids cannot count a settled task.
function waitingCountNow(context) {
  if (isAggregateView() || !waitingDecisionTaskIds) return waitingCountFromTasks(context);
  const tasks = taskList(context);
  const listed = new Map(tasks.map((t) => [t.id, t]));
  const ids = new Set(tasks.filter(taskWaitsOnYou).map((t) => t.id));
  for (const id of waitingDecisionTaskIds) {
    const live = listed.get(id);
    if (!live || isOpenTask(live)) ids.add(id);
  }
  return ids.size;
}
// The cache holds the ids of the workspace and moment it was read; forget it so
// the next chip refresh reads /api/decisions again.
function invalidateWaitingDecisions() {
  waitingGeneration += 1;
  waitingFetchAt = 0;
}
function resetWaitingDecisions() {
  invalidateWaitingDecisions();
  waitingDecisionTaskIds = null;
}
onWorkspaceChange(resetWaitingDecisions);
onDecisionAnswered(invalidateWaitingDecisions);
function sameIds(a, b) {
  return Boolean(a) && Boolean(b) && a.size === b.size && [...a].every((id) => b.has(id));
}
// Throttled: callers on every chip refresh may call it freely.
async function refreshWaitingCount(context) {
  if (!isWorkspaceListReady() || (!getWorkspace() && !isAggregateView())) return;
  if (waitingFetchPending || Date.now() - waitingFetchAt < 15000) return;
  // Federated/aggregate: loaded tasks already carry open_decisions and crew,
  // so count locally — a workspace-spanning /api/decisions read has no scope.
  if (isAggregateView()) { waitingDecisionTaskIds = null; waitingFetchAt = Date.now(); renderChips(context); return; }
  waitingFetchPending = true;
  const generation = waitingGeneration;
  let redo = false;
  let changed = false;
  try {
    const visit = captureWorkspaceVisit();
    const payload = await fetchJson(visit.path("/api/decisions"));
    if (!visit.isCurrent() || generation !== waitingGeneration) { redo = true; return; }
    const items = Array.isArray(payload?.items) ? payload.items : [];
    const ids = new Set(items.map((r) => r && r.task_id).filter(Boolean));
    changed = !sameIds(ids, waitingDecisionTaskIds);
    waitingDecisionTaskIds = ids;
    waitingFetchAt = Date.now();
  } catch {
    if (generation !== waitingGeneration) redo = true;
    else waitingFetchAt = Date.now();
  } finally {
    waitingFetchPending = false;
    // Rows filtered by the old ids repaint; otherwise only the chip changes.
    if (changed && waitingOnly) rerender(context); else renderChips(context);
    if (redo) refreshWaitingCount(context);
  }
}

// `quick: false` leaves out the Shift-Shift quick filter, for the rows it narrows.
export function filterTasks(tasks, context, { quick = true } = {}) {
  const activeStatuses = activeStatusSet(context);
  const quickNarrows = quick && quickFilterActive();
  return tasks.filter((t) => {
    if (!activeStatuses.has(t.status)) return false;
    if (notReadyOnly && !taskReadiness.isNotReady(t)) return false;
    if (pilotWarningsOnly && !taskReadiness.pilotWarnings(t).length) return false;
    if (myWorkOnly && !isMyWorkTask(t)) return false;
    if (waitingOnly && !taskWaitsOnYou(t)) return false;
    if (quickNarrows && !quickFilterMatches(t)) return false;
    return true;
  });
}

export async function copyTaskIdWithNotice(taskId, context) {
  const copied = await copyText(taskId);
  taskState.taskActionNotice = `${taskId} is not in the filtered task list; ${copied ? "copied ID" : "could not copy the ID"}`;
  rerender(context);
}

function findTaskRow(taskId) {
  return Array.from(document.querySelectorAll("#tasks-body .row"))
    .find((row) => row.dataset.taskId === taskId &&
      (!taskState.selectedTaskWorkspace || row.dataset.workspace === taskState.selectedTaskWorkspace)) || null;
}

export function openVisibleTask(taskId, context) {
  // A full page has no list beside it: the link opens that task's own page,
  // whatever the list's filter hides.
  if (taskState.taskPageId) {
    if (taskState.taskPageId !== taskId) openLinkedTaskPage(taskId, context);
    return;
  }
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
export function revealTaskRow(taskId, block = "nearest") {
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
export function selectTask(taskId, context, { anchor = null, workspace = null } = {}) {
  const top = anchor && typeof anchor.getBoundingClientRect === "function" ? anchor.getBoundingClientRect().top : null;
  taskState.pinnedExternalTask = null;
  taskState.selectedTaskId = taskId;
  const owner = taskList(context).find(task => task.id === taskId && (!workspace || task.workspace_id === workspace));
  taskState.selectedTaskWorkspace = workspace || owner?.workspace_id || getWorkspace() || null;
  setOpenTask(taskId);
  rerender(context);
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
export function closeTaskPane(context, { forget = false } = {}) {
  if (taskState.taskPageId) {
    leaveTaskPage(context);
    return;
  }
  const taskId = shownTaskId();
  if (!taskId) return;
  const owner = taskState.pinnedExternalTask?.task || taskList(context).find(isSelectedTask);
  const pane = $("task-detail-pane");
  const active = document.activeElement;
  const focusWasInPane = !active || active === document.body || Boolean(pane && pane.contains && pane.contains(active));
  const row = findTaskRow(taskId);
  const top = row && typeof row.getBoundingClientRect === "function" ? row.getBoundingClientRect().top : null;
  if (taskState.pinnedExternalTask && taskState.pinnedExternalTask.id === taskId) taskState.pinnedExternalTask = null;
  if (taskState.selectedTaskId === taskId) taskState.selectedTaskId = null;
  if (taskState.openTaskId === taskId) setOpenTask(null);
  // Reopening re-reads the detail, which is also the retry for a read that
  // failed.
  if (forget) forgetTaskDetailLoad(owner || { id: taskId, workspace_id: taskState.selectedTaskWorkspace });
  rerender(context);
  keepRowInPlace(taskId, top);
  if (focusWasInPane) focusTaskRow(taskId);
}

export function focusTaskRow(taskId) {
  const button = findTaskRow(taskId)?.querySelector?.("[data-row-focus]");
  if (button && typeof button.focus === "function") button.focus({ preventScroll: true });
}

// The task the pane holds right now: the pinned resolver result, else the
// selected row.
function shownTaskId() {
  return (taskState.pinnedExternalTask && taskState.pinnedExternalTask.id) || taskState.selectedTaskId || null;
}

// Enter the full-page view of a task. The list behind it keeps its state; the
// scroll is remembered because a hidden list loses it.
export function openTaskPage(taskId, context) {
  const body = $("tasks-body");
  taskState.taskPageReturn = { id: taskId, scrollTop: body ? body.scrollTop : 0 };
  taskState.taskPageId = taskId;
  taskState.taskPageError = null;
  taskState.focusPaneTitle = true;
  if (taskState.openTaskId !== taskId) setOpenTask(taskId);
  rerender(context);
  window.location.hash = taskPageHash(taskId);
}

// Follow a link from one full page to another task's page. The route changes
// with the state, so the address, reload and Back name the task on screen; Back
// returns to the page left, and the page has no list entry behind it.
function openLinkedTaskPage(taskId, context) {
  taskState.taskPageReturn = null;
  taskState.taskPageId = taskId;
  taskState.taskPageError = null;
  taskState.taskActionNotice = null;
  taskState.focusPaneTitle = true;
  taskState.selectedTaskId = null;
  taskState.selectedTaskWorkspace = null;
  taskState.pinnedExternalTask = null;
  taskState.pendingOpenTaskId = taskId;
  setOpenTask(taskId);
  rerender(context);
  window.location.hash = taskPageHash(taskId);
}

// Leave the page state; the next render restores the list where it was.
function exitTaskPageState() {
  if (!taskState.taskPageId) return;
  const back = taskState.taskPageReturn && taskState.taskPageReturn.id === taskState.taskPageId ? taskState.taskPageReturn : null;
  taskState.listRestore = { id: taskState.taskPageId, scrollTop: back ? back.scrollTop : null };
  taskState.taskPageId = null;
  taskState.taskPageReturn = null;
  taskState.taskPageError = null;
}

// Back to the list at the same selection. A page entered from the list goes
// back through history, so the browser's own Back agrees with this control; a
// page reached by a link has no list entry behind it and opens one.
function leaveTaskPage(context) {
  const taskId = taskState.taskPageId;
  if (!taskId) return;
  const fromList = Boolean(taskState.taskPageReturn && taskState.taskPageReturn.id === taskId);
  exitTaskPageState();
  if (!taskState.pinnedExternalTask && filterTasks(taskList(context), context).some((task) => task.id === taskId)) {
    taskState.selectedTaskId = taskId;
  }
  returningFromPage = true;
  rerender(context);
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
  const target = taskState.commentTarget;
  if (target && target.taskId === taskId) params.set("comment", String(target.number));
  else params.delete("comment");
  const query = params.toString();
  const next = query ? `#tasks?${query}` : "#tasks";
  if (next !== hash) window.history.replaceState(null, "", next);
}

export function setOpenTask(taskId) {
  openTaskGeneration += 1;
  taskState.openTaskId = taskId;
  // A comment link names one task's thread; another task drops it.
  if (taskState.commentTarget && taskState.commentTarget.taskId !== taskId) taskState.commentTarget = null;
  writeOpenTaskToUrl(taskId);
}

// Every path that closes a task detail goes through here so the URL stops
// naming a task the page no longer shows. A full-page task stays: the page is
// that task, and its actions repaint it in place.
export function collapseTask(taskId) {
  if (taskState.taskPageId === taskId) return;
  if (taskState.selectedTaskId === taskId) taskState.selectedTaskId = null;
  if (taskState.openTaskId === taskId) setOpenTask(null);
}

// Show the task a hash names, once the list that may contain it has rendered.
// A task the filter or page hides is fetched and pinned instead, so following
// a link never rewrites the saved status filters.
export function resolvePendingOpenTask(tasks, context) {
  const filtered = filterTasks(tasks, context);
  // The page's route keeps owning its task after a status edit or a refreshed
  // list drops its row. Promote its last full record to the existing pinned
  // path, which also re-reads filtered-out tasks on subsequent refreshes.
  const taskId = taskState.pendingOpenTaskId || (taskState.taskPageId && taskState.selectedTaskId === taskState.taskPageId &&
    !filtered.some(isSelectedTask) ? taskState.taskPageId : null);
  if (!taskId) return;
  taskState.pendingOpenTaskId = null;
  const workspace = taskState.taskPageId === taskId || taskState.selectedTaskId === taskId ? taskState.selectedTaskWorkspace : null;
  const listed = filtered.find(task => task.id === taskId && (!workspace || taskDispatchIdentity(task) === `${workspace}:${taskId}`));
  if (listed) {
    // A route replay for the row already selected leaves the list where it
    // is, and coming back from the full page restores the list's own scroll.
    const settled = taskState.selectedTaskId === taskId || (taskState.listRestore && taskState.listRestore.id === taskId);
    taskState.selectedTaskId = taskId;
    taskState.selectedTaskWorkspace = listed.workspace_id || getWorkspace() || null;
    taskState.pinnedExternalTask = null;
    if (!settled) revealTaskRow(taskId, "center");
  } else if (!(taskState.pinnedExternalTask && taskState.pinnedExternalTask.id === taskId)) {
    const cached = taskState.taskPageId === taskId ? taskDetailSnapshot({ id: taskId, workspace_id: taskState.selectedTaskWorkspace }) : null;
    // Retire the list selection once it becomes a pin, including while its
    // first read is pending or failed; ordinary renders must not retry it.
    if (taskState.taskPageId === taskId) taskState.selectedTaskId = null;
    if (cached) taskState.pinnedExternalTask = { task: cached, id: taskId };
    else pinOpenTask(taskId, context, tasks);
  }
}

async function pinOpenTask(taskId, context, tasks = []) {
  const visit = captureWorkspaceVisit();
  const generation = openTaskGeneration;
  const superseded = () => !visit.isCurrent() || taskState.openTaskId !== taskId || generation !== openTaskGeneration;
  try {
    // Scope the read to the owner the list already names: an unscoped read in
    // the aggregate view falls back to the default workspace and can pin a
    // different task (or a false not-found) for this id.
    const workspace = taskState.taskPageId === taskId ? taskState.selectedTaskWorkspace : null;
    const owner = (Array.isArray(tasks) ? tasks : []).find(task => task && task.id === taskId && (!workspace || task.workspace_id === workspace));
    const ownerTask = { id: taskId, workspace_id: workspace || owner?.workspace_id };
    const task = await fetchJson(taskWorkspacePath(ownerTask, `/api/tasks/${encodeURIComponent(taskId)}`));
    if (superseded()) return;
    if (!task || task.id !== taskId) throw new Error(`${taskId} not found`);
    if (ownerTask.workspace_id && task.workspace_id && task.workspace_id !== ownerTask.workspace_id) throw new Error(`${taskId} not found`);
    taskState.pinnedExternalTask = { task: mergeTaskRecord(ownerTask, task), id: taskId };
    if (isAggregateView()) await loadAggregateCrews([taskState.pinnedExternalTask.task]);
  } catch (error) {
    if (superseded()) return;
    taskState.taskActionNotice = error && error.status === 404
      ? `${taskId} not found`
      : `Could not open ${taskId}: ${(error && error.message) || error}`;
    // A full page has no list on screen to carry the notice.
    if (taskState.taskPageId === taskId) taskState.taskPageError = taskState.taskActionNotice;
  }
  rerender(context);
}

let pinnedRefreshInFlight = null;

// Re-read a pinned task from its owner on a list refresh or change notice.
// The pinned object is the only copy of a filtered-out task, so without this
// it stays at whatever the first open fetched. Drafts are preserved by the
// pane's draft retention; this only replaces the cached record, guarded by
// the workspace visit and the open-task generation like the first read, and
// by the starting record so a completed save wins over an older response.
export function refreshPinnedTask(context) {
  const pinned = taskState.pinnedExternalTask;
  if (!pinned || !pinned.task) return Promise.resolve();
  const taskId = pinned.id || pinned.task.id;
  if (!taskId) return Promise.resolve();
  if (pinnedRefreshInFlight && pinnedRefreshInFlight.id === taskId) return pinnedRefreshInFlight.promise;
  const visit = captureWorkspaceVisit();
  const generation = openTaskGeneration;
  const previous = pinned.task;
  const superseded = () => !visit.isCurrent() || taskState.openTaskId !== taskId || generation !== openTaskGeneration
    || !taskState.pinnedExternalTask || taskState.pinnedExternalTask.id !== taskId;
  const promise = (async () => {
    try {
      const task = await fetchJson(taskMutationPath(previous));
      if (superseded()) return;
      // Check only before applying the response: our own update also replaces
      // the pinned record, but must still reach the repaint below.
      if (taskState.pinnedExternalTask.task !== previous) return;
      if (!task || task.id !== taskId) return;
      const merged = mergeTaskRecord(previous, task);
      taskState.pinnedExternalTask = { task: merged, id: taskId };
      applyUpdatedTask(merged, context);
      if (isAggregateView()) await loadAggregateCrews([merged]);
    } catch {
      // Keep the cached copy on a failed refresh; the next refresh retries.
    } finally {
      if (pinnedRefreshInFlight && pinnedRefreshInFlight.promise === promise) pinnedRefreshInFlight = null;
    }
    if (!superseded()) rerender(context);
  })();
  pinnedRefreshInFlight = { id: taskId, promise };
  return promise;
}

// Per-status counts for the chips: each section's total under the server-side
// filter, loaded or not. A status that is off was never read.
function statusCounts(context) {
  if (!panelCanRender("tasks-body")) return null;
  const sections = tasksMeta(context)?.sections;
  if (!sections) return null;
  const counts = new Map();
  for (const [status, section] of sections) counts.set(status, section.total);
  return counts;
}

// Repaints the chips and keeps the "Waiting on you" decision ids fresh.
export function refreshChips(context) {
  renderChips(context);
  if ($("task-filter")) refreshWaitingCount(context);
}

function renderChips(context) {
  const container = $("task-filter");
  if (!container) return;
  const activeStatuses = activeStatusSet(context);
  const allOn = activeStatuses.size === statusOrder(context).length;
  const counts = statusCounts(context);
  const myWorkCount = taskList(context).filter(isMyWorkTask).length;
  for (const chip of container.querySelectorAll(".chip[data-status], .chip[data-role]")) {
    if (chip.dataset.role === "workspaces") continue;
    const status = chip.dataset.status;
    const isAll = chip.dataset.role === "all";
    const on = chip.dataset.role === "not-ready"
      ? notReadyOnly
      : chip.dataset.role === "pilot-warnings" ? pilotWarningsOnly
      : chip.dataset.role === "waiting" ? waitingOnly
      : chip.dataset.role === "my-work" ? myWorkOnly
      : isAll ? allOn : activeStatuses.has(status);
    chip.classList.toggle("active", on);
    if (chip.dataset.role === "my-work") chip.hidden = myWorkCrewNames().length === 0 && !myWorkOnly && myWorkCount === 0;
    // ORB-10874: chip state must not rely on the active/inactive color
    // difference alone — aria-pressed exposes it to assistive tech too.
    chip.setAttribute("aria-pressed", on ? "true" : "false");
    const count = chip.querySelector(".chip-count");
    if (!count) continue;
    if (chip.dataset.role === "waiting") {
      // Updated even while its chip is hidden, unlike a status count that clears while its chip is off.
      count.textContent = String(waitingCountNow(context));
    } else if (chip.dataset.role === "my-work") {
      count.textContent = String(myWorkCount);
    } else {
      count.textContent = status && on && counts ? String(counts.get(status) || 0) : "";
    }
  }
  paintWorkspaceFilter();
}

// The status filters are represented in the tasks
// hash (`#tasks?status=a,b`), mirroring the audit tab's buildAuditHash/
// applyAuditHashQuery pair, so a reload or the browser's back/forward button
// restores the same filtered view instead of resetting to the default.
export function buildTasksHash(context) {
  // The full-page view is its own route; the list filter waits behind it.
  if (taskState.taskPageId) return taskPageHash(taskState.taskPageId);
  return buildTasksListHash(context);
}

// `#tasks/<id>`, keeping the comment a link named while it is that task's.
function taskPageHash(taskId) {
  const target = taskState.commentTarget;
  const comment = target && target.taskId === taskId ? `?comment=${target.number}` : "";
  return `#tasks/${encodeURIComponent(taskId)}${comment}`;
}

// The list filter as query parameters; the open task is added by
// buildTasksListHash.
function filterParams(context) {
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
  if (myWorkOnly) sp.set("crew", "mine");
  if (waitingOnly) sp.set("waiting", "you");
  return sp;
}

function buildTasksListHash(context) {
  const sp = filterParams(context);
  if (taskState.openTaskId) sp.set("open", taskState.openTaskId);
  const target = taskState.commentTarget;
  if (target && target.taskId === taskState.openTaskId) sp.set("comment", String(target.number));
  const qs = sp.toString();
  return qs ? `#tasks?${qs}` : "#tasks";
}

// The list filter alone, which the rail's Tasks item reopens. An open task or
// a page is not carried over: a hash naming one takes the in-place rewrite,
// which skips the refresh, so the page layout would stay on screen.
export function buildTasksFilterHash(context) {
  const qs = filterParams(context).toString();
  return qs ? `#tasks?${qs}` : "#tasks";
}

// `page` is the `#tasks/<id>` route segment: that task's full-page view. It
// names only the task, so the list filter behind it is left as it was.
export function applyTasksHashQuery(query, context, page = null) {
  const pageId = parseOpenTaskId(page);
  // A router replay of the route we just wrote is still the same lookup.
  // Retire it only when navigation actually changes the represented state.
  const current = buildTasksHash(context);
  const open = pageId || parseOpenTaskId(query.get("open"));
  const commentNumber = open && /^[1-9]\d{0,5}$/.test(query.get("comment") || "") ? Number(query.get("comment")) : null;
  const replay = pageId
    ? current === `#tasks/${encodeURIComponent(pageId)}${commentNumber ? `?comment=${commentNumber}` : ""}`
    : query.toString() === (current.split("?")[1] || "");
  if (taskState.openTaskId !== open) {
    taskState.selectedTaskWorkspace = null;
    if (taskState.selectedTaskId && taskState.selectedTaskId !== open) taskState.selectedTaskId = null;
    taskState.pinnedExternalTask = null;
    taskState.taskActionNotice = null;
  }
  openTaskGeneration += 1;
  taskState.openTaskId = open;
  taskState.pendingOpenTaskId = open;
  taskState.commentTarget = commentNumber ? { taskId: open, number: commentNumber } : null;
  if (pageId) {
    if (taskState.taskPageId !== pageId) taskState.taskPageError = null;
    taskState.taskPageId = pageId;
    return;
  }
  // Back from the full page (this pane's control or the browser's) returns to
  // the list as it was: the same filter, page and scroll.
  const fromPage = Boolean(taskState.taskPageId) || returningFromPage;
  returningFromPage = false;
  exitTaskPageState();
  if (fromPage && query.toString() === (buildTasksListHash(context).split("?")[1] || "")) return;
  // A link that only names a task (`#tasks?open=ID`) opens it under whatever
  // filter the operator already has; reading it as "no filter" would reset
  // the chips to their defaults.
  if (open && !["status", "ready", "pilot", "crew", "waiting"].some((key) => query.has(key))) return;
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
  myWorkOnly = query.get("crew") === "mine";
  waitingOnly = query.get("waiting") === "you";
}

// Re-sync chip states after the hash changed them, including Back/Forward.
export function syncTaskControls(context) {
  refreshChips(context);
}

// An empty hash is the default Tasks board, as on a bare dashboard load.
// Explicit Tasks entries include the list and a full page.
function onTasksEntry() {
  const hash = window.location.hash || "";
  return hash === "" || /^#tasks([/?]|$)/.test(hash);
}

// A filter change rewrites the current Tasks entry rather than pushing one, so
// only a destination adds a Back step (`history.length` is unchanged). Without
// a hashchange, the state the router would derive from the new address is
// applied here, and the router's last-routed address moves with it so a later
// navigation to that address is not skipped (ORB-19783).
function navigateTasksHash(context) {
  if (context && typeof context.resetTaskPagination === "function") {
    context.resetTaskPagination();
  }
  const hash = buildTasksHash(context);
  if (window.location.hash === hash) {
    refreshChips(context);
    refreshTasks(context).catch((error) => console.error("Failed to reset task pages", error));
    return;
  }
  if (!onTasksEntry()) {
    window.location.hash = hash;
    return;
  }
  window.history.replaceState(null, "", hash);
  recordRoutedHref();
  applyTasksHashQuery(new URLSearchParams(hash.split("?")[1] || ""), context, taskState.taskPageId);
  refreshChips(context);
  refreshTasks(context).catch((error) => console.error("Failed to reset task pages", error));
}

export function buildChips(context) {
  const container = $("task-filter");
  container.innerHTML = "";
  const chip = (text, attrs, quiet, title) => {
    const node = el("button", { class: `chip${quiet ? " chip-quiet" : ""}`, type: "button", "aria-description": title }, [
      el("span", { class: "chip-label", text }),
      ...((attrs.status || attrs.role === "waiting" || attrs.role === "my-work") ? [el("span", { class: "chip-count" })] : []),
    ]);
    Object.assign(node.dataset, attrs);
    return node;
  };
  // The open statuses lead as chips with a dot and a count; My work, Waiting
  // on you, the closed statuses, All and Not ready wait behind More, a
  // disclosure. A quiet chip that is on stays in view while More is closed, so
  // the filter the list obeys is always on screen.
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
  const myWork = chip("My work", { role: "my-work" }, true, "Open tasks assigned to your human crew: operator work no drain or task pilot picks up");
  myWork.classList.add("my-work-filter");
  myWork.addEventListener("click", () => {
    myWorkOnly = !myWorkOnly;
    navigateTasksHash(context);
  });
  const waiting = chip("Waiting on you", { role: "waiting" }, true, "Open human-crew tasks and tasks with an open decision");
  waiting.classList.add("waiting-filter");
  waiting.addEventListener("click", () => { waitingOnly = !waitingOnly; navigateTasksHash(context); });
  secondary.prepend(waiting);
  secondary.prepend(myWork);
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
    class: "chip chip-quiet",
    type: "button",
    "aria-controls": "task-filter-more",
    "aria-expanded": "false",
    "aria-description": "My work, Waiting on you, closed statuses, All, Not ready and Pilot warnings",
  }, [el("span", { text: "More" })]);
  more.addEventListener("click", () => {
    const open = more.getAttribute("aria-expanded") !== "true";
    more.setAttribute("aria-expanded", String(open));
    secondary.classList.toggle("open", open);
  });
  const workspaceFilter = buildWorkspaceFilter(context);
  container.append(primary, more, workspaceFilter, secondary);
  refreshChips(context);
}

// All workspaces narrows from here, immediately after More. A single board
// hides it. An empty filter checks every active workspace and omits
// `workspaces=`; two or more narrow the list; one remaining opens that board.
let workspaceFilterContext = null;
let workspaceFilterOptionsKey = "";

function activeWorkspaceEntries() {
  return workspaceCatalogEntries().filter((workspace) => workspace && workspace.status === "active" && workspace.id);
}

function filterCoversActive(ids, activeIds) {
  if (!ids.length || ids.length < activeIds.length) return false;
  const have = new Set(ids);
  return activeIds.every((id) => have.has(id));
}

function placeTaskWorkspaceList(anchor, list) {
  list.hidden = false;
  const rect = anchor.getBoundingClientRect();
  const width = Math.min(list.offsetWidth || rect.width, window.innerWidth - 24);
  const left = Math.max(12, Math.min(rect.left, window.innerWidth - width - 12));
  const height = list.offsetHeight;
  const below = rect.bottom + 4;
  const top = below + height + 12 <= window.innerHeight ? below : Math.max(12, rect.top - height - 4);
  list.style.left = `${left}px`;
  list.style.top = `${top}px`;
}

function closeTaskWorkspaceFilter() {
  const list = $("task-workspace-options");
  const button = document.querySelector("#task-filter [data-role='workspaces']");
  if (list) list.hidden = true;
  if (button) button.setAttribute("aria-expanded", "false");
}

function syncTaskWorkspaceOptions(list, entries) {
  const key = entries.map((workspace) => `${workspace.id}\t${workspace.status}\t${workspace.name || ""}`).join("\n");
  if (key === workspaceFilterOptionsKey && list.childElementCount === entries.length) return;
  workspaceFilterOptionsKey = key;
  list.replaceChildren();
  for (const workspace of entries) {
    const available = workspace.status === "active";
    const checkbox = el("input", { type: "checkbox", "aria-hidden": "true" });
    checkbox.tabIndex = -1;
    const option = el("div", {
      role: "option",
      "aria-selected": "false",
      "aria-disabled": String(!available),
    }, [checkbox, `${workspace.name || workspace.id}${available ? "" : " (unavailable)"}`]);
    option.dataset.wsColor = String(workspaceColor(workspace.id));
    option.dataset.workspace = workspace.id;
    option.tabIndex = -1;
    const commit = () => {
      if (!available) return;
      const switched = applyTaskWorkspaceFilter(workspace.id);
      if (!switched && option.isConnected) option.focus();
    };
    // pointerdown runs before focusout can close the list, and preventDefault
    // keeps the key target so a click does not commit a second time.
    option.addEventListener("pointerdown", (event) => { event.preventDefault(); commit(); });
    option.addEventListener("keydown", (event) => {
      const options = [...list.children];
      let index = options.indexOf(option);
      if (event.key === " " || event.key === "Enter") { event.preventDefault(); commit(); }
      else if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
        event.preventDefault();
        index = event.key === "Home" ? 0 : event.key === "End" ? options.length - 1 : (index + (event.key === "ArrowDown" ? 1 : -1) + options.length) % options.length;
        options[index].focus();
      } else if (event.key === "Escape") {
        event.preventDefault();
        closeTaskWorkspaceFilter();
        document.querySelector("#task-filter [data-role='workspaces']")?.focus();
      }
    });
    list.append(option);
  }
}

function applyTaskWorkspaceFilter(id) {
  const activeIds = activeWorkspaceEntries().map((workspace) => workspace.id);
  const current = isAggregateView() ? getSelectedWorkspaces() : [];
  const base = !current.length || filterCoversActive(current, activeIds) ? activeIds : current;
  const next = base.includes(id) ? base.filter((item) => item !== id) : [...base, id];
  const kept = [...new Set(next)].filter((item) => activeIds.includes(item)).sort();
  if (kept.length <= 1) {
    const only = kept[0];
    if (!only) return false;
    setSelectedWorkspaces([]);
    setWorkspace(only);
    rememberWorkspace();
    persistScopeToUrl();
    closeTaskWorkspaceFilter();
    document.getElementById("workspace-select")?.focus();
    refreshTasks(workspaceFilterContext);
    return true;
  }
  if (kept.length === activeIds.length) setSelectedWorkspaces([]);
  else setSelectedWorkspaces(kept);
  rememberWorkspace();
  persistScopeToUrl();
  paintWorkspaceFilter();
  refreshTasks(workspaceFilterContext);
  return false;
}

function paintWorkspaceFilter() {
  const wrap = document.querySelector("#task-filter .workspace-filter");
  const button = wrap?.querySelector("[data-role='workspaces']");
  const list = $("task-workspace-options");
  if (!wrap || !button || !list) return;
  const entries = workspaceCatalogEntries();
  syncTaskWorkspaceOptions(list, entries);
  const activeIds = entries.filter((workspace) => workspace.status === "active" && workspace.id).map((workspace) => workspace.id);
  const show = isAggregateView() && activeIds.length > 1;
  if (!show) {
    wrap.hidden = true;
    closeTaskWorkspaceFilter();
    return;
  }
  wrap.hidden = false;
  const selected = getSelectedWorkspaces();
  const narrowing = selected.length > 0 && !filterCoversActive(selected, activeIds);
  const checked = new Set(narrowing ? selected : activeIds);
  const label = button.querySelector(".chip-label");
  if (label) label.textContent = narrowing ? `${selected.length} workspace${selected.length === 1 ? "" : "s"}` : "Workspaces";
  button.classList.toggle("active", narrowing);
  button.setAttribute("aria-pressed", narrowing ? "true" : "false");
  button.setAttribute("aria-description", narrowing ? selected.map((workspaceId) => workspaceDisplayName(workspaceId)).join(", ") : "All workspaces");
  for (const option of list.children) {
    const on = checked.has(option.dataset.workspace);
    option.setAttribute("aria-selected", String(on));
    const input = option.querySelector("input");
    if (input) input.checked = on;
  }
}

function buildWorkspaceFilter(context) {
  workspaceFilterContext = context;
  const wrap = el("div", { class: "workspace-filter" });
  wrap.hidden = true;
  const button = el("button", {
    class: "chip",
    type: "button",
    "aria-haspopup": "listbox",
    "aria-expanded": "false",
    "aria-controls": "task-workspace-options",
    "aria-label": "Filter by workspace",
    "aria-description": "All workspaces",
  }, [el("span", { class: "chip-label", text: "Workspaces" })]);
  button.dataset.role = "workspaces";
  const list = el("div", {
    id: "task-workspace-options",
    class: "workspace-options",
    role: "listbox",
    "aria-multiselectable": "true",
    "aria-label": "Workspaces",
  });
  list.hidden = true;
  button.addEventListener("click", () => {
    if (list.hidden) {
      placeTaskWorkspaceList(button, list);
      button.setAttribute("aria-expanded", "true");
      list.querySelector('[aria-selected="true"]')?.focus();
    } else {
      closeTaskWorkspaceFilter();
    }
  });
  button.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (list.hidden) button.click();
    }
  });
  wrap.append(button, list);
  wrap.addEventListener("focusout", (event) => {
    if (!wrap.contains(event.relatedTarget)) closeTaskWorkspaceFilter();
  });
  return wrap;
}

onWorkspaceChange(() => paintWorkspaceFilter());
onWorkspaceNamesChange(() => paintWorkspaceFilter());

// ORB-10874: a compact, always-visible restatement of the filters currently
// narrowing the list — the chips already encode this, but not in a
// form a screen reader announces or a glance confirms without reading each
// control. Rendered next to the count so "why am I seeing this many" is
// answered in the same place as "how many is this".
export function renderFilterSummary(context) {
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
  if (myWorkOnly) parts.push(`my work (${myWorkCrewNames().join(", ") || "human crews"})`);
  if (waitingOnly) parts.push("waiting on you");
  if (isAggregateView()) {
    const workspaceIds = getSelectedWorkspaces();
    const activeIds = activeWorkspaceEntries().map((workspace) => workspace.id);
    if (workspaceIds.length && !filterCoversActive(workspaceIds, activeIds)) {
      parts.push(`workspaces: ${workspaceIds.map((id) => workspaceDisplayName(id)).join(", ")}`);
    }
  }
  node.textContent = parts.length > 0 ? `Filtering by ${parts.join(" · ")}` : "Showing all statuses";
}
