// Orbit dashboard task history timeline.

import { el, statusPill, isHttpUrl, copyWithFeedback, formatClock, formatDate, formatDayHeading, makeDisclosure, onClockZoneChange } from './common.js';
import * as taskShared from './task-shared.js';

// ORB-17931: the History tab as a timeline. Entries are grouped under a day
// heading, each row names its actor, a sentence built from the entry's fields
// and the raw event name (so it can still be grepped). A note that parses as
// JSON becomes a key/value details panel behind a disclosure instead of
// raw text. Display only: the entries are the API's, unchanged.
const HISTORY_CATEGORIES = [
  { key: "lifecycle", label: "Lifecycle" },
  { key: "context", label: "Context" },
  { key: "crew", label: "Crew" },
  { key: "run", label: "Runs" },
  { key: "warning", label: "Warnings" },
  { key: "other", label: "Other" },
];

const HISTORY_EVENT_CATEGORY = {
  created: "lifecycle",
  status_changed: "lifecycle",
  started: "lifecycle",
  proposal_approved: "lifecycle",
  review_approved: "lifecycle",
  operator_requeued: "lifecycle",
  context_creation_authorized: "context",
  context_files_pruned: "context",
  context_files_restored: "context",
  context_files_widened: "context",
  crew_assigned: "crew",
  crew_redrawn: "crew",
  claimed: "run",
  run_finished: "run",
  pull_request: "run",
  landing_completed: "run",
  transient_infra_requeued: "warning",
  escalated: "warning",
};

// Two entries of one event with the same payload recorded this close together
// are one write seen twice (the task's owner and the system both log a grant).
const HISTORY_MERGE_WINDOW_MS = 5000;

function historyText(text) {
  return document.createTextNode(text);
}

// The note as text, and as a JSON object or array when it is one.
function parseHistoryNote(note) {
  const text = typeof note === "string" ? note.trim() : "";
  if (text && (text[0] === "{" || text[0] === "[")) {
    try {
      const json = JSON.parse(text);
      if (json && typeof json === "object") return { text, json };
    } catch (_) {
      // Not JSON after all; the note reads as text.
    }
  }
  return { text, json: null };
}

function historyStatusFlow(entry) {
  if (!entry.from_status || !entry.to_status) return null;
  return el("span", { class: "history-flow" }, [
    statusPill(entry.from_status),
    el("span", { class: "history-arrow", text: "→", "aria-label": "to" }),
    statusPill(entry.to_status),
  ]);
}

// "assigned crew `opus` from pool:hard": the crew and, when named, its pool.
// The wording of a note is not a contract, so a note that does not read this
// way stays visible as text.
function historyCrewParts(verb, text) {
  const match = /crew\s+`?([\w.:-]+)`?(?:\s+from\s+`?([\w.:/-]+)`?)?/i.exec(text);
  if (!match) return { parts: [`${verb} a crew`] };
  const parts = [`${verb} crew `, el("strong", { text: match[1] })];
  if (match[2]) parts.push(` from ${match[2]}`);
  const rest = text.replace(match[0], "").replace(/[\s.]/g, "");
  return rest ? { parts, note: text } : { parts, note: "" };
}

function historyPullRequestParts(text) {
  const number = /\/pull\/(\d+)|#(\d+)/.exec(text);
  if (!number) return { parts: ["opened a pull request"] };
  const label = `#${number[1] || number[2]}`;
  const url = /https?:\/\/\S+/.exec(text);
  const target = url && isHttpUrl(url[0]) ? el("a", { text: label }) : el("strong", { text: label });
  if (url && isHttpUrl(url[0])) {
    target.href = url[0];
    target.target = "_blank";
    target.rel = "noopener noreferrer";
  }
  const rest = text.replace(url ? url[0] : number[0], "").replace(/[\s.#\d]/g, "");
  return { parts: ["opened pull request ", target], note: rest ? text : "" };
}

function historyCountLabel(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

// The structured payload of an entry: a JSON note, else its typed decision.
function historyStructured(entry, parsed) {
  if (parsed.json) return parsed.json;
  const decision = entry.decision;
  return decision && typeof decision === "object" ? decision : null;
}

function historyJsonSummary(data) {
  if (Array.isArray(data)) return historyCountLabel(data.length, "item");
  const pairs = Object.entries(data)
    .filter(([, value]) => value == null || typeof value !== "object")
    .slice(0, 2)
    .map(([key, value]) => {
      const text = String(value);
      return `${key}: ${text.length > 40 ? `${text.slice(0, 40)}…` : text}`;
    });
  return pairs.length ? pairs.join(" · ") : historyCountLabel(Object.keys(data).length, "field");
}

// The sentence after the actor, as `{ parts, note, fallback }`. `note` is the
// text still to show under the sentence; absent, the note's own text shows.
function describeHistoryEntry(entry, parsed, data) {
  const text = parsed.json ? "" : parsed.text;
  switch (entry.event) {
    case "created":
      return { parts: ["created the task"] };
    case "status_changed": {
      const flow = historyStatusFlow(entry);
      return flow ? { parts: ["changed status ", flow] } : { parts: ["changed status"] };
    }
    case "started": {
      const flow = historyStatusFlow(entry);
      return flow ? { parts: ["started work ", flow] } : { parts: ["started work"] };
    }
    case "claimed":
      return { parts: ["claimed the task"] };
    case "crew_assigned":
      return historyCrewParts("assigned", text);
    case "crew_redrawn":
      return historyCrewParts("redrew", text);
    case "proposal_approved":
      return { parts: ["approved the proposal"] };
    case "review_approved":
      return { parts: ["approved the review"] };
    case "context_creation_authorized": {
      if (!data || !Array.isArray(data.selectors)) return { parts: ["authorized context creation"] };
      if (data.selectors.length === 0) return { parts: ["revoked context creation authorization"] };
      const parts = ["authorized context: ", el("strong", { text: historyCountLabel(data.selectors.length, "selector") })];
      if (data.generation) parts.push(` · generation ${data.generation}`);
      return { parts, summarized: true };
    }
    case "context_files_pruned":
    case "context_files_restored":
    case "context_files_widened": {
      const verb = entry.event.slice("context_files_".length);
      const selectors = data && Array.isArray(data.selectors) ? data.selectors : null;
      if (!selectors) return { parts: [`${verb} context files`] };
      return { parts: [`${verb} context: `, el("strong", { text: historyCountLabel(selectors.length, "selector") })], summarized: true };
    }
    case "run_finished":
      return { parts: ["finished a run"] };
    case "pull_request":
      return historyPullRequestParts(text);
    case "landing_completed":
      return { parts: ["completed landing"] };
    case "operator_requeued":
      return { parts: ["requeued the task"] };
    case "transient_infra_requeued":
      return { parts: ["requeued the task after a transient infrastructure failure"] };
    case "escalated":
      return { parts: ["escalated the task"] };
    default:
      return { parts: [entry.event || "(no event)"], fallback: true };
  }
}

// Consecutive entries of one event carrying an identical payload, recorded by
// different actors within a few seconds, are one group.
function groupHistoryEntries(entries) {
  const groups = [];
  for (const item of entries) {
    const group = groups[groups.length - 1];
    const last = group && group.items[group.items.length - 1];
    const same = last
      && last.entry.event === item.entry.event
      && last.entry.from_status === item.entry.from_status
      && last.entry.to_status === item.entry.to_status
      && item.parsed.text !== ""
      && last.parsed.text === item.parsed.text
      && !group.items.some((member) => member.entry.by === item.entry.by)
      && Math.abs(item.time - last.time) <= HISTORY_MERGE_WINDOW_MS;
    if (same) group.items.push(item);
    else groups.push({ items: [item], category: item.category });
  }
  return groups;
}

function historyValueNode(value) {
  if (Array.isArray(value)) {
    return el("div", { class: "history-kv-list" }, value.map((item) =>
      el("div", { class: "history-kv-item", text: typeof item === "string" ? item : JSON.stringify(item) })));
  }
  if (value && typeof value === "object") return el("span", { text: JSON.stringify(value) });
  return el("span", { text: value == null ? "null" : String(value) });
}

function buildHistoryDetails(group, data, context, id) {
  const first = group.items[0];
  const list = el("dl", { class: "history-kv" });
  const add = (key, valueNode) => {
    list.appendChild(el("dt", { text: key }));
    list.appendChild(el("dd", {}, [valueNode]));
  };
  if (data && !Array.isArray(data)) {
    for (const [key, value] of Object.entries(data)) add(key, historyValueNode(value));
  } else if (data) {
    add("items", historyValueNode(data));
  }
  add(group.items.length > 1 ? `recorded ×${group.items.length}` : "recorded", el("div", { class: "history-kv-list" },
    group.items.map((item) => el("div", { class: "history-kv-item" }, [
      el("strong", { text: item.entry.by || "?" }),
      historyText(" · "),
      taskShared.absTimeNode(context, item.entry.at),
    ]))));
  const details = el("div", { class: "history-details", id }, [list]);
  if (data) {
    const raw = first.parsed.json ? first.parsed.text : JSON.stringify(data);
    const copy = el("button", { class: "history-copy", text: "Copy raw JSON", title: "Copy the recorded payload" });
    copy.type = "button";
    copy.addEventListener("click", (event) => {
      event.stopPropagation();
      copyWithFeedback(copy, raw);
    });
    details.appendChild(el("div", { class: "history-details-actions" }, [copy]));
  }
  return details;
}

function buildHistoryRow(task, context, group, expandedKeys) {
  const first = group.items[0];
  const entry = first.entry;
  const data = historyStructured(entry, first.parsed);
  const described = describeHistoryEntry(entry, first.parsed, data);
  const category = HISTORY_CATEGORIES.find((candidate) => candidate.key === group.category);

  const sentence = el("span", { class: "history-sentence" }, [
    el("span", { class: "actor", text: entry.by || "?" }),
    historyText(described.fallback ? ": " : " "),
  ]);
  for (const part of described.parts) sentence.appendChild(typeof part === "string" ? historyText(part) : part);
  if (group.items.length > 1) {
    sentence.appendChild(el("span", {
      class: "history-merge",
      text: `×${group.items.length}`,
      title: `Recorded ${group.items.length} times by ${group.items.map((item) => item.entry.by || "?").join(", ")}`,
    }));
  }

  // A payload shows as a short summary; its fields wait in the details panel.
  const body = el("div", { class: "history-main" }, [sentence]);
  const note = described.note !== undefined ? described.note : (data ? "" : first.parsed.text);
  if (data && !described.summarized) {
    body.appendChild(el("span", { class: "history-summary", text: historyJsonSummary(data) }));
  }
  if (note) body.appendChild(el("span", { class: "history-note", text: note }));

  const head = el("div", { class: "history-head" }, [
    body,
    el("span", { class: "history-event", text: entry.event || "?" }),
  ]);
  const row = el("div", { class: "history-line" });
  row.dataset.category = group.category;
  const time = el("span", { class: "history-time", text: formatClock(entry.at, { zone: false }), title: taskShared.fmtAbsTimeValue(context, entry.at) });
  const node = el("span", { class: "history-node", title: category ? category.label : "" }, [el("span", { class: "history-dot" })]);
  node.setAttribute("aria-hidden", "true");
  row.append(time, node);

  const content = el("div", { class: "history-content" }, [head]);
  row.appendChild(content);

  const expandable = Boolean(data) || group.items.length > 1;
  if (expandable) {
    const key = `${task.id}:${first.index}`;
    const id = `history-${task.id}-${first.index}`.replace(/[^\w-]/g, "-");
    const details = buildHistoryDetails(group, data, context, id);
    const toggle = el("button", { class: "history-toggle", text: "Details" });
    const setOpen = (open) => {
      details.hidden = !open;
      toggle.setAttribute("aria-expanded", String(open));
      row.classList.toggle("expanded", open);
      if (open) expandedKeys.add(key);
      else expandedKeys.delete(key);
    };
    makeDisclosure(head, toggle, {
      expanded: false,
      controls: id,
      onToggle: (event) => {
        // A click on text being selected, or on a link, is not a toggle.
        if (event && event.target && event.target.closest && event.target.closest("a")) return;
        setOpen(details.hidden);
      },
    });
    body.appendChild(toggle);
    content.appendChild(details);
    setOpen(expandedKeys.has(key));
  }
  return row;
}

// Every meaningful history entry as a timeline, newest first by default.
export function buildTaskHistory(task, context) {
  // ORB-10311: legacy bare `commented` stubs are comment noise; the thread has
  // its own tab.
  const meaningful = Array.isArray(task.history) ? task.history.filter((h) => h && h.event !== "commented") : [];
  if (meaningful.length === 0) {
    return { count: 0, node: el("p", { class: "field-empty", text: "No history recorded." }) };
  }
  const items = meaningful.map((entry, index) => ({
    entry,
    index,
    time: Date.parse(entry.at),
    parsed: parseHistoryNote(entry.note),
    category: HISTORY_EVENT_CATEGORY[entry.event] || "other",
  }));
  // Recorded order is chronological; an unreadable time keeps the recorded
  // order rather than guessing.
  if (items.every((item) => Number.isFinite(item.time))) items.sort((a, b) => a.time - b.time || a.index - b.index);
  const groups = groupHistoryEntries(items);
  const counts = { all: items.length };
  for (const item of items) counts[item.category] = (counts[item.category] || 0) + 1;

  const view = { filter: "all", newestFirst: true };
  const expandedKeys = new Set();
  const timeline = el("div", { class: "history-timeline" });
  const chips = [];
  const orders = [];

  const render = () => {
    const shown = groups.filter((group) => view.filter === "all" || group.category === view.filter);
    if (view.newestFirst) shown.reverse();
    const days = [];
    for (const group of shown) {
      const at = group.items[0].entry.at;
      const day = formatDate(at);
      let section = days[days.length - 1];
      if (!section || section.day !== day) {
        section = { day, heading: formatDayHeading(at), rows: [] };
        days.push(section);
      }
      section.rows.push(group);
    }
    timeline.replaceChildren(...days.map((section) => el("div", { class: "history-day" }, [
      el("div", { class: "history-day-head", text: section.heading }),
      ...section.rows.map((group) => buildHistoryRow(task, context, group, expandedKeys)),
    ])));
  };

  const choose = (buttons, activeOf) => {
    for (const button of buttons) button.setAttribute("aria-pressed", String(activeOf(button)));
  };
  const chip = (key, label) => {
    const button = el("button", { class: "history-chip" }, [
      historyText(label),
      el("span", { class: "history-chip-count", text: String(counts[key] || 0) }),
    ]);
    button.type = "button";
    button.dataset.filter = key;
    button.addEventListener("click", () => {
      view.filter = key;
      choose(chips, (candidate) => candidate.dataset.filter === view.filter);
      render();
    });
    chips.push(button);
    return button;
  };
  const order = (newestFirst, label) => {
    const button = el("button", { class: "history-order", text: label });
    button.type = "button";
    button.dataset.order = newestFirst ? "newest" : "oldest";
    button.addEventListener("click", () => {
      view.newestFirst = newestFirst;
      choose(orders, (candidate) => candidate.dataset.order === (view.newestFirst ? "newest" : "oldest"));
      render();
    });
    orders.push(button);
    return button;
  };

  const filters = el("div", { class: "history-filters", role: "group", "aria-label": "Filter history by category" }, [
    chip("all", "All"),
    ...HISTORY_CATEGORIES.filter((category) => counts[category.key]).map((category) => chip(category.key, category.label)),
  ]);
  const sort = el("div", { class: "history-order-toggle", role: "group", "aria-label": "Order" }, [
    order(true, "Newest"),
    order(false, "Oldest"),
  ]);
  choose(chips, (candidate) => candidate.dataset.filter === view.filter);
  choose(orders, (candidate) => candidate.dataset.order === "newest");

  const wrap = el("div", { class: "history-list" }, [
    el("div", { class: "history-toolbar" }, [filters, sort]),
    timeline,
  ]);
  render();
  // Day headings and times follow the dashboard clock: repaint on a switch.
  const stopClock = onClockZoneChange(render);
  wrap.teardown = stopClock;
  return { count: meaningful.length, node: wrap };
}
