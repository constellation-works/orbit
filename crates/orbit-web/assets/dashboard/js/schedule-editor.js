// Schedule and trigger editors for routines and auto-tasks.
//
// Only when a definition fires is editable here; every other field stays
// read-only. The form checks the shape it can see before saving, and the
// server, which runs the same validation as `orbit.routine.control` and
// `orbit.auto_task.update`, has the last word: its refusal text is shown in
// the form verbatim and the draft is kept.
//
// An open editor holds a draft the 30 s refresh must not discard, so the
// owning panel asks `editingIn(scope)` and leaves its rows alone while one is
// open. The editor re-renders only itself.

import { el } from './common.js';

const editors = new Map();

const ROUTINE_KIND_LABELS = { cron: "Cron schedule", state: "Task state", deliveries_landed: "Deliveries landed" };
const AUTO_TASK_KIND_LABELS = { cron: "Cron schedule", every_minutes: "Fixed interval", deliveries_landed: "Deliveries landed" };
const STATE_KIND_LABELS = {
  preparation_eligible: "Task ready for preparation",
  finding_unverified: "Review finding unverified",
  pilot_warnings_held: "Pilot warnings held promotion",
  block_escalated: "Blocked task escalated",
  decision_requested: "Decision requested",
  execution_failed: "Execution failed (retired)",
};
const DELIVERY_COVERAGE = "landed_code_review_v1";

/** Whether an editor of `scope` ("routine" or "auto-task") is open. */
export function editingIn(scope) {
  for (const editor of editors.values()) if (editor.scope === scope) return true;
  return false;
}

/** Drop every open editor of `scope`; the next render shows plain rows. */
export function closeEditors(scope) {
  for (const [key, editor] of editors) if (editor.scope === scope) editors.delete(key);
}

/** The row's `edit` affordance, styled like the dashboard's other field editors. */
export function scheduleEditButton({ key, label, reason, onOpen }) {
  const button = el("button", { class: "field-edit schedule-edit", text: "edit" });
  button.type = "button";
  button.disabled = Boolean(reason) || editors.has(key);
  button.title = reason || `Edit ${label}`;
  button.setAttribute("aria-label", `Edit ${label}`);
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    if (!button.disabled) onOpen();
  });
  return button;
}

export function isOpen(key) {
  return editors.has(key);
}

/** Open an editor for `key`, seeding its draft from the observed value. */
export function openEditor(key, scope, draft, observed) {
  const snapshot = observed === undefined ? undefined : JSON.parse(JSON.stringify(observed));
  editors.set(key, { scope, draft, error: "", pending: false, observed: snapshot });
}

/** The value observed when the editor for `key` opened, for its whole lifetime. */
export function editorObserved(key) {
  const observed = editors.get(key)?.observed;
  return observed === undefined ? undefined : JSON.parse(JSON.stringify(observed));
}

function closeEditor(key) {
  editors.delete(key);
}

// ---------------------------------------------------------------------------
// Validation: the shape checks the form can make without the server.

const CRON_FIELD = /^[0-9A-Za-z*,/\-?#LW]+$/;

function cronProblem(cron) {
  const fields = String(cron || "").trim().split(/\s+/).filter(Boolean);
  if (fields.length !== 5 || !fields.every((part) => CRON_FIELD.test(part))) {
    return "Cron needs five fields: minute hour day-of-month month day-of-week.";
  }
  return "";
}

function wholeNumber(value, { min = 0, max = Number.MAX_SAFE_INTEGER, optional = false } = {}) {
  const text = String(value ?? "").trim();
  if (text === "") return optional ? { ok: true, value: null } : { ok: false };
  if (!/^\d+$/.test(text)) return { ok: false };
  const number = Number(text);
  return number >= min && number <= max ? { ok: true, value: number } : { ok: false };
}

function branchProblem(branch) {
  const text = String(branch || "");
  if (!text || text.startsWith("-") || /\s/.test(text)) return "Branch is required, without spaces or a leading '-'.";
  return "";
}

// Each numeric field: [key, label, rule, hint].
const STATE_NUMBERS = [
  ["debounce_minutes", "Debounce (min)", { min: 1 }, "quiet period before a member is due"],
  ["max_wait_minutes", "Max wait (min)", { min: 1 }, "at least the debounce"],
  ["max_items", "Max members", { min: 1, max: 50 }, "1–50"],
  ["batch_size", "Batch size", { min: 1, max: 50, optional: true }, "blank for the default"],
  ["retries", "Retries", { min: 0, max: 5 }, "0–5"],
  ["deadline_minutes", "Deadline (min)", { min: 1, max: 1440 }, "1–1440"],
];
const DELIVERY_NUMBERS = [
  ["threshold", "Threshold", { min: 1, max: 50 }, "deliveries per batch"],
  ["max_wait_minutes", "Max wait (min)", { min: 1 }, "fire a partial batch after this"],
  ["max_items", "Max items", { min: 1, max: 50 }, "at least the threshold"],
  ["retries", "Retries", { min: 0, max: 5 }, "0–5"],
];

function numbersProblem(values, spec) {
  for (const [key, label, rule] of spec) {
    if (!wholeNumber(values[key], rule).ok) return `${label} must be a whole number${rule.max != null && rule.max < Number.MAX_SAFE_INTEGER ? ` from ${rule.min} to ${rule.max}` : ` of at least ${rule.min}`}.`;
  }
  return "";
}

function numbers(values, spec) {
  const out = {};
  for (const [key, , rule] of spec) {
    const parsed = wholeNumber(values[key], rule);
    if (parsed.value != null) out[key] = parsed.value;
  }
  return out;
}

// ---------------------------------------------------------------------------
// Drafts: flat string fields per kind, so switching kinds keeps what was typed.

function deliveryDraft(delivery = {}) {
  return {
    owner_machine: delivery.owner_machine || "",
    branch: delivery.branch || "",
    threshold: delivery.threshold ?? "",
    max_wait_minutes: delivery.max_wait_minutes ?? "",
    max_items: delivery.max_items ?? "",
    retries: delivery.retries ?? 0,
    coverage: delivery.coverage || DELIVERY_COVERAGE,
  };
}

/** A routine's trigger as an editor draft. */
export function routineDraft(trigger = {}) {
  const state = trigger.state || {};
  return {
    kind: trigger.state ? "state" : trigger.deliveries_landed ? "deliveries_landed" : "cron",
    cron: trigger.cron || "",
    missed_run: trigger.missed_run || "skip",
    state: {
      kind: state.kind || "",
      owner_machine: state.owner_machine || "",
      branch: state.branch || "",
      debounce_minutes: state.debounce_minutes ?? "",
      max_wait_minutes: state.max_wait_minutes ?? "",
      max_items: state.max_items ?? "",
      batch_size: state.batch_size ?? "",
      retries: state.retries ?? 0,
      deadline_minutes: state.deadline_minutes ?? "",
    },
    // Eligibility and freshness are kind-specific settings this form does not
    // edit; they are carried over while the state kind is unchanged.
    stateExtras: trigger.state ? { kind: state.kind, eligibility: state.eligibility, freshness: state.freshness } : null,
    delivery: deliveryDraft(trigger.deliveries_landed),
  };
}

/** An auto-task's schedule as an editor draft. */
export function autoTaskDraft(schedule = {}) {
  return {
    kind: schedule.deliveries_landed ? "deliveries_landed" : schedule.every_minutes != null ? "every_minutes" : "cron",
    cron: schedule.cron || "",
    every_minutes: schedule.every_minutes ?? "",
    delivery: deliveryDraft(schedule.deliveries_landed),
  };
}

function deliveryValue(draft) {
  const problem = branchProblem(draft.branch) || numbersProblem(draft, DELIVERY_NUMBERS);
  if (problem) return { problem };
  const value = { branch: draft.branch, coverage: draft.coverage || DELIVERY_COVERAGE, ...numbers(draft, DELIVERY_NUMBERS) };
  if (value.threshold > value.max_items) return { problem: "Threshold must not exceed max items." };
  if (draft.owner_machine.trim()) value.owner_machine = draft.owner_machine.trim();
  return { value };
}

/** The trigger a routine draft describes, or the first problem with it. */
export function routineTriggerValue(draft) {
  if (draft.kind === "cron") {
    const problem = cronProblem(draft.cron);
    return problem ? { problem } : { value: { cron: draft.cron.trim().split(/\s+/).join(" "), missed_run: draft.missed_run } };
  }
  if (draft.kind === "deliveries_landed") {
    const delivery = deliveryValue(draft.delivery);
    return delivery.problem ? delivery : { value: { deliveries_landed: delivery.value, missed_run: draft.missed_run } };
  }
  const state = draft.state;
  if (!state.kind) return { problem: "Choose what task state fires this routine." };
  if (!state.owner_machine.trim()) return { problem: "Owner machine is required for a state trigger." };
  const problem = branchProblem(state.branch) || numbersProblem(state, STATE_NUMBERS);
  if (problem) return { problem };
  const value = { kind: state.kind, owner_machine: state.owner_machine.trim(), branch: state.branch, ...numbers(state, STATE_NUMBERS) };
  if (value.max_wait_minutes < value.debounce_minutes) return { problem: "Max wait must be at least the debounce." };
  const extras = draft.stateExtras;
  if (extras && extras.kind === state.kind) {
    if (extras.eligibility) value.eligibility = extras.eligibility;
    if (extras.freshness && Object.keys(extras.freshness).length) value.freshness = extras.freshness;
  }
  return { value: { state: value, missed_run: draft.missed_run } };
}

/** The schedule an auto-task draft describes, or the first problem with it. */
export function autoTaskScheduleValue(draft) {
  if (draft.kind === "cron") {
    const problem = cronProblem(draft.cron);
    return problem ? { problem } : { value: { cron: draft.cron.trim().split(/\s+/).join(" ") } };
  }
  if (draft.kind === "every_minutes") {
    const minutes = wholeNumber(draft.every_minutes, { min: 1 });
    return minutes.ok ? { value: { every_minutes: minutes.value } } : { problem: "Interval must be a whole number of minutes, 1 or more." };
  }
  const delivery = deliveryValue(draft.delivery);
  return delivery.problem ? delivery : { value: { deliveries_landed: delivery.value } };
}

// ---------------------------------------------------------------------------
// Form furniture.

function labelled(label, control, hint = "") {
  const wrap = el("label", { class: "schedule-field" }, [
    el("span", { class: "schedule-field-label", text: label }),
    control,
  ]);
  if (hint) wrap.appendChild(el("span", { class: "schedule-field-hint", text: hint }));
  return wrap;
}

function textInput(target, key, { placeholder = "", mono = true, onInput } = {}) {
  const input = el("input", { class: `schedule-input${mono ? " mono" : ""}` });
  input.type = "text";
  input.value = String(target[key] ?? "");
  input.placeholder = placeholder;
  input.spellcheck = false;
  input.autocomplete = "off";
  input.addEventListener("input", () => {
    target[key] = input.value;
    onInput?.();
  });
  return input;
}

function numberInput(target, key, rule, onInput) {
  const input = textInput(target, key, { onInput });
  input.inputMode = "numeric";
  input.placeholder = rule.optional ? "default" : "";
  return input;
}

function selectInput(options, value, onChange) {
  const select = el("select", { class: "schedule-input" });
  for (const [optionValue, text] of options) {
    const option = el("option", { text });
    option.value = optionValue;
    option.selected = optionValue === value;
    select.appendChild(option);
  }
  select.addEventListener("change", () => onChange(select.value));
  return select;
}

function numberFields(target, spec, onInput) {
  return el("div", { class: "schedule-grid" }, spec.map(([key, label, rule, hint]) => labelled(label, numberInput(target, key, rule, onInput), hint)));
}

function deliveryFields(delivery, onInput) {
  return [
    el("div", { class: "schedule-grid" }, [
      labelled("Branch", textInput(delivery, "branch", { placeholder: "agent-main", onInput })),
      labelled("Owner machine", textInput(delivery, "owner_machine", { placeholder: "registered owner", onInput }), "blank: the workspace's registered owner"),
    ]),
    numberFields(delivery, DELIVERY_NUMBERS, onInput),
    el("p", { class: "schedule-note", text: `Coverage: ${delivery.coverage || DELIVERY_COVERAGE}` }),
  ];
}

function missedRunField(draft, changed) {
  return labelled("Missed slots", selectInput(
    [["skip", "Skip — wait for the next slot"], ["catch_up_once", "Catch up once"]],
    draft.missed_run,
    (value) => { draft.missed_run = value; changed(); },
  ), "slots that fell while the host was off");
}

// ---------------------------------------------------------------------------
// The editor.

/**
 * Build the open editor for `key`.
 *
 * `kinds` is [[value, label], …]; `fields(draft, refresh)` returns the kind's
 * controls; `value(draft)` returns `{ value }` or `{ problem }`; `save(value)`
 * posts it and resolves with the server's reply; `onClose(saved)` hands the
 * panel back.
 */
function buildEditor({ key, title, note, kinds, fields, value, preview, save, onClose }) {
  const editor = editors.get(key);
  // What the definition holds now, in the form's own shape: Save stays off
  // until the draft differs from it.
  editor.initial ??= JSON.stringify(value(editor.draft).value ?? null);
  const root = el("div", { class: "schedule-editor" });
  root.dataset.editing = "schedule";
  const render = () => {
    const draft = editor.draft;
    const status = el("div", { class: "schedule-preview" });
    const error = el("div", { class: "schedule-error" });
    error.setAttribute("role", "alert");
    const saveButton = el("button", { class: "operation-button", text: editor.pending ? "Saving…" : "Save" });
    saveButton.type = "button";
    const cancelButton = el("button", { class: "operation-button secondary", text: "Cancel" });
    cancelButton.type = "button";
    const sync = () => {
      const checked = value(draft);
      const shown = editor.error || checked.problem || "";
      error.textContent = shown;
      error.hidden = !shown;
      // A refused value's preview would read as accepted; show the refusal alone.
      status.textContent = shown ? "" : preview(checked.value);
      status.hidden = !status.textContent;
      const unchanged = !checked.problem && JSON.stringify(checked.value) === editor.initial;
      saveButton.disabled = editor.pending || Boolean(checked.problem) || unchanged;
      saveButton.title = unchanged ? "No change to save" : "";
      cancelButton.disabled = editor.pending;
    };
    const changed = () => {
      // An edit after a refusal supersedes it; the new draft is checked afresh.
      editor.error = "";
      sync();
    };
    const kindSelect = selectInput(kinds, draft.kind, (kind) => {
      draft.kind = kind;
      editor.error = "";
      render();
    });
    kindSelect.disabled = kinds.length < 2 || editor.pending;
    saveButton.addEventListener("click", async () => {
      const checked = value(draft);
      if (checked.problem || editor.pending || JSON.stringify(checked.value) === editor.initial) return;
      editor.pending = true;
      editor.error = "";
      render();
      try {
        await save(checked.value);
        closeEditor(key);
        onClose(true);
      } catch (failure) {
        if (!editors.has(key)) return;
        editor.pending = false;
        editor.error = failure.message || String(failure);
        render();
      }
    });
    cancelButton.addEventListener("click", () => {
      closeEditor(key);
      onClose(false);
    });
    root.replaceChildren(...[
      el("div", { class: "schedule-editor-head" }, [el("strong", { text: title })]),
      labelled("Trigger", kindSelect),
      ...fields(draft, changed),
      note ? el("p", { class: "schedule-note", text: note }) : null,
      status,
      error,
      el("div", { class: "schedule-editor-controls" }, [saveButton, cancelButton]),
    ].filter(Boolean));
    for (const control of root.querySelectorAll("input, select")) {
      if (control !== kindSelect) control.disabled = editor.pending;
    }
    sync();
  };
  render();
  root.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !editor.pending) {
      closeEditor(key);
      onClose(false);
    }
  });
  return root;
}

/** The open trigger editor for one routine. */
export function routineTriggerEditor({ key, routine, cronPreview, save, onClose }) {
  const options = routine.trigger_options || { kinds: ["cron"], state_kinds: [] };
  const kinds = options.kinds.map((kind) => [kind, ROUTINE_KIND_LABELS[kind] || kind]);
  const stateKinds = options.state_kinds.map((kind) => [kind, STATE_KIND_LABELS[kind] || kind]);
  return buildEditor({
    key,
    title: `Trigger · ${routine.name}`,
    note: routine.trigger_options?.seeded
      ? "Orbit seeded this routine. Once edited, `orbit workspace sync` keeps your file and stops refreshing it from Orbit's template."
      : "",
    kinds,
    fields: (draft, changed) => {
      if (draft.kind === "cron") {
        return [
          el("div", { class: "schedule-grid" }, [
            labelled("Cron", textInput(draft, "cron", { placeholder: "m h dom mon dow", onInput: changed }), "host-local time"),
            missedRunField(draft, changed),
          ]),
        ];
      }
      if (draft.kind === "deliveries_landed") return deliveryFields(draft.delivery, changed);
      if (!draft.state.kind && stateKinds.length) draft.state.kind = stateKinds[0][0];
      return [
        el("div", { class: "schedule-grid" }, [
          labelled("Fires on", selectInput(stateKinds, draft.state.kind, (kind) => { draft.state.kind = kind; changed(); }), "limited to this routine's target job"),
          labelled("Owner machine", textInput(draft.state, "owner_machine", { onInput: changed })),
          labelled("Branch", textInput(draft.state, "branch", { placeholder: "agent-main", onInput: changed })),
        ]),
        numberFields(draft.state, STATE_NUMBERS, changed),
      ];
    },
    value: routineTriggerValue,
    preview: (trigger) => (trigger.cron ? `Fires ${cronPreview(trigger.cron)}` : ""),
    save,
    onClose,
  });
}

/** The open schedule editor for one auto-task. */
export function autoTaskScheduleEditor({ key, definition, cronPreview, save, onClose }) {
  const kinds = Object.entries(AUTO_TASK_KIND_LABELS);
  return buildEditor({
    key,
    title: `Schedule · ${definition.name}`,
    note: "",
    kinds,
    fields: (draft, changed) => {
      if (draft.kind === "cron") {
        return [labelled("Cron", textInput(draft, "cron", { placeholder: "m h dom mon dow", onInput: changed }), "host-local time")];
      }
      if (draft.kind === "every_minutes") {
        return [labelled("Every (minutes)", numberInput(draft, "every_minutes", { min: 1 }, changed), "anchored at the definition's first-observed slot")];
      }
      return deliveryFields(draft.delivery, changed);
    },
    value: autoTaskScheduleValue,
    preview: (schedule) => (schedule.cron ? `Mints ${cronPreview(schedule.cron)}` : schedule.every_minutes ? `Mints every ${schedule.every_minutes} minute${schedule.every_minutes === 1 ? "" : "s"}` : ""),
    save,
    onClose,
  });
}
