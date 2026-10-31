// Orbit dashboard: the warnings box above the Tasks dock's tabs [ORB-20446].
// Pure vanilla JS, split into ES modules with no build step.
//
// One list, newest first, merges two sources: the live drain window's
// submission warnings and notes (the readiness payload's
// `capacity.submission_notices`, which operations-drain.js hands over on every
// render) and the Log's warning lines (log-tail.js hands over its rows on every
// filter pass). The Log tab's warnings badge reads its count from here, so the
// two always agree. The box is hidden while the list is empty.

import { el, formatClock, formatDateTime, onClockZoneChange } from './common.js';

const $ = (id) => document.getElementById(id);

let submission = null;
let logWarnings = [];
const listeners = new Set();

// The live window's recorded notices, or null when no window is live or its
// submission reported none. Unchanged payloads repaint nothing.
export function setSubmissionNotices(record) {
  const next = record && typeof record === "object" && (record.warnings?.length || record.notes?.length) ? record : null;
  if (JSON.stringify(next) === JSON.stringify(submission)) return;
  submission = next;
  renderDockWarnings();
}

// The Log's warning lines: `{ ts, source, message, reveal }`, where `reveal`
// selects the Log tab and brings that line into view.
export function setLogWarnings(entries) {
  logWarnings = Array.isArray(entries) ? entries : [];
  renderDockWarnings();
}

// How many warning rows the box shows (notes are not warnings).
export function dockWarningCount() {
  return dockWarningEntries().filter((entry) => entry.level === "warn").length;
}

// Called after every repaint, so the Log badge follows submission changes too.
export function onDockWarningsChange(callback) {
  listeners.add(callback);
}

function dockWarningEntries() {
  const entries = [];
  if (submission) {
    const at = submission.recorded_at || "";
    for (const message of submission.warnings || []) entries.push({ level: "warn", ts: at, source: "Start", message });
    for (const message of submission.notes || []) entries.push({ level: "note", ts: at, source: "Start", message });
  }
  for (const entry of logWarnings) entries.push({ level: "warn", ...entry });
  // Newest first; a stable sort keeps one submission's warnings above its notes.
  const time = (entry) => Date.parse(entry.ts || "") || 0;
  return entries.sort((left, right) => time(right) - time(left));
}

function warningTime(ts) {
  const d = new Date(ts || "");
  if (isNaN(d.getTime())) return { text: "", title: "" };
  return { text: formatClock(d, { zone: false }), title: formatDateTime(d, { seconds: true }) };
}

// Stable identity for one rendered row. `|` is safe as a joiner because every
// part is encoded, so two entries share a key exactly when they show the same
// warning; focus and expansion follow the key across repaints.
function warningKey(entry) {
  return [entry.level, entry.ts, entry.source, entry.message]
    .map((part) => encodeURIComponent(part || ""))
    .join("|");
}

function renderRow(entry) {
  const warn = entry.level === "warn";
  const row = el("li", { class: `dock-warning ${warn ? "warn" : "note"}` });
  row.dataset.warningKey = warningKey(entry);
  const { text, title } = warningTime(entry.ts);
  const source = entry.reveal
    ? el("button", { type: "button", class: "dock-warning-source link", text: entry.source || "Log", title: "Show this line in the Log" })
    : el("span", { class: "dock-warning-source", text: entry.source || "", title: "Reported when this drain window started" });
  if (entry.reveal) {
    source.addEventListener("click", (event) => {
      event.stopPropagation();
      entry.reveal();
    });
  }
  row.append(
    el("span", { class: "dock-warning-icon", text: warn ? "⚠" : "ⓘ", "aria-label": warn ? "Warning" : "Note", role: "img" }),
    el("span", { class: "dock-warning-time", text, title }),
    source,
    el("span", { class: "dock-warning-message", text: entry.message || "" }),
  );
  row.title = entry.message || "";
  row.tabIndex = 0;
  row.setAttribute("aria-expanded", "false");
  const toggle = () => row.setAttribute("aria-expanded", String(row.classList.toggle("expanded")));
  row.addEventListener("click", toggle);
  row.addEventListener("keydown", (event) => {
    if (event.target !== row || (event.key !== "Enter" && event.key !== " ")) return;
    event.preventDefault();
    toggle();
  });
  return row;
}

function renderDockWarnings() {
  const box = $("dock-warnings");
  if (box) {
    const entries = dockWarningEntries();
    // A repaint keeps the rows the operator opened, keyed by warning identity.
    const open = new Set(
      [...box.querySelectorAll(".dock-warning.expanded")].map((row) => row.dataset.warningKey || row.title),
    );
    // Keyboard focus lives on a row or its Log action, and clearing the box
    // drops it to the page body; hand it to the rebuilt counterpart instead.
    const active = document.activeElement;
    const focusedRow = active instanceof Element ? active.closest(".dock-warning") : null;
    const focusInBox = focusedRow && box.contains(focusedRow)
      ? {
        key: focusedRow.dataset.warningKey || focusedRow.title,
        onAction: active !== focusedRow,
        index: [...box.children].indexOf(focusedRow),
      }
      : null;
    box.textContent = "";
    for (const entry of entries) {
      const row = renderRow(entry);
      if (open.has(row.dataset.warningKey)) {
        row.classList.add("expanded");
        row.setAttribute("aria-expanded", "true");
      }
      box.append(row);
    }
    box.hidden = entries.length === 0;
    if (focusInBox) {
      const survivor = [...box.children].find((row) => row.dataset.warningKey === focusInBox.key);
      if (survivor) {
        const target = focusInBox.onAction
          ? survivor.querySelector("button.dock-warning-source") || survivor
          : survivor;
        target.focus({ preventScroll: true });
      } else if (box.children.length > 0) {
        // The focused warning is gone: stay nearby on the row that took its place.
        box.children[Math.min(focusInBox.index, box.children.length - 1)].focus({ preventScroll: true });
      } else {
        // No warnings remain and the box hides: fall back to the dock's selected tab.
        document.querySelector('#dock-mode-toggle .dock-seg.on')?.focus({ preventScroll: true });
      }
    }
  }
  for (const callback of listeners) callback();
}

onClockZoneChange(renderDockWarnings);
