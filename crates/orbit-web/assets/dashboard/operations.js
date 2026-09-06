// Routine-definition, host sweep-clock, and auto-task operations [ORB-10875, ORB-10876].

import { el, fetchJson, getWorkspace, postJson } from './common.js';
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
let context = null;

export function initOperations(nextContext) {
  context = nextContext;
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

function time(value) {
  if (!value) return "Not observed";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? String(value) : context.formatAbsoluteTime(value);
}

function field(label, value) {
  return el("div", { class: "operation-field" }, [
    el("span", { class: "operation-field-label", text: label }),
    el("span", { class: "operation-field-value", text: value == null || value === "" ? "—" : String(value) }),
  ]);
}

function controlReason(payload, routine) {
  if (!getWorkspace()) return "Select one workspace before changing routine or clock state.";
  if (!payload.controls_authorized) return "Controls require an authorized operator session.";
  if (routine && !routine.pinned_to_host) return `Select pinned host ${payload.host_id} before changing this routine.`;
  return "";
}

function routineButton(payload, routine) {
  const nextEnabled = !routine.enabled;
  const key = `routine:${routine.name}`;
  const reason = controlReason(payload, routine);
  const button = el("button", {
    class: `operation-button ${nextEnabled ? "enable" : "disable"}`,
    text: pendingOperations.has(key) ? "Pending…" : nextEnabled ? "Enable" : "Disable",
    title: reason || `${nextEnabled ? "Enable" : "Disable"} ${routine.target}`,
  });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", async () => {
    if (pendingOperations.has(key)) return;
    pendingOperations.add(key);
    feedback("routine-operation-feedback", "pending", `${nextEnabled ? "Enabling" : "Disabling"} ${routine.name} → ${routine.target}…`);
    renderOperations(payload);
    try {
      const result = await postJson("/api/routines/toggle", {
        name: routine.name,
        source: routine.source,
        target: routine.target,
        host_id: payload.host_id,
        expected_enabled: routine.enabled,
        enabled: nextEnabled,
      });
      feedback("routine-operation-feedback", "success", `${result.message}: ${routine.name} → ${routine.target}.`);
      await fetchAndRenderOperations();
    } catch (error) {
      feedback("routine-operation-feedback", "error", `Routine change failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastOperations) renderOperations(lastOperations);
    }
  });
  return button;
}

function renderOperations(payload) {
  lastOperations = payload;
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
    const card = el("article", { class: "operation-card routine-card" });
    card.append(
      el("div", { class: "operation-card-title" }, [
        el("div", {}, [el("strong", { text: routine.name }), el("span", { class: `operation-state ${state}`, text: state })]),
        routineButton(payload, routine),
      ]),
      el("div", { class: "operation-target mono", text: routine.target }),
      el("div", { class: "operation-grid" }, [
        field("Source workspace", routine.source),
        field("Schedule", routine.trigger?.deliveries_landed ? `${routine.trigger.deliveries_landed.threshold} verified deliveries on ${routine.trigger.deliveries_landed.branch}` : routine.cron),
        field("Host pin", (routine.hosts || []).join(", ") || "Local host"),
        field("Last evaluation", time(routine.last_evaluated_slot || routine.first_observed_at)),
        field("Next evaluation", time(routine.next_due)),
        field("Last fire", fire ? time(fire.finished_at || fire.started_at) : "Never"),
        field("Linked run / outcome", fire ? `${fire.run_id || "No run"} · ${fire.state}` : "No fire recorded"),
      ]),
    );
    const automation = renderAutomation(routine.automation);
    if (automation) card.appendChild(automation);
    if (routine.description) card.appendChild(el("p", { class: "operation-description", text: routine.description }));
    const reason = controlReason(payload, routine);
    if (reason) card.appendChild(el("p", { class: "operation-control-note", text: reason }));
    body.appendChild(card);
  }
  $("routines-count").textContent = workspace ? `${routines.length} · ${workspace}` : "read-only";
  renderClock(payload);
}

function clockButton(payload, action, label) {
  const key = "clock:service";
  const reason = controlReason(payload);
  const button = el("button", { class: "operation-button", text: pendingOperations.has(key) ? "Pending…" : label, title: reason });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", async () => {
    if (pendingOperations.has(key)) return;
    const verb = action === "enable" ? "Start" : "Stop";
    if (!window.confirm(`${verb} the ${payload.clock.provider} sweep clock on ${payload.host_id}? This does not change any routine definition.`)) return;
    pendingOperations.add(key);
    feedback("clock-operation-feedback", "pending", `${action === "enable" ? "Starting" : "Stopping"} the host sweep clock…`);
    renderClock(payload);
    try {
      const result = await postJson("/api/routines/clock", {
        action,
        host_id: payload.host_id,
        expected_enabled: payload.clock.enabled,
        expected_cadence_seconds: payload.clock.configured_cadence_seconds,
      });
      feedback("clock-operation-feedback", "success", `${result.message} on ${payload.host_id}.`);
      await fetchAndRenderOperations();
    } catch (error) {
      feedback("clock-operation-feedback", "error", `Clock service change failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastOperations) renderClock(lastOperations);
    }
  });
  return button;
}

function renderClock(payload) {
  const clock = payload.clock;
  const body = $("clock-body");
  body.textContent = "";
  const reason = controlReason(payload);
  if (reason) body.appendChild(el("div", { class: "operations-readonly-note", text: reason }));
  body.append(
    el("div", { class: "operation-clock-summary" }, [
      el("span", { class: `operation-state ${clock.health}`, text: clock.health }),
      el("span", { class: "mono", text: clock.provider }),
      el("span", { text: clock.enabled ? "service enabled" : "service paused" }),
    ]),
    el("div", { class: "operation-grid" }, [
      field("Configured cadence", `${clock.configured_cadence_seconds}s`),
      field("Effective cadence", clock.effective_cadence_seconds ? `${clock.effective_cadence_seconds}s` : "Inactive"),
      field("Loaded", clock.loaded ? "Yes" : "No"),
      field("Running / waiting", clock.running == null ? "Provider does not expose" : clock.running ? "Yes" : "No"),
      field("Last tick", clock.last_tick_at || "Provider does not expose"),
      field("Next expected tick", clock.next_tick_at || (clock.schedulable ? "Armed; exact time unavailable" : "Not scheduled")),
    ]),
  );
  if (clock.health_issue) body.appendChild(el("p", { class: "operation-control-note error", text: clock.health_issue }));
  const actions = el("div", { class: "operation-clock-actions" });
  actions.appendChild(clockButton(payload, clock.enabled ? "disable" : "enable", clock.enabled ? "Pause clock" : "Enable clock"));
  const cadence = el("select", { class: "operation-cadence", title: "Clock cadence" });
  for (const seconds of [60, 300, 900, 1800, 3600]) {
    const option = el("option", { text: seconds === 60 ? "Every minute" : `Every ${seconds / 60} minutes` });
    option.value = String(seconds);
    option.selected = seconds === clock.configured_cadence_seconds;
    cadence.appendChild(option);
  }
  cadence.disabled = Boolean(reason) || pendingOperations.has("clock:cadence");
  const apply = el("button", { class: "operation-button secondary", text: pendingOperations.has("clock:cadence") ? "Pending…" : "Apply cadence", title: "Reload cadence without changing whether the clock is enabled" });
  apply.type = "button";
  apply.disabled = Boolean(reason) || pendingOperations.has("clock:cadence") || Number(cadence.value) === clock.configured_cadence_seconds;
  cadence.addEventListener("change", () => { apply.disabled = Boolean(reason) || Number(cadence.value) === clock.configured_cadence_seconds; });
  apply.addEventListener("click", async () => {
    const key = "clock:cadence";
    if (pendingOperations.has(key)) return;
    pendingOperations.add(key);
    feedback("clock-operation-feedback", "pending", `Changing cadence to ${cadence.value}s; clock service state remains separate…`);
    renderClock(payload);
    try {
      const result = await postJson("/api/routines/clock", {
        action: "set_cadence",
        host_id: payload.host_id,
        expected_enabled: clock.enabled,
        expected_cadence_seconds: clock.configured_cadence_seconds,
        cadence_seconds: Number(cadence.value),
      });
      feedback("clock-operation-feedback", "success", `${result.message}; service is ${result.clock.enabled ? "enabled" : "paused"}.`);
      await fetchAndRenderOperations();
    } catch (error) {
      feedback("clock-operation-feedback", "error", `Cadence change failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastOperations) renderClock(lastOperations);
    }
  });
  actions.append(cadence, apply);
  body.appendChild(actions);
  $("clock-host").textContent = payload.host_id || "unknown host";
}

function autoTaskControlReason(payload) {
  const workspaceReason = workspaceReadOnlyReason();
  if (workspaceReason) return workspaceReason;
  if (payload && payload.read_only_reason) return payload.read_only_reason;
  if (payload && payload.controls_authorized === false) return "Controls require an authorized operator session.";
  return "";
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

function autoTaskToggleButton(payload, definition) {
  const nextEnabled = !definition.enabled;
  const key = `auto-task-toggle:${definition.name}`;
  const reason = autoTaskControlReason(payload);
  const verb = nextEnabled ? "Enable" : "Disable";
  const button = el("button", {
    class: `operation-button ${nextEnabled ? "enable" : "disable"}`,
    text: pendingOperations.has(key) ? "Pending…" : verb,
    title: reason || `${verb} ${definition.name}`,
  });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", async () => {
    if (pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const summary = definition.template_summary || definition.template?.title || definition.name;
    if (!window.confirm(`${verb} auto-task "${definition.name}" in workspace "${workspace?.name || workspace?.id}"?\n\nTarget: ${summary}\nThis writes the definition's enabled field.`)) return;
    pendingOperations.add(key);
    feedback("auto-task-operation-feedback", "pending", `${verb === "Enable" ? "Enabling" : "Disabling"} ${definition.name} (${summary})…`);
    renderAutoTasks(payload);
    try {
      const result = await postJson("/api/auto-tasks/toggle", {
        name: definition.name,
        expected_enabled: definition.enabled,
        enabled: nextEnabled,
      });
      feedback("auto-task-operation-feedback", "success", `${result.message}: ${definition.name}.`);
      await fetchAndRenderAutoTasks();
    } catch (error) {
      feedback("auto-task-operation-feedback", "error", `Auto-task change failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastAutoTasks) renderAutoTasks(lastAutoTasks);
    }
  });
  return button;
}

function autoTaskMintButton(payload, definition) {
  const key = `auto-task-mint:${definition.name}`;
  const reason = autoTaskControlReason(payload);
  const button = el("button", {
    class: "operation-button secondary",
    text: pendingOperations.has(key) ? "Pending…" : "Mint now",
    title: reason || "Mint one task now, ignoring schedule, enabled, and dedupe",
  });
  button.type = "button";
  button.disabled = Boolean(reason) || pendingOperations.has(key);
  button.addEventListener("click", async () => {
    if (pendingOperations.has(key)) return;
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
    pendingOperations.add(key);
    feedback("auto-task-operation-feedback", "pending", `Minting ${definition.name} now…`);
    renderAutoTasks(payload);
    try {
      const result = await postJson("/api/auto-tasks/mint", {
        name: definition.name,
        acknowledge_unconditional: true,
      });
      feedback("auto-task-operation-feedback", "success", `${result.message}`);
      await fetchAndRenderAutoTasks();
    } catch (error) {
      feedback("auto-task-operation-feedback", "error", `Manual mint failed: ${error.message}`);
    } finally {
      pendingOperations.delete(key);
      if (lastAutoTasks) renderAutoTasks(lastAutoTasks);
    }
  });
  return button;
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
    const actions = el("div", { class: "operation-card-actions" }, [
      autoTaskToggleButton(payload, definition),
      autoTaskMintButton(payload, definition),
    ]);
    const card = el("article", { class: "operation-card auto-task-card" });
    card.append(
      el("div", { class: "operation-card-title" }, [
        el("div", {}, [
          el("strong", { text: definition.name }),
          el("span", { class: `operation-state ${state}`, text: state }),
        ]),
        actions,
      ]),
      el("div", { class: "operation-target mono", text: definition.template_summary || definition.template?.title || "" }),
      el("div", { class: "operation-grid" }, [
        field("Schedule", definition.schedule_summary || "—"),
        field("Dedupe", definition.dedupe === "always" ? "always fire" : "skip if open"),
        field("Last evaluation / mint", lastEvaluationText(definition)),
        field("Last minted task", definition.last_minted_task_id
          ? `${definition.last_minted_task_id}${definition.last_minted_task_status ? ` · ${definition.last_minted_task_status}` : ""}`
          : "None"),
        field("Next evaluation", time(definition.next_evaluation)),
        field("Open duplicate", definition.open_duplicate ? "Yes — mint will create another" : "No"),
      ]),
    );
    const automation = renderAutomation(definition.automation);
    if (automation) card.appendChild(automation);
    if (definition.description) {
      card.appendChild(el("p", { class: "operation-description", text: definition.description }));
    }
    card.appendChild(el("p", {
      class: "operation-control-note operation-mint-warning",
      text: payload.unconditional_mint_warning || UNCONDITIONAL_MINT_WARNING,
    }));
    const controlReason = autoTaskControlReason(payload);
    if (controlReason) card.appendChild(el("p", { class: "operation-control-note", text: controlReason }));
    body.appendChild(card);
  }
  const count = $("auto-tasks-count");
  if (count) count.textContent = workspace && !workspaceReason ? `${definitions.length} · ${workspace.name}` : "read-only";
}

function fetchAndRenderAutoTasks() {
  return fetchJson("/api/auto-tasks").then(renderAutoTasks);
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
    renderAutoDrain({});
    return Promise.resolve();
  }
  const query = autoDrainConcurrency ? `?concurrency=${encodeURIComponent(autoDrainConcurrency)}` : "";
  return fetchJson(`/api/workflows/auto/readiness${query}`).then(renderAutoDrain);
}

export function fetchAndRenderOperations() {
  return Promise.all([
    fetchJson("/api/routines").then(renderOperations),
    fetchAndRenderAutoTasks().catch((error) => {
      feedback("auto-task-operation-feedback", "error", `Failed to load auto-tasks: ${error.message}`);
    }),
    fetchAndRenderAutoDrain().catch((error) => {
      feedback("auto-drain-operation-feedback", "error", `Failed to load auto-delivery readiness: ${error.message}`);
    }),
  ]);
}
