// Routine-definition, host clock, and auto-task operations [ORB-10875, ORB-10876].

import { requestPanel, detailsPanel, el, fetchJson, getWorkspace, getWorkspaceRevision, onWorkspaceChange, postJson } from './common.js';
import { navigateToRun } from './router.js';
import { renderAutomation } from './automation.js';

const $ = (id) => document.getElementById(id);
const pendingOperations = new Set();
const UNCONDITIONAL_MINT_WARNING = "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.";
const AUTO_DRAIN_DURATIONS = ["15m", "30m", "1h", "2h", "4h", "8h"];
const AUTO_DRAIN_COMPLETE_WARNING = "Also marks every task this window ships as done (review -> done), not only the ones eligible right now.";
let lastOperations = null;
let lastAutoTasks = null;
let lastAutoDrain = null;
let autoDrainDuration = "1h";
let autoDrainConcurrency = "";
let autoDrainComplete = false;
let lastAutoDrainRun = null;
// Whether the dependency-waiting rows are expanded; held outside the render so
// a background refresh or a duration click does not collapse them.
let autoDrainDependencyRowsOpen = false;
let lastOperationMode = null;
let context = null;
let unsubscribeWorkspace = null;
// The operator's unapplied cadence choice, held outside the rebuilt <select>
// so a background refresh cannot revert it. Host-scoped, like the clock itself.
let pendingCadenceSeconds = null;

export function initOperations(nextContext) {
  context = nextContext;
  unsubscribeWorkspace?.();
  unsubscribeWorkspace = onWorkspaceChange(() => {
    lastOperations = lastAutoTasks = lastAutoDrain = lastOperationMode = null;
    for (const id of ["routine-operation-feedback", "clock-operation-feedback", "auto-task-operation-feedback", "auto-drain-operation-feedback", "operation-mode-operation-feedback"]) feedback(id, "", "");
  });
}

function selectedWorkspace() {
  const selected = getWorkspace();
  if (!selected) return null;
  return context.getWorkspaces().find((workspace) => workspace.id === selected) || null;
}

function selectedWorkspaceName() {
  return selectedWorkspace()?.name || null;
}

function workspaceReadOnlyReason() {
  const selected = getWorkspace();
  if (!selected) return "All-workspace mode is read-only. Select one workspace; auto-task definitions are workspace-scoped.";
  const workspace = selectedWorkspace();
  if (!workspace) return `Workspace ${selected} is not a concrete active selection.`;
  if (workspace.status && workspace.status !== "active") {
    return `Workspace ${workspace.name} is inactive; select an active workspace.`;
  }
  return "";
}

function feedback(id, kind, message) {
  const node = $(id);
  if (!node) return;
  node.className = `operation-feedback ${kind || ""}`;
  node.textContent = message || "";
}

function timezoneName(date) {
  return new Intl.DateTimeFormat("en-US", { timeZoneName: "short" })
    .formatToParts(date)
    .find((part) => part.type === "timeZoneName")?.value || "UTC";
}

function looksLikeDuration(value) {
  const text = String(value).trim();
  return /\d+\s*(?:h|hr|hrs|hour|hours|min|mins|minute|minutes|s|sec|secs|seconds)\b/i.test(text)
    && Number.isNaN(new Date(text).getTime());
}

function time(value) {
  if (!value) return "Not observed";
  if (looksLikeDuration(value)) return `Duration ${value}`;
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return String(value);
  const formatted = String(context.formatAbsoluteTime(value));
  const tz = timezoneName(parsed);
  if (formatted.includes(tz) || /\bUTC\b/.test(formatted) || /[+-]\d{2}:\d{2}$/.test(formatted)) {
    return formatted;
  }
  return `${formatted} ${tz}`;
}

function cadenceText(seconds) {
  if (seconds == null || seconds === "") return "Inactive";
  const n = Number(seconds);
  if (!Number.isFinite(n)) return String(seconds);
  if (n % 60 === 0) {
    const minutes = n / 60;
    const label = minutes === 1 ? "every 1 minute" : `every ${minutes} minutes`;
    return `${label} (${n}s)`;
  }
  return `every ${n}s`;
}

function clockUnavailable(clock) {
  return clock?.health === "unknown" || (typeof clock?.error === "string" && clock.error.length > 0);
}

function clockUnavailableReason(clock) {
  return clock?.health_issue || clock?.error || "Clock state is unavailable; controls are disabled.";
}

function nextEvaluationText(projection, fallbackAt) {
  const state = projection?.state;
  const at = projection?.at || (state === "disabled" || state === "paused" ? fallbackAt : null);
  const when = at ? time(at) : null;
  if (state === "disabled") return when ? `Disabled · hypothetical next ${when}` : "Disabled";
  if (state === "paused") return when ? `Paused · hypothetical next ${when}` : "Paused";
  if (state === "waiting") return "Waiting for deliveries";
  if (state === "never_observed") return "Never observed";
  if (state === "unavailable") return "Unavailable";
  if (state === "scheduled") return when || "Scheduled";
  return when || "Unavailable";
}

function lastMintedText(definition) {
  if (!definition.last_minted_task_id) return "None";
  const status = definition.last_minted_task_status ? ` · ${definition.last_minted_task_status}` : "";
  const schedulerId = definition.last_evaluation?.last_task_id;
  const source = schedulerId && definition.last_minted_task_id !== schedulerId
    ? " · manual mint"
    : schedulerId
      ? " · scheduler"
      : "";
  return `${definition.last_minted_task_id}${status}${source}`;
}

function clockTickText(value, clock) {
  if (value) {
    if (looksLikeDuration(value)) {
      return `Duration from boot ${value} (not a wall-clock tick)`;
    }
    return time(value);
  }
  if (clockUnavailable(clock)) return "Unknown";
  if (!clock.enabled) return "Paused";
  if (clock.schedulable) return "Armed; exact wall-clock time unavailable";
  return "Not scheduled";
}

function field(label, value) {
  return el("div", { class: "operation-field" }, [
    el("span", { class: "operation-field-label", text: label }),
    el("span", { class: "operation-field-value", text: value == null || value === "" ? "—" : String(value) }),
  ]);
}

function lastFireText(fire) {
  if (!fire) return "Never";
  const when = time(fire.finished_at || fire.started_at);
  return fire.state ? `${fire.state} · ${when}` : when;
}

function routineScheduleText(routine) {
  if (routine.trigger?.deliveries_landed) {
    return `${routine.trigger.deliveries_landed.threshold} verified deliveries on ${routine.trigger.deliveries_landed.branch}`;
  }
  return routine.cron || "—";
}

function operationIdentity(name, state) {
  return el("div", { class: "operation-identity" }, [
    el("strong", { text: name }),
    el("span", { class: `operation-state ${state}`, text: state }),
  ]);
}

function operationFact(label, value) {
  return el("div", { class: "operation-fact" }, [
    el("span", { class: "operation-fact-label", text: label }),
    el("span", { class: "operation-fact-value", text: value == null || value === "" ? "—" : String(value) }),
  ]);
}

function operationDetails(key, children) {
  const panel = detailsPanel(key, { class: "operation-details" });
  panel.appendChild(el("summary", { text: "Details" }));
  for (const child of children) {
    if (child) panel.appendChild(child);
  }
  return panel;
}

function actionReason(payload, action) {
  const selectionReason = workspaceReadOnlyReason();
  if (selectionReason) return selectionReason;
  const capability = payload.capabilities?.[action];
  if (capability) return capability.authorized === true ? "" : capability.reason || "Action unavailable. See session access above.";
  return "Action availability is unavailable. Refresh the dashboard server and this page.";
}

function controlReason(payload, action = "routine_toggle") {
  return actionReason(payload, action) || "";
}

function explainUnavailable(button, reason) {
  if (!reason) return button;
  // The wrapper is focusable because native disabled buttons cannot receive focus.
  const wrapper = el("span", { class: "operation-action-unavailable" }, [button]);
  wrapper.tabIndex = 0;
  wrapper.title = reason;
  wrapper.setAttribute("aria-label", `${button.textContent}: ${reason}`);
  return wrapper;
}

function selectionSnapshot() {
  const workspace = getWorkspace();
  const revision = getWorkspaceRevision();
  return {
    workspace,
    revision,
    current: () => workspace === getWorkspace() && revision === getWorkspaceRevision(),
  };
}

// Keep the guard until readback finishes; old results never replace a new selection.
async function runOperation({ selection, key, feedbackId, pending, failure, render, request, refresh, success }) {
  if (!selection.current() || pendingOperations.has(key)) return;
  pendingOperations.add(key);
  feedback(feedbackId, "pending", pending);
  render();
  try {
    const result = await request();
    if (!selection.current()) return;
    feedback(feedbackId, "success", success(result));
    if (result.task_id) {
      const link = el("a", { text: ` Open ${result.task_id} →` });
      link.href = `?workspace=${encodeURIComponent(selection.workspace)}#tasks?status=all&q=${encodeURIComponent(result.task_id)}`;
      $(feedbackId)?.appendChild(link);
    }
    try {
      await refresh();
    } catch (error) {
      if (selection.current()) {
        $(feedbackId)?.appendChild(el("span", {
          text: ` Refresh failed: ${error.message}. The action succeeded; refresh before submitting another action.`,
        }));
      }
    }
  } catch (error) {
    if (selection.current()) feedback(feedbackId, "error", `${failure}: ${error.message}`);
  } finally {
    pendingOperations.delete(key);
    // Repaint current cached data only; host clock guards span workspaces.
    render();
  }
}

function routineButton(payload, routine) {
  const selection = selectionSnapshot();
  const nextEnabled = !routine.enabled;
  const key = `routine:${selection.workspace}:${routine.name}`;
  const reason = controlReason(payload);
  const button = el("button", {
    class: `operation-button ${nextEnabled ? "enable" : "disable"}`,
    text: pendingOperations.has(key) ? "Pending…" : nextEnabled ? "Enable" : "Disable",
    title: reason || `${nextEnabled ? "Enable" : "Disable"} ${routine.target}`,
  });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", () => {
    if (reason || !selection.current()) return;
    return runOperation({
      selection, key, feedbackId: "routine-operation-feedback",
      pending: `Updating ${routine.name} → ${routine.target}…`, failure: "Routine change failed",
      render: () => { if (lastOperations) renderOperations(lastOperations); },
      request: () => postJson("/api/routines/toggle", {
        name: routine.name, source: routine.source, target: routine.target,
        host_id: payload.host_id, expected_enabled: routine.enabled, enabled: nextEnabled,
      }),
      refresh: fetchAndRenderOperations,
      success: (result) => `${result.message}: ${routine.name} → ${routine.target}.`,
    });
  });
  return explainUnavailable(button, reason);
}

function renderOperations(payload) {
  lastOperations = payload;
  if ($("operations-session")) $("operations-session").textContent = payload.session_explanation || "Operations actions require the capabilities granted to this dashboard server. Refresh to load session access details.";
  const workspace = selectedWorkspaceName();
  const routines = workspace
    ? (payload.routines || []).filter((routine) => routine.source === workspace)
    : [];
  const body = $("routines-body");
  body.textContent = "";
  if (!workspace) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: `All-workspace mode is read-only. Select one workspace; this host is already resolved as ${payload.host_id}.` }));
  }
  if (routines.length === 0) {
    body.appendChild(el("div", { class: "empty-state", text: workspace ? "No routines are defined by this workspace." : "Select a workspace to list its routines." }));
  }
  for (const routine of routines) {
    const fire = routine.last_fire;
    const state = routine.enabled ? (routine.effective ? "enabled" : "blocked") : "disabled";
    const schedule = routineScheduleText(routine);
    const detailsKey = `routine:${routine.name}`;
    const card = el("article", { class: "operation-card routine-card" });
    card.append(
      el("div", { class: "operation-row-head" }, [
        operationIdentity(routine.name, state),
        el("div", { class: "operation-row-facts" }, [
          operationFact("Schedule", schedule),
          operationFact("Next", nextEvaluationText(routine.next_evaluation, routine.next_due)),
          operationFact("Last outcome", lastFireText(fire)),
        ]),
        el("div", { class: "operation-card-actions" }, [routineButton(payload, routine)]),
      ]),
      operationDetails(detailsKey, [
        el("div", { class: "operation-target mono", text: routine.target }),
        el("div", { class: "operation-grid" }, [
          field("Source workspace", routine.source),
          field("Schedule", schedule),
          field("Last evaluation", time(routine.last_evaluated_slot || routine.first_observed_at)),
          field("Next evaluation", nextEvaluationText(routine.next_evaluation, routine.next_due)),
          field("Last fire", fire ? time(fire.finished_at || fire.started_at) : "Never"),
          field("Linked run / outcome", fire ? `${fire.run_id || "No run"} · ${fire.state}` : "No fire recorded"),
        ]),
        renderAutomation(routine.automation, `${detailsKey}:automation`),
        routine.description ? el("p", { class: "operation-description", text: routine.description }) : null,
      ]),
    );
    body.appendChild(card);
  }
  $("routines-count").textContent = workspace ? `${routines.length} · ${workspace}` : "read-only";
  renderClock(payload);
}

function clockButton(payload, action, label) {
  const selection = selectionSnapshot();
  const key = `clock:${payload.host_id}`;
  const unavailable = clockUnavailable(payload.clock);
  const reason = unavailable
    ? clockUnavailableReason(payload.clock)
    : controlReason(payload, "clock_service");
  const button = el("button", { class: "operation-button", text: pendingOperations.has(key) ? "Pending…" : label, title: reason });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", () => {
    if (reason || unavailable || !selection.current() || pendingOperations.has(key)) return;
    const verb = action === "enable" ? "Start" : "Stop";
    if (!window.confirm(`${verb} the ${payload.clock.provider} clock on ${payload.host_id}? This does not change any routine definition.`)) return;
    return runOperation({
      selection, key, feedbackId: "clock-operation-feedback",
      pending: `${verb} host clock…`, failure: "Clock service change failed",
      render: () => { if (lastOperations) renderClock(lastOperations); },
      request: () => postJson("/api/routines/clock", {
        action, host_id: payload.host_id, expected_enabled: payload.clock.enabled,
        expected_cadence_seconds: payload.clock.configured_cadence_seconds,
      }),
      refresh: fetchAndRenderOperations,
      success: (result) => `${result.message} on ${payload.host_id}.`,
    });
  });
  return explainUnavailable(button, reason);
}

function renderClock(payload) {
  const clock = payload.clock;
  const body = $("clock-body");
  body.textContent = "";
  const unavailable = clockUnavailable(clock);
  const reason = unavailable
    ? clockUnavailableReason(clock)
    : controlReason(payload, "clock_cadence");
  const selection = selectionSnapshot();
  const key = `clock:${payload.host_id}`;
  const serviceLabel = unavailable ? "service unknown" : (clock.enabled ? "service enabled" : "service paused");
  const cadenceLabel = unavailable ? "Unknown" : cadenceText(clock.configured_cadence_seconds);
  const effectiveCadenceLabel = unavailable ? "Unknown" : cadenceText(clock.effective_cadence_seconds);
  body.append(
    el("div", { class: "operation-clock-summary" }, [
      el("span", { class: `operation-state ${clock.health}`, text: clock.health }),
      el("span", { class: "mono", text: unavailable ? (clock.provider || "unknown") : clock.provider }),
      el("span", { text: serviceLabel }),
    ]),
    el("div", { class: "operation-row-facts" }, [
      operationFact("Cadence", cadenceLabel),
      operationFact("Next tick", clockTickText(clock.next_tick_at, clock)),
    ]),
    operationDetails("clock", [
      el("div", { class: "operation-grid" }, [
        field("Configured cadence", cadenceLabel),
        field("Effective cadence", effectiveCadenceLabel),
        field("Loaded", unavailable ? "Unknown" : clock.loaded ? "Yes" : "No"),
        field("Running / waiting", unavailable ? "Unknown" : clock.running == null ? "Provider does not expose" : clock.running ? "Yes" : "No"),
        field("Last tick", clock.last_tick_at ? time(clock.last_tick_at) : unavailable ? "Unknown" : "Provider does not expose"),
        field("Next expected tick", clockTickText(clock.next_tick_at, clock)),
      ]),
    ]),
  );
  if (clock.health_issue) body.appendChild(el("p", { class: "operation-control-note error", text: clock.health_issue }));
  const actions = el("div", { class: "operation-clock-actions" });
  actions.appendChild(clockButton(
    payload,
    unavailable ? "enable" : (clock.enabled ? "disable" : "enable"),
    unavailable ? "Clock unavailable" : (clock.enabled ? "Pause clock" : "Enable clock"),
  ));
  // A cadence the operator picked but has not applied yet outlives this render;
  // it clears once the host reports that value as configured, whoever applied it.
  if (pendingCadenceSeconds === clock.configured_cadence_seconds) pendingCadenceSeconds = null;
  const selectedCadence = pendingCadenceSeconds ?? clock.configured_cadence_seconds;
  const cadence = el("select", { class: "operation-cadence", title: "Clock cadence" });
  for (const seconds of [60, 300, 900, 1800, 3600]) {
    const option = el("option", { text: seconds === 60 ? "Every minute" : `Every ${seconds / 60} minutes` });
    option.value = String(seconds);
    option.selected = seconds === selectedCadence;
    cadence.appendChild(option);
  }
  cadence.disabled = Boolean(reason) || pendingOperations.has(key);
  const apply = el("button", { class: "operation-button secondary", text: pendingOperations.has(key) ? "Pending…" : "Apply cadence", title: "Reload cadence without changing whether the clock is enabled" });
  apply.type = "button";
  const syncApplyState = (chosen) => {
    apply.disabled = Boolean(reason) || pendingOperations.has(key) || chosen === clock.configured_cadence_seconds;
  };
  syncApplyState(selectedCadence);
  cadence.addEventListener("change", () => {
    pendingCadenceSeconds = Number(cadence.value);
    syncApplyState(pendingCadenceSeconds);
  });
  apply.addEventListener("click", () => {
    if (reason || unavailable || !selection.current()) return;
    return runOperation({
      selection, key, feedbackId: "clock-operation-feedback",
      pending: `Changing cadence to ${cadence.value}s…`, failure: "Cadence change failed",
      render: () => { if (lastOperations) renderClock(lastOperations); },
      request: () => postJson("/api/routines/clock", {
        action: "set_cadence", host_id: payload.host_id, expected_enabled: clock.enabled,
        expected_cadence_seconds: clock.configured_cadence_seconds, cadence_seconds: Number(cadence.value),
      }),
      refresh: fetchAndRenderOperations,
      success: (result) => `${result.message}; service is ${result.clock.enabled ? "enabled" : "paused"}.`,
    });
  });
  actions.append(cadence, explainUnavailable(apply, reason));
  body.appendChild(actions);
  $("clock-host").textContent = payload.host_id || "unknown host";
}

function lastEvaluationText(definition) {
  const evaluation = definition.last_evaluation;
  if (!evaluation) return "Never evaluated";
  if (evaluation.kind === "fired") {
    const task = evaluation.last_task_id ? ` · ${evaluation.last_task_id}` : "";
    return `${time(evaluation.last_fired_at || evaluation.last_slot)}${task}`;
  }
  return `Baselined ${time(evaluation.baseline_at)}; no fire yet`;
}

function lastOutcomeText(definition) {
  if (definition.last_minted_task_id) return lastMintedText(definition);
  return lastEvaluationText(definition);
}

function autoTaskToggleButton(payload, definition) {
  const selection = selectionSnapshot();
  const nextEnabled = !definition.enabled;
  const key = `auto-task-toggle:${selection.workspace}:${definition.name}`;
  const reason = actionReason(payload, "auto_task_toggle");
  const verb = nextEnabled ? "Enable" : "Disable";
  const button = el("button", {
    class: `operation-button ${nextEnabled ? "enable" : "disable"}`,
    text: pendingOperations.has(key) ? "Pending…" : verb, title: reason || `${verb} ${definition.name}`,
  });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", () => {
    if (reason || !selection.current() || pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const summary = definition.template_summary || definition.template?.title || definition.name;
    if (!window.confirm(`${verb} auto-task "${definition.name}" in workspace "${workspace?.name || workspace?.id}"?\n\nTarget: ${summary}\nThis writes the definition's enabled field.`)) return;
    return runOperation({
      selection, key, feedbackId: "auto-task-operation-feedback",
      pending: `${verb} ${definition.name}…`, failure: "Auto-task change failed",
      render: () => { if (lastAutoTasks) renderAutoTasks(lastAutoTasks); },
      request: () => postJson("/api/auto-tasks/toggle", {
        name: definition.name, expected_enabled: definition.enabled, enabled: nextEnabled,
      }),
      refresh: fetchAndRenderAutoTasks,
      success: (result) => `${result.message}: ${definition.name}.`,
    });
  });
  return explainUnavailable(button, reason);
}

function autoTaskMintButton(payload, definition) {
  const selection = selectionSnapshot();
  const key = `auto-task-mint:${selection.workspace}:${definition.name}`;
  const reason = actionReason(payload, "auto_task_mint");
  const button = el("button", {
    class: "operation-button secondary",
    text: pendingOperations.has(key) ? "Pending…" : "Mint now",
    title: reason || "Mint one task now, ignoring schedule, enabled, and dedupe",
  });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", async () => {
    if (reason || !selection.current() || pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const template = definition.template || {};
    const duplicateLine = definition.may_create_open_duplicate
      ? "An open instance already exists; this will create another open task."
      : "No open instance is currently tagged for this definition.";
    const confirmText = [
      `Mint auto-task "${definition.name}" now in workspace "${workspace?.name || workspace?.id}"?`,
      "",
      `Resulting task: ${definition.template_summary || template.title || definition.name}`,
      `Crew: ${template.crew || "none"} · Status: ${template.status || "backlog"} · Priority: ${template.priority || "medium"}`,
      "",
      `WARNING: ${payload.unconditional_mint_warning || UNCONDITIONAL_MINT_WARNING}`,
      duplicateLine,
    ].join("\n");
    if (!window.confirm(confirmText)) return;
    return runOperation({
      selection, key, feedbackId: "auto-task-operation-feedback",
      pending: `Minting ${definition.name} now…`, failure: "Manual mint failed",
      render: () => { if (lastAutoTasks) renderAutoTasks(lastAutoTasks); },
      request: () => postJson("/api/auto-tasks/mint", {
        name: definition.name, acknowledge_unconditional: true,
      }),
      refresh: fetchAndRenderAutoTasks,
      success: (result) => `${result.message}. Task created; no delivery was started.`,
    });
  });
  return explainUnavailable(button, reason);
}

function renderAutoTasks(payload) {
  lastAutoTasks = payload;
  const body = $("auto-tasks-body");
  if (!body) return;
  body.textContent = "";
  const workspaceReason = workspaceReadOnlyReason();
  const reason = workspaceReason || payload.read_only_reason || "";
  const workspace = selectedWorkspace();
  const definitions = workspace && !workspaceReason ? (payload.definitions || []) : [];
  if (reason) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: reason }));
  }
  if (definitions.length === 0) {
    body.appendChild(el("div", {
      class: "empty-state",
      text: workspace && !workspaceReason ? "No auto-task definitions are defined by this workspace." : "Select a workspace to list its auto-task definitions.",
    }));
  }
  for (const definition of definitions) {
    const state = definition.enabled ? "enabled" : "disabled";
    const detailsKey = `auto-task:${definition.name}`;
    const actions = el("div", { class: "operation-card-actions" }, [
      autoTaskToggleButton(payload, definition),
      autoTaskMintButton(payload, definition),
    ]);
    const duplicate = definition.open_duplicate ? "Yes — mint will create another" : "No";
    const card = el("article", { class: "operation-card auto-task-card" });
    card.append(
      el("div", { class: "operation-row-head" }, [
        operationIdentity(definition.name, state),
        el("div", { class: "operation-row-facts" }, [
          operationFact("Schedule", definition.schedule_summary || "—"),
          operationFact("Next", nextEvaluationText(definition.next_evaluation)),
          operationFact("Last outcome", lastOutcomeText(definition)),
        ]),
        actions,
      ]),
      operationDetails(detailsKey, [
        el("div", { class: "operation-target mono", text: definition.template_summary || definition.template?.title || "" }),
        el("div", { class: "operation-grid" }, [
          field("Schedule", definition.schedule_summary || "—"),
          field("Dedupe", definition.dedupe === "always" ? "always fire" : "skip if open"),
          field("Last scheduler evaluation", lastEvaluationText(definition)),
          field("Last minted task", lastMintedText(definition)),
          field("Next evaluation", nextEvaluationText(definition.next_evaluation)),
          field("Open duplicate", duplicate),
        ]),
        renderAutomation(definition.automation, `${detailsKey}:automation`),
        definition.description ? el("p", { class: "operation-description", text: definition.description }) : null,
        el("p", {
          class: "operation-control-note operation-mint-warning",
          text: payload.unconditional_mint_warning || UNCONDITIONAL_MINT_WARNING,
        }),
      ]),
    );
    body.appendChild(card);
  }
  const count = $("auto-tasks-count");
  if (count) count.textContent = workspace && !workspaceReason ? `${definitions.length} · ${workspace.name}` : "read-only";
}

function operationCountId(bodyId) {
  return bodyId === "clock-body" ? "clock-host" : bodyId.replace("-body", "-count");
}

function loadOperationPanel(bodyId, path, render) {
  return requestPanel(bodyId, path, () => fetchJson(path), render, operationCountId(bodyId));
}

function fetchAndRenderAutoTasks() {
  return loadOperationPanel("auto-tasks-body", "/api/auto-tasks", renderAutoTasks);
}

// ORB-11250: bounded backlog auto-drain window ("orbit run auto --for
// <duration> [--complete]" from the dashboard). One workspace-scoped action,
// not a per-row one, so it follows the mint/clock in-flight idiom (a single
// fixed `pendingOperations` key, guard released in `finally`) rather than
// tasks.js's per-task Ship guard.
function autoDrainReasons(payload) {
  const workspaceReason = workspaceReadOnlyReason();
  return {
    submit: workspaceReason,
    complete: workspaceReason || (payload.controls_authorized === false
      ? "Automatic completion requires an authorized operator session; the window can still start with default review completion."
      : ""),
  };
}

function autoDrainCounts(payload) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  const eligible = tasks.filter((task) => task.eligible === true).length;
  return { eligible, waiting: tasks.length - eligible };
}

const AUTO_DRAIN_REASON_LABELS = {
  ready: "Ready for admission",
  ready_as_epic: "Ready as the next epic",
  not_backlog: "Not in backlog",
  unmet_dependency: "Waiting on a dependency",
  task_pilot_preparation_required: "Task-pilot preparation required",
  crew_not_allowed: "Crew excluded by this drain",
  epic_managed: "Managed by an epic",
  admissions_stopped: "Admissions stopped",
  epic_run_active: "Another epic run is active",
  queued_behind_epic: "Queued behind another epic",
  context_lock_conflict: "Context is locked",
  group_member_conflict: "A grouped task conflicts",
  claimed_by_live_child: "Claimed by a live child run",
  outside_grant_scope: "Outside the active grant scope",
  grant_expired: "The active grant expired",
  grant_stopped: "The active grant stopped admitting",
  grant_revoked: "The active grant was revoked",
  outside_candidate_pool: "Outside the examined candidate pool",
  conflict_deferred: "Deferred behind a conflicting candidate",
  capacity_saturated: "No admission capacity",
};

function autoDrainTaskLink(taskId, workspace) {
  const link = el("a", { class: "auto-drain-reference mono", text: taskId, title: `Open task ${taskId}` });
  link.href = `?workspace=${encodeURIComponent(workspace.id)}#tasks?status=all&q=${encodeURIComponent(taskId)}`;
  return link;
}

function autoDrainRunLink(runId, workspace) {
  const link = el("a", { class: "auto-drain-reference mono", text: runId, title: `Open run ${runId}` });
  link.href = `?workspace=${encodeURIComponent(workspace.id)}#runs/${encodeURIComponent(runId)}`;
  link.addEventListener("click", (event) => {
    event.preventDefault();
    navigateToRun(runId, workspace.id);
  });
  return link;
}

function autoDrainEvidenceRow(label, values) {
  return el("div", { class: "auto-drain-evidence-row" }, [
    el("span", { class: "auto-drain-evidence-label", text: label }),
    el("div", { class: "auto-drain-evidence-values" }, values),
  ]);
}

function autoDrainTextValues(values) {
  return values
    .filter((value) => value != null && String(value).trim() !== "")
    .map((value) => el("span", { class: "auto-drain-evidence-value mono", text: String(value) }));
}

function autoDrainMissingEvidence(task, reason, evidence) {
  if (reason === "unmet_dependency" && evidence.dependencies === 0) {
    return "Dependency details were not supplied.";
  }
  if (["context_lock_conflict", "group_member_conflict", "conflict_deferred"].includes(reason)
    && evidence.conflicts === 0 && evidence.blockers === 0) {
    return "Conflict details were not supplied.";
  }
  if (reason === "claimed_by_live_child" && evidence.claimingRuns === 0) {
    return "Claiming run details were not supplied.";
  }
  if (reason === "capacity_saturated" && evidence.activeRuns === 0) {
    return "Active run details were not supplied.";
  }
  if (reason === "crew_not_allowed" && task.crew == null && !Array.isArray(task.allowed_crews)) {
    return "Crew restriction details were not supplied.";
  }
  if (["outside_grant_scope", "grant_expired", "grant_stopped", "grant_revoked"].includes(reason) && !task.grant_id) {
    return "Grant details were not supplied.";
  }
  if (!AUTO_DRAIN_REASON_LABELS[reason] && evidence.rows === 0) {
    return "No additional evidence was supplied for this server reason.";
  }
  return "";
}

function autoDrainTaskEvidence(task, workspace) {
  const rows = [];
  if (task.status && task.status !== "backlog") {
    rows.push(autoDrainEvidenceRow("Task status", autoDrainTextValues([task.status])));
  }
  const dependencies = Array.isArray(task.dependencies) ? task.dependencies : [];
  if (dependencies.length > 0) {
    rows.push(autoDrainEvidenceRow("Dependencies", dependencies.map((dependency) => {
      const taskId = typeof dependency === "string" ? dependency : dependency?.task_id;
      const value = el("span", { class: "auto-drain-evidence-value" });
      if (taskId) value.appendChild(autoDrainTaskLink(taskId, workspace));
      else value.appendChild(el("span", { text: "Unknown dependency" }));
      if (dependency?.status) value.appendChild(el("span", { text: ` · ${dependency.status}` }));
      return value;
    })));
  }

  const conflicts = Array.isArray(task.conflicts) ? task.conflicts : [];
  if (conflicts.length > 0) {
    rows.push(autoDrainEvidenceRow("Conflicts", conflicts.map((conflict) => {
      const value = el("span", { class: "auto-drain-evidence-value" });
      value.appendChild(el("span", { class: "mono", text: conflict?.requested_file || "File not supplied" }));
      if (conflict?.locking_task_id) {
        value.append(el("span", { text: " · held by " }), autoDrainTaskLink(conflict.locking_task_id, workspace));
      }
      return value;
    })));
  }

  const blockingTaskIds = Array.isArray(task.blocking_task_ids) ? task.blocking_task_ids : [];
  if (blockingTaskIds.length > 0) {
    rows.push(autoDrainEvidenceRow("Blocking tasks", blockingTaskIds.map((taskId) => autoDrainTaskLink(taskId, workspace))));
  }
  const liveRunIds = Array.isArray(task.run_ids) ? task.run_ids : [];
  if (liveRunIds.length > 0) {
    rows.push(autoDrainEvidenceRow("Claiming runs", liveRunIds.map((runId) => autoDrainRunLink(runId, workspace))));
  }
  const activeRunIds = Array.isArray(task.active_run_ids) ? task.active_run_ids : [];
  if (activeRunIds.length > 0) {
    rows.push(autoDrainEvidenceRow("Active runs", activeRunIds.map((runId) => autoDrainRunLink(runId, workspace))));
  }
  if (task.crew != null || Array.isArray(task.allowed_crews)) {
    const allowed = Array.isArray(task.allowed_crews) && task.allowed_crews.length > 0
      ? task.allowed_crews.join(", ")
      : "none supplied";
    rows.push(autoDrainEvidenceRow("Crew", autoDrainTextValues([`${task.crew ?? "not supplied"} · allowed: ${allowed}`])));
  }
  if (task.grant_id) rows.push(autoDrainEvidenceRow("Grant", autoDrainTextValues([task.grant_id])));
  if (task.epic_run_id) rows.push(autoDrainEvidenceRow("Active epic run", [autoDrainRunLink(task.epic_run_id, workspace)]));
  if (task.next_epic_task_id) rows.push(autoDrainEvidenceRow("Next epic", [autoDrainTaskLink(task.next_epic_task_id, workspace)]));

  const reason = typeof task.reason === "string" && task.reason.trim() ? task.reason : "unknown";
  const evidenceMissing = autoDrainMissingEvidence(task, reason, {
    dependencies: dependencies.length,
    conflicts: conflicts.length,
    blockers: blockingTaskIds.length,
    claimingRuns: liveRunIds.length,
    activeRuns: activeRunIds.length,
    rows: rows.length,
  });
  if (evidenceMissing) rows.push(autoDrainEvidenceRow("Details", autoDrainTextValues([evidenceMissing])));

  return rows;
}

// Readiness groups. The server hands back one flat row per task with a
// reason; the pane sorts those into the three questions an operator asks
// before starting a window — what starts now, what is only waiting on a
// running task, and what is waiting on other backlog — and leaves every
// other reason in a fourth group so no server row is dropped.
const AUTO_DRAIN_LOCK_REASONS = new Set(["context_lock_conflict", "group_member_conflict", "conflict_deferred", "claimed_by_live_child"]);

function autoDrainTaskId(task) {
  return typeof task.task_id === "string" && task.task_id.trim() ? task.task_id : null;
}

function autoDrainReason(task) {
  return typeof task.reason === "string" && task.reason.trim() ? task.reason : "unknown";
}

function autoDrainGroups(tasks) {
  const groups = { eligible: [], locked: [], dependency: [], other: [] };
  for (const task of tasks) {
    const reason = autoDrainReason(task);
    if (task.eligible === true) groups.eligible.push(task);
    else if (AUTO_DRAIN_LOCK_REASONS.has(reason)) groups.locked.push(task);
    else if (reason === "unmet_dependency") groups.dependency.push(task);
    else groups.other.push(task);
  }
  return groups;
}

// Holder ids for a lock-blocked row. Context locks report
// `conflicts[].locking_task_id`; same-wave deferrals report
// `conflicts[].blocking_task_id` plus `blocking_task_ids`; live-child claims
// report only `run_ids`, which have no task holder and fall into "unknown".
function autoDrainHolders(task) {
  const holders = new Set();
  for (const conflict of Array.isArray(task.conflicts) ? task.conflicts : []) {
    const holder = conflict?.locking_task_id || conflict?.blocking_task_id;
    if (holder) holders.add(holder);
  }
  for (const holder of Array.isArray(task.blocking_task_ids) ? task.blocking_task_ids : []) {
    if (holder) holders.add(holder);
  }
  return [...holders].sort();
}

function autoDrainConflictSelectors(task, holder) {
  const selectors = [];
  for (const conflict of Array.isArray(task.conflicts) ? task.conflicts : []) {
    const conflictHolder = conflict?.locking_task_id || conflict?.blocking_task_id;
    if (holder && conflictHolder && conflictHolder !== holder) continue;
    const selector = conflict?.requested_file || conflict?.requested_selector || conflict?.blocking_selector;
    if (selector && !selectors.includes(selector)) selectors.push(selector);
  }
  return selectors;
}

function autoDrainDependencyIds(task) {
  const ids = [];
  for (const dependency of Array.isArray(task.dependencies) ? task.dependencies : []) {
    const taskId = typeof dependency === "string" ? dependency : dependency?.task_id;
    if (taskId && !ids.includes(taskId)) ids.push(taskId);
  }
  return ids;
}

// Depth of every dependency-waiting task within its own group: 0 for a task
// whose dependencies all lie outside the group (the chain roots), otherwise
// one more than its deepest in-group dependency. A cycle, which the server
// should never emit, stops at the visited node rather than recursing.
function autoDrainDependencyDepths(tasks) {
  const byId = new Map(tasks.map((task) => [autoDrainTaskId(task), task]).filter(([id]) => id));
  const depths = new Map();
  const visiting = new Set();
  const depth = (id) => {
    if (depths.has(id)) return depths.get(id);
    if (visiting.has(id)) return 0;
    visiting.add(id);
    let value = 0;
    for (const dependency of autoDrainDependencyIds(byId.get(id))) {
      if (byId.has(dependency)) value = Math.max(value, depth(dependency) + 1);
    }
    visiting.delete(id);
    depths.set(id, value);
    return value;
  };
  for (const id of byId.keys()) depth(id);
  return depths;
}

// Dependencies referenced by the waiting group that are not themselves in it:
// the tasks the whole group is really waiting on.
function autoDrainDependencyRoots(tasks, allTasks) {
  const inGroup = new Set(tasks.map(autoDrainTaskId).filter(Boolean));
  const byId = new Map(allTasks.map((task) => [autoDrainTaskId(task), task]).filter(([id]) => id));
  const roots = new Map();
  for (const task of tasks) {
    for (const dependency of Array.isArray(task.dependencies) ? task.dependencies : []) {
      const taskId = typeof dependency === "string" ? dependency : dependency?.task_id;
      if (!taskId || inGroup.has(taskId) || roots.has(taskId)) continue;
      const row = byId.get(taskId);
      const status = typeof dependency === "object" ? dependency?.status : null;
      roots.set(taskId, row
        ? AUTO_DRAIN_REASON_LABELS[autoDrainReason(row)] || autoDrainReason(row)
        : status || "outside this snapshot");
    }
  }
  return roots;
}

function autoDrainTaskIdentity(task, workspace) {
  const taskId = autoDrainTaskId(task);
  return taskId
    ? autoDrainTaskLink(taskId, workspace)
    : el("strong", { class: "auto-drain-missing-id", text: "Task ID not supplied" });
}

function autoDrainReasonText(task) {
  const reason = autoDrainReason(task);
  return `${AUTO_DRAIN_REASON_LABELS[reason] || "Unknown readiness reason"} · ${reason}`;
}

// The full per-task card: identity, state pill, server reason, and every
// reason-specific evidence row. Used as-is inside the expanded dependency and
// "other" groups so nothing the server said is hidden.
function autoDrainTaskCard(task, workspace) {
  const eligible = task.eligible === true;
  const state = eligible ? "eligible" : "waiting";
  const item = el("li", { class: `operation-card auto-drain-task ${state}` });
  item.appendChild(el("div", { class: "auto-drain-task-head" }, [
    autoDrainTaskIdentity(task, workspace),
    el("span", { class: `operation-state ${eligible ? "enabled" : "waiting"}`, text: state }),
    el("span", { class: "auto-drain-reason", text: autoDrainReasonText(task) }),
  ]));
  const evidence = autoDrainTaskEvidence(task, workspace);
  if (evidence.length > 0) item.appendChild(el("div", { class: "auto-drain-evidence" }, evidence));
  return item;
}

function autoDrainGroupHeader(tone, title, count, hint, trailing = null) {
  return el("div", { class: "auto-drain-group-head" }, [
    el("div", { class: "auto-drain-group-title" }, [
      el("span", { class: `auto-drain-group-dot ${tone}` }),
      el("strong", { text: title }),
      el("span", { class: "auto-drain-group-count mono", text: String(count) }),
    ]),
    trailing || (hint ? el("span", { class: "auto-drain-group-hint", text: hint }) : null),
  ]);
}

function autoDrainEligibleGroup(tasks, workspace) {
  const group = el("section", { class: "auto-drain-group" });
  group.appendChild(autoDrainGroupHeader("eligible", "Eligible now", tasks.length,
    tasks.length > 0 ? "Admitted in this order when the window starts" : "Nothing in this snapshot can start right now"));
  if (tasks.length === 0) return group;
  const list = el("ol", { class: "auto-drain-rows" });
  tasks.forEach((task, index) => {
    const reason = autoDrainReason(task);
    list.appendChild(el("li", { class: "auto-drain-row auto-drain-row-eligible auto-drain-task eligible" }, [
      el("span", { class: "auto-drain-row-index mono", text: String(index + 1) }),
      autoDrainTaskIdentity(task, workspace),
      el("span", { class: "auto-drain-row-label", text: AUTO_DRAIN_REASON_LABELS[reason] || "Unknown readiness reason" }),
      el("span", { class: "auto-drain-row-reason mono eligible", text: reason }),
    ]));
  });
  group.appendChild(list);
  return group;
}

function autoDrainSelectorChips(selectors) {
  const chips = el("div", { class: "auto-drain-chips" });
  const visible = selectors.slice(0, 3);
  for (const selector of visible) chips.appendChild(el("span", { class: "auto-drain-chip mono", text: selector, title: selector }));
  if (selectors.length > visible.length) {
    const more = el("details", { class: "auto-drain-chips-more" });
    more.appendChild(el("summary", { class: "auto-drain-chip mono", text: `+${selectors.length - visible.length} more` }));
    more.appendChild(el("div", { class: "auto-drain-chips" }, selectors.slice(visible.length).map((selector) =>
      el("span", { class: "auto-drain-chip mono", text: selector, title: selector }))));
    chips.appendChild(more);
  }
  return chips;
}

// Lock-blocked rows grouped by the task holding the lock, so one running task
// that holds five files reads as one holder with five files, not five rows
// each repeating "held by".
function autoDrainLockedGroup(tasks, workspace, occupancy) {
  const group = el("section", { class: "auto-drain-group" });
  group.appendChild(autoDrainGroupHeader("locked", "Blocked by a running task", tasks.length,
    tasks.length > 0 ? "Become eligible when the holder finishes" : ""));
  if (tasks.length === 0) return group;

  const slotPhase = new Map();
  for (const run of Array.isArray(occupancy?.runs) ? occupancy.runs : []) {
    for (const taskId of Array.isArray(run?.task_ids) ? run.task_ids : []) {
      if (run.phase) slotPhase.set(taskId, run.phase);
    }
  }

  // One row per task, filed under its first holder; further holders are
  // named on the row so the count of rows stays the count of tasks.
  const byHolder = new Map();
  for (const task of tasks) {
    const holder = autoDrainHolders(task)[0] ?? "";
    if (!byHolder.has(holder)) byHolder.set(holder, []);
    byHolder.get(holder).push(task);
  }

  for (const [holder, blocked] of [...byHolder.entries()].sort(([a], [b]) => a.localeCompare(b))) {
    const head = el("div", { class: "auto-drain-holder" });
    if (holder) {
      head.append(
        el("span", { text: "held by " }),
        autoDrainTaskLink(holder, workspace),
      );
      const phase = slotPhase.get(holder);
      head.appendChild(el("span", {
        class: `auto-drain-holder-phase mono ${phase ? "running" : ""}`,
        text: phase ? `${phase.replaceAll("_", " ")} · occupying a slot` : "not in an occupied slot",
      }));
    } else {
      head.appendChild(el("span", { text: "holder not supplied" }));
    }
    group.appendChild(head);
    const list = el("ul", { class: "auto-drain-rows" });
    for (const task of blocked) {
      const reason = autoDrainReason(task);
      const selectors = autoDrainConflictSelectors(task, holder || null);
      const detail = el("div", { class: "auto-drain-row-detail" });
      if (selectors.length > 0) detail.appendChild(autoDrainSelectorChips(selectors));
      const runIds = Array.isArray(task.run_ids) ? task.run_ids : [];
      if (runIds.length > 0) {
        detail.appendChild(el("div", { class: "auto-drain-row-note" }, [
          el("span", { text: "claimed by " }),
          ...runIds.flatMap((runId, index) => [index > 0 ? el("span", { text: ", " }) : null, autoDrainRunLink(runId, workspace)]),
        ]));
      }
      const otherHolders = autoDrainHolders(task).filter((other) => other !== holder);
      if (holder && otherHolders.length > 0) {
        detail.appendChild(el("div", { class: "auto-drain-row-note" }, [
          el("span", { text: "also blocked by " }),
          ...otherHolders.flatMap((other, index) => [index > 0 ? el("span", { text: ", " }) : null, autoDrainTaskLink(other, workspace)]),
        ]));
      }
      if (selectors.length === 0 && runIds.length === 0) {
        detail.appendChild(el("div", { class: "auto-drain-row-note", text: "Conflict details were not supplied." }));
      }
      list.appendChild(el("li", { class: "auto-drain-row auto-drain-row-locked auto-drain-task waiting" }, [
        autoDrainTaskIdentity(task, workspace),
        el("span", { class: "auto-drain-row-reason mono", text: reason, title: AUTO_DRAIN_REASON_LABELS[reason] || "Unknown readiness reason" }),
        detail,
      ]));
    }
    group.appendChild(list);
  }
  return group;
}

// Dependency-waiting rows collapsed to what they have in common: the tasks
// outside the group the chain bottoms out on, and the chain itself by depth.
// The full cards stay one click away so every server row is still shown.
function autoDrainDependencyGroup(tasks, allTasks, workspace) {
  const group = el("section", { class: "auto-drain-group" });
  const list = el("ul", { class: "auto-drain-task-list auto-drain-group-rows" });
  list.id = "auto-drain-dependency-rows";
  const toggle = el("button", { class: "auto-drain-group-toggle" });
  toggle.type = "button";
  toggle.setAttribute("aria-controls", list.id);
  const applyOpen = () => {
    list.hidden = !autoDrainDependencyRowsOpen;
    toggle.setAttribute("aria-expanded", autoDrainDependencyRowsOpen ? "true" : "false");
    toggle.textContent = autoDrainDependencyRowsOpen ? "Hide rows" : `Show all ${tasks.length}`;
  };
  toggle.addEventListener("click", () => {
    autoDrainDependencyRowsOpen = !autoDrainDependencyRowsOpen;
    applyOpen();
  });
  applyOpen();
  group.appendChild(autoDrainGroupHeader("dependency", "Waiting on dependencies", tasks.length,
    "Chained behind other backlog tasks", tasks.length > 0 ? toggle : null));
  if (tasks.length === 0) return group;

  const body = el("div", { class: "auto-drain-group-body" });
  const roots = autoDrainDependencyRoots(tasks, allTasks);
  const lead = el("p", { class: "auto-drain-chain-lead" });
  if (roots.size === 0) {
    lead.textContent = `All ${tasks.length} wait on each other; the server reported no dependency outside this group.`;
  } else {
    lead.append(el("span", { text: `All ${tasks.length} chain back to ` }));
    [...roots.entries()].forEach(([taskId, why], index) => {
      if (index > 0) lead.appendChild(el("span", { text: index === roots.size - 1 ? " and " : ", " }));
      lead.append(autoDrainTaskLink(taskId, workspace), el("span", { class: "auto-drain-chain-why", text: ` (${why})` }));
    });
    lead.appendChild(el("span", { text: ". Nothing here can start until that clears." }));
  }
  body.appendChild(lead);

  const depths = autoDrainDependencyDepths(tasks);
  const byDepth = new Map();
  for (const task of tasks) {
    const taskId = autoDrainTaskId(task);
    if (!taskId) continue;
    const depth = depths.get(taskId) ?? 0;
    if (!byDepth.has(depth)) byDepth.set(depth, []);
    byDepth.get(depth).push(taskId);
  }
  const chain = el("div", { class: "auto-drain-chain" });
  const levels = [...byDepth.keys()].sort((a, b) => a - b);
  if (roots.size > 0) {
    for (const taskId of roots.keys()) chain.appendChild(el("span", { class: "auto-drain-chain-node mono root" }, [autoDrainTaskLink(taskId, workspace)]));
    chain.appendChild(el("span", { class: "auto-drain-chain-arrow", text: "→" }));
  }
  levels.forEach((level, index) => {
    if (index > 0) chain.appendChild(el("span", { class: "auto-drain-chain-arrow", text: "→" }));
    const ids = byDepth.get(level).sort();
    chain.appendChild(el("span", { class: "auto-drain-chain-node mono" }, ids.flatMap((taskId, i) =>
      [i > 0 ? el("span", { class: "auto-drain-chain-sep", text: " · " }) : null, autoDrainTaskLink(taskId, workspace)])));
  });
  body.appendChild(chain);

  for (const task of tasks) list.appendChild(autoDrainTaskCard(task, workspace));
  body.appendChild(list);
  group.appendChild(body);
  return group;
}

function autoDrainOtherGroup(tasks, workspace) {
  const group = el("section", { class: "auto-drain-group" });
  group.appendChild(autoDrainGroupHeader("other", "Other reasons", tasks.length, "Every remaining server reason, with its evidence"));
  const list = el("ul", { class: "auto-drain-task-list" });
  for (const task of tasks) list.appendChild(autoDrainTaskCard(task, workspace));
  group.appendChild(list);
  return group;
}

function autoDrainReadinessList(payload, workspace) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  const section = el("section", { class: "auto-drain-readiness" });
  const heading = el("h3", { class: "auto-drain-readiness-title", text: "Task readiness" });
  heading.id = "auto-drain-readiness-title";
  section.setAttribute("aria-labelledby", heading.id);
  section.appendChild(heading);
  if (tasks.length === 0) {
    section.appendChild(el("div", {
      class: "empty-state auto-drain-empty",
      text: "No readiness rows were returned in this bounded snapshot. This does not establish that the workspace has no backlog tasks.",
    }));
    return section;
  }

  const groups = autoDrainGroups(tasks);
  section.appendChild(autoDrainEligibleGroup(groups.eligible, workspace));
  if (groups.locked.length > 0) section.appendChild(autoDrainLockedGroup(groups.locked, workspace, payload.capacity?.occupancy));
  if (groups.dependency.length > 0) section.appendChild(autoDrainDependencyGroup(groups.dependency, tasks, workspace));
  if (groups.other.length > 0) section.appendChild(autoDrainOtherGroup(groups.other, workspace));
  return section;
}

// Slot tiles: one per configured leaf slot, occupied ones naming the task(s)
// the run carries and its phase, so "2 / 5" is visible as which two.
function autoDrainSlots(capacity, workspace) {
  const occupancy = capacity.occupancy || {};
  const runs = Array.isArray(occupancy.runs) ? occupancy.runs : [];
  const active = Number(capacity.active_leaf_runs ?? occupancy.active_leaf_runs ?? runs.length);
  const max = Number(capacity.max_active_leaf_runs);
  const free = Number(capacity.free_slots ?? occupancy.free_slots);
  const phaseEntries = Object.entries(occupancy.phases || {}).filter(([, count]) => Number(count) > 0);
  const phaseSummary = phaseEntries.map(([phase, count]) => `${count} ${phase.replaceAll("_", "-")}`).join(", ");
  const section = el("section", { class: "auto-drain-slots" });
  section.appendChild(el("div", { class: "auto-drain-slots-head" }, [
    el("div", { class: "auto-drain-slots-title" }, [
      el("span", { class: "operation-field-label", text: "Slots" }),
      el("span", { class: "auto-drain-slots-count mono", text: Number.isFinite(max) ? `${active} / ${max} occupied` : `${active} occupied` }),
    ]),
    el("span", { class: "auto-drain-group-hint", text: [
      phaseSummary,
      capacity.limit_source ? `limit from ${String(capacity.limit_source).replaceAll("_", " ")}` : "",
    ].filter(Boolean).join(" · ") }),
  ]));
  const tiles = el("div", { class: "auto-drain-slot-grid" });
  if (runs.length === 0) {
    // Older servers roll occupancy up by phase only; one tile per counted
    // phase keeps the picture honest without inventing task ids.
    for (const [phase, count] of phaseEntries) {
      for (let index = 0; index < Number(count); index += 1) {
        tiles.appendChild(el("div", { class: `auto-drain-slot occupied ${phase}` }, [
          el("span", { class: "auto-drain-slot-tasks mono", text: "task not supplied" }),
          el("span", { class: "auto-drain-slot-phase mono", text: phase.replaceAll("_", " ") }),
        ]));
      }
    }
  }
  for (const run of runs) {
    const taskIds = Array.isArray(run?.task_ids) ? run.task_ids : [];
    const phase = run?.phase ? String(run.phase).replaceAll("_", " ") : "phase not supplied";
    const tile = el("div", { class: `auto-drain-slot occupied ${run?.phase || ""}` });
    const ids = el("div", { class: "auto-drain-slot-tasks" });
    if (taskIds.length === 0) ids.appendChild(run?.run_id ? autoDrainRunLink(run.run_id, workspace) : el("span", { class: "mono", text: "task not supplied" }));
    for (const taskId of taskIds) ids.appendChild(autoDrainTaskLink(taskId, workspace));
    tile.append(ids, el("span", { class: "auto-drain-slot-phase mono", text: phase }));
    tiles.appendChild(tile);
  }
  const freeTiles = Number.isFinite(free) ? Math.max(0, free) : 0;
  for (let index = 0; index < freeTiles; index += 1) {
    tiles.appendChild(el("div", { class: "auto-drain-slot free", text: "free" }));
  }
  if (tiles.childElementCount === 0) {
    tiles.appendChild(el("div", { class: "auto-drain-slot free", text: "Slot details were not supplied" }));
  }
  section.appendChild(tiles);
  return section;
}

function autoDrainStat(tone, label, value, note) {
  return el("div", { class: `auto-drain-stat ${tone}` }, [
    el("span", { class: "operation-field-label", text: label }),
    el("span", { class: "auto-drain-stat-value mono", text: value == null || value === "" ? "—" : String(value) }),
    el("span", { class: "auto-drain-stat-note", text: note }),
  ]);
}

function autoDrainSummary(payload, groups, counts) {
  const capacity = payload.capacity || {};
  const total = counts.eligible + counts.waiting;
  const free = Number(capacity.free_slots);
  const eligibleNote = !Number.isFinite(free) ? "free slots unknown"
    : counts.eligible === 0 ? "nothing to admit"
    : counts.eligible >= free ? "fills every free slot"
    : `leaves ${free - counts.eligible} slot${free - counts.eligible === 1 ? "" : "s"} free`;
  const holders = new Set(groups.locked.flatMap(autoDrainHolders));
  const roots = autoDrainDependencyRoots(groups.dependency, Array.isArray(payload.tasks) ? payload.tasks : []);
  const poolNote = capacity.candidate_pool_size == null
    ? "candidate pool not supplied"
    : `candidate pool ${capacity.candidate_pool_size}${capacity.candidate_pool_truncated ? " · truncated" : ""}`;
  return el("div", { class: "auto-drain-summary" }, [
    autoDrainStat("eligible", "Eligible now", counts.eligible, eligibleNote),
    autoDrainStat("locked", "Blocked by a running task", groups.locked.length,
      holders.size > 0 ? `held by ${holders.size} task${holders.size === 1 ? "" : "s"}` : "no holders reported"),
    autoDrainStat("dependency", "Waiting on deps", groups.dependency.length,
      roots.size > 0 ? `${roots.size === 1 ? "one chain, rooted at" : "rooted at"} ${[...roots.keys()].join(", ")}` : "no chain roots reported"),
    autoDrainStat("", "Waiting", counts.waiting, `of ${total} scanned · ${poolNote}`),
  ]);
}

function autoDrainStartButton(payload) {
  const key = "auto-drain:start";
  const reasons = autoDrainReasons(payload);
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: "operation-button primary auto-drain-start",
    text: pending ? "Starting…" : `Start ${autoDrainDuration} window`,
    title: reasons.submit || "Submit orbit.workflow.auto with this duration and concurrency",
  });
  button.type = "button";
  button.disabled = Boolean(reasons.submit) || pending || autoDrainComplete && Boolean(reasons.complete);
  button.addEventListener("click", async () => {
    if (pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const counts = autoDrainCounts(payload);
    const completeLine = autoDrainComplete
      ? `WARNING: ${AUTO_DRAIN_COMPLETE_WARNING}`
      : "Shipped tasks stay in review; a separate action completes them.";
    const confirmText = [
      `Start a bounded auto-delivery window in workspace "${workspace?.name || workspace?.id}"?`,
      `Duration: ${autoDrainDuration} · Concurrency: ${autoDrainConcurrency || "runtime default"}`,
      `Currently eligible: ${counts.eligible} · waiting: ${counts.waiting}`,
      "",
      completeLine,
    ].join("\n");
    if (!window.confirm(confirmText)) return;
    pendingOperations.add(key);
    feedback("auto-drain-operation-feedback", "pending", `Starting a ${autoDrainDuration} auto-delivery window…`);
    renderAutoDrain(payload);
    try {
      const body = { for_duration: autoDrainDuration, complete: autoDrainComplete };
      if (autoDrainConcurrency) body.concurrency = Number(autoDrainConcurrency);
      const result = await postJson("/api/workflows/auto", body);
      const runId = result?.run_id ?? null;
      const state = result?.state ?? "submitted";
      const completion = result?.completion ?? "review";
      feedback("auto-drain-operation-feedback", "success", `Run ${runId ?? "(no run id)"} ${state} (completion: ${completion}).`);
      lastAutoDrainRun = runId ? { runId, state, completion, workspaceId: workspace?.id } : null;
      await fetchAndRenderAutoDrain();
    } catch (error) {
      feedback("auto-drain-operation-feedback", "error", `Auto-delivery window failed to start: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastAutoDrain) renderAutoDrain(lastAutoDrain);
    }
  });
  return button;
}

// The window controls sit above the readiness groups: the operator reads the
// slot picture, then acts, instead of scrolling past every row to find the
// button. The duration picker is a segmented control over the same bounded
// list the confirm text quotes.
function autoDrainControls(payload, counts) {
  const reasons = autoDrainReasons(payload);
  const capacity = payload.capacity || {};
  const controls = el("section", { class: "auto-drain-controls" });

  const durationGroup = el("div", { class: "auto-drain-control" });
  durationGroup.appendChild(el("span", { class: "operation-field-label", text: "Duration" }));
  const segment = el("div", { class: "auto-drain-segment" });
  segment.setAttribute("role", "group");
  segment.setAttribute("aria-label", "Window duration");
  for (const value of AUTO_DRAIN_DURATIONS) {
    const option = el("button", { class: `auto-drain-segment-option${value === autoDrainDuration ? " selected" : ""}`, text: value });
    option.type = "button";
    option.setAttribute("aria-pressed", value === autoDrainDuration ? "true" : "false");
    option.addEventListener("click", () => {
      autoDrainDuration = value;
      renderAutoDrain(payload);
    });
    segment.appendChild(option);
  }
  durationGroup.appendChild(segment);

  const concurrencyGroup = el("div", { class: "auto-drain-control" });
  const concurrencyLabel = el("label", { class: "operation-field-label", text: "Concurrency" });
  concurrencyLabel.htmlFor = "auto-drain-concurrency";
  const concurrencyInput = el("input", { class: "operation-cadence auto-drain-concurrency", title: "Leaf-run concurrency (blank = runtime default)" });
  concurrencyInput.id = "auto-drain-concurrency";
  concurrencyInput.type = "number";
  concurrencyInput.min = "1";
  concurrencyInput.placeholder = Number.isFinite(Number(capacity.max_active_leaf_runs)) ? `Runtime default (${capacity.max_active_leaf_runs})` : "Runtime default";
  concurrencyInput.value = autoDrainConcurrency;
  concurrencyInput.addEventListener("change", () => {
    autoDrainConcurrency = concurrencyInput.value.trim();
  });
  concurrencyGroup.append(concurrencyLabel, concurrencyInput);

  const completeGroup = el("div", { class: "auto-drain-control" });
  completeGroup.appendChild(el("span", { class: "operation-field-label", text: "Completion" }));
  const completeLabel = el("label", { class: "auto-drain-complete", title: reasons.complete || AUTO_DRAIN_COMPLETE_WARNING });
  const completeCheckbox = el("input");
  completeCheckbox.type = "checkbox";
  completeCheckbox.checked = autoDrainComplete;
  completeCheckbox.disabled = Boolean(reasons.complete);
  completeCheckbox.addEventListener("change", () => {
    autoDrainComplete = completeCheckbox.checked;
    renderAutoDrain(payload);
  });
  completeLabel.append(completeCheckbox, el("span", { text: "Also mark shipped tasks done " }), el("span", { class: "auto-drain-muted", text: "(skip review)" }));
  completeGroup.appendChild(completeLabel);
  if (reasons.complete) completeGroup.appendChild(el("span", { class: "auto-drain-control-note", text: reasons.complete }));

  const free = Number(capacity.free_slots);
  const admits = Number.isFinite(free) ? Math.min(free, counts.eligible) : counts.eligible;
  const outcome = el("div", { class: "auto-drain-outcome" });
  outcome.append(
    el("span", { text: "Admits up to " }),
    el("strong", { text: `${admits} task${admits === 1 ? "" : "s"}` }),
    el("span", { text: Number.isFinite(free) ? " into " : "" }),
    Number.isFinite(free) ? el("strong", { text: `${free} free slot${free === 1 ? "" : "s"}` }) : null,
    el("br"),
    el("span", { text: "Completion " }),
    el("strong", { text: autoDrainComplete ? "done (skip review)" : "review" }),
    el("span", { text: " · concurrency " }),
    el("strong", { text: autoDrainConcurrency || "runtime default" }),
  );

  controls.appendChild(el("div", { class: "auto-drain-controls-row" }, [
    el("div", { class: "auto-drain-controls-fields" }, [durationGroup, concurrencyGroup, completeGroup]),
    el("div", { class: "auto-drain-controls-action" }, [autoDrainStartButton(payload), outcome]),
  ]));
  if (autoDrainComplete) {
    controls.appendChild(el("p", { class: "operation-control-note operation-mint-warning", text: AUTO_DRAIN_COMPLETE_WARNING }));
  }
  const total = counts.eligible + counts.waiting;
  const limitations = payload.snapshot?.limitations || "Snapshot only: eligibility can change immediately and does not guarantee a task will start.";
  const truncated = capacity.candidate_pool_truncated ? " The candidate pool was truncated before all available slots could be filled." : "";
  controls.appendChild(el("p", {
    class: "auto-drain-snapshot-note",
    title: limitations,
    text: `Snapshot only: nothing is reserved or started until you start a window. The server returned a bounded snapshot of ${total} task${total === 1 ? "" : "s"}; these counts are not a workspace total.${truncated} Proposed tasks are never drained automatically; promote a task to backlog first.`,
  }));
  return controls;
}

function renderAutoDrain(payload) {
  lastAutoDrain = payload;
  const body = $("auto-drain-body");
  if (!body) return;
  body.textContent = "";
  const reasons = autoDrainReasons(payload);
  const workspace = selectedWorkspace();
  if (reasons.submit) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: reasons.submit }));
    $("auto-drain-count").textContent = "read-only";
    return;
  }
  const counts = autoDrainCounts(payload);
  const capacity = payload.capacity || {};
  const groups = autoDrainGroups(Array.isArray(payload.tasks) ? payload.tasks : []);

  body.appendChild(autoDrainControls(payload, counts));
  if (lastAutoDrainRun && lastAutoDrainRun.workspaceId === workspace?.id) {
    const runLink = el("a", {
      class: "operation-control-note auto-drain-last-run",
      text: `Open run ${lastAutoDrainRun.runId} (${lastAutoDrainRun.state}, completion: ${lastAutoDrainRun.completion}) →`,
      title: "Open the submitted parent run",
    });
    runLink.href = "#";
    runLink.addEventListener("click", (event) => {
      event.preventDefault();
      navigateToRun(lastAutoDrainRun.runId, lastAutoDrainRun.workspaceId);
    });
    body.appendChild(runLink);
  }
  body.appendChild(autoDrainSlots(capacity, workspace));
  body.appendChild(autoDrainSummary(payload, groups, counts));
  body.appendChild(autoDrainReadinessList(payload, workspace));

  const slots = Number.isFinite(Number(capacity.max_active_leaf_runs))
    ? ` · ${capacity.active_leaf_runs ?? "—"}/${capacity.max_active_leaf_runs} slots`
    : "";
  $("auto-drain-count").textContent = `${counts.eligible} eligible${slots} · ${workspace?.name || workspace?.id}`;
}

function fetchAndRenderAutoDrain() {
  const workspace = selectedWorkspace();
  if (!workspace) {
    return requestPanel("auto-drain-body", "unselected", () => Promise.resolve({}), renderAutoDrain, "auto-drain-count");
  }
  const query = autoDrainConcurrency ? `?concurrency=${encodeURIComponent(autoDrainConcurrency)}` : "";
  return loadOperationPanel("auto-drain-body", `/api/workflows/auto/readiness${query}`, renderAutoDrain);
}

// ORB-11332: operation mode. The panel projects `orbit operation explain`
// (current preferences, the captured grant policy when one is active, caps,
// and limiting reasons) and offers the two governed grant controls.
// Enablement is deliberately not a dashboard action: a grant names a finite
// task set and explicit rights, which is an operator decision made from the
// CLI or MCP.
const OPERATION_MODE_CONTROLS = {
  stop: {
    label: "Stop grant",
    confirm: "Stop new admissions and promotion under this grant? Admitted work keeps its captured bounds, including completion. This is not cancellation.",
    enabled: (authority) => authority.admission === "open",
  },
  revoke: {
    label: "Revoke grant",
    confirm: "WARNING: revocation withdraws privileged actions, including completion, from work already admitted under this grant. Bound drains stop admitting. Continue?",
    enabled: (authority) => authority.status !== "revoked",
  },
};

function policyField(policy, name, label) {
  const entry = policy?.[name] || {};
  const value = entry.value ?? "—";
  const source = entry.source ? ` [${entry.source}]` : "";
  return field(label, `${value}${source}`);
}

function policyGrid(policy) {
  return el("div", { class: "operation-grid" }, [
    policyField(policy, "preset", "Preset"),
    policyField(policy, "preparation", "Preparation"),
    policyField(policy, "promotion", "Promotion"),
    policyField(policy, "completion", "Completion"),
    policyField(policy, "recovery", "Recovery"),
    policyField(policy, "leaf_ceiling", "Leaf ceiling"),
    policyField(policy, "review_policy", "Review policy"),
    policyField(policy, "review_crew", "Review crew (before-PR)"),
    policyField(policy, "review_reviewer_starts", "Reviewer starts / lineage"),
    policyField(policy, "review_repair_cycles", "Repair cycles / lineage"),
    policyField(policy, "review_minutes", "Review minutes / lineage"),
    policyField(policy, "delivery_cap", "Delivery cap"),
  ]);
}

function operationControlButton(payload, kind) {
  const control = OPERATION_MODE_CONTROLS[kind];
  const authority = payload.authority || {};
  const key = `operation:${kind}`;
  const pending = pendingOperations.has(key);
  const unauthorized = payload.controls_authorized === false;
  const button = el("button", {
    class: `operation-button ${kind === "revoke" ? "disable" : "secondary"}`,
    text: pending ? `${control.label}…` : control.label,
    title: unauthorized
      ? "Grant controls require an authorized operator session."
      : control.confirm,
  });
  button.type = "button";
  button.disabled = unauthorized || pending || !control.enabled(authority);
  button.addEventListener("click", async () => {
    if (pendingOperations.has(key)) return;
    if (!window.confirm(`${control.confirm}\n\nGrant: ${authority.grant_id} (revision ${authority.revision})`)) return;
    pendingOperations.add(key);
    feedback("operation-mode-operation-feedback", "pending", `${control.label} in progress…`);
    renderOperationMode(payload);
    try {
      const result = await postJson(`/api/operation/${kind}`, {
        grant_id: authority.grant_id,
        expected_revision: authority.revision,
      });
      feedback("operation-mode-operation-feedback", "success", `Grant ${result?.grant_id ?? authority.grant_id}: ${result?.outcome ?? kind} (revision ${result?.revision ?? "?"}).`);
      await fetchAndRenderOperationMode();
    } catch (error) {
      feedback("operation-mode-operation-feedback", "error", `${control.label} failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastOperationMode) renderOperationMode(lastOperationMode);
    }
  });
  return button;
}

function renderOperationMode(payload) {
  lastOperationMode = payload;
  const body = $("operation-mode-body");
  if (!body) return;
  body.textContent = "";
  const workspaceReason = workspaceReadOnlyReason();
  const workspace = selectedWorkspace();
  if (workspaceReason) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: workspaceReason }));
    $("operation-mode-count").textContent = "read-only";
    return;
  }
  const policy = payload.policy || {};
  const authority = payload.authority || {};
  const delivery = payload.delivery || {};
  const grantPolicy = authority.policy;
  body.append(
    el("div", { class: "operation-row-head" }, [
      operationIdentity(authority.grant_id || "No grant", authority.admission || "none"),
      el("div", { class: "operation-row-facts" }, [
        operationFact("Completion", `${delivery.effective_completion ?? "—"}${delivery.cap ? ` (cap: ${delivery.cap})` : ""}`),
        operationFact("Expires", authority.expires_at ? time(authority.expires_at) : "—"),
      ]),
    ]),
    operationDetails("operation-mode", [
      grantPolicy
        ? el("p", {
          class: "operation-control-note",
          text: "Active grant policy (captured at enablement; retuning preferences does not change it).",
        })
        : null,
      grantPolicy ? policyGrid(grantPolicy) : null,
      el("p", {
        class: "operation-control-note",
        text: grantPolicy
          ? "Current preferences (apply to a future grant only)."
          : "Current preferences.",
      }),
      policyGrid(policy),
      el("div", { class: "operation-grid" }, [
        field("Effective completion", `${delivery.effective_completion ?? "—"}${delivery.cap ? ` (cap: ${delivery.cap})` : ""}`),
        field("Grant", authority.grant_id ?? "none"),
        field("Admission", authority.admission ?? "none"),
        field("Rights", Array.isArray(authority.rights) && authority.rights.length ? authority.rights.join(", ") : "—"),
        field("Scope", Array.isArray(authority.task_ids) ? `${authority.task_ids.length} task(s)` : "—"),
        field("Expires", authority.expires_at ? time(authority.expires_at) : "—"),
      ]),
      el("p", {
        class: "operation-control-note",
        text: Array.isArray(payload.limiting_reasons) && payload.limiting_reasons.length
          ? `Limiting reasons: ${payload.limiting_reasons.join(", ")}`
          : "No limiting reasons.",
      }),
      el("p", {
        class: "operation-control-note",
        text: "Changing a preference activates nothing. Only an explicit grant (orbit operation enable) authorizes scoped automation, and no grant authorizes merge.",
      }),
      el("p", {
        class: "operation-control-note",
        text: "Review crew selects the reviewer for before-PR review only. After-landing review runs from its own delivery auto-task, which mints tasks with that definition's template crew.",
      }),
    ]),
  );
  if (authority.grant_id) {
    body.appendChild(el("div", { class: "operation-clock-actions" }, [
      operationControlButton(payload, "stop"),
      operationControlButton(payload, "revoke"),
    ]));
  }
  $("operation-mode-count").textContent = `${authority.grant_id ? authority.admission : "no grant"} · ${workspace?.name || workspace?.id}`;
}

export function fetchAndRenderOperationMode() {
  if (!selectedWorkspace()) {
    return requestPanel("operation-mode-body", "unselected", () => Promise.resolve({}), renderOperationMode, "operation-mode-count");
  }
  return loadOperationPanel("operation-mode-body", "/api/operation/explain", renderOperationMode);
}

export async function fetchAndRenderOperations() {
  const routines = fetchJson("/api/routines");
  const results = await Promise.allSettled([
    requestPanel("routines-body", "routines", () => routines, renderOperations, "routines-count"),
    requestPanel("clock-body", "clock", () => routines, renderClock, "clock-host"),
    fetchAndRenderAutoTasks(),
    fetchAndRenderAutoDrain(),
    fetchAndRenderOperationMode(),
  ]);
  const errors = results.filter(result => result.status === "rejected").map(result => result.reason);
  // Preserve transport classification even if a different panel also fails.
  if (errors.length) throw errors.find(error => error.networkFailure) || errors[0];
}
