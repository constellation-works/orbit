// Orbit dashboard log-tail panel (SSE stream, buffered logs, viewport resize, filters).
// Pure vanilla JS, split into ES modules with no build step.
//
// This module owns the #log-panel behavior on the Tasks tab. It is initialized
// by a single `initLogTail();` call from app.js (the bootstrap call site is kept
// in app.js per the extraction contract; the call still fires exactly once at
// page load). The module exports `initLogTail` and `fitLogPanelToViewport` for
// the two call sites that remain in app.js (`refreshDashboard` and `setActiveTab`).

import { el, fetchJson } from './common.js';

const $ = (id) => document.getElementById(id);

let logStream = null;
let logBuffered = [];
let logFollowTail = true;
let logRows = []; // Keep track to enforce max 200 after 250 limit
let activeLogFilters = new Set(["all"]);
let logPanelResizeWired = false;
const LOG_STREAM_RETRY_MIN_MS = 1000;
const LOG_STREAM_RETRY_MAX_MS = 15000;
const LOG_STREAM_UNAVAILABLE = "log stream unavailable, retrying";
let logStreamOffset = 0;
let logStreamRetryTimer = null;
let logStreamRetryMs = LOG_STREAM_RETRY_MIN_MS;

// ORB-10972: the log lives in the Tasks tab's right dock, which has two modes
// — Status (in-flight runs, locked files, sweep clock) and Log (the tail at
// full dock height). The mode is a local presentation preference, not shared
// state, so it persists to localStorage rather than the URL. This supersedes
// ORB-10874's collapse toggle and height-resize handle: a full-height dock has
// no height to negotiate, and the always-visible bottom status bar took over
// the job the collapsed panel used to do.
const LOG_PANEL_PREFS_KEY = "orbit.dashboard.logPanel";
const DOCK_MODES = ["status", "log"];

function loadLogPanelPrefs() {
  try {
    const raw = window.localStorage.getItem(LOG_PANEL_PREFS_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return { dockMode: DOCK_MODES.includes(parsed.dockMode) ? parsed.dockMode : "status" };
  } catch (_) {
    return { dockMode: "status" };
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

// Kept as the exported name because app.js (refreshDashboard) and router.js
// (setActiveTab) both call it. The dock is sized by the CSS grid now, so there
// is no viewport arithmetic left — this only re-asserts the current mode.
export function fitLogPanelToViewport() {
  applyDockMode();
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

function setDockMode(mode) {
  if (!DOCK_MODES.includes(mode)) return;
  logPanelPrefs = { ...logPanelPrefs, dockMode: mode };
  saveLogPanelPrefs(logPanelPrefs);
  applyDockMode();
}

function wireDockModeToggle() {
  const toggle = $("dock-mode-toggle");
  if (!toggle) return;
  for (const btn of toggle.querySelectorAll(".dock-seg")) {
    btn.addEventListener("click", () => setDockMode(btn.dataset.mode));
  }
  // Arrow keys move between the two segments, matching the window selectors.
  toggle.addEventListener("keydown", (event) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const idx = DOCK_MODES.indexOf(logPanelPrefs.dockMode);
    const next = event.key === "ArrowLeft" ? idx - 1 : idx + 1;
    const mode = DOCK_MODES[(next + DOCK_MODES.length) % DOCK_MODES.length];
    setDockMode(mode);
    const btn = toggle.querySelector(`.dock-seg[data-mode="${mode}"]`);
    if (btn) btn.focus();
  });
}

// ORB-10972: the bottom status bar carries the newest line on every tab, not
// just the Tasks tab where the dock is mounted. The SSE connection is opened
// once at boot and never torn down on a tab change, so mirroring here is
// enough to keep the bar live everywhere.
function updateLogStatusBar(ev) {
  const bar = $("log-statusbar");
  if (!bar || !ev) return;
  let timeStr = ev.ts || "";
  if (timeStr && timeStr.includes("T")) {
    const d = new Date(timeStr);
    if (!isNaN(d.getTime())) timeStr = d.toLocaleTimeString("en-US", { hour12: false });
  }
  const t = $("log-statusbar-time");
  const ag = $("log-statusbar-source");
  const m = $("log-statusbar-message");
  if (t) t.textContent = timeStr;
  if (ag) ag.textContent = ev.source || "";
  if (m) {
    m.innerHTML = ev.message_html || "";
    m.dataset.level = getLogClass(ev.level, ev.code);
  }
  bar.classList.remove("empty");
}

function wireLogPanelResize() {
  if (logPanelResizeWired) return;
  logPanelResizeWired = true;
  wireDockModeToggle();
  applyDockMode();
}

function getLogClass(level, code) {
  if (code === "DENY") return "deny";
  if (code === "OK") return "ok";
  if (code === "ERR" || level === "error") return "err";
  if (code === "WRN" || level === "warn") return "warn";
  return "info";
}

function renderLogEvent(ev, isFresh) {
  const row = el("div", { class: "log-line" + (isFresh ? " fresh" : "") });
  row.dataset.code = ev.code || "";
  row.dataset.level = ev.level || "info";

  let timeStr = ev.ts || "";
  if (timeStr && timeStr.includes("T")) {
    const d = new Date(timeStr);
    if (!isNaN(d.getTime())) {
      timeStr = d.toLocaleTimeString("en-US", {hour12: false});
    }
  }

  const tSpan = el("span", { class: "t", text: timeStr });
  const agSpan = el("span", { class: "ag", text: ev.source || "" });
  const lvClass = getLogClass(ev.level, ev.code);
  // ORB-10972: the dock is 336px, so the level is carried by a coloured
  // keyline on the row rather than a 42px text column — that width goes to the
  // message instead. The class stays on the span for the filter code, which
  // reads it back out of dataset.
  row.classList.add(`lv-${lvClass}`);
  const lvSpan = el("span", { class: `lv ${lvClass}`, title: ev.code || "", text: ev.code || "" });
  const mSpan = el("span", { class: "m" });
  mSpan.innerHTML = ev.message_html || "";

  row.appendChild(tSpan);
  row.appendChild(agSpan);
  row.appendChild(lvSpan);
  row.appendChild(mSpan);

  // Click to expand/collapse the full message
  row.addEventListener("click", () => row.classList.toggle("expanded"));

  return row;
}

export function initLogTail() {
  wireLogPanelResize();
  fitLogPanelToViewport();
  fetchJson("/api/log?limit=50").then((payload) => {
    const inner = $("logInner");
    if (!inner) return;
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
    }
    events.slice().reverse().forEach(ev => {
      const row = renderLogEvent(ev, false);
      inner.appendChild(row);
      logRows.push(row);
    });
    applyLogFilters();
    if (events.length > 0) updateLogStatusBar(events[events.length - 1]);
    connectLogStream();
  }).catch(console.error);
  
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

  document.querySelectorAll("#side-dock .filter-pill").forEach(pill => {
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
  for (const ev of logBuffered) {
    const row = renderLogEvent(ev, true);
    inner.insertBefore(row, inner.firstChild);
    logRows.unshift(row);
    setTimeout(() => row.classList.remove("fresh"), 600);
  }
  logBuffered = [];
  const btnBuffered = $("log-buffered-count");
  if (btnBuffered) btnBuffered.style.display = "none";
  enforceLogBounds();
  if (!wasEmpty) applyLogFilters();
}

function enforceLogBounds() {
  if (logRows.length > 250) {
    const toRemove = logRows.splice(200);
    for (const row of toRemove) {
      row.remove();
    }
  }
}

// The pills are toggle buttons: `on` carries the visual state and `aria-pressed`
// carries the same fact for assistive tech, so both are written from the one
// active-filter set rather than from the click target.
function syncLogFilterPills() {
  for (const pill of document.querySelectorAll("#side-dock .filter-pill")) {
    const on = activeLogFilters.has(pill.dataset.filter);
    pill.classList.toggle("on", on);
    pill.setAttribute("aria-pressed", String(on));
  }
}

function applyLogFilters() {
  let visibleCount = 0;
  for (const row of logRows) {
    const code = row.dataset.code;
    const level = row.dataset.level;
    const lvClass = getLogClass(level, code);
    
    let show = false;
    if (activeLogFilters.has("all")) {
      show = true;
    } else {
      if (activeLogFilters.has("err") && lvClass === "err") show = true;
      if (activeLogFilters.has("deny") && lvClass === "deny") show = true;
      if (activeLogFilters.has("warn") && lvClass === "warn") show = true;
    }
    row.style.display = show ? "" : "none";
    if (show) visibleCount++;
  }
  
  const cnt = $("log-count");
  if (cnt) cnt.textContent = `${visibleCount}`;
}

function rememberStreamOffset(lastEventId) {
  if (!lastEventId) return;
  const parsed = Number.parseInt(lastEventId, 10);
  if (Number.isFinite(parsed) && parsed >= 0) logStreamOffset = parsed;
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
}

function connectLogStream() {
  if (logStreamRetryTimer !== null) {
    clearTimeout(logStreamRetryTimer);
    logStreamRetryTimer = null;
  }
  if (logStream) {
    logStream.close();
    logStream = null;
  }
  logStream = new EventSource(`/api/log/stream?from=${encodeURIComponent(String(logStreamOffset))}`);
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
      updateLogStatusBar(ev);
      if (logFollowTail) {
        const inner = $("logInner");
        const row = renderLogEvent(ev, true);
        inner.insertBefore(row, inner.firstChild);
        logRows.unshift(row);
        applyLogFilters();
        enforceLogBounds();
        setTimeout(() => row.classList.remove("fresh"), 600);
      } else {
        logBuffered.push(ev);
        const btn = $("log-buffered-count");
        if (btn) {
          btn.textContent = `${logBuffered.length} buffered`;
          btn.style.display = "";
        }
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
