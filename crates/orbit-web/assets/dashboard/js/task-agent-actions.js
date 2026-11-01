// Orbit dashboard task agent actions: Run pilot, escalation triage, recovery and Fix PR, with their refusal maps and run watchers.

import { captureWorkspaceVisit, el, fetchJson, postJson } from './common.js';
import { taskState, rerender, refreshTasks, canMutateTask, aggregateRefusalTitle, taskWorkspacePath, taskMutationPath, GITHUB_PR_REF_SYSTEM, actionErrorNode } from './task-shared.js';
import * as taskShared from './task-shared.js';
import * as taskReadiness from './task-readiness.js';

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
export const recoveryRefusals = new Map();
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
export function appendPilotControls(actions, task, detail, context, mutable) {
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
    link.href = taskShared.taskRunHref(task, running.runId);
    actions.appendChild(link);
  }
  const refusal = pilotRefusals.get(taskShared.taskDispatchIdentity(task));
  if (refusal) {
    const note = actionErrorNode(refusal);
    note.style.flexBasis = "100%";
    actions.appendChild(note);
  }
}

export function pilotSignature(task) {
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

// Drop this run's own local guard. A retired visit or a settled run releases
// only the entry it created: a newer run on the same task keeps its guard.
function releaseRunGuard(runs, key, runId) {
  const entry = runs.get(key);
  if (entry && entry.runId === runId) runs.delete(key);
}

// Follow the submitted run until it settles, then refresh: the task then shows
// the new `task_pilot_applied` entry or the pilot's warnings. A run that cannot
// be read any more is dropped; `readiness.preparing` takes over for a live one.
async function watchPilotRun(task, key, runId, visit, context) {
  const path = visit.path(taskWorkspacePath(task, `/api/runs/${encodeURIComponent(runId)}`));
  let state = "";
  for (let polls = 0; polls < PILOT_POLL_LIMIT; polls += 1) {
    await new Promise((resolve) => setTimeout(resolve, PILOT_POLL_MS));
    if (!visit.isCurrent()) return releaseRunGuard(pilotRunsByTask, key, runId);
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
  releaseRunGuard(pilotRunsByTask, key, runId);
  if (!visit.isCurrent()) return;
  if (state) taskState.taskActionNotice = `${task.id}: pilot run ${runId} ${state}`;
  await refreshTasks(context);
}

// The detail's escalation triage readout, or null when no Triage action is
// shown: the task is not held by pilot warnings (a blocked task is decided
// by final recovery and shows its decision card instead). The server
// derives it from the pipeline's own candidacy, so the panel enables the
// action exactly when the endpoint would submit.
export function escalationTriage(task) {
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

// Triage on a task held by pilot warnings: one explicit escalation triage
// run for this task. It only opens a decision card or names the human
// action; it never approves, promotes or edits the task itself.
export function appendTriageControls(actions, task, detail, context, mutable) {
  const triage = escalationTriage(task);
  const running = triageRunsByTask.get(taskShared.taskDispatchIdentity(task))
    || (triage && triage.run_id ? { runId: String(triage.run_id) } : null);
  const reason = triageDisabledReason(task, mutable, running);
  const subjectWords = "Triage this held pilot assessment: a hard-tier triager opens a decision card or names the human action that settles the hold. It never approves, promotes or edits the task";
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
    link.href = taskShared.taskRunHref(task, running.runId);
    actions.appendChild(link);
  }
  const refusal = triageRefusals.get(taskShared.taskDispatchIdentity(task));
  if (refusal) {
    const note = actionErrorNode(refusal);
    note.style.flexBasis = "100%";
    actions.appendChild(note);
  }
}

export function triageSignature(task) {
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
    if (!visit.isCurrent()) return releaseRunGuard(triageRunsByTask, key, runId);
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
  releaseRunGuard(triageRunsByTask, key, runId);
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
export function appendRecoveryControls(actions, task, detail, context, mutable) {
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

export function recoverySignature(task) {
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
export function taskPullRequestNumber(task) {
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
export function fixPrState(task, context) {
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
export function appendFixPrControls(actions, task, detail, context, mutable) {
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
export function refreshFixPrControls(wrap, task, context) {
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

export function fixPrSignature(task) {
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
