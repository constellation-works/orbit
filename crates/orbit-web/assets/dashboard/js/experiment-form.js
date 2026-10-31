// Orbit dashboard Analytics › Experiments authoring: the New experiment form,
// the same form editing a draft, and a draft's Start action. Saving posts to
// /api/diagnostics/experiments (create) or patches
// /api/diagnostics/experiments/{id} (edit); starting posts to
// /api/diagnostics/experiments/{id}/start. Each route runs core's own create,
// update and start, so the form adds no rule of its own except one: it
// collects the definition, sends it and shows core's refusal next to the
// field the message names. The exception is clearing an optional draft
// field (`primary_metric`, `min_sample`, `max_days`) the stored draft
// already sets: the API treats an omitted field as unchanged, so the form
// refuses the clear next to its field instead of closing as a saved edit.
// "Save and start" is two writes, so a refused start leaves the saved
// draft open in the form and says so.
//
// The form lives in its own host above the polled panel body. An unchanged
// authoring refresh keeps its nodes; changed choices rebuild from typed state.
// Draft actions keep their state per experiment id across polls, too.
// Every node is built with DOM calls, never markup strings.

import { el, captureFocus, captureWorkspaceVisit, onWorkspaceChange, requestJson } from './common.js';

const ENDPOINT = "/api/diagnostics/experiments";
const HOST = "experiments-form";
const OPEN = "experiments-new";
const ROLES = ["implement", "review", "recovery"];
const SURFACES = [
  ["crew", "Crew pool"],
  ["instructions", "Instructions"],
  ["tools", "Tools"],
  ["skills", "Skills"],
  ["config", "Config values"],
];
const POPULATION_LISTS = [
  ["task_type", "Task type"],
  ["tags_include", "Tags to include"],
  ["tags_exclude", "Tags to exclude"],
  ["origin", "Origin"],
];

let form = null;
let authoring = null;
let refresh = () => Promise.resolve();
let renderDraftStartState = () => {};
let nextKey = 1;
let bound = false;
// Draft Start confirmations by experiment id, so a poll that rebuilds a card
// keeps an open confirmation and its typed reason.
const starts = new Map();

const $ = (id) => document.getElementById(id);

onWorkspaceChange(() => {
  syncExperimentAuthoring(null);
  starts.clear();
});

/// Record what the latest list or detail payload says this session may do,
/// and show the New experiment button only when create is authorized.
/// `onSaved` refreshes the page after a write.
export function syncExperimentAuthoring(next, onSaved, onDraftStartStateChange) {
  const changed = JSON.stringify(authoring) !== JSON.stringify(next || null);
  authoring = next || null;
  if (onSaved) refresh = onSaved;
  if (onDraftStartStateChange) renderDraftStartState = onDraftStartStateChange;
  const button = $(OPEN);
  if (button && !bound) {
    bound = true;
    button.addEventListener("click", () => openExperimentForm(null));
  }
  const allowed = Boolean(authoring?.create?.authorized);
  if (button) button.hidden = !allowed;
  if (!authoring) closeExperimentForm();
  else if (changed && form) {
    // Changed permissions or choices require a fresh confirmation, but keep
    // the definition the operator typed. In-flight writes retain their fences.
    form.confirming = false;
    renderForm();
  }
}

/// The draft Start state included in the experiments panel's render hash.
export function draftStartRenderState() {
  return [...starts.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([id, state]) => [id, state.reason, state.pending, state.status]);
}

export function closeExperimentForm() {
  form = null;
  const host = $(HOST);
  if (host) {
    host.hidden = true;
    host.replaceChildren();
  }
  $(OPEN)?.setAttribute("aria-expanded", "false");
}

/// Open the form: a new draft when `experiment` is null, else that draft.
export function openExperimentForm(experiment) {
  if (!(experiment ? authoring?.update : authoring?.create)?.authorized) return;
  form = experiment ? editState(experiment) : createState();
  const host = $(HOST);
  if (!host) return;
  host.hidden = false;
  $(OPEN)?.setAttribute("aria-expanded", experiment ? "false" : "true");
  renderForm();
  host.querySelector("input")?.focus();
  host.scrollIntoView?.({ block: "start" });
}

// ---- state ------------------------------------------------------------------

function key() {
  nextKey += 1;
  return nextKey;
}

function blankArm(name, control) {
  const instructions = {};
  for (const role of ROLES) instructions[role] = { choice: "default", path: "" };
  return {
    key: key(),
    name,
    weight: "1",
    control,
    crew: [],
    instructions,
    tools: { add: "", remove: "", roles: ["implement"] },
    skills: { rows: [], roles: ["implement"] },
    config: { rows: [], roles: ["implement"] },
    open: {},
  };
}

function createState() {
  return {
    mode: "create",
    id: null,
    original: null,
    fields: {
      name: "", hypothesis: "", complexity: [], task_type: "", tags_include: "", tags_exclude: "", origin: "",
      primary_metric: "", min_sample: "", max_days: "", mode: "shadow", reason: "",
    },
    arms: [blankArm("control", true), blankArm("treatment", false)],
    guardrails: [],
    errors: {},
    status: "",
    pending: false,
    confirming: false,
  };
}

function text(value) {
  return value == null ? "" : String(value);
}

function armFromView(arm) {
  const overlay = arm.overlay && typeof arm.overlay === "object" ? arm.overlay : {};
  const state = blankArm(text(arm.name), Boolean(arm.control));
  state.weight = text(arm.weight ?? 1);
  state.crew = (overlay.crew?.pool || []).map((member) => ({ key: key(), crew: text(member.crew), weight: text(member.weight ?? 1) }));
  for (const role of ROLES) {
    const value = overlay.instructions?.[role];
    if (typeof value === "string") state.instructions[role] = { choice: "file", path: value };
    else if (value === false) state.instructions[role] = { choice: "suppress", path: "" };
  }
  if (overlay.tools) {
    state.tools = {
      add: (overlay.tools.add || []).join(", "),
      remove: (overlay.tools.remove || []).join(", "),
      roles: [...(overlay.tools.roles || ["implement"])],
    };
  }
  if (overlay.skills) {
    state.skills = {
      rows: Object.entries(overlay.skills.replace || {}).map(([skill, path]) => ({ key: key(), skill, path: text(path) })),
      roles: [...(overlay.skills.roles || ["implement"])],
    };
  }
  if (overlay.config) {
    state.config = {
      rows: Object.entries(overlay.config.values || {}).map(([name, value]) => ({
        key: key(), name, value: typeof value === "string" ? value : JSON.stringify(value),
      })),
      roles: [...(overlay.config.roles || ["implement"])],
    };
  }
  for (const [surface] of SURFACES) state.open[surface] = surfaceSet(state, surface);
  return state;
}

function editState(experiment) {
  const population = experiment.population || {};
  const joined = (name) => (population[name] || []).join(", ");
  return {
    mode: "edit",
    id: experiment.id,
    original: experiment,
    fields: {
      name: text(experiment.name),
      hypothesis: text(experiment.hypothesis),
      complexity: [...(population.complexity || [])],
      task_type: joined("task_type"),
      tags_include: joined("tags_include"),
      tags_exclude: joined("tags_exclude"),
      origin: joined("origin"),
      primary_metric: text(experiment.primary_metric),
      min_sample: text(experiment.min_sample),
      max_days: text(experiment.max_days),
      mode: experiment.mode || "shadow",
      reason: "",
    },
    arms: (experiment.arms || []).filter((arm) => arm.retired_segment == null).map(armFromView),
    guardrails: Object.entries(experiment.guardrails || {}).map(([metric, threshold]) => ({ key: key(), metric, threshold: text(threshold) })),
    errors: {},
    status: "",
    pending: false,
    confirming: false,
  };
}

function surfaceSet(arm, surface) {
  switch (surface) {
    case "crew": return arm.crew.length > 0;
    case "instructions": return ROLES.some((role) => arm.instructions[role].choice !== "default");
    case "tools": return Boolean(arm.tools.add.trim() || arm.tools.remove.trim());
    case "skills": return arm.skills.rows.length > 0;
    default: return arm.config.rows.length > 0;
  }
}

// ---- request body -------------------------------------------------------------

function list(value) {
  return String(value || "").split(",").map((item) => item.trim()).filter(Boolean);
}

// A whole number is sent as one; anything else is sent as typed, so core's
// parser, not the form, refuses it.
function count(raw) {
  const value = String(raw).trim();
  return /^-?\d+$/.test(value) ? Number(value) : value;
}

// A config value is JSON when it parses (numbers, booleans, lists) and a
// string otherwise.
function configValue(raw) {
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

function armBody(arm) {
  const overlay = {};
  if (arm.crew.length) {
    overlay.crew = { pool: arm.crew.map((member) => ({ crew: member.crew, weight: count(member.weight) })) };
  }
  const instructions = {};
  for (const role of ROLES) {
    const entry = arm.instructions[role];
    if (entry.choice === "file") instructions[role] = entry.path;
    else if (entry.choice === "suppress") instructions[role] = false;
  }
  if (Object.keys(instructions).length) overlay.instructions = instructions;
  const add = list(arm.tools.add);
  const remove = list(arm.tools.remove);
  if (add.length || remove.length) overlay.tools = { add, remove, roles: [...arm.tools.roles] };
  if (arm.skills.rows.length) {
    overlay.skills = {
      replace: Object.fromEntries(arm.skills.rows.map((row) => [row.skill.trim(), row.path])),
      roles: [...arm.skills.roles],
    };
  }
  if (arm.config.rows.length) {
    overlay.config = {
      values: Object.fromEntries(arm.config.rows.map((row) => [row.name, configValue(row.value)])),
      roles: [...arm.config.roles],
    };
  }
  return { name: arm.name.trim(), control: arm.control, weight: count(arm.weight), overlay };
}

function populationBody(fields) {
  const population = {};
  if (fields.complexity.length) population.complexity = [...fields.complexity];
  for (const [name] of POPULATION_LISTS) {
    const values = list(fields[name]);
    if (values.length) population[name] = values;
  }
  return population;
}

function definitionBody(state) {
  const fields = state.fields;
  const guardrails = {};
  for (const row of state.guardrails) {
    if (row.metric || row.threshold.trim()) guardrails[row.metric] = row.threshold.trim();
  }
  const body = {
    hypothesis: fields.hypothesis,
    arms: state.arms.map(armBody),
    population: populationBody(fields),
    guardrails,
    mode: fields.mode,
  };
  // Empty-value contract, shared by create, edit and Save and start: a blank
  // optional field is omitted, and an omitted field leaves the stored draft
  // unchanged. Blank is a value only when the stored draft has none; edit
  // callers must refuse `clearedOptionalFields` before sending.
  if (fields.primary_metric) body.primary_metric = fields.primary_metric;
  for (const name of ["min_sample", "max_days"]) {
    if (fields[name].trim()) body[name] = count(fields[name]);
  }
  if (fields.reason.trim()) body.reason = fields.reason.trim();
  const name = fields.name.trim();
  if (state.mode === "create" || name !== state.original?.name) body.name = name;
  return body;
}

// The optional fields `definitionBody` omits when blank. In edit mode an
// omitted field the stored draft already sets would silently keep its old
// value, so callers refuse it before reporting a successful save. Create
// mode has no stored draft, so nothing is ever cleared there.
function clearedOptionalFields(original, body) {
  if (!original) return [];
  const cleared = [];
  if (body.primary_metric === undefined && text(original.primary_metric).trim() !== "") cleared.push("primary_metric");
  for (const name of ["min_sample", "max_days"]) {
    if (body[name] === undefined && text(original[name]).trim() !== "") cleared.push(name);
  }
  return cleared;
}

const CLEAR_REFUSAL = {
  primary_metric: "Clearing the primary metric is not supported: the draft keeps its saved `primary_metric`. Re-select a metric to change it.",
  min_sample: "Clearing `min_sample` is not supported: the draft keeps its saved value. Enter a positive number to change it.",
  max_days: "Clearing `max_days` is not supported: the draft keeps its saved value. Enter a positive number to change it.",
};

function showClearRefusal(state, cleared) {
  state.errors = Object.fromEntries(cleared.map((field) => [field, CLEAR_REFUSAL[field]]));
  state.status = "";
  state.pending = false;
  state.confirming = false;
  renderForm();
  $(HOST)?.querySelector(".exf-error")?.scrollIntoView?.({ block: "center" });
}

// ---- error placement ------------------------------------------------------------

/// The form field a core refusal names. Core's messages name a field in
/// backticks (`population.complexity`, `guardrails`, `min_sample`) or name an
/// arm (`arm \`treatment\``) and, for an overlay, its surface; anything else
/// belongs to the whole form.
export function experimentErrorField(message, armNames) {
  const textValue = String(message || "");
  // A started experiment's refusal names every definition field at once.
  if (/editable only in draft/.test(textValue)) return "form";
  const armMatch = textValue.match(/\barm(?: name)? `([^`]*)`/);
  if (armMatch) {
    const index = armNames.findIndex((name) => name.trim() === armMatch[1].trim());
    if (index < 0) return "arms";
    const rest = textValue.slice(armMatch.index + armMatch[0].length);
    const surface = rest.match(/\b(crew|instructions|tools?|skills|config)\b/);
    if (!surface) return `arm:${index}`;
    return `arm:${index}:${surface[1] === "tool" ? "tools" : surface[1]}`;
  }
  if (/population/.test(textValue)) return "population";
  if (/primary_metric|primary metric/.test(textValue)) return "primary_metric";
  if (/guardrail/.test(textValue)) return "guardrails";
  if (/min_sample/.test(textValue)) return "min_sample";
  if (/max_days/.test(textValue)) return "max_days";
  if (/experiment mode|`mode`|\bmode:/.test(textValue)) return "mode";
  if (/experiment name|experiment named|\bname:/.test(textValue)) return "name";
  if (/hypothesis/.test(textValue)) return "hypothesis";
  if (/`arms`|\barms\b|control|treatment arm/.test(textValue)) return "arms";
  return "form";
}

function showError(state, error, prefix = "") {
  const message = `${prefix}${error?.message || "The request failed."}`;
  const field = error?.code === "authorization_denied"
    ? "form"
    : experimentErrorField(error?.message, state.arms.map((arm) => arm.name));
  state.errors = { [field]: message };
  const surface = field.match(/^arm:(\d+):(\w+)$/);
  if (surface) state.arms[Number(surface[1])].open[surface[2]] = true;
  state.status = "";
  state.pending = false;
  renderForm();
  $(HOST)?.querySelector(".exf-error")?.scrollIntoView?.({ block: "center" });
}

function errorSlot(state, field) {
  const message = state.errors[field];
  if (!message) return null;
  const slot = el("p", { class: "exf-error", role: "alert", text: message });
  slot.id = `exf-error-${field.replace(/:/g, "-")}`;
  return slot;
}

// ---- submission -------------------------------------------------------------------

async function submit(start) {
  const state = form;
  if (!state || state.pending) return;
  if (!(state.mode === "create" ? authoring?.create : authoring?.update)?.authorized) return;
  if (start && (!authoring?.start?.authorized || !state.confirming)) return;
  // Save and start writes the definition its confirmation captured, not the
  // form as it stands when Confirm is pressed.
  const body = start ? state.confirming.body : definitionBody(state);
  // Clearing an optional field the stored draft sets would silently keep its
  // old value, so refuse it next to its field instead of saving. This one
  // check covers ordinary Save and the captured Save-and-start body.
  if (state.mode === "edit") {
    const cleared = clearedOptionalFields(state.original, body);
    if (cleared.length) {
      showClearRefusal(state, cleared);
      return;
    }
  }
  const visit = captureWorkspaceVisit();
  const current = () => form === state && visit.isCurrent();
  state.pending = true;
  state.confirming = false;
  state.errors = {};
  state.status = start ? "Saving and starting…" : "Saving…";
  renderForm();
  let saved;
  try {
    saved = state.mode === "create"
      ? await requestJson(ENDPOINT, "POST", body, visit)
      : await requestJson(`${ENDPOINT}/${encodeURIComponent(state.id)}`, "PATCH", body, visit);
  } catch (error) {
    if (current()) showError(state, error);
    return;
  }
  if (!current()) return;
  if (saved && saved.id) {
    state.mode = "edit";
    state.id = saved.id;
    state.original = saved;
  }
  if (start) {
    try {
      await requestJson(`${ENDPOINT}/${encodeURIComponent(state.id)}/start`, "POST",
        body.reason ? { reason: body.reason } : {}, visit);
    } catch (error) {
      if (!current()) return;
      showError(state, error, "Saved as a draft, but it did not start: ");
      refresh();
      return;
    }
    if (!current()) return;
  }
  closeExperimentForm();
  await refresh();
}

// ---- start confirmation ---------------------------------------------------------------

/// What Save and start confirms, captured when it is pressed. Confirm starts
/// `body`, the definition these fields describe, so the form can change
/// afterwards without changing what starts.
function confirmationOf(state) {
  return {
    name: state.fields.name.trim(),
    arms: state.arms.map((arm) => ({ name: arm.name.trim(), control: arm.control, weight: arm.weight.trim() })),
    population: populationBody(state.fields),
    mode: state.fields.mode,
    body: definitionBody(state),
  };
}

function populationText(population) {
  const parts = [];
  const add = (label, values) => { if (values && values.length) parts.push(`${label} ${values.join(", ")}`); };
  add("complexity", population.complexity);
  add("task type", population.task_type);
  add("tags include", population.tags_include);
  add("tags exclude", population.tags_exclude);
  add("origin", population.origin);
  return parts.length ? parts.join(" · ") : "every task";
}

/// What a start changes, said before it happens: the arms with their
/// weights, the population and the mode.
function startSummary({ name, arms, population, mode }) {
  const armText = arms.map((arm) => `${arm.name || "(unnamed)"}${arm.control ? " (control)" : ""} weight ${arm.weight}`).join(", ");
  const modeText = mode === "live"
    ? "Live: enrolled tasks' runs get their arm's overlay, which changes how those dispatches run."
    : "Shadow: assigns tasks to arms and applies nothing.";
  const lines = [
    el("p", { class: "exf-confirm-title", text: `Start ${name || "this experiment"}?` }),
    el("dl", { class: "exf-confirm-facts" }, [
      el("div", {}, [el("dt", { text: "Arms" }), el("dd", { text: armText || "none" })]),
      el("div", {}, [el("dt", { text: "Population" }), el("dd", { text: populationText(population || {}) })]),
      el("div", {}, [el("dt", { text: "Mode" }), el("dd", { text: modeText })]),
    ]),
  ];
  if (authoring && authoring.enabled === false) {
    lines.push(el("p", { class: "exf-note", text: "experiments.enabled is false in this workspace, so Orbit will refuse the start." }));
  }
  return lines;
}

/// Edit and Start for a draft the session may change, with Start's
/// confirmation; null for a started experiment or a read-only session.
export function draftActions(experiment, payloadAuthoring) {
  if (experiment.status !== "draft") return null;
  const canEdit = Boolean(payloadAuthoring?.update?.authorized);
  const canStart = Boolean(payloadAuthoring?.start?.authorized);
  if (!canEdit && !canStart) return null;
  const host = el("div", { class: "exf-draft-actions" });
  host.dataset.key = `draft-actions-${experiment.id}`;
  const paint = () => {
    const state = starts.get(experiment.id) || null;
    const nodes = [];
    const buttons = el("div", { class: "exf-buttons" });
    if (canEdit) {
      const edit = el("button", { class: "action save exf-edit", type: "button", text: "Edit" });
      edit.disabled = Boolean(state?.pending);
      edit.addEventListener("click", () => openExperimentForm(experiment));
      buttons.append(edit);
    }
    if (canStart && !state) {
      const start = el("button", { class: "action ship exf-start", type: "button", text: "Start" });
      start.addEventListener("click", () => {
        starts.set(experiment.id, { reason: "", pending: false, status: "" });
        paint();
        host.querySelector("button.exf-confirm-start")?.focus();
      });
      buttons.append(start);
    }
    nodes.push(buttons);
    if (state) nodes.push(startConfirmation(experiment, state, paint));
    host.replaceChildren(...nodes);
  };
  paint();
  return host;
}

function startConfirmation(experiment, state, paint) {
  const reason = el("textarea", { class: "exf-control", "aria-label": "Reason for starting" });
  reason.rows = 2;
  reason.value = state.reason;
  reason.disabled = state.pending;
  reason.addEventListener("input", () => { state.reason = reason.value; });
  const confirm = el("button", { class: "action ship exf-confirm-start", type: "button", text: state.pending ? "Starting…" : "Confirm start" });
  confirm.disabled = state.pending;
  const back = el("button", { class: "action cancel", type: "button", text: "Cancel" });
  back.disabled = state.pending;
  back.addEventListener("click", () => { starts.delete(experiment.id); paint(); });
  confirm.addEventListener("click", async () => {
    const visit = captureWorkspaceVisit();
    state.pending = true;
    state.status = "";
    paint();
    try {
      const why = state.reason.trim();
      await requestJson(`${ENDPOINT}/${encodeURIComponent(experiment.id)}/start`, "POST", why ? { reason: why } : {}, visit);
    } catch (error) {
      if (!visit.isCurrent() || starts.get(experiment.id) !== state) return;
      state.pending = false;
      state.status = error?.message || "Start failed.";
      renderDraftStartState(experiment.id);
      return;
    }
    if (!visit.isCurrent()) return;
    starts.delete(experiment.id);
    await refresh();
  });
  return el("div", { class: "exf-confirm", role: "group", "aria-label": `Start ${experiment.name}` }, [
    ...startSummary({
      name: experiment.name,
      arms: (experiment.arms || []).filter((arm) => arm.retired_segment == null),
      population: experiment.population,
      mode: experiment.mode,
    }),
    el("label", { class: "exf-field" }, [el("span", { text: "Reason (recorded with the start)" }), reason]),
    el("div", { class: "exf-buttons" }, [confirm, back]),
    state.status ? el("p", { class: "exf-error", role: "alert", text: state.status }) : null,
  ]);
}

/// For a started experiment in a session that may edit drafts: why there is
/// no Edit.
export function lockedNote(experiment, payloadAuthoring) {
  if (experiment.status === "draft" || !payloadAuthoring?.update?.authorized) return null;
  return el("p", {
    class: "exf-note exf-locked",
    text: `This experiment is ${experiment.status}, so its definition can no longer be edited; only a draft is editable.`,
  });
}

// ---- rendering --------------------------------------------------------------------------

function renderForm() {
  const host = $(HOST);
  if (!host || !form) return;
  const restore = captureFocus(host);
  host.replaceChildren(formView(form));
  restore();
}

// A structural edit (a row added or removed, a choice that shows or hides a
// field) rebuilds the form from state; typing only updates state.
function rerender() {
  renderForm();
}

// The definition's controls are read-only while a request is in flight and
// while Save and start confirms it; Back to editing unlocks them.
function locked(state = form) {
  return Boolean(state?.pending || state?.confirming);
}

function input(value, onInput, opts = {}) {
  const node = el(opts.multiline ? "textarea" : "input", { class: "exf-control", "aria-label": opts.label || null });
  if (!opts.multiline) node.type = opts.type || "text";
  if (opts.multiline) node.rows = opts.rows || 2;
  if (opts.placeholder) node.placeholder = opts.placeholder;
  if (opts.min != null) node.min = String(opts.min);
  node.value = value;
  node.disabled = locked();
  node.addEventListener("input", () => onInput(node.value));
  return node;
}

function select(options, value, onChange, label) {
  const node = el("select", { class: "exf-control", "aria-label": label || null });
  for (const [optionValue, optionLabel] of options) {
    const option = el("option", { text: optionLabel });
    option.value = optionValue;
    node.append(option);
  }
  node.value = value;
  node.disabled = locked();
  node.addEventListener("change", () => onChange(node.value));
  return node;
}

function field(label, control, error, hint = null) {
  return el("label", { class: "exf-field" }, [
    el("span", { class: "exf-label", text: label }),
    control,
    hint ? el("span", { class: "exf-hint", text: hint }) : null,
    error,
  ]);
}

function smallButton(textValue, onClick, className = "") {
  const button = el("button", { class: `action exf-small ${className}`.trim(), type: "button", text: textValue });
  button.disabled = locked();
  button.addEventListener("click", onClick);
  return button;
}

function rolePicker(roles, label) {
  return el("div", { class: "exf-roles", role: "group", "aria-label": label }, ROLES.map((role) => {
    const box = el("input", { type: "checkbox" });
    box.checked = roles.includes(role);
    box.disabled = locked();
    box.addEventListener("change", () => {
      const at = roles.indexOf(role);
      if (box.checked && at < 0) roles.push(role);
      if (!box.checked && at >= 0) roles.splice(at, 1);
    });
    return el("label", { class: "exf-check" }, [box, el("span", { text: role })]);
  }));
}

function formView(state) {
  const fields = state.fields;
  const title = state.mode === "create" ? "New experiment" : `Edit draft ${state.original?.name || state.id}`;
  const root = el("form", { class: "mx-card exf", "aria-label": title });
  root.dataset.key = "experiment-form";
  root.noValidate = true;
  root.setAttribute("aria-busy", String(state.pending));
  root.addEventListener("submit", (event) => { event.preventDefault(); submit(false); });
  const metrics = authoring?.metrics || [];
  root.append(
    el("div", { class: "mx-card-head" }, [el("h2", { class: "mx-card-title", text: title })]),
    el("p", { class: "mx-blurb", text: state.mode === "create"
      ? "Saving creates a draft; nothing is assigned until the experiment starts."
      : "Only a draft is editable. Saving replaces its arms, population and guardrails with what this form holds." }),
    el("div", { class: "exf-grid" }, [
      field("Name", input(fields.name, (value) => { fields.name = value; }), errorSlot(state, "name")),
      field("Mode", select([["shadow", "shadow (assigns, applies nothing)"], ["live", "live (applies the arm's overlay)"]],
        fields.mode, (value) => { fields.mode = value; }), errorSlot(state, "mode")),
    ]),
    field("Hypothesis", input(fields.hypothesis, (value) => { fields.hypothesis = value; }, { multiline: true }), errorSlot(state, "hypothesis")),
    populationView(state),
    armsView(state),
    el("div", { class: "exf-grid" }, [
      field("Primary metric", select([["", "none yet"], ...metrics.map((metric) => [metric, metric])],
        fields.primary_metric, (value) => { fields.primary_metric = value; }), errorSlot(state, "primary_metric")),
      field("Minimum sample per arm", input(fields.min_sample, (value) => { fields.min_sample = value; }, { type: "number", min: 1 }), errorSlot(state, "min_sample")),
      field("Maximum days", input(fields.max_days, (value) => { fields.max_days = value; }, { type: "number", min: 1 }), errorSlot(state, "max_days")),
    ]),
    guardrailsView(state, metrics),
    field("Reason", input(fields.reason, (value) => { fields.reason = value; }, { multiline: true }), errorSlot(state, "reason"),
      "Recorded with the event."),
    actionsView(state),
  );
  return root;
}

function populationView(state) {
  const fields = state.fields;
  const complexities = authoring?.complexities || [];
  const boxes = el("div", { class: "exf-roles", role: "group", "aria-label": "Complexity" }, complexities.map((complexity) => {
    const box = el("input", { type: "checkbox" });
    box.checked = fields.complexity.includes(complexity);
    box.disabled = locked(state);
    box.addEventListener("change", () => {
      fields.complexity = complexities.filter((value) => (value === complexity ? box.checked : fields.complexity.includes(value)));
    });
    return el("label", { class: "exf-check" }, [box, el("span", { text: complexity })]);
  }));
  return el("fieldset", { class: "exf-section exf-population" }, [
    el("legend", { text: "Population" }),
    el("p", { class: "exf-hint", text: "An empty filter places no restriction. Separate several values with commas." }),
    el("div", { class: "exf-field" }, [el("span", { class: "exf-label", text: "Complexity" }), boxes]),
    el("div", { class: "exf-grid" }, POPULATION_LISTS.map(([name, label]) =>
      field(label, input(fields[name], (value) => { fields[name] = value; })))),
    errorSlot(state, "population"),
  ]);
}

function armsView(state) {
  const add = smallButton("Add treatment", () => {
    state.arms.push(blankArm(`treatment-${state.arms.length}`, false));
    rerender();
    $(HOST)?.querySelectorAll(".exf-arm")?.[state.arms.length - 1]?.querySelector("input")?.focus();
  }, "exf-add-arm");
  return el("fieldset", { class: "exf-section exf-arms" }, [
    el("legend", { text: "Arms" }),
    el("p", { class: "exf-hint", text: "One arm is the control; every other arm is a treatment. Weights set each arm's share of assignments." }),
    errorSlot(state, "arms"),
    ...state.arms.map((arm, index) => armView(state, arm, index)),
    el("div", { class: "exf-buttons" }, [add]),
  ]);
}

function armView(state, arm, index) {
  const control = el("input", { type: "radio", "aria-label": `${arm.name || "arm"} is the control` });
  control.name = "exf-control-arm";
  control.checked = arm.control;
  control.disabled = locked(state);
  control.addEventListener("change", () => {
    state.arms.forEach((other) => { other.control = other === arm; });
    rerender();
  });
  const remove = smallButton("Remove arm", () => {
    state.arms.splice(index, 1);
    state.errors = {};
    rerender();
  }, "exf-remove");
  const card = el("section", { class: "exf-arm", "aria-label": `Arm ${index + 1}` }, [
    el("div", { class: "exf-arm-head" }, [
      el("span", { class: "mx-sc-tag", text: arm.control ? "control" : "treatment" }),
      remove,
    ]),
    el("div", { class: "exf-grid" }, [
      field("Arm name", input(arm.name, (value) => { arm.name = value; })),
      field("Weight", input(arm.weight, (value) => { arm.weight = value; }, { type: "number", min: 1 })),
      el("label", { class: "exf-check exf-control-pick" }, [control, el("span", { text: "Control arm" })]),
    ]),
    errorSlot(state, `arm:${index}`),
    ...SURFACES.map(([surface, label]) => surfaceView(state, arm, index, surface, label)),
  ]);
  card.dataset.key = `arm-${arm.key}`;
  return card;
}

function surfaceView(state, arm, index, surface, label) {
  const details = el("details", { class: `exf-surface exf-surface-${surface}` });
  details.open = Boolean(arm.open[surface]);
  details.addEventListener("toggle", () => { arm.open[surface] = details.open; });
  const summaryText = surface === "skills" ? `${label} · Claude on Unix only` : label;
  const children = [el("summary", { text: summaryText }), ...surfaceBody(state, arm, surface), errorSlot(state, `arm:${index}:${surface}`)];
  details.append(...children.filter(Boolean));
  return details;
}

function surfaceBody(state, arm, surface) {
  switch (surface) {
    case "crew": return crewBody(arm);
    case "instructions": return instructionsBody(arm);
    case "tools": return [
      el("div", { class: "exf-grid" }, [
        field("Add tools", input(arm.tools.add, (value) => { arm.tools.add = value; }, { placeholder: "orbit.search" })),
        field("Remove tools", input(arm.tools.remove, (value) => { arm.tools.remove = value; })),
      ]),
      el("div", { class: "exf-field" }, [el("span", { class: "exf-label", text: "Roles" }), rolePicker(arm.tools.roles, "Tool roles")]),
    ];
    case "skills": return skillsBody(arm);
    default: return configBody(arm);
  }
}

function crewBody(arm) {
  const crews = authoring?.crews || [];
  const options = crews.map((crew) => [crew.name, crew.enabled === false ? `${crew.name} (disabled)` : crew.name]);
  const rows = arm.crew.map((member, at) => {
    // A stored crew no longer configured stays visible so the edit does not
    // silently drop it; core refuses it on start.
    const choices = options.some(([name]) => name === member.crew) || !member.crew
      ? options : [[member.crew, `${member.crew} (not configured)`], ...options];
    return el("div", { class: "exf-row" }, [
      select([["", "choose a crew"], ...choices], member.crew, (value) => { member.crew = value; }, "Crew"),
      input(member.weight, (value) => { member.weight = value; }, { type: "number", min: 1, label: "Crew weight" }),
      smallButton("Remove", () => { arm.crew.splice(at, 1); rerender(); }, "exf-remove"),
    ]);
  });
  return [
    el("p", { class: "exf-hint", text: crews.length
      ? "Each task draws one crew from the pool by weight."
      : "This workspace configures no crews." }),
    ...rows,
    crews.length ? el("div", { class: "exf-buttons" }, [smallButton("Add crew", () => {
      arm.crew.push({ key: key(), crew: "", weight: "1" });
      rerender();
    }, "exf-add-crew")]) : null,
  ];
}

function instructionsBody(arm) {
  return [
    el("p", { class: "exf-hint", text: "A repository file replaces a role's instructions; suppressing leaves Orbit's default tag instructions out." }),
    ...ROLES.map((role) => {
      const entry = arm.instructions[role];
      return el("div", { class: "exf-row exf-instruction" }, [
        el("span", { class: "exf-label", text: role }),
        select([["default", "default"], ["file", "repository file"], ["suppress", "suppress the default"]], entry.choice,
          (value) => { entry.choice = value; rerender(); }, `${role} instructions`),
        entry.choice === "file"
          ? input(entry.path, (value) => { entry.path = value; }, { label: `${role} instruction file`, placeholder: "path/in/repo.md" })
          : null,
      ]);
    }),
  ];
}

function skillsBody(arm) {
  const rows = arm.skills.rows.map((row, at) => el("div", { class: "exf-row" }, [
    input(row.skill, (value) => { row.skill = value; }, { label: "Skill name", placeholder: "skill name" }),
    input(row.path, (value) => { row.path = value; }, { label: "Bundle directory", placeholder: "path/to/bundle" }),
    smallButton("Remove", () => { arm.skills.rows.splice(at, 1); rerender(); }, "exf-remove"),
  ]));
  return [
    el("p", { class: "exf-hint", text: "Replaces a named skill with a repository bundle. Applies to Claude crews on Unix hosts only." }),
    ...rows,
    el("div", { class: "exf-buttons" }, [smallButton("Add skill", () => {
      arm.skills.rows.push({ key: key(), skill: "", path: "" });
      rerender();
    })]),
    arm.skills.rows.length
      ? el("div", { class: "exf-field" }, [el("span", { class: "exf-label", text: "Roles" }), rolePicker(arm.skills.roles, "Skill roles")])
      : null,
  ];
}

function configBody(arm) {
  const keys = authoring?.config_keys || [];
  const rows = arm.config.rows.map((row, at) => {
    const choices = keys.includes(row.name) || !row.name ? keys : [row.name, ...keys];
    return el("div", { class: "exf-row" }, [
      select([["", "choose a key"], ...choices.map((name) => [name, keys.includes(name) ? name : `${name} (not allowed)`])],
        row.name, (value) => { row.name = value; }, "Config key"),
      input(row.value, (value) => { row.value = value; }, { label: "Config value", placeholder: "value" }),
      smallButton("Remove", () => { arm.config.rows.splice(at, 1); rerender(); }, "exf-remove"),
    ]);
  });
  return [
    el("p", { class: "exf-hint", text: keys.length
      ? "Only the keys experiments.config_keys allows. A value that parses as JSON is sent as JSON, anything else as text."
      : "experiments.config_keys allows no key in this workspace." }),
    ...rows,
    keys.length ? el("div", { class: "exf-buttons" }, [smallButton("Add config value", () => {
      arm.config.rows.push({ key: key(), name: "", value: "" });
      rerender();
    })]) : null,
    arm.config.rows.length
      ? el("div", { class: "exf-field" }, [el("span", { class: "exf-label", text: "Roles" }), rolePicker(arm.config.roles, "Config roles")])
      : null,
  ];
}

function guardrailsView(state, metrics) {
  const rows = state.guardrails.map((row, at) => el("div", { class: "exf-row" }, [
    select([["", "choose a metric"], ...metrics.map((metric) => [metric, metric])], row.metric,
      (value) => { row.metric = value; }, "Guardrail metric"),
    input(row.threshold, (value) => { row.threshold = value; }, { label: "Guardrail threshold", placeholder: "-5pt" }),
    smallButton("Remove", () => { state.guardrails.splice(at, 1); rerender(); }, "exf-remove"),
  ]));
  return el("fieldset", { class: "exf-section exf-guardrails" }, [
    el("legend", { text: "Guardrails" }),
    el("p", { class: "exf-hint", text: "A metric and the largest difference from control a treatment may show, such as -5pt, +0.5 or +3pt." }),
    ...rows,
    el("div", { class: "exf-buttons" }, [smallButton("Add guardrail", () => {
      state.guardrails.push({ key: key(), metric: "", threshold: "" });
      rerender();
    }, "exf-add-guardrail")]),
    errorSlot(state, "guardrails"),
  ]);
}

function actionsView(state) {
  const canSave = Boolean((state.mode === "create" ? authoring?.create : authoring?.update)?.authorized);
  const canStart = Boolean(authoring?.start?.authorized);
  if (state.confirming) {
    const confirm = el("button", { class: "action ship exf-confirm-start", type: "button", text: "Confirm: save and start" });
    confirm.addEventListener("click", () => submit(true));
    const back = el("button", { class: "action cancel", type: "button", text: "Back to editing" });
    back.addEventListener("click", () => { state.confirming = false; rerender(); });
    return el("div", { class: "exf-confirm", role: "group", "aria-label": "Confirm start" }, [
      ...startSummary(state.confirming),
      el("div", { class: "exf-buttons" }, [confirm, back]),
    ]);
  }
  const save = el("button", { class: "action save exf-save", type: "submit", text: state.mode === "create" ? "Save draft" : "Save changes" });
  save.disabled = state.pending || !canSave;
  const start = canStart ? el("button", { class: "action ship exf-save-start", type: "button", text: "Save and start" }) : null;
  if (start) {
    start.disabled = state.pending || !canSave;
    start.addEventListener("click", () => {
      // Refuse a cleared optional field before confirming, so Save and start
      // cannot capture a body that would silently keep its old value.
      if (state.mode === "edit") {
        const cleared = clearedOptionalFields(state.original, definitionBody(state));
        if (cleared.length) {
          showClearRefusal(state, cleared);
          return;
        }
      }
      state.confirming = confirmationOf(state); rerender();
    });
  }
  const cancel = el("button", { class: "action cancel exf-cancel", type: "button", text: "Cancel" });
  cancel.disabled = state.pending;
  cancel.addEventListener("click", () => {
    closeExperimentForm();
    $(OPEN)?.focus();
  });
  return el("div", { class: "exf-footer" }, [
    errorSlot(state, "form"),
    el("div", { class: "exf-buttons" }, [save, start, cancel]),
    el("p", { class: "exf-status", role: "status", text: state.status }),
  ]);
}
