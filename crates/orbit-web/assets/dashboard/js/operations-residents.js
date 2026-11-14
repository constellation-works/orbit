// Automation › Residents: start, supervise, steer and stop resident mandates.
// The page is a list of mandates, one row each, with the selected mandate's
// detail opening under its row; one start flow creates a mandate, grants its
// rights and turns the workspace's cycle routine on. Every read and write goes
// through the `orbit.resident.*` tools behind /api/residents; the server keeps
// the mandate rules, so a refused write shows the server's own message.

import { captureWorkspaceVisit, detailsPanel, el, fetchJson, infoHint, makeToggleRow, requestJson, requestPanel } from './common.js';
import { actionReason, explainUnavailable, feedback, selectedWorkspaceName, setRailSubtabCount } from './operations-shared.js';

const FEEDBACK = "resident-operation-feedback";
const RIGHTS = ["dispatch", "promote", "merge", "plugin_upgrade"];
const STATE_STATUS = { active: "done", paused: "todo", taken_over: "in_progress", ended: "archived" };
const STATE_ORDER = { active: 0, taken_over: 1, paused: 2 };

const list = (values) => (values?.length ? values.join(", ") : "none");
const lines = (text) => String(text || "").split("\n").map(line => line.trim()).filter(Boolean);
const csv = (text) => String(text || "").split(",").map(part => part.trim()).filter(Boolean);
const when = (value) => (value ? String(value).replace("T", " ").replace(/\.\d+Z$/, "Z") : "none");
const capitalize = (text) => (text ? text[0].toUpperCase() + text.slice(1) : "");

// Page state that is the operator's, not the payload's: the selected mandate,
// the open start flow and a chosen action awaiting its reason. A background
// refresh rebuilds the page from it; a workspace switch clears it.
const ui = { revision: null, payload: null, selected: null, startForm: null, pending: null };

function resetUiFor(visit) {
  if (ui.revision === visit.revision) return;
  Object.assign(ui, { revision: visit.revision, payload: null, selected: null, startForm: null, pending: null });
}


function fact(label, value) {
  return el("div", { class: "resident-fact" }, [
    el("span", { class: "resident-fact-label", text: label }),
    el("span", { class: "resident-fact-value", text: value }),
  ]);
}

// "in 22m", "in 3h", "due 40m ago": a wake time read at a glance.
function relative(value) {
  const at = Date.parse(value);
  if (Number.isNaN(at)) return "-";
  const minutes = Math.round((at - Date.now()) / 60000);
  const span = (mins) => (mins < 60 ? `${mins}m` : mins < 2880 ? `${Math.round(mins / 60)}h` : `${Math.round(mins / 1440)}d`);
  return minutes > 0 ? `in ${span(minutes)}` : minutes === 0 ? "now" : `due ${span(-minutes)} ago`;
}

// One write: pinned to the visit the operator acted in, refreshing the view
// on success and showing the server's message on refusal. `applied` updates
// page state before that refresh renders.
async function write(path, body, success, controls = [], applied = null) {
  const visit = captureWorkspaceVisit();
  for (const control of controls) control.disabled = true;
  let result;
  try {
    result = await requestJson(path, "POST", body, visit);
  } catch (error) {
    if (!visit.isCurrent() || error?.code === "stale_workspace_visit") return null;
    for (const control of controls) control.disabled = false;
    feedback(FEEDBACK, "error", `Refused: ${error.message || error}`);
    return null;
  }
  if (!visit.isCurrent()) return null;
  feedback(FEEDBACK, "success", typeof success === "function" ? success(result) : success);
  applied?.();
  await refreshAfterWrite(visit);
  return result;
}

async function refreshAfterWrite(visit) {
  try {
    await fetchAndRenderResidents({ deliberate: true });
  } catch (error) {
    if (visit.isCurrent()) {
      // The write already changed page state; do not leave a finished form up.
      rerender();
      document.getElementById(FEEDBACK)?.appendChild(el("span", {
        text: `\nRefresh failed: ${error.message || error}. The change was applied; refresh before acting again.`,
      }));
    }
  }
}

function gated(payload, action, button) {
  const reason = actionReason(payload, action);
  if (!reason) return button;
  button.disabled = true;
  return explainUnavailable(button, reason);
}

function button(text, label, variant = "") {
  return el("button", { class: `operation-button ${variant}`.trim(), type: "button", text, "aria-label": label || text });
}

function field(label, control, hint) {
  return el("label", { class: "resident-field" }, [
    el("span", { class: "resident-field-label", text: label }),
    control,
    hint ? el("span", { class: "resident-field-hint", text: hint }) : null,
  ]);
}

function input(name, attrs = {}) {
  return el("input", { name, type: "text", ...attrs });
}

function rightBoxes(prefix, legend, checked = []) {
  return el("fieldset", { class: "resident-rights" }, [
    el("legend", { text: legend }),
    ...RIGHTS.map(right => {
      const box = el("input", { type: "checkbox", name: `${prefix}-${right}`, value: right });
      box.checked = checked.includes(right);
      return el("label", { class: "resident-right" }, [box, el("span", { text: right })]);
    }),
  ]);
}

const checkedRights = (form, prefix) => RIGHTS.filter(right => form.elements[`${prefix}-${right}`]?.checked);

// A render the operator's own action asked for: it replaces a focused form,
// which only a background refresh may not.
function rerender() {
  if (ui.payload) renderResidents(ui.payload, true);
}

// --- routine -----------------------------------------------------------------

function routineToggleBody(routine, enabled) {
  return {
    name: routine.name,
    source: routine.source,
    target: routine.target,
    machine_name: routine.machine_name,
    expected_enabled: routine.enabled,
    enabled,
  };
}

function routineStatus(payload) {
  const routine = payload.routine;
  if (!routine) {
    return el("div", { class: "resident-routine" }, [
      el("span", { class: "resident-routine-label", text: "No cycle routine" }),
      infoHint("No resident cycle routine is loaded for this workspace, so no cycle wakes a mandate."),
    ]);
  }
  const state = el("span", { class: "pill", text: routine.enabled ? "on" : "off" });
  state.dataset.status = routine.enabled ? "done" : "todo";
  const verb = routine.enabled ? "Disable" : "Enable";
  const toggle = button(routine.enabled ? "Turn off" : "Turn on", `${verb} the resident cycle routine`, "resident-small");
  toggle.addEventListener("click", () => write(
    "/api/routines/toggle",
    routineToggleBody(routine, !routine.enabled),
    `Resident cycle routine ${routine.enabled ? "disabled" : "enabled"}.`,
    [toggle],
  ));
  return el("div", { class: "resident-routine" }, [
    el("span", { class: "resident-routine-label", text: "Cycle routine" }),
    state,
    infoHint(`${routine.name} wakes due mandates. While it is off, no resident cycles.`),
    gated(payload, "routine_toggle", toggle),
  ]);
}

// --- start flow ----------------------------------------------------------------

function startButton(payload) {
  const start = button("Start a resident", "Start a resident", "primary");
  start.addEventListener("click", () => {
    ui.startForm = startForm(payload);
    rerender();
    ui.startForm?.querySelector('[name="goal"]')?.focus();
  });
  return gated(payload, "resident_write", start);
}

function routineNote(payload) {
  const routine = payload.routine;
  if (!routine) return el("p", { class: "resident-step-note warn", text: "No cycle routine is loaded here, so this resident won't wake until one is." });
  if (!routine.enabled) return el("p", { class: "resident-step-note", text: "The cycle routine is off; starting turns it on." });
  return el("p", { class: "resident-step-note", text: "The cycle routine is on." });
}

function startForm(payload) {
  const crew = el("select", { name: "crew" }, (payload.crews || []).map(name => {
    const option = el("option", { value: name, text: name });
    option.selected = name === payload.default_crew;
    return option;
  }));
  const continuity = el("select", { name: "continuity" }, [
    el("option", { value: "prefer", text: "prefer" }),
    el("option", { value: "off", text: "off" }),
  ]);
  const complexity = el("select", { name: "complexity" }, ["low", "medium", "hard", "xhard"].map(level => {
    const option = el("option", { value: level, text: level });
    option.selected = level === "medium";
    return option;
  }));
  const submit = el("button", { class: "operation-button primary", type: "submit", text: "Start resident" });
  const cancel = button("Cancel", "Cancel starting a resident");
  const status = el("p", { class: "resident-start-status", role: "status", "aria-live": "polite" });
  const form = el("form", { class: "resident-form resident-start", "aria-label": "Start a resident" }, [
    el("h3", { class: "resident-start-title", text: "Start a resident" }),
    el("fieldset", { class: "resident-step" }, [
      el("legend", { text: "1 · What it should do" }),
      field("Goal", input("goal", { maxlength: "240", required: "", placeholder: "One line: what the resident keeps true" })),
      field("Exit criteria", el("textarea", { name: "exit_criteria", rows: "3" }), "One per line, 1–8."),
      field("Watch tags", input("watch_tags"), "Comma-separated."),
      field("Watch tasks", input("watch_tasks"), "Comma-separated task ids."),
    ]),
    el("fieldset", { class: "resident-step" }, [
      el("legend", { text: "2 · What it may do" }),
      rightBoxes("start", "Rights"),
      field("Workspaces", input("workspaces", { value: selectedWorkspaceName() || "" }), "Comma-separated."),
      field("Your words", input("quote", { placeholder: "Recorded verbatim with the grant" }), "Needed when you pick a right. No rights: it observes only."),
    ]),
    el("fieldset", { class: "resident-step" }, [
      el("legend", { text: "3 · When it runs" }),
      field("Floor minutes", input("floor_minutes", { type: "number", min: "1", value: "15" })),
      field("Heartbeat minutes", input("heartbeat_minutes", { type: "number", min: "1", value: "60" }), "At least the floor."),
      field("Deadline", input("deadline", { type: "datetime-local" }), "Optional."),
      field("Crew", crew),
      field("Continuity", continuity),
      field("Complexity", complexity),
      field("Title", input("title"), "Optional; defaults to the goal."),
      routineNote(payload),
    ]),
    el("div", { class: "resident-form-actions" }, [status, cancel, gated(payload, "resident_write", submit)]),
  ]);
  cancel.addEventListener("click", () => {
    ui.startForm = null;
    rerender();
  });
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    startResident(form, [submit, cancel], status);
  });
  return form;
}

async function post(path, body, visit) {
  try {
    return { ok: true, result: await requestJson(path, "POST", body, visit) };
  } catch (error) {
    if (!visit.isCurrent() || error?.code === "stale_workspace_visit") return { stale: true };
    return { ok: false, message: error?.message || String(error) };
  }
}

// The start flow sequences three existing endpoints: create the mandate,
// grant its rights with the operator's words, then turn the cycle routine on
// when it is off. They are separate writes, so a refusal names the step and
// says plainly what already exists.
async function startResident(form, controls, status) {
  const values = form.elements;
  const rights = checkedRights(form, "start");
  const workspaces = csv(values.workspaces.value);
  const quote = values.quote.value.trim();
  if (rights.length && !quote) {
    status.textContent = "Add your words authorizing these rights.";
    values.quote.focus();
    return;
  }
  const payload = ui.payload || {};
  const routine = payload.routine;
  const steps = ["create the mandate"];
  if (rights.length) steps.push("grant its rights");
  if (routine && !routine.enabled) steps.push("turn on the cycle routine");
  const refused = (index, message, exists) => `Step ${index + 1} of ${steps.length}, ${steps[index]}, was refused: ${message}. ${exists}`;
  const mandate = {
    schema: "resident.mandate/v1",
    goal: values.goal.value.trim(),
    exit_criteria: lines(values.exit_criteria.value),
    workspaces,
    watch: { tasks: csv(values.watch_tasks.value), tags: csv(values.watch_tags.value) },
    crew: values.crew.value,
    cadence: {
      floor_minutes: Number(values.floor_minutes.value),
      heartbeat_minutes: Number(values.heartbeat_minutes.value),
    },
    continuity: values.continuity.value,
  };
  if (values.deadline.value) mandate.deadline = new Date(values.deadline.value).toISOString();

  const visit = captureWorkspaceVisit();
  for (const control of controls) control.disabled = true;
  status.textContent = "Creating the mandate…";
  const created = await post("/api/residents", { title: values.title.value.trim(), complexity: values.complexity.value, mandate }, visit);
  if (created.stale) return;
  if (!created.ok) {
    for (const control of controls) control.disabled = false;
    status.textContent = "";
    feedback(FEEDBACK, "error", refused(0, created.message, "Nothing was created."));
    return;
  }
  const id = created.result.task_id;
  const finish = async (kind, message) => {
    ui.startForm = null;
    ui.selected = id;
    ui.pending = null;
    feedback(FEEDBACK, kind, message);
    await refreshAfterWrite(visit);
  };

  let granted = "it observes only";
  if (rights.length) {
    status.textContent = "Granting its rights…";
    const grant = await post(`/api/residents/${encodeURIComponent(id)}/grant`, { add: rights, workspaces, quote }, visit);
    if (grant.stale) return;
    if (!grant.ok) {
      await finish("error", refused(1, grant.message, `${id} exists with no rights, so it observes only; grant them from its detail.`));
      return;
    }
    granted = `it may ${rights.join(", ")}`;
  }

  let routineTurnedOn = Boolean(routine?.enabled);
  if (routine && !routine.enabled) {
    status.textContent = "Turning on the cycle routine…";
    const index = steps.length - 1;
    const toggled = await post("/api/routines/toggle", routineToggleBody(routine, true), visit);
    if (toggled.stale) return;
    if (!toggled.ok) {
      await finish("error", refused(index, toggled.message, `${id} exists and ${granted}, but the cycle routine is still off, so it won't wake. Turn it on above.`));
      return;
    }
    routineTurnedOn = true;
  }
  // The server reports a disabled cycle routine on creation; when this
  // page had no routine row to turn on, that report is the notice.
  if (created.result?.cycle_routine_disabled && !routineTurnedOn) {
    await finish("error", `Started ${id}: ${granted}; ${created.result.warning || "the cycle routine is disabled, so it won't wake until it is enabled"}.`);
    return;
  }
  const wakes = routine ? "the cycle routine is on" : "no cycle routine is loaded here, so it won't wake until one is";
  await finish(routine ? "success" : "error", `Started ${id}: ${granted}; ${wakes}.`);
}

// --- one mandate: row ------------------------------------------------------------

// Why a mandate needs the operator, or "".
function needsOperator(mandate) {
  const requests = mandate.open_requests?.length || 0;
  return [
    requests ? `${requests} request${requests === 1 ? "" : "s"}` : null,
    mandate.mandate_error || mandate.authority_error ? "invalid mandate" : null,
    mandate.stalled ? "stalled" : null,
  ].filter(Boolean).join(" · ").replace(/^./, first => first.toUpperCase());
}

function lastActivity(mandate) {
  const live = mandate.live_cycle;
  if (live) return { text: "Cycling now", title: `${live.cycle} (run ${live.run_id}) until ${when(live.expires_at)}` };
  const checkpoint = mandate.latest_checkpoint;
  if (checkpoint) {
    const actions = checkpoint.actions?.length || 0;
    return {
      text: `${capitalize(checkpoint.outcome || "cycle")} · ${actions} action${actions === 1 ? "" : "s"}`,
      title: checkpoint.notes || `Cycle ${checkpoint.cycle}`,
    };
  }
  const last = mandate.control?.last;
  if (last) return { text: `${capitalize(last.action)} · ${relative(last.at).replace(/^due /, "")}`, title: when(last.at) };
  return { text: "No cycle yet", title: "" };
}

function nextWake(payload, mandate) {
  if (mandate.state !== "active") return { text: "-", title: "" };
  if (mandate.live_cycle) return { text: "Now", title: "" };
  if (!payload.routine?.enabled) return { text: "Routine off", title: "The cycle routine is off, so no cycle wakes this mandate." };
  const wake = mandate.latest_checkpoint?.next_wake;
  if (wake) return { text: relative(wake.not_before), title: `${wake.reason} after ${when(wake.not_before)}` };
  return { text: "Next tick", title: "" };
}

function stateLabel(mandate) {
  const state = el("span", { class: "pill", text: mandate.state.replace("_", " ") });
  state.dataset.status = STATE_STATUS[mandate.state] || "todo";
  return state;
}

function mandateRow(payload, mandate) {
  const selected = ui.selected === mandate.task_id;
  const needs = needsOperator(mandate);
  const activity = lastActivity(mandate);
  const wake = nextWake(payload, mandate);
  const row = el("div", {
    class: `resident-row${selected ? " selected" : ""}${mandate.state === "ended" ? " ended" : ""}`,
    "aria-label": `Resident mandate ${mandate.task_id}`,
  }, [
    el("span", { class: "resident-row-goal" }, [
      el("span", { class: "resident-goal", text: mandate.goal || mandate.mandate?.goal || mandate.title || mandate.task_id }),
      el("span", { class: "resident-id", text: mandate.task_id }),
    ]),
    el("span", { class: "resident-row-state" }, [stateLabel(mandate)]),
    el("span", { class: "resident-row-activity", text: activity.text, title: activity.title }),
    el("span", { class: "resident-row-wake", text: wake.text, title: wake.title }),
    el("span", { class: `resident-row-needs${needs ? " needs" : ""}`, text: needs || "-" }),
  ]);
  row.dataset.task = mandate.task_id;
  return makeToggleRow(row, {
    expanded: selected,
    controls: selected ? `resident-detail-${mandate.task_id}` : null,
    onToggle: () => {
      ui.selected = selected ? null : mandate.task_id;
      ui.pending = null;
      rerender();
      document.querySelector(`.resident-row[data-task="${CSS.escape(mandate.task_id)}"]`)?.focus();
    },
  });
}

function listHead() {
  return el("div", { class: "resident-row resident-list-head", "aria-hidden": "true" }, [
    el("span", { text: "Goal" }),
    el("span", { text: "State" }),
    el("span", { text: "Last activity" }),
    el("span", { text: "Next wake" }),
    el("span", { text: "Needs you" }),
  ]);
}

// --- one mandate: detail -----------------------------------------------------------

function historyList(authority) {
  const entries = [
    ...(authority?.granted || []).map(entry => ({ ...entry, kind: "granted" })),
    ...(authority?.narrowed || []).map(entry => ({ ...entry, kind: "narrowed" })),
  ].sort((a, b) => (a.revision || 0) - (b.revision || 0));
  if (!entries.length) return el("p", { class: "resident-muted", text: "No authority changes yet." });
  return el("ol", { class: "resident-history" }, entries.map(entry => {
    const change = entry.kind === "granted"
      ? `+${list(entry.added)}; +workspaces ${list(entry.workspaces_added)}`
      : `−${list(entry.removed)}; −workspaces ${list(entry.workspaces_dropped)}`;
    return el("li", {}, [
      el("span", { class: "resident-history-head", text: `r${entry.revision} ${entry.kind} by ${entry.by} via ${entry.via}` }),
      el("span", { text: ` · ${change} · ${when(entry.at)}` }),
      entry.quote ? el("q", { class: "resident-quote", text: entry.quote }) : null,
    ]);
  }));
}

function checkpointBlock(checkpoint) {
  if (!checkpoint) return el("p", { class: "resident-muted", text: "No checkpoint yet." });
  const facts = [
    fact("Cycle", `${checkpoint.cycle || "-"} (${checkpoint.outcome || "-"})`),
    fact("Wake", `${list(checkpoint.wake?.reasons)} · ${checkpoint.wake?.digest || "-"}`),
    fact("Actions", String(checkpoint.actions?.length || 0)),
    fact("Next wake", checkpoint.next_wake ? `${checkpoint.next_wake.reason} after ${when(checkpoint.next_wake.not_before)}` : "-"),
  ];
  return el("div", { class: "resident-checkpoint" }, [
    checkpoint.notes ? el("p", { class: "resident-notes", text: checkpoint.notes }) : null,
    el("div", { class: "resident-facts" }, facts),
  ]);
}

function requestRows(payload, mandate) {
  const requests = mandate.open_requests || [];
  if (!requests.length) return null;
  return el("div", { class: "resident-requests" }, requests.map(request => {
    const ask = request.resident_grant;
    const text = ask
      ? `Asks for +${list(ask.rights)}${ask.workspaces?.length ? ` in ${list(ask.workspaces)}` : ""}, relayed by ${ask.relay?.by || request.raised_by}`
      : request.summary || request.gate;
    const answers = (request.answers || []).map(answer => {
      const id = answer.id || answer;
      const action = button(answer.label || id, `${answer.label || id} request ${request.id}`);
      action.addEventListener("click", () => write(
        `/api/decisions/${encodeURIComponent(request.id)}/answer`,
        { answer: id, note: "Answered from the dashboard Residents view" },
        `Answered ${request.id}: ${id}.`,
        [action],
      ));
      return gated(payload, "resident_write", action);
    });
    return el("div", { class: "resident-request" }, [
      el("span", { class: "resident-request-id", text: request.id }),
      el("span", { class: "resident-request-text", text }),
      el("span", { class: "resident-actions" }, answers),
      ask?.quote ? el("q", { class: "resident-quote", text: ask.quote }) : null,
    ]);
  }));
}

function authorityForm(payload, mandate) {
  const effective = mandate.effective_authority || {};
  const grantRights = rightBoxes("add", "Rights to add");
  const narrowRights = rightBoxes("remove", "Rights to remove");
  const quote = input("quote", { placeholder: "Your words, kept with the grant" });
  const grant = button("Widen", `Widen ${mandate.task_id}`);
  const narrow = button("Narrow", `Narrow ${mandate.task_id}`);
  const form = el("form", { class: "resident-form resident-authority-form", "aria-label": `Authority for ${mandate.task_id}` }, [
    el("div", { class: "resident-authority-column" }, [
      grantRights,
      field("Add workspaces", input("workspaces"), "Comma-separated."),
      field("Quote", quote, "Required to widen."),
      gated(payload, "resident_write", grant),
    ]),
    el("div", { class: "resident-authority-column" }, [
      narrowRights,
      field("Drop workspaces", input("drop_workspaces", { placeholder: list(effective.workspaces) })),
      field("Deadline", input("deadline", { type: "datetime-local" }), "Shortens the mandate; applied as a separate narrow change."),
      gated(payload, "resident_write", narrow),
    ]),
  ]);
  form.addEventListener("submit", event => event.preventDefault());
  const path = `/api/residents/${encodeURIComponent(mandate.task_id)}/grant`;
  grant.addEventListener("click", async () => {
    const values = form.elements;
    const add = checkedRights(form, "add");
    const workspaces = csv(values.workspaces.value);
    const quoteValue = values.quote.value.trim();
    const deadline = values.deadline.value ? new Date(values.deadline.value).toISOString() : null;
    const hasWiden = add.length > 0 || workspaces.length > 0;
    // A deadline with a widening goes through the single grant endpoint,
    // which runs two core operations and reports a refused narrowing
    // without hiding whether the widening applied.
    if (hasWiden && deadline) {
      await write(path, { add, workspaces, quote: quoteValue, deadline }, (result) => {
        if (result?.outcome === "granted_narrowing_refused") {
          return `Widened ${mandate.task_id}; recorded as your answered request. Narrowing refused: ${result.narrowing_error || "unknown reason"}`;
        }
        if (result?.outcome === "unchanged_narrowing_refused") {
          return `No widening applied for ${mandate.task_id}: the requested rights and workspaces were already held, so no grant request was recorded. Narrowing refused: ${result.narrowing_error || "unknown reason"}`;
        }
        if (result?.outcome === "unchanged_and_narrowed") {
          return `No widening applied for ${mandate.task_id}: the requested rights and workspaces were already held, so no grant request was recorded. Shortened its deadline.`;
        }
        return `Widened ${mandate.task_id} and shortened its deadline; recorded as two changes.`;
      }, [grant, narrow]);
      return;
    }
    if (!hasWiden && deadline) {
      await write(path, { deadline }, `Shortened the deadline for ${mandate.task_id}.`, [grant, narrow]);
      return;
    }
    await write(path, { add, workspaces, quote: quoteValue }, (result) => result?.outcome === "unchanged"
      ? `No widening applied for ${mandate.task_id}: the requested rights and workspaces were already held, so no grant request was recorded.`
      : `Widened ${mandate.task_id}; recorded as your answered request.`, [grant, narrow]);
  });
  narrow.addEventListener("click", () => {
    const values = form.elements;
    const body = { remove: checkedRights(form, "remove"), drop_workspaces: csv(values.drop_workspaces.value) };
    if (values.deadline.value) body.deadline = new Date(values.deadline.value).toISOString();
    write(path, body, `Narrowed ${mandate.task_id}.`, [grant, narrow]);
  });
  return form;
}

// The controls each state allows, primary first. `ask` actions open a
// composer for the reason (or, for relay, the direction) before writing.
const CONTROL_ACTIONS = {
  active: ["wake", "pause", "takeover", "relay", "stop"],
  paused: ["resume", "takeover", "relay", "stop"],
  taken_over: ["release", "relay", "stop"],
  handed_off: ["relay"],
  invalid: ["relay"],
  ended: [],
};
const ACTION = {
  wake: { label: "Wake for status" },
  resume: { label: "Resume" },
  release: { label: "Release" },
  pause: { label: "Pause", ask: "Why pause it?" },
  takeover: { label: "Takeover", ask: "Why take it over?" },
  stop: { label: "Stop", ask: "Why stop it? Stopping ends the mandate.", variant: "danger" },
  relay: { label: "Relay", ask: "Direction for the resident, in your words" },
};

function control(mandate, action, reason, controls) {
  const id = mandate.task_id;
  const settled = () => { ui.pending = null; };
  if (action === "relay") {
    return write(`/api/residents/${encodeURIComponent(id)}/relay`, { quote: reason }, `Relayed to ${id}.`, controls, settled);
  }
  const body = { action, reason: reason || `${action} from the dashboard` };
  if (action === "wake") body.status_request = true;
  if (action === "takeover") body.by_session = "dashboard";
  return write(`/api/residents/${encodeURIComponent(id)}/control`, body, (result) => `${id} is now ${result.state || action}.`, controls, settled);
}

function composer(payload, mandate, action) {
  const spec = ACTION[action];
  const text = input(action === "relay" ? "relay" : "reason", {
    placeholder: action === "relay" ? "Relay a direction to the resident" : "Reason (kept in the history)",
    "aria-label": action === "relay" ? `Relay to ${mandate.task_id}` : `Reason for ${mandate.task_id}`,
  });
  text.value = ui.pending?.text || "";
  const confirm = button(action === "relay" ? "Send" : spec.label, `${spec.label} ${mandate.task_id}`, spec.variant || "primary");
  const cancel = button("Cancel", `Cancel ${spec.label.toLowerCase()}`);
  const unavailable = Boolean(actionReason(payload, "resident_write"));
  confirm.disabled = unavailable || !text.value.trim();
  text.addEventListener("input", () => {
    if (ui.pending) ui.pending.text = text.value;
    confirm.disabled = unavailable || !text.value.trim();
  });
  const form = el("form", { class: "resident-composer", "aria-label": `${spec.label} ${mandate.task_id}` }, [
    el("span", { class: "resident-field-label", text: spec.ask }),
    el("div", { class: "resident-control" }, [text, gated(payload, "resident_write", confirm), cancel]),
  ]);
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (!unavailable && text.value.trim()) control(mandate, action, text.value.trim(), [confirm]);
  });
  confirm.type = "submit";
  cancel.addEventListener("click", () => {
    ui.pending = null;
    rerender();
  });
  return form;
}

function controlRow(payload, mandate) {
  const actions = CONTROL_ACTIONS[mandate.state] || [];
  if (!actions.length) return null;
  const pending = ui.pending?.task === mandate.task_id ? ui.pending.action : null;
  const buttons = actions.map((action, index) => {
    const spec = ACTION[action];
    const variant = index === 0 ? "primary" : spec.variant || "";
    const label = spec.ask ? `${spec.label}…` : spec.label;
    const made = button(label, `${spec.ask ? "Choose " : ""}${spec.label} ${mandate.task_id}`, variant);
    if (pending === action) made.setAttribute("aria-pressed", "true");
    made.addEventListener("click", () => {
      if (!spec.ask) {
        control(mandate, action, "", [made]);
        return;
      }
      ui.pending = { task: mandate.task_id, action, text: "" };
      rerender();
      document.querySelector(`#resident-detail-${CSS.escape(mandate.task_id)} .resident-composer input`)?.focus();
    });
    return gated(payload, "resident_write", made);
  });
  return el("div", { class: "resident-detail-actions" }, [
    el("div", { class: "resident-actions" }, buttons),
    pending ? composer(payload, mandate, pending) : null,
  ]);
}

function mandateDetail(payload, mandate) {
  const effective = mandate.effective_authority || {};
  const body = mandate.mandate || {};
  const live = mandate.live_cycle;
  const warnings = [
    mandate.mandate_error,
    mandate.authority_error,
    mandate.cycle_routine_disabled ? `Cycle routine off: ${mandate.cycle_routine_name || "the resident cycle routine"} is disabled, so no cycle will fire until it is enabled.` : null,
    mandate.stalled ? `Stalled: ${mandate.stalled_reason || "no checkpoint within the heartbeat"}` : null,
  ].filter(Boolean);
  const facts = [
    fact("Rights", effective.rights?.length || mandate.state === "ended" ? list(effective.rights) : "none (observes only)"),
    fact("Workspaces", list(effective.workspaces)),
    fact("Crew", body.crew || "-"),
    fact("Cadence", body.cadence ? `floor ${body.cadence.floor_minutes}m · heartbeat ${body.cadence.heartbeat_minutes}m` : "-"),
    fact("Deadline", when(body.deadline || mandate.authority?.deadline)),
    fact("Live cycle", live ? `${live.cycle} (run ${live.run_id}) until ${when(live.expires_at)}` : "none"),
  ];
  const id = mandate.task_id;
  // detailsPanel keeps each disclosure's open state across refreshes.
  let authority = null;
  if (mandate.state !== "ended") {
    authority = detailsPanel(`resident-authority:${id}`, { class: "resident-disclosure resident-authority" });
    authority.append(el("summary", { text: "Change authority" }), authorityForm(payload, mandate));
  }
  const last = mandate.control?.last;
  const history = detailsPanel(`resident-history:${id}`, { class: "resident-disclosure" });
  history.append(...[
    el("summary", { text: "Exit criteria and history" }),
    body.exit_criteria?.length ? el("h4", { text: "Exit criteria" }) : null,
    body.exit_criteria?.length ? el("ul", {}, body.exit_criteria.map(item => el("li", { text: item }))) : null,
    el("h4", { text: "Authority history" }),
    historyList(mandate.authority),
    last ? el("h4", { text: "Last control" }) : null,
    last ? el("p", { class: "resident-muted", text: `${last.action} by ${last.by || "-"} · ${when(last.at)}${last.reason ? ` · ${last.reason}` : ""}` }) : null,
  ].filter(node => node != null));
  return el("section", { class: "resident-detail", "aria-label": `Detail for ${id}` }, [
    mandate.state_reason ? el("p", { class: "resident-muted", text: mandate.state_reason }) : null,
    ...warnings.map(text => el("p", { class: "resident-stalled", text })),
    requestRows(payload, mandate),
    controlRow(payload, mandate),
    el("div", { class: "resident-detail-grid" }, [
      el("div", {}, [el("h4", { text: "Mandate" }), el("div", { class: "resident-facts" }, facts)]),
      el("div", {}, [el("h4", { text: "Latest checkpoint" }), checkpointBlock(mandate.latest_checkpoint)]),
    ]),
    authority,
    history,
  ]);
}

// --- page -------------------------------------------------------------------------

function sortedLive(mandates) {
  return mandates
    .map((mandate, index) => ({ mandate, index }))
    .sort((a, b) => (needsOperator(b.mandate) ? 1 : 0) - (needsOperator(a.mandate) ? 1 : 0)
      || (STATE_ORDER[a.mandate.state] ?? 3) - (STATE_ORDER[b.mandate.state] ?? 3)
      || a.index - b.index)
    .map(entry => entry.mandate);
}

function mandateList(payload, mandates) {
  const items = [];
  for (const mandate of mandates) {
    items.push(mandateRow(payload, mandate));
    if (ui.selected === mandate.task_id) {
      const detail = mandateDetail(payload, mandate);
      detail.id = `resident-detail-${mandate.task_id}`;
      items.push(detail);
    }
  }
  return items;
}

function renderResidents(payload, deliberate = false) {
  const body = document.getElementById("residents-body");
  const count = document.getElementById("residents-count");
  if (!body) return;
  resetUiFor(captureWorkspaceVisit());
  ui.payload = payload;
  const mandates = payload?.mandates || [];
  const live = sortedLive(mandates.filter(mandate => mandate.state !== "ended"));
  const ended = mandates.filter(mandate => mandate.state === "ended");
  const attention = live.filter(needsOperator).length;
  if (count) count.textContent = String(live.length);
  setRailSubtabCount("rail-count-ops-residents", attention ? String(attention) : "");
  // A form the operator is typing in survives a background refresh; the
  // start flow also keeps its own node, so its values survive every render.
  // A deliberate render (Cancel, a submitted write) must not be dropped.
  if (!deliberate && body.querySelector("form :focus")) return;
  if (ui.selected && !mandates.some(mandate => mandate.task_id === ui.selected)) ui.selected = null;
  const empty = !mandates.length && !ui.startForm;
  const toolbar = el("div", { class: "resident-toolbar" }, [
    routineStatus(payload),
    !empty && !ui.startForm ? startButton(payload) : null,
  ]);
  const parts = [toolbar];
  if (ui.startForm) parts.push(ui.startForm);
  if (empty) {
    parts.push(el("div", { class: "empty-state resident-empty" }, [startButton(payload)]));
  } else if (live.length) {
    parts.push(el("div", { class: "resident-list" }, [listHead(), ...mandateList(payload, live)]));
  } else if (mandates.length) {
    parts.push(el("p", { class: "resident-muted resident-none", text: "No running residents." }));
  }
  if (ended.length) {
    const panel = detailsPanel("resident-ended", { class: "resident-disclosure resident-ended" });
    panel.append(el("summary", { text: `Ended (${ended.length})` }), el("div", { class: "resident-list" }, mandateList(payload, ended)));
    if (ended.some(mandate => mandate.task_id === ui.selected)) panel.open = true;
    parts.push(panel);
  }
  body.replaceChildren(...parts);
}

export async function fetchAndRenderResidents({ deliberate = false } = {}) {
  await requestPanel("residents-body", "residents", () => fetchJson("/api/residents?all=true"), payload => renderResidents(payload, deliberate), "residents-count");
}
