// Orbit dashboard run-detail (Run Detail tab: meta header, steps list, knowledge-pack panel,
// Gantt timeline, event log, per-step stdout/stderr blocks).
// Pure vanilla JS, split into ES module with no build step.
//
// Extracted from app.js (ORB-00181). Owns RUN_EVENTS_LIMIT, the six run-detail state lets,
// RUN_EVENT_COLUMNS, renderRunDetail*, renderRunSteps*, renderRunKnowledge, renderRunGantt +
// tooltip fns, logsForStep/build*, renderRunEvents, summarizeEvent.
// Fetch orchestrators (fetchAndRenderRun*) and refresh wiring remain in app.js as the
// cross-domain shell. Gantt retry markers read activeRunEvents (populated by separate fetch)
// so both Gantt and Events renderers live here for local coupling.
//
// Receives one-time context via initRunDetail(runDetailContext()) with state accessors
// (re-exported by app's routerContext), formatters, run-action builders (from runs.js),
// and setRunDetailSubtab (from router.js) for the Gantt click-to-steps behavior.
// No behavior change: identical rendering, expand/collapse, tooltips, routing, subtab
// activation, and scroll-to-step.

import { el, syncNodes, stateCell, positiveIntParam, makeToggleRow, getWorkspace, getWorkspaceRevision, onWorkspaceChange } from './common.js';
import { buildExecutionProvenance } from './distributed.js';

const $ = (id) => document.getElementById(id);

const RUN_EVENTS_LIMIT = positiveIntParam("events", 100);  // re-export for app orchestrators
const LIVE_RUN_STATES = new Set(["pending", "running", "retrying"]);
const TERMINAL_RUN_STATES = new Set(["success", "failed", "timeout", "cancelled", "interrupted", "held"]);

// Run detail module-scoped state (was in app.js)
let activeRunId = null;
let activeRunDetail = null;
let activeRunEvents = [];
let activeRunEventsError = null;
let activeRunLogs = [];
let activeRunLogsError = null;
let activeRunSubtab = "steps";
let expandedStepIndices = new Set();

let _runDetailCtx = null;

function hasCtx(key) {
  const c = _runDetailCtx;
  return !!(c && typeof c[key] === "function");
}

// --- wrappers for ctx-provided values (formatters, callbacks, builders) ---

function fmtTimestamp(v) {
  return hasCtx("fmtTimestamp") ? _runDetailCtx.fmtTimestamp(v) : (v || "-");
}

function fmtDuration(v) {
  return hasCtx("fmtDuration") ? _runDetailCtx.fmtDuration(v) : (v == null ? "-" : String(v));
}

function fmtAbsTime(v) {
  return hasCtx("fmtAbsTime") ? _runDetailCtx.fmtAbsTime(v) : (v || "-");
}

function fmtRelative(v) {
  return hasCtx("fmtRelative") ? _runDetailCtx.fmtRelative(v) : (v || "-");
}

function truncate(text, max = 200) {
  if (hasCtx("truncate")) return _runDetailCtx.truncate(text, max);
  if (text == null) return "";
  return String(text).slice(0, max);
}

function setRunDetailSubtab(name) {
  if (hasCtx("setRunDetailSubtab")) {
    try { _runDetailCtx.setRunDetailSubtab(name); } catch (_) {}
  }
}

function navigateToRun(runId) {
  if (hasCtx("navigateToRun")) {
    try { _runDetailCtx.navigateToRun(runId); } catch (_) {}
  }
}

function setActiveTab(tab) {
  if (hasCtx("setActiveTab")) {
    try { _runDetailCtx.setActiveTab(tab); } catch (_) {}
  }
}

function runIsCancellable(run) {
  return hasCtx("runIsCancellable") ? _runDetailCtx.runIsCancellable(run) : false;
}

function buildCancelRunButton(run, host) {
  return hasCtx("buildCancelRunButton") ? _runDetailCtx.buildCancelRunButton(run, host) : null;
}

function buildReplayRunButton(run, host) {
  const button = hasCtx("buildReplayRunButton") ? _runDetailCtx.buildReplayRunButton(run, host) : null;
  if (button && !TERMINAL_RUN_STATES.has(run.state)) {
    button.disabled = true;
    button.title = "Replay is available after this run finishes.";
  }
  return button;
}

// --- public state accessors (re-exported by app.js routerContext + runDetailContext) ---

// One generation per fetch channel. Navigation and a newer fetch of the same
// channel both move it, so a response is applied only when the run, workspace,
// revision, and generation it captured are still the ones on screen. A return
// to the same run (A → B → A) and a workspace change that keeps the run id
// both miss that check.
const runDetailFetchGeneration = { detail: 0, events: 0, logs: 0 };

function bumpRunDetailFetches() {
  runDetailFetchGeneration.detail += 1;
  runDetailFetchGeneration.events += 1;
  runDetailFetchGeneration.logs += 1;
}

export function beginRunDetailFetch(channel) {
  return {
    runId: activeRunId,
    workspace: getWorkspace(),
    revision: getWorkspaceRevision(),
    generation: ++runDetailFetchGeneration[channel],
  };
}

export function runDetailFetchCurrent(channel, token) {
  return !!token
    && runDetailFetchGeneration[channel] === token.generation
    && activeRunId === token.runId
    && getWorkspace() === token.workspace
    && getWorkspaceRevision() === token.revision;
}

// Drop the previous run's data and action buttons before the next paint.
// Waiting for the in-flight response would leave its cancel/replay targets
// mounted under the new run or workspace.
function retireRunDetailView() {
  bumpRunDetailFetches();
  activeRunDetail = null;
  activeRunEvents = [];
  activeRunEventsError = null;
  activeRunLogs = [];
  activeRunLogsError = null;
  expandedStepIndices = new Set();
  if (typeof document !== "undefined" && document.getElementById("run-detail-meta")) {
    renderRunDetailEmpty(activeRunId ? "Loading run…" : "No run selected.");
  }
}

export function getActiveRunId() { return activeRunId; }
export function setActiveRunId(v) {
  if (v === activeRunId) return;
  activeRunId = v;
  retireRunDetailView();
}

onWorkspaceChange(retireRunDetailView);

export function getActiveRunDetail() { return activeRunDetail; }
export function setActiveRunDetail(v) { activeRunDetail = v; }

export function getActiveRunEvents() { return activeRunEvents; }
export function setActiveRunEvents(v) {
  activeRunEvents = v || [];
  activeRunEventsError = null;
}

export function setActiveRunEventsError(v) { activeRunEventsError = v || null; }

export function getActiveRunLogs() { return activeRunLogs; }
export function setActiveRunLogs(v) {
  activeRunLogs = v || [];
  activeRunLogsError = null;
}

export function setActiveRunLogsError(v) { activeRunLogsError = v || null; }

export function getActiveRunSubtab() { return activeRunSubtab; }
export function setActiveRunSubtab(v) { activeRunSubtab = v || "steps"; }

export function getExpandedStepIndices() { return expandedStepIndices; }
export function setExpandedStepIndices(v) {
  if (v instanceof Set) {
    expandedStepIndices = v;
  } else {
    expandedStepIndices = new Set(v || []);
  }
}
export function clearExpandedStepIndices() { expandedStepIndices.clear(); }
export function toggleExpandedStepIndex(idx) {
  const n = Number(idx);
  if (expandedStepIndices.has(n)) expandedStepIndices.delete(n);
  else expandedStepIndices.add(n);
}

export function initRunDetail(ctx) {
  _runDetailCtx = ctx;
}

export { RUN_EVENTS_LIMIT };

// --- renderers (exported for app orchestrators and runDetailContext) ---

export function renderRunDetailEmpty(message) {
  const meta = $("run-detail-meta");
  if (meta) syncNodes(meta, [el("div", { class: "empty-state" }, [
    el("div", { class: "icon", text: "✧" }),
    el("div", { class: "text", text: message }),
  ])]);
  const title = $("run-detail-title");
  if (title) title.textContent = "Run Detail";
  const count = $("run-detail-count");
  if (count) count.textContent = "-";
  const steps = $("run-steps-body");
  if (steps) steps.innerHTML = "";
  const events = $("run-events-body");
  if (events) events.innerHTML = "";
  const knowledge = $("run-knowledge-panel");
  const gantt = $("run-gantt-panel");
  if (knowledge) knowledge.style.display = "none";
  if (gantt) gantt.style.display = "none";
}

export function renderRunDetailMeta() {
  const meta = $("run-detail-meta");
  if (!meta) return;
  const detail = activeRunDetail || {};
  const run = detail.run || {};
  $("run-detail-title").textContent = `Run ${run.run_id || activeRunId || "?"}`;
  const stepCount = Array.isArray(detail.steps) ? detail.steps.length : 0;
  $("run-detail-count").textContent = `${stepCount} ${stepCount === 1 ? "step" : "steps"}`;

  const grid = el("div", { class: "run-meta-grid" });
  const addCell = (label, value) => {
    const cell = el("div");
    cell.appendChild(el("div", { class: "label", text: label }));
    cell.appendChild(el("div", { class: "value", text: value == null ? "-" : String(value) }));
    grid.appendChild(cell);
  };
  addCell("job", run.job_id);
  {
    const cell = el("div");
    cell.appendChild(el("div", { class: "label", text: "state" }));
    cell.appendChild(el("div", { class: "value" }, [stateCell(run.state || "unknown")]));
    grid.appendChild(cell);
  }
  addCell("attempt", run.attempt);
  addCell("started", run.started_at ? fmtAbsTime(run.started_at) : "-");
  addCell("finished", run.finished_at ? fmtAbsTime(run.finished_at) : "-");
  addCell("duration", run.duration_ms != null ? fmtDuration(run.duration_ms) : "-");
  // ORB-12516: with more than one execution host, a run id alone no longer says
  // where it ran. The store field is the truth; a row without one is *unknown*,
  // never assumed to be this machine.
  {
    const cell = el("div");
    cell.appendChild(el("div", { class: "label", text: "executed on" }));
    const value = el("div", { class: "value" });
    value.appendChild(buildExecutionProvenance(runExecutionLocation(run)));
    cell.appendChild(value);
    grid.appendChild(cell);
  }

  const wrap = el("div");
  const back = el("button", { class: "back-action", text: "← Runs" });
  back.addEventListener("click", () => setActiveTab("diagnostics/runs"));
  const actions = el("div", { class: "run-detail-actions" }, [back]);
  if (run.retry_source_run_id) {
    const sourceId = run.retry_source_run_id;
    const lineage = el("button", {
      class: "back-action replay-source",
      text: `Replayed from ${sourceId}`,
      title: `Open ${sourceId}`,
    });
    lineage.addEventListener("click", () => navigateToRun(sourceId));
    actions.appendChild(lineage);
  }
  const replay = run.run_id ? buildReplayRunButton(run, wrap) : null;
  if (replay) actions.appendChild(replay);
  // The claim rides beside the run in the detail payload; the cancel
  // confirmation reads it off the run it is handed.
  if (runIsCancellable(run)) actions.appendChild(buildCancelRunButton({ ...run, pull_claim: detail.pull_claim }, wrap));
  wrap.appendChild(actions);
  const failure = buildRunFailure(run, Array.isArray(detail.steps) ? detail.steps : []);
  if (failure) wrap.appendChild(failure);
  if (run.state === "held") wrap.appendChild(buildRunHold(run));
  wrap.appendChild(grid);
  const leaves = buildClaimedLeaves(run, Array.isArray(detail.claimed_leaves) ? detail.claimed_leaves : []);
  if (leaves) wrap.appendChild(leaves);
  const crews = buildCrewWindow(detail.crew_window || null);
  if (crews) wrap.appendChild(crews);
  const waiting = buildStillWaiting(run.drain_last_pass || null);
  if (waiting) wrap.appendChild(waiting);
  const children = buildChildDispatches(run);
  if (children) wrap.appendChild(children);
  syncNodes(meta, [wrap]);
}

// A pull drain's launched leaves that are still running, and whether a
// graceful cancel is waiting for them (`run.drain_cancel`). Mirrors the
// `Cancelling:` and `Claimed leaves:` lines of `orbit run show`.
function buildClaimedLeaves(run, leaves) {
  const cancel = run.drain_cancel || null;
  if (leaves.length === 0 && !cancel) return null;
  const panel = el("div", { class: "child-dispatch-panel" });
  if (cancel) {
    const by = cancel.actor ? ` (requested by ${cancel.actor}${cancel.reason ? `: ${cancel.reason}` : ""})` : "";
    const text = run.state === "running"
      ? `Cancelling: waiting for ${leaves.length} leaves to finish and settle${by}. Cancel again and choose to stop them to end it now.`
      : `Cancelled gracefully${by}.`;
    panel.appendChild(el("div", { class: "label", text }));
  }
  if (leaves.length > 0) {
    panel.appendChild(el("div", { class: "label", text: `claimed leaves (${leaves.length} running)` }));
    for (const leaf of leaves) {
      const parts = [`task ${leaf.task_id || "?"}`, `owner ${leaf.owner || "?"}`];
      if (leaf.leaf_state) parts.push(`state ${leaf.leaf_state}`);
      if (leaf.settlement_phase) parts.push(`claim ${leaf.settlement_phase}`);
      const link = el("button", {
        class: "back-action",
        text: leaf.leaf_run_id,
        title: `Open ${leaf.leaf_run_id}`,
      });
      link.addEventListener("click", () => navigateToRun(leaf.leaf_run_id));
      panel.appendChild(el("div", { class: "child-dispatch-row" }, [
        link,
        el("span", { class: "child-dispatch-meta", text: parts.join(" · ") }),
      ]));
    }
  }
  return panel;
}

const CREW_EXCLUSION_SOURCES = {
  provider_unavailable: "provider unavailable",
  leaf_released: "leaf released",
};

// A pull drain's crew window: the crews its provider preflight found
// runnable, and each crew it excluded for the window with the source and
// reason. Mirrors the `Crews:` lines of `orbit run show`.
function buildCrewWindow(window) {
  if (!window) return null;
  const excluded = Array.isArray(window.excluded) ? window.excluded : [];
  const runnable = Array.isArray(window.runnable) ? window.runnable : null;
  if (!runnable && excluded.length === 0) return null;
  const panel = el("div", { class: "child-dispatch-panel crew-window" });
  const summary = runnable
    ? `crews runnable: ${runnable.length > 0 ? runnable.join(", ") : "none"}`
    : "crews: no preflight recorded";
  panel.appendChild(el("div", { class: "label", text: summary }));
  for (const exclusion of excluded) {
    const source = CREW_EXCLUSION_SOURCES[exclusion.source] || "preflight";
    panel.appendChild(el("div", { class: "child-dispatch-row crew-exclusion" }, [
      el("span", { class: "child-dispatch-meta", text: `excluded ${exclusion.crew} (${source}): ${exclusion.reason}` }),
    ]));
  }
  return panel;
}

// Reason codes whose `detail` is the sentence that names what clears the wait.
const WAITING_DETAIL_REASONS = new Set([
  "host_os_mismatch", "local_route_before_pr", "crew_unavailable", "owner_hold", "invalid_candidate",
]);

const KEPT_OFF_CAUSES = {
  context_lock_conflict: "footprint holds",
  dependency_not_done: "unmet dependencies",
  host_os_mismatch: "for another OS",
  crew_unavailable: "needing a crew this host cannot run",
  owner_hold: "held on the owner",
  invalid_candidate: "invalid",
};

// Consecutive idle owner answers after which a pull drain says why it claims
// nothing. Mirrors the CLI's `idle:` line.
const IDLE_SUMMARY_PASSES = 3;

function waitingTaskText(task, fallback) {
  let text = `Task ${task.task_id}: ${task.reason || fallback}`;
  if (Array.isArray(task.blocked_by) && task.blocked_by.length > 0) text += ` blocked-by=${task.blocked_by.join(",")}`;
  if (WAITING_DETAIL_REASONS.has(task.reason) && task.detail) text += ` (${task.detail})`;
  return text;
}

// The backlog a drain's last admission pass left unstarted, for local and pull
// drains alike: each task with its reason and the tasks it waits on. A pull
// drain's list is the owner's last answer, so it is dated. Mirrors the
// `Still waiting:` lines of `orbit run show`.
function buildStillWaiting(pass) {
  if (!pass) return null;
  const deferred = Array.isArray(pass.deferred) ? pass.deferred : [];
  const excluded = Array.isArray(pass.excluded) ? pass.excluded : [];
  const queued = pass.queued || 0;
  const excludedTotal = pass.excluded_total || 0;
  if (queued === 0 && deferred.length === 0 && excludedTotal === 0) return null;
  const panel = el("div", { class: "child-dispatch-panel still-waiting" });
  const answered = pass.waiting_recorded_at ? ` (the owner answered ${fmtAbsTime(pass.waiting_recorded_at)})` : "";
  panel.appendChild(el("div", {
    class: "label",
    text: `still waiting: ${queued} admissible and ${excludedTotal} excluded backlog task(s) were never started at the last pass${answered}`,
  }));
  const rows = [
    ...deferred.map((task) => waitingTaskText(task, "lock conflict")),
    ...excluded.map((task) => waitingTaskText(task, "excluded")),
  ];
  if (excludedTotal > excluded.length) rows.push(`... and ${excludedTotal - excluded.length} more excluded`);
  for (const text of rows) {
    panel.appendChild(el("div", { class: "child-dispatch-row waiting-task" }, [
      el("span", { class: "child-dispatch-meta", text }),
    ]));
  }
  const byReason = pass.waiting_by_reason || {};
  const keptOff = Object.values(byReason).reduce((sum, count) => sum + count, 0);
  if ((pass.consecutive_idle_passes || 0) >= IDLE_SUMMARY_PASSES && keptOff > 0) {
    const causes = Object.entries(byReason)
      .sort(([a, x], [b, y]) => y - x || a.localeCompare(b))
      .map(([reason, count]) => `${count} ${KEPT_OFF_CAUSES[reason] || reason}`)
      .join(", ");
    panel.appendChild(el("div", {
      class: "child-dispatch-row waiting-idle",
      text: `idle: ${keptOff} backlog task(s) kept off this host for ${pass.consecutive_idle_passes} consecutive passes (${causes})`,
    }));
  }
  return panel;
}

const FAILED_RUN_STATES = new Set(["failed", "timeout", "interrupted"]);
const FAILED_STEP_STATES = new Set(["error", "failed", "timeout", "interrupted"]);

// A failed run leads with why: the step it stopped at and the error it
// recorded, above the metadata, so nobody has to open Errors or expand every
// step to find the one line that matters. The run-level message wins; a
// failed step's own message stands in when the run carries none.
function buildRunFailure(run, steps) {
  if (!FAILED_RUN_STATES.has(run.state)) return null;
  const step = steps.find((candidate) => FAILED_STEP_STATES.has(candidate.state)) || null;
  const pass = run.drain_last_pass || {};
  const code = run.error_code || (step && step.error_code) || pass.last_pass_error_code;
  const message = run.error_message || (step && step.error_message) || pass.last_pass_error || "";
  const where = step
    ? `at step ${Number(step.step_index) + 1} of ${steps.length} · ${step.target_id || step.target_type || "step"}`
    : "";
  const verb = run.state === "timeout" ? "Timed out" : run.state === "interrupted" ? "Interrupted" : "Failed";
  const head = el("div", { class: "run-failure-head" }, [
    el("strong", { text: verb }),
    where ? el("span", { class: "run-failure-where", text: ` ${where}` }) : null,
    code ? el("span", { class: "run-failure-code mono", text: code }) : null,
  ]);
  const box = el("section", { class: "run-failure" }, [head]);
  box.setAttribute("aria-label", "Why this run failed");
  if (message) box.appendChild(el("pre", { class: "run-failure-message mono", text: message }));
  if (code === "protocol_skew") box.appendChild(el("p", { text: "Deploy matching Orbit builds on the owner and follower, restart their long-lived processes, then start a new pull drain." }));
  return box;
}

function buildRunHold(run) {
  const box = el("section", { class: "run-hold" }, [
    el("strong", { text: "Delivery held" }),
    el("pre", { class: "run-hold-message mono", text: run.error_message || "Delivery is awaiting required external evidence." }),
    el("p", { text: "Record the required external evidence for this task. Its receipt queues a fresh review; delivery resumes when that review clears the hold." }),
  ]);
  box.setAttribute("aria-label", "Why this run is held");
  return box;
}

/// Normalize the run's stored `executed_on` into the shape the shared
/// provenance renderer reads. An absent field is explicitly unknown.
function runExecutionLocation(run) {
  const location = run && run.executed_on;
  if (!location || !location.machine_id) return { known: false };
  return { known: true, machine_id: location.machine_id, machine_name: location.machine_name || null };
}

// [ORB-10971] The child Runs this run dispatched, from the durable dispatch
// checkpoint the API projects as `run.child_dispatches`.
//
// This is the answer to "the parent has been sitting on a dispatch step for an
// hour — did it actually submit anything?", so it is rendered from the moment
// the child is submitted rather than only once the parent's wait returns, and
// it stays visible after the parent terminalizes.
function buildChildDispatches(run) {
  const dispatches = Array.isArray(run.child_dispatches) ? run.child_dispatches : [];
  if (dispatches.length === 0) return null;

  const rows = dispatches.map((d) => {
    const parts = [`job ${d.job_name || "?"}`, `phase ${d.phase || "?"}`];
    if (d.parent_step_id) parts.push(`step ${d.parent_step_id}`);
    if (d.queued) parts.push("queued");
    if (d.child_status) parts.push(`status ${d.child_status}`);
    if (d.cancellation) parts.push(`cancel ${d.cancellation.policy}/${d.cancellation.outcome}`);

    const link = el("button", {
      class: "back-action",
      text: d.child_run_id,
      title: `Open ${d.child_run_id}`,
    });
    link.addEventListener("click", () => navigateToRun(d.child_run_id));

    const row = el("div", { class: "child-dispatch-row" }, [
      link,
      el("span", { class: "child-dispatch-meta", text: parts.join(" · ") }),
    ]);
    if (d.error) {
      row.appendChild(el("span", { class: "child-dispatch-error", text: d.error }));
    }
    return row;
  });

  return el("div", { class: "child-dispatch-panel" }, [
    el("div", { class: "label", text: `child runs (${dispatches.length})` }),
    ...rows,
  ]);
}

export function renderRunSteps() {
  const body = $("run-steps-body");
  if (!body) return;
  const notices = activeRunLogsError ? [el("div", {
    class: "action-error",
    role: "alert",
    text: `Unable to load agent logs: ${activeRunLogsError}. Use Refresh to retry.`,
  })] : [];
  const steps = (activeRunDetail && activeRunDetail.steps) || [];
  if (steps.length === 0) {
    syncNodes(body, [...notices, el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: "No steps recorded for this run." }),
    ])]);
    return;
  }
  const header = el("div", { class: "step-header" }, [
    el("span", { class: "idx", text: "Step" }),
    el("span", { class: "target", text: "Target" }),
    el("span", { text: "State" }),
    el("span", { class: "duration", text: "Duration" }),
    el("span", { class: "exit", text: "Exit code" }),
  ]);
  const frag = document.createDocumentFragment();
  for (const step of steps) {
    const exit = step.exit_code;
    const exitClass = exit != null && exit !== 0 ? "exit fail" : "exit";
    const row = el("div", { class: "step-row" }, [
      el("span", { class: "idx", text: `#${Number(step.step_index) + 1}` }),
      el("span", { class: "target", text: `${step.target_type}:${step.target_id}` }),
      el("span", {}, [stateCell(step.state)]),
      el("span", { class: "duration", text: fmtDuration(step.duration_ms) }),
      el("span", { class: exitClass, text: exit == null ? "-" : String(exit), title: exit == null ? "No exit code recorded" : `Exit code ${exit}` }),
    ]);
    row.dataset.key = `step-${step.step_index}`;
    // Expansion is part of the row's identity: without it the keyed diff reuses
    // the collapsed node and drops the `expanded` class and `aria-expanded`
    // the toggle just set.
    row.dataset.hash = `${step.step_index}-${step.state}-${exit}-${expandedStepIndices.has(step.step_index)}`;
    if (expandedStepIndices.has(step.step_index)) row.classList.add("expanded");
    makeToggleRow(row, {
      expanded: expandedStepIndices.has(step.step_index),
      onToggle: () => {
        if (expandedStepIndices.has(step.step_index)) expandedStepIndices.delete(step.step_index);
        else expandedStepIndices.add(step.step_index);
        renderRunSteps();
      },
    });
    frag.appendChild(row);
    if (expandedStepIndices.has(step.step_index)) {
      frag.appendChild(buildStepDetail(step));
    }
  }
  syncNodes(body, [...notices, header, ...Array.from(frag.children)]);
}

export function renderRunKnowledge() {
  const panel = $("run-knowledge-panel");
  if (!panel) return;
  const km = activeRunDetail && activeRunDetail.run && activeRunDetail.run.knowledge_metrics;
  panel.innerHTML = "";
  panel.style.display = km == null || Object.keys(km).length === 0 ? "none" : "block";
  if (panel.style.display === "none") return;
  const header = el("div", { class: "knowledge-header", text: "Knowledge Pack" });
  panel.appendChild(header);
  const grid = el("div", { class: "knowledge-grid" });
  const baseline = Number(km.raw_read_token_baseline || 0);
  const packTokens = km.knowledge_pack_tokens == null ? null : Number(km.knowledge_pack_tokens);
  const totalLlm = km.total_llm_input_tokens == null ? null : Number(km.total_llm_input_tokens);
  const ratioText = baseline === 0 || packTokens == null
    ? "n/a"
    : `${((packTokens / baseline) * 100).toFixed(1)}%`;
  const addCell = (label, value, extra = "") => {
    const cell = el("div");
    cell.appendChild(el("div", { class: "label", text: label }));
    cell.appendChild(el("div", { class: `value${extra ? " " + extra : ""}`, text: value }));
    grid.appendChild(cell);
  };
  addCell("raw_read_token_baseline", String(baseline));
  addCell("knowledge_pack_tokens", packTokens == null ? "-" : String(packTokens));
  addCell("total_llm_input_tokens", totalLlm == null ? "-" : String(totalLlm));
  addCell("compression_ratio", ratioText, "ratio");
  panel.appendChild(grid);
}

export function renderRunGantt() {
  const panel = $("run-gantt-panel");
  if (!panel) return;
  const detail = activeRunDetail || {};
  const run = detail.run || {};
  const steps = Array.isArray(detail.steps) ? detail.steps : [];
  if (steps.length === 0) {
    panel.style.display = "none";
    panel.innerHTML = "";
    return;
  }
  panel.style.display = "block";
  panel.innerHTML = "";
  panel.appendChild(el("div", { class: "gantt-header", text: "Step Timeline" }));

  const startMs = run.started_at ? new Date(run.started_at).getTime() : null;
  let endMs = run.finished_at ? new Date(run.finished_at).getTime() : null;
  // Walk steps for tighter bounds when run-level timestamps are missing.
  let derivedStart = startMs;
  let derivedEnd = endMs;
  for (const s of steps) {
    if (s.started_at) {
      const t = new Date(s.started_at).getTime();
      if (derivedStart == null || t < derivedStart) derivedStart = t;
    }
    if (s.finished_at) {
      const t = new Date(s.finished_at).getTime();
      if (derivedEnd == null || t > derivedEnd) derivedEnd = t;
    }
  }
  // A live run's timeline runs to now even after a step has finished, so the
  // active step's bar keeps growing. Terminal runs keep their recorded bounds
  // and fall back to now only when no finish timestamp exists at all.
  const now = Date.now();
  if (derivedEnd == null || (LIVE_RUN_STATES.has(run.state) && derivedEnd < now)) derivedEnd = now;
  if (derivedStart == null) derivedStart = derivedEnd - 1000;
  if (derivedEnd <= derivedStart) derivedEnd = derivedStart + 1000;

  const PAD_LEFT = 230;  // step-name gutter
  const PAD_RIGHT = 12;
  const PAD_TOP = 18;
  const ROW_H = 22;
  const BAR_H = 14;
  const W_TOTAL = 1000;  // virtual viewBox width; SVG rescales to container
  const innerW = W_TOTAL - PAD_LEFT - PAD_RIGHT;
  const totalH = PAD_TOP + steps.length * ROW_H + 18;
  const span = derivedEnd - derivedStart;
  const xOf = (ms) => PAD_LEFT + ((ms - derivedStart) / span) * innerW;

  const svgNS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(svgNS, "svg");
  svg.setAttribute("class", "gantt-svg");
  svg.setAttribute("viewBox", `0 0 ${W_TOTAL} ${totalH}`);
  svg.setAttribute("preserveAspectRatio", "none");
  svg.style.height = `${totalH}px`;

  // Lane backgrounds + step-index labels.
  steps.forEach((step, i) => {
    const y = PAD_TOP + i * ROW_H;
    const bg = document.createElementNS(svgNS, "rect");
    bg.setAttribute("class", `gantt-lane-bg${i % 2 === 0 ? "" : " alt"}`);
    bg.setAttribute("x", String(0));
    bg.setAttribute("y", String(y));
    bg.setAttribute("width", String(W_TOTAL));
    bg.setAttribute("height", String(ROW_H));
    svg.appendChild(bg);
    const label = document.createElementNS(svgNS, "text");
    label.setAttribute("class", "gantt-lane-label");
    label.setAttribute("x", String(8));
    label.setAttribute("y", String(y + ROW_H / 2 + 3));
    const name = String(step.target_id || `#${Number(step.step_index) + 1}`);
    label.textContent = name.length > 34 ? `${name.slice(0, 33)}…` : name;
    svg.appendChild(label);
  });

  // Axis: start and end labels.
  const axisY = PAD_TOP + steps.length * ROW_H + 6;
  const axisLine = document.createElementNS(svgNS, "line");
  axisLine.setAttribute("class", "gantt-axis-line");
  axisLine.setAttribute("x1", String(PAD_LEFT));
  axisLine.setAttribute("y1", String(axisY));
  axisLine.setAttribute("x2", String(W_TOTAL - PAD_RIGHT));
  axisLine.setAttribute("y2", String(axisY));
  svg.appendChild(axisLine);
  const fmtAxis = (ms) => {
    const d = new Date(ms);
    const pad = (n) => String(n).padStart(2, "0");
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const axisStart = document.createElementNS(svgNS, "text");
  axisStart.setAttribute("class", "gantt-axis-label");
  axisStart.setAttribute("x", String(PAD_LEFT));
  axisStart.setAttribute("y", String(axisY + 12));
  axisStart.textContent = fmtAxis(derivedStart);
  svg.appendChild(axisStart);
  const axisEnd = document.createElementNS(svgNS, "text");
  axisEnd.setAttribute("class", "gantt-axis-label");
  axisEnd.setAttribute("x", String(W_TOTAL - PAD_RIGHT));
  axisEnd.setAttribute("y", String(axisY + 12));
  axisEnd.setAttribute("text-anchor", "end");
  axisEnd.textContent = fmtAxis(derivedEnd);
  svg.appendChild(axisEnd);

  // Bars per step.
  steps.forEach((step, i) => {
    const sStart = step.started_at ? new Date(step.started_at).getTime() : derivedStart;
    const sEnd = step.finished_at ? new Date(step.finished_at).getTime() : derivedEnd;
    const x1 = xOf(sStart);
    const x2 = xOf(sEnd);
    const w = Math.max(2, x2 - x1);
    const y = PAD_TOP + i * ROW_H + (ROW_H - BAR_H) / 2;
    const bar = document.createElementNS(svgNS, "rect");
    bar.setAttribute("class", "gantt-bar");
    bar.setAttribute("x", String(x1));
    bar.setAttribute("y", String(y));
    bar.setAttribute("width", String(w));
    bar.setAttribute("height", String(BAR_H));
    bar.setAttribute("data-state", step.state);
    bar.setAttribute("fill", "var(--dot)");
    bar.addEventListener("mousemove", (e) => showGanttTooltip(e, step));
    bar.addEventListener("mouseleave", hideGanttTooltip);
    bar.addEventListener("click", () => {
      // Reuse the per-step expand/collapse panel from the Steps sub-tab.
      if (expandedStepIndices.has(step.step_index)) {
        expandedStepIndices.delete(step.step_index);
      } else {
        expandedStepIndices.add(step.step_index);
      }
      setRunDetailSubtab("steps");
      renderRunSteps();
      const target = document.querySelector(`[data-key="step-${step.step_index}"]`);
      if (target && target.scrollIntoView) {
        target.scrollIntoView({ block: "nearest", behavior: "smooth" });
      }
    });
    svg.appendChild(bar);
  });

  // Retry markers from StepRetry events.
  const stepIdToIndex = new Map();
  steps.forEach((s) => {
    // step_id is the activity name (or step.id); the step file stores it as
    // target_id. Map both forms to the lane index.
    if (s.target_id != null) stepIdToIndex.set(String(s.target_id), s.step_index);
  });
  for (const ev of activeRunEvents || []) {
    if (ev.body_kind !== "step_retry") continue;
    const stepId = ev.step_id;
    const index = stepIdToIndex.get(stepId);
    if (index == null) continue;
    const tsMs = ev.ts ? new Date(ev.ts).getTime() : null;
    if (!tsMs || isNaN(tsMs)) continue;
    const cx = xOf(Math.max(derivedStart, Math.min(derivedEnd, tsMs)));
    const cy = PAD_TOP + index * ROW_H + ROW_H / 2;
    const marker = document.createElementNS(svgNS, "circle");
    marker.setAttribute("class", "gantt-retry-marker");
    marker.setAttribute("cx", String(cx));
    marker.setAttribute("cy", String(cy));
    marker.setAttribute("r", "3.5");
    const title = document.createElementNS(svgNS, "title");
    title.textContent = `retry attempt=${ev.attempt} backoff=${ev.next_backoff_ms}ms`;
    marker.appendChild(title);
    svg.appendChild(marker);
  }

  panel.appendChild(svg);
  const legend = el("div", { class: "gantt-legend" });
  legend.setAttribute("aria-label", "Timeline legend");
  for (const state of [...new Set(steps.map(step => step.state))]) {
    legend.appendChild(stateCell(state));
  }
  if ((activeRunEvents || []).some(event => event.body_kind === "step_retry")) {
    legend.appendChild(el("span", { class: "gantt-retry-key", text: "● Retry" }));
  }
  panel.appendChild(legend);
}

function showGanttTooltip(e, step) {
  const tip = $("gantt-tooltip");
  if (!tip) return;
  const lines = [
    `step: ${Number(step.step_index) + 1}`,
    `state: ${step.state}`,
    `exit_code: ${step.exit_code == null ? "-" : step.exit_code}`,
    `duration_ms: ${step.duration_ms == null ? "-" : step.duration_ms}`,
  ];
  tip.textContent = lines.join("\n");
  tip.style.display = "block";
  tip.setAttribute("aria-hidden", "false");
  // Position relative to viewport; clamp to keep the tooltip on-screen.
  const x = Math.min(window.innerWidth - 220, e.clientX + 12);
  const y = Math.min(window.innerHeight - 80, e.clientY + 12);
  tip.style.left = `${x}px`;
  tip.style.top = `${y}px`;
}

function hideGanttTooltip() {
  const tip = $("gantt-tooltip");
  if (!tip) return;
  tip.style.display = "none";
  tip.setAttribute("aria-hidden", "true");
}

function logsForStep(step) {
  const stepId = step.target_id == null ? null : String(step.target_id);
  return (activeRunLogs || []).filter((record) => {
    if (record.step_index != null && Number(record.step_index) === Number(step.step_index)) return true;
    return stepId != null && record.step_id === stepId;
  });
}

function buildLogBlock(record, stream) {
  const isErr = stream === "stderr";
  const preview = record[`${stream}_preview`] || "";
  if (!preview) return null;
  const block = el("div", { class: `step-log-block ${isErr ? "stderr" : "stdout"}` });
  const meta = [
    record.provider || "cli",
    record.exit_code == null ? "exit -" : `exit ${record.exit_code}`,
    record.timed_out ? "timeout" : null,
    record[`${stream}_truncated`] ? "truncated" : null,
  ].filter(Boolean).join(" · ");
  const toggle = el("button", { class: "back-action log-wrap-toggle", text: "Wrap lines", title: "Toggle line wrapping" });
  toggle.setAttribute("aria-pressed", "true");
  block.appendChild(el("div", { class: "step-log-head" }, [
    el("span", { class: "label", text: stream }),
    el("span", { class: "meta", text: meta }),
    toggle,
  ]));
  const pre = el("pre", { class: "wrap" });
  toggle.addEventListener("click", () => {
    const wrapped = pre.classList.toggle("wrap");
    toggle.setAttribute("aria-pressed", String(wrapped));
  });
  const formatted = isErr ? preview : preview.split("\n").map(line => {
    // Logs may mix JSON frames, plain text and a truncated final frame.
    // Preserve unrecognized lines and render every value as text, never HTML.
    try {
      const value = JSON.parse(line);
      return value && typeof value === "object" ? JSON.stringify(value, null, 2) : line;
    } catch (_) {
      return line;
    }
  }).join("\n");
  for (const line of formatted.split("\n")) {
    const row = el("span", {
      class: isErr && /\bERROR\s+[^:]+:/.test(line) ? "step-log-line error-line" : "step-log-line",
      text: line || " ",
    });
    pre.appendChild(row);
  }
  block.appendChild(pre);
  return block;
}

function knowledgeMetricsForStep(step) {
  if (!step || step.step_index !== 0) return null;
  const metrics = activeRunDetail && activeRunDetail.run && activeRunDetail.run.knowledge_metrics;
  return metrics || null;
}

// Log blocks and step 0's knowledge metrics are not fields of `step`.
// Keyed reconciliation retains the previous detail node while this hash
// matches, so a late /logs payload never paints unless those inputs are in it.
function stepDetailHash(step, logs) {
  return JSON.stringify({
    step,
    logs,
    logsError: activeRunLogsError,
    knowledge: knowledgeMetricsForStep(step),
  });
}

function buildStepDetail(step) {
  const logs = logsForStep(step);
  const wrap = el("div", { class: "step-detail" });
  wrap.dataset.key = `step-detail-${step.step_index}`;
  wrap.dataset.hash = stepDetailHash(step, logs);
  wrap.addEventListener("click", (e) => e.stopPropagation());

  const addBlock = (label, raw) => {
    const v = raw == null ? "" : (typeof raw === "string" ? raw : JSON.stringify(raw, null, 2));
    if (!v) return;
    const block = el("div", { class: "audit-detail-block" });
    block.appendChild(el("div", { class: "label", text: label }));
    block.appendChild(el("pre", { text: v }));
    wrap.appendChild(block);
  };

  if (step.error_message) addBlock("error", `${step.error_code || ""} ${step.error_message}`);
  addBlock("agent_response", step.agent_response_json);
  const blocks = logs.flatMap(record => [buildLogBlock(record, "stdout"), buildLogBlock(record, "stderr")]).filter(Boolean);
  if (blocks.length > 0) {
    const section = el("div", { class: "step-log-section" });
    section.appendChild(el("div", { class: "label", text: "agent logs" }));
    for (const block of blocks) section.appendChild(block);
    wrap.appendChild(section);
  } else {
    wrap.appendChild(el("div", { class: "step-logs-empty", text: activeRunLogsError ? "Logs unavailable. Use Refresh to retry." : "No logs recorded for this step" }));
  }
  const km = knowledgeMetricsForStep(step);
  if (km) addBlock("knowledge_metrics (run)", km);
  return wrap;
}

const RUN_EVENT_COLUMNS = [
  { key: "ts", label: "time" },
  { key: "body_kind", label: "kind" },
  { key: "event_type", label: "scope" },
  { key: "agent_identity", label: "agent" },
  { key: "summary", label: "detail" },
];

export function renderRunEvents() {
  const body = $("run-events-body");
  if (!body) return;
  if (activeRunEventsError) {
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "!" }),
      el("div", { class: "text", text: activeRunEventsError }),
      el("div", { class: "text", text: "Use Refresh to retry loading events." }),
    ])]);
    return;
  }
  const events = activeRunEvents || [];
  if (events.length === 0) {
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: "No v2 envelope events for this run." }),
    ])]);
    return;
  }
  let table = body.querySelector("table.scoreboard-table");
  let tbody;
  if (!table) {
    table = el("table", { class: "scoreboard-table" });
    const thead = el("thead");
    const headRow = el("tr");
    for (const col of RUN_EVENT_COLUMNS) {
      headRow.appendChild(el("th", { text: col.label }));
    }
    thead.appendChild(headRow);
    table.appendChild(thead);
    tbody = el("tbody");
    table.appendChild(tbody);
    syncNodes(body, [table]);
  } else {
    tbody = table.querySelector("tbody");
  }
  const frag = document.createDocumentFragment();
  for (let i = 0; i < events.length; i++) {
    const ev = events[i];
    const summary = summarizeEvent(ev);
    const tr = el("tr");
    tr.appendChild(el("td", { text: fmtTimestamp(ev.ts) }));
    tr.appendChild(el("td", { text: ev.body_kind || "-" }));
    tr.appendChild(el("td", { text: ev.event_type || "-" }));
    tr.appendChild(el("td", { text: ev.agent_identity || "-" }));
    const td = el("td", { class: "stderr" });
    td.title = summary.title;
    td.textContent = summary.text;
    tr.appendChild(td);
    tr.dataset.key = `runev-${ev.event_id || i}`;
    tr.dataset.hash = `${ev.event_id || i}-${ev.body_kind}`;
    frag.appendChild(tr);
  }
  syncNodes(tbody, Array.from(frag.children));
}

function summarizeEvent(ev) {
  const ignoreKeys = new Set([
    "schemaVersion", "event_type", "event_id", "ts", "run_id",
    "agent_identity", "parent_event_id", "workspace_path", "body_kind",
  ]);
  const parts = [];
  for (const [k, v] of Object.entries(ev)) {
    if (ignoreKeys.has(k)) continue;
    if (v == null) continue;
    if (typeof v === "object") {
      parts.push(`${k}=${JSON.stringify(v)}`);
    } else {
      parts.push(`${k}=${v}`);
    }
  }
  const text = parts.join(" ");
  return { text: truncate(text, 200), title: text };
}
