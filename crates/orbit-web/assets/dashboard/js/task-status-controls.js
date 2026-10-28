// Orbit dashboard task status and crew controls: the status and crew selects, their forced-status confirmation and their apply functions.

import { captureWorkspaceVisit, el, patchJson, isAggregateView } from './common.js';
import { taskState, rerender, statusOrder, statusTransitions, statusTransition, canMutateTask, aggregateRefusalTitle, taskMutationPath, MUTATION_UNDO_WINDOW_MS, STATUS_LABELS } from './task-shared.js';
import { scheduleFeedbackExpiry, buildMutationFeedback, explicitCrewValue, hasStaleExplicitCrew, resolvedCrewName, defaultCrewOptionText, crewOptionTitle } from './task-crew.js';
import { applyUpdatedTask, currentCrewTask, loadTaskDetail, mergeTaskRecord } from './task-data.js';
import { collapseTask, stopRowInteraction } from './task-nav.js';

// Group heading for the status targets the lifecycle table refuses. Choosing one
// is the operator override recorded in task history as `forced`.
const FORCED_STATUS_GROUP_LABEL = "force (off-table)";

export function buildStatusUpdateControl(task, context) {
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
export function buildCrewUpdateControl(task, context, { withChip = true } = {}) {
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

export async function applyTaskStatusChange(task, nextStatus, context) {
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
