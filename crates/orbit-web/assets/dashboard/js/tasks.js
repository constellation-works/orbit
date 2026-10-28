// Orbit dashboard task-domain rendering and actions.
// Pure vanilla JS, split into ES modules with no build step.

import { captureWorkspaceVisit, findRegisteredHost, getHost, getRegisteredHosts, getServingHost, getWorkspace, onWorkspaceChange, panelCanRender, el, infoHint, fetchJson, patchJson, postJson, syncNodes, isAggregateView, hostWriteRefusal, makeToggleRow, makeDisclosure, makeRowDisclosure, enableRovingRows, makeCopyButton, runHref, submissionNoticeLines, stateCell, fmtDuration, elapsedDurationInfo } from './common.js';
import { renderMarkdown } from './markdown.js';
import { DRAIN_POOLS, backlogReadiness, drainReadinessFor, drainWaitSignature } from './drain-waits.js';
import { elapsedLabel, runStartedAt, taskLockFiles } from './task-runtime.js';
import { buildRiskChip, buildRiskDetail, buildRiskRowPart, riskinessSignature } from './task-riskiness.js';
import { buildAttentionCallout, buildDeliveryPanel, buildDeliveryProgress, buildExecutionProvenance, formatExecutionLocation, claimAttention, claimedReviewApproval, deliverySteps, handoffApprovalRequest, invalidateDistributedConsole, lastKnownClaims, registeredMachineHost } from './distributed.js';
import { $, taskState, setRenderer, rerender, taskList, statusOrder, statusTransitions, statusTransition, fmtAbsTimeValue, absTimeNode, refreshTasks, tasksMeta, canMutateTask, aggregateRefusalTitle, taskWorkspacePath, taskMutationPath, GITHUB_PR_REF_SYSTEM, pullRequestLink, actionErrorNode, plural, MUTATION_UNDO_WINDOW_MS, isOsTag, STATUS_LABELS } from './task-shared.js';
import { scheduleFeedbackExpiry, buildMutationFeedback, crewOptionsSignature, feedbackSignature, explicitCrewValue, hasStaleExplicitCrew, resolvedCrewName, defaultCrewOptionText, crewOptionTitle } from './task-crew.js';
export { normalizeCrewPayload, cacheCrewPayload, hasCrewOptions } from './task-crew.js';
import * as taskShared from './task-shared.js';
import * as taskHistory from './task-history.js';
import * as taskReadiness from './task-readiness.js';
import * as taskEvidence from './task-evidence.js';
import * as taskComments from './task-comments.js';
import * as taskFields from './task-fields.js';
import * as taskWhyStuck from './task-why-stuck.js';
export { buildArtifacts } from './task-evidence.js';
export { scrollToComment } from './task-comments.js';
import { applyUpdatedTask, clearTaskDataCaches, currentCrewTask, forgetTaskDetailLoad, loadTaskDetail, mergeTaskRecord, taskDetailState, taskDrainSignature, taskDrainState } from './task-data.js';
import { closeTaskPane, collapseTask, filterTasks, focusTaskRow, openTaskPage, refreshChips, renderFilterSummary, renderTaskPagination, resolvePendingOpenTask, revealTaskRow, selectTask, setOpenTask, stopRowInteraction, syncSettleMergedButton } from './task-nav.js';
export { taskPilotWarningsFilter, copyTaskIdWithNotice, openVisibleTask, setPinnedExternalTask, clearPinnedExternalTask, cancelTaskSearchDebounce, clearTaskSearch, buildTasksHash, applyTasksHashQuery, syncTaskControls, renderTaskPagination, buildChips, wireSearch } from './task-nav.js';
import { buildExternalRefs, buildRelationRow, drainDependencyIds, relationTargets } from './task-relations.js';

// The pane's tab is a page-wide choice: moving the selection keeps it.
let activeDetailTab = "overview";
// The Runs tab's read per task (`GET /api/job-runs?task_id=`), kept so a
// rebuilt detail repaints the last answer while a fresh read is in flight.
let taskRunLoads = new Map();
const TASK_RUNS_LIMIT = 20;
const TASK_RUNS_FRESH_MS = 15000;
const reviewApprovalsInFlight = new Set();

// Task modules repaint the list through task-shared's `rerender`.
setRenderer(renderTasks);

onWorkspaceChange(() => {
  invalidateDistributedConsole();
  taskState.pinnedExternalTask = null;
  taskState.taskActionNotice = null;
  quickActionState.clear();
  taskState.selectedTaskId = null;
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
  taskState.crewFeedback.clear();
  taskState.complexityFeedback.clear();
  taskState.fieldFeedback.clear();
  clearTaskDataCaches();
  recoveryRefusals.clear();
  taskState.taskDecisionLoads.clear();
  taskState.expandedComments.clear();
  taskComments.rawComments.clear();
  taskState.restoredCommentHash = null;
});

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
// Recovery (requeue / settle-merged): a refused recovery's typed code and
// message, kept with the action like a refused Triage.
const recoveryRefusals = new Map();
const RECOVERY_REFUSAL_MS = 20000;
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
// A failed row action stays visible long enough to be read, then clears
// itself: about three minutes, after which the row loses `has-quick-error`
// and its layout recovers.
const QUICK_ACTION_ERROR_MS = 180_000;

function pruneQuickActionErrors(now = Date.now()) {
  let pruned = false;
  for (const [id, state] of quickActionState) {
    if (!state || state.kind !== "error") continue;
    const at = typeof state.at === "number" ? state.at : 0;
    if (now - at >= QUICK_ACTION_ERROR_MS) {
      quickActionState.delete(id);
      pruned = true;
    }
  }
  return pruned;
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

// The detail's own controls — status, complexity, crew and each field editor —
// keep their feedback outside the task object, so the detail's diff hash has to
// see it the way the row's hash sees the status/crew entries. Without this a
// saved or failed note would never paint.
function detailFeedbackSignature(taskId) {
  const parts = [feedbackSignature(taskState.statusFeedback, taskId), feedbackSignature(taskState.complexityFeedback, taskId), feedbackSignature(taskState.crewFeedback, taskId)];
  for (const field of Object.keys(taskFields.TASK_FIELD_EDITORS)) {
    parts.push(feedbackSignature(taskState.fieldFeedback, taskFields.fieldFeedbackKey(taskId, field)));
  }
  return parts.join("|");
}

// Who made and carried the task, recorded by the pipeline and so read-only.
const TASK_PEOPLE_FIELDS = [
  ["created_by", "Created by"],
  ["planned_by", "Planned by"],
  ["implemented_by", "Implemented by"],
  ["orchestrator", "Orchestrator"],
];

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

function buildTaskDetail(task, context, drain = null) {
  const detail = el("div", { class: "row-detail task-detail" });
  detail.addEventListener("click", (e) => e.stopPropagation());
  detail.teardown = () => taskComments.disconnectCommentCards(detail);

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
  const editor = (field, editSlot = null) => taskFields.buildTaskFieldEditor(task, field, detail, context, editSlot);
  const showsEditor = (field) =>
    canMutateTask(task) || taskFields.taskFieldHasValue(task, taskFields.TASK_FIELD_EDITORS[field]);

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
    const callout = buildAttentionCallout(attention, { pullRequest, onRetry: (claim) => taskWhyStuck.goToLandingRetry(detail, claim) });
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
    if (taskWhyStuck.isLongCriteria(task.acceptance_criteria)) {
      const open = taskState.expandedCriteria.has(task.id);
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
        if (nowOpen) taskState.expandedCriteria.add(task.id);
        else taskState.expandedCriteria.delete(task.id);
        toggle.setAttribute("aria-expanded", String(nowOpen));
        toggle.textContent = nowOpen ? "Show less" : "Show full AC";
      });
      block.appendChild(toggle);
    }
  }

  if (showsEditor("description")) {
    const actions = actionSlot();
    const block = addField(leftCol, "Description", taskFields.taskMarkdownPanel(editor("description", actions)), false, false, { actions, class: "description-block" });
    if (taskWhyStuck.isLongDescription(task.description)) {
      const open = taskState.expandedDescriptions.has(task.id);
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
        if (nowOpen) taskState.expandedDescriptions.add(task.id);
        else taskState.expandedDescriptions.delete(task.id);
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
    addField(disclosures, "Owed after deploy", taskEvidence.buildOwedCriteria(task.owed_criteria, task, detail, context), true, false, {
      count: task.owed_criteria.length,
    });
  }

  if (Array.isArray(task.open_decisions) && task.open_decisions.length) {
    addField(disclosures, "Decision needed", taskEvidence.buildOpenDecisions(task, detail, context), true, false, {
      count: task.open_decisions.length,
    });
  }

  if (task.plan && task.plan.trim()) {
    addField(disclosures, "Plan", taskFields.taskMarkdownPanel(taskFields.markdownView(task.plan)), true, true);
  }

  if (task.execution_summary && task.execution_summary.trim()) {
    addField(disclosures, "Execution summary", taskFields.taskMarkdownPanel(taskFields.markdownView(task.execution_summary)), true, true);
  }

  if (task.review && typeof task.review === "object") {
    addField(disclosures, "Review gate", taskEvidence.buildReviewGate(task.review), true, true);
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
  const history = taskHistory.buildTaskHistory(task, context);
  const artifacts = Array.isArray(task.artifacts) ? task.artifacts.length : 0;
  const comments = Array.isArray(task.comments) ? task.comments.length : 0;
  const runs = el("div", { class: "task-runs", id: `detail-${task.id}-runs` });
  const lifecycleActions = buildActionsRow(task, detail, context, { includeComment: false });
  const commentActions = buildActionsRow(task, detail, context, { includeLifecycle: false });
  const overview = el("div", { class: "task-overview-content" }, [
    el("div", { class: "split-layout detail-overview" }, [leftCol, rightCol.children.length ? rightCol : null]),
  ]);
  // Each tab's actions stick to the bottom of the detail scroll viewport,
  // outside the tab panels; only the active tab's footer shows. Keep the
  // wrapper when Reject or Comment replaces the action row with its form.
  const actionFooter = (tab, label, row) => {
    const footer = el("div", { class: "task-action-footer", role: "region", "aria-label": label }, [row]);
    footer.dataset.footerTab = tab;
    return footer;
  };
  const contextFilesEditSlot = actionSlot();
  const actionFooters = [
    lifecycleActions ? actionFooter("overview", "Task actions", lifecycleActions) : null,
    commentActions ? actionFooter("comments", "Comment actions", commentActions) : null,
    showsEditor("context_files") ? actionFooter("context_files", "Context files actions", contextFilesEditSlot) : null,
  ].filter(Boolean);
  const commentsContent = el("div", { class: "comments-tab-content" }, [
    comments > 0 ? taskComments.buildCommentsPanel(task, context) : el("p", { class: "field-empty", text: "No comments yet." }),
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
      content: showsEditor("context_files") ? el("div", { class: "field-block context-files-block" }, [editor("context_files", contextFilesEditSlot)]) : null,
    },
    { name: "delivery", label: "Delivery", content: delivery, available: Boolean(known && known.length) },
    { name: "history", label: "History", count: history.count, content: history.node },
    { name: "runs", label: "Runs", count: taskRunCount(task), content: runs },
    {
      name: "artifacts",
      label: "Artifacts",
      count: artifacts,
      content: artifacts > 0 ? taskEvidence.buildArtifacts(task) : el("p", { class: "field-empty", text: "No artifacts attached." }),
    },
    {
      name: "comments",
      label: "Comments",
      count: comments,
      content: commentsContent,
    },
  ], context);
  // Everything above the tab content shares one band. The task's stage strip
  // closes it after the metadata, callouts and delivery progress, just above
  // the tab strip.
  const [tabStrip, ...tabPanels] = sections;
  const band = el("div", { class: "detail-band" });
  band.appendChild(metaBlock);
  const blockCallout = taskWhyStuck.buildTaskBlockCallout(task, context, detail);
  if (blockCallout) band.appendChild(blockCallout);
  const whyStuck = taskWhyStuck.buildWhyStuckCard(task);
  if (whyStuck) band.appendChild(whyStuck);
  const pilotCallout = taskReadiness.buildPilotWarnings(task);
  if (pilotCallout) band.appendChild(pilotCallout);
  const admissionCallout = taskReadiness.buildPilotAdmissionHold(task, detail, context);
  if (admissionCallout) band.appendChild(admissionCallout);
  const deliveryAttentionBanner = taskReadiness.buildDeliveryAttentionBanner(task);
  if (deliveryAttentionBanner) band.appendChild(deliveryAttentionBanner);
  band.appendChild(attentionSlot);
  band.appendChild(progressSlot);
  const stageStrip = buildStageStrip(task);
  if (stageStrip) band.appendChild(stageStrip);
  band.appendChild(tabStrip);
  detail.appendChild(band);
  for (const panel of tabPanels) detail.appendChild(panel);
  for (const footer of actionFooters) detail.appendChild(footer);
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

// The detail's metadata head: what the task is and where it stands.
// The levers an operator sets (status, complexity, crew, host, tags) share
// one row of chips; the lifecycle strip places the status on the way to done;
// the delivery card gathers the pull request, the run, the hands that carried
// the task and its relations to other tasks. The delivery-stage strip is
// appended to the shared band immediately before its tabs. The pane head
// carries only the id and title, so nothing here is repeated there, and the
// timestamps close the Overview.
function buildTaskMeta(task, context, { editor, showsEditor, drain }) {
  const levers = el("div", { class: "task-levers" });
  levers.appendChild(buildStatusUpdateControl(task, context));
  levers.appendChild(taskFields.buildComplexityUpdateControl(task, context));
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
  const risk = buildRiskChip(task);
  if (risk) levers.appendChild(risk);
  if (showsEditor("tags")) levers.appendChild(el("div", { class: "task-levers-tags" }, [editor("tags")]));

  const head = el("div", { class: "task-meta-block" }, [levers]);
  const riskDetail = buildRiskDetail(task);
  if (riskDetail) head.appendChild(riskDetail);
  const lifecycle = buildLifecycleStrip(task.status);
  if (lifecycle) head.appendChild(lifecycle);
  const delivery = buildDeliveryCard(task, drain, context, { editor, showsEditor });
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

// The delivery stage the task's run is on, as a thinner second strip in the
// lifecycle strip's past/current/future language. The detail API derives
// `job_run_stage` from durable run and step state; a task with no readable
// run carries none and shows no strip. Rendered full-width at the end of the
// shared band, just above the tabs; at 375px a one-column strip cannot keep
// the current stage readable.
const RUN_ENDED_STATES = ["failed", "cancelled", "timeout", "interrupted"];

function buildStageStrip(task) {
  const info = task.job_run_stage;
  if (!info || !["in-progress", "review"].includes(task.status)) return null;
  const ended = RUN_ENDED_STATES.includes(info.run_state);
  const awaiting = info.run_state === "success" && task.status === "review";
  let stages = Array.isArray(info.stages) ? info.stages.map((stage) => ({ ...stage })) : [];
  if (awaiting) {
    stages = stages.map((stage) => ({ ...stage, state: "past" }));
    stages.push({ id: "awaiting_approval", label: "Awaiting approval", state: "current" });
  }
  if (!stages.length) {
    const label = info.stage_label || info.stage || info.step_id;
    if (!label) return null;
    stages = [{ id: info.stage || info.step_id, label, state: "current" }];
  }
  const classes = ["task-stage"];
  if (ended) classes.push("ended");
  if (awaiting) classes.push("awaiting");
  const list = el("ol", { class: classes.join(" "), "aria-label": "Delivery stage" });
  list.dataset.runState = info.run_state || "";
  const tooltip = [
    info.step_id,
    info.step_index != null && info.step_count != null ? `step ${info.step_index} of ${info.step_count}` : "",
  ].filter(Boolean).join(" · ");
  for (const stage of stages) {
    let text = stage.label;
    if (stage.state === "current" && !awaiting) {
      if (ended) {
        const stateLabel = info.run_state.charAt(0).toUpperCase() + info.run_state.slice(1);
        text = `${stateLabel} at ${stage.label}`;
      } else if (info.started_at) {
        const elapsed = elapsedLabel(Date.now() - Date.parse(info.started_at));
        if (elapsed) text = `${stage.label} · ${elapsed}`;
      }
    }
    let label = el("span", { class: "task-stage-label", text });
    if (stage.state === "current" && ended && task.job_run_id && task.job_run_navigable !== false) {
      const link = el("a", { class: "task-stage-label task-stage-link", text });
      link.href = runHref(String(task.job_run_id));
      label = link;
    }
    const step = el("li", { class: `task-stage-step ${stage.state}` }, [
      el("span", { class: "task-stage-bar" }),
      label,
    ]);
    if (stage.state === "current") {
      step.setAttribute("aria-current", "step");
      if (tooltip) step.title = tooltip;
    }
    list.appendChild(step);
  }
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

function buildDeliveryCard(task, drain, context, { editor, showsEditor }) {
  const row = (kind, label, value) =>
    el("div", { class: `property-row fact task-delivery-row ${kind}` }, [el("span", { class: "label", text: label }), value]);
  const rows = [];
  if (showsEditor("not_before")) rows.push(row("schedule", "Scheduled", editor("not_before")));
  if (task.started_at) rows.push(row("started-at task-started-at", "Actual start", absTimeNode(context, task.started_at)));
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
  if (task.job_run_id != null && task.job_run_id !== "") {
    rows.push(row("run", "Run", buildRunValue(task)));
    // Detail-only: the list row carries no `job_run_worktree`, so these rows
    // appear once the task detail read lands.
    const worktree = task.job_run_worktree;
    if (worktree) {
      rows.push(row("run-branch", "Branch", buildRunWorktreeValue(worktree.branch, "branch", worktree.unrecorded)));
      rows.push(row("run-worktree", "Worktree", buildRunWorktreeValue(worktree.worktree_path, "worktree path", worktree.unrecorded)));
    }
  }
  const hands = buildHandsChain(task);
  if (hands) rows.push(row("hands", "Handled by", hands));
  // One row per relation type; its ids sit inline and each opens its task.
  // A blocked_by target the Readiness card already lists under "Depends on"
  // stays there only, so the same id is never shown twice.
  const covered = drainDependencyIds(drain);
  const relations = relationTargets(Array.isArray(task.relations) ? task.relations : []);
  for (const { key, label, targets } of relations) {
    const visible = key === "blocked_by" ? targets.filter((target) => !covered.has(target)) : targets;
    if (!visible.length) continue;
    rows.push(buildRelationRow(key, label, visible, context));
  }
  const readiness = taskReadiness.readinessOf(task);
  if (readiness || drain) rows.push(row("readiness", "Readiness", taskReadiness.buildReadinessBlock(readiness, drain, task.workspace_id)));
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

// The run's branch or worktree path on its execution machine, as the detail
// read's `job_run_worktree` reports it from the run's own record; a click
// copies the exact value. A value the run never recorded reads
// "unavailable", with the reason as the hint, never a derived guess.
function buildRunWorktreeValue(recorded, noun, unrecorded) {
  const value = el("span", { class: "value run-worktree-value" });
  if (recorded) {
    // Isolate the label's LTR path order while CSS uses RTL to elide its start.
    const copy = makeCopyButton(recorded, { class: "mono run-worktree-copy", text: `\u2066${recorded}\u2069`, title: recorded });
    copy.setAttribute("aria-label", `Copy the run's ${noun}: ${recorded}`);
    value.appendChild(copy);
    return value;
  }
  value.appendChild(el("span", { class: "fact-note", text: "unavailable" }));
  if (unrecorded) value.appendChild(infoHint(unrecorded));
  return value;
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
    for (const footer of detail.querySelectorAll(".task-action-footer")) footer.hidden = footer.dataset.footerTab !== next;
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
      rerender(context);
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
    const shipped = taskState.shipInFlightTaskIds.has(taskShared.taskDispatchIdentity(task));
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
  if (includeLifecycle && (task.status === "blocked" || task.why_stuck)) appendRecoveryControls(actions, task, detail, context, mutable);
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
  const feedback = taskState.statusFeedback.get(task.id);
  const label = `Update status for ${task.id}`;
  const select = el("select", {
    class: "task-status-select mono",
    title: mutable ? label : aggregateRefusalTitle(label, "change status"),
  });
  select.setAttribute("aria-label", label);
  const placeholder = el("option", { text: STATUS_LABELS[task.status] || task.status || "status" });
  placeholder.value = "";
  placeholder.disabled = true;
  placeholder.selected = true;
  placeholder.hidden = true;
  select.appendChild(placeholder);

  for (const status of targets) {
    const option = el("option", { text: STATUS_LABELS[status] || status });
    option.value = status;
    select.appendChild(option);
  }

  if (forcedTargets.length > 0) {
    const group = el("optgroup", {
      title: `Off-table targets — forcing one overrides the lifecycle table for ${task.id}`,
    });
    group.setAttribute("label", FORCED_STATUS_GROUP_LABEL);
    for (const status of forcedTargets) {
      const option = el("option", { text: `${STATUS_LABELS[status] || status} ⚠` });
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
  const feedback = taskState.crewFeedback.get(task.id);
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

  const crews = Array.isArray(taskState.lastCrewPayload.crews) ? taskState.lastCrewPayload.crews : [];
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
    taskState.statusFeedback.set(task.id, { kind: "error", text });
    rerender(context);
    if (taskState.foldedStatusTasks.has(task.id) && taskState.selectedTaskId !== task.id) {
      scheduleFeedbackExpiry(taskState.statusFeedback, task.id, context, MUTATION_UNDO_WINDOW_MS + 500);
    }
  };
  // A summary row lists the governed targets but not what each one requires —
  // the `done` requirement depends on the task's job run, which only the
  // detail endpoint reads. Ask it before deciding whether to prompt.
  if (!forced && !("required_field" in transition)) {
    taskState.statusFeedback.set(task.id, { kind: "pending", text: "checking…" });
    rerender(context);
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
  taskState.statusFeedback.set(task.id, { kind: "pending", text: "saving…" });
  rerender(context);
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
    taskState.statusFeedback.set(task.id, feedback);
    // Fold the open detail in; an undo from a folded row keeps it folded. A
    // pinned detail is the only view of a filtered-out task, so it stays open.
    // A full page is that task's own view, so it stays too.
    const pinned = taskState.pinnedExternalTask && taskState.pinnedExternalTask.id === task.id;
    if (!pinned && taskState.taskPageId !== task.id && (taskState.selectedTaskId === task.id || taskState.foldedStatusTasks.has(task.id))) {
      if (taskState.selectedTaskId === task.id) taskState.rowFocusAfterRender = task.id;
      collapseTask(task.id);
      taskState.foldedStatusTasks.set(task.id, merged);
    }
  } catch (error) {
    if (!visit.isCurrent()) return;
    taskState.statusFeedback.set(task.id, {
      kind: "error",
      text: `status update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  rerender(context);
  scheduleFeedbackExpiry(taskState.statusFeedback, task.id, context, MUTATION_UNDO_WINDOW_MS + 500);
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
  taskState.crewFeedback.set(currentTask.id, { kind: "pending", text: "saving…" });
  rerender(context);
  try {
    const updatedTask = await patchJson(mutationPath, { crew: nextValue || null });
    if (!visit.isCurrent()) return;
    applyUpdatedTask(updatedTask, context);
    taskState.crewFeedback.set(currentTask.id, {
      kind: "success",
      text: "crew saved",
      undo: { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS },
    });
  } catch (error) {
    if (!visit.isCurrent()) return;
    taskState.crewFeedback.set(currentTask.id, {
      kind: "error",
      text: `crew update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  rerender(context);
  scheduleFeedbackExpiry(taskState.crewFeedback, currentTask.id, context, MUTATION_UNDO_WINDOW_MS + 500);
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
  const readiness = taskReadiness.readinessOf(task);
  if (readiness && readiness.preparing) return "A task-pilot run is already preparing this task";
  return "";
}

// Run pilot on a proposed task: one explicit task-pilot run for this task. It
// refreshes the task's selectors and complexity; it never approves or ships it.
function appendPilotControls(actions, task, detail, context, mutable) {
  const running = pilotRunsByTask.get(taskShared.taskDispatchIdentity(task));
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
  const refusal = pilotRefusals.get(taskShared.taskDispatchIdentity(task));
  if (refusal) {
    const note = actionErrorNode(refusal);
    note.style.flexBasis = "100%";
    actions.appendChild(note);
  }
}

function pilotSignature(task) {
  const key = taskShared.taskDispatchIdentity(task);
  const running = pilotRunsByTask.get(key);
  return `${running ? running.runId : ""}|${pilotRefusals.get(key) || ""}`;
}

async function runPilot(task, detail, btnNode, context) {
  const visit = captureWorkspaceVisit();
  const key = taskShared.taskDispatchIdentity(task);
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
    rerender(context);
    setTimeout(() => {
      if (pilotRefusals.get(key) !== message) return;
      pilotRefusals.delete(key);
      rerender(context);
    }, PILOT_REFUSAL_MS);
    return;
  }
  const runId = result && result.run_id ? String(result.run_id) : "";
  if (!visit.isCurrent()) return;
  if (runId) pilotRunsByTask.set(key, { runId });
  taskState.taskActionNotice = `${task.id}: pilot run ${runId || "(no run id)"} ${result && result.state ? result.state : "submitted"}`;
  rerender(context);
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
  if (state) taskState.taskActionNotice = `${task.id}: pilot run ${runId} ${state}`;
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
  const running = triageRunsByTask.get(taskShared.taskDispatchIdentity(task))
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
  const refusal = triageRefusals.get(taskShared.taskDispatchIdentity(task));
  if (refusal) {
    const note = actionErrorNode(refusal);
    note.style.flexBasis = "100%";
    actions.appendChild(note);
  }
}

function triageSignature(task) {
  const key = taskShared.taskDispatchIdentity(task);
  const running = triageRunsByTask.get(key);
  return `${running ? running.runId : ""}|${triageRefusals.get(key) || ""}`;
}

async function runTriage(task, detail, btnNode, context) {
  const visit = captureWorkspaceVisit();
  const key = taskShared.taskDispatchIdentity(task);
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
    rerender(context);
    setTimeout(() => {
      if (triageRefusals.get(key) !== message) return;
      triageRefusals.delete(key);
      rerender(context);
    }, TRIAGE_REFUSAL_MS);
    return;
  }
  const runId = result && result.run_id ? String(result.run_id) : "";
  if (!visit.isCurrent()) return;
  if (runId) triageRunsByTask.set(key, { runId });
  taskState.taskActionNotice = `${task.id}: triage run ${runId || "(no run id)"} ${result && result.state ? result.state : "submitted"}`;
  rerender(context);
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
  if (state) taskState.taskActionNotice = `${task.id}: triage run ${runId} ${state}`;
  await refreshTasks(context);
}

// Recovery actions on a blocked task or a held delivery (a task carrying
// `why_stuck`): requeue onto its candidate, requeue discarding it, or settle
// it from a delivery pull request merged outside the pipeline. Each asks for
// confirmation explaining what it does before anything is sent; requeue also
// asks for the reason recorded in task history. A refusal shows its typed
// code and message inline and changes nothing.
function appendRecoveryControls(actions, task, detail, context, mutable) {
  const blocked = task.status === "blocked";
  const settleable = blocked || task.status === "in-progress" || task.status === "review";
  const disabledTitle = mutable ? "" : aggregateRefusalTitle("Recovery", "recover a task");
  if (blocked) {
    const resume = el("button", {
      class: "action requeue-resume",
      text: "Resume candidate",
      title: disabledTitle || "Return this blocked task to the backlog keeping its published candidate: the next run adopts that candidate and pull request without re-implementing, then validates, reviews and lands it afresh",
    });
    resume.disabled = !mutable;
    resume.addEventListener("click", (e) => { e.stopPropagation(); runRecovery(task, detail, resume, context, "resume"); });
    actions.appendChild(resume);
    const discard = el("button", {
      class: "action requeue-discard",
      text: "Discard and re-implement",
      title: disabledTitle || "Discard this blocked task's published candidate, close its undelivered pull requests, and return it to the backlog so the next run implements fresh",
    });
    discard.disabled = !mutable;
    discard.addEventListener("click", (e) => { e.stopPropagation(); runRecovery(task, detail, discard, context, "discard"); });
    actions.appendChild(discard);
  }
  if (settleable || task.why_stuck) {
    const settle = el("button", {
      class: "action settle-merged",
      text: "Settle merged PR",
      title: disabledTitle || "Settle this task as done from its delivery pull request if that pull request already merged outside the pipeline into its target branch; if it has not merged, nothing changes",
    });
    settle.disabled = !mutable;
    settle.addEventListener("click", (e) => { e.stopPropagation(); runRecovery(task, detail, settle, context, "settle"); });
    actions.appendChild(settle);
  }
  const refusal = recoveryRefusals.get(taskShared.taskDispatchIdentity(task));
  if (refusal) {
    const note = actionErrorNode(refusal);
    note.style.flexBasis = "100%";
    actions.appendChild(note);
  }
}

function recoverySignature(task) {
  return recoveryRefusals.get(taskShared.taskDispatchIdentity(task)) || "";
}

async function runRecovery(task, detail, btnNode, context, kind) {
  const visit = captureWorkspaceVisit();
  const key = taskShared.taskDispatchIdentity(task);
  if (!canMutateTask(task)) return;
  let body;
  let suffix;
  let successText;
  if (kind === "settle") {
    if (typeof window.confirm !== "function" || !window.confirm(
      `Settle merged PR for ${task.id}? If its delivery pull request already merged outside the pipeline into its target branch, the task moves to done with a landing note naming the pull request and merge commit. If it has not merged, nothing changes and the refusal is shown here.`,
    )) return;
    suffix = "/settle-merged";
    body = {};
    successText = `${task.id}: settled from its merged pull request`;
  } else {
    const discard = kind === "discard";
    const explain = discard
      ? `Discard and re-implement ${task.id}? This discards the candidate its stopped run published, closes the task's undelivered pull requests, and returns the task to the backlog so the next run implements fresh.`
      : `Resume candidate for ${task.id}? This returns the blocked task to the backlog keeping the candidate its stopped run published: the next run adopts that candidate and pull request without re-implementing, then validates, reviews and lands it afresh.`;
    if (typeof window.confirm !== "function" || !window.confirm(explain)) return;
    if (typeof window.prompt !== "function") return;
    const reason = window.prompt(`Reason for requeueing ${task.id} (recorded in task history):`, "");
    if (reason === null) return;
    if (!reason.trim()) {
      const message = "[requeue_reason_required] requeue needs a reason; it is recorded with the decision";
      recoveryRefusals.set(key, message);
      rerender(context);
      return;
    }
    suffix = "/requeue";
    body = { reason: reason.trim(), discard_candidate: discard };
    successText = discard ? `${task.id}: requeued to implement fresh` : `${task.id}: requeued onto its published candidate`;
  }
  const requestPath = visit.path(taskMutationPath(task, suffix));
  recoveryRefusals.delete(key);
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  for (const b of detail.querySelectorAll(".action")) b.disabled = true;
  btnNode.innerHTML = `<span class="spinner"></span>wait`;
  try {
    await postJson(requestPath, body);
  } catch (error) {
    if (!visit.isCurrent()) return;
    const code = error && error.code ? error.code : "recovery_refused";
    const message = `[${code}] ${error.message || String(error)}`;
    recoveryRefusals.set(key, message);
    rerender(context);
    setTimeout(() => {
      if (recoveryRefusals.get(key) !== message) return;
      recoveryRefusals.delete(key);
      rerender(context);
    }, RECOVERY_REFUSAL_MS);
    return;
  }
  if (!visit.isCurrent()) return;
  taskState.taskActionNotice = successText;
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
  const key = taskShared.taskDispatchIdentity(task);
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
  rerender(context);
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
  btn.dataset.fixPrSignature = fixPrSignature(task);
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    runFixPr(task, detail, btn, context, data);
  });
  actions.appendChild(btn);
  const feedback = fixPrFeedback.get(taskShared.taskDispatchIdentity(task));
  if (feedback) {
    const note = feedback.ok
      ? el("div", { class: "task-action-notice", text: feedback.text, role: "status" })
      : actionErrorNode(feedback.text);
    note.classList.add("fix-pr-feedback");
    note.style.flexBasis = "100%";
    actions.appendChild(note);
  }
}

// Update only the repair action and its feedback: the surrounding detail may
// hold an editor whose text, selection and focus must survive the response.
function refreshFixPrControls(wrap, task, context) {
  const button = wrap.querySelector(".action.fix-pr");
  if (!button) return;
  fixPrState(task, context);
  if (button.dataset.fixPrSignature === fixPrSignature(task)) return;
  const detail = button.closest(".task-detail");
  if (!detail) return;
  const actions = button.parentNode;
  for (const note of actions.querySelectorAll(".fix-pr-feedback")) note.remove();
  const replacement = document.createDocumentFragment();
  appendFixPrControls(replacement, task, detail, context, canMutateTask(task));
  button.replaceWith(replacement);
}

function fixPrSignature(task) {
  const key = taskShared.taskDispatchIdentity(task);
  const entry = fixPrReadiness.get(key);
  const state = entry ? `${entry.pending}|${entry.error}|${JSON.stringify(entry.data)}` : "";
  const feedback = fixPrFeedback.get(key);
  return `${state}|${feedback ? `${feedback.ok}|${feedback.text}` : ""}`;
}

function showFixPrFeedback(key, feedback, context) {
  fixPrFeedback.set(key, feedback);
  rerender(context);
  setTimeout(() => {
    if (fixPrFeedback.get(key) !== feedback) return;
    fixPrFeedback.delete(key);
    rerender(context);
  }, FIX_PR_FEEDBACK_MS);
}

async function runFixPr(task, detail, btnNode, context, readiness) {
  const visit = captureWorkspaceVisit();
  const key = taskShared.taskDispatchIdentity(task);
  if (!canMutateTask(task)) return;
  const number = readiness.pr_number || taskPullRequestNumber(task);
  if (!window.confirm(`Hand pull request #${number} back to its delivery for an auto-fix? A repair agent pushes to the same branch; the pull request is merged only if its delivery was started to complete it.`)) return;
  const requestPath = visit.path(taskMutationPath(task, "/auto-fix"));
  fixPrFeedback.delete(key);
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  const controls = Array.from(detail.querySelectorAll(".action"), button => ({ button, disabled: button.disabled }));
  for (const { button } of controls) button.disabled = true;
  btnNode.innerHTML = `<span class="spinner"></span>wait`;
  const restoreControls = () => {
    for (const { button, disabled } of controls) button.disabled = disabled;
    btnNode.textContent = "fix PR";
  };
  let result;
  try {
    result = await postJson(requestPath, {});
  } catch (error) {
    if (!visit.isCurrent()) return;
    restoreControls();
    fixPrReadiness.delete(key);
    showFixPrFeedback(key, { ok: false, text: `fix PR refused: ${error.message || String(error)}` }, context);
    return;
  }
  if (!visit.isCurrent()) return;
  restoreControls();
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
  const dispatchKey = taskShared.taskDispatchIdentity(task);
  const requestPath = visit.path(taskWorkspacePath(task, "/api/workflows/ship"));
  if (!canMutateTask(task)) return;
  if (taskState.shipInFlightTaskIds.has(taskShared.taskDispatchIdentity(task))) return;
  taskState.shipInFlightTaskIds.add(dispatchKey);
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  for (const b of detail.querySelectorAll(".action")) b.disabled = true;
  const oldText = btnNode.textContent;
  btnNode.innerHTML = `<span class="spinner"></span>wait`;
  let dispatched = false;
  try {
    const result = await postJson(requestPath, { task_ids: [task.id] });
    taskState.shipInFlightTaskIds.delete(dispatchKey);
    dispatched = true;
    if (!visit.isCurrent()) return;
    const runId = result && result.run_id ? result.run_id : "(no run id)";
    const state = result && result.state ? result.state : "submitted";
    taskState.taskActionNotice = [`${task.id}: ship run ${runId} ${state}`, ...submissionNoticeLines(result)].join("\n");
    collapseTask(task.id);
    await refreshTasks(context);
  } catch (error) {
    if (!dispatched) taskState.shipInFlightTaskIds.delete(dispatchKey);
    if (!visit.isCurrent()) return;
    // A refresh may have replaced the detail while this request was pending.
    rerender(context);
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
    refreshFixPrControls(detail, task, context);
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
    if (opts.successNotice) taskState.taskActionNotice = opts.successNotice;
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
  if (!taskState.foldedStatusTasks.has(taskId) || taskState.selectedTaskId === taskId) return null;
  const entry = taskState.statusFeedback.get(taskId);
  return buildMutationFeedback(entry, () => {
    if (!entry || !entry.undo) return;
    const task = taskList(context).find((t) => t && t.id === taskId) || taskState.foldedStatusTasks.get(taskId);
    applyTaskStatusChange(task, entry.undo.previousValue, context);
  });
}

function foldedStatusSignature(taskId) {
  return taskState.foldedStatusTasks.has(taskId) && taskState.selectedTaskId !== taskId ? feedbackSignature(taskState.statusFeedback, taskId) : "";
}

// Folded status feedback for a task with no row on screen, usually because its
// new status is filtered out, so the undo stays reachable above the list. A
// pinned detail shows its own.
function buildFoldedStatusNotices(rowIds, context) {
  const notices = [];
  for (const [taskId, task] of taskState.foldedStatusTasks) {
    if (rowIds.has(taskId) || (taskState.pinnedExternalTask && taskState.pinnedExternalTask.id === taskId)) continue;
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
  if (!taskState.taskActionNotice) return null;
  const notice = el("div", { class: "task-action-notice", text: taskState.taskActionNotice });
  notice.dataset.key = "task-action-notice";
  notice.dataset.hash = taskState.taskActionNotice;
  notice.setAttribute("role", "status");
  notice.setAttribute("aria-live", "polite");
  taskState.taskActionNotice = null;
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
  refreshFixPrControls(wrap, task, context);
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
  wrap.dataset.hash = `${row.dataset.hash}-${JSON.stringify(ptask)}-${taskDrainSignature(drain)}-${detailFeedbackSignature(ptask.id)}-${fixPrSignature(ptask)}-${taskEvidence.decisionSignature(ptask)}`;
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
  return `${task.job_run_id || ""}-${navigable}-${host}-${pullRequest ? pullRequest.url : ""}-${taskState.shipInFlightTaskIds.has(taskShared.taskDispatchIdentity(task))}-${state ? `${state.kind}:${state.text}:${state.at || ""}` : ""}`;
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
  const pending = (state && state.kind === "pending") || (spec.kind === "ship" && taskState.shipInFlightTaskIds.has(taskShared.taskDispatchIdentity(task)));
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

function buildQuickActionError(task, context) {
  const state = quickActionState.get(task.id);
  if (!state || state.kind !== "error") return null;
  if (typeof state.at === "number" && Date.now() - state.at >= QUICK_ACTION_ERROR_MS) {
    quickActionState.delete(task.id);
    return null;
  }

  const text = state.text;
  const runMatch = text.match(/\b(jrun-[a-zA-Z0-9_\-\.]+)\b/);
  let message;
  if (runMatch) {
    const runId = runMatch[1];
    const idx = text.indexOf(runId);
    const before = text.slice(0, idx);
    const after = text.slice(idx + runId.length);
    const link = el("a", { class: "task-quick-error-link", text: runId, title: `Open run ${runId}` });
    link.href = runHref(runId);
    link.addEventListener("click", (e) => e.stopPropagation());
    message = el("span", { class: "task-quick-error-message" }, [
      before,
      link,
      after,
    ]);
  } else {
    message = el("span", { class: "task-quick-error-message", text });
  }
  const dismiss = el("button", {
    class: "task-quick-error-dismiss",
    text: "×",
    title: "Dismiss this error",
    "aria-label": `Dismiss error for ${task.id}`,
  });
  dismiss.type = "button";
  dismiss.addEventListener("click", (event) => {
    event.stopPropagation();
    if (quickActionState.get(task.id) === state) {
      quickActionState.delete(task.id);
      rerender(context || taskState.paneContext);
    }
  });
  const wrap = el("div", { class: "task-quick-error", title: "" }, [
    message,
    dismiss,
  ]);
  wrap.setAttribute("role", "status");
  wrap.setAttribute("aria-live", "assertive");
  stopRowInteraction(wrap);
  return wrap;
}

async function runQuickAction(task, kind, context) {
  const visit = captureWorkspaceVisit();
  const dispatchKey = taskShared.taskDispatchIdentity(task);
  const requestPath = visit.path(kind === "ship" ? taskWorkspacePath(task, "/api/workflows/ship") : taskMutationPath(task, "/approve"));
  if (!canRunQuickAction(task)) return;
  if (quickActionState.get(task.id)?.kind === "pending") return;
  if (kind === "ship" && taskState.shipInFlightTaskIds.has(taskShared.taskDispatchIdentity(task))) return;
  quickActionState.set(task.id, { kind: "pending", text: kind });
  if (kind === "ship") taskState.shipInFlightTaskIds.add(dispatchKey);
  rerender(context);
  let dispatched = false;
  try {
    if (kind === "ship") {
      const result = await postJson(requestPath, { task_ids: [task.id] });
      taskState.shipInFlightTaskIds.delete(dispatchKey);
      dispatched = true;
      if (!visit.isCurrent()) return;
      const runId = result && result.run_id ? result.run_id : "(no run id)";
      const state = result && result.state ? result.state : "submitted";
      taskState.taskActionNotice = [`${task.id}: ship run ${runId} ${state}`, ...submissionNoticeLines(result)].join("\n");
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
      taskState.taskActionNotice = claimed?.claim
        ? `${task.id}: handoff approved; the owner landing job completes it`
        : `${task.id} approved and moved to ${task.status === "review" ? "done" : "backlog"}`;
    }
    quickActionState.delete(task.id);
    await refreshTasks(context);
  } catch (error) {
    if (kind === "ship" && !dispatched) taskState.shipInFlightTaskIds.delete(dispatchKey);
    if (!visit.isCurrent()) return;
    quickActionState.set(task.id, { kind: "error", text: dispatched
      ? `${kind === "ship" ? "Ship" : "Approval"} was accepted, but the view could not refresh: ${error.message || String(error)}. Use Refresh to update it.`
      : `${kind} failed: ${error.message || String(error)}`, at: Date.now() });
    rerender(context);
    scheduleFeedbackExpiry(quickActionState, task.id, context, QUICK_ACTION_ERROR_MS);
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
        rerender(context);
      });
      return button;
    }),
    counts.unknown > 0 ? el("span", { class: "ready-unknown", text: `${counts.unknown} not in the drain snapshot` }) : null,
  ]);
  const why = el("a", { class: "ready-why", text: "Why? See Tasks dock", title: "Open the Tasks dock: admission status and window controls" });
  why.href = "#tasks?dock=drain";
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
    if (filterTasks(tasks, context).some((task) => task.id === p.id)) {
      taskState.pinnedExternalTask = null;
      if (taskState.openTaskId === p.id) taskState.selectedTaskId = p.id;
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
  const notice = takeTaskActionNotice();

  const filtered = filterTasks(tasks, context);
  // Rows keep a fixed-width action column so actions line up between groups;
  // when no visible row has an action (a Done-only view) the whole column is
  // dead space, so the list gives it back as one.
  body.dataset.quickActions = filtered.some(hasQuickAction) ? "some" : "none";
  $("tasks-count").textContent = formatTaskCount(filtered.length, tasks.length, tasksMeta(context));
  renderTaskPagination(context);
  syncSettleMergedButton(context);
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
      const rowHash = `${JSON.stringify(t.pilot_warnings || [])}-${JSON.stringify(t.delivery_attention || null)}-${t.id}-${t.title}-${t.status}-${JSON.stringify(t.os_requirement || null)}-${(t.tags || []).filter(isOsTag).join(",")}-${t.not_before || ""}-${t.scheduled_hold}-${Boolean(t.not_before && Date.parse(t.not_before) > Date.now())}-${taskReadiness.readinessSignature(t)}-${t.status === "backlog" ? drainWaitSignature(t.id) : ""}-${run ? runCellSignature(t) : ""}-${t.crew || ""}-${t.resolved_crew || ""}-${t.workspace_id || ""}-${crewOptionsSignature()}-${feedbackSignature(taskState.crewFeedback, t.id)}-${quickActionSignature(t)}-${foldedStatusSignature(t.id)}-${riskinessSignature(t)}`;
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
          buildScheduledChip(t, context),
          ...taskReadiness.buildReadinessBadge(t),
          ...taskReadiness.buildPilotWarningBadge(t),
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
            if (taskState.selectedTaskId === t.id && !taskState.pinnedExternalTask) closeTaskPane(context, { forget: true });
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
  const selected = !taskState.pinnedExternalTask && taskState.selectedTaskId ? `task-${taskState.selectedTaskId}` : null;
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

// The pane beside the list: a head naming the task with its close, back and
// full-page controls, then the task's detail. A pinned resolver result shows
// here too, marked as outside the list.
function renderTaskPane(filtered, context) {
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
function followRovingSelection(button) {
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
