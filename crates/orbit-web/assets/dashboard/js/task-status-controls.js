// Orbit dashboard task status and crew controls: the status and crew selects, their forced-status confirmation and their apply functions.

import { captureWorkspaceVisit, el, patchJson, isAggregateView, confirmForceOverride, forceOverrideGuardText } from './common.js';
import { taskState, rerender, statusOrder, statusTransitions, statusTransition, usesHumanCrewBypass, canMutateTask, aggregateRefusalTitle, taskMutationPath, MUTATION_UNDO_WINDOW_MS, STATUS_LABELS, taskDispatchIdentity, isSelectedTask } from './task-shared.js';
import { scheduleFeedbackExpiry, buildMutationFeedback, explicitCrewValue, hasStaleExplicitCrew, findConfiguredCrew, resolvedCrewName, defaultCrewOptionText, crewOptionTitle, aggregateCrewPayload, aggregateCrewUnavailable } from './task-crew.js';
import { applyUpdatedTask, currentCrewTask, loadTaskDetail, mergeTaskRecord } from './task-data.js';
import { collapseTask, stopRowInteraction } from './task-nav.js';

// Group heading for the status targets the lifecycle table refuses. Choosing one
// is the operator override recorded in task history as `forced`.
const FORCED_STATUS_GROUP_LABEL = "force (off-table)";

export function buildStatusUpdateControl(task, context) {
  const cell = el("span", { class: "status-cell" });
  cell.dataset.status = task.status || "";
  // A human crew's task takes every status directly: no off-table group and
  // no forced confirmation, because the server applies it as the operator's
  // own work and records it as a human-crew bypass.
  const humanBypass = usesHumanCrewBypass(task);
  const targets = humanBypass
    ? statusOrder(context).filter((status) => status && status !== task.status)
    : statusTransitions(task).map((transition) => transition.status);
  // ORB-12445: an operator may move a task to any status from any status, the
  // same override the bare CLI spells `--force`. The governed targets keep the
  // plain list; everything else the lifecycle table refuses is offered under a
  // marked group so the two are never confused, and a terminal task is no
  // longer a dead control.
  const forcedTargets = humanBypass ? [] : statusOrder(context).filter(
    (status) => status && status !== task.status && !targets.includes(status),
  );
  const mutable = canMutateTask(task);
  const feedback = taskState.statusFeedback.get(taskDispatchIdentity(task));
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
// All workspaces installs that task's own workspace registry for the build,
// then puts the ambient payload back. A selected workspace never takes this
// branch, so its select is built against `lastCrewPayload` as before.
export function buildCrewUpdateControl(task, context, options = {}) {
  if (!isAggregateView()) return buildScopedCrewUpdateControl(task, context, options);
  const previous = taskState.lastCrewPayload;
  const registryUnavailable = aggregateCrewUnavailable(task);
  taskState.lastCrewPayload = aggregateCrewPayload(task);
  try {
    return buildScopedCrewUpdateControl(task, context, { ...options, registryUnavailable });
  } finally {
    taskState.lastCrewPayload = previous;
  }
}

function buildScopedCrewUpdateControl(task, context, { withChip = true, registryUnavailable = false } = {}) {
  const cell = el("span", { class: "crew-cell" });

  const mutable = canMutateTask(task);
  const feedback = taskState.crewFeedback.get(taskDispatchIdentity(task));
  const label = `Update crew for ${task.id}`;
  const select = el("select", {
    class: "task-crew-select mono",
    title: mutable ? label : aggregateRefusalTitle(label, "change crew"),
  });
  select.setAttribute("aria-label", label);
  const currentValue = explicitCrewValue(task);
  // A failed or missing aggregate registry is "crew unavailable", not a crew
  // the workspace no longer configures. A loaded registry keeps the (missing) option.
  const staleCurrentValue = !registryUnavailable && hasStaleExplicitCrew(task);
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

  // Human crews are listed apart: assigning one hands the task to that
  // operator, and no agent picks it up.
  const humanCrews = taskState.lastCrewPayload.human_crews || [];
  if (humanCrews.length > 0) {
    const group = el("optgroup", { label: "Human crews (operator work)" });
    for (const crew of humanCrews) {
      const option = el("option", {
        text: crew.name,
        title: crew.description || "Operator work: no drain or task pilot picks it up",
      });
      option.value = crew.name;
      group.appendChild(option);
    }
    select.appendChild(group);
  }

  if (staleCurrentValue) {
    const option = el("option", {
      text: `${currentValue} (missing)`,
      title: "Configured crew no longer found",
    });
    option.value = currentValue;
    select.appendChild(option);
  } else if (currentValue && !Array.from(select.options).some((option) => option.value === currentValue)) {
    const baseCrew = findConfiguredCrew(currentValue);
    const option = el("option", {
      text: currentValue,
      title: baseCrew
        ? (baseCrew.model != null ? crewOptionTitle(baseCrew) : (baseCrew.description || "Operator work: no drain or task pilot picks it up"))
        : "",
    });
    option.value = currentValue;
    select.appendChild(option);
  }

  const registryEmpty = registryUnavailable || (crews.length === 0 && humanCrews.length === 0);
  if (registryEmpty) {
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
      text: registryEmpty ? "unavailable" : staleCurrentValue
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
  // A human crew's task skips the lifecycle gates server-side: send the bare
  // status, with no evidence prompt, forced confirmation or detail lookup.
  const humanBypass = usesHumanCrewBypass(task) && statusOrder(context).includes(nextStatus);
  let transition = humanBypass ? null : statusTransition(task, nextStatus);
  // A target the projection did not offer is an override, not a mistake: the
  // operator gets one confirm naming the move, and the PATCH carries `force`
  // so the server applies it and records the `forced` history event. A target
  // that is not a lifecycle status at all stays a refusal.
  const forced = !humanBypass && !transition;
  // A refusal before any write. Beside an open detail's control it stays until
  // the next change; on a folded row or notice it expires like the feedback it
  // replaced, so a refused undo does not linger there.
  const refuse = (text) => {
    taskState.statusFeedback.set(taskDispatchIdentity(task), { kind: "error", text });
    rerender(context);
    if (taskState.foldedStatusTasks.has(taskDispatchIdentity(task)) && !isSelectedTask(task)) {
      scheduleFeedbackExpiry(taskState.statusFeedback, taskDispatchIdentity(task), context, MUTATION_UNDO_WINDOW_MS + 500);
    }
  };
  // A summary row lists the governed targets but not what each one requires —
  // the `done` requirement depends on the task's job run, which only the
  // detail endpoint reads. Ask it before deciding whether to prompt.
  if (!humanBypass && !forced && !("required_field" in transition)) {
    taskState.statusFeedback.set(taskDispatchIdentity(task), { kind: "pending", text: "checking…" });
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
  if (!humanBypass && !forced && transition.required_field) {
    const evidence = collectStatusTransitionEvidence(task, nextStatus, transition.required_field);
    if (!evidence) {
      refuse(statusTransitionEvidenceUnavailable(transition.required_field));
      return;
    }
    payload[transition.required_field] = evidence;
  }

  const previousValue = task.status;
  taskState.statusFeedback.set(taskDispatchIdentity(task), { kind: "pending", text: "saving…" });
  rerender(context);
  const sendStatus = async (payloadToSend, wasForced) => {
    const updatedTask = await patchJson(mutationPath, payloadToSend);
    if (!visit.isCurrent()) return;
    const merged = applyUpdatedTask(updatedTask, context, task) || task;
    const feedback = {
      kind: "success",
      text: wasForced ? "status forced" : "status saved",
    };
    const reverse = statusTransition(updatedTask, previousValue);
    if (usesHumanCrewBypass(updatedTask) || (reverse && !reverse.required_field)) {
      feedback.undo = { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS };
    }
    taskState.statusFeedback.set(taskDispatchIdentity(task), feedback);
    // Fold the open detail in; an undo from a folded row keeps it folded. A
    // pinned detail is the only view of a filtered-out task, so it stays open.
    // A full page is that task's own view, so it stays too.
    const pinned = taskState.pinnedExternalTask && taskState.pinnedExternalTask.id === task.id;
    if (!pinned && taskState.taskPageId !== task.id && (isSelectedTask(task) || taskState.foldedStatusTasks.has(taskDispatchIdentity(task)))) {
      if (isSelectedTask(task)) taskState.rowFocusAfterRender = task.id;
      collapseTask(task.id);
      taskState.foldedStatusTasks.set(taskDispatchIdentity(task), merged);
    }
  };
  try {
    await sendStatus(payload, forced);
  } catch (error) {
    if (!visit.isCurrent()) return;
    // A guard `--force` overrides refused a governed target: offer the
    // override instead of a dead failure line. The server's structured
    // code decides, never its message text.
    if (!forced && error && error.code === "force_overridable") {
      const confirmed = await confirmForceOverride({
        heading: `Force ${task.id} from ${previousValue} to ${nextStatus}?`,
        guard: forceOverrideGuardText(error.body),
        effect: "Forcing does not cancel the run. The status change is applied and recorded in task history as forced.",
      });
      if (!visit.isCurrent()) return;
      if (!confirmed) {
        refuse("status update cancelled");
        return;
      }
      taskState.statusFeedback.set(taskDispatchIdentity(task), { kind: "pending", text: "saving…" });
      rerender(context);
      try {
        await sendStatus({ ...payload, force: true }, true);
      } catch (retryError) {
        if (!visit.isCurrent()) return;
        taskState.statusFeedback.set(taskDispatchIdentity(task), {
          kind: "error",
          text: `status update failed: ${retryError.message || String(retryError)}`,
        });
        console.error(retryError);
      }
    } else {
      taskState.statusFeedback.set(taskDispatchIdentity(task), {
        kind: "error",
        text: `status update failed: ${error.message || String(error)}`,
      });
      console.error(error);
    }
  }
  if (!visit.isCurrent()) return;
  rerender(context);
  scheduleFeedbackExpiry(taskState.statusFeedback, taskDispatchIdentity(task), context, MUTATION_UNDO_WINDOW_MS + 500);
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
  taskState.crewFeedback.set(taskDispatchIdentity(currentTask), { kind: "pending", text: "saving…" });
  rerender(context);
  try {
    const updatedTask = await patchJson(mutationPath, { crew: nextValue || null });
    if (!visit.isCurrent()) return;
    applyUpdatedTask(updatedTask, context, currentTask);
    taskState.crewFeedback.set(taskDispatchIdentity(currentTask), {
      kind: "success",
      text: "crew saved",
      undo: { previousValue, expiresAt: Date.now() + MUTATION_UNDO_WINDOW_MS },
    });
  } catch (error) {
    if (!visit.isCurrent()) return;
    taskState.crewFeedback.set(taskDispatchIdentity(currentTask), {
      kind: "error",
      text: `crew update failed: ${error.message || String(error)}`,
    });
    console.error(error);
  }
  rerender(context);
  scheduleFeedbackExpiry(taskState.crewFeedback, taskDispatchIdentity(currentTask), context, MUTATION_UNDO_WINDOW_MS + 500);
}
