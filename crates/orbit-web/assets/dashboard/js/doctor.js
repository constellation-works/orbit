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

import {
  captureWorkspaceVisit,
  copyWithFeedback,
  el,
  formatAge,
  formatDateTime,
  isAggregateView,
  onWorkspaceChange,
  renderPanelPlaceholder,
  syncNodes,
} from './common.js';

const $ = (id) => document.getElementById(id);

// The server bounds one run at 45 s, under the host forward's 60 s bound.
// Leave room for the forward to open its tunnel on the first request.
const DOCTOR_TIMEOUT_MS = 75_000;

// Problems first, in the order an operator acts on them.
const STATUS_ORDER = ["error", "warning", "ok", "skipped"];

// The report shown, for the host and workspace visit it was read in.
let current = null;
// The unavailable or failed state of the last request, for the same visit.
let failure = null;
let running = null;
let detailsOpen = false;

onWorkspaceChange(() => {
  current = null;
  failure = null;
  running = null;
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

function adopt(visit, body) {
  current = { visit, body, receivedAt: Date.now() };
}

/// Run (or reuse) the report for the selected workspace and render it.
/// `refresh` asks the server for a run that starts after this request.
async function load(mode) {
  const visit = captureWorkspaceVisit();
  if (running && running.visit.isCurrent() && (mode !== "refresh" || running.mode === "refresh")) {
    return running.promise;
  }
  const entry = { visit, mode };
  entry.promise = (async () => {
    renderDoctor();
    try {
      const result = await requestDoctor(visit, mode);
      if (!visit.isCurrent()) return;
      if (result.unavailable) {
        current = null;
        failure = { visit, unavailable: true };
      } else if (result.body && Array.isArray(result.body.checks)) {
        adopt(visit, result.body);
        failure = null;
      } else {
        failure = { visit, message: "the server returned no report" };
      }
    } catch (error) {
      if (!visit.isCurrent()) return;
      failure = { visit, message: error.message || String(error) };
    } finally {
      if (running === entry) running = null;
      if (visit.isCurrent()) renderDoctor();
    }
  })();
  running = entry;
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
  // The server keeps only its latest run, so this is never older than ours.
  adopt(visit, body);
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
  meta.textContent = `Ran ${age} ago · took ${durationText(body.duration_ms)} · read-only; repairs run with the CLI${busy ? " · running again…" : ""}`;
  meta.title = `Ran at ${formatDateTime(ran, { seconds: true })}. Refresh runs doctor again; otherwise this is the cached result.`;
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

function rowNode(row, index) {
  const status = STATUS_ORDER.includes(row.status) ? row.status : "unknown";
  const article = el("article", { class: `doctor-row ${status}` }, [
    el("div", { class: "doctor-row-head" }, [
      el("span", { class: `doctor-status ${status}`, text: status }),
      el("span", { class: "doctor-check", text: row.check || "-" }),
      el("span", { class: "doctor-duration", text: durationText(row.duration_ms), title: "How long this check took" }),
    ]),
    el("p", { class: "doctor-message", text: row.message || "" }),
    row.remediation
      ? el("div", { class: "doctor-remediation" }, [
        el("span", { class: "doctor-remediation-label", text: "Fix" }),
        el("span", { class: "doctor-remediation-text" }, remediationChildren(row.remediation)),
      ])
      : null,
  ]);
  article.dataset.key = `doctor-${index}-${row.check}`;
  article.dataset.hash = JSON.stringify(row);
  return article;
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
  const ordered = checks
    .map((row, index) => ({ row, index }))
    .sort((a, b) => {
      const rank = (row) => {
        const at = STATUS_ORDER.indexOf(row.status);
        return at < 0 ? 0 : at;
      };
      return rank(a.row) - rank(b.row) || a.index - b.index;
    });
  const problems = ordered.filter(({ row }) => row.status !== "ok" && row.status !== "skipped");
  const quiet = ordered.filter(({ row }) => row.status === "ok" || row.status === "skipped");
  if (problems.length) {
    const list = el("div", { class: "doctor-list" }, problems.map(({ row, index }) => rowNode(row, index)));
    list.dataset.key = "doctor-problems";
    list.dataset.hash = JSON.stringify(problems);
    nodes.push(list);
  } else {
    nodes.push(stateNode("doctor-healthy", `All ${plural(checks.length, "check")} passed or were skipped.`));
  }
  if (quiet.length) {
    const details = el("details", { class: "doctor-passing" });
    details.open = detailsOpen;
    details.addEventListener("toggle", () => {
      if (details.isConnected) detailsOpen = details.open;
    });
    const ok = quiet.filter(({ row }) => row.status === "ok").length;
    details.append(
      el("summary", { text: `${plural(ok, "passing check")} · ${quiet.length - ok} skipped` }),
      el("div", { class: "doctor-list" }, quiet.map(({ row, index }) => rowNode(row, index))),
    );
    details.dataset.key = "doctor-passing";
    details.dataset.hash = JSON.stringify([quiet, detailsOpen]);
    nodes.push(details);
  }
  syncNodes(body, nodes);
}

export function renderDoctor() {
  renderMeta();
  renderBody();
  setHealthRailCount(visitIsCurrent(current) ? current.body : null, visitIsCurrent(current) ? ranAt(current) : null);
}

export function wireDoctorPanel() {
  const button = $("doctor-refresh");
  if (button) button.addEventListener("click", () => { void refreshDoctor(); });
}
