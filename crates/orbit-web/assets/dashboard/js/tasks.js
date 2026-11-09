// Orbit dashboard task-domain rendering and actions.
// Pure vanilla JS, split into ES modules with no build step.

import { findRegisteredHost, getHost, getRegisteredHosts, getServingHost, getWorkspace, hostWriteRefusal, onWorkspaceChange, panelCanRender, raiseMessageSlot, el, syncNodes, makeRowDisclosure, enableRovingRows, makeCopyButton } from './common.js';
import { DRAIN_POOLS, backlogReadiness, drainReadinessFor, drainWaitSignature } from './drain-waits.js';
import { elapsedLabel, runStartedAt, taskLockFiles } from './task-runtime.js';
import { buildRiskRowPart, riskinessSignature } from './task-riskiness.js';
import { $, absTimeNode, rerender, taskState, setRenderer, taskList, tasksMeta, plural, isOsTag, taskDispatchIdentity, isSelectedTask } from './task-shared.js';
import { buildMutationFeedback, crewOptionsSignature, feedbackSignature, humanCrewFor } from './task-crew.js';
import { invalidateDistributedConsole } from './distributed.js';
import { recoveryRefusals } from './task-agent-actions.js';
export { normalizeCrewPayload, cacheCrewPayload, hasCrewOptions } from './task-crew.js';
import * as taskReadiness from './task-readiness.js';
import * as taskComments from './task-comments.js';
export { buildArtifacts } from './task-evidence.js';
export { scrollToComment } from './task-comments.js';
import { clearTaskDataCaches } from './task-data.js';
import { closeTaskPane, filterTasks, focusTaskRow, refreshChips, renderFilterSummary, renderTaskPagination, resolvePendingOpenTask, revealTaskRow, selectTask, setOpenTask, stopRowInteraction } from './task-nav.js';
export { taskPilotWarningsFilter, copyTaskIdWithNotice, openVisibleTask, refreshPinnedTask, setPinnedExternalTask, clearPinnedExternalTask, cancelTaskSearchDebounce, clearTaskSearch, buildTasksHash, applyTasksHashQuery, syncTaskControls, renderTaskPagination, refreshChips, buildChips, wireSearch } from './task-nav.js';
import { buildQuickAction, buildQuickActionError, hasQuickAction, pruneQuickActionErrors, quickActionSignature, quickActionState } from './task-quick-actions.js';
import { applyTaskStatusChange, buildCrewUpdateControl } from './task-status-controls.js';
import { followRovingSelection, renderTaskPane } from './task-pane.js';
import { taskRunLoads } from './task-detail.js';
export { wireTaskPane } from './task-pane.js';

// Task modules repaint the list through task-shared's `rerender`.
setRenderer(renderTasks);
onWorkspaceChange(() => {
  invalidateDistributedConsole();
  taskState.pinnedExternalTask = null;
  taskState.taskActionNotice = null;
  quickActionState.clear();
  taskState.selectedTaskId = null;
  taskState.selectedTaskWorkspace = null;
  taskRunLoads.clear();
  // The initial workspace selection also lands here; a task the URL named but
  // no render has shown yet must survive it.
  if (!taskState.pendingOpenTaskId && taskState.openTaskId) {
    taskState.taskPageId = null;
    taskState.taskPageReturn = null;
    setOpenTask(null);
  }
  taskState.statusFeedback.clear();
  taskState.foldedStatusTasks.clear();
  // The list empties with the workspace, and so do its notices beside it.
  syncTaskNotices([]);
  taskState.crewFeedback.clear();
  taskState.complexityFeedback.clear();
  taskState.fieldFeedback.clear();
  clearTaskDataCaches();
  recoveryRefusals.clear();
  taskState.expandedComments.clear();
  taskComments.rawComments.clear();
  taskState.restoredCommentHash = null;
});
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

// How an empty open group reads in the "Nothing …" line above the cards.
const EMPTY_GROUP_PHRASES = {
  blocked: "blocked",
  "in-progress": "in progress",
  review: "in review",
  proposed: "proposed",
  backlog: "in the backlog",
};

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
// The task row's OS chips, from the parsed requirement the server attaches;
// a tag outside the namespace reads as the wait it causes.
function buildScheduledChip(task, context) {
  if (task.status !== "backlog" || !task.not_before || task.scheduled_hold === false || Date.parse(task.not_before) <= Date.now()) return null;
  return el("span", { class: "scheduled-chip", title: "Waiting for the scheduled time; starts on the next admission pass afterwards" }, [
    "Scheduled ", absTimeNode(context, task.not_before),
  ]);
}

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
// `[source] Rest of the title`: the bracketed source prefixes become tag chips
// beside the title (`[auto-task]` reads as an "auto-task" chip), so the title
// starts with the problem it names.
function splitTitlePrefix(title) {
  const match = /^((?:\[[^\]]+\]\s*)+)(.*)$/.exec(String(title || ""));
  if (!match || !match[2]) return [[], String(title || "")];
  const labels = [...match[1].matchAll(/\[([^\]]+)\]/g)].map((part) => part[1].trim()).filter(Boolean);
  // `[resident]: title` keeps its separator after the chip; drop it.
  const rest = match[2].replace(/^[\s:]+/, "");
  if (!rest) return [[], String(title || "")];
  return [labels, rest];
}

// Where a running or blocked task's run is and what it holds: the host, the
// time since the run started (known for runs this host records), and the files
// the task has locked, each file in the tooltip.
//
// A machine id is the execution identity. The display name is optional metadata.
// A linked run with no machine id is unknown. The host selected in the dashboard
// is not a substitute: historical links and machine-id-only records used to
// render as that host.
function executionIdentity(machine) {
  const machineId = machine && typeof machine.machine_id === "string" ? machine.machine_id.trim() : "";
  if (!machineId) return { known: false, machine_id: "", machine_name: "" };
  const machineName = typeof machine.machine_name === "string" ? machine.machine_name.trim() : "";
  return { known: true, machine_id: machineId, machine_name: machineName };
}

function selectedExecutionHost() {
  const selectedName = getHost() || getServingHost()?.name || "";
  const row = selectedName
    ? findRegisteredHost(selectedName)
    : getRegisteredHosts().find((host) => host.local);
  return { name: row?.name || selectedName, machine_id: row?.machine_id || "" };
}

function runCellParts(task) {
  const identity = executionIdentity(task.job_run_machine);
  let host = "";
  let provenance = "none";
  if (identity.known && identity.machine_name) {
    host = identity.machine_name;
    provenance = "named";
  } else if (identity.known) {
    host = identity.machine_id;
    provenance = "machine_id";
  } else if (task.job_run_id) {
    host = "unknown";
    provenance = "unknown";
  }
  const selected = selectedExecutionHost();
  const sameMachine = identity.known && (
    (selected.machine_id && identity.machine_id === selected.machine_id)
    || (identity.machine_name && selected.name && identity.machine_name === selected.name)
  );
  const started = task.status === "in-progress" ? runStartedAt(task.job_run_id) : null;
  const files = taskLockFiles(task.id);
  return {
    host,
    provenance,
    remote: identity.known && !sameMachine,
    elapsed: started != null ? elapsedLabel(Date.now() - started) : "",
    files,
  };
}

function runLocationLead(host, provenance) {
  if (provenance === "named") return `Run on ${host}`;
  if (provenance === "machine_id") return `Execution machine id: ${host}`;
  if (provenance === "unknown") return "Execution location unknown";
  return "No run recorded";
}

function buildRunCell(task) {
  const { host, provenance, elapsed, files } = runCellParts(task);
  const sub = [elapsed, files.length ? plural(files.length, "file") : ""].filter(Boolean);
  const title = [
    runLocationLead(host, provenance),
    elapsed ? `running for ${elapsed}` : "",
    ...files,
  ].filter(Boolean).join("\n");
  return el("span", { class: "run-cell", title }, [
    el("span", { class: `run-host${host ? "" : " none"}`, text: host || "no run" }),
    ...sub.map((text) => el("span", { class: "run-sub", text })),
  ]);
}

function runCellSignature(task) {
  const { provenance, host, elapsed, files } = runCellParts(task);
  return JSON.stringify([provenance, host, elapsed, files]);
}
/* ORB-10444: one-click Ship. The dispatch carries only the task id — the
   pipeline resolves the crew from the task's own record and the mode from the
   workspace's configured default — so there is deliberately no crew picker and
   no PR/local toggle here. The resulting run id (or the server's error) is
   surfaced so the operator can see the click took effect. */

/* ORB-10444: human comments on a task. The write goes to the task's existing
   review-thread structure via POST /api/tasks/<id>/comments, which records a
   human identity rather than the server process's ambient one.

   ORB-12645: the draft is Markdown and the thread renders it, so the composer
   says so — in the placeholder, in a hint line naming the marks that carry,
   and in a preview that runs the draft through the same renderMarkdown() the
   posted comment will go through. The write itself is unchanged. */
// The feedback of a status change whose detail folded in, with its undo. Null
// while the detail is open again (it shows the feedback itself) or once the
// feedback has expired.
function buildFoldedStatusFeedback(task, context) {
  const taskId = taskDispatchIdentity(task);
  if (!taskState.foldedStatusTasks.has(taskId) || isSelectedTask(task)) return null;
  const entry = taskState.statusFeedback.get(taskId);
  return buildMutationFeedback(entry, () => {
    if (!entry || !entry.undo) return;
    const current = taskList(context).find((t) => t && taskDispatchIdentity(t) === taskId) || taskState.foldedStatusTasks.get(taskId);
    applyTaskStatusChange(current, entry.undo.previousValue, context);
  });
}

function foldedStatusSignature(task) {
  const key = taskDispatchIdentity(task);
  return taskState.foldedStatusTasks.has(key) && !isSelectedTask(task) ? feedbackSignature(taskState.statusFeedback, key) : "";
}

// Folded status feedback for a task with no row on screen, usually because its
// new status is filtered out, so the undo stays reachable in the Tasks message
// area. A pinned detail shows its own.
function buildFoldedStatusNotices(rowIds, context) {
  const notices = [];
  for (const [taskId, task] of taskState.foldedStatusTasks) {
    if (rowIds.has(taskId) || (taskState.pinnedExternalTask && taskDispatchIdentity(taskState.pinnedExternalTask.task) === taskId)) continue;
    const feedback = buildFoldedStatusFeedback(task, context);
    if (!feedback) continue;
    const notice = el("div", { class: "task-status-notice" }, [
      el("span", { class: "mono", text: `${task.id} → ${task.status}` }),
      feedback,
    ]);
    notice.dataset.key = `status-notice-${taskId}`;
    notice.dataset.hash = `${task.status}-${foldedStatusSignature(task)}`;
    notices.push(notice);
  }
  return notices;
}

// Action and folded status notices sit in the Tasks message area beside the
// list, not above its first card, so one coming or going moves no row. A new
// one brings the notices to the top of the area.
function syncTaskNotices(notices) {
  const host = $("tasks-notices");
  if (!host) return;
  const before = Array.from(host.children, node => `${node.dataset.key}:${node.dataset.hash}`);
  syncNodes(host, notices);
  if (notices.some(node => !before.includes(`${node.dataset.key}:${node.dataset.hash}`))) raiseMessageSlot(host);
}

// An action notice stays up for a minimum read, like the quick-action
// errors (180 s) and mutation feedback beside it: it is rebuilt from the
// persisting state on every render instead of being consumed by the first
// one, and a timer retires it once its window lapses. A newer notice resets
// the window.
const TASK_ACTION_NOTICE_MS = 30_000;
let actionNoticeShownFor = null;
let actionNoticeTimer = null;

function takeTaskActionNotice(context) {
  const text = taskState.taskActionNotice;
  if (!text) {
    actionNoticeShownFor = null;
    if (actionNoticeTimer) { clearTimeout(actionNoticeTimer); actionNoticeTimer = null; }
    return null;
  }
  const notice = el("div", { class: "task-action-notice", text });
  notice.dataset.key = "task-action-notice";
  notice.dataset.hash = text;
  notice.setAttribute("role", "status");
  notice.setAttribute("aria-live", "polite");
  if (actionNoticeShownFor !== text) {
    actionNoticeShownFor = text;
    if (actionNoticeTimer) clearTimeout(actionNoticeTimer);
    actionNoticeTimer = setTimeout(() => {
      actionNoticeTimer = null;
      if (taskState.taskActionNotice === text) {
        taskState.taskActionNotice = null;
        actionNoticeShownFor = null;
        rerender(context);
      }
    }, TASK_ACTION_NOTICE_MS);
  }
  return notice;
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

// The Backlog card's readiness strip: one slim bar split by pool and a legend
// whose entries filter the card to that pool. It is absent until the drain
// snapshot arrives, so the card never claims readiness it does not know.
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
        rerender(context);
      });
      return button;
    }),
    counts.unknown > 0 ? el("span", { class: "ready-unknown", text: `${counts.unknown} not in the drain snapshot` }) : null,
  ]);
  const strip = el("div", { class: "group-readiness" }, [bar, legend]);
  strip.dataset.key = "readiness-backlog";
  strip.dataset.hash = `${DRAIN_POOLS.map(({ key }) => counts[key]).join(",")},${counts.unknown}-${readyPoolFilter || ""}`;
  return strip;
}

// Operator work: the task's crew is a human crew, so it waits for that person
// and no drain or task pilot picks it up.
function buildHumanCrewBadge(task) {
  const human = humanCrewFor(task);
  if (!human) return null;
  return el("span", {
    class: "human-crew-badge",
    text: `for ${human.name}`,
    title: `Operator work for human crew ${human.name}: no drain or task pilot picks it up`,
  });
}

// A card whose rows the readiness filter hid entirely still says so.
function buildFilteredEmpty(status) {
  const note = el("div", { class: "group-empty", text: `No ${(GROUP_LABELS[status] || status).toLowerCase()} tasks match this readiness filter.` });
  note.dataset.key = `empty-${status}`;
  note.dataset.hash = `empty-${status}`;
  return note;
}
export function renderTasks(tasks, context) {
  if (!panelCanRender("tasks-body")) return;
  const body = $("tasks-body");
  if (!body) return;
  taskState.paneContext = context;
  // Expired row-action errors clear on the next render (the regular refresh
  // included), so the row recovers its layout without operator action.
  pruneQuickActionErrors();
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
  if (taskState.pinnedExternalTask && taskState.pinnedExternalTask.task) {
    const p = taskState.pinnedExternalTask.task;
    if (filterTasks(tasks, context).some((task) => taskDispatchIdentity(task) === taskDispatchIdentity(p))) {
      taskState.pinnedExternalTask = null;
      if (taskState.openTaskId === p.id) {
        taskState.selectedTaskId = p.id;
        taskState.selectedTaskWorkspace = p.workspace_id || getWorkspace() || null;
      }
    }
  }
  resolvePendingOpenTask(tasks, context);
  for (const taskId of taskState.foldedStatusTasks.keys()) {
    if (!taskState.statusFeedback.has(taskId)) taskState.foldedStatusTasks.delete(taskId);
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
  const notice = takeTaskActionNotice(context);

  const filtered = filterTasks(tasks, context);
  // Rows keep a fixed-width action column so actions line up between groups;
  // when no visible row has an action (a Done-only view) the whole column is
  // dead space, so the list gives it back as one.
  const quickActions = filtered.some(hasQuickAction) ? "some" : "none";
  if (body.dataset.quickActions !== quickActions) body.dataset.quickActions = quickActions;
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
    syncTaskNotices([notice, ...buildFoldedStatusNotices(new Set(), context)].filter(Boolean));
    syncNodes(body, [emptyState]);
    renderTaskPane(filtered, context);
    finishTaskRender(body);
    return;
  }
  const groups = new Map();
  const rowIds = new Set();

  for (const t of filtered) {
    if (!groups.has(t.status)) groups.set(t.status, []);
    groups.get(t.status).push(t);
  }
  const ordered = GROUP_ORDER.filter((s) => groups.has(s)).concat(
    [...groups.keys()].filter((s) => !GROUP_ORDER.includes(s)),
  );
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
      if (readyPoolFilter && strip) rows = group.filter((t) => drainReadinessFor(t)?.pool === readyPoolFilter);
    }
    for (const t of rows) {
      const rowKey = `task-${taskDispatchIdentity(t)}`;
      const run = RUN_GROUPS.has(t.status);
      // Basic hash based on row presentation parameters (selection is painted
      // apart, by syncRowSelection)
      const rowHash = `${JSON.stringify(t.pilot_warnings || [])}-${JSON.stringify(t.active_triage || [])}-${JSON.stringify(t.delivery_attention || null)}-${t.id}-${t.title}-${t.status}-${JSON.stringify(t.os_requirement || null)}-${(t.tags || []).filter(isOsTag).join(",")}-${t.not_before || ""}-${t.scheduled_hold}-${Boolean(t.not_before && Date.parse(t.not_before) > Date.now())}-${taskReadiness.readinessSignature(t)}-${t.status === "backlog" ? drainWaitSignature(t) : ""}-${run ? runCellSignature(t) : ""}-${t.crew || ""}-${t.resolved_crew || ""}-${t.workspace_id || ""}-${t.workspace_name || ""}-${aggregate}-${JSON.stringify(t.human_crew || null)}-${crewOptionsSignature()}-${feedbackSignature(taskState.crewFeedback, taskDispatchIdentity(t))}-${quickActionSignature(t)}-${foldedStatusSignature(t)}-${riskinessSignature(t)}-${hostWriteRefusal()}`;
      const existingRow = existingRowNodes.get(rowKey);
      let row = existingRow && existingRow.dataset.hash === rowHash ? existingRow : null;
      if (!row) {
        // Two lines beside the crew and the action: the title with its tag
        // chips, then the ID and what the task is waiting on or where its run
        // is. The ID line comes first in the DOM so Tab goes title → crew →
        // action; the grid draws it under the title.
        const meta = el("span", { class: "row-meta" }, [
          makeCopyButton(t.id, { class: "id mono", title: "Copy task ID" }),
          t.status === "backlog" ? taskReadiness.buildRowReason(t) : null,
          run ? buildRunCell(t) : null,
          buildRiskRowPart(t),
        ]);
        const [prefixes, rest] = splitTitlePrefix(t.title);
        const tags = [
          aggregate && t.workspace_name
            ? el("span", { class: "ws-badge mono", text: t.workspace_name, title: `Workspace: ${t.workspace_name}` })
            : null,
          ...buildOsBadges(t),
          buildHumanCrewBadge(t),
          buildScheduledChip(t, context),
          ...taskReadiness.buildReadinessBadge(t),
          ...taskReadiness.buildPilotWarningBadge(t),
          ...taskReadiness.buildActiveTriageBadges(t),
          ...taskReadiness.buildDeliveryAttentionBadge(t),
          ...prefixes.map((label) => el("span", { class: "title-tag", text: label, title: `[${label}]` })),
        ].filter(Boolean);
        // The title is the row's disclosure: the row holds the copy-id button,
        // the crew select and the quick action, so it cannot be a button itself.
        const titleCell = el("button", { class: "title" }, [rest]);
        const titleLine = el("span", { class: "title-line" }, [
          titleCell,
          tags.length ? el("span", { class: "title-tags" }, tags) : null,
        ]);
        const quickError = buildQuickActionError(t, context);
        const statusFeedbackNode = buildFoldedStatusFeedback(t, context);
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
        row.dataset.taskId = t.id;
        row.dataset.workspace = t.workspace_id || getWorkspace() || "";
        row.dataset.hash = rowHash;
        // Selection is painted after the sync (syncRowSelection), so moving it
        // rebuilds no row and the focused one keeps its node.
        makeRowDisclosure(row, titleCell, {
          expanded: false,
          controls: "task-detail-pane",
          onToggle: () => {
            if (isSelectedTask(t) && !taskState.pinnedExternalTask) closeTaskPane(context, { forget: true });
            else selectTask(t.id, context, { anchor: row, workspace: row.dataset.workspace });
          },
        });
      }
      groupNodes.push(row);
      rowIds.add(taskDispatchIdentity(t));
    }
    if (rows.length === 0) groupNodes.push(buildFilteredEmpty(status));
    // A card whose rows carry no action (Blocked) gives the action column
    // back, so its crew sits at the card's right edge.
    const noQuick = !group.some(hasQuickAction);
    // Every write is conditional, so a reused row is left untouched.
    groupNodes.forEach((node, index) => {
      if (node.dataset.group !== status) node.dataset.group = status;
      node.classList.toggle("no-quick", noQuick);
      node.classList.toggle("group-end", index === groupNodes.length - 1);
    });
    nodes.push(...groupNodes);
  }
  // Built once the rows are known: a folded notice is only for a task with no row.
  syncTaskNotices([notice, ...buildFoldedStatusNotices(rowIds, context)].filter(Boolean));
  syncNodes(body, nodes);
  syncRowSelection(body);
  renderTaskPane(filtered, context);
  finishTaskRender(body);
  const commentHash = taskComments.getCommentHash();
  if (commentHash.startsWith("#comment-")) {
    if (taskState.restoredCommentHash !== commentHash) {
      if (taskComments.scrollToComment(commentHash)) {
        taskState.restoredCommentHash = commentHash;
      }
    }
  } else {
    taskState.restoredCommentHash = null;
  }
}
// The selected row reads as selected without being rebuilt: its class and
// its disclosure state are set on the node the list already holds.
function syncRowSelection(body) {
  const selected = !taskState.pinnedExternalTask && taskState.selectedTaskId
    ? `task-${taskState.selectedTaskWorkspace || getWorkspace() || ""}:${taskState.selectedTaskId}` : null;
  for (const row of body.querySelectorAll(".row")) {
    if (!row.dataset.key || !row.dataset.key.startsWith("task-")) continue;
    const on = row.dataset.key === selected;
    row.classList.toggle("expanded", on);
    const button = row.querySelector("[data-row-focus]");
    if (button && button.getAttribute("aria-expanded") !== String(on)) button.setAttribute("aria-expanded", String(on));
  }
}

// Work a render leaves for after both the list and the pane are in place:
// the list scroll a full page saved, and focus a closed pane hands back.
function finishTaskRender(body) {
  const active = document.activeElement;
  const pane = $("task-detail-pane");
  const focusLost = !active || active === document.body;
  if (taskState.listRestore && !taskState.taskPageId) {
    const restore = taskState.listRestore;
    taskState.listRestore = null;
    if (restore.scrollTop != null) body.scrollTop = restore.scrollTop;
    else revealTaskRow(restore.id, "center");
    if (focusLost || (pane && pane.contains && pane.contains(active))) focusTaskRow(restore.id);
  }
  if (taskState.rowFocusAfterRender) {
    const taskId = taskState.rowFocusAfterRender;
    taskState.rowFocusAfterRender = null;
    if (focusLost) focusTaskRow(taskId);
  }
}
