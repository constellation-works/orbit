// Auto-task operations [ORB-10876].

import { el, fetchJson, getWorkspace, postJson, requestPanel, statusPill } from './common.js';
import { renderAutomation } from './automation.js';
import { autoTaskDraft, autoTaskScheduleEditor, closeEditors, editingIn, editorObserved, isOpen, openEditor, scheduleEditButton } from './schedule-editor.js';

import {
  $,
  actionReason,
  clockHm,
  cronText,
  explainUnavailable,
  feedback,
  field,
  inactivePluginGroup,
  inactivePluginToggle,
  lastMintedText,
  nextEvaluationText,
  noteCronZone,
  operationCell,
  operationColumns,
  operationDetails,
  operationGroup,
  operationStat,
  operationSwitch,
  operationsState,
  pendingOperations,
  registerOperationsReset,
  relativeTime,
  runOperation,
  selectedWorkspace,
  selectedWorkspaceName,
  selectionSnapshot,
  setRailSubtabCount,
  taskLink,
  time,
  whenCell,
  withInactivePlugins,
  workspaceReadOnlyReason,
  workspaceSelectionReason
} from './operations-shared.js';
import { jobIdFromTarget } from './operations-routines.js';

const UNCONDITIONAL_MINT_WARNING = "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.";

// ---------------------------------------------------------------------------
// Auto-tasks
registerOperationsReset(() => closeEditors("auto-task"));


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

// Grouping, counts, row state and the switch follow the effective switch.
// The raw `enabled` flag stays the compare-and-set token the toggle posts:
// a legacy after-landing consumer can be on while that flag is still false.
function autoTaskOn(definition) {
  return definition.effective_enabled ?? Boolean(definition.enabled);
}

function autoTaskToggleButton(payload, definition) {
  const selection = selectionSnapshot();
  const on = autoTaskOn(definition);
  const nextEnabled = !on;
  const key = `auto-task-toggle:${selection.workspace}:${definition.name}`;
  const reason = actionReason(payload, "auto_task_toggle");
  const verb = nextEnabled ? "Enable" : "Disable";
  const pending = pendingOperations.has(key);
  const button = operationSwitch({
    name: `Auto-task: ${definition.name}`,
    on,
    text: pending ? "Pending…" : verb,
    pending,
    reason: reason || "",
  });
  if (!reason) button.title = `${verb} ${definition.name}`;
  button.addEventListener("click", () => {
    if (reason || !selection.current() || pendingOperations.has(key)) return;
    const workspace = selectedWorkspace();
    const summary = definition.template_summary || definition.template?.title || definition.name;
    if (!window.confirm(`${verb} auto-task "${definition.name}" in workspace "${workspace?.name || workspace?.id}"?\n\nTarget: ${summary}\nThis writes the definition's enabled field.`)) return;
    return runOperation({
      selection, key, feedbackId: "auto-task-operation-feedback",
      pending: `${verb} ${definition.name}…`, failure: "Auto-task change failed",
      render: () => { if (operationsState.lastAutoTasks) renderAutoTasks(operationsState.lastAutoTasks); },
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
      render: () => { if (operationsState.lastAutoTasks) renderAutoTasks(operationsState.lastAutoTasks); },
      request: () => postJson("/api/auto-tasks/mint", {
        name: definition.name, acknowledge_unconditional: true,
      }),
      refresh: fetchAndRenderAutoTasks,
      success: (result) => `${result.message}. Task created; no delivery was started.`,
    });
  });
  return explainUnavailable(button, reason);
}

function autoTaskTrigger(definition) {
  const schedule = definition.schedule || {};
  if (schedule.deliveries != null || schedule.deliveries_landed || /deliver/i.test(definition.schedule_summary || "")) return "delivery";
  return "schedule";
}

function autoTaskChip(text, tone = "") {
  return el("span", { class: `operation-chip mono ${tone}`.trim(), text });
}

function autoTaskEditKey(definition) {
  return `auto-task-schedule:${getWorkspace()}:${definition.name}`;
}

function autoTaskEditButton(payload, definition) {
  const key = autoTaskEditKey(definition);
  return scheduleEditButton({
    key,
    label: `${definition.name} schedule`,
    reason: definition.plugin_inactive ? "Seeded by a plugin that is off here" : actionReason(payload, "auto_task_schedule"),
    onOpen: () => {
      openEditor(key, "auto-task", autoTaskDraft(definition.schedule), { schedule: definition.schedule });
      renderAutoTasks(operationsState.lastAutoTasks || payload, { force: true });
    },
  });
}

function autoTaskEditor(definition) {
  const key = autoTaskEditKey(definition);
  if (!isOpen(key)) return null;
  const selection = selectionSnapshot();
  return autoTaskScheduleEditor({
    key,
    definition,
    cronPreview: cronText,
    save: (schedule) => {
      const observed = editorObserved(key);
      return postJson("/api/auto-tasks/schedule", {
        name: definition.name, expected_schedule: observed ? observed.schedule : definition.schedule, schedule,
      }).then((result) => {
        if (selection.current()) feedback("auto-task-operation-feedback", "success", `${result.message}: ${definition.name}.`);
        return result;
      });
    },
    onClose: (saved) => {
      if (saved) fetchAndRenderAutoTasks()?.catch?.(() => {});
      else if (operationsState.lastAutoTasks) renderAutoTasks(operationsState.lastAutoTasks, { force: true });
    },
  });
}

function autoTaskRow(payload, definition, workspaceId) {
  const on = autoTaskOn(definition);
  const state = on ? "enabled" : "disabled";
  const detailsKey = `auto-task:${definition.name}`;
  const template = definition.template || {};
  const duplicate = definition.open_duplicate ? "Yes — mint will create another" : "No";
  const schedulerId = definition.last_evaluation?.last_task_id;
  const mintSource = definition.last_minted_task_id && schedulerId && definition.last_minted_task_id !== schedulerId
    ? "manual mint"
    : schedulerId ? "scheduler" : "";
  const lastMinted = definition.last_minted_task_id
    ? [
      el("span", { class: "operation-cell-main operation-outcome" }, [
        taskLink(definition.last_minted_task_id, workspaceId),
        definition.last_minted_task_status ? statusPill(definition.last_minted_task_status) : null,
      ].filter(Boolean)),
      el("span", { class: `operation-cell-sub${definition.open_duplicate ? " warn" : ""}`, text: definition.open_duplicate
        ? "still open · scheduler will skip"
        : [mintSource, relativeTime(definition.last_evaluation?.last_fired_at)].filter(Boolean).join(" · "),
      title: definition.last_evaluation?.last_fired_at ? time(definition.last_evaluation.last_fired_at) : "" }),
    ]
    : [el("span", { class: "operation-cell-main muted", text: lastEvaluationText(definition) })];
  const next = definition.next_evaluation;
  const nextCell = next?.state === "scheduled" && next.at
    ? whenCell("Next mint", next.at)
    : operationCell("Next mint", [el("span", { class: `operation-cell-main${on ? "" : " muted"}`, text: nextEvaluationText(next) })]);
  const card = el("article", { class: `operation-card operation-row auto-task-card ${state}` });
  card.append(
    el("div", { class: "operation-row-head" }, [
      operationCell("", [autoTaskToggleButton(payload, definition)], "operation-cell-control"),
      operationCell("Definition", [
        el("div", { class: "operation-identity" }, [el("strong", { text: definition.name })]),
        el("span", { class: "operation-cell-sub operation-chips" }, [
          template.crew ? autoTaskChip(template.crew, "crew") : null,
          template.priority ? autoTaskChip(template.priority) : null,
        ].filter(Boolean)),
      ], "operation-cell-identity"),
      operationCell("Trigger", [
        el("span", { class: "operation-cell-main", text: definition.schedule?.cron ? cronText(definition.schedule.cron) : (definition.schedule_summary || "—") }),
        el("span", { class: "operation-cell-sub", text: [definition.schedule?.cron, definition.dedupe === "always" ? "always fire" : "skip if open"].filter(Boolean).join(" · ") }),
        autoTaskEditButton(payload, definition),
      ]),
      nextCell,
      operationCell("Last minted", lastMinted),
      operationCell("", [autoTaskMintButton(payload, definition)], "operation-cell-action"),
    ]),
    operationDetails(detailsKey, [
      el("div", { class: "operation-target mono", text: definition.template_summary || template.title || "" }),
      el("div", { class: "operation-grid" }, [
        field("Schedule", definition.schedule_summary || "—"),
        field("Dedupe", definition.dedupe === "always" ? "always fire" : "skip if open"),
        field("Last scheduler evaluation", lastEvaluationText(definition)),
        field("Last minted task", lastMintedText(definition)),
        field("Next evaluation", nextEvaluationText(definition.next_evaluation)),
        field("Open duplicate", duplicate),
        field("Template", [template.crew && `crew ${template.crew}`, template.priority && `priority ${template.priority}`, template.complexity && `complexity ${template.complexity}`, `→ ${template.status || "backlog"}`].filter(Boolean).join(" · ")),
      ]),
      renderAutomation(definition.automation, `${detailsKey}:automation`, { kind: "auto-task", name: definition.name, workspace: getWorkspace() }),
      definition.description ? el("p", { class: "operation-description", text: definition.description }) : null,
      el("p", {
        class: "operation-control-note operation-mint-warning",
        text: payload.unconditional_mint_warning || UNCONDITIONAL_MINT_WARNING,
      }),
    ]),
  );
  const editor = autoTaskEditor(definition);
  if (editor) card.insertBefore(editor, card.lastChild);
  return card;
}

// Where the workspace defines an auto_task_scheduler routine, a paused one
// silently stops every scheduled definition, so the pane says so instead of
// leaving "next mint" looking live.
export function syncAutoTaskSchedulerNote() {
  const note = $("auto-tasks-scheduler-note");
  if (!note) return;
  const workspace = selectedWorkspaceName();
  const scheduler = workspace && operationsState.lastOperations
    ? (operationsState.lastOperations.routines || []).find((routine) => routine.source === workspace && jobIdFromTarget(routine.target) === "auto_task_scheduler_pipeline")
    : null;
  note.textContent = "";
  note.className = "operation-control-note auto-tasks-scheduler-note";
  note.title = "Definitions are stored in .orbit/auto_tasks/ and evaluated on the host sweep clock. Toggle enables or disables a definition; Mint now bypasses its schedule, enabled flag and duplicate checks.";
  if (!scheduler) {
    note.appendChild(el("span", { text: "Enabled definitions create tasks automatically when their trigger is due." }));
    return;
  }
  const link = el("a", { class: "operation-run-link operation-routine-link operation-link", text: scheduler.name });
  link.href = "#operations/routines";
  if (!scheduler.enabled) {
    note.className += " warn";
    note.appendChild(el("span", { text: "The " }));
    note.append(link, el("span", { text: " routine is paused, so scheduled tasks will wait until it resumes." }));
    return;
  }
  note.appendChild(el("span", { text: "Scheduled tasks are created by " }));
  note.append(link, el("span", { text: ` (${cronText(scheduler.cron)}).` }));
}

export function renderAutoTasks(payload, { force = false } = {}) {
  noteCronZone(payload);
  operationsState.lastAutoTasks = payload;
  const body = $("auto-tasks-body");
  if (!body) return;
  // An open schedule editor holds a draft: leave the rows alone until it closes.
  if (!force && editingIn("auto-task") && body.querySelector(".schedule-editor")) return;
  body.textContent = "";
  const workspace = selectedWorkspace();
  const readable = Boolean(workspace) && !workspaceSelectionReason();
  const reason = workspaceReadOnlyReason() || payload.read_only_reason || "";
  const listed = readable ? (payload.definitions || []) : [];
  const definitions = listed.filter((definition) => !definition.plugin_inactive);
  const inactive = listed.filter((definition) => definition.plugin_inactive);
  if (reason) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: reason }));
  }
  if (definitions.length === 0 && inactive.length === 0) {
    body.appendChild(el("div", {
      class: "empty-state",
      text: readable ? "No auto-task definitions are defined by this workspace." : "Select a workspace to list its auto-task definitions.",
    }));
  } else if (definitions.length) {
    const enabled = definitions.filter(autoTaskOn);
    const duplicates = definitions.filter((definition) => definition.open_duplicate).length;
    const nextMint = enabled
      .map((definition) => definition.next_evaluation?.state === "scheduled" ? new Date(definition.next_evaluation.at || NaN).getTime() : NaN)
      .filter(Number.isFinite)
      .sort((a, b) => a - b)[0];
    body.appendChild(el("div", { class: "operation-summary auto-tasks-summary" }, [
      operationStat("", "Definitions", definitions.length),
      operationStat("active", "Enabled", enabled.length),
      operationStat("accent", "Next mint", nextMint ? relativeTime(nextMint) : "—", nextMint ? clockHm(nextMint) : "no scheduled definition"),
      operationStat(duplicates ? "warn" : "", "Open duplicates", duplicates, duplicates ? "scheduler skips these slots" : null),
    ]));
    const schedulerNote = el("p", { class: "operation-control-note auto-tasks-scheduler-note" });
    schedulerNote.id = "auto-tasks-scheduler-note";
    body.appendChild(schedulerNote);
    const groups = [
      ["active", "On a schedule", "minted by the scheduler when due and no open instance exists", enabled.filter((definition) => autoTaskTrigger(definition) === "schedule")],
      ["accent", "On delivery", "minted after landed deliveries, not on a clock", enabled.filter((definition) => autoTaskTrigger(definition) === "delivery")],
      ["paused", "Disabled", "kept in the repo, never evaluated", definitions.filter((definition) => !autoTaskOn(definition))],
    ];
    const columns = ["", "Definition", "Trigger", "Next mint", "Last minted", ""];
    for (const [tone, title, hint, members] of groups) {
      if (!members.length) continue;
      const group = operationGroup(tone, title, members.length, hint);
      group.appendChild(operationColumns(columns));
      for (const definition of members) group.appendChild(autoTaskRow(payload, definition, workspace.id));
      body.appendChild(group);
    }
  }
  if (inactive.length) {
    body.appendChild(inactivePluginGroup(inactive.map((definition) => ({ name: definition.name, detail: definition.schedule_summary, reason: definition.skipped_reason })), "minted"));
  }
  if (readable) {
    const toggle = inactivePluginToggle(payload.inactive_plugin_count || 0);
    if (toggle) body.appendChild(toggle);
  }
  syncAutoTaskSchedulerNote();
  const count = $("auto-tasks-count");
  const enabledCount = definitions.filter(autoTaskOn).length;
  if (count) count.textContent = readable ? `${enabledCount} enabled of ${definitions.length} · ${workspace.name}` : "read-only";
  setRailSubtabCount("rail-count-ops-auto-tasks", readable && definitions.length ? `${enabledCount}/${definitions.length}` : "");
}

function operationCountId(bodyId) {
  return bodyId === "clock-body" ? "clock-host" : bodyId.replace("-body", "-count");
}

function loadOperationPanel(bodyId, path, render) {
  return requestPanel(bodyId, path, () => fetchJson(path), render, operationCountId(bodyId));
}

export function fetchAndRenderAutoTasks() {
  return loadOperationPanel("auto-tasks-body", withInactivePlugins("/api/auto-tasks"), renderAutoTasks);
}
