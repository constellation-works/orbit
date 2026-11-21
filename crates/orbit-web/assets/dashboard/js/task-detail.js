// Orbit dashboard task detail: the detail body, its tabs and the runs tab.

import { captureWorkspaceVisit, copyWithFeedback, el, elapsedDurationInfo, fetchJson, fmtDuration, formatDateTime, isAggregateView, makeDisclosure, makeToggleRow, stateCell } from './common.js';
import { humanCrewFor } from './task-crew.js';
import { absTimeNode, canMutateTask, pullRequestLink, refreshTasks, rerender, bindTaskRunLink, taskState, taskWorkspacePath, GITHUB_PR_REF_SYSTEM, taskDispatchIdentity } from './task-shared.js';
import { buildAttentionCallout, buildDeliveryPanel, buildDeliveryProgress, claimAttention, deliverySteps, lastKnownClaims } from './distributed.js';
import { buildTaskMeta, buildStageStrip } from './task-lifecycle.js';
import { buildActionsRow } from './task-actions.js';
import { forgetTaskDetailLoad, taskDetailState } from './task-data.js';
import { crewOptionsSignature, feedbackSignature } from './task-crew.js';
import { buildExternalRefs } from './task-relations.js';
import * as taskHistory from './task-history.js';
import * as taskReadiness from './task-readiness.js';
import * as taskEvidence from './task-evidence.js';
import * as taskDecision from './task-decision.js';
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

export function taskRunsTabActive() {
  return activeDetailTab === "runs";
}

export function primeTaskRuns(task, payload) {
  if (!task?.id || !payload || typeof payload !== "object") return;
  const previous = taskRunLoads.get(taskDispatchIdentity(task));
  const items = Array.isArray(payload.items) ? payload.items : [];
  const failed = typeof payload.error === "string";
  taskRunLoads.set(taskDispatchIdentity(task), {
    pending: false,
    error: failed ? payload.error : null,
    // A failed first read knows nothing about the run history: keep `items`
    // null (as the direct-read path does) so the tab shows the error alone —
    // never "No runs recorded" or a zero count — and keep the entry stale so
    // opening the tab refetches instead of standing on the failure for the
    // freshness window.
    items: failed ? (previous?.items ?? null) : items,
    total: failed ? (previous?.total ?? null) : (Number.isFinite(payload.total) ? payload.total : items.length),
    at: failed ? (previous?.at ?? 0) : Date.now(),
  });
  if (task && taskRunsTabActive()) {
    const node = document.getElementById(`detail-${task.id}-runs`);
    if (node?.dataset.taskIdentity === taskDispatchIdentity(task)) renderTaskRuns(node, task, taskState.paneContext);
  }
}
// The detail's own controls — status, complexity, crew and each field editor —
// keep their feedback outside the task object, so the detail's diff hash has to
// see it the way the row's hash sees the status/crew entries. Without this a
// saved or failed note would never paint.
export function detailFeedbackSignature(task) {
  const taskId = taskDispatchIdentity(task);
  const parts = [feedbackSignature(taskState.statusFeedback, taskId), feedbackSignature(taskState.complexityFeedback, taskId), feedbackSignature(taskState.crewFeedback, taskId)];
  for (const field of Object.keys(taskFields.TASK_FIELD_EDITORS)) {
    parts.push(feedbackSignature(taskState.fieldFeedback, taskFields.fieldFeedbackKey(task, field)));
  }
  return parts.join("|");
}
function detabLeading(line) {
  let col = 0;
  let i = 0;
  while (i < line.length && (line[i] === " " || line[i] === "\t")) {
    col += line[i] === "\t" ? 4 - (col % 4) : 1;
    i++;
  }
  return i ? " ".repeat(col) + line.slice(i) : line;
}

// Column width of a whitespace run, tabs at stops of four. Measures a fence's
// own indent; it is not a rewritten command line.
function whitespaceColumns(text) {
  let col = 0;
  for (let i = 0; i < text.length; i++) {
    col += text[i] === "\t" ? 4 - (col % 4) : 1;
  }
  return col;
}

function leadingWhitespace(line) {
  let i = 0;
  while (i < line.length && (line[i] === " " || line[i] === "\t")) i++;
  return line.slice(0, i);
}

// True when `lead` itself has a tab occupying exactly [col, col + width).
function leadTabAt(lead, col, width) {
  let c = 0;
  for (let i = 0; i < lead.length; i++) {
    if (lead[i] === "\t") {
      const w = 4 - (c % 4);
      if (c === col && w === width) return true;
      c += w;
    } else {
      c += 1;
    }
    if (c > col) return false;
  }
  return false;
}

// Drop the opening fence's indent from a code line. Spaces come off up to
// that indent's column count. A tab comes off only when the fence lead covers
// those columns with a tab, so a recipe tab is never rewritten as spaces.
function stripFenceLead(line, lead) {
  if (!lead) return line;
  const columns = whitespaceColumns(lead);
  let col = 0;
  let i = 0;
  while (i < line.length && col < columns) {
    const ch = line[i];
    if (ch === " ") {
      col += 1;
      i += 1;
    } else if (ch === "\t") {
      const width = 4 - (col % 4);
      if (col + width > columns || !leadTabAt(lead, col, width)) break;
      col += width;
      i += 1;
    } else {
      break;
    }
  }
  return line.slice(i);
}

function leadingSpaces(line) {
  let i = 0;
  while (i < line.length && line[i] === " ") i++;
  return i;
}

function stripBlockquotePrefix(line) {
  let rest = line;
  let quoted = false;
  let depth = 0;
  for (;;) {
    const match = /^ {0,3}> ?/.exec(rest);
    if (!match) return { line: rest, quoted, depth };
    quoted = true;
    depth++;
    rest = rest.slice(match[0].length);
  }
}

// Remove at most `depth` quote prefixes — the enclosing container's own.
// Any further `>` is content (for example a shell redirection inside a
// fenced block quoted at that depth), not another quote marker to strip.
function stripBlockquotePrefixes(line, depth) {
  let rest = line;
  for (let i = 0; i < depth; i++) {
    const match = /^ {0,3}> ?/.exec(rest);
    if (!match) break;
    rest = rest.slice(match[0].length);
  }
  return rest;
}

// A list item's marker column and the column its content starts at, or null.
// Content indent follows CommonMark: marker width plus the spaces after it
// (more than four spaces counts as one).
function parseListItem(line) {
  const match = /^([ ]*)([-*+]|\d{1,9}[.)])(?:([ ]+)(.*)|[ ]*)$/.exec(line);
  if (!match) return null;
  const markerIndent = match[1].length;
  const markerWidth = match[2].length;
  if (match[3] === undefined) {
    return { markerIndent, contentIndent: markerIndent + markerWidth + 1 };
  }
  const spaces = match[3].length;
  return {
    markerIndent,
    contentIndent: markerIndent + markerWidth + (spaces > 4 ? 1 : spaces),
  };
}

function parseOpeningFence(line, baseIndent = 0) {
  const indent = leadingSpaces(line);
  if (indent < baseIndent || indent - baseIndent > 3) return null;
  let i = indent;
  if (i === line.length) return null;
  const char = line[i];
  if (char !== "`" && char !== "~") return null;
  let count = 0;
  while (i < line.length && line[i] === char) {
    count++;
    i++;
  }
  if (count < 3) return null;
  const info = line.slice(i);
  if (char === "`" && info.includes("`")) return null;
  return { fenceChar: char, fenceLength: count, indent };
}

function isClosingFence(line, fenceChar, fenceLength, baseIndent = 0) {
  const indent = leadingSpaces(line);
  if (indent < baseIndent || indent - baseIndent > 3) return false;
  let i = indent;
  if (i === line.length || line[i] !== fenceChar) return false;
  let count = 0;
  while (i < line.length && line[i] === fenceChar) {
    count++;
    i++;
  }
  if (count < fenceLength) return false;
  while (i < line.length && line[i] === " ") i++;
  return i === line.length;
}

// Fenced commands in a task description, in order.
export function humanTaskCommands(text) {
  const out = [];
  const lines = String(text || "").split(/\r?\n/);
  let inFence = false;
  let fenceChar = "";
  let fenceLength = 0;
  let fenceLead = "";
  let fenceBaseIndent = 0;
  let fenceInBlockquote = false;
  let fenceQuoteDepth = 0;
  let fenceInList = false;
  let blockLines = [];
  // Open list items, outermost first. A fence indented to an item's content
  // indent (plus up to three columns) is a fenced block inside that item —
  // the way `marked` renders it — while the same indent at the top level is
  // an indented code block and yields no command. Each item records the
  // blockquote depth it opened at, so an item inside a quote does not own
  // lines after that quote ends.
  let listStack = [];

  const flushBlock = (dropTrailingBlank = false) => {
    let end = blockLines.length;
    if (dropTrailingBlank) {
      while (end > 0 && !blockLines[end - 1].trim()) end--;
    }
    const c = blockLines.slice(0, end).join("\n");
    if (c.trim()) out.push(c);
    blockLines = [];
  };

  // `lead` is the opening fence's indent in command bytes (spaces, or the
  // fence line's own whitespace when the fence is a later line). `baseIndent`
  // is the enclosing list item's content column, or 0 at the top level.
  const beginFence = (open, lead, baseIndent, quote) => {
    inFence = true;
    fenceChar = open.fenceChar;
    fenceLength = open.fenceLength;
    fenceLead = lead;
    fenceBaseIndent = baseIndent;
    fenceInBlockquote = quote.quoted;
    fenceQuoteDepth = quote.depth;
    fenceInList = listStack.length > 0;
    blockLines = [];
  };

  for (let lineIndex = 0; lineIndex < lines.length; lineIndex++) {
    const rawLine = lines[lineIndex];
    // Expanded text measures list and fence structure. Command bytes stay on
    // the original line: a tab inside the command is not four spaces.
    const expanded = detabLeading(rawLine);
    if (inFence) {
      const structural = fenceInBlockquote
        ? stripBlockquotePrefixes(expanded, fenceQuoteDepth)
        : expanded;
      const source = fenceInBlockquote
        ? stripBlockquotePrefixes(rawLine, fenceQuoteDepth)
        : rawLine;
      // Markdown ends an unclosed fenced block when its enclosing list
      // item or blockquote ends. A quoted fence needs its quote prefix on
      // every line; a list fence needs lines at or past its item's content
      // indent (blank lines close neither). End the block and reprocess
      // this line outside the fence, so dedented prose stays prose and a
      // later top-level fence still opens as its own command.
      const quoteEnded =
        fenceInBlockquote &&
        stripBlockquotePrefix(expanded).depth < fenceQuoteDepth;
      const listEnded =
        !quoteEnded &&
        fenceInList &&
        !!structural.trim() &&
        leadingSpaces(structural) < fenceBaseIndent;
      if (quoteEnded || listEnded) {
        flushBlock(true);
        inFence = false;
        fenceInList = false;
        fenceInBlockquote = false;
        fenceQuoteDepth = 0;
        lineIndex--;
        continue;
      }
      if (isClosingFence(structural, fenceChar, fenceLength, fenceBaseIndent)) {
        inFence = false;
        fenceInList = false;
        fenceInBlockquote = false;
        fenceQuoteDepth = 0;
        flushBlock();
      } else {
        blockLines.push(stripFenceLead(source, fenceLead));
      }
      continue;
    }

    const { line, quoted, depth } = stripBlockquotePrefix(expanded);
    if (!line.trim()) continue;
    // A line outside an item's quote has left that quote's container, so
    // items opened inside the quote no longer own it.
    while (
      listStack.length &&
      listStack[listStack.length - 1].quoteDepth > depth
    ) {
      listStack.pop();
    }
    const indent = leadingSpaces(line);
    const item = parseListItem(line);
    // Close the items this line sits outside of. A sibling item closes the
    // previous item at its marker column; any other dedented line closes
    // every item whose content indent it falls short of. Blank lines (above)
    // close nothing, so a loose item still owns a later indented fence.
    while (listStack.length) {
      const top = listStack[listStack.length - 1];
      if (item && item.markerIndent === top.markerIndent) {
        listStack.pop();
        break;
      }
      if (indent >= top.contentIndent) break;
      listStack.pop();
    }
    if (item) {
      // An item marker indented past the top level with no item around it
      // is indented code, not a list; an item that dedents past its parent
      // after the sibling pop likewise starts nothing. Within a parent,
      // four or more extra columns make the marker indented code.
      const parent = listStack[listStack.length - 1];
      if (parent ? indent >= parent.contentIndent && indent - parent.contentIndent <= 3 : item.markerIndent <= 3) {
        listStack.push({ ...item, quoteDepth: depth });
        // The marker and its padding are not content. A fence opening in
        // that content, on this same line, is the item's code block: `marked`
        // renders it, and continuation lines are indented to the content
        // column. Skipping the line here dropped the command.
        const content = line.slice(item.contentIndent);
        const open = parseOpeningFence(content, 0);
        if (open) {
          const quote = { quoted, depth };
          beginFence(
            open,
            " ".repeat(item.contentIndent + open.indent),
            item.contentIndent,
            quote,
          );
        }
        continue;
      }
    }

    const containerIndent = listStack.length
      ? listStack[listStack.length - 1].contentIndent
      : 0;
    const open = parseOpeningFence(line, containerIndent);
    if (open) {
      const rawStructural = quoted ? stripBlockquotePrefixes(rawLine, depth) : rawLine;
      beginFence(open, leadingWhitespace(rawStructural), containerIndent, { quoted, depth });
    }
  }

  if (inFence) flushBlock();

  return out;
}
export function humanTaskSteps(text) {
  return String(text || "").split("\n").map((l) => l.trim()).filter((l) => /^([-*]|\d+[.)])\s+/.test(l)).map((l) => l.replace(/^([-*]|\d+[.)])\s+/, ""));
}
// Compact human-crew panel: summary, steps, one copy button per command,
// plus copy-all (the verify/run-all action).
export function buildHumanTaskPanel(task) {
  const crew = humanCrewFor(task);
  if (!crew) return null;
  const desc = String(task.description || "");
  const summary = desc.split(/\n\s*\n/).map((s) => s.trim()).filter(Boolean)[0] || task.title || "";
  const steps = humanTaskSteps(desc);
  const commands = humanTaskCommands(desc);
  const panel = el("div", { class: "human-task-panel" });
  panel.appendChild(el("div", { class: "human-task-head" }, [
    el("span", { class: "human-task-badge", text: `For you · ${crew.name}` }),
    commands.length ? (() => { const b = el("button", { class: "action human-copy-all", type: "button", text: "Copy all commands" }); b.addEventListener("click", (e) => { e.stopPropagation(); copyWithFeedback(b, commands.join("\n")); }); return b; })() : null,
  ]));
  if (summary) panel.appendChild(el("p", { class: "human-task-summary", text: summary.replace(/^#+\s*/, "") }));
  if (steps.length) { const ol = el("ol", { class: "human-task-steps" }); for (const s of steps) ol.appendChild(el("li", { text: s.replace(/`([^`]+)`/g, "$1") })); panel.appendChild(ol); }
  for (const cmd of commands) {
    const btn = el("button", { class: "action human-copy-cmd", type: "button", text: "Copy" });
    btn.setAttribute("aria-label", `Copy command: ${cmd.split("\n")[0]}`);
    btn.addEventListener("click", (e) => { e.stopPropagation(); copyWithFeedback(btn, cmd); });
    panel.appendChild(el("div", { class: "human-command" }, [el("pre", { class: "human-command-code", text: cmd }), btn]));
  }
  if (!steps.length && !commands.length) panel.appendChild(el("p", { class: "field-empty", text: "No steps or commands in the description yet." }));
  return panel;
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

  const humanPanel = buildHumanTaskPanel(task);
  if (humanPanel) leftCol.appendChild(humanPanel);

  if (showsEditor("acceptance_criteria")) {
    const count = Array.isArray(task.acceptance_criteria) ? task.acceptance_criteria.length : 0;
    const actions = actionSlot();
    const block = addField(leftCol, "Acceptance criteria", editor("acceptance_criteria", actions), false, false, {
      actions,
      class: "criteria-block",
      ...(count ? { count } : {}),
    });
    if (taskWhyStuck.isLongCriteria(task.acceptance_criteria)) {
      const open = taskState.expandedCriteria.has(taskDispatchIdentity(task));
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
        if (nowOpen) taskState.expandedCriteria.add(taskDispatchIdentity(task));
        else taskState.expandedCriteria.delete(taskDispatchIdentity(task));
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
      const open = taskState.expandedDescriptions.has(taskDispatchIdentity(task));
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
        if (nowOpen) taskState.expandedDescriptions.add(taskDispatchIdentity(task));
        else taskState.expandedDescriptions.delete(taskDispatchIdentity(task));
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
  // ORB-23322: the `completion.hold` rule that kept a `--complete` run's task
  // in review, with what it matched and the run it held.
  if (task.completion_hold && typeof task.completion_hold === "object") {
    const hold = task.completion_hold;
    addField(disclosures, "Held for review", el("div", { class: "completion-hold" }, [
      el("p", { text: taskHistory.completionHoldSummary(hold) }),
      el("p", { class: "muted", text: `Run ${hold.run_id || "?"} was authorized to complete this task; approve it to finish.` }),
    ]), true, false);
  }
  if (Array.isArray(task.owed_criteria) && task.owed_criteria.length) {
    addField(disclosures, "Owed after deploy", taskEvidence.buildOwedCriteria(task.owed_criteria, task, detail, context), true, false, {
      count: task.owed_criteria.length,
    });
  }

  const disclosedDecisions = taskDecision.disclosedDecisions(task);
  if (disclosedDecisions.length) {
    addField(disclosures, "Decision needed", taskDecision.buildOpenDecisions(task, detail, context), true, false, {
      count: disclosedDecisions.length,
    });
  }
  // Answered state (picked option, by whom, when) for any task.
  const decisionHistoryBlock = addField(disclosures, "Decision history", taskDecision.buildAnsweredDecisions(task), true, true);
  decisionHistoryBlock.hidden = true;

  // The rendered Markdown is the disclosure's whole body, so it sits in the
  // disclosure card with no card of its own (the description keeps its card).
  if (task.plan && task.plan.trim()) {
    addField(disclosures, "Plan", taskFields.markdownView(task.plan), true, true, { class: "task-markdown-disclosure" });
  }

  if (task.execution_summary && task.execution_summary.trim()) {
    addField(disclosures, "Execution summary", taskFields.markdownView(task.execution_summary), true, true, { class: "task-markdown-disclosure" });
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
  const history = taskHistory.buildTaskHistory(task);
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
    comments > 0 ? taskComments.buildCommentsPanel(task) : el("p", { class: "field-empty", text: "No comments yet." }),
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
  const triageBadges = taskReadiness.buildActiveTriageBadges(task);
  if (triageBadges.length) band.appendChild(el("div", { class: "task-active-triage" }, triageBadges));
  const blockCallout = taskWhyStuck.buildTaskBlockCallout(task, context, detail);
  if (blockCallout) band.appendChild(blockCallout);
  const whyStuck = taskWhyStuck.buildWhyStuckCard(task);
  if (whyStuck) band.appendChild(whyStuck);
  const pilotCallout = taskReadiness.buildPilotWarnings(task, detail, context);
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
    formatTime: (value) => formatDateTime(value),
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
  const load = taskRunLoads.get(taskDispatchIdentity(task));
  return load && load.items ? load.total : null;
}

function loadTaskRuns(task, context, fallback = null) {
  const previous = taskRunLoads.get(taskDispatchIdentity(task));
  const target = () => {
    const node = document.getElementById(`detail-${task.id}-runs`);
    return node?.dataset.taskIdentity === taskDispatchIdentity(task) ? node : fallback;
  };
  if (previous && (previous.pending || Date.now() - previous.at < TASK_RUNS_FRESH_MS)) return;
  if (isAggregateView() && !task.workspace_id) {
    taskRunLoads.set(taskDispatchIdentity(task), { pending: false, error: "Task workspace is unknown", items: null, total: null, at: Date.now() });
    renderTaskRuns(target(), task, context);
    return;
  }
  const visit = captureWorkspaceVisit();
  const load = { pending: true, error: null, items: previous ? previous.items : null, total: previous ? previous.total : null, at: Date.now() };
  taskRunLoads.set(taskDispatchIdentity(task), load);
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
    if (!visit.isCurrent() || taskRunLoads.get(taskDispatchIdentity(task)) !== load) return;
    renderTaskRuns(target(), task, context);
  });
}

function renderTaskRuns(node, task, context) {
  if (!node) return;
  node.dataset.taskIdentity = taskDispatchIdentity(task);
  const load = taskRunLoads.get(taskDispatchIdentity(task));
  const children = [];
  if (!load || (load.pending && !load.items && !load.error)) {
    const note = el("p", { class: "panel-placeholder", text: "Loading runs…", role: "status", "aria-live": "polite" });
    children.push(note);
  } else {
    if (load.error) {
      const retry = el("button", { class: "action", type: "button", text: "Retry" });
      retry.addEventListener("click", (event) => {
        event.stopPropagation();
        taskRunLoads.delete(taskDispatchIdentity(task));
        loadTaskRuns(task, context, node);
      });
      children.push(el("div", { class: "action-error", text: `Could not load runs: ${load.error}`, role: "alert" }), retry);
    }
    if (load.items && load.items.length === 0) {
      children.push(el("p", { class: "field-empty", text: "No runs recorded for this task." }));
    } else if (load.items) {
      const list = el("ul", { class: "task-run-list" });
      for (const run of load.items) list.appendChild(buildTaskRun(run, task));
      children.push(list);
      if (load.total > load.items.length) {
        children.push(el("p", { class: "task-runs-more", text: `Latest ${load.items.length} of ${load.total} runs.` }));
      }
    }
  }
  node.replaceChildren(...children);
  const count = document.getElementById(`detail-${task.id}-tab-runs`)?.querySelector?.(".detail-tab-count");
  if (count && document.getElementById(`detail-${task.id}-runs`) === node) {
    count.textContent = load && load.items ? String(load.total) : "";
  }
}

function buildTaskRun(run, task) {
  const runId = String(run.run_id || "");
  const link = el("a", { class: "mono task-run-id", text: runId });
  bindTaskRunLink(link, task, runId);
  const when = run.started_at || run.scheduled_at || run.finished_at;
  const { durationMs } = elapsedDurationInfo(run);
  return el("li", { class: "task-run" }, [
    el("div", { class: "task-run-head" }, [link, stateCell(run.state || "unknown")]),
    el("div", { class: "task-run-meta" }, [
      run.job_id ? el("span", { class: "task-run-job", text: run.job_id }) : null,
      when ? absTimeNode(when) : null,
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
      forgetTaskDetailLoad(task);
      rerender(context);
    });
    detail.appendChild(el("div", { class: "actions" }, [retry]));
  }
  return detail;
}
