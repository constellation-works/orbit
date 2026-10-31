// Orbit dashboard task pane: the pane render, its head, the resizer and the
// keyboard navigation around it.

import { el, makeCopyButton, syncNodes } from './common.js';
import { $, taskState } from './task-shared.js';
import * as taskShared from './task-shared.js';
import { closeTaskPane, focusTaskRow, openTaskPage, revealTaskRow, selectTask, stopRowInteraction } from './task-nav.js';
import { taskDetailState, taskDrainSignature, taskDrainState } from './task-data.js';
import { buildTaskDetail, buildTaskDetailPlaceholder, detailFeedbackSignature } from './task-detail.js';
import { osReadoutNote, osReadoutText } from './task-lifecycle.js';
import { buildCrewUpdateControl } from './task-status-controls.js';
import { fixPrSignature, fixPrState, pilotSignature, recoverySignature, refreshFixPrControls, taskPullRequestNumber, triageSignature } from './task-agent-actions.js';
import { crewOptionsSignature, feedbackSignature } from './task-crew.js';
import { buildRelationRow, drainDependencyIds, relationTargets } from './task-relations.js';
import * as taskReadiness from './task-readiness.js';
import * as taskEvidence from './task-evidence.js';
import * as taskComments from './task-comments.js';
import * as taskFields from './task-fields.js';

// A detail kept for an open draft can refresh its read-only readiness and
// selector displays without touching the editor holding that draft.
function refreshDraftDetails(wrap, task, context, readinessTask = task) {
  for (const readout of wrap.querySelectorAll(".task-os-readout")) {
    readout.textContent = osReadoutText(task);
    const note = readout.parentNode && readout.parentNode.querySelector(".info-hint-text");
    if (note) note.textContent = osReadoutNote(task);
  }
  refreshDraftCrewControl(wrap, task, context);
  refreshFixPrControls(wrap, task, context);
  taskFields.refreshFieldCells(wrap, readinessTask);
  taskReadiness.refreshOperatorRecordFeedback(wrap, task);
  if (Array.isArray(readinessTask.context_files)) {
    const count = wrap.querySelector(`#detail-${task.id}-tab-context_files .detail-tab-count`);
    if (count) count.textContent = String(readinessTask.context_files.length);
    for (const ul of wrap.querySelectorAll(".file-list")) {
      if (!ul.contentionTask) continue;
      ul.contentionTask = readinessTask;
      taskFields.refreshFileList(ul);
    }
  }
  const current = wrap.querySelector(".readiness-list");
  const drain = taskDrainState(task, context);
  if (!current) return;
  if (!drain) return;
  const signature = taskDrainSignature(drain);
  if (current.dataset.drainSignature !== signature) {
    const replacement = taskReadiness.buildReadinessBlock(taskReadiness.readinessOf(readinessTask), drain, readinessTask.workspace_id);
    replacement.dataset.drainSignature = signature;
    const parent = current.parentNode;
    if (!parent) return;
    parent.insertBefore(replacement, current);
    parent.removeChild(current);
  }
  refreshBlockedByRow(wrap, readinessTask, drain, context);
}

// The drain read lands after the detail is shown, so the Blocked by row is
// derived from that same entry: covered ids stay hidden, uncovered ones keep
// their row, and the row returns when the entry stops covering them.
function refreshBlockedByRow(wrap, task, drain, context) {
  const card = wrap.querySelector ? wrap.querySelector(".task-delivery-card") : null;
  if (!card) return;
  const relations = relationTargets(Array.isArray(task.relations) ? task.relations : []);
  const blocked = relations.find(({ key }) => key === "blocked_by");
  // Only ids the rendered Readiness block actually displays are covered; a
  // retained detail without one keeps every Blocked by link.
  const displayed = new Set(Array.from(wrap.querySelectorAll(".readiness-list .readiness-dependency a"))
    .map((link) => link.textContent));
  const covered = new Set([...drainDependencyIds(drain)].filter((id) => displayed.has(id)));
  const visible = blocked ? blocked.targets.filter((target) => !covered.has(target)) : [];
  const existing = Array.from(card.children).filter((child) =>
    child.classList && child.classList.contains("relation") && child.dataset.relation === "blocked_by");
  const shown = existing.flatMap((child) =>
    Array.from(child.querySelectorAll(".task-delivery-relation")).map((button) => button.textContent));
  if (shown.length === visible.length && shown.every((target, index) => target === visible[index])) return;
  for (const child of existing) child.remove();
  if (!visible.length) return;
  const row = buildRelationRow("blocked_by", blocked.label, visible, context);
  // Blocked by leads the relation rows: before any other relation row, else
  // before Readiness, else before Location, else at the card's end.
  const anchor = Array.from(card.children).find((child) =>
    (child.classList && child.classList.contains("relation")) ||
    (child.classList && (child.classList.contains("readiness") || child.classList.contains("location"))));
  if (anchor) card.insertBefore(row, anchor);
  else card.appendChild(row);
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
  row.dataset.hash = `${ptask.id}-${ptask.title}-${ptask.status}-${ptask.crew || ""}-${ptask.resolved_crew || ""}-${crewOptionsSignature()}-${feedbackSignature(taskState.crewFeedback, ptask.id)}`;

  const drain = taskDrainState(ptask, context);
  const detail = buildTaskDetail(ptask, context, drain);
  const readiness = detail.querySelector(".readiness-list");
  if (readiness) readiness.dataset.drainSignature = taskDrainSignature(drain);

  const wrap = el("div", { class: "pinned-task-wrap" }, [row, detail]);
  wrap.teardown = () => taskComments.disconnectCommentCards(wrap);
  wrap.dataset.key = `pinned-${ptask.id}`;
  wrap.dataset.hash = `${row.dataset.hash}-${JSON.stringify(ptask)}-${taskDrainSignature(drain)}-${detailFeedbackSignature(ptask.id)}-${fixPrSignature(ptask)}-${taskEvidence.decisionSignature(ptask)}-${taskReadiness.operatorRecordSignature(ptask)}`;
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
// The pane beside the list: a head naming the task with its close, back and
// full-page controls, then the task's detail. A pinned resolver result shows
// here too, marked as outside the list.
export function renderTaskPane(filtered, context) {
  const pane = $("task-detail-pane");
  const paneBody = $("task-detail-body");
  if (!pane || !paneBody) return;
  const ptask = taskState.pinnedExternalTask && taskState.pinnedExternalTask.task ? taskState.pinnedExternalTask.task : null;
  const listTask = !ptask && taskState.selectedTaskId ? filtered.find((task) => task.id === taskState.selectedTaskId) || null : null;
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
  } else if (taskState.taskPageId) {
    nodes.push(buildTaskPagePlaceholder(context));
  }
  syncNodes(paneBody, nodes);
  const open = nodes.length > 0;
  pane.hidden = !open;
  const main = $("tasks-main");
  if (main) {
    main.classList.toggle("task-pane-open", open);
    main.classList.toggle("task-page-mode", open && Boolean(taskState.taskPageId));
  }
  syncPaneChrome(open);
  if (taskState.focusPaneTitle && open) {
    taskState.focusPaneTitle = false;
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
  const baseHash = `${JSON.stringify(state.task || t)}-${readState}-${crewOptionsSignature()}-${detailFeedbackSignature(t.id)}-${taskState.shipInFlightTaskIds.has(taskShared.taskDispatchIdentity(t))}-${pilotSignature(t)}-${triageSignature(t)}-${recoverySignature(t)}-${taskReadiness.operatorRecordSignature(t)}-${fixPrSignature(t)}-${taskEvidence.decisionSignature(t)}`;
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
  const page = taskState.taskPageId === task.id;
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
  const id = makeCopyButton(taskState.taskPageId, { class: "id mono", title: "Copy task ID" });
  const note = el("p", {
    class: taskState.taskPageError ? "panel-placeholder action-error" : "panel-placeholder",
    text: taskState.taskPageError || `Loading ${taskState.taskPageId}…`,
    role: "status",
    "aria-live": "polite",
  });
  const title = el("h2", { class: "task-pane-title", id: "task-pane-title", text: taskState.taskPageId });
  title.tabIndex = -1;
  const bar = el("div", { class: "task-pane-bar" }, [id, el("span", { class: "task-pane-spacer" }), back]);
  const node = el("div", { class: "task-pane-head" }, [bar, title, note]);
  node.dataset.key = `pane-page-${taskState.taskPageId}`;
  node.dataset.hash = taskState.taskPageError || "loading";
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
export function followRovingSelection(button) {
  if (!taskState.selectedTaskId || taskState.pinnedExternalTask || taskState.taskPageId || !taskState.paneContext) return;
  const taskId = taskIdOfRow(button.closest ? button.closest(".row") : null);
  if (taskId && taskId !== taskState.selectedTaskId) selectTask(taskId, taskState.paneContext);
}

// j/k from inside the pane: select the next or previous row and keep the
// keyboard in the pane, on the new task's heading.
function moveTaskSelection(step) {
  const body = $("tasks-body");
  if (!body || !taskState.paneContext) return false;
  const buttons = Array.from(body.querySelectorAll("[data-row-focus]"))
    .filter((button) => typeof button.getClientRects !== "function" || button.getClientRects().length > 0);
  const at = buttons.findIndex((button) => taskIdOfRow(button.closest(".row")) === taskState.selectedTaskId);
  const next = at < 0 ? buttons[0] : buttons[at + step];
  const taskId = next ? taskIdOfRow(next.closest(".row")) : null;
  if (!taskId) return false;
  selectTask(taskId, taskState.paneContext);
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
  taskState.paneContext = taskState.paneContext || context;
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
    if (!taskId || taskId !== taskState.selectedTaskId || taskState.pinnedExternalTask) return;
    event.preventDefault();
    openTaskPage(taskId, taskState.paneContext || context);
  });
  pane.addEventListener("keydown", (event) => {
    if ((event.key !== "j" && event.key !== "k") || event.altKey || event.ctrlKey || event.metaKey) return;
    if (taskState.taskPageId || taskState.pinnedExternalTask || isEditableTarget(event.target)) return;
    if (moveTaskSelection(event.key === "j" ? 1 : -1)) event.preventDefault();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    if (pane.hidden || pane.getClientRects().length === 0 || isEditableTarget(event.target)) return;
    // The composer and other dialogs of the page keep their own Esc.
    if (event.target && event.target.closest && event.target.closest("[role=dialog]:not(#task-detail-pane), #task-create-panel")) return;
    event.preventDefault();
    closeTaskPane(taskState.paneContext || context, { forget: true });
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
