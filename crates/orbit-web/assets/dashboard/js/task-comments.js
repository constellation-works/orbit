// Orbit dashboard task comment thread: expansion, raw/rendered toggle and thread order.

import { el, makeDisclosure, copyWithFeedback, clockStamp } from './common.js';
import { taskState, fmtAbsTimeValue, absTimeNode } from './task-shared.js';
import { explicitCrewValue, isConfiguredCrewValue } from './task-crew.js';
import * as taskFields from './task-fields.js';

// ORB-12645: which comments show their Markdown source instead of the rendered
// body, keyed `<task id>#<index into task.comments>` like
// `taskState.expandedComments` so a refresh that rebuilds the detail restores
// the thread the way it was left. The thread order is a page-wide preference,
// so it persists like the other dashboard presentation prefs.
export let rawComments = new Set();
const COMMENT_PREFS_KEY = "orbit.dashboard.comments";
let commentPrefs = loadCommentPrefs();

/* ORB-12645: the comment thread.

   Agents write spec-length Markdown here, so a comment is a document rather
   than a log line: the thread spans the full detail width below the two field
   columns, every body renders through markdown.js (the same sanitization as
   any other dashboard-authored body), and anything taller than a screenful is
   capped until the operator opens it.

   Expansion, the raw/rendered toggle and the thread order are operator state
   rather than task data, so they live in this module — keyed
   `<task id>#<index into task.comments>` — and survive the 30 s refresh that
   rebuilds the detail node. */
const COMMENT_COLLAPSE_HEIGHT_PX = 300;
// A rendered body is 13px sans at line-height 1.65 (~22px a line), and the
// ~72ch measure takes roughly 96 source columns before it wraps.
const COMMENT_LINE_HEIGHT_PX = 22;
const COMMENT_WRAP_COLUMNS = 96;
// Below this an outline would only repeat what the body already shows.
const COMMENT_OUTLINE_MIN_SECTIONS = 3;
// `by` on an agent-written comment names the crew or the agent family that
// wrote it. A human note carries the operator identity the API sanitizes to
// (ORB-10444), which is never one of these.
const AGENT_COMMENT_AUTHORS = new Set(["codex", "claude", "gemini", "grok", "agent", "system", "orbit"]);

function loadCommentPrefs() {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(COMMENT_PREFS_KEY) || "{}");
    return {
      newestFirst: parsed.newestFirst === true,
      collapsed: parsed.collapsed === true,
    };
  } catch (_) {
    return { newestFirst: false, collapsed: false };
  }
}

function saveCommentPrefs() {
  try {
    window.localStorage.setItem(COMMENT_PREFS_KEY, JSON.stringify(commentPrefs));
  } catch (_) {
    /* localStorage unavailable (private mode, quota) — thread order is a
       presentation preference, so losing it costs nothing */
  }
}

function commentSourceLines(message) {
  return String(message ?? "").split("\n");
}

// The `##` sections of a body: the collapsed card lists them instead of the
// text it is hiding, and three or more of them earn an outline.
function commentSections(message) {
  const titles = [];
  let fenced = false;
  for (const line of commentSourceLines(message)) {
    if (/^\s*(```|~~~)/.test(line)) {
      fenced = !fenced;
      continue;
    }
    if (fenced) continue;
    const heading = /^##\s+(.*?)\s*#*$/.exec(line);
    if (heading && heading[1].trim()) titles.push(heading[1].trim());
  }
  return titles;
}

/* Height has to be known while the card is still detached — the detail is
   built off-document and measuring it would mean painting the thread a frame
   late, after a reflow — so it is estimated from the source instead: one row
   a line, plus a row for every wrap. */
function estimatedCommentHeight(message) {
  let rows = 0;
  for (const line of commentSourceLines(message)) {
    rows += Math.max(1, Math.ceil(line.length / COMMENT_WRAP_COLUMNS));
  }
  return rows * COMMENT_LINE_HEIGHT_PX;
}

function commentIsLong(message) {
  return estimatedCommentHeight(message) > COMMENT_COLLAPSE_HEIGHT_PX;
}

function commentWordCount(message) {
  return String(message ?? "").split(/\s+/).filter(Boolean).length;
}

function commentMessage(comment) {
  return comment && typeof comment.message === "string" ? comment.message : "";
}

// The audit body of a pilot receipt: the whole message when the comment
// carries its typed operation id.
function pilotReceiptBody(comment) {
  const message = commentMessage(comment);
  if (comment.operation_id == null) return legacyPilotReceiptBody(message);
  return /^[a-fA-F0-9]{64}$/.test(comment.operation_id) ? message : null;
}

// Compatibility reader for receipts written before the typed operation id:
// an `operation_id=<id>` first line ahead of the audit body.
function legacyPilotReceiptBody(message) {
  const newline = message.indexOf("\n");
  if (newline < 0 || !/^operation_id=[a-fA-F0-9]{64}\r?$/.test(message.slice(0, newline))) return null;
  return message.slice(newline + 1);
}

// A pilot receipt is persisted data: summarize only a recognized assessment,
// and keep the original message for raw/copy and for unrecognized comments.
function pilotAssessment(comment) {
  if (!comment || comment.by !== "task-pilot") return null;
  const body = pilotReceiptBody(comment);
  if (body == null) return null;
  try {
    const receipt = JSON.parse(body);
    const assessment = receipt && receipt.assessment;
    const fields = ["disposition", "confidence", "recommended_crew", "recommended_complexity", "assessment_rationale"];
    if (!assessment || !fields.every(field => typeof assessment[field] === "string" && assessment[field].trim())) return null;
    return { receipt, assessment };
  } catch {
    return null;
  }
}

function pilotAssessmentView({ receipt, assessment }) {
  const view = el("div", { class: "pilot-assessment" });
  const fields = el("dl", { class: "pilot-assessment-fields" });
  const addField = (label, value) => {
    fields.appendChild(el("dt", { text: label }));
    fields.appendChild(el("dd", { text: value }));
  };
  const recommendation = (field, before) => {
    const after = assessment[field];
    return typeof before === "string" && before && before !== after ? `${before} → ${after}` : after;
  };
  addField("Disposition", assessment.disposition);
  addField("Confidence", assessment.confidence);
  addField("Recommended crew", recommendation("recommended_crew", receipt.crew_before));
  addField("Recommended complexity", recommendation("recommended_complexity", receipt.complexity_before));
  addField("Rationale", assessment.assessment_rationale);
  view.appendChild(fields);
  for (const [field, label] of [["evidence_gaps", "Evidence gaps"], ["reassessment_triggers", "Reassessment triggers"], ["blocked_by", "Blocked by"]]) {
    const items = Array.isArray(assessment[field]) ? assessment[field].filter(item => typeof item === "string" && item.trim()) : [];
    if (items.length === 0) continue;
    view.appendChild(el("div", { class: "pilot-assessment-label", text: label }));
    view.appendChild(el("ul", {}, items.map(text => el("li", { text }))));
  }
  for (const [field, label] of [["duplicate_of", "Duplicate of"], ["already_landed", "Already landed"]]) {
    const finding = assessment[field];
    if (finding == null) continue;
    const text = typeof finding === "string" ? finding : finding && typeof finding === "object"
      ? [finding.task_id, finding.commit, finding.evidence].filter(value => typeof value === "string" && value.trim()).join(" · ")
      : "";
    if (text) addField(label, text);
  }
  return view;
}

export function commentKey(taskId, index) {
  return `${taskId}#${index}`;
}

// Stable across a reorder and across the refresh: the position in
// `task.comments`, never the position on screen.
export function commentAnchorId(taskId, index) {
  return `comment-${taskId}-${index + 1}`;
}

function commentInitial(name) {
  const match = /[a-z0-9]/i.exec(String(name ?? ""));
  return match ? match[0].toUpperCase() : "?";
}

function isAgentComment(name, task) {
  const value = String(name ?? "").trim();
  if (!value) return false;
  if (AGENT_COMMENT_AUTHORS.has(value.toLowerCase())) return true;
  if (isConfiguredCrewValue(value)) return true;
  const resolved = task && task.resolved_crew ? String(task.resolved_crew) : "";
  return value === explicitCrewValue(task) || value === resolved;
}

// The task context carries absolute time only, and a thread also wants the
// "how long ago" a reader scans for, on the scale the rest of the dashboard
// uses.
function commentAge(value) {
  const at = value ? new Date(value) : null;
  if (!at || Number.isNaN(at.getTime())) return "";
  const seconds = (Date.now() - at.getTime()) / 1000;
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

function commentActionButton(label, title) {
  const button = el("button", { class: "comment-action", text: label, title });
  button.type = "button";
  return button;
}

function commentPermalink(anchorId) {
  const href = window.location && window.location.href ? String(window.location.href) : "";
  return `${href.split("#")[0]}#${anchorId}`;
}

function renderedCommentHeadings(view) {
  return typeof view.querySelectorAll === "function" ? Array.from(view.querySelectorAll("h2")) : [];
}

/* The outline follows the reader: whichever heading last crossed the top of
   the viewport owns the highlight. Observed rather than polled on scroll, and
   only where the browser supports it — without an observer the outline is
   still a working set of links. */
function trackCommentOutline(headings, links) {
  if (typeof IntersectionObserver !== "function" || headings.length === 0) return null;
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const index = headings.indexOf(entry.target);
        if (index < 0) continue;
        highlightCommentOutline(links, index);
      }
    },
    { rootMargin: "0px 0px -70% 0px" },
  );
  for (const heading of headings) observer.observe(heading);
  return observer;
}

function highlightCommentOutline(links, index) {
  links.forEach((link, position) => {
    link.className = position === index ? "comment-outline-link active" : "comment-outline-link";
  });
}

function buildCommentOutline(headings, titles) {
  const outline = el("nav", { class: "comment-outline" });
  outline.appendChild(el("div", { class: "comment-outline-title", text: "In this comment" }));
  const links = [];
  titles.forEach((title, index) => {
    const target = headings[index] || null;
    const link = el("button", { class: "comment-outline-link", text: title });
    link.type = "button";
    link.addEventListener("click", (event) => {
      event.stopPropagation();
      if (target && typeof target.scrollIntoView === "function") {
        target.scrollIntoView({ behavior: "smooth", block: "start" });
      }
      highlightCommentOutline(links, index);
    });
    links.push(link);
    outline.appendChild(link);
  });
  outline.links = links;
  return outline;
}

export function disconnectCommentCards(container) {
  if (!container) return;
  for (const child of container.children || []) {
    if (typeof child.disconnectOutline === "function") {
      child.disconnectOutline();
    }
    disconnectCommentCards(child);
  }
}

function buildCommentCard(task, comment, index, context) {
  const key = commentKey(task.id, index);
  const message = commentMessage(comment);
  const writer = comment && comment.by ? String(comment.by) : "?";
  const pilot = pilotAssessment(comment);
  const titles = pilot ? [] : commentSections(message);
  const long = !pilot && commentIsLong(message);

  const card = el("article", { class: "comment-card" });
  card.id = commentAnchorId(task.id, index);
  card.dataset.commentKey = key;

  const rawToggle = commentActionButton("raw", "Show this comment's original text");
  const copy = commentActionButton("copy", "Copy this comment's original text");
  const permalink = commentActionButton("#", "Scroll to this comment and copy a link to it");
  permalink.className = "comment-action permalink";
  const actions = el("span", { class: "comment-actions" }, [rawToggle, copy, permalink]);

  const size = long
    ? el("span", {
        class: "comment-size",
        text: titles.length > 0
          ? `${commentWordCount(message)} words · ${titles.length} sections`
          : `${commentWordCount(message)} words`,
      })
    : null;
  const head = el("div", { class: "comment-head" }, [
    el("span", { class: "comment-initial", text: commentInitial(writer) }),
    el("span", {
      class: isAgentComment(writer, task) ? "comment-by agent" : "comment-by",
      text: writer,
    }),
    isAgentComment(writer, task) ? el("span", { class: "comment-agent-pill", text: "agent" }) : null,
    absTimeNode(context, comment && comment.at, { class: "comment-at" }),
    clockStamp(
      el("span", { class: "comment-age", text: commentAge(comment && comment.at), title: fmtAbsTimeValue(context, comment && comment.at) }),
      comment && comment.at,
      { title: true },
    ),
    size,
    actions,
  ]);

  const view = pilot ? pilotAssessmentView(pilot) : taskFields.markdownView(message);
  view.className = "markdown-body comment-body";
  const headings = renderedCommentHeadings(view);
  const raw = el("pre", { class: "comment-raw", text: message });
  raw.style.display = "none";
  const bodies = el("div", { class: "comment-bodies" }, [view, raw]);
  // The outline is built from the rendered headings so its entries and its
  // scroll targets are the same nodes; the parsed `##` titles stand in only
  // where no renderer is available and the body fell back to plain text.
  const outlineTitles = headings.length > 0 ? headings.map((h) => String(h.textContent || "")) : titles;
  const outline = outlineTitles.length >= COMMENT_OUTLINE_MIN_SECTIONS
    ? buildCommentOutline(headings, outlineTitles)
    : null;
  const layout = el("div", { class: outline ? "comment-layout outlined" : "comment-layout" }, [outline, bodies]);

  let outlineObserver = null;
  const startTracking = () => {
    if (outlineObserver || !outline || headings.length === 0) return;
    outlineObserver = trackCommentOutline(headings, outline.links);
  };
  const stopTracking = () => {
    if (outlineObserver) {
      outlineObserver.disconnect();
      outlineObserver = null;
    }
  };
  card.disconnectOutline = stopTracking;
  card.teardown = stopTracking;

  const summary = el("div", {
    class: "comment-summary",
    text: titles.length > 0 ? `sections: ${titles.join(" · ")}` : `${commentWordCount(message)} words`,
  });
  const toggle = el("button", { class: "comment-toggle" });
  toggle.type = "button";
  const foot = el("div", { class: "comment-foot" }, [summary, toggle]);

  const apply = () => {
    const expanded = !long || taskState.expandedComments.has(key);
    const showsRaw = rawComments.has(key);
    card.className = [
      "comment-card",
      long ? "long" : "",
      long && !expanded ? "collapsed" : "",
      long && expanded ? "expanded" : "",
      showsRaw ? "raw" : "",
    ].filter(Boolean).join(" ");
    view.style.display = showsRaw ? "none" : "";
    raw.style.display = showsRaw ? "" : "none";
    if (outline) outline.style.display = showsRaw || !expanded ? "none" : "";
    rawToggle.textContent = showsRaw ? "rendered" : "raw";
    rawToggle.setAttribute("aria-pressed", String(showsRaw));
    toggle.textContent = expanded ? "Collapse" : "Show full comment";
    // A short comment has nothing to disclose, so its card keeps the density
    // of the line it replaced: no footer, no empty row under the body.
    foot.style.display = long ? "" : "none";
    summary.style.display = long && !expanded ? "" : "none";
    // The disclosure state belongs to the control that changes it, not to the
    // card, which is an article rather than a widget.
    toggle.setAttribute("aria-expanded", String(expanded));
    if (!expanded || showsRaw) {
      stopTracking();
    } else {
      startTracking();
    }
  };

  toggle.addEventListener("click", (event) => {
    event.stopPropagation();
    if (taskState.expandedComments.has(key)) taskState.expandedComments.delete(key);
    else taskState.expandedComments.add(key);
    apply();
  });
  rawToggle.addEventListener("click", (event) => {
    event.stopPropagation();
    if (rawComments.has(key)) rawComments.delete(key);
    else rawComments.add(key);
    apply();
  });
  copy.addEventListener("click", (event) => {
    event.stopPropagation();
    copyWithFeedback(copy, message);
  });
  permalink.addEventListener("click", (event) => {
    event.stopPropagation();
    if (typeof card.scrollIntoView === "function") {
      card.scrollIntoView({ behavior: "smooth", block: "start" });
    }
    copyWithFeedback(permalink, commentPermalink(card.id));
  });

  card.appendChild(head);
  card.appendChild(layout);
  card.appendChild(foot);
  apply();
  return card;
}

export function getCommentHash() {
  if (typeof window === "undefined" || !window.location || !window.location.hash) return "";
  const raw = String(window.location.hash);
  return raw.startsWith("#") ? raw : `#${raw}`;
}

export function buildCommentsPanel(task, context) {
  let isCollapsed = commentPrefs.collapsed === true;
  const currentHash = getCommentHash();
  if (currentHash.startsWith(`#comment-${task.id}-`) && taskState.restoredCommentHash !== currentHash) {
    isCollapsed = false;
  }
  let classes = "field-block collapsible comments-panel";
  if (isCollapsed) classes += " collapsed";
  const panel = el("div", { class: classes });
  renderCommentsPanel(panel, task, context);
  return panel;
}

function findCommentCard(id) {
  if (typeof document.getElementById === "function") {
    const node = document.getElementById(id);
    if (node && (node.id === id || String(node.className).includes("comment-card"))) return node;
  }
  const root = (typeof document.getElementById === "function" ? document.getElementById("task-detail-body") : null) || (typeof document !== "undefined" ? document.body : null);
  if (!root) return null;
  const queue = [root];
  while (queue.length > 0) {
    const curr = queue.shift();
    if (curr.id === id) return curr;
    if (curr.children) {
      for (const child of curr.children) queue.push(child);
    }
  }
  return null;
}

function findCommentsPanel(node) {
  let curr = node ? node.parentNode : null;
  while (curr) {
    if (curr.classList && typeof curr.classList.contains === "function" && curr.classList.contains("comments-panel")) {
      return curr;
    }
    if (String(curr.className || "").split(/\s+/).includes("comments-panel")) {
      return curr;
    }
    curr = curr.parentNode;
  }
  return null;
}

export function scrollToComment(hash = getCommentHash()) {
  if (!hash) return false;
  const rawId = String(hash).replace(/^#/, "");
  if (!rawId.startsWith("comment-")) return false;
  const target = findCommentCard(rawId);
  if (!target) return false;
  // The thread lives on the detail's Comments tab.
  for (let node = target.parentNode; node; node = node.parentNode) {
    if (typeof node.activateTab === "function") {
      node.activateTab("comments");
      break;
    }
  }
  const panel = findCommentsPanel(target);
  if (panel) {
    const isCollapsed = panel.classList && typeof panel.classList.contains === "function"
      ? panel.classList.contains("collapsed")
      : String(panel.className || "").split(/\s+/).includes("collapsed");
    if (isCollapsed) {
      if (panel.classList && typeof panel.classList.remove === "function") {
        panel.classList.remove("collapsed");
      }
      panel.className = String(panel.className || "")
        .split(/\s+/)
        .filter((c) => c !== "collapsed")
        .join(" ");
      const toggle = panel.querySelector ? panel.querySelector("h4 .field-toggle") : null;
      if (toggle && typeof toggle.setAttribute === "function") {
        toggle.setAttribute("aria-expanded", "true");
      }
      commentPrefs = { ...commentPrefs, collapsed: false };
      saveCommentPrefs();
    }
  }
  if (typeof target.scrollIntoView === "function") {
    target.scrollIntoView({ behavior: "smooth", block: "start" });
  }
  const currentHash = getCommentHash();
  if (currentHash && (hash === currentHash || rawId === currentHash.replace(/^#/, ""))) {
    taskState.restoredCommentHash = currentHash;
  }
  return true;
}

if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
  window.addEventListener("hashchange", () => {
    const commentHash = getCommentHash();
    if (commentHash.startsWith("#comment-")) {
      if (scrollToComment(commentHash)) {
        taskState.restoredCommentHash = commentHash;
      } else {
        taskState.restoredCommentHash = null;
      }
    } else {
      taskState.restoredCommentHash = null;
    }
  });
}

/* Rendered in place rather than through renderTasks: the thread's order and
   each card's disclosure are this panel's own state, and the detail node is
   diffed on the task payload, which none of them are part of. */
export function renderCommentsPanel(panel, task, context) {
  disconnectCommentCards(panel);
  const isCollapsed = panel.classList && typeof panel.classList.contains === "function"
    ? panel.classList.contains("collapsed")
    : String(panel.className || "").split(/\s+/).includes("collapsed");
  const comments = Array.isArray(task.comments) ? task.comments : [];
  const actions = el("span", { class: "field-actions" });
  const order = el("button", {
    class: "comment-pref",
    text: commentPrefs.newestFirst ? "newest first" : "oldest first",
    title: "Toggle the order this thread is listed in",
  });
  order.type = "button";
  order.addEventListener("click", (event) => {
    event.stopPropagation();
    commentPrefs = { ...commentPrefs, newestFirst: !commentPrefs.newestFirst };
    saveCommentPrefs();
    renderCommentsPanel(panel, task, context);
  });
  actions.appendChild(order);
  if (comments.some((comment) => commentIsLong(commentMessage(comment)))) {
    const collapseAll = el("button", {
      class: "comment-pref",
      text: "collapse all",
      title: "Collapse every expanded comment on this task",
    });
    collapseAll.type = "button";
    collapseAll.addEventListener("click", (event) => {
      event.stopPropagation();
      for (let index = 0; index < comments.length; index++) {
        taskState.expandedComments.delete(commentKey(task.id, index));
      }
      renderCommentsPanel(panel, task, context);
    });
    actions.appendChild(collapseAll);
  }
  // The header also holds the order and collapse-all buttons, so the title and
  // count are the disclosure button rather than the whole header.
  const toggle = el("button", { class: "field-toggle" }, [
    el("span", { class: "field-title", text: "Comments" }),
    el("span", { class: "field-count", text: String(comments.length) }),
  ]);
  const head = el("h4", { class: "section-title" }, [toggle, actions]);
  makeDisclosure(head, toggle, {
    expanded: !isCollapsed,
    onToggle: (event) => {
      if (event && typeof event.stopPropagation === "function") {
        event.stopPropagation();
      }
      const hasToggle = panel.classList && typeof panel.classList.toggle === "function";
      const nowCollapsed = hasToggle
        ? panel.classList.toggle("collapsed")
        : !isCollapsed;
      if (!hasToggle || nowCollapsed === undefined) {
        const set = new Set(String(panel.className || "").split(/\s+/).filter(Boolean));
        const next = !set.has("collapsed");
        if (next) set.add("collapsed");
        else set.delete("collapsed");
        panel.className = Array.from(set).join(" ");
      }
      toggle.setAttribute("aria-expanded", String(!nowCollapsed));
      commentPrefs = { ...commentPrefs, collapsed: nowCollapsed };
      saveCommentPrefs();
    },
  });
  const thread = el("div", { class: "comment-thread" });
  const ordered = comments.map((comment, index) => ({ comment, index }));
  if (commentPrefs.newestFirst) ordered.reverse();
  for (const entry of ordered) {
    thread.appendChild(buildCommentCard(task, entry.comment, entry.index, context));
  }
  panel.replaceChildren(head, thread);
}
