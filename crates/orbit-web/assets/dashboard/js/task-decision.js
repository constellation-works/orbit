// Orbit dashboard decision cards: the open human decision request on a task as
// a summary, one button per option beside its consequence, and a note field.

import { captureWorkspaceVisit, el, postJson } from './common.js';
import * as taskShared from './task-shared.js';
import { forgetTaskDetailLoad } from './task-data.js';

function openDecisions(task) {
  return Array.isArray(task.open_decisions) ? task.open_decisions : [];
}

// The open block decision on a blocked task: final recovery's decision card,
// shown by the block callout.
export function blockDecision(task) {
  if (task.status !== "blocked") return null;
  return openDecisions(task).find((request) => request.gate === "block_triage") || null;
}

// The open decision card on a proposed task a pilot warning holds: the pilot
// warning triage's summary and options, shown by the pilot-hold callout.
export function pilotHoldDecision(task) {
  if (task.status !== "proposed") return null;
  return openDecisions(task).find((request) => request.gate === "proposed_approval" && request.summary) || null;
}

// The open requests no callout card shows: they list under "Decision needed".
export function disclosedDecisions(task) {
  const card = [blockDecision(task), pilotHoldDecision(task)];
  return openDecisions(task).filter((request) => !card.includes(request));
}

// One decision as a card: the short summary, the exact human action when only
// a person outside Orbit can resolve it, a note field, and one button per
// option beside its consequence, the recommended option marked. Answering
// posts to the decisions API, which applies the option in core as the human
// writer — the same apply step as `orbit decision answer` — and the detail
// then re-reads the task, whose request now reads answered and drops out of
// `open_decisions`. The option id stays the submitted key.
export function buildDecisionCard(task, request, detail, context) {
  const card = el("div", { class: "decision-card" });
  if (request.summary) card.appendChild(el("p", { class: "decision-summary", text: request.summary }));
  if (request.human_action) {
    card.appendChild(el("p", { class: "decision-human-action" }, [
      el("strong", { text: "Do this: " }),
      el("span", { text: request.human_action }),
    ]));
  }
  const options = Array.isArray(request.options) ? request.options : [];
  if (!options.length) {
    if (!request.human_action) {
      card.appendChild(el("div", { class: "decision-pending", role: "status", text: "Answer options are unavailable for this request." }));
    }
    return card;
  }
  const mutable = taskShared.canMutateTask(task);
  const note = el("input", {
    class: "decision-note",
    type: "text",
    "aria-label": "Note recorded with the answer",
  });
  note.placeholder = "note / evidence (optional)";
  card.appendChild(note);
  const list = el("div", { class: "decision-answers" });
  for (const answer of options) {
    const answerId = String(answer.id);
    const option = el("div", { class: answer.recommended ? "decision-option recommended" : "decision-option" });
    const btn = el("button", {
      class: answer.recommended ? "action decision-answer primary" : "action decision-answer",
      type: "button",
      text: answer.label,
      title: `Answer ${request.id} with option ${answerId}`,
    });
    btn.disabled = !mutable;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      answerDecision(task, detail, request.id, answerId, note.value.trim(), btn, context);
    });
    option.appendChild(btn);
    if (answer.recommended) option.appendChild(el("span", { class: "decision-recommended", text: "Recommended" }));
    option.appendChild(el("span", { class: "decision-consequence", text: answer.consequence }));
    list.appendChild(option);
  }
  card.appendChild(list);
  return card;
}

// Open human decision requests no callout shows, one row per request.
export function buildOpenDecisions(task, detail, context) {
  const wrap = el("div", { class: "decision-requests" });
  for (const request of disclosedDecisions(task)) {
    const row = el("div", { class: "decision-request" });
    row.appendChild(el("div", {
      text: `${request.gate} decision needed (${request.id}), expires ${request.expires_at}`,
    }));
    row.appendChild(buildDecisionCard(task, request, detail, context));
    wrap.appendChild(row);
  }
  return wrap;
}

async function answerDecision(task, detail, requestId, answer, note, btnNode, context) {
  const visit = captureWorkspaceVisit();
  if (!taskShared.canMutateTask(task)) return;
  const requestPath = visit.path(
    taskShared.taskWorkspacePath(task, `/api/decisions/${encodeURIComponent(requestId)}/answer`),
  );
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  btnNode.disabled = true;
  try {
    await postJson(requestPath, note ? { answer, note } : { answer });
  } catch (error) {
    if (!visit.isCurrent()) return;
    btnNode.disabled = false;
    detail.prepend(taskShared.actionErrorNode(`decision answer: ${error.message || String(error)}`));
    return;
  }
  if (!visit.isCurrent()) return;
  taskShared.taskState.taskActionNotice = `${task.id}: decision ${requestId} answered (${answer})`;
  forgetTaskDetailLoad(task.id);
  await taskShared.refreshTasks(context);
}
