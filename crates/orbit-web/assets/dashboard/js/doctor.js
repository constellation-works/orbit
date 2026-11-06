// Orbit dashboard Health › Doctor [ORB-14830].
// Pure vanilla JS, split into ES modules with no build step.
//
// Renders `GET /api/doctor`: the `orbit doctor` checks for the selected
// workspace on the selected host, with the same fields as `orbit doctor
// --json`. The panel is read-only. A repair is a CLI command the remediation
// names, which the panel offers to copy; the endpoint takes no fix flag.
//
// Doctor is slow enough (seconds on a large host) that the dashboard never
// runs it on its poll. Opening the view asks for a recent report, which the
// server runs only when it has none; Refresh asks for a new run; and the poll
// asks only for whatever the server has cached, to keep the Health rail count
// and the report's age current.
//
// The page is a work queue: the operator reads what is wrong, runs the fix and
// refreshes. Problems are one-line rows grouped by severity; the selected
// check's fix and full message sit in a pane beside them, with the first problem
// selected on open. A narrow screen has no room for a pane, so the same node
// opens under the row the operator taps.

import {
  captureWorkspaceVisit,
  copyWithFeedback,
  detailsPanel,
  el,
  formatAge,
  formatDateTime,
  isAggregateView,
  onWorkspaceChange,
  renderPanelPlaceholder,
  syncNodes,
} from './common.js';
import { forecastHint } from './forecast-shared.js';

const $ = (id) => document.getElementById(id);

// The server bounds one run at 45 s, under the host forward's 60 s bound.
// Leave room for the forward to open its tunnel on the first request.
const DOCTOR_TIMEOUT_MS = 75_000;

// Problems first, in the order an operator acts on them.
// An `info` row is a fact about the setup, not a problem: it folds away with
// the passing checks.
const STATUS_ORDER = ["error", "warning", "ok", "info", "skipped"];
const QUIET_STATUSES = new Set(["ok", "info", "skipped"]);

// Matches the `.doctor-triage` breakpoint in health.css.
const NARROW_LAYOUT = window.matchMedia("(max-width: 900px)");

const PANEL_HINT = "Read-only: repairs run with the CLI. Refresh runs doctor again; otherwise this is the cached result.";

// The report shown, for the host and workspace visit it was read in, with the
// request that read it (see `supersedes`).
let current = null;
// The unavailable or failed state of the last request, for the same visit.
let failure = null;
let running = null;
// How many doctor requests this tab has sent: each request's sequence number.
let issued = 0;
// The check the operator picked, or null for the default: the first problem,
// or on a narrow screen nothing.
let selectedKey = null;
// The rows of the rendered report by selection key.
let rowsByKey = new Map();

onWorkspaceChange(() => {
  current = null;
  failure = null;
  running = null;
  selectedKey = null;
  setHealthRailCount(null);
});

function visitIsCurrent(entry) {
  return Boolean(entry && entry.visit.isCurrent());
}

/// Read `/api/doctor` in `mode`: `recent`, `refresh` or `cached`. An older
/// host's dashboard has no such route; its plain 404 (no error text, unlike an
/// unknown workspace) is reported as `unavailable` rather than as a failure.
async function requestDoctor(visit, mode) {
  const query = mode === "refresh" ? "?refresh=true" : mode === "cached" ? "?cached=true" : "";
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), DOCTOR_TIMEOUT_MS);
  try {
    const res = await fetch(visit.path(`/api/doctor${query}`), {
      headers: { accept: "application/json" },
      signal: controller.signal,
    });
    const text = await res.text();
    let body = null;
    try {
      body = text ? JSON.parse(text) : null;
    } catch (_) {
      body = null;
    }
    if (res.ok) return { body };
    const message = body && typeof body.error === "string" ? body.error.trim() : "";
    if (res.status === 404 && !message) return { unavailable: true };
    const error = new Error(message || `/api/doctor: HTTP ${res.status}`);
    error.status = res.status;
    if (body && typeof body.code === "string") error.code = body.code;
    throw error;
  } catch (error) {
    if (controller.signal.aborted) {
      throw new Error(`doctor did not answer within ${DOCTOR_TIMEOUT_MS / 1000} seconds`);
    }
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

/// Whether the response to request `seq`, read in `mode`, may replace the
/// report `shown`. The server's cache only moves forward, so a request sent
/// after the shown report arrived answers with that report or a newer one.
/// A request that overlapped it may carry an older report however late it
/// arrives: of those, only a refresh, whose run starts after the request
/// reached the server, replaces a report that no later refresh produced.
/// A newer report that loses here is shown by the next poll.
function supersedes(seq, mode, shown) {
  if (seq > shown.after) return true;
  return mode === "refresh" && (shown.mode !== "refresh" || seq > shown.seq);
}

function adopt(visit, seq, mode, body) {
  current = { visit, body, seq, mode, after: issued, receivedAt: Date.now() };
}

/// Run (or reuse) the report for the selected workspace and render it.
/// `refresh` asks the server for a run that starts after this request.
async function load(mode) {
  const visit = captureWorkspaceVisit();
  if (running && running.visit.isCurrent() && (mode !== "refresh" || running.mode === "refresh")) {
    return running.promise;
  }
  const entry = { visit, mode };
  const seq = ++issued;
  // A response that a report shown since has superseded changes nothing,
  // whether it carries a report or a failure.
  const settles = () => visit.isCurrent() && (!visitIsCurrent(current) || supersedes(seq, mode, current));
  entry.promise = (async () => {
    try {
      const result = await requestDoctor(visit, mode);
      if (!settles()) return;
      if (result.unavailable) {
        current = null;
        failure = { visit, unavailable: true };
      } else if (result.body && Array.isArray(result.body.checks)) {
        adopt(visit, seq, mode, result.body);
        failure = null;
      } else {
        failure = { visit, message: "the server returned no report" };
      }
    } catch (error) {
      if (!settles()) return;
      failure = { visit, message: error.message || String(error) };
    } finally {
      if (running === entry) running = null;
      if (visit.isCurrent()) renderDoctor();
    }
  })();
  // The request settles no sooner than the next microtask, so the busy state
  // is installed and painted before its `finally` can clear it.
  running = entry;
  renderDoctor();
  return entry.promise;
}

/// The view was opened or the dashboard ticked while it is open: show the
/// report, reading one only if this visit has none yet.
export function openDoctor() {
  if (isAggregateView()) {
    renderPanelPlaceholder("doctor-body");
    renderMeta();
    return Promise.resolve();
  }
  if (visitIsCurrent(current) || (failure && failure.visit.isCurrent())) {
    renderDoctor();
    return Promise.resolve();
  }
  return load("recent");
}

/// The panel's Refresh button: run doctor again.
export function refreshDoctor() {
  if (isAggregateView()) return Promise.resolve();
  return load("refresh");
}

/// Every dashboard tick: read the cached report without running doctor, so
/// the Health rail count and the panel's age follow runs made from any tab.
export async function peekDoctor() {
  if (isAggregateView()) {
    setHealthRailCount(null);
    return;
  }
  const visit = captureWorkspaceVisit();
  const seq = ++issued;
  let result;
  try {
    result = await requestDoctor(visit, "cached");
  } catch (_) {
    // The rail count is advisory; the panel itself reports failures.
    return;
  }
  if (!visit.isCurrent()) return;
  if (result.unavailable) {
    setHealthRailCount(null);
    return;
  }
  const body = result.body;
  if (!body || !Array.isArray(body.checks)) {
    // Nothing cached on the server; keep what this tab already shows.
    if (!visitIsCurrent(current)) setHealthRailCount(null);
    return;
  }
  if (visitIsCurrent(current) && !supersedes(seq, "cached", current)) return;
  adopt(visit, seq, "cached", body);
  failure = null;
  renderDoctor();
}

function count(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function durationText(ms) {
  if (typeof ms !== "number" || !Number.isFinite(ms)) return "-";
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
}

/// When the shown report ran, on the browser's clock: the server measures the
/// age on its own clock, so a skewed remote host still reads correctly.
function ranAt(entry) {
  return new Date(entry.receivedAt - count(entry.body.age_ms));
}

function setHealthRailCount(body, ran) {
  const node = $("rail-count-diagnostics");
  if (!node) return;
  const failures = body ? count(body.failures) : 0;
  const warnings = body ? count(body.warnings) : 0;
  if (!body || (failures === 0 && warnings === 0)) {
    node.textContent = "";
    node.classList.remove("alert");
    node.title = "";
    return;
  }
  const parts = [];
  if (failures) parts.push(`${failures} err`);
  if (warnings) parts.push(`${warnings} warn`);
  node.textContent = parts.join(" · ");
  node.classList.toggle("alert", failures > 0);
  node.title = `orbit doctor${ran ? `, run ${formatAge(ran)} ago` : ""}: ${plural(failures, "failure")}, ${plural(warnings, "warning")}. See Health › Doctor.`;
}

function renderMeta() {
  const meta = $("doctor-meta");
  const countNode = $("doctor-count");
  const button = $("doctor-refresh");
  const busy = Boolean(running && running.visit.isCurrent());
  if (button) {
    button.disabled = busy || isAggregateView();
    button.textContent = busy ? "Running…" : "Refresh";
  }
  if (!meta || !countNode) return;
  if (isAggregateView()) {
    meta.textContent = "Select a workspace to run doctor";
    meta.title = "";
    countNode.textContent = "-";
    return;
  }
  if (!visitIsCurrent(current)) {
    const unavailable = failure && failure.unavailable && failure.visit.isCurrent();
    meta.textContent = busy ? "Running doctor…" : unavailable ? "Not available on this host" : "Not run yet";
    meta.title = "";
    countNode.textContent = "-";
    return;
  }
  const body = current.body;
  const ran = ranAt(current);
  const age = formatAge(ran);
  meta.textContent = `Ran ${age} ago · took ${durationText(body.duration_ms)}${busy ? " · running again…" : ""}`;
  meta.title = `Ran at ${formatDateTime(ran, { seconds: true })}`;
  const checks = Array.isArray(body.checks) ? body.checks : [];
  countNode.textContent = `${plural(count(body.failures), "failure")} · ${plural(count(body.warnings), "warning")} · ${plural(checks.length, "check")}`;
}

/// A remediation with each backtick-quoted command shown as code and given
/// its own copy button. An unmatched backtick stays literal text.
function remediationChildren(text) {
  const parts = String(text).split("`");
  if (parts.length % 2 === 0) {
    const tail = parts.splice(parts.length - 2, 2).join("`");
    parts.push(tail);
  }
  const children = [];
  parts.forEach((part, index) => {
    if (!part) return;
    if (index % 2 === 0) {
      children.push(part);
      return;
    }
    const copy = el("button", { class: "doctor-copy", text: "copy", title: `Copy: ${part}`, type: "button" });
    copy.setAttribute("aria-live", "polite");
    copy.setAttribute("aria-label", `Copy command ${part}`);
    copy.addEventListener("click", () => copyWithFeedback(copy, part));
    children.push(el("span", { class: "doctor-command" }, [el("code", { text: part }), copy]));
  });
  return children;
}

function statusClass(row) {
  return STATUS_ORDER.includes(row.status) ? row.status : "unknown";
}

/// One queue row: the check and one clipped line of its message. It stays the
/// same node as the selection moves, so a poll never takes focus from it.
function queueRowNode({ row, key }) {
  const button = el("button", { class: `doctor-row ${statusClass(row)}`, type: "button" }, [
    el("span", { class: "doctor-check", text: row.check || "-" }),
    row.remediation ? el("span", { class: "doctor-fix-flag", text: "Fix" }) : null,
    el("span", { class: "doctor-preview", text: row.message || "" }),
  ]);
  button.dataset.check = key;
  button.setAttribute("aria-controls", "doctor-detail");
  button.addEventListener("click", (event) => selectCheck(key, event.detail === 0));
  return button;
}

/// Rows of one status under a heading that names it. A heading replaces the
/// status word each row would otherwise repeat.
function statusGroupNode(status, entries) {
  const group = el("div", { class: "doctor-group", role: "group", "aria-label": status }, [
    el("h3", { class: `doctor-group-title ${statusClass({ status })}` }, [
      el("span", { text: status }),
      el("span", { class: "doctor-group-count", text: String(entries.length) }),
    ]),
    el("div", { class: "doctor-list" }, entries.map(queueRowNode)),
  ]);
  return group;
}

/// Each status in `entries`, in report order of severity, as its own group.
function statusGroups(entries) {
  const groups = new Map();
  for (const entry of entries) {
    const status = entry.row.status || "unknown";
    if (!groups.has(status)) groups.set(status, []);
    groups.get(status).push(entry);
  }
  return [...groups].map(([status, rows]) => statusGroupNode(status, rows));
}

/// What the pane shows for one check. The row already says what is wrong, so
/// the fix leads; a long message cannot push it off the screen.
function detailChildren(row) {
  return [
    el("div", { class: "doctor-detail-head" }, [
      el("h3", { class: "doctor-detail-check", text: row.check || "-" }),
      el("span", { class: `doctor-status ${statusClass(row)}`, text: row.status || "unknown" }),
      el("span", { class: "doctor-duration", text: durationText(row.duration_ms), title: "How long this check took" }),
    ]),
    row.remediation
      ? el("div", { class: "doctor-remediation" }, [
        el("span", { class: "doctor-remediation-label", text: "Fix" }),
        el("span", { class: "doctor-remediation-text" }, remediationChildren(row.remediation)),
      ])
      : null,
    el("p", { class: "doctor-message", text: row.message || "" }),
  ];
}

function triageNode() {
  const body = $("doctor-body");
  return body ? body.querySelector(":scope > .doctor-triage") : null;
}

/// Show the selection: mark its row and put the pane beside the queue, or on a
/// narrow screen directly under the row. A selected passing row behind a closed
/// fold no longer counts: a check that a fix just turned passing hands over to
/// the next problem.
function applySelection() {
  const triage = triageNode();
  if (!triage) return;
  const detail = triage.querySelector(".doctor-detail");
  const fold = triage.querySelector(".doctor-passing");
  const rows = [...triage.querySelectorAll(".doctor-row")];
  const showing = (row) => !(fold && fold.contains(row)) || fold.open;
  let chosen = rows.find((row) => row.dataset.check === selectedKey && showing(row));
  if (!chosen) {
    selectedKey = null;
    chosen = NARROW_LAYOUT.matches ? undefined : rows.find((row) => !(fold && fold.contains(row)));
  }
  for (const row of rows) row.setAttribute("aria-expanded", String(row === chosen));
  const pane = Boolean(chosen) && !NARROW_LAYOUT.matches;
  triage.classList.toggle("has-detail", pane);
  detail.hidden = !chosen;
  // The pane scrolls by itself, so the keyboard must be able to reach it.
  detail.tabIndex = pane ? 0 : -1;
  if (!chosen) {
    detail.dataset.shown = "";
    detail.replaceChildren();
    if (detail.parentNode !== triage) triage.append(detail);
    return;
  }
  if (detail.dataset.shown !== chosen.dataset.check) {
    detail.dataset.shown = chosen.dataset.check;
    detail.replaceChildren(...detailChildren(rowsByKey.get(chosen.dataset.check)).filter(Boolean));
    detail.scrollTop = 0;
  }
  if (pane) {
    if (detail.parentNode !== triage) triage.append(detail);
  } else if (chosen.nextElementSibling !== detail) {
    chosen.after(detail);
  }
}

/// A row was activated. In the pane layout the keyboard follows into the pane,
/// which would otherwise be every later row away; Escape returns. A narrow
/// layout's row is a plain disclosure, so activating it again closes it.
function selectCheck(key, fromKeyboard) {
  const triage = triageNode();
  if (!triage) return;
  const open = triage.querySelector('.doctor-row[aria-expanded="true"]');
  selectedKey = NARROW_LAYOUT.matches && open && open.dataset.check === key ? null : key;
  applySelection();
  if (fromKeyboard && !NARROW_LAYOUT.matches) triage.querySelector(".doctor-detail").focus();
}

function stateNode(key, text, className = "empty-state") {
  const node = el("div", { class: className }, [el("div", { class: "text", text })]);
  node.dataset.key = key;
  node.dataset.hash = text;
  return node;
}

function renderBody() {
  const body = $("doctor-body");
  if (!body) return;
  body.setAttribute("aria-busy", running && running.visit.isCurrent() ? "true" : "false");
  if (failure && failure.visit.isCurrent() && failure.unavailable) {
    syncNodes(body, [stateNode(
      "doctor-unavailable",
      "Doctor is not available on this host: its Orbit dashboard predates the Doctor panel. Run orbit doctor on the host itself, or upgrade Orbit there.",
    )]);
    return;
  }
  const nodes = [];
  if (failure && failure.visit.isCurrent()) {
    const prefix = visitIsCurrent(current) ? "Doctor failed; showing the previous result" : "Doctor failed";
    nodes.push(stateNode("doctor-error", `${prefix}: ${failure.message}. Use Refresh to retry.`, "panel-placeholder action-error"));
  }
  if (!visitIsCurrent(current)) {
    if (!nodes.length) {
      nodes.push(stateNode("doctor-loading", running ? "Running orbit doctor…" : "Not run yet.", "panel-placeholder"));
    }
    syncNodes(body, nodes);
    return;
  }
  const checks = Array.isArray(current.body.checks) ? current.body.checks : [];
  // A check's name can repeat in a report, and each row must stay selectable.
  const seen = new Map();
  const keyed = checks.map((row, index) => {
    const occurrence = seen.get(row.check) || 0;
    seen.set(row.check, occurrence + 1);
    return { row, index, key: `${row.check}#${occurrence}` };
  });
  rowsByKey = new Map(keyed.map(({ row, key }) => [key, row]));
  const ordered = [...keyed].sort((a, b) => {
    const rank = (row) => {
      const at = STATUS_ORDER.indexOf(row.status);
      return at < 0 ? 0 : at;
    };
    return rank(a.row) - rank(b.row) || a.index - b.index;
  });
  const problems = ordered.filter(({ row }) => !QUIET_STATUSES.has(row.status));
  const quiet = ordered.filter(({ row }) => QUIET_STATUSES.has(row.status));
  const queue = el("div", { class: "doctor-queue" });
  if (problems.length) {
    queue.append(...statusGroups(problems));
  } else {
    queue.append(stateNode("doctor-healthy", `All ${plural(checks.length, "check")} passed or were skipped.`));
  }
  if (quiet.length) {
    const details = detailsPanel("doctor:passing", { class: "doctor-passing" });
    const ok = quiet.filter(({ row }) => row.status === "ok").length;
    const info = quiet.filter(({ row }) => row.status === "info").length;
    const summary = [plural(ok, "passing check"), info ? `${info} info` : null, `${quiet.length - ok - info} skipped`];
    details.append(el("summary", { text: summary.filter(Boolean).join(" · ") }), ...statusGroups(quiet));
    details.addEventListener("toggle", applySelection);
    queue.append(details);
  }
  const detail = el("aside", { class: "doctor-detail", id: "doctor-detail", "aria-label": "Selected check" });
  detail.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    const triage = triageNode();
    const open = triage && triage.querySelector('.doctor-row[aria-expanded="true"]');
    if (open) open.focus();
  });
  const triage = el("div", { class: "doctor-triage" }, [queue, detail]);
  triage.dataset.key = "doctor-triage";
  triage.dataset.hash = JSON.stringify(checks);
  nodes.push(triage);
  syncNodes(body, nodes);
  applySelection();
}

export function renderDoctor() {
  renderMeta();
  renderBody();
  setHealthRailCount(visitIsCurrent(current) ? current.body : null, visitIsCurrent(current) ? ranAt(current) : null);
}

export function wireDoctorPanel() {
  const button = $("doctor-refresh");
  if (button) button.addEventListener("click", () => { void refreshDoctor(); });
  const meta = $("doctor-meta");
  if (meta) meta.after(forecastHint(PANEL_HINT));
  NARROW_LAYOUT.addEventListener("change", applySelection);
}
