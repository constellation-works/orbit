// The navigation keeps an expand button in a narrow strip. Like the Tasks
// dock, its contents slide before the grid column changes once; the two
// preferences are independent. On phones the rail is a top strip instead.
const RAIL_COLLAPSED_PREFS_KEY = "orbit.dashboard.railCollapsed";
const RAIL_SLIDE_FALLBACK_MS = 260;

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

/// Restore the navigation preference and wire its button, without a shortcut.
export function initRailToggle() {
  const rail = document.getElementById("navigation-rail");
  const button = document.getElementById("rail-toggle");
  const host = document.getElementById("host-block");
  const tabs = document.getElementById("tabs");
  const shell = rail?.closest(".shell");
  if (!shell || !button) return;
  const slideTarget = tabs || host;
  const phone = window.matchMedia("(max-width: 760px)");
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  let rememberedCollapsed = loadRailCollapsedPref();
  let cancelPendingSlide = null;
  let toggleHadFocus = false;

  function syncRailControls(collapsed) {
    button.setAttribute("aria-expanded", String(!collapsed));
    button.setAttribute("aria-label", collapsed ? "Show navigation" : "Hide navigation");
    // Keep focus on the surviving button before withdrawing its neighbours.
    if (collapsed && rail.contains(document.activeElement)) button.focus();
    for (const content of rail.querySelectorAll(".rail-host, .rail-workspace, .tabs, .rail-foot, #host-block, .rail-conn, .clock-zone")) {
      content.inert = collapsed;
    }
    if (phone.matches && (document.activeElement === button || toggleHadFocus)) {
      rail.querySelector(".tab.active")?.focus();
      toggleHadFocus = false;
    }
  }

  function afterRailSlide(done) {
    let timer = 0;
    const cancel = () => {
      slideTarget?.removeEventListener("transitionend", onEnd);
      window.clearTimeout(timer);
      cancelPendingSlide = null;
    };
    const finish = () => {
      cancel();
      done();
    };
    const onEnd = (event) => {
      if (event.target === slideTarget && event.propertyName === "opacity") finish();
    };
    slideTarget?.addEventListener("transitionend", onEnd);
    timer = window.setTimeout(finish, RAIL_SLIDE_FALLBACK_MS);
    cancelPendingSlide = cancel;
  }

  function setRailCollapsed(collapsed) {
    if (phone.matches || collapsed === rememberedCollapsed) return;
    cancelPendingSlide?.();
    rememberedCollapsed = collapsed;
    saveRailCollapsedPref(collapsed);
    syncRailControls(collapsed);
    shell.classList.remove("rail-leaving", "rail-entering");
    if (collapsed) {
      const settle = () => {
        shell.classList.remove("rail-leaving");
        shell.classList.add("rail-collapsed");
      };
      if (reducedMotion.matches) settle();
      else {
        shell.classList.add("rail-leaving");
        afterRailSlide(settle);
      }
      return;
    }
    shell.classList.remove("rail-collapsed");
    if (!reducedMotion.matches && slideTarget) {
      shell.classList.add("rail-entering");
      void getComputedStyle(slideTarget).opacity;
      shell.classList.remove("rail-entering");
    }
  }

  function restoreRailLayout() {
    cancelPendingSlide?.();
    shell.classList.remove("rail-leaving", "rail-entering");
    const collapsed = rememberedCollapsed && !phone.matches;
    syncRailControls(collapsed);
    shell.classList.toggle("rail-collapsed", collapsed);
  }

  button.addEventListener("focus", () => { toggleHadFocus = true; });
  button.addEventListener("blur", () => {
    // CSS can hide and blur the button before the media-query callback.
    // Retain that focus only until the phone layout chooses a visible target.
    if (!phone.matches) toggleHadFocus = false;
  });
  restoreRailLayout();
  button.addEventListener("click", () => setRailCollapsed(!rememberedCollapsed));
  phone.addEventListener("change", restoreRailLayout);
}
