// Orbit dashboard decision cards: the open human decision request on a task as
// a summary, one button per option beside its consequence, and a note field.

import { captureWorkspaceVisit, el, fetchJson, postJson } from './common.js';
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
  card.dataset.key = `decision-${task.id}-${request.id}`;
  card.dataset.requestId = request.id;
  if (request.summary) card.appendChild(el("p", { class: "decision-summary", text: request.summary }));
  if (request.human_action) {
    card.appendChild(el("p", { class: "decision-human-action" }, [
      el("strong", { text: "Do this: " }),
      el("span", { text: request.human_action }),
    ]));
  }
  // Recommended first, stable for the rest (server order otherwise).
  const options = (Array.isArray(request.options) ? [...request.options] : [])
    .sort((a, b) => Number(Boolean(b.recommended)) - Number(Boolean(a.recommended)));
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
      const label = answer.label || answerId;
      if (typeof window.confirm === "function"
        && !window.confirm(`Answer this decision with “${label}”?${answer.consequence ? `\n\n${answer.consequence}` : ""}`)) return;
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

// Notes belong to the mounted request, so a refresh may rebuild its card
// without losing evidence or freezing its task data. A removed/answered card
// or a navigation away has no counterpart and naturally retires the draft.
export function retainDecisionNotes(container) {
  const drafts = new Map(Array.from(container.querySelectorAll(".decision-card"))
    .map(card => {
      const note = card.querySelector(".decision-note");
      return [card.dataset.key, note ? { value: note.value, selection: document.activeElement === note ? [note.selectionStart, note.selectionEnd] : null } : null];
    }));
  return () => {
    for (const card of container.querySelectorAll(".decision-card")) {
      const note = card.querySelector(".decision-note");
      const draft = drafts.get(card.dataset.key);
      if (note && draft) {
        note.value = draft.value;
        if (document.activeElement === note && draft.selection) note.setSelectionRange(...draft.selection);
      }
    }
  };
}

// Another editor can retain the whole detail node; closed requests must still
// retire their answer controls and notes inside that retained node.
export function retireClosedDecisionCards(container, task) {
  const open = new Set(openDecisions(task).map(request => request.id));
  for (const card of container.querySelectorAll(".decision-card")) {
    if (!open.has(card.dataset.requestId)) card.remove();
  }
}

// Answered decisions for a task, read from the closed-inclusive list:
// the picked option label, who answered and when. Only a successful empty
// read removes the enclosing block; failures keep an unavailable note and
// Retry. Works in the federated view via taskWorkspacePath.
export function buildAnsweredDecisions(task, context) {
  const wrap = el("div", { class: "decision-answered-list" });
  const visit = captureWorkspaceVisit();
  const path = taskShared.taskWorkspacePath(task, `/api/decisions?task_id=${encodeURIComponent(task.id)}&include_closed=true`);
  const load = () => {
    const pendingBlock = wrap.closest(".field-block"); if (pendingBlock) pendingBlock.hidden = true;
    wrap.replaceChildren(el("p", { class: "field-empty", text: "Loading decision history…", role: "status" }));
    return fetchJson(path).then((payload) => {
      if (!visit.isCurrent()) return;
      wrap.replaceChildren();
      const items = Array.isArray(payload?.items) ? payload.items : [];
      const answered = items.filter((r) => r && r.resolution && (r.state === "answered" || r.resolution.answer_id));
      if (!answered.length) { const block = wrap.closest(".field-block"); const container = block?.parentElement; if (block) block.remove(); if (container && !container.children.length) container.remove(); return; }
      const shownBlock = wrap.closest(".field-block"); if (shownBlock) shownBlock.hidden = false;
      for (const request of answered) {
        const res = request.resolution;
        const picked = (Array.isArray(request.answers) ? request.answers : []).find((a) => String(a.id) === String(res.answer_id));
        const label = picked ? picked.label : res.answer_id;
        const when = res.answered_at ? taskShared.fmtAbsTimeValue(context, res.answered_at) : "";
        wrap.appendChild(el("div", { class: "decision-answered" }, [
          el("span", { class: "decision-answered-option", text: `Answered: ${label}` }),
          el("span", { class: "decision-answered-meta", text: `by ${res.answered_by || "unknown"}${when ? ` · ${when}` : ""}` }),
          res.note ? el("span", { class: "decision-answered-note", text: res.note }) : null,
        ]));
      }
    }).catch((error) => {
      if (!visit.isCurrent()) return;
      const errorBlock = wrap.closest(".field-block"); if (errorBlock) errorBlock.hidden = false;
      const retry = el("button", { class: "action", type: "button", text: "Retry" });
      retry.addEventListener("click", (event) => { event.stopPropagation(); load(); });
      wrap.replaceChildren(
        el("p", { class: "action-error", role: "alert", text: `Decision history unavailable: ${error.message || String(error)}` }),
        retry,
      );
    });
  };
  load();
  return wrap;
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

// One answer per request may be in flight: a second, contradictory POST
// would race the first and let arrival order pick the recorded answer.
const decisionAnswersInFlight = new Set();

async function answerDecision(task, detail, requestId, answer, note, btnNode, context) {
  const visit = captureWorkspaceVisit();
  if (!taskShared.canMutateTask(task)) return;
  const flightKey = `${taskShared.taskDispatchIdentity(task)}:${requestId}`;
  if (decisionAnswersInFlight.has(flightKey)) return;
  decisionAnswersInFlight.add(flightKey);
  const requestPath = visit.path(
    taskShared.taskWorkspacePath(task, `/api/decisions/${encodeURIComponent(requestId)}/answer`),
  );
  const prior = detail.querySelector(".action-error");
  if (prior) prior.remove();
  // Disable every option on this card (and its note), not just the clicked
  // one, for the whole flight — the pattern the detail's other actions use.
  const card = btnNode.closest(".decision-card");
  const controls = card
    ? [...card.querySelectorAll(".decision-answer, .decision-note")]
    : [btnNode];
  for (const control of controls) control.disabled = true;
  try {
    await postJson(requestPath, note ? { answer, note } : { answer });
  } catch (error) {
    decisionAnswersInFlight.delete(flightKey);
    if (!visit.isCurrent()) return;
    for (const control of controls) control.disabled = false;
    detail.prepend(taskShared.actionErrorNode(`decision answer: ${error.message || String(error)}`));
    return;
  }
  decisionAnswersInFlight.delete(flightKey);
  if (!visit.isCurrent()) return;
  taskShared.taskState.taskActionNotice = `${task.id}: decision ${requestId} answered (${answer})`;
  forgetTaskDetailLoad(task);
  taskShared.notifyDecisionAnswered();
  await taskShared.refreshTasks(context);
}
