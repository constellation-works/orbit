// Orbit dashboard task actions row: Ship, the comment and reject forms, Approve and the shared action runner.

import { captureWorkspaceVisit, el, postJson, hostWriteRefusal, submissionNoticeLines } from './common.js';
import { renderMarkdown } from './markdown.js';
import { claimedReviewApproval, handoffApprovalRequest, invalidateDistributedConsole } from './distributed.js';
import { $, taskState, rerender, refreshTasks, canMutateTask, taskWorkspacePath, taskMutationPath, actionErrorNode } from './task-shared.js';
import * as taskShared from './task-shared.js';
import { collapseTask } from './task-nav.js';
import { appendFixPrControls, appendPilotControls, appendRecoveryControls, appendTriageControls, escalationTriage, refreshFixPrControls, taskPullRequestNumber } from './task-agent-actions.js';

const reviewApprovalsInFlight = new Set();

export const APPROVE_STATUSES = new Set(["proposed", "review"]);
const REJECT_STATUSES = new Set(["proposed", "review", "backlog"]);
// Ship dispatches a task through the pipeline, which admits it out of backlog —
// so backlog is the only status where the control means anything.
export const SHIP_STATUSES = new Set(["backlog"]);
// Rows whose quick-action cell links the delivery pull request, when the task
// has one and the cell holds nothing else.
export const PULL_REQUEST_LINK_STATUSES = new Set(["review", "in-progress"]);

export function buildActionsRow(task, detail, context, { includeLifecycle = true, includeComment = true } = {}) {
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

// Closing a form swaps the actions row back in, which drops focus to the page.
// Put it back on the control that opened the form.
function restoreActionFocus(actions, selector) {
  const opener = actions.querySelector?.(selector);
  if (opener && typeof opener.focus === "function") opener.focus();
}

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
