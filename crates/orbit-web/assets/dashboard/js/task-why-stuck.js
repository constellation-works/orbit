// Orbit dashboard task description and criteria toggles, why-stuck and the block callout.

import { el, runHref } from './common.js';
import { taskState, absTimeNode } from './task-shared.js';
import * as taskComments from './task-comments.js';
import * as taskFields from './task-fields.js';

// Descriptions longer than this open clamped behind "Show full description".
// Estimated from the source, like a long comment, because the detail is built
// off-document and cannot be measured before it paints.
const LONG_DESCRIPTION_CHARS = 700;
const LONG_DESCRIPTION_LINES = 12;

export function isLongDescription(text) {
  const value = String(text || "");
  return value.length > LONG_DESCRIPTION_CHARS || value.split("\n").length > LONG_DESCRIPTION_LINES;
}

export function isLongCriteria(criteria) {
  return isLongDescription(taskFields.linesToText(criteria));
}

// The attention callout's Retry landing goes to the claim panel's own control,
// which asks for the reason and sends the request; nothing is sent from here.
export function goToLandingRetry(detail, claim) {
  if (typeof detail.activateTab === "function") detail.activateTab("delivery");
  const panel = Array.from(detail.querySelectorAll(".claim-panel"))
    .find((node) => node.getAttribute("data-claim-id") === String((claim && claim.claim_id) || ""));
  const target = panel && (panel.querySelector('[data-reason-for~="retry-handoff"]') || panel.querySelector(".claim-actions"));
  if (!target) return;
  if (typeof target.scrollIntoView === "function") target.scrollIntoView({ block: "center", behavior: "smooth" });
  if (target.tagName === "INPUT") target.focus({ preventScroll: true });
}

// The five facts an operator needs before deciding what to do with a blocked
// task or a held delivery. Read-only: the block callout keeps the actions.
export function buildWhyStuckCard(task) {
  const stuck = task.why_stuck;
  if (!stuck || typeof stuck !== "object") return null;
  const headingId = `why-stuck-${task.id}`;
  return el("section", { class: "why-stuck", "aria-labelledby": headingId }, [
    el("h3", { class: "why-stuck-title", id: headingId, text: "Why stuck" }),
    el("dl", { class: "why-stuck-rows" }, [
      whyStuckRow("Escalation", whyStuckEscalationValue(task, stuck.escalation_note)),
      whyStuckRow("Landing review", whyStuckLanding(stuck.landing_review)),
      whyStuckRow("Root cause", whyStuckLongNote(task, "root cause", whyStuckNote(stuck.root_cause))),
      whyStuckRow("Mergeable", whyStuckMergeable(stuck.pull_request)),
      whyStuckRow("Required checks", whyStuckChecks(stuck.pull_request)),
    ]),
  ]);
}

function whyStuckEscalationValue(task, fact) {
  const note = whyStuckNote(fact);
  const block = task.block || {};
  const readableNote = fact && fact.availability === "read" && typeof fact.text === "string" && fact.text.trim();
  if (task.status === "blocked" && block.cause === "escalation" && readableNote &&
      typeof block.reason === "string" && block.reason.trim() === fact.text.trim()) {
    const reference = el("a", { text: "See blocked note above" });
    const headingId = `block-heading-${task.id}`;
    reference.href = `#${headingId}`;
    reference.addEventListener("click", (event) => {
      event.preventDefault();
      document.getElementById(headingId)?.scrollIntoView?.({ block: "center" });
    });
    return reference;
  }
  return whyStuckLongNote(task, "escalation", note);
}

function whyStuckRow(label, value) {
  const body = typeof value === "string"
    ? el("dd", { class: "why-stuck-value", text: value })
    : value;
  if (body && body.classList && !body.classList.contains("why-stuck-value") && body.tagName !== "UL") {
    body.classList.add("why-stuck-value");
  }
  return el("div", { class: "why-stuck-row" }, [
    el("dt", { class: "why-stuck-label", text: label }),
    body,
  ]);
}

// Escalation notes and root causes can carry whole CI log excerpts; past this
// they open clamped behind "Show full …", like a long description.
const LONG_WHY_STUCK_CHARS = 280;
const LONG_WHY_STUCK_LINES = 5;
// `<task id>:<row>` of the notes the operator expanded; a rebuild keeps them open.
const expandedWhyStuck = new Set();

function whyStuckLongNote(task, name, text) {
  if (text.length <= LONG_WHY_STUCK_CHARS && text.split("\n").length <= LONG_WHY_STUCK_LINES) return text;
  const key = `${task.id}:${name}`;
  const open = expandedWhyStuck.has(key);
  const value = el("dd", { class: open ? "why-stuck-value" : "why-stuck-value clamped" }, [
    el("div", { class: "why-stuck-text", text }),
  ]);
  const closedLabel = `Show full ${name}`;
  const toggle = el("button", {
    class: "why-stuck-toggle",
    type: "button",
    text: open ? "Show less" : closedLabel,
    "aria-expanded": String(open),
  });
  toggle.addEventListener("click", (event) => {
    event.stopPropagation();
    const nowOpen = value.classList.contains("clamped");
    value.classList.toggle("clamped", !nowOpen);
    if (nowOpen) expandedWhyStuck.add(key);
    else expandedWhyStuck.delete(key);
    toggle.setAttribute("aria-expanded", String(nowOpen));
    toggle.textContent = nowOpen ? "Show less" : closedLabel;
  });
  value.appendChild(toggle);
  return value;
}

function whyStuckNote(fact) {
  if (!fact || typeof fact !== "object") return "unavailable";
  if (fact.availability === "none") return "none";
  if (fact.availability === "read" && typeof fact.text === "string" && fact.text.trim()) return fact.text;
  return "unavailable";
}

function whyStuckLanding(fact) {
  if (!fact || fact.availability !== "read" || typeof fact.verdict !== "string" || !fact.verdict.trim()) {
    return "unavailable";
  }
  return `${fact.verdict} · ${whyStuckFindings(fact.finding_count)}`;
}

function whyStuckFindings(count) {
  if (typeof count !== "number" || !Number.isFinite(count)) return "findings unavailable";
  const n = Math.trunc(count);
  return n === 1 ? "1 finding" : `${n} findings`;
}

function whyStuckMergeable(pull) {
  if (pull && pull.availability === "read" && (pull.mergeable === "mergeable" || pull.mergeable === "conflicting")) {
    return pull.mergeable;
  }
  const detail = pull && typeof pull.detail === "string" ? pull.detail.trim() : "";
  return detail ? `unavailable — ${detail}` : "unavailable";
}

function whyStuckChecks(pull) {
  if (!pull || pull.availability !== "read" || !Array.isArray(pull.checks)) {
    return el("dd", { class: "why-stuck-value", text: "unavailable" });
  }
  const checks = pull.checks.filter((check) => check && typeof check.name === "string" && check.name.trim());
  if (!checks.length) return el("dd", { class: "why-stuck-value", text: "none" });
  return el("ul", { class: "why-stuck-checks" }, checks.map((check) => {
    const state = check.state === "failed" || check.state === "passed" || check.state === "pending"
      ? check.state
      : "pending";
    const item = el("li", { class: "why-stuck-check" }, [
      el("span", { class: "why-stuck-check-name", text: check.name }),
      el("span", { class: "why-stuck-state", text: state }),
    ]);
    item.setAttribute("data-state", state);
    return item;
  }));
}

// Core selects the latest block or escalation and supplies readable evidence.
// Render it as text: diagnoses and operator actions are untrusted task content.
export function buildTaskBlockCallout(task, context, detail) {
  if (task.status !== "blocked") return null;
  const block = task.block || {};
  const headingId = `block-heading-${task.id}`;
  const content = el("div", { class: "task-attention-content" }, [
    el("h3", { class: "task-attention-title", id: headingId, text: "Blocked · needs your attention" }),
    el("p", {
      class: "task-attention-body task-block-reason",
      text: block.reason || "No block reason was recorded.",
    }),
  ]);
  const time = el("p", { class: "task-block-time" });
  if (block.at) {
    time.append("Blocked at ", absTimeNode(context, block.at));
  } else {
    time.textContent = "Block time not recorded.";
  }
  content.appendChild(time);
  const next = block.next_action || (task.next_step && task.next_step.resume_command
    ? "Review the failed run, resolve the cause, then resume the run or move this task back to backlog."
    : "Review the task and its evidence, resolve the cause, then move this task back to backlog when it is ready to retry.");
  content.appendChild(el("p", { class: "task-attention-body task-block-next", text: `Next: ${next}` }));
  const actions = el("div", { class: "task-attention-actions" });
  if (Number.isInteger(block.comment_index) && Array.isArray(task.comments) && task.comments[block.comment_index]) {
    const read = el("button", { class: "attention-action primary", type: "button", text: "Read escalation" });
    read.addEventListener("click", () => {
      detail.activateTab("comments");
      const id = taskComments.commentAnchorId(task.id, block.comment_index);
      taskState.expandedComments.add(taskComments.commentKey(task.id, block.comment_index));
      const panel = detail.querySelector(".comments-panel");
      if (panel) taskComments.renderCommentsPanel(panel, task, context);
      taskComments.scrollToComment(`#${id}`);
    });
    actions.appendChild(read);
  }
  if (block.run_id) {
    const run = el("a", { class: "attention-action", text: "View run" });
    run.href = runHref(block.run_id, task.workspace_id ? { workspace: task.workspace_id } : {});
    actions.appendChild(run);
  }
  if (!actions.children.length) {
    const history = el("button", { class: "attention-action", type: "button", text: "View history" });
    history.addEventListener("click", () => detail.activateTab("history"));
    actions.appendChild(history);
  }
  content.appendChild(actions);
  return el("section", { class: "task-attention warning task-block-callout", "aria-labelledby": headingId }, [
    el("span", { class: "task-attention-icon", "aria-hidden": "true" }), content,
  ]);
}
