// Routine-definition, host sweep-clock, and auto-task operations [ORB-10875, ORB-10876].

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
  const reason = controlReason(payload, "clock_service");
  const button = el("button", { class: "operation-button", text: pendingOperations.has(key) ? "Pending…" : label, title: reason });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", () => {
    if (reason || !selection.current() || pendingOperations.has(key)) return;
    const verb = action === "enable" ? "Start" : "Stop";
    if (!window.confirm(`${verb} the ${payload.clock.provider} sweep clock on ${payload.host_id}? This does not change any routine definition.`)) return;
    return runOperation({
      selection, key, feedbackId: "clock-operation-feedback",
      pending: `${verb} host sweep clock…`, failure: "Clock service change failed",
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
  const reason = controlReason(payload, "clock_cadence");
  const selection = selectionSnapshot();
  const key = `clock:${payload.host_id}`;
  body.append(
    el("div", { class: "operation-clock-summary" }, [
      el("span", { class: `operation-state ${clock.health}`, text: clock.health }),
      el("span", { class: "mono", text: clock.provider }),
      el("span", { text: clock.enabled ? "service enabled" : "service paused" }),
    ]),
    el("div", { class: "operation-row-facts" }, [
      operationFact("Cadence", cadenceText(clock.configured_cadence_seconds)),
      operationFact("Next tick", clockTickText(clock.next_tick_at, clock)),
    ]),
    operationDetails("clock", [
      el("div", { class: "operation-grid" }, [
        field("Configured cadence", cadenceText(clock.configured_cadence_seconds)),
        field("Effective cadence", cadenceText(clock.effective_cadence_seconds)),
        field("Loaded", clock.loaded ? "Yes" : "No"),
        field("Running / waiting", clock.running == null ? "Provider does not expose" : clock.running ? "Yes" : "No"),
        field("Last tick", clock.last_tick_at ? time(clock.last_tick_at) : "Provider does not expose"),
        field("Next expected tick", clockTickText(clock.next_tick_at, clock)),
      ]),
    ]),
  );
  if (clock.health_issue) body.appendChild(el("p", { class: "operation-control-note error", text: clock.health_issue }));
  const actions = el("div", { class: "operation-clock-actions" });
  actions.appendChild(clockButton(payload, clock.enabled ? "disable" : "enable", clock.enabled ? "Pause clock" : "Enable clock"));
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
    if (reason || !selection.current()) return;
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
  const eligible = tasks.filter((task) => task.eligible).length;
  return { eligible, waiting: tasks.length - eligible };
}

function autoDrainStartButton(payload) {
  const key = "auto-drain:start";
  const reasons = autoDrainReasons(payload);
  const pending = pendingOperations.has(key);
  const button = el("button", {
    class: "operation-button secondary",
    text: pending ? "Starting…" : "Start bounded window",
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
  body.append(
    el("div", { class: "operation-grid" }, [
      field("Active leaf runs", `${capacity.active_leaf_runs ?? "—"} / ${capacity.max_active_leaf_runs ?? "—"}`),
      field("Free slots", capacity.free_slots),
      field("Eligible now", counts.eligible),
      field("Waiting", counts.waiting),
    ]),
  );
  body.appendChild(el("p", {
    class: "operation-control-note",
    text: "Proposed tasks are never drained automatically; promote a task to backlog first. This snapshot can change the instant after it is read.",
  }));

  const durationSelect = el("select", { class: "operation-cadence", title: "Bounded drain window" });
  for (const value of AUTO_DRAIN_DURATIONS) {
    const option = el("option", { text: value });
    option.value = value;
    option.selected = value === autoDrainDuration;
    durationSelect.appendChild(option);
  }
  durationSelect.addEventListener("change", () => {
    autoDrainDuration = durationSelect.value;
  });

  const concurrencyInput = el("input", { class: "operation-cadence", title: "Leaf-run concurrency (blank = runtime default)" });
  concurrencyInput.type = "number";
  concurrencyInput.min = "1";
  concurrencyInput.placeholder = "Default";
  concurrencyInput.value = autoDrainConcurrency;
  concurrencyInput.addEventListener("change", () => {
    autoDrainConcurrency = concurrencyInput.value.trim();
  });

  const completeLabel = el("label", { class: "operation-field", title: reasons.complete || AUTO_DRAIN_COMPLETE_WARNING });
  const completeCheckbox = el("input");
  completeCheckbox.type = "checkbox";
  completeCheckbox.checked = autoDrainComplete;
  completeCheckbox.disabled = Boolean(reasons.complete);
  completeCheckbox.addEventListener("change", () => {
    autoDrainComplete = completeCheckbox.checked;
    renderAutoDrain(payload);
  });
  completeLabel.append(completeCheckbox, el("span", { text: " Also mark shipped tasks done (skip review)" }));

  const form = el("div", { class: "operation-clock-actions" }, [durationSelect, concurrencyInput, completeLabel, autoDrainStartButton(payload)]);
  body.appendChild(form);
  if (autoDrainComplete) {
    body.appendChild(el("p", { class: "operation-control-note operation-mint-warning", text: AUTO_DRAIN_COMPLETE_WARNING }));
  }
  if (lastAutoDrainRun && lastAutoDrainRun.workspaceId === workspace?.id) {
    const runLink = el("a", {
      class: "operation-control-note",
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

  $("auto-drain-count").textContent = `${counts.eligible} eligible · ${workspace?.name || workspace?.id}`;
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
