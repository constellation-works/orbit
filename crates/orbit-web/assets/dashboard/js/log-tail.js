// Orbit dashboard log-tail panel (SSE stream, buffered logs, filters).
// Pure vanilla JS, split into ES modules with no build step.
//
// This module owns the #log-panel behavior on the Drain page. It is initialized
// by a single `initLogTail();` call from app.js (the bootstrap call site is kept
// in app.js per the extraction contract; the call still fires exactly once at
// page load). The module exports `initLogTail` and `fitLogPanelToViewport` for
// the two call sites that remain in app.js (`refreshDashboard` and `setActiveTab`),
// and `setDockMode` for the router to select the Drain or Log view.

import { bindTaskLink, el, fetchJson, formatClock, formatDateTime, onClockZoneChange, onWorkspaceChange, getHost, isHostUnavailable, withHost } from './common.js';

const $ = (id) => document.getElementById(id);

let logStream = null;
let logBuffered = [];
// Events dropped from the front of logBuffered while paused, stated in the UI
// so a gap in the history is never silent.
let logBufferedDropped = 0;
let logFollowTail = true;
let logRows = []; // Keep track to enforce max 200 after 250 limit
const LOG_ROWS_RETAINED = 200;
const LOG_ROWS_MAX = 250;
// A flush keeps at most LOG_ROWS_RETAINED rows, so a paused buffer holding
// more would only be rendered and thrown away.
const LOG_BUFFERED_MAX = LOG_ROWS_RETAINED;
let activeLogFilters = new Set(["all"]);
let logPanelPrefsWired = false;
const LOG_STREAM_RETRY_MIN_MS = 1000;
const LOG_STREAM_RETRY_MAX_MS = 15000;
const LOG_STREAM_UNAVAILABLE = "log stream unavailable, retrying";
let logStreamOffset = 0;
let logAgentStreamOffset = 0;
let logStreamRetryTimer = null;
let logStreamRetryMs = LOG_STREAM_RETRY_MIN_MS;
// True once the snapshot resolved and a live stream is owed. A hidden tab
// closes its stream (the server allows only a few, and a background tab would
// hold a permit for nothing); this flag tells the visibility handler to reopen
// it from `logStreamOffset` instead of before there is an offset to resume.
let logStreamWanted = false;
// The stream stays closed until a snapshot succeeds. Opening it at offset 0
// after a failed snapshot would replay history that the next snapshot also
// renders. Retries share the stream's backoff, and the same disconnected
// status, until that cursor exists.
let logSnapshotReady = false;
let logSnapshotRetryTimer = null;
let logSnapshotRetryMs = LOG_STREAM_RETRY_MIN_MS;
let logSnapshotAttempt = 0;
let logVisibilityWired = false;

// Overview, Start a window and Log share one dock. It sits on the Drain page and, while Tasks
// shows, beside the task list (placeDock). The Drain route selects its view;
// on Tasks the view is the last one chosen. Agent and wrapping preferences
// stay local to this browser.
const LOG_PANEL_PREFS_KEY = "orbit.dashboard.logPanel";
const DOCK_MODES = ["drain", "start", "log"];

function loadLogPanelPrefs() {
  try {
    const raw = window.localStorage.getItem(LOG_PANEL_PREFS_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    // `status` is the Drain mode's name before ORB-12898; a stored one opens as Drain.
    const dockMode = parsed.dockMode === "status" ? "drain" : parsed.dockMode;
    return { dockMode: DOCK_MODES.includes(dockMode) ? dockMode : "drain", showAgent: parsed.showAgent !== false };
  } catch (_) {
    return { dockMode: "drain", showAgent: true };
  }
}

function saveLogPanelPrefs(prefs) {
  try {
    window.localStorage.setItem(LOG_PANEL_PREFS_KEY, JSON.stringify(prefs));
  } catch (_) {
    /* localStorage unavailable (private mode, quota) — presentation prefs are non-essential */
  }
}

let logPanelPrefs = loadLogPanelPrefs();

const LOG_WRAP_PREFS_KEY = "orbit.dashboard.logWrap";
let logWrap = false;

export function loadLogWrapPref() {
  try {
    return window.localStorage.getItem(LOG_WRAP_PREFS_KEY) === "true";
  } catch (_) {
    return false;
  }
}

export function saveLogWrapPref(wrap) {
  try {
    window.localStorage.setItem(LOG_WRAP_PREFS_KEY, String(wrap));
  } catch (_) {
    /* localStorage unavailable */
  }
}

export function applyLogWrap(wrap) {
  const stream = document.querySelector(".log-stream");
  const btn = $("log-wrap-lines");
  if (stream && stream.classList && typeof stream.classList.toggle === "function") {
    stream.classList.toggle("wrap", wrap);
  }
  if (btn) {
    if (btn.classList && typeof btn.classList.toggle === "function") {
      btn.classList.toggle("on", wrap);
    }
    if (typeof btn.setAttribute === "function") {
      btn.setAttribute("aria-pressed", String(wrap));
    }
  }
}

function wireLogWrapToggle() {
  logWrap = loadLogWrapPref();
  applyLogWrap(logWrap);

  const btn = $("log-wrap-lines");
  if (btn && typeof btn.addEventListener === "function") {
    btn.addEventListener("click", () => {
      logWrap = !logWrap;
      saveLogWrapPref(logWrap);
      applyLogWrap(logWrap);
    });
  }
}

// Kept as the exported name because app.js (refreshDashboard) and router.js
// (setActiveTab) both call it. The dock is sized by the CSS grid now, so there
// is no viewport arithmetic left — this only re-asserts the current mode.
export function fitLogPanelToViewport() {
  applyDockMode();
  applyLogWrap(loadLogWrapPref());
}

function applyDockMode() {
  const dock = $("side-dock");
  if (!dock) return;
  const mode = logPanelPrefs.dockMode;
  dock.dataset.mode = mode;
  for (const btn of document.querySelectorAll("#dock-mode-toggle .dock-seg")) {
    const on = btn.dataset.mode === mode;
    btn.classList.toggle("on", on);
    btn.setAttribute("aria-selected", on ? "true" : "false");
    btn.tabIndex = on ? 0 : -1;
  }
}

// One dock, two homes: the Tasks page's right column and the Drain page. It
// moves to whichever of the two is shown, so its cards keep one renderer and
// one set of element ids. On Tasks its first view is named Drain, since the
// page around it is not.
const DOCK_HOMES = { tasks: { slot: "tasks-dock", label: "Drain" }, drain: { slot: "drain-main", label: "Overview" } };

export function placeDock(tab) {
  const dock = $("side-dock");
  const home = DOCK_HOMES[tab];
  const slot = home ? $(home.slot) : null;
  if (!dock || !slot || dock.parentNode === slot) return;
  slot.appendChild(dock);
  dock.dataset.home = tab;
  const button = $("dock-tab-drain");
  const label = button && button.querySelector(".dock-seg-label");
  if (label) label.textContent = home.label;
  if (button && button.dataset.drainLabel) button.setAttribute("aria-label", `${home.label}: ${button.dataset.drainLabel}`);
}

// On the Drain page a view is a route; on Tasks choosing one only switches it.
export function chooseDockMode(mode) {
  setDockMode(mode);
  if ($("side-dock")?.dataset.home !== "tasks") window.location.hash = mode === "drain" ? "#drain" : `#drain/${mode}`;
}

// The router selects the Drain page view through this presentation handle.
export function setDockMode(mode) {
  if (!DOCK_MODES.includes(mode)) return;
  logPanelPrefs = { ...logPanelPrefs, dockMode: mode };
  saveLogPanelPrefs(logPanelPrefs);
  applyDockMode();
}

function wireDockModeToggle() {
  const toggle = $("dock-mode-toggle");
  if (!toggle) return;
  for (const btn of toggle.querySelectorAll(".dock-seg")) {
    btn.addEventListener("click", () => chooseDockMode(btn.dataset.mode));
  }
  // Arrow keys move between the segments, matching the window selectors;
  // Home and End jump to the first and last, as a tablist promises.
  toggle.addEventListener("keydown", (event) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const idx = DOCK_MODES.indexOf(logPanelPrefs.dockMode);
    const next = event.key === "Home" ? 0
      : event.key === "End" ? DOCK_MODES.length - 1
        : event.key === "ArrowLeft" ? idx - 1 : idx + 1;
    const mode = DOCK_MODES[(next + DOCK_MODES.length) % DOCK_MODES.length];
    chooseDockMode(mode);
    const btn = toggle.querySelector(`.dock-seg[data-mode="${mode}"]`);
    if (btn) btn.focus();
  });
}

// ORB-10972: the bottom status bar carries the newest line on every tab, not
// just the Drain page where the full tail is mounted. The SSE connection is opened
// once at boot and never torn down on a tab change, so mirroring here is
// enough to keep the bar live everywhere.
//
// The bar follows the dock's agent toggle: while agent stdout is hidden, relays
// never replace the line, so an operator glancing at it sees the newest Orbit
// event rather than a drain's raw agent traffic. Both candidates are kept so
// flipping the toggle can redraw the bar without waiting for another event.
let statusNewestEvent = null;
let statusQuietEvent = null;

function trackLogStatusEvent(ev) {
  statusNewestEvent = ev;
  if (ev.agent_stdout !== true) statusQuietEvent = ev;
}

function noteLogStatusEvent(ev) {
  if (!ev) return;
  trackLogStatusEvent(ev);
  refreshLogStatusBar();
}

function refreshLogStatusBar() {
  writeLogStatusBar(logPanelPrefs.showAgent ? statusNewestEvent : statusQuietEvent);
}

// No event clears the bar: a snapshot without one, or a host switch, must not
// leave an earlier line on show.
function writeLogStatusBar(ev) {
  const bar = $("log-statusbar");
  if (!bar) return;
  const t = $("log-statusbar-time");
  const ag = $("log-statusbar-source");
  const m = $("log-statusbar-message");
  if (!ev) {
    if (t) {
      t.textContent = "";
      t.title = "";
    }
    if (ag) {
      ag.textContent = "";
      ag.title = "";
    }
    if (m) {
      m.textContent = "";
      delete m.dataset.level;
    }
    bar.classList.add("empty");
    return;
  }
  const { text: timeStr, title: timeTitle } = logTime(ev.ts);
  if (t) {
    t.textContent = timeStr;
    t.title = timeTitle;
  }
  if (ag) {
    ag.textContent = ev.source || "";
    ag.title = ev.target || ev.source || "";
  }
  if (m) {
    m.innerHTML = ev.message_html || "";
    m.dataset.level = getLogClass(ev.level, ev.code);
  }
  bar.classList.remove("empty");
}

function wireLogPanelPreferences() {
  if (logPanelPrefsWired) return;
  logPanelPrefsWired = true;
  wireDockModeToggle();
  applyDockMode();
}

// The dock and status bar are too narrow for a zone on every line, so the
// wall-clock time carries its full date and zone in the title.
function logTime(ts) {
  const raw = ts || "";
  if (!raw.includes("T")) return { text: raw, title: "" };
  const d = new Date(raw);
  if (isNaN(d.getTime())) return { text: raw, title: "" };
  return { text: formatClock(d, { zone: false }), title: formatDateTime(d, { seconds: true }) };
}

function getLogClass(level, code) {
  if (code === "DENY") return "deny";
  if (code === "OK") return "ok";
  if (code === "ERR" || level === "error") return "err";
  if (code === "WRN" || level === "warn") return "warn";
  return "info";
}

// The task a line is about, when its fields name one (`task_id=<id>` in
// an agent relay's context); the row links it into the Tasks list.
const LOG_TASK_ID = /\btask_id=([A-Z][A-Z0-9]*-\d+)\b/;

// A row is the time beside two lines: a badge (AGENT for an agent's stdout,
// else the event code), the task it names and its source, then the message,
// cut to one line until the row is expanded or Wrap is on.
function renderLogEvent(ev, isFresh) {
  const row = el("div", { class: "log-line" + (isFresh ? " fresh" : "") });
  row.dataset.code = ev.code || "";
  row.dataset.level = ev.level || "info";
  row.dataset.agentStdout = String(ev.agent_stdout === true);
  row.dataset.ts = ev.ts || "";

  const { text: timeStr, title: timeTitle } = logTime(ev.ts);
  const tSpan = el("span", { class: "t", text: timeStr, title: timeTitle });
  const agSpan = el("span", { class: "ag", text: ev.source || "", title: ev.target || ev.source || "" });
  const lvClass = getLogClass(ev.level, ev.code);
  // The filters read the level back out of dataset; the class colours the
  // row's badge and keyline.
  row.classList.add(`lv-${lvClass}`);
  const agent = ev.agent_stdout === true;
  const lvSpan = el("span", { class: `lv ${agent ? "agent" : lvClass}`, title: ev.code || "", text: agent ? "AGENT" : ev.code || "" });
  const mSpan = el("span", { class: "m" });
  mSpan.innerHTML = ev.message_html || "";
  const taskId = LOG_TASK_ID.exec(mSpan.textContent || "")?.[1];
  let task = null;
  if (taskId) {
    task = bindTaskLink(el("a", { class: "log-task", text: taskId, title: `Open ${taskId} in Tasks` }), taskId);
  }

  row.appendChild(tSpan);
  row.appendChild(el("div", { class: "log-body" }, [
    el("div", { class: "log-head" }, [lvSpan, task, agSpan]),
    mSpan,
  ]));

  // Click to expand/collapse the full message
  row.addEventListener("click", () => row.classList.toggle("expanded"));

  return row;
}

function clearLogSnapshotRetry() {
  if (logSnapshotRetryTimer !== null) {
    clearTimeout(logSnapshotRetryTimer);
    logSnapshotRetryTimer = null;
  }
}

function scheduleLogSnapshotRetry() {
  clearLogSnapshotRetry();
  if (document.hidden || logSnapshotReady) return;
  const delay = logSnapshotRetryMs;
  logSnapshotRetryMs = Math.min(logSnapshotRetryMs * 2, LOG_STREAM_RETRY_MAX_MS);
  logSnapshotRetryTimer = setTimeout(() => {
    logSnapshotRetryTimer = null;
    loadLogSnapshot();
  }, delay);
}

// Returns false when the dock is absent. That is not a server failure, so the
// caller must not mark the snapshot ready and must not open a stream.
function applyLogSnapshot(payload) {
  const inner = $("logInner");
  if (!inner) return false;
  inner.innerHTML = "";
  logRows = [];
  const events = payload && Array.isArray(payload.events) ? payload.events : [];
  if (
    payload &&
    typeof payload.offset === "number" &&
    Number.isFinite(payload.offset) &&
    payload.offset >= 0
  ) {
    logStreamOffset = payload.offset;
    logAgentStreamOffset = Number.isFinite(payload.agent_offset) && payload.agent_offset >= 0
      ? payload.agent_offset : 0;
  }
  events.slice().reverse().forEach(ev => {
    const row = renderLogEvent(ev, false);
    inner.appendChild(row);
    logRows.push(row);
  });
  applyLogFilters();
  statusNewestEvent = null;
  statusQuietEvent = null;
  events.forEach(trackLogStatusEvent);
  refreshLogStatusBar();
  connectLogStream();
  return true;
}

function loadLogSnapshot() {
  if (document.hidden || logSnapshotReady) return;
  // A selected host that cannot be shown is not asked again until a refresh
  // finds it reachable; the retry keeps its place in the backoff meanwhile.
  if (isHostUnavailable()) {
    scheduleLogSnapshotRetry();
    return;
  }
  const attempt = ++logSnapshotAttempt;
  fetchJson("/api/log?limit=50").then((payload) => {
    if (attempt !== logSnapshotAttempt || logSnapshotReady) return;
    if (!applyLogSnapshot(payload)) return;
    logSnapshotReady = true;
    logSnapshotRetryMs = LOG_STREAM_RETRY_MIN_MS;
    clearLogSnapshotRetry();
  }).catch((error) => {
    if (attempt !== logSnapshotAttempt || logSnapshotReady) return;
    console.error(error);
    setLogStreamConnected(false);
    scheduleLogSnapshotRetry();
  });
}

// ORB-14680: the tail follows the selected host. Another host's log has its
// own offsets, so a switch drops the rows and cursor and starts from that
// host's snapshot, exactly as a first load does. The old host's rows, buffered
// count and status line go at the switch, not when the snapshot settles: a
// held or failed snapshot must not leave them shown under the new host.
let logHost = null;

function restartLogTailForHost() {
  if (getHost() === logHost) return;
  logHost = getHost();
  logSnapshotAttempt += 1;
  clearLogSnapshotRetry();
  closeLogStream();
  logStreamWanted = false;
  logSnapshotReady = false;
  logStreamOffset = 0;
  logAgentStreamOffset = 0;
  logBuffered = [];
  logBufferedDropped = 0;
  logSnapshotRetryMs = LOG_STREAM_RETRY_MIN_MS;
  logStreamRetryMs = LOG_STREAM_RETRY_MIN_MS;
  clearLogView();
  loadLogSnapshot();
}

function clearLogView() {
  const inner = $("logInner");
  if (inner) inner.innerHTML = "";
  logRows = [];
  applyLogFilters();
  const btnBuffered = $("log-buffered-count");
  if (btnBuffered) {
    btnBuffered.textContent = "";
    btnBuffered.title = "";
    btnBuffered.style.display = "none";
  }
  statusNewestEvent = null;
  statusQuietEvent = null;
  refreshLogStatusBar();
}

export function repaintLogTimes() {
  for (const row of logRows) {
    if (!row.dataset.ts) continue;
    const time = row.querySelector(".t");
    if (!time) continue;
    const painted = logTime(row.dataset.ts);
    time.textContent = painted.text;
    time.title = painted.title;
  }
  refreshLogStatusBar();
}

export function initLogTail() {
  logHost = getHost();
  onClockZoneChange(repaintLogTimes);
  onWorkspaceChange(restartLogTailForHost);
  wireLogVisibility();
  wireLogPanelPreferences();
  wireLogWrapToggle();
  fitLogPanelToViewport();
  syncLogFilterPills();
  loadLogSnapshot();

  const followBtn = $("log-follow-tail");
  if (followBtn) {
    followBtn.addEventListener("click", () => {
      logFollowTail = !logFollowTail;
      followBtn.classList.toggle("on", logFollowTail);
      followBtn.setAttribute("aria-pressed", String(logFollowTail));
      if (logFollowTail) {
        flushBufferedLogs();
      }
    });
  }

  const btnBuffered = $("log-buffered-count");
  if (btnBuffered) {
    btnBuffered.addEventListener("click", () => {
      if (!logFollowTail) flushBufferedLogs();
    });
  }

  const agentBtn = $("log-show-agent");
  if (agentBtn) {
    agentBtn.addEventListener("click", () => {
      logPanelPrefs = { ...logPanelPrefs, showAgent: !logPanelPrefs.showAgent };
      saveLogPanelPrefs(logPanelPrefs);
      syncLogFilterPills();
      applyLogFilters();
      refreshLogStatusBar();
    });
  }

  document.querySelectorAll("#side-dock .filter-pill[data-filter]").forEach(pill => {
    pill.addEventListener("click", () => {
      const filter = pill.dataset.filter;
      if (filter === "all") {
        activeLogFilters.clear();
        activeLogFilters.add("all");
      } else {
        if (activeLogFilters.has("all")) {
          activeLogFilters.clear();
        }
        if (activeLogFilters.has(filter)) {
          activeLogFilters.delete(filter);
          if (activeLogFilters.size === 0) activeLogFilters.add("all");
        } else {
          activeLogFilters.add(filter);
        }
      }

      syncLogFilterPills();
      applyLogFilters();
    });
  });
}

function flushBufferedLogs() {
  const inner = $("logInner");
  if (!inner) return;
  const wasEmpty = logBuffered.length === 0;
  if (logBufferedDropped > 0) {
    const gap = renderLogEvent({
      ts: "",
      source: "orbit.log",
      code: "GAP",
      level: "warn",
      message_html: "",
    }, true);
    gap.dataset.gap = "true";
    const message = gap.querySelector(".m");
    if (message) message.textContent = logGapMessage(logBufferedDropped);
    inner.insertBefore(gap, inner.firstChild);
    logRows.unshift(gap);
    setTimeout(() => gap.classList.remove("fresh"), 600);
  }
  for (const ev of logBuffered) {
    const row = renderLogEvent(ev, true);
    inner.insertBefore(row, inner.firstChild);
    logRows.unshift(row);
    setTimeout(() => row.classList.remove("fresh"), 600);
  }
  logBuffered = [];
  logBufferedDropped = 0;
  const btnBuffered = $("log-buffered-count");
  if (btnBuffered) btnBuffered.style.display = "none";
  enforceLogBounds();
  if (!wasEmpty) applyLogFilters();
  const stream = $("side-dock") ? $("side-dock").querySelector(".log-stream") : document.querySelector(".log-stream");
  if (stream) stream.scrollTop = 0;
}

function logGapMessage(dropped) {
  return `${dropped} older ${dropped === 1 ? "line" : "lines"} discarded while paused`;
}

// Oldest first: past the cap the oldest buffered event is dropped and counted.
function bufferPausedLogEvent(ev) {
  logBuffered.push(ev);
  if (logBuffered.length > LOG_BUFFERED_MAX) {
    logBuffered.shift();
    logBufferedDropped += 1;
  }
  const btn = $("log-buffered-count");
  if (btn) {
    btn.textContent = logBufferedDropped > 0
      ? `${logBuffered.length} buffered · ${logBufferedDropped} discarded`
      : `${logBuffered.length} buffered`;
    btn.title = logBufferedDropped > 0 ? logGapMessage(logBufferedDropped) : "";
    btn.style.display = "";
  }
}

function enforceLogBounds() {
  if (logRows.length > LOG_ROWS_MAX) {
    const toRemove = logRows.splice(LOG_ROWS_RETAINED);
    for (const row of toRemove) {
      row.remove();
    }
  }
}

// The pills are toggle buttons: `on` carries the visual state and `aria-pressed`
// carries the same fact for assistive tech, so both are written from the one
// active-filter set rather than from the click target.
function syncLogFilterPills() {
  for (const pill of document.querySelectorAll("#side-dock .filter-pill[data-filter]")) {
    const on = activeLogFilters.has(pill.dataset.filter);
    pill.classList.toggle("on", on);
    pill.setAttribute("aria-pressed", String(on));
  }
  const agentBtn = $("log-show-agent");
  if (agentBtn) {
    agentBtn.classList.toggle("on", logPanelPrefs.showAgent);
    agentBtn.setAttribute("aria-pressed", String(logPanelPrefs.showAgent));
  }
}

function applyLogFilters() {
  let visibleCount = 0;
  const counts = { err: 0, deny: 0, warn: 0 };
  for (const row of logRows) {
    if (row.dataset.gap) {
      row.style.display = "";
      continue;
    }
    const code = row.dataset.code;
    const level = row.dataset.level;
    const lvClass = getLogClass(level, code);
    if (lvClass in counts) counts[lvClass] += 1;

    let show = false;
    if (activeLogFilters.has("all")) {
      show = true;
    } else {
      if (activeLogFilters.has("err") && lvClass === "err") show = true;
      if (activeLogFilters.has("deny") && lvClass === "deny") show = true;
      if (activeLogFilters.has("warn") && lvClass === "warn") show = true;
    }
    if (!logPanelPrefs.showAgent && row.dataset.agentStdout === "true") show = false;
    row.style.display = show ? "" : "none";
    if (show) visibleCount++;
  }
  
  const cnt = $("log-count");
  if (cnt) cnt.textContent = `${visibleCount}`;
  writeLogCounts(counts);
}

// Each level pill counts its lines among those kept, and the Log tab names
// the worst level present so a problem shows while Drain is selected.
function writeLogCounts(counts) {
  for (const node of document.querySelectorAll("#side-dock .filter-count[data-count]")) {
    const count = counts[node.dataset.count] || 0;
    node.textContent = String(count);
    node.classList.toggle("nonzero", count > 0);
  }
  const state = $("dock-log-state");
  if (!state) return;
  const level = counts.err > 0 ? "err" : counts.deny > 0 ? "deny" : counts.warn > 0 ? "warn" : "";
  const word = { err: "error", deny: "denial", warn: "warning" }[level];
  state.textContent = level ? `${counts[level]} ${word}${counts[level] === 1 ? "" : "s"}` : "";
  state.dataset.level = level;
}

function rememberStreamOffset(lastEventId) {
  if (!lastEventId) return;
  const [operational, agent] = lastEventId.split(":");
  const parsed = Number(operational);
  if (Number.isFinite(parsed) && parsed >= 0) logStreamOffset = parsed;
  const agentParsed = Number(agent);
  if (Number.isFinite(agentParsed) && agentParsed >= 0) logAgentStreamOffset = agentParsed;
}

function setLogStreamConnected(connected) {
  const bar = $("log-statusbar");
  const dock = $("side-dock");
  if (bar) {
    bar.classList.toggle("disconnected", !connected);
    const label = bar.querySelector(".sb-label");
    if (label) label.textContent = connected ? "orbit.log" : LOG_STREAM_UNAVAILABLE;
    bar.setAttribute("aria-label", connected ? "Latest log line" : LOG_STREAM_UNAVAILABLE);
  }
  if (dock) dock.classList.toggle("disconnected", !connected);
  const live = $("log-live-label");
  if (live) live.textContent = connected ? "Live" : "Reconnecting";
}

function closeLogStream() {
  if (logStreamRetryTimer !== null) {
    clearTimeout(logStreamRetryTimer);
    logStreamRetryTimer = null;
  }
  if (logStream) {
    logStream.close();
    logStream = null;
  }
}

function handleLogVisibilityChange() {
  if (document.hidden) {
    // Invalidate an in-flight snapshot so a late failure cannot arm a retry
    // while this tab is hidden. A late success is discarded with it; becoming
    // visible loads a fresh cursor before any stream opens.
    logSnapshotAttempt += 1;
    clearLogSnapshotRetry();
    closeLogStream();
  } else if (!logSnapshotReady) {
    logSnapshotRetryMs = LOG_STREAM_RETRY_MIN_MS;
    loadLogSnapshot();
  } else if (logStreamWanted && !logStream) {
    logStreamRetryMs = LOG_STREAM_RETRY_MIN_MS;
    connectLogStream();
  }
}

function wireLogVisibility() {
  if (logVisibilityWired) return;
  logVisibilityWired = true;
  document.addEventListener("visibilitychange", handleLogVisibilityChange);
}

function connectLogStream() {
  logStreamWanted = true;
  closeLogStream();
  // A hidden tab opens nothing; becoming visible reconnects from the offset.
  if (document.hidden) return;
  logStream = new EventSource(withHost(`/api/log/stream?from=${encodeURIComponent(String(logStreamOffset))}&agent_from=${encodeURIComponent(String(logAgentStreamOffset))}`));
  logStream.onopen = () => {
    logStreamRetryMs = LOG_STREAM_RETRY_MIN_MS;
    setLogStreamConnected(true);
  };
  logStream.onmessage = (e) => {
    logStreamRetryMs = LOG_STREAM_RETRY_MIN_MS;
    setLogStreamConnected(true);
    rememberStreamOffset(e.lastEventId);
    try {
      const ev = JSON.parse(e.data);
      noteLogStatusEvent(ev);
      if (logFollowTail) {
        const inner = $("logInner");
        const row = renderLogEvent(ev, true);
        inner.insertBefore(row, inner.firstChild);
        logRows.unshift(row);
        enforceLogBounds();
        applyLogFilters();
        setTimeout(() => row.classList.remove("fresh"), 600);
        const stream = $("side-dock") ? $("side-dock").querySelector(".log-stream") : document.querySelector(".log-stream");
        if (stream) stream.scrollTop = 0;
      } else {
        bufferPausedLogEvent(ev);
      }
    } catch (err) {
      console.error("Failed to parse SSE event", err);
    }
  };
  logStream.onerror = () => {
    setLogStreamConnected(false);
    if (!logStream || logStream.readyState !== EventSource.CLOSED) return;
    logStream.close();
    logStream = null;
    const delay = logStreamRetryMs;
    logStreamRetryMs = Math.min(logStreamRetryMs * 2, LOG_STREAM_RETRY_MAX_MS);
    logStreamRetryTimer = setTimeout(() => {
      logStreamRetryTimer = null;
      connectLogStream();
    }, delay);
  };
}
