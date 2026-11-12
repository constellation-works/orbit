// Knowledge › Memory: crew memory spaces, their history and the global
// proposals, read-only. The list and detail reuse the Frictions sub-tab's
// components (knowledge rows and pills, the detail head, the property card
// and, at one column, the inline accordion detail). Every read goes through
// `/api/memory/*`, which calls the operations `orbit memory` uses and writes
// nothing; accepting, rejecting and writing memory stay on the operator CLI.

import { bindTaskLink, el, enableRovingRows, fetchJson, formatAge, formatDate, formatDateTime, getWorkspaceRevision, isAggregateView, makeRowDisclosure, onWorkspaceChange, renderPanelPlaceholder, requestPanel, syncNodes } from './common.js';
import { renderMarkdown } from './markdown.js';
import { setActiveTab } from './router.js';

/// Rows a list shows before "Show all" (STD-07 §R17).
const MEMORY_ROWS_SHOWN = 7;
/// The detail's selection that shows the global proposals.
export const MEMORY_PROPOSALS = "proposals";
const ACCORDION_QUERY = "(max-width: 1000px)";
const accordionMedia = window.matchMedia(ACCORDION_QUERY);

// What is on screen: a memory key, the proposals, or null (the first key
// beside the list; nothing in the one-column accordion).
let selected = null;
let spaces = null;
let proposals = null;
// The visit (`getWorkspaceRevision`) these payloads were read for. A host
// change bumps that revision too. The memory key is often `global` in every
// workspace, so a key match alone cannot tell one visit's files from another's.
let listRevision = null;
let detail = { key: null, space: null, history: null, error: null, revision: null };
const expanded = { keys: false, history: false, proposals: false };

function emptyDetail() {
  return { key: null, space: null, history: null, error: null, revision: null };
}

function clearMemoryDom() {
  for (const id of ["memory-body", "memory-detail"]) {
    const node = document.getElementById(id);
    if (node) node.replaceChildren();
  }
}

// Drop every cached payload at the scope boundary, before a repaint can read
// it. The list panel is reset by the shared panel scope listener; the detail
// panel is not, when the open selection has no detail request of its own
// (global proposals). Clear both so a repaint cannot keep the previous visit.
// A later read of the same visit may keep its detail until the refresh lands.
function retireMemoryCache() {
  spaces = null;
  proposals = null;
  listRevision = null;
  detail = emptyDetail();
  clearMemoryDom();
  const count = document.getElementById("knowledge-count");
  if (count) count.textContent = "—";
}

onWorkspaceChange(retireMemoryCache);

function detailInVisit(key) {
  if (detail.revision !== getWorkspaceRevision() || detail.key !== key) return null;
  return detail;
}

const EVENT_LABELS = {
  dream_minted: "Dream minted",
  dream_skipped: "Dream skipped",
  no_revision: "No revision",
  revision_written: "Revision written",
  write_refused: "Write refused",
  reset: "Reset",
  copied: "Copied",
  proposal_opened: "Proposal opened",
  propose_refused: "Proposal refused",
  proposal_accepted: "Proposal accepted",
  proposal_rejected: "Proposal rejected",
  proposal_expired: "Proposal expired",
};

/// The selection on screen, for the router.
export function getMemorySelection() {
  return selected;
}

/// Select a memory key, the proposals, or nothing.
export function setMemorySelection(value) {
  const next = value ? String(value) : null;
  if (next !== selected) {
    expanded.history = false;
    expanded.proposals = false;
  }
  selected = next;
}

/// The hash route of the view on screen, without the leading `#`.
export function memoryRoute() {
  return selected ? `knowledge/memory/${encodeURIComponent(selected)}` : "knowledge/memory";
}

function select(value) {
  setMemorySelection(value);
  setActiveTab(memoryRoute());
}

function accordion() {
  return accordionMedia.matches;
}

/// The key the detail shows: the selection, or beside the list the first key.
function shownSelection() {
  if (selected) return selected;
  if (accordion()) return null;
  return spaces?.keys?.[0]?.key ?? null;
}

// Crossing the accordion breakpoint changes which key the detail shows.
// Paint alone leaves that key on "Loading…" until the next refresh.
accordionMedia.addEventListener("change", () => {
  paint();
  const key = shownSelection();
  if (key && key !== MEMORY_PROPOSALS && !detailInVisit(key)) fetchDetail();
});

export function fetchAndRenderMemory() {
  if (isAggregateView()) {
    renderPanelPlaceholder("memory-body");
    renderPanelPlaceholder("memory-detail");
    return Promise.resolve();
  }
  const revision = getWorkspaceRevision();
  return requestPanel("memory-body", "memory", () => Promise.all([
    fetchJson("/api/memory/spaces"),
    fetchJson("/api/memory/proposals"),
  ]), ([spacesPayload, proposalsPayload]) => {
    if (revision !== getWorkspaceRevision()) return;
    spaces = spacesPayload;
    proposals = proposalsPayload;
    listRevision = revision;
    paint();
    return fetchDetail();
  });
}

function fetchDetail() {
  const key = shownSelection();
  const revision = getWorkspaceRevision();
  if (!spaces?.enabled || !key || key === MEMORY_PROPOSALS || listRevision !== revision) return Promise.resolve();
  const query = `key=${encodeURIComponent(key)}`;
  return requestPanel("memory-detail", `memory:${key}`, () => Promise.all([
    fetchJson(`/api/memory/space?${query}`),
    fetchJson(`/api/memory/history?${query}`),
  ]), ([space, history]) => {
    if (revision !== getWorkspaceRevision()) return;
    detail = { key, space, history, error: null, revision };
    paint();
  }).catch((error) => {
    if (revision !== getWorkspaceRevision()) return;
    detail = { key, space: null, history: null, error: error.message || String(error), revision };
    paint();
  });
}

// ---- list ---------------------------------------------------------------------

function paint() {
  const body = document.getElementById("memory-body");
  if (!body || !spaces || listRevision !== getWorkspaceRevision()) return;
  enableRovingRows(body);
  const count = document.getElementById("knowledge-count");
  if (!spaces.enabled) {
    if (count) count.textContent = "off";
    syncNodes(body, [keyed(emptyState(offText()), "memory-off", spaces.enabled_key)]);
    paintDetailPanel(null);
    return;
  }
  const keys = Array.isArray(spaces.keys) ? spaces.keys : [];
  if (count) count.textContent = `${keys.length} ${keys.length === 1 ? "key" : "keys"}`;
  const shown = shownSelection();
  const visible = expanded.keys ? keys : keys.slice(0, MEMORY_ROWS_SHOWN);
  // A selected key past the fold stays on screen.
  if (!expanded.keys && shown && !visible.some((space) => space.key === shown)) {
    const hidden = keys.find((space) => space.key === shown);
    if (hidden) visible.push(hidden);
  }
  const nodes = [];
  for (const space of visible) {
    nodes.push(spaceRow(space, shown === space.key));
    if (accordion() && shown === space.key) nodes.push(inlineDetail(space.key));
  }
  if (keys.length > MEMORY_ROWS_SHOWN) {
    nodes.push(showAllControl("memory-keys-toggle", expanded.keys, keys.length, "keys", () => {
      expanded.keys = !expanded.keys;
      paint();
    }));
  }
  nodes.push(proposalsRow(shown === MEMORY_PROPOSALS));
  if (accordion() && shown === MEMORY_PROPOSALS) nodes.push(inlineDetail(MEMORY_PROPOSALS));
  syncNodes(body, nodes);
  paintDetailPanel(accordion() ? null : shown);
}

function offText() {
  return [
    "Memory is off for this workspace: ",
    el("code", { text: spaces?.enabled_key || "crew_memory.enabled" }),
    " is false.",
  ];
}

function spaceRow(space, current) {
  const revision = space.revision == null ? "No revision yet" : `Rev ${space.revision}`;
  const open = Number(space.open_proposals) || 0;
  const caps = space.caps || {};
  const button = el("button", { class: "title", text: space.key, type: "button" });
  const row = el("div", { class: "knowledge-row memory-row", title: space.key }, [
    el("div", { class: "top" }, [
      el("span", { class: "id", text: revision }),
      el("span", { class: "spacer" }),
      space.created_at
        ? el("span", { class: "when", text: formatAge(space.created_at), title: `Last written ${formatDateTime(space.created_at)}` })
        : null,
      accordion() ? el("span", { class: "friction-row-toggle", text: current ? "▾" : "▸" }) : null,
    ]),
    button,
    el("div", { class: "meta" }, [
      el("span", {
        class: "memory-usage",
        text: `${space.file_count}/${caps.max_files} files · ${bytes(space.bytes)} / ${bytes(caps.max_bytes)} B`,
      }),
      space.writer ? el("span", { class: "dot", text: "·" }) : null,
      space.writer ? el("span", { text: writerLabel(space.writer) }) : null,
      open > 0 ? el("span", { class: "knowledge-pill open", text: `${open} open`, title: `${open} open global proposals from ${space.key}` }) : null,
    ]),
  ]);
  if (current) row.classList.add("active");
  makeRowDisclosure(row, button, accordion()
    ? { expanded: current, controls: inlineDetailId(space.key), onToggle: () => select(current ? null : space.key) }
    : { controls: "memory-detail", current, onToggle: () => select(space.key) });
  return keyed(row, `memory-key-${space.key}`, { space, current, accordion: accordion() });
}

function proposalsRow(current) {
  const open = Array.isArray(proposals?.open) ? proposals.open.length : 0;
  const button = el("button", { class: "title", text: "Global proposals", type: "button" });
  const row = el("div", { class: "knowledge-row memory-row memory-proposals-row" }, [
    el("div", { class: "top" }, [
      el("span", { class: "id", text: "global" }),
      el("span", { class: "spacer" }),
      accordion() ? el("span", { class: "friction-row-toggle", text: current ? "▾" : "▸" }) : null,
    ]),
    button,
    el("div", { class: "meta" }, [
      el("span", { class: `knowledge-pill ${open > 0 ? "open" : "closed"}`, text: `${open} open` }),
    ]),
  ]);
  if (current) row.classList.add("active");
  makeRowDisclosure(row, button, accordion()
    ? { expanded: current, controls: inlineDetailId(MEMORY_PROPOSALS), onToggle: () => select(current ? null : MEMORY_PROPOSALS) }
    : { controls: "memory-detail", current, onToggle: () => select(MEMORY_PROPOSALS) });
  return keyed(row, "memory-proposals", { open, current, accordion: accordion() });
}

function inlineDetailId(key) {
  return `memory-accordion-${key.replace(/[^A-Za-z0-9_-]/g, "_")}`;
}

function inlineDetail(key) {
  const section = el("section", { class: "friction-accordion-detail memory-accordion-detail", "aria-label": `Details for ${key}` });
  section.id = inlineDetailId(key);
  section.append(...detailNodes(key));
  return keyed(section, `memory-inline-${key}`, detailHash(key));
}

// ---- detail -------------------------------------------------------------------

function paintDetailPanel(key) {
  const panel = document.getElementById("memory-detail");
  const title = document.getElementById("memory-detail-title");
  if (title) title.textContent = key === MEMORY_PROPOSALS ? "Global proposals" : "Memory detail";
  if (!panel) return;
  if (!spaces?.enabled) {
    syncNodes(panel, [keyed(emptyState("No memory space to show."), "memory-detail-off", "off")]);
    return;
  }
  if (!key) {
    syncNodes(panel, [keyed(emptyState("No memory key selected."), "memory-detail-none", "none")]);
    return;
  }
  const wrap = el("div", { class: "memory-detail-wrap" }, detailNodes(key));
  syncNodes(panel, [keyed(wrap, `memory-detail-${key}`, detailHash(key))]);
}

function detailHash(key) {
  return key === MEMORY_PROPOSALS
    ? { key, proposals, revision: listRevision, all: expanded.proposals }
    : { key, detail: detailInVisit(key), all: expanded.history };
}

function detailNodes(key) {
  if (key === MEMORY_PROPOSALS) return proposalNodes();
  const current = detailInVisit(key);
  if (!current) return [emptyState("Loading…")];
  if (current.error) return [emptyState(current.error)];
  const space = current.space || {};
  const nodes = [];
  if (space.revision == null) {
    const text = key === "global" ? "Global memory has no revision yet." : "This model has no revision yet.";
    nodes.push(detailHead(key, []), emptyState(text));
    if (key === "global") {
      const jump = el("button", { class: "knowledge-btn", type: "button", text: "View global proposals" });
      jump.addEventListener("click", () => select(MEMORY_PROPOSALS));
      nodes.push(el("div", { class: "memory-actions" }, [jump]));
    }
  } else {
    nodes.push(detailHead(key, [
      `Revision ${space.revision}`,
      writerLabel(space.writer),
      formatDateTime(space.created_at),
    ]));
    nodes.push(el("div", { class: "knowledge-detail-body" }, [
      el("aside", { class: "knowledge-side" }, [revisionCard(space)]),
      el("div", { class: "knowledge-body memory-files" }, (space.files || []).map(memoryFile)),
    ]));
  }
  nodes.push(historySection(current.history));
  return nodes;
}

function detailHead(key, facts) {
  const sub = [];
  for (const fact of facts.filter(Boolean)) {
    if (sub.length) sub.push(el("span", { class: "dot", text: "·" }));
    sub.push(el("span", { text: fact }));
  }
  return el("div", { class: "knowledge-detail-head" }, [
    el("div", { class: "crumb" }, [el("span", { text: key === "global" ? "global memory" : "model memory" })]),
    el("h2", { text: key }),
    sub.length ? el("div", { class: "sub" }, sub) : null,
  ]);
}

function revisionCard(space) {
  const caps = space.caps || {};
  const rows = [
    fact("Digest", el("span", { class: "value mono", text: shortDigest(space.digest), title: space.digest || "" })),
    fact("Usage", el("span", { class: "value mono", text: `${space.file_count}/${caps.max_files} files · ${bytes(space.bytes)} / ${bytes(caps.max_bytes)} B` })),
  ];
  if (space.reason) rows.push(fact("Reason", collapsedText(space.reason)));
  rows.push(fact("Evidence", taskLinks(space.evidence)));
  return el("div", { class: "task-delivery-card knowledge-meta-card" }, rows);
}

function fact(label, value) {
  return el("div", { class: "property-row fact" }, [el("span", { class: "label", text: label }), value]);
}

// A long reason stays one line until it is opened (STD-07 §R14).
function collapsedText(text) {
  const firstLine = String(text).split("\n")[0];
  if (firstLine.length <= 80 && firstLine === text) return el("span", { class: "value", text });
  return el("details", { class: "value memory-reason" }, [
    el("summary", { text: firstLine.length > 80 ? `${firstLine.slice(0, 79)}…` : firstLine }),
    el("p", { text }),
  ]);
}

function taskLinks(ids) {
  const values = Array.isArray(ids) ? ids : [];
  const wrap = el("div", { class: "value knowledge-detail-list" });
  if (values.length === 0) {
    wrap.appendChild(el("span", { class: "knowledge-tag dim", text: "-" }));
    return wrap;
  }
  for (const id of values) {
    const link = el("a", { class: "knowledge-link-row", text: id, title: `Open task ${id}` });
    bindTaskLink(link, id);
    wrap.appendChild(link);
  }
  return wrap;
}

function memoryFile(file) {
  return el("section", { class: "memory-file" }, [
    el("h4", { class: "memory-file-path", text: file.path }),
    markdown(file.text),
  ]);
}

function markdown(text) {
  const rendered = renderMarkdown(text || "");
  if (rendered === null) return el("pre", { class: "friction-detail-body", text: text || "" });
  const view = el("div", { class: "markdown-body" });
  view.innerHTML = rendered;
  return view;
}

function historySection(history) {
  // The store returns events oldest first; newest first is by time, with the
  // later-recorded of two equal times first.
  const events = Array.isArray(history?.events)
    ? history.events.map((event, index) => ({ event, index, at: Date.parse(event.at) || 0 }))
      .sort((a, b) => b.at - a.at || b.index - a.index)
      .map(({ event }) => event)
    : [];
  const section = el("section", { class: "memory-history" }, [el("h4", { class: "section-title", text: "History" })]);
  if (events.length === 0) {
    section.appendChild(el("div", { class: "memory-history-empty", text: "No events recorded for this key." }));
    return section;
  }
  const list = el("ol", { class: "memory-events" });
  list.id = "memory-history-list";
  for (const event of expanded.history ? events : events.slice(0, MEMORY_ROWS_SHOWN)) {
    list.appendChild(el("li", { class: "memory-event" }, [
      el("time", { text: formatDateTime(event.at), title: formatDateTime(event.at, { seconds: true }) }),
      el("span", { class: "memory-event-label", text: EVENT_LABELS[event.action] || event.action }),
      el("span", { class: "memory-event-note", text: eventNote(event) }),
    ]));
  }
  section.appendChild(list);
  if (events.length > MEMORY_ROWS_SHOWN) {
    section.appendChild(showAllControl("memory-history-toggle", expanded.history, events.length, "events", () => {
      expanded.history = !expanded.history;
      paint();
    }, "memory-history-list"));
  }
  return section;
}

function eventNote(event) {
  const detail = event.detail || {};
  const parts = [];
  if (event.revision != null) parts.push(`rev ${event.revision}`);
  if (detail.proposal_id) parts.push(detail.proposal_id);
  if (detail.task_id) parts.push(detail.task_id);
  if (detail.reason && typeof detail.reason === "string") parts.push(detail.reason);
  else if (detail.message) parts.push(detail.message);
  if (event.actor) parts.push(event.actor);
  return parts.join(" · ");
}

// ---- proposals ----------------------------------------------------------------

function proposalNodes() {
  if (listRevision !== getWorkspaceRevision()) return [emptyState("Loading…")];
  const open = Array.isArray(proposals?.open) ? proposals.open : [];
  const closed = Array.isArray(proposals?.recently_closed) ? proposals.recently_closed : [];
  const nodes = [detailHead("global", [`${open.length} open`])];
  const list = el("div", { class: "memory-proposals" });
  list.id = "memory-proposal-list";
  if (open.length === 0) list.appendChild(emptyState("No open proposals. Dreaming runs open them for operator review."));
  for (const proposal of expanded.proposals ? open : open.slice(0, MEMORY_ROWS_SHOWN)) {
    list.appendChild(proposalItem(proposal));
  }
  nodes.push(list);
  if (open.length > MEMORY_ROWS_SHOWN) {
    nodes.push(showAllControl("memory-proposals-toggle", expanded.proposals, open.length, "proposals", () => {
      expanded.proposals = !expanded.proposals;
      paint();
    }, "memory-proposal-list"));
  }
  const group = el("details", { class: "memory-closed" }, [
    el("summary", { text: `Recently closed (${closed.length})` }),
    closed.length === 0
      ? el("div", { class: "memory-history-empty", text: "No proposals have closed yet." })
      : el("div", { class: "memory-proposals" }, closed.map(proposalItem)),
  ]);
  nodes.push(group);
  return nodes;
}

function proposalItem(proposal) {
  const closed = proposal.status !== "open";
  const dates = [`Opened ${formatDate(proposal.created_at)}`];
  // An expired proposal is closed at expires_at with no decision time.
  if (closed && proposal.decided_at) dates.push(`Closed ${formatDate(proposal.decided_at)}`);
  else if (proposal.status === "expired") dates.push(`Expired ${formatDate(proposal.expires_at)}`);
  else dates.push(`Expires ${formatDate(proposal.expires_at)}`);
  const summary = el("summary", {}, [
    el("span", { class: "memory-proposal-model", text: proposal.key }),
    el("strong", { class: "memory-proposal-title", text: proposalTitle(proposal.text) }),
    el("span", { class: "memory-proposal-meta" }, [
      closed ? el("span", { class: `knowledge-pill ${proposal.status}`, text: proposal.status }) : null,
      el("span", { text: dates.join(" · ") }),
    ]),
  ]);
  const body = [markdown(proposalBody(proposal.text))];
  if (closed && proposal.decision_reason) {
    body.push(el("div", { class: "memory-decision" }, [
      el("span", { class: "label", text: "Decision" }),
      el("span", { text: [proposal.decided_by, proposal.decision_reason].filter(Boolean).join(" · ") }),
    ]));
  }
  body.push(taskLinks(proposal.evidence));
  const item = el("details", { class: "memory-proposal" }, [summary, el("div", { class: "memory-proposal-body" }, body)]);
  item.dataset.id = proposal.id;
  return item;
}

/// A proposal's text without the leading heading its summary already shows.
function proposalBody(text) {
  return String(text || "").replace(/^\s*#{1,6}\s+[^\n]*\n*/, "");
}

/// A proposal's title: its first heading, or its first line.
function proposalTitle(text) {
  const lines = String(text || "").split("\n").map((line) => line.trim()).filter(Boolean);
  const heading = lines.find((line) => /^#{1,6}\s/.test(line));
  const line = (heading || lines[0] || "").replace(/^#{1,6}\s+/, "");
  return line.length > 120 ? `${line.slice(0, 119)}…` : line;
}

// ---- shared -------------------------------------------------------------------

function showAllControl(key, open, total, unit, onToggle, controls = null) {
  const button = el("button", {
    class: "knowledge-btn memory-show-all",
    type: "button",
    text: open ? "Show fewer" : `Show all ${total} ${unit}`,
    "aria-expanded": String(open),
  });
  if (controls) button.setAttribute("aria-controls", controls);
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    onToggle();
  });
  return keyed(el("div", { class: "memory-show-all-row" }, [button]), key, { open, total });
}

function emptyState(text) {
  return el("div", { class: "empty-state" }, [
    el("div", { class: "icon", text: "✧" }),
    el("div", { class: "text" }, Array.isArray(text) ? text : [text]),
  ]);
}

function keyed(node, key, hash) {
  node.dataset.key = key;
  node.dataset.hash = JSON.stringify(hash);
  return node;
}

function writerLabel(writer) {
  if (!writer) return "";
  if (writer.kind === "operator") return "operator";
  return writer.crew || writer.actor || "crew";
}

/// `sha256:` and the first 12 hex digits; the full digest is the hover text.
function shortDigest(digest) {
  if (!digest) return "-";
  const [algorithm, hex] = String(digest).split(":");
  return hex && hex.length > 12 ? `${algorithm}:${hex.slice(0, 12)}…` : digest;
}

function bytes(value) {
  return Number(value || 0).toLocaleString("en-US");
}
