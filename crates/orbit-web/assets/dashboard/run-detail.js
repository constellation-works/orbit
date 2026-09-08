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

import { el, syncNodes, stateCell, positiveIntParam, makeToggleRow } from './common.js';

const $ = (id) => document.getElementById(id);

const RUN_EVENTS_LIMIT = positiveIntParam("events", 100);  // re-export for app orchestrators

// Run detail module-scoped state (was in app.js)
let activeRunId = null;
let activeRunDetail = null;
let activeRunEvents = [];
let activeRunEventsError = null;
let activeRunLogs = [];
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
  return hasCtx("buildReplayRunButton") ? _runDetailCtx.buildReplayRunButton(run, host) : null;
}

// --- public state accessors (re-exported by app.js routerContext + runDetailContext) ---

export function getActiveRunId() { return activeRunId; }
export function setActiveRunId(v) { activeRunId = v; }

export function getActiveRunDetail() { return activeRunDetail; }
export function setActiveRunDetail(v) { activeRunDetail = v; }

export function getActiveRunEvents() { return activeRunEvents; }
export function setActiveRunEvents(v) {
  activeRunEvents = v || [];
  activeRunEventsError = null;
}

export function setActiveRunEventsError(v) { activeRunEventsError = v || null; }

export function getActiveRunLogs() { return activeRunLogs; }
export function setActiveRunLogs(v) { activeRunLogs = v || []; }

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
  $("run-detail-title").textContent = "Run Detail";
  $("run-detail-count").textContent = "-";
  $("run-steps-body").innerHTML = "";
  $("run-events-body").innerHTML = "";
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
  $("run-detail-count").textContent = `${stepCount} steps`;

  const grid = el("div", { class: "run-meta-grid" });
  const addCell = (label, value) => {
    const cell = el("div");
    cell.appendChild(el("div", { class: "label", text: label }));
    cell.appendChild(el("div", { class: "value", text: value == null ? "-" : String(value) }));
    grid.appendChild(cell);
  };
  addCell("job", run.job_id);
  addCell("state", run.state);
  addCell("attempt", run.attempt);
  addCell("started", run.started_at ? fmtAbsTime(run.started_at) : "-");
  addCell("finished", run.finished_at ? fmtAbsTime(run.finished_at) : "-");
  addCell("duration", run.duration_ms != null ? fmtDuration(run.duration_ms) : "-");

  const wrap = el("div");
  const back = el("button", { class: "back-action", text: "← back to runs" });
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
  if (run.run_id) actions.appendChild(buildReplayRunButton(run, wrap));
  if (runIsCancellable(run)) actions.appendChild(buildCancelRunButton(run, wrap));
  wrap.appendChild(actions);
  wrap.appendChild(grid);
  const children = buildChildDispatches(run);
  if (children) wrap.appendChild(children);
  syncNodes(meta, [wrap]);
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
  const steps = (activeRunDetail && activeRunDetail.steps) || [];
  if (steps.length === 0) {
    syncNodes(body, [el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: "No steps recorded for this run." }),
    ])]);
    return;
  }
  const frag = document.createDocumentFragment();
  for (const step of steps) {
    const exit = step.exit_code;
    const exitClass = exit != null && exit !== 0 ? "exit fail" : "exit";
    const row = el("div", { class: "step-row" }, [
      el("span", { class: "idx", text: `#${step.step_index}` }),
      el("span", { class: "target", text: `${step.target_type}:${step.target_id}` }),
      el("span", {}, [stateCell(step.state)]),
      el("span", { class: "duration", text: fmtDuration(step.duration_ms) }),
      el("span", { class: exitClass, text: exit == null ? "-" : String(exit) }),
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
  syncNodes(body, Array.from(frag.children));
}

export function renderRunKnowledge() {
  const panel = $("run-knowledge-panel");
  if (!panel) return;
  panel.style.display = "block";
  const km = activeRunDetail && activeRunDetail.run && activeRunDetail.run.knowledge_metrics;
  panel.innerHTML = "";
  const header = el("div", { class: "knowledge-header", text: "Knowledge Pack" });
  panel.appendChild(header);
  if (km == null) {
    panel.appendChild(el("div", { class: "knowledge-empty", text: "no knowledge metrics for this run" }));
    return;
  }
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
  // Run still in flight or missing finish: extend to now.
  if (derivedEnd == null) derivedEnd = Date.now();
  if (derivedStart == null) derivedStart = derivedEnd - 1000;
  if (derivedEnd <= derivedStart) derivedEnd = derivedStart + 1000;

  const PAD_LEFT = 56;   // step-index gutter
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
    label.textContent = `#${step.step_index}`;
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
    bar.setAttribute("fill", `var(--state-${step.state}, var(--fg-dim))`);
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
}

function showGanttTooltip(e, step) {
  const tip = $("gantt-tooltip");
  if (!tip) return;
  const lines = [
    `step_index: ${step.step_index}`,
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
  block.appendChild(el("div", { class: "step-log-head" }, [
    el("span", { class: "label", text: stream }),
    el("span", { class: "meta", text: meta }),
  ]));
  const pre = el("pre");
  for (const line of preview.split("\n")) {
    const row = el("span", {
      class: isErr && /\bERROR\s+[^:]+:/.test(line) ? "log-line error-line" : "log-line",
      text: line || " ",
    });
    pre.appendChild(row);
  }
  block.appendChild(pre);
  return block;
}

function buildStepDetail(step) {
  const wrap = el("div", { class: "step-detail" });
  wrap.dataset.key = `step-detail-${step.step_index}`;
  wrap.dataset.hash = JSON.stringify(step);
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
  const logs = logsForStep(step);
  if (logs.length > 0) {
    const section = el("div", { class: "step-log-section" });
    section.appendChild(el("div", { class: "label", text: "agent logs" }));
    for (const record of logs) {
      const stdout = buildLogBlock(record, "stdout");
      const stderr = buildLogBlock(record, "stderr");
      if (stdout) section.appendChild(stdout);
      if (stderr) section.appendChild(stderr);
    }
    wrap.appendChild(section);
  }
  const km = activeRunDetail && activeRunDetail.run && activeRunDetail.run.knowledge_metrics;
  if (km && step.step_index === 0) addBlock("knowledge_metrics (run)", km);
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
      el("div", { class: "text", text: "Try narrowing the kind filter to reduce the scan." }),
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
