// Orbit dashboard task detail: the detail body, its tabs and the runs tab.

import { captureWorkspaceVisit, el, elapsedDurationInfo, fetchJson, fmtDuration, isAggregateView, makeDisclosure, makeToggleRow, runHref, stateCell } from './common.js';
import { absTimeNode, canMutateTask, fmtAbsTimeValue, pullRequestLink, refreshTasks, rerender, taskState, taskWorkspacePath, GITHUB_PR_REF_SYSTEM } from './task-shared.js';
import { buildAttentionCallout, buildDeliveryPanel, buildDeliveryProgress, claimAttention, deliverySteps, lastKnownClaims } from './distributed.js';
import { buildTaskMeta, buildStageStrip } from './task-lifecycle.js';
import { buildActionsRow } from './task-actions.js';
import { forgetTaskDetailLoad, taskDetailState } from './task-data.js';
import { crewOptionsSignature, feedbackSignature } from './task-crew.js';
import { buildExternalRefs } from './task-relations.js';
import * as taskHistory from './task-history.js';
import * as taskReadiness from './task-readiness.js';
import * as taskEvidence from './task-evidence.js';
import * as taskFields from './task-fields.js';
import * as taskComments from './task-comments.js';
import * as taskWhyStuck from './task-why-stuck.js';

// The pane's tab is a page-wide choice: moving the selection keeps it.
let activeDetailTab = "overview";
// The Runs tab's read per task (`GET /api/job-runs?task_id=`), kept so a
// rebuilt detail repaints the last answer while a fresh read is in flight.
export let taskRunLoads = new Map();
const TASK_RUNS_LIMIT = 20;
const TASK_RUNS_FRESH_MS = 15000;
// The detail's own controls — status, complexity, crew and each field editor —
// keep their feedback outside the task object, so the detail's diff hash has to
// see it the way the row's hash sees the status/crew entries. Without this a
// saved or failed note would never paint.
export function detailFeedbackSignature(taskId) {
  const parts = [feedbackSignature(taskState.statusFeedback, taskId), feedbackSignature(taskState.complexityFeedback, taskId), feedbackSignature(taskState.crewFeedback, taskId)];
  for (const field of Object.keys(taskFields.TASK_FIELD_EDITORS)) {
    parts.push(feedbackSignature(taskState.fieldFeedback, taskFields.fieldFeedbackKey(taskId, field)));
  }
  return parts.join("|");
}
export function buildTaskDetail(task, context, drain = null) {
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
export function buildTaskDetailPlaceholder(task, state, context) {
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
