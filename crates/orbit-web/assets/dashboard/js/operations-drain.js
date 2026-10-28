import { captureWorkspaceVisit, requestPanel, el, fetchJson, formatDateTime, getWorkspaceRevision, postJson } from './common.js';
import { drainCrewNote, drainPilotNote, setDrainReadiness, setDrainReadinessFailure } from './drain-waits.js';
import { AUTO_DRAIN_APPROVE_RULE } from './drain-start.js';
import { autoDrainForecast, refreshBacklogForecast } from './operations-forecast.js';
import { configureOperationsFleetCallbacks, loadFleetSnapshot, paintAggregateFleet, renderFleetIndicator } from './operations-fleet.js';
import { $, feedback, operationsState, pendingOperations, registerOperationsReset, runLink, selectedWorkspace, workspaceReadOnlyReason } from './operations-shared.js';
import { taskList, taskState } from './task-shared.js';
import { configureOperationsDrainControls, autoDrainConcurrencyValid, autoDrainConcurrencyValue, autoDrainStopButton, autoDrainStopNote, refreshDrainAfterAction, renderAutoDrainStart } from './operations-drain-controls.js';
import { autoDrainBlocked, autoDrainCapacity, autoDrainCounts, autoDrainPhase, autoDrainPlural, autoDrainProviderAuth, autoDrainProviderLimitNote, autoDrainReasons, autoDrainShortRunId, autoDrainThrottleLine, autoDrainTimeLeft, updateDrainIndicators } from './operations-drain-capacity.js';

let announcedDrainState = null;
// Set while Start or Stop is refreshing the card after reporting its result, so
// the state change that result caused is not announced over the result.
let holdDrainAnnouncement = false;
function renderAutoDrainHead(payload) {
  const { capacity, phase, running, label, pulling, pullLabel } = autoDrainPhase(payload);
  updateDrainIndicators(pulling ? "pulling" : phase, pulling ? pullLabel : label);
  const card = $("auto-drain-panel");
  if (card) {
    card.dataset.drainState = pulling ? "pulling" : phase;
    const title = card.querySelector('.drain-title');
    if (title) title.textContent = 'Auto-drain';
  }
  const pill = $("auto-drain-phase");
  if (pill) pill.dataset.drainState = pulling ? "pulling" : phase;
  const dot = $("auto-drain-dot");
  if (dot) dot.className = `drain-dot ${pulling ? "pulling" : phase}`;
  const text = $("auto-drain-phase-label");
  if (text) {
    text.textContent = pulling ? pullLabel
      : phase === "idle" ? (workspaceReadOnlyReason() ? "Read-only" : "Idle") : label;
  }
  // The window's deadline sits beside its phase; the pill already says when
  // admissions have stopped.
  const when = $("auto-drain-when");
  if (when) when.textContent = !pulling && phase === "draining" ? autoDrainTimeLeft(capacity.ends_at) : "";
  const stateKey = `${phase}:${phase === "winding_down" ? running : ""}`;
  if (announcedDrainState !== null && announcedDrainState !== stateKey && !holdDrainAnnouncement) {
    feedback("auto-drain-operation-feedback", "", `Auto-drain ${label}${phase === "winding_down" ? ` · this window: ${running} still running` : ""}.`);
  }
  announcedDrainState = stateKey;
}

// The live window as one row of figures: what still runs, what it has
// admitted and, when it was started with approve-proposed, what it approved
// and held. Its run and deadline live in the footer and header. Empty when no
// auto window is live.
function autoDrainWindow(payload) {
  const { capacity, phase, running, pulling } = autoDrainPhase(payload);
  if (pulling || phase === "idle") return null;
  const stats = el("dl", { class: "drain-window-stats" });
  stats.id = "auto-drain-live";
  stats.setAttribute("aria-label", "This window");
  const stat = (key, label, value, detail = "", title = "") => {
    const figure = el("div", { class: "drain-window-stat", title }, [
      el("dt", { text: label }),
      el("dd", { text: String(value) }),
      detail ? el("span", { class: "drain-window-stat-detail", text: detail }) : null,
    ]);
    figure.dataset.stat = key;
    stats.appendChild(figure);
  };
  if (capacity.admitted_workers != null) {
    stat("running", "running", running);
    stat("admitted", "admitted", capacity.admitted_workers);
  }
  const approvals = autoDrainApprovals(payload.approvals);
  if (approvals) stat("approved", "approved", approvals.approved, approvals.held, approvals.title);
  if (stats.children.length === 0) return null;
  if (!approvals || !approvals.reasons.length) return stats;
  const summary = el("div", { class: "drain-approval-summary" });
  approvals.reasons.forEach(([reason, count], index) => {
    if (index) summary.append(" · ");
    const label = reason === "warnings" ? "held for pilot warnings" : (reason || "unclassified").replaceAll("_", " ");
    const item = el(reason === "warnings" ? "a" : "span", { text: `${count} ${label}` });
    if (reason === "warnings") {
      item.href = "#tasks?status=proposed&pilot=warnings";
      item.title = "Show proposed tasks with pilot warnings";
    }
    summary.append(item);
  });
  return el("div", { class: "drain-window" }, [stats, summary]);
}

// The card's last row: the coordinator run (or the replica's pull drain), the
// window's auto-concurrency, and Stop. Its help line follows.
function autoDrainFoot(payload) {
  const { live, capacity, phase, pulling } = autoDrainPhase(payload);
  const runId = pulling ? live.pullRunId : phase === "idle" ? "" : capacity.drain_status_run_id || live.runId;
  const meta = el("div", { class: "drain-foot-meta" });
  meta.id = "auto-drain-run";
  if (runId) meta.appendChild(runLink(runId, selectedWorkspace()?.id, autoDrainShortRunId(runId)));
  const mode = !pulling && phase !== "idle" ? payload.auto_concurrency?.mode : "";
  if (mode) {
    meta.appendChild(el("span", {
      class: "drain-foot-concurrency",
      text: `auto-concurrency ${mode}`,
      title: `Auto-concurrency: ${mode} (${payload.auto_concurrency.source}).`,
    }));
  }
  return el("div", { class: "drain-foot" }, [
    el("div", { class: "drain-foot-row" }, [meta, autoDrainStopButton(payload)]),
    autoDrainStopNote(payload),
  ]);
}

// A queued window's length: `for_seconds` 0 is a single pass.
function autoDrainWindowLength(seconds) {
  const total = Number(seconds);
  if (!Number.isFinite(total) || total <= 0) return "single pass";
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  if (!hours) return minutes ? `${minutes}m window` : `${total}s window`;
  return `${hours}h${minutes ? ` ${String(minutes).padStart(2, "0")}m` : ""} window`;
}

// Windows started while another runs wait for its slot (one coordinator per
// workspace). Each is listed under the live window with what it will admit,
// when it starts and, when its start lacked every Claude credential, the
// Claude crews in its window that will run without one. Null when none wait,
// so the card is unchanged.
function autoDrainQueued(payload) {
  const capacity = payload.capacity || {};
  const queued = (Array.isArray(capacity.queued_drains) ? capacity.queued_drains : []).filter(drain => drain?.run_id);
  if (!queued.length) return null;
  const section = el("section", { class: "drain-queued" });
  section.id = "auto-drain-queued";
  section.setAttribute("aria-label", "Queued windows");
  section.append(el("h4", { class: "drain-queued-title", text: `Queued · ${autoDrainPlural(queued.length, "window")}` }));
  const list = el("ul", { class: "drain-queued-list" });
  for (const drain of queued) list.append(autoDrainQueuedRow(payload, drain));
  section.append(list);
  return section;
}

function autoDrainQueuedRow(payload, drain) {
  const capacity = payload.capacity || {};
  const runId = String(drain.run_id);
  const row = el("li", { class: "drain-queued-row" });
  row.dataset.runId = runId;
  const head = el("div", { class: "drain-queued-head" }, [
    runLink(runId, selectedWorkspace()?.id, autoDrainShortRunId(runId)),
    el("span", { class: "drain-queued-state", text: capacity.drain_run_id ? "Waiting for the live window" : "Waiting for its slot" }),
    autoDrainQueuedCancel(payload, runId),
  ]);
  const crews = Array.isArray(drain.allowed_crews) && drain.allowed_crews.length ? drain.allowed_crews.join(", ") : "all crews";
  const facts = el("div", { class: "drain-queued-facts" }, [
    ["ceiling", `${drain.max_active_leaf_runs ?? "default"} parallel`],
    ["completion", drain.completion === "done" ? "completes to done" : "completes to review"],
    ["crews", crews],
    ["window", autoDrainWindowLength(drain.for_seconds)],
  ].map(([key, text]) => {
    const fact = el("span", { class: "drain-queued-fact", text });
    fact.dataset.field = key;
    return fact;
  }));
  row.append(head, facts);
  if (capacity.drain_run_id) {
    const left = autoDrainTimeLeft(capacity.ends_at);
    const ends = Date.parse(capacity.ends_at || "");
    row.append(el("div", {
      class: "drain-queued-start",
      text: `Starts when ${autoDrainShortRunId(String(capacity.drain_run_id))} ends${left && left !== "window closed" ? ` · ${left}` : ""}`,
      title: Number.isFinite(ends) ? `The live window closes ${formatDateTime(capacity.ends_at)}` : "",
    }));
  }
  const unset = Array.isArray(drain.env_pass_unset) ? drain.env_pass_unset.filter(Boolean) : [];
  const uncredentialed = Array.isArray(drain.uncredentialed_crews) ? drain.uncredentialed_crews.filter(Boolean) : [];
  if (uncredentialed.length) {
    const flag = el("div", {
      class: "drain-queued-flag",
      text: `No Claude credential for ${uncredentialed.join(", ")}: this window was started without ${unset.join(", ")}.`,
    });
    flag.setAttribute("role", "note");
    row.append(flag);
  } else if (unset.length) {
    row.append(el("div", { class: "drain-queued-env", text: `Started without ${unset.join(", ")}` }));
  }
  return row;
}

// Cancels this queued run alone through the run cancel path; a never-started
// window has admitted nothing, and the live window is not touched.
function autoDrainQueuedCancel(payload, runId) {
  const key = `auto-drain:cancel-queued:${runId}`;
  const refusal = workspaceReadOnlyReason();
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: "operation-button drain-queued-cancel",
    text: pending ? "Cancelling…" : "Cancel",
    title: refusal || `Cancel queued window ${runId} before it starts`,
  });
  button.type = "button";
  button.dataset.drainFocus = `cancel-queued:${runId}`;
  button.setAttribute("aria-label", `Cancel queued window ${runId}`);
  button.disabled = Boolean(refusal) || pending;
  button.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    if (pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    if (!window.confirm(`Cancel queued window ${runId}? It has not started and has admitted no work; the live window keeps running.\n\nWorkspace "${workspace?.name || workspace?.id}"`)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", `Cancelling queued window ${runId}…`);
    renderAutoDrain(payload);
    try {
      const result = await postJson(visit.path(`/api/runs/${encodeURIComponent(runId)}/cancel`), { reason: null, force: false });
      if (!visit.isCurrent()) return;
      feedback("auto-drain-operation-feedback", "success", result?.outcome === "already_terminal"
        ? `Queued window ${runId} had already ended.`
        : `Queued window ${runId} cancelled.`);
      await refreshDrainAfterAction();
    } catch (error) {
      if (visit.isCurrent()) feedback("auto-drain-operation-feedback", "error", `Cancelling queued window ${runId} failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (operationsState.lastAutoDrain) renderAutoDrain(operationsState.lastAutoDrain);
    }
  });
  return button;
}

// Whether the live window was started with approve-proposed, and what its
// passes have done so far. The payload's `approvals` reports the status
// drain's own input: `approved_total` counts the whole window, while the
// held figures describe the latest pass. Findings live on each task.
function autoDrainApprovals(approvals) {
  if (!approvals || approvals.enabled !== true) return null;
  const approved = Number(approvals.approved_total);
  const held = Number(approvals.held_total);
  const reasons = Object.entries(approvals.held_by_reason || {});
  return {
    approved: Number.isFinite(approved) ? approved : "—",
    held: Number.isFinite(held) && held > 0 ? `${held} held` : "",
    title: `Approving proposed tasks. ${AUTO_DRAIN_APPROVE_RULE}`,
    reasons,
  };
}

// A Start-card field's label with its help as an info hint beside it; `warn`
// tints the hint amber. The hint sits outside the <label>, so pressing it never
// focuses or toggles the control.
function renderAutoDrain(payload) {
  operationsState.lastAutoDrain = payload;
  setDrainReadiness(payload);
  const body = $("auto-drain-body");
  if (!body) return;
  // A poll or a control click rebuilds both cards; keep keyboard focus on the
  // control that had it.
  const focusKey = document.activeElement?.dataset?.drainFocus;
  body.textContent = "";
  renderAutoDrainHead(payload);
  const reasons = autoDrainReasons(payload);
  const holds = [
    autoDrainThrottleLine(payload.capacity),
    autoDrainProviderLimitNote(payload.provider_limits),
    autoDrainProviderAuth(payload.provider_auth_failures),
    drainCrewNote(payload),
    drainPilotNote(payload),
  ].filter(Boolean);
  if (reasons.submit) {
    body.append(...[autoDrainForecast(payload), ...holds, autoDrainQueued(payload)].filter(Boolean), el("div", { class: "operations-readonly-note", text: reasons.submit }));
    renderAutoDrainStart(payload, reasons);
    return;
  }
  body.append(...[
    autoDrainWindow(payload),
    autoDrainCapacity(payload.capacity || {}, autoDrainCounts(payload), autoDrainBlocked(payload).length),
    ...holds,
    autoDrainForecast(payload),
    autoDrainFoot(payload),
    autoDrainQueued(payload),
  ].filter(Boolean));
  renderAutoDrainStart(payload, reasons);
  if (focusKey) document.querySelector?.(`[data-drain-focus="${focusKey}"]`)?.focus();
}

function clearStartCard(id) {
  const body = $(`${id}-body`);
  if (body) body.textContent = "";
  const card = $(id);
  if (card) card.hidden = true;
}

registerOperationsReset(() => {
  announcedDrainState = null;
  updateDrainIndicators("idle", "idle");
  clearStartCard("auto-drain-quick-start");
});

configureOperationsFleetCallbacks({ updateDrainIndicators, clearStartCard });
function fetchAndRenderAutoDrain({ detailTaskId = null, forecast = true } = {}) {
  const workspace = selectedWorkspace();
  if (!workspace) {
    const token = ++operationsState.aggregateFleetToken;
    const revision = getWorkspaceRevision();
    const guard = () => token === operationsState.aggregateFleetToken && revision === getWorkspaceRevision() && !selectedWorkspace();
    return requestPanel("auto-drain-body", "fleet", () => loadFleetSnapshot({ refreshWhenStale: true, onPartial: payload => { if (guard()) paintAggregateFleet(payload); } }), payload => {
      paintAggregateFleet(payload);
    }, "auto-drain-phase-label");
  }
  const concurrency = autoDrainConcurrencyValid() ? autoDrainConcurrencyValue() : "";
  const params = new URLSearchParams();
  if (concurrency) params.set("concurrency", concurrency);
  if (detailTaskId) params.set("also_task", detailTaskId);
  const query = params.size ? `?${params.toString()}` : "";
  const scope = "/api/workflows/auto/readiness";
  const path = `${scope}${query}`;
  const visit = captureWorkspaceVisit();
  const readiness = requestPanel("auto-drain-body", scope, () => fetchJson(path), payload => {
    renderAutoDrain(payload);
  }, "auto-drain-phase-label").catch(error => {
    if (detailTaskId && visit.isCurrent()) {
      setDrainReadinessFailure(detailTaskId, error.message || String(error));
    }
    throw error;
  });
  const fleet = loadFleetSnapshot({ refreshWhenStale: false, onPartial: payload => { if (visit.isCurrent()) { renderFleetIndicator(payload); } } }).then(payload => {
    if (visit.isCurrent()) {
      renderFleetIndicator(payload);
    }
    return payload;
  });
  // The optional summary paints itself and contains its failures. A slow
  // forecast must not delay dashboard refresh or release of action controls.
  // The Tasks page's 30 s tick leaves it to the page's slow reads.
  if (forecast) void refreshBacklogForecast();
  return Promise.all([readiness, fleet]).then(() => {});
}


configureOperationsDrainControls({
  render: renderAutoDrain,
  fetch: () => {
    // Action refreshes replace the shared snapshot just like a poll. Keep
    // the selected task in it even when it is outside the bounded backlog.
    const selectedTask = !taskState.pinnedExternalTask
      ? taskList(taskState.paneContext).find(task => task.id === taskState.selectedTaskId)
      : null;
    const detailTaskId = ["proposed", "backlog"].includes(selectedTask?.status) ? selectedTask.id : null;
    return fetchAndRenderAutoDrain({ detailTaskId });
  },
  holdAnnouncement: held => { holdDrainAnnouncement = held; },
});

// Readiness refreshes on every page to feed the chip and task readiness;
// `forecast: false` skips the backlog forecast read.
export async function fetchAndRenderAutoDrainPane({ detailTaskId = null, forecast = true } = {}) {
  await fetchAndRenderAutoDrain({ detailTaskId, forecast });
}
