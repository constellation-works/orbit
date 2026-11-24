import { el, escCloseButton, postJson } from './common.js';
import { autoTaskDraft, autoTaskScheduleFields, autoTaskScheduleValue, labelled } from './schedule-editor.js';

function textField(label, name, { rows, mono = false, format } = {}) {
  return labelled(label, el(rows ? "textarea" : "input", {
    name,
    rows,
    class: mono ? "schedule-input mono" : "schedule-input",
    autocomplete: "off",
    placeholder: format,
    "aria-description": format,
  }));
}

function choice(label, name, options, initial) {
  const select = el("select", { name, class: "schedule-input" }, options.map(([value, text]) =>
    el("option", { value, text, selected: value === initial ? "" : null })));
  return labelled(label, select);
}

/** Open the create dialog for one workspace selection. */
export function openAutoTaskCreate({ selection, onCreated, onClose }) {
  const schedule = autoTaskDraft({ every_minutes: "" });
  const dialog = el("dialog", {
    id: "auto-task-create-dialog",
    class: "dialog-surface auto-task-create-dialog",
    "aria-labelledby": "auto-task-create-title",
  });
  const close = escCloseButton("Close auto-task creation (Esc)", { onClick: () => dialog.close() });
  const submit = el("button", { class: "operation-button primary", type: "submit", text: "Create" });
  const error = el("div", { class: "schedule-error", role: "alert", hidden: "" });
  const form = el("form", {}, [
    el("div", { class: "dialog-body" }, [
      textField("Name", "name", { mono: true }),
      textField("Description", "description", { rows: 2 }),
      ...autoTaskScheduleFields(schedule),
      textField("Task title", "title"),
      textField("Task body", "body", { rows: 3 }),
      labelled("Enabled", el("input", { type: "checkbox", name: "enabled" })),
      el("details", { class: "auto-task-create-options" }, [
        el("summary", { text: "Advanced options" }),
        el("div", { class: "auto-task-create-advanced" }, [
          textField("Criteria", "criteria", { rows: 3, format: "One per line" }),
          textField("Tags", "tags", { format: "Comma-separated" }),
          textField("Required tools", "required_tools", { mono: true, format: "Comma-separated" }),
          textField("Granted tools", "granted_tools", { mono: true, format: "Comma-separated" }),
          textField("Crew", "crew"),
          choice("Task type", "task_type", [
            ["chore", "Chore"], ["feature", "Feature"], ["bug", "Bug"], ["refactor", "Refactor"],
          ], "chore"),
          choice("Priority", "priority", [
            ["low", "Low"], ["medium", "Medium"], ["high", "High"], ["critical", "Critical"],
          ], "medium"),
          choice("Complexity", "complexity", [
            ["", "Unassessed"], ["low", "Low"], ["medium", "Medium"], ["hard", "Hard"], ["xhard", "Xhard"],
          ], ""),
          choice("Initial status", "status", [
            ["proposed", "Proposed"], ["backlog", "Backlog"], ["in_progress", "In progress"],
            ["review", "Review"], ["done", "Done"], ["blocked", "Blocked"], ["archived", "Archived"],
            ["rejected", "Rejected"], ["someday", "Someday"],
          ], "backlog"),
          choice("Open instance", "dedupe", [
            ["skip_if_open", "Skip mint"], ["always", "Mint again"],
          ], "skip_if_open"),
        ]),
      ]),
      error,
    ]),
    el("div", { class: "dialog-actions" }, [submit]),
  ]);
  dialog.append(el("div", { class: "dialog-header" }, [
    el("h2", { id: "auto-task-create-title", text: "Create auto-task" }),
    close,
  ]), form);

  const showError = (message) => {
    error.hidden = !message;
    error.textContent = message;
    if (message) error.scrollIntoView({ block: "nearest" });
  };
  form.addEventListener("input", () => showError(""));
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (submit.disabled || !selection.current()) return;
    const checked = autoTaskScheduleValue(schedule);
    if (checked.problem) {
      showError(checked.problem);
      return;
    }
    const data = new FormData(form);
    const list = (key, separator) => data.get(key).split(separator).map((value) => value.trim()).filter(Boolean);
    const body = {
      name: data.get("name"),
      description: data.get("description"),
      schedule: checked.value,
      enabled: data.has("enabled"),
      dedupe: data.get("dedupe"),
      granted_tools: list("granted_tools", ","),
      template: {
        title: data.get("title"),
        description: data.get("body"),
        acceptance_criteria: list("criteria", "\n"),
        tags: list("tags", ","),
        required_tools: list("required_tools", ","),
        task_type: data.get("task_type"),
        priority: data.get("priority"),
        complexity: data.get("complexity") || null,
        crew: data.get("crew").trim() || null,
        status: data.get("status"),
      },
    };
    showError("");
    const controls = dialog.querySelectorAll("button, input, select, textarea");
    for (const control of controls) control.disabled = true;
    form.setAttribute("aria-busy", "true");
    let result;
    try {
      result = await postJson("/api/auto-tasks/create", body);
    } catch (failure) {
      if (!selection.current()) return;
      for (const control of controls) control.disabled = false;
      form.removeAttribute("aria-busy");
      showError(failure.message);
      return;
    }
    if (!selection.current() || !dialog.isConnected) return;
    dialog.close();
    await onCreated(result);
  });
  dialog.addEventListener("cancel", (event) => {
    if (submit.disabled) event.preventDefault();
  });
  dialog.addEventListener("close", () => {
    dialog.remove();
    if (selection.current()) onClose();
  });
  document.body.append(dialog);
  dialog.showModal();
  form.elements.namedItem("name").focus();
}
