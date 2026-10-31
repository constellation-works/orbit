// Orbit dashboard task row quick actions: the Approve, Ship and remote-run controls a row shows, with their pending and failed state.

import { captureWorkspaceVisit, getWorkspace, el, postJson, isAggregateView, runHref, submissionNoticeLines } from './common.js';
import { drainReadinessFor } from './drain-waits.js';
import { claimedReviewApproval, handoffApprovalRequest, invalidateDistributedConsole, registeredMachineHost } from './distributed.js';
import { taskState, rerender, refreshTasks, canMutateTask, aggregateRefusalTitle, taskWorkspacePath, taskMutationPath, pullRequestLink } from './task-shared.js';
import { humanCrewFor, scheduleFeedbackExpiry } from './task-crew.js';
import * as taskShared from './task-shared.js';
import { stopRowInteraction } from './task-nav.js';
import { APPROVE_STATUSES, PULL_REQUEST_LINK_STATUSES, SHIP_STATUSES } from './task-actions.js';

// Row shortcuts (Approve on proposed or review tasks, Ship on backlog): the
// pending or failed state of each, keyed by task id, so a refresh repaints it.
export let quickActionState = new Map();
// A failed row action stays visible long enough to be read, then clears
// itself: about three minutes, after which the row loses `has-quick-error`
// and its layout recovers.
const QUICK_ACTION_ERROR_MS = 180_000;

export function pruneQuickActionErrors(now = Date.now()) {
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

export function quickActionSignature(task) {
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
export function hasQuickAction(task) {
  if (task.status === "in-progress" && task.job_run_id) {
    if (task.job_run_navigable !== false) return true;
    if (summaryExecutionLocation(task.job_run_machine).known) return true;
  }
  if (PULL_REQUEST_LINK_STATUSES.has(task.status) && pullRequestLink(task)) return true;
  // A human crew's task is operator work no drain admits, so it has no Ship.
  return APPROVE_STATUSES.has(task.status) || (SHIP_STATUSES.has(task.status) && !humanCrewFor(task));
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

export function buildQuickAction(task, context) {
  const cell = el("span", { class: "task-quick-cell" });
  stopRowInteraction(cell);
  const state = quickActionState.get(task.id);
  if (task.status === "in-progress" && task.job_run_id) {
    // A missing flag stays a link: that is the historical unrecorded-host row.
    // An explicit false means the run was recorded on another machine, so open
    // it on that host rather than this host's job store.
    if (task.job_run_navigable !== false) {
      const link = el("a", { class: "task-quick-link", text: "View run", title: `Open run ${task.job_run_id}` });
      link.href = taskShared.taskRunHref(task, task.job_run_id);
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
  } else if (SHIP_STATUSES.has(task.status) && !humanCrewFor(task)) {
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

export function buildQuickActionError(task, context) {
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
    link.href = taskShared.taskRunHref(task, runId);
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
