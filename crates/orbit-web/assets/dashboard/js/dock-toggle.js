// Tasks dock collapse: Cmd+B (Ctrl+B off the Mac), the hide button in the
// dock's tab bar and the show button at the end of the filter bar slide the
// dock (#side-dock) out of view and back, so the task list takes the full
// width. `dock-collapsed` on #tasks-main is the settled state (tasks.css); the
// slide is a transform and opacity on the dock alone (`dock-leaving`,
// `dock-entering`), and the grid column changes once, after the slide out or
// before the slide in, so the dock's tab compaction never relays out per
// frame. The choice is remembered per browser. In the one-column Tasks layout
// the dock is a route of its own, so the shortcut does nothing there and the
// collapsed state has no effect (tasks.css guards it by the same breakpoint).

import { tasksOneColumn } from './log-tail.js';

const DOCK_COLLAPSED_PREFS_KEY = "orbit.dashboard.dockCollapsed";
// Matches the #side-dock transition in tasks.css, plus slack for a missed
// transitionend (a hidden tab, a style change mid-slide).
const DOCK_SLIDE_FALLBACK_MS = 260;

const onMac = /mac|iphone|ipad/i.test(navigator.userAgentData?.platform || navigator.platform || "");
const dockShortcutLabel = onMac ? "⌘B" : "Ctrl+B";

let dockCollapsed = false;
let cancelPendingSlide = null;

function loadDockCollapsedPref() {
  try {
    return window.localStorage.getItem(DOCK_COLLAPSED_PREFS_KEY) === "true";
  } catch {
    return false;
  }
}

function saveDockCollapsedPref(collapsed) {
  try {
    window.localStorage.setItem(DOCK_COLLAPSED_PREFS_KEY, String(collapsed));
  } catch {
    // Storage blocked: the choice lasts until the page reloads.
  }
}

function prefersReducedMotion() {
  return typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function syncDockToggleButtons() {
  const label = dockCollapsed ? "Show dock" : "Hide dock";
  for (const id of ["dock-collapse", "dock-expand"]) {
    const button = document.getElementById(id);
    if (!button) continue;
    button.setAttribute("aria-expanded", String(!dockCollapsed));
    button.setAttribute("aria-label", label);
    button.setAttribute("aria-description", dockShortcutLabel);
  }
}

// The dock is beside the list: the Tasks view is showing, both columns fit,
// and no task pane or task page has taken the dock's place.
function dockBesideList(main) {
  return Boolean(main) && main.getClientRects().length > 0 && !tasksOneColumn()
    && !main.classList.contains("task-pane-open") && !main.classList.contains("task-page-mode");
}

// Run `done` once the dock's slide out ends, or after the fallback delay.
function afterDockSlide(side, done) {
  let timer = 0;
  const finish = () => {
    side.removeEventListener("transitionend", onEnd);
    window.clearTimeout(timer);
    cancelPendingSlide = null;
    done();
  };
  const onEnd = (event) => {
    if (event.target === side && event.propertyName === "opacity") finish();
  };
  side.addEventListener("transitionend", onEnd);
  timer = window.setTimeout(finish, DOCK_SLIDE_FALLBACK_MS);
  cancelPendingSlide = () => {
    side.removeEventListener("transitionend", onEnd);
    window.clearTimeout(timer);
    cancelPendingSlide = null;
  };
}

function setDockCollapsed(collapsed) {
  const main = document.getElementById("tasks-main");
  const side = document.getElementById("side-dock");
  if (!main || !side || collapsed === dockCollapsed) return;
  cancelPendingSlide?.();
  const animate = dockBesideList(main) && !prefersReducedMotion();
  // Focus inside a dock that is about to hide moves to the show button.
  const focusLeavesDock = collapsed && side.contains(document.activeElement);
  const focusFromShow = !collapsed && document.activeElement === document.getElementById("dock-expand");
  dockCollapsed = collapsed;
  saveDockCollapsedPref(collapsed);
  syncDockToggleButtons();
  main.classList.remove("dock-leaving", "dock-entering");
  if (collapsed) {
    const settle = () => {
      main.classList.remove("dock-leaving");
      main.classList.add("dock-collapsed");
      if (focusLeavesDock) document.getElementById("dock-expand")?.focus();
    };
    if (!animate) {
      settle();
      return;
    }
    main.classList.add("dock-leaving");
    afterDockSlide(side, settle);
    return;
  }
  main.classList.remove("dock-collapsed");
  if (animate) {
    // The column is back; start the dock off to the side, commit that style,
    // then let it slide in.
    main.classList.add("dock-entering");
    void getComputedStyle(side).opacity;
    main.classList.remove("dock-entering");
  }
  if (focusFromShow) document.getElementById("dock-collapse")?.focus();
}

/// Show the dock beside the list for an explicit dock target (Open log,
/// `#tasks?dock=…`, `#notices`, Open Tasks dock). Above the one-column
/// breakpoint this clears a remembered collapse, writes that choice and
/// syncs both toggle buttons. At one column the dock is already its own
/// route, so the remembered choice stays. A no-op when the dock is already
/// beside the list.
export function showDockBesideList() {
  if (tasksOneColumn()) return;
  setDockCollapsed(false);
}

function onDockShortcut(event) {
  if (event.defaultPrevented || (event.key || "").toLowerCase() !== "b") return;
  const primary = onMac ? event.metaKey : event.ctrlKey;
  const other = onMac ? event.ctrlKey : event.metaKey;
  if (!primary || other || event.altKey || event.shiftKey) return;
  const target = event.target;
  if (target?.isContentEditable || target?.closest?.("input, textarea, select, [contenteditable], dialog[open]")) return;
  if (!dockBesideList(document.getElementById("tasks-main"))) return;
  event.preventDefault();
  if (event.repeat) return;
  setDockCollapsed(!dockCollapsed);
}

/// Restore the remembered dock state and wire the shortcut and both buttons.
export function initDockToggle() {
  const main = document.getElementById("tasks-main");
  if (!main) return;
  dockCollapsed = loadDockCollapsedPref();
  main.classList.toggle("dock-collapsed", dockCollapsed);
  syncDockToggleButtons();
  document.getElementById("dock-collapse")?.addEventListener("click", () => setDockCollapsed(true));
  document.getElementById("dock-expand")?.addEventListener("click", () => setDockCollapsed(false));
  document.addEventListener("keydown", onDockShortcut);
}
