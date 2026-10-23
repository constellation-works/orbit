// Dashboard polling and the background watch (moved from router.js).
//
// A visible tab refreshes its panels on a one-shot 30 s timer that backs off
// after failures. A hidden tab refreshes no panels; it makes a light 60 s read
// of what an operator would otherwise miss there: the failed-run count, whose
// growth prefixes the page title until the tab is shown again, and the drain
// phase and baseline-red holds, which raise a browser notification only after
// the operator has opted in. A visible tab never notifies and keeps its plain
// title, so nothing changes while the operator is looking.

import { fetchJson, getHostRevision, getWorkspace, getWorkspaceRevision, isAggregateView } from './common.js';

const REFRESH_INTERVAL_MS = 30_000;
const MAX_REFRESH_INTERVAL_MS = 5 * 60_000;
const BACKGROUND_INTERVAL_MS = 60_000;
const NOTIFY_STORAGE_KEY = "orbit.dashboard.notify";
const HOLD_REASON = "baseline_red_hold";
const DRAINING_PHASES = new Set(["draining", "winding_down"]);
const $ = (id) => document.getElementById(id);

let baseTitle = "orbit";
// Failed runs since the tab was hidden; shown in the title until it is seen.
let unseenFailures = 0;

function applyTitle() {
  document.title = unseenFailures > 0 ? `(${unseenFailures} failed) ${baseTitle}` : baseTitle;
}

/// Name the page after the open destination, keeping any unseen-failure prefix.
export function setDashboardTitle(title) {
  baseTitle = title;
  applyTitle();
}

function notificationsSupported() {
  return typeof Notification === "function";
}

function optedIn() {
  try {
    return localStorage.getItem(NOTIFY_STORAGE_KEY) === "on";
  } catch (_) {
    return false;
  }
}

function setOptedIn(on) {
  try {
    if (on) localStorage.setItem(NOTIFY_STORAGE_KEY, "on");
    else localStorage.removeItem(NOTIFY_STORAGE_KEY);
  } catch (_) {}
}

function notificationsEnabled() {
  return notificationsSupported() && Notification.permission === "granted" && optedIn();
}

function notify(title, body) {
  if (!document.hidden || !notificationsEnabled()) return;
  try {
    new Notification(title, { body, tag: `orbit-${title}` });
  } catch (error) {
    console.error(error);
  }
}

function syncNotifyButton() {
  const button = $("notify-btn");
  if (!button) return;
  button.hidden = !notificationsSupported();
  if (button.hidden) return;
  const enabled = notificationsEnabled();
  const blocked = Notification.permission === "denied";
  button.setAttribute("aria-pressed", String(enabled));
  button.title = blocked
    ? "Notifications are blocked for this site in the browser's settings"
    : enabled
      ? "Notifying when a drain ends or a task is held while this tab is in the background. Click to stop."
      : "Notify me when a drain ends or a task is held while this tab is in the background";
}

/// The opt-in for background notifications. Permission is asked for only when
/// the operator presses the button, never on load.
export function initBackgroundNotifications() {
  const button = $("notify-btn");
  if (!button) return;
  syncNotifyButton();
  button.addEventListener("click", async () => {
    if (!notificationsSupported()) return;
    if (notificationsEnabled()) {
      setOptedIn(false);
    } else {
      const permission = Notification.permission === "granted"
        ? "granted"
        : await Promise.resolve(Notification.requestPermission()).catch(() => "default");
      setOptedIn(permission === "granted");
    }
    syncNotifyButton();
  });
}

function drainPhase(readiness) {
  const phase = readiness?.capacity?.drain_phase;
  return typeof phase === "string" ? phase : null;
}

function heldTasks(readiness) {
  const tasks = Array.isArray(readiness?.tasks) ? readiness.tasks : [];
  return new Set(tasks
    .filter((entry) => entry?.reason === HOLD_REASON && typeof entry.task_id === "string" && entry.task_id)
    .map((entry) => entry.task_id));
}

// The aggregate total sums only the workspaces that answered, so a workspace
// dropping out lowers it and its return raises it with no failure in either.
// The key names the unavailable workspaces; totals compare only under one key.
function unavailableKey(payload) {
  const names = Array.isArray(payload?.unavailable) ? payload.unavailable : [];
  return names.map((entry) => entry?.workspace_id).sort().join(",");
}

// One light read for the hidden tab. A field is null when its source did not
// answer, so a failed read never reads as a change.
async function readBackground() {
  const aggregate = isAggregateView();
  const failed = fetchJson(`${aggregate ? "/api/job-runs/all" : "/api/job-runs"}?state=failed&limit=1`)
    .then((payload) => (Number.isFinite(payload?.total)
      ? { total: payload.total, unavailable: unavailableKey(payload) }
      : null));
  const readiness = aggregate || !getWorkspace() ? Promise.resolve(null) : fetchJson("/api/workflows/auto/readiness");
  const [failedResult, readinessResult] = await Promise.allSettled([failed, readiness]);
  const drain = readinessResult.status === "fulfilled" ? readinessResult.value : null;
  return {
    failed: failedResult.status === "fulfilled" ? failedResult.value : null,
    phase: drain ? drainPhase(drain) : null,
    holds: drain ? heldTasks(drain) : null,
  };
}

function watchScope() {
  return `${getHostRevision()}:${getWorkspaceRevision()}`;
}

// What the hidden tab last saw. The first read after hiding, or after a scope
// change, only records; later reads compare against it.
let watch = null;

async function checkBackground() {
  const scope = watchScope();
  const seen = await readBackground();
  if (!document.hidden || scope !== watchScope()) return;
  if (!watch || watch.scope !== scope) {
    watch = { scope, failed: seen.failed, phase: seen.phase, holds: seen.holds };
    return;
  }
  if (seen.failed !== null) {
    const sameWorkspaces = watch.failed !== null && watch.failed.unavailable === seen.failed.unavailable;
    if (sameWorkspaces && seen.failed.total > watch.failed.total) {
      unseenFailures += seen.failed.total - watch.failed.total;
      applyTitle();
    }
    watch.failed = seen.failed;
  }
  if (seen.phase !== null) {
    if (DRAINING_PHASES.has(watch.phase) && seen.phase === "idle") {
      notify("Orbit drain ended", `The drain${getWorkspace() ? ` in ${getWorkspace()}` : ""} has finished.`);
    }
    watch.phase = seen.phase;
  }
  if (seen.holds !== null) {
    const added = watch.holds ? [...seen.holds].filter((id) => !watch.holds.has(id)) : [];
    if (added.length) notify("Orbit task held", `${added.join(", ")} held until the base branch passes.`);
    watch.holds = seen.holds;
  }
}

// Showing the tab acknowledges what the title announced.
function acknowledgeBackground() {
  watch = null;
  if (unseenFailures !== 0) {
    unseenFailures = 0;
    applyTitle();
  }
}

// A one-shot timer avoids overlapping refreshes: the next poll is scheduled
// only after the current refresh or background read settles. Hiding swaps the
// pending refresh for an immediate background read and its 60 s cadence;
// becoming visible starts exactly one refresh and resumes the normal cadence.
export function startDashboardPolling(refresh) {
  let timer = null;
  let interval = REFRESH_INTERVAL_MS;

  const clearScheduled = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  const schedule = (fn, delay) => {
    if (timer !== null) return;
    timer = setTimeout(fn, delay);
    // Node-based dashboard tests should not be kept alive by a browser poll.
    // Browsers return a numeric timer handle, so this is a no-op in production.
    if (typeof timer === "object" && typeof timer.unref === "function") timer.unref();
  };

  const scheduleRefresh = () => {
    if (!document.hidden) schedule(runRefresh, interval);
  };

  const scheduleBackground = () => {
    if (document.hidden) schedule(runBackground, BACKGROUND_INTERVAL_MS);
  };

  const runRefresh = () => {
    timer = null;
    if (document.hidden) return;
    Promise.resolve(refresh())
      .then((succeeded) => {
        if (succeeded === true) interval = REFRESH_INTERVAL_MS;
        if (succeeded === false) interval = Math.min(interval * 2, MAX_REFRESH_INTERVAL_MS);
      })
      .catch(() => {
        interval = Math.min(interval * 2, MAX_REFRESH_INTERVAL_MS);
      })
      .finally(scheduleRefresh);
  };

  const runBackground = () => {
    timer = null;
    if (!document.hidden) return;
    checkBackground()
      .catch((error) => console.error(error))
      .finally(scheduleBackground);
  };

  document.addEventListener("visibilitychange", () => {
    clearScheduled();
    if (document.hidden) {
      runBackground();
      return;
    }
    acknowledgeBackground();
    syncNotifyButton();
    runRefresh();
  });

  if (document.hidden) runBackground();
  else runRefresh();
}
