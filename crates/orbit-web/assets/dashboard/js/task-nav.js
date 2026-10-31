// Orbit dashboard task list navigation: row selection, the task page, pins, the hash, filters, search and pagination.

import { captureWorkspaceVisit, panelCanRender, raiseMessageSlot, el, fetchJson, postJson, isAggregateView, hostWriteRefusal, copyText } from './common.js';
import { $, taskState, rerender, taskList, searchQueryValue, activeStatusSet, setSearchQuery, setActiveStatuses, statusOrder, refreshTasks, defaultActiveStatuses, tasksMeta, aggregateRefusalTitle, STATUS_LABELS } from './task-shared.js';
import { forgetTaskDetailLoad } from './task-data.js';
import * as taskReadiness from './task-readiness.js';

let settleMergedInFlight = false;
// Advances whenever `taskState.openTaskId` changes, so a pin read for a task
// the hash no longer names is dropped.
let openTaskGeneration = 0;
let returningFromPage = false;
let searchDebounce = null;

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

export function filterTasks(tasks, context) {
  const q = searchQueryValue(context);
  const activeStatuses = activeStatusSet(context);
  return tasks.filter((t) => {
    if (!activeStatuses.has(t.status)) return false;
    if (notReadyOnly && !taskReadiness.isNotReady(t)) return false;
    if (pilotWarningsOnly && !taskReadiness.pilotWarnings(t).length) return false;
    if (!q) return true;
    return (
      (t.id && t.id.toLowerCase().includes(q)) ||
      (t.title && t.title.toLowerCase().includes(q))
    );
  });
}

export async function copyTaskIdWithNotice(taskId, context) {
  const copied = await copyText(taskId);
  taskState.taskActionNotice = `${taskId} is not in the filtered task list; ${copied ? "copied ID" : "could not copy the ID"}`;
  rerender(context);
}

function findTaskRow(taskId) {
  return Array.from(document.querySelectorAll("#tasks-body .row"))
    .find((row) => row.dataset.key === `task-${taskId}`) || null;
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
export function selectTask(taskId, context, { anchor = null } = {}) {
  const top = anchor && typeof anchor.getBoundingClientRect === "function" ? anchor.getBoundingClientRect().top : null;
  if (taskState.pinnedExternalTask && taskState.pinnedExternalTask.id !== taskId) taskState.pinnedExternalTask = null;
  taskState.selectedTaskId = taskId;
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
  if (forget) forgetTaskDetailLoad(taskId);
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
// a link never rewrites the saved status or search filter.
export function resolvePendingOpenTask(tasks, context) {
  const taskId = taskState.pendingOpenTaskId;
  if (!taskId) return;
  taskState.pendingOpenTaskId = null;
  if (filterTasks(tasks, context).some((task) => task.id === taskId)) {
    // A route replay for the row already selected leaves the list where it
    // is, and coming back from the full page restores the list's own scroll.
    const settled = taskState.selectedTaskId === taskId || (taskState.listRestore && taskState.listRestore.id === taskId);
    taskState.selectedTaskId = taskId;
    taskState.pinnedExternalTask = null;
    if (!settled) revealTaskRow(taskId, "center");
  } else if (!(taskState.pinnedExternalTask && taskState.pinnedExternalTask.id === taskId)) {
    pinOpenTask(taskId, context);
  }
}

async function pinOpenTask(taskId, context) {
  const visit = captureWorkspaceVisit();
  const generation = openTaskGeneration;
  const superseded = () => !visit.isCurrent() || taskState.openTaskId !== taskId || generation !== openTaskGeneration;
  try {
    const task = await fetchJson(`/api/tasks/${encodeURIComponent(taskId)}`);
    if (superseded()) return;
    if (!task || task.id !== taskId) throw new Error(`${taskId} not found`);
    taskState.pinnedExternalTask = { task, id: taskId };
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

/* Global ID resolver support (ORB-00211): allow rendering detail for a task that
   is outside the filtered list (hidden status, search, or truncated fetch)
   without mutating DASHBOARD_TASK_STATUSES. The pinned detail shows in the
   side pane, marked as outside the list; auto-clears once the filtered list
   actually contains the task. */
export function setPinnedExternalTask(task, context) {
  if (!task || !task.id) return;
  cancelTaskSearchDebounce();
  if (taskState.selectedTaskId && taskState.selectedTaskId !== task.id) taskState.selectedTaskId = null;
  taskState.pinnedExternalTask = { task, id: task.id };
  taskState.pendingOpenTaskId = null;
  setOpenTask(task.id);
  // The resolver has cleared the text query; retire its URL and page too.
  window.history?.replaceState?.(null, "", buildTasksHash(context));
  if (context?.resetTaskPagination) context.resetTaskPagination();
}

export function clearPinnedExternalTask(context) {
  if (taskState.pinnedExternalTask && taskState.openTaskId === taskState.pinnedExternalTask.id) setOpenTask(null);
  taskState.pinnedExternalTask = null;
  if (context) rerender(context);
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
  taskState.selectedTaskId = null;
  taskState.taskPageId = null;
  taskState.taskPageReturn = null;
  taskState.pendingOpenTaskId = null;
  taskState.pinnedExternalTask = null;
  taskState.taskActionNotice = null;
  setSearchQuery(context, "");
  setOpenTask(null);
  window.history?.replaceState?.(null, "", buildTasksHash(context));
  if (context?.resetTaskPagination) context.resetTaskPagination();
  syncTaskControls(context);
  rerender(context);
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

export function refreshChips(context) {
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
  if (taskState.taskPageId) return taskPageHash(taskState.taskPageId);
  return buildTasksListHash(context);
}

// `#tasks/<id>`, keeping the comment a link named while it is that task's.
function taskPageHash(taskId) {
  const target = taskState.commentTarget;
  const comment = target && target.taskId === taskId ? `?comment=${target.number}` : "";
  return `#tasks/${encodeURIComponent(taskId)}${comment}`;
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
  if (taskState.openTaskId) sp.set("open", taskState.openTaskId);
  const target = taskState.commentTarget;
  if (target && target.taskId === taskState.openTaskId) sp.set("comment", String(target.number));
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
  const open = pageId || parseOpenTaskId(query.get("open"));
  const commentNumber = open && /^[1-9]\d{0,5}$/.test(query.get("comment") || "") ? Number(query.get("comment")) : null;
  const replay = pageId
    ? current === `#tasks/${encodeURIComponent(pageId)}${commentNumber ? `?comment=${commentNumber}` : ""}`
    : query.toString() === (current.split("?")[1] || "");
  if (!replay && context?.cancelTaskLookup) context.cancelTaskLookup();
  if (taskState.openTaskId !== open) {
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
  // One page needs no pager. Its loading and failure text is in the Tasks
  // message area, so neither brings the pager back nor moves the list.
  const pager = previous.closest ? previous.closest(".task-pagination") : null;
  if (pager) pager.hidden = !state.canPrevious && !state.canNext;
  const text = state.error
    ? `Page failed: ${state.error}`
    : state.loading ? "Loading task page…" : "";
  if (text && text !== status.textContent) raiseMessageSlot(status);
  status.textContent = text;
  status.className = state.error ? "error" : "";
  if (previous.dataset.wired !== "true") {
    previous.dataset.wired = "true";
    previous.addEventListener("click", () => context.navigateTaskPage("previous"));
    next.addEventListener("click", () => context.navigateTaskPage("next"));
  }
}

// The page-level Settle merged PRs control: one confirmation settles every
// blocked, in-progress and review task in the selected workspace whose own
// delivery PR already merged. Disabled in aggregate and read-only views,
// like the per-task recovery controls.
export function syncSettleMergedButton(context) {
  const btn = $("tasks-settle-merged");
  if (!btn) return;
  const blocked = isAggregateView() || Boolean(hostWriteRefusal());
  btn.disabled = blocked || settleMergedInFlight;
  btn.textContent = settleMergedInFlight ? "Settling…" : "Settle merged PRs";
  btn.title = blocked
    ? aggregateRefusalTitle("Settle merged PRs", "settle merged tasks")
    : "Settle every blocked, in-progress and review task in this workspace whose delivery pull request already merged into its target branch; tasks whose pull request has not merged do not change";
  if (btn.dataset.wired !== "true") {
    btn.dataset.wired = "true";
    btn.addEventListener("click", () => {
      settleMergedPrs(context).catch((error) => console.error("Failed to settle merged PRs", error));
    });
  }
}

async function settleMergedPrs(context) {
  if (settleMergedInFlight || isAggregateView() || hostWriteRefusal()) return;
  if (typeof window.confirm !== "function" || !window.confirm(
    "Settle merged PRs? Only tasks in this workspace whose delivery pull request already merged into its target branch change: each moves to done with a landing note naming the pull request and merge commit. Tasks whose pull request has not merged, and tasks with a live run or claim, do not change and are reported as skipped.",
  )) return;
  const visit = captureWorkspaceVisit();
  settleMergedInFlight = true;
  syncSettleMergedButton(context);
  try {
    const result = await postJson(visit.path("/api/tasks/settle-merged"), {});
    if (!visit.isCurrent()) return;
    const settled = Array.isArray(result && result.settled) ? result.settled : [];
    const skipped = Array.isArray(result && result.skipped) ? result.skipped : [];
    const settledText = settled.map((entry) => `${entry.task_id} (#${entry.pr_number})`).join(", ");
    const skippedText = skipped.map((entry) => entry.task_id).join(", ");
    taskState.taskActionNotice = `Settled ${settled.length}${settledText ? `: ${settledText}` : ""}; skipped ${skipped.length}${skippedText ? `: ${skippedText}` : ""}`;
  } catch (error) {
    if (!visit.isCurrent()) return;
    const code = error && error.code ? error.code : "settle_merged_failed";
    taskState.taskActionNotice = `[${code}] ${error && error.message ? error.message : String(error)}`;
  } finally {
    settleMergedInFlight = false;
  }
  await refreshTasks(context);
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
    if (target && (target.isContentEditable || (target.closest && target.closest("input, textarea, select, dialog[open]")))) return;
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
  const q = searchQueryValue(context);
  if (q) parts.push(`search: "${q}"`);
  node.textContent = parts.length > 0 ? `Filtering by ${parts.join(" · ")}` : "Showing all statuses";
}
