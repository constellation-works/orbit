// The Tasks quick filter: a double Shift opens a floating, draggable bar whose
// text narrows the rows the board already loaded. Nothing here reads the server
// or persists: closing the bar drops the query and the board returns to its
// normal list. The top-bar search (global-search.js) is separate: it queries
// the server and navigates; this only narrows the board in place.
import { el, escCloseButton, onWorkspaceChange } from './common.js';
import { $, rerender, taskState } from './task-shared.js';
import { taskPullRequestNumber } from './task-agent-actions.js';

// Two Shift keydowns this close together, with no other key between, open or
// close the bar.
const DOUBLE_SHIFT_MS = 300;
const PHONE_QUERY = "(max-width: 760px)";

let query = "";
let bar = null;
let input = null;
let countNode = null;
let opener = null;

export function quickFilterActive() {
  return query.trim() !== "";
}

// Whether the filter has any words: `quickFilterMatches` passes every task
// while it has none, so the board is exactly its normal list.
function queryWords() {
  return query.toLowerCase().split(/\s+/).filter(Boolean);
}

function startsWith(value, word) {
  return typeof value === "string" && value.toLowerCase().startsWith(word);
}

function contains(value, word) {
  return typeof value === "string" && value.toLowerCase().includes(word);
}

// The execution machine's name and id: a row shows the name, a task linked to
// a machine with no name only has the id.
function hostNames(task) {
  const machine = task.job_run_machine;
  if (!machine || typeof machine !== "object") return [];
  return [machine.machine_name, machine.machine_id].filter((value) => typeof value === "string" && value);
}

// Id, PR number and job run id match from the start of the value; title,
// branch, host and tag values anywhere in it.
function wordMatchesTask(task, word) {
  const prNumber = taskPullRequestNumber(task).replace(/^#/, "");
  const prWord = word.replace(/^#/, "");
  if (startsWith(task.id, word) || startsWith(task.job_run_id, word)) return true;
  if (prNumber && prWord && prNumber.startsWith(prWord)) return true;
  if (contains(task.title, word) || contains(task.job_run_branch, word)) return true;
  if (hostNames(task).some((host) => contains(host, word))) return true;
  return Array.isArray(task.tags) && task.tags.some((tag) => contains(tag, word));
}

// Every space-separated word has to match some field.
export function quickFilterMatches(task) {
  const words = queryWords();
  return words.every((word) => wordMatchesTask(task, word));
}

// The board reports how many of its loaded rows the filter keeps.
export function syncQuickFilterCount(shown, loaded) {
  if (countNode) countNode.textContent = `${shown} of ${loaded} shown`;
}

function boardVisible() {
  const pane = document.querySelector(".tab-pane.active");
  const body = $("tasks-body");
  return Boolean(pane && pane.dataset.tab === "tasks" && body && body.getClientRects().length > 0);
}

function editableTarget(target) {
  return Boolean(target && (target.isContentEditable || (target.closest && target.closest("input, textarea, select"))));
}

function phoneLayout() {
  try {
    return window.matchMedia(PHONE_QUERY).matches;
  } catch (_) {
    return false;
  }
}

function paintFilter() {
  if (taskState.paneContext) rerender(taskState.paneContext);
}

function buildSearchIcon() {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 20 20");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.7");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("aria-hidden", "true");
  svg.classList.add("quick-filter-icon");
  const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
  circle.setAttribute("cx", "9");
  circle.setAttribute("cy", "9");
  circle.setAttribute("r", "5.5");
  const handle = document.createElementNS("http://www.w3.org/2000/svg", "path");
  handle.setAttribute("d", "M13.2 13.2 17 17");
  svg.append(circle, handle);
  return svg;
}

function keepOnScreen() {
  if (!bar || !bar.style.left) return;
  const rect = bar.getBoundingClientRect();
  const left = Math.min(Math.max(0, rect.left), Math.max(0, window.innerWidth - rect.width));
  const top = Math.min(Math.max(0, rect.top), Math.max(0, window.innerHeight - rect.height));
  bar.style.left = `${left}px`;
  bar.style.top = `${top}px`;
}

// Dragging moves the bar by its own surface (not the input or the close
// button), with the pointer captured so it follows outside the bar, and keeps
// the whole bar inside the viewport. The one-column layout pins it instead.
function wireDrag(node) {
  node.addEventListener("pointerdown", (event) => {
    if (event.button !== 0 || phoneLayout() || event.target.closest("input, button")) return;
    const rect = node.getBoundingClientRect();
    const grabX = event.clientX - rect.left;
    const grabY = event.clientY - rect.top;
    node.style.left = `${rect.left}px`;
    node.style.top = `${rect.top}px`;
    node.classList.add("dragging");
    node.setPointerCapture(event.pointerId);
    const move = (next) => {
      const width = node.offsetWidth;
      const height = node.offsetHeight;
      node.style.left = `${Math.min(Math.max(0, next.clientX - grabX), Math.max(0, window.innerWidth - width))}px`;
      node.style.top = `${Math.min(Math.max(0, next.clientY - grabY), Math.max(0, window.innerHeight - height))}px`;
    };
    const end = () => {
      node.classList.remove("dragging");
      node.removeEventListener("pointermove", move);
      node.removeEventListener("pointerup", end);
      node.removeEventListener("pointercancel", end);
    };
    node.addEventListener("pointermove", move);
    node.addEventListener("pointerup", end);
    node.addEventListener("pointercancel", end);
    event.preventDefault();
  });
}

function buildBar() {
  input = el("input", {
    class: "quick-filter-input",
    type: "text",
    placeholder: "Filter tasks on the board",
    autocomplete: "off",
    spellcheck: "false",
    "aria-label": "Filter tasks on the board by ID, title, PR, branch, host, tag or run",
  });
  input.addEventListener("input", () => {
    query = input.value;
    paintFilter();
  });
  countNode = el("span", { class: "quick-filter-count", role: "status", "aria-live": "polite" });
  const close = escCloseButton("Close quick filter (Esc)", { onClick: closeQuickFilter });
  bar = el("div", { class: "quick-filter", role: "search" }, [buildSearchIcon(), input, countNode, close]);
  wireDrag(bar);
  document.body.appendChild(bar);
}

export function openQuickFilter() {
  if (bar || !boardVisible()) return;
  opener = document.activeElement;
  buildBar();
  paintFilter();
  input.focus();
}

export function closeQuickFilter() {
  if (!bar) return;
  bar.remove();
  bar = null;
  input = null;
  countNode = null;
  const wasFiltering = quickFilterActive();
  query = "";
  const previous = opener;
  opener = null;
  if (wasFiltering) paintFilter();
  if (previous && previous !== document.body && document.contains(previous) && typeof previous.focus === "function") {
    previous.focus({ preventScroll: true });
  }
}

// Wires the double Shift, Esc and the follow-ups once, at page start.
export function wireQuickFilter() {
  let lastShiftAt = 0;
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Shift") {
      lastShiftAt = 0;
      return;
    }
    if (event.repeat || event.isComposing || event.altKey || event.ctrlKey || event.metaKey) return;
    const now = performance.now();
    const second = lastShiftAt > 0 && now - lastShiftAt <= DOUBLE_SHIFT_MS;
    lastShiftAt = second ? 0 : now;
    if (!second) return;
    if (bar) {
      closeQuickFilter();
    } else if (!editableTarget(document.activeElement) && !document.querySelector("dialog[open]")) {
      openQuickFilter();
    }
  }, true);
  // Capture phase and stopped here: with a task panel open, the first Esc
  // closes only the bar and the panel's own handler never sees it.
  document.addEventListener("keydown", (event) => {
    if (!bar || event.key !== "Escape" || event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.target.closest && event.target.closest("dialog[open]")) return;
    event.preventDefault();
    event.stopPropagation();
    closeQuickFilter();
  }, true);
  // Leaving the board (another tab, a task page) closes the bar; selecting a
  // row changes the hash too but keeps the board, and with it the bar.
  window.addEventListener("hashchange", () => {
    setTimeout(() => { if (bar && !boardVisible()) closeQuickFilter(); }, 0);
  });
  window.addEventListener("resize", () => {
    if (!bar) return;
    if (phoneLayout()) {
      bar.style.left = "";
      bar.style.top = "";
    } else {
      keepOnScreen();
    }
  });
  onWorkspaceChange(closeQuickFilter);
}
