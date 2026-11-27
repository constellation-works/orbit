// Navigation rail mode and collapse state across width tiers:
// - Wide (>=1316px): 200px labelled rail by default; collapsing switches to the
//   56px icon rail and moves the scope pickers to the header, remembered per
//   browser in localStorage.
// - Medium (761px..1315px): 56px icon rail by default with scope pickers in the
//   header; toggling expands the labelled rail as a transient overlay over the
//   content without shifting layout. Esc, an outside click or a resize that
//   leaves the medium band closes it; a resize within the band keeps it open.
// - Small (<=760px): bottom bar replaces the rail; the toggle is withdrawn.
const RAIL_COLLAPSED_PREFS_KEY = "orbit.dashboard.railCollapsed";
const PHONE_QUERY = "(max-width: 760px)";
const MEDIUM_QUERY = "(min-width: 761px) and (max-width: 1315px)";
const VIEWS_TABS = new Set(["diagnostics", "analytics", "operations", "config"]);

const modeListeners = new Set();
let currentState = {
  phone: false,
  medium: false,
  wideCollapsed: false,
  overlayOpen: false,
  iconMode: false,
  scopeInHeader: false,
};
let closeOverlayImpl = () => {};

function loadRailCollapsedPref() {
  try {
    return window.localStorage.getItem(RAIL_COLLAPSED_PREFS_KEY) === "true";
  } catch {
    return false;
  }
}

function saveRailCollapsedPref(collapsed) {
  try {
    window.localStorage.setItem(RAIL_COLLAPSED_PREFS_KEY, String(collapsed));
  } catch {
    // Storage blocked: the choice lasts until the page reloads.
  }
}

/// True while the desktop/medium rail shows icons without inline labels.
export function isRailIconMode() {
  return currentState.iconMode;
}

/// True while host and workspace pickers live in the header row.
export function isScopeInHeader() {
  return currentState.scopeInHeader;
}

/// True while the medium-width rail is temporarily expanded over content.
export function isRailOverlayOpen() {
  return currentState.overlayOpen;
}

/// Close the medium overlay rail if open.
export function closeRailOverlay(options) {
  closeOverlayImpl(options);
}

/// Subscribe to rail mode, overlay or scope-placement changes.
export function onRailModeChange(listener) {
  modeListeners.add(listener);
  return () => modeListeners.delete(listener);
}

function notifyModeListeners() {
  for (const listener of modeListeners) listener({ ...currentState });
}

/// Restore the navigation preference and wire its toggle, without a shortcut.
export function initRailToggle() {
  const rail = document.getElementById("navigation-rail");
  const button = document.getElementById("rail-toggle");
  const foot = rail?.querySelector(".rail-foot");
  const shell = rail?.closest(".shell");
  if (!shell || !button) return;
  const phone = window.matchMedia(PHONE_QUERY);
  const medium = window.matchMedia(MEDIUM_QUERY);
  let rememberedCollapsed = loadRailCollapsedPref();
  let overlayOpen = false;
  let toggleHadFocus = false;

  function syncTabAttributes(iconMode) {
    for (const tab of rail.querySelectorAll(".rail-group > .tab")) {
      const hasViews = VIEWS_TABS.has(tab.dataset.tab);
      if (tab.id !== "rail-drain") {
        if (iconMode) {
          const label = tab.querySelector(".rail-label")?.textContent?.trim();
          if (label) tab.title = label;
        } else {
          tab.removeAttribute("title");
        }
      }
      if (hasViews) {
        if (iconMode) {
          tab.setAttribute("aria-haspopup", "true");
          if (!tab.hasAttribute("aria-expanded")) tab.setAttribute("aria-expanded", "false");
        } else {
          tab.removeAttribute("aria-haspopup");
          tab.removeAttribute("aria-expanded");
        }
      }
    }
  }

  function applyRailState() {
    const isPhone = phone.matches;
    const isMedium = !isPhone && medium.matches;
    if (!isMedium) overlayOpen = false;
    const wideCollapsed = !isPhone && !isMedium && rememberedCollapsed;
    const iconMode = !isPhone && (wideCollapsed || (isMedium && !overlayOpen));
    const scopeInHeader = isPhone || isMedium || wideCollapsed;

    currentState = {
      phone: isPhone,
      medium: isMedium,
      wideCollapsed,
      overlayOpen: isMedium && overlayOpen,
      iconMode,
      scopeInHeader,
    };

    shell.classList.toggle("rail-collapsed", wideCollapsed);
    shell.classList.toggle("rail-icon-mode", iconMode);
    shell.classList.toggle("rail-overlay-open", currentState.overlayOpen);

    button.setAttribute("aria-expanded", String(!iconMode));
    button.setAttribute("aria-label", iconMode ? "Show navigation" : "Hide navigation");

    if (foot) {
      if (iconMode && foot.contains(document.activeElement)) button.focus();
      foot.inert = iconMode;
    }

    syncTabAttributes(iconMode);

    if (isPhone && (document.activeElement === button || toggleHadFocus)) {
      requestAnimationFrame(() => {
        if (phone.matches) {
          const target = document.querySelector(".phone-nav [aria-current='page']") || document.getElementById("phone-more-open");
          target?.focus();
        }
      });
      toggleHadFocus = false;
    }

    notifyModeListeners();
  }

  closeOverlayImpl = ({ restoreFocus = false } = {}) => {
    if (!overlayOpen) return;
    overlayOpen = false;
    applyRailState();
    if (restoreFocus && !phone.matches) button.focus();
  };

  button.addEventListener("focus", () => { toggleHadFocus = true; });
  button.addEventListener("blur", () => {
    // CSS can hide and blur the button before the media-query callback.
    // Retain that focus only until the phone layout chooses a visible target.
    if (!phone.matches) toggleHadFocus = false;
  });

  button.addEventListener("click", () => {
    if (phone.matches) return;
    if (medium.matches) {
      overlayOpen = !overlayOpen;
      applyRailState();
      return;
    }
    rememberedCollapsed = !rememberedCollapsed;
    saveRailCollapsedPref(rememberedCollapsed);
    applyRailState();
  });

  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || event.defaultPrevented || !overlayOpen) return;
    const hostDetail = document.getElementById("host-detail");
    if (hostDetail && !hostDetail.hidden) return;
    if (event.target?.closest?.("dialog[open], [role='dialog']:not([hidden])")) return;
    event.preventDefault();
    closeRailOverlay({ restoreFocus: true });
  });

  document.addEventListener("pointerdown", (event) => {
    if (!overlayOpen) return;
    if (rail.contains(event.target)) return;
    const hostDetail = document.getElementById("host-detail");
    if (hostDetail && !hostDetail.hidden && hostDetail.contains(event.target)) return;
    closeRailOverlay();
  });

  applyRailState();
  phone.addEventListener("change", applyRailState);
  medium.addEventListener("change", applyRailState);
}
