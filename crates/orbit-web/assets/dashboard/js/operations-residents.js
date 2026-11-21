// Tasks dock › Residents: start, supervise, steer and stop resident mandates.
// The pane is a list of mandates, one row each, with the selected mandate's
// detail opening under its row; one start flow creates a mandate, grants its
// rights and turns the workspace's cycle routine on. Every read and write goes
// through the `orbit.resident.*` tools behind /api/residents; the server keeps
// the mandate rules, so a refused write shows the server's own message.

import { captureWorkspaceVisit, detailsPanel, el, escCloseButton, fetchJson, infoHint, isAggregateView, makeToggleRow, onWorkspaceChange, renderPanelPlaceholder, requestJson, requestPanel, taskHref } from './common.js';
import { actionReason, explainUnavailable, feedback, selectedWorkspaceName } from './operations-shared.js';

const FEEDBACK = "resident-operation-feedback";
const RIGHTS = ["dispatch", "promote", "merge", "plugin_upgrade"];
const STATE_STATUS = { active: "done", paused: "todo", taken_over: "in_progress", ended: "archived" };
const STATE_ORDER = { active: 0, taken_over: 1, paused: 2 };

const list = (values) => (values?.length ? values.join(", ") : "none");
const lines = (text) => String(text || "").split("\n").map(line => line.trim()).filter(Boolean);
const csv = (text) => String(text || "").split(",").map(part => part.trim()).filter(Boolean);
const when = (value) => (value ? String(value).replace("T", " ").replace(/\.\d+Z$/, "Z") : "none");

// Pane state that is the operator's, not the payload's: the selected mandate,
// the open start flow and a chosen action awaiting its reason. A background
// refresh rebuilds the pane from it; a workspace switch clears it.
const ui = { revision: null, payload: null, selected: null, startForm: null, pending: null };

function resetUiFor(visit) {
  if (ui.revision === visit.revision) return;
  closeResidentStart();
  Object.assign(ui, { revision: visit.revision, payload: null, selected: null, startForm: null, pending: null });
}


function closeResidentStart() {
  const dialog = document.getElementById("resident-start-dialog");
  if (dialog?.open) dialog.close();
  ui.startForm = null;
}

onWorkspaceChange(() => {
  resetUiFor(captureWorkspaceVisit());
  const count = document.getElementById("dock-residents-state");
  if (count) count.textContent = "";
});
window.addEventListener("hashchange", () => {
  if (!document.querySelector('.tab-pane[data-tab="tasks"].active #side-dock[data-mode="residents"]')) closeResidentStart();
});


function fact(label, value, title = "") {
  return el("div", { class: "resident-fact" }, [
    el("span", { class: "resident-fact-label", text: label }),
    el("span", { class: "resident-fact-value", text: value, title: title || null }),
  ]);
}

function clock(value) {
  return value ? String(value).replace("T", " ").replace(/\.\d+Z$/, "Z") : "";
}

// Open decisions when the card read succeeded; grant requests when it did not
// or this server does not send the field yet.
function openDecisionList(mandate) {
  return Array.isArray(mandate.open_decisions) ? mandate.open_decisions : (mandate.open_requests || []);
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

let residentHintSeq = 0;

function field(label, control, hint) {
  if (!hint) {
    return el("label", { class: "resident-field" }, [
      el("span", { class: "resident-field-label", text: label }),
      control,
    ]);
  }
  residentHintSeq += 1;
  const hintId = `resident-field-hint-${residentHintSeq}`;
  const tip = infoHint(hint, { id: hintId });
  if (control?.setAttribute) control.setAttribute("aria-describedby", hintId);
  return el("label", { class: "resident-field" }, [
    el("span", { class: "resident-field-label", text: label }, [tip]),
    control,
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
  const start = button("Start", "Start a resident", "primary");
  start.setAttribute("aria-haspopup", "dialog");
  start.addEventListener("click", () => {
    ui.startForm = startForm(payload);
    const dialog = document.getElementById("resident-start-dialog");
    if (!dialog) return;
    dialog.replaceChildren(ui.startForm);
    dialog.onclose = () => {
      if (dialog.open) return;
      ui.startForm = null;
      // A refresh may have replaced the button that opened the dialog.
      // Restore focus to its current instance instead of a hidden input.
      document.querySelector('.tab-pane[data-tab="tasks"].active #side-dock[data-mode="residents"] #residents-body button[aria-haspopup="dialog"]')?.focus();
    };
    dialog.oncancel = event => {
      // Match the disabled Cancel button while a start write is pending.
      if (ui.startForm?.querySelector('[type="submit"]')?.disabled) event.preventDefault();
    };
    dialog.showModal();
    ui.startForm.querySelector('[name="goal"]')?.focus();
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
    el("header", { class: "dialog-header" }, [el("h3", { id: "resident-start-heading", class: "resident-start-title", text: "Start a resident" }), escCloseButton("Close (Esc)", { onClick: () => { if (!submit.disabled) closeResidentStart(); } })]),
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
      field("Heartbeat minutes", input("heartbeat_minutes", { type: "number", min: "1", value: "60" }), "The longest it goes without a cycle."),
      field("Floor minutes", input("floor_minutes", { type: "number", min: "1" }), "Optional; defaults to the heartbeat. Set it lower only to react to changes sooner."),
      field("Deadline", input("deadline", { type: "datetime-local" }), "Optional."),
      field("Crew", crew),
      field("Continuity", continuity),
      field("Complexity", complexity),
      field("Title", input("title"), "Optional; defaults to the goal."),
      routineNote(payload),
    ]),
    el("div", { class: "resident-form-actions dialog-actions" }, [status, cancel, gated(payload, "resident_write", submit)]),
  ]);
  const body = el("div", { class: "dialog-body" });
  for (const step of form.querySelectorAll(".resident-step")) body.append(step);
  form.insertBefore(body, form.querySelector(".dialog-actions"));
  cancel.addEventListener("click", () => {
    closeResidentStart();
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
    cadence: { heartbeat_minutes: Number(values.heartbeat_minutes.value) },
    continuity: values.continuity.value,
  };
  if (values.floor_minutes.value.trim()) mandate.cadence.floor_minutes = Number(values.floor_minutes.value);
  if (values.deadline.value) mandate.deadline = new Date(values.deadline.value).toISOString();

  const visit = captureWorkspaceVisit();
  for (const control of controls) control.disabled = true;
  status.textContent = "Creating the mandate…";
  const created = await post("/api/residents", { title: values.title.value.trim(), complexity: values.complexity.value, mandate }, visit);
  if (created.stale) return;
  if (!created.ok) {
    for (const control of controls) control.disabled = false;
    status.textContent = refused(0, created.message, "Nothing was created.");
    return;
  }
  const id = created.result.task_id;
  const finish = async (kind, message) => {
    closeResidentStart();
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
  const requests = openDecisionList(mandate).length;
  return [
    requests ? `${requests} request${requests === 1 ? "" : "s"}` : null,
    mandate.mandate_error || mandate.authority_error ? "invalid mandate" : null,
    mandate.stalled ? "stalled" : null,
  ].filter(Boolean).join(" · ").replace(/^./, first => first.toUpperCase());
}

// "12m ago", "3h ago": how long since a status line was posted.
function ago(value) {
  const at = Date.parse(value);
  if (Number.isNaN(at)) return "";
  const minutes = Math.max(0, Math.round((Date.now() - at) / 60000));
  if (minutes < 1) return "just now";
  return `${minutes < 60 ? `${minutes}m` : minutes < 2880 ? `${Math.round(minutes / 60)}h` : `${Math.round(minutes / 1440)}d`} ago`;
}

// The mandate's status line: the one info notice its cycle replaces each
// time. A line past its expiry (twice the heartbeat) is marked silent, so a
// resident that stopped cycling does not read as alive behind its last words.
function statusLine(mandate) {
  const line = mandate.status_line;
  if (!line?.title) return { text: "No status line yet", stale: false, empty: true };
  const posted = ago(line.posted_at);
  const lapsed = Date.parse(line.expires_at) <= Date.now();
  const stale = mandate.state !== "ended" && (line.stale === true || lapsed);
  return {
    text: stale ? `Silent · last status ${posted}: ${line.title}` : `${line.title}${posted ? ` · ${posted}` : ""}`,
    stale,
    empty: false,
  };
}

function wakeReason(reason) {
  const known = {
    heartbeat: "heartbeat",
    status_request: "status request",
    digest_changed: "watched state changed",
    deadline: "deadline",
  };
  const text = String(reason || "").trim();
  if (!text) return "";
  return known[text] || text.replaceAll("_", " ");
}

function spanLabel(mins) {
  return mins < 60 ? `${mins}m` : mins < 2880 ? `${Math.round(mins / 60)}h` : `${Math.round(mins / 1440)}d`;
}

// What happens next, and when. An inactive mandate does not pretend to have a wake.
function nextWake(payload, mandate) {
  if (mandate.state !== "active") {
    const why = mandate.state === "ended"
      ? "The mandate has ended."
      : (mandate.state_reason || `The mandate is ${mandate.state.replaceAll("_", " ")}.`);
    return { text: "Will not wake", title: why };
  }
  if (mandate.live_cycle) return { text: "Running now", title: "" };
  if (!payload.routine?.enabled) return { text: "Will not wake", title: "The cycle routine is off, so no cycle wakes this mandate." };
  const wake = mandate.latest_checkpoint?.next_wake;
  if (!wake?.not_before) return { text: "Wakes next routine tick", title: "No wake time is recorded." };
  const at = Date.parse(wake.not_before);
  if (Number.isNaN(at)) return { text: "Wakes at an unreadable time", title: wakeReason(wake.reason) };
  const minutes = Math.round((at - Date.now()) / 60000);
  const whenText = minutes > 0 ? `Wakes in ${spanLabel(minutes)}` : minutes === 0 ? "Wakes now" : `Was due ${spanLabel(-minutes)} ago`;
  const reason = wakeReason(wake.reason);
  return { text: reason ? `${whenText} · ${reason}` : whenText, title: clock(wake.not_before) };
}

function stateLabel(mandate) {
  const state = el("span", { class: "pill", text: mandate.state.replace("_", " ") });
  state.dataset.status = STATE_STATUS[mandate.state] || "todo";
  return state;
}

function endedMeta(mandate) {
  if (mandate.state !== "ended") return null;
  const outcome = mandate.latest_checkpoint?.outcome;
  const at = mandate.control?.last?.at;
  const parts = [];
  if (outcome) parts.push(outcome);
  if (at) parts.push(clock(at));
  if (!parts.length) return null;
  return el("span", { class: "resident-row-ended", text: parts.join(" · ") });
}

function mandateRow(mandate) {
  const selected = ui.selected === mandate.task_id;
  const goal = mandate.goal || mandate.mandate?.goal || mandate.title || mandate.task_id;
  const status = statusLine(mandate);
  const open = openDecisionList(mandate).length;
  const meta = [
    el("span", { class: "resident-row-state" }, [stateLabel(mandate)]),
    el("span", { class: "resident-id", text: mandate.task_id }),
  ];
  if (open) {
    const pill = el("span", { class: "pill", text: `${open} open` });
    pill.dataset.status = "open";
    meta.push(pill);
  }
  const statusAttrs = { class: "resident-row-status", text: status.text };
  if (status.text.length > 72) statusAttrs.title = status.text;
  const row = el("div", {
    class: `resident-row${selected ? " selected" : ""}${mandate.state === "ended" ? " ended" : ""}`,
    "aria-label": `Resident ${goal}, ${mandate.state}, task ${mandate.task_id}. ${status.text}`,
  }, [
    el("span", { class: "resident-row-goal resident-goal", text: goal }),
    el("span", { class: "resident-row-meta" }, meta),
    el("span", statusAttrs),
    endedMeta(mandate),
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

function checkpointNotes(checkpoint) {
  const notes = checkpoint.notes;
  if (!notes) return null;
  if (notes.length <= 180) return el("p", { class: "resident-notes", text: notes });
  const panel = detailsPanel(`resident-notes:${checkpoint.cycle || checkpoint.mandate || "notes"}`, { class: "resident-disclosure" });
  panel.append(el("summary", { text: "Show notes" }), el("p", { class: "resident-notes", text: notes }));
  return panel;
}

function wakeLine(checkpoint) {
  const reasons = (checkpoint.wake?.reasons || []).map(wakeReason).filter(Boolean);
  const digest = checkpoint.wake?.digest;
  if (!reasons.length && !digest) return null;
  return el("p", { class: "resident-muted resident-wake" }, [
    el("span", { text: reasons.length ? `Woke for ${reasons.join(", ")}` : "Wake recorded" }),
    digest ? infoHint(`Wake digest ${digest}`) : null,
  ]);
}

function recordList(label, items, render, empty) {
  const rows = Array.isArray(items) ? items : [];
  return el("div", { class: "resident-records" }, [
    el("h4", { text: label }),
    rows.length
      ? el("ul", { class: "resident-record-list" }, rows.map(render))
      : el("p", { class: "resident-muted", text: empty }),
  ]);
}

function actionItem(action) {
  const parts = [action.kind, action.subject, action.result, action.ref].filter(Boolean);
  const line = el("li", { text: parts.join(" · ") || "Action" });
  if (action.key) line.title = action.key;
  return line;
}

function decisionItem(decision) {
  const parts = [decision.summary, decision.class].filter(Boolean);
  const line = el("li", { text: parts.join(" · ") || "Decision" });
  if (decision.id) line.title = decision.id;
  return line;
}

function costItem(cost) {
  const parts = [cost.kind, cost.what].filter(Boolean);
  if (Number.isFinite(Number(cost.minutes))) parts.push(`${cost.minutes}m`);
  if (cost.retries) parts.push(`${cost.retries} retries`);
  const line = el("li", { text: parts.join(" · ") || "Cost" });
  if (cost.signature) line.title = cost.signature;
  return line;
}

function checkpointRequestItem(request) {
  const label = [request.decision_request || request.id, request.blocking ? "blocking" : ""].filter(Boolean).join(" · ");
  return el("li", {}, [
    el("span", { text: label || "Request" }),
    request.task ? el("a", { class: "resident-id", href: taskHref(request.task), text: request.task }) : null,
  ]);
}

function checkpointBlock(checkpoint) {
  if (!checkpoint) {
    return el("div", { class: "resident-checkpoint" }, [
      el("p", { class: "resident-muted", text: "No checkpoint yet." }),
    ]);
  }
  const outcome = checkpoint.outcome || "Outcome not recorded";
  const probe = checkpoint.probe ? el("span", { class: "pill", text: "probe" }) : null;
  if (probe) probe.dataset.status = "in_progress";
  return el("div", { class: "resident-checkpoint" }, [
    el("p", { class: "resident-checkpoint-summary" }, [
      checkpoint.cycle ? el("span", { class: "resident-id", text: checkpoint.cycle }) : null,
      el("span", { text: checkpoint.cycle ? ` · ${outcome}` : outcome }),
      probe,
    ]),
    checkpoint.error ? el("p", { class: "resident-stalled", text: checkpoint.error }) : null,
    wakeLine(checkpoint),
    checkpointNotes(checkpoint),
    recordList("Actions", checkpoint.actions, actionItem, "No actions this cycle"),
    recordList("Decisions", checkpoint.decisions, decisionItem, "No decisions this cycle"),
    recordList("Costs", checkpoint.costs, costItem, "No costs this cycle"),
    recordList("Requests", checkpoint.requests, checkpointRequestItem, "No requests this cycle"),
  ]);
}

const GATE_LABEL = {
  resident_authority: "Grant",
  agent_request: "Decision",
  block_triage: "Block triage",
  pilot_admission: "Pilot",
  proposed_approval: "Proposed",
  review_approval: "Review",
  model_answer_review: "Model answer",
  script_grant: "Script",
};

function gateLabel(gate) {
  const name = typeof gate === "string" ? gate : "";
  return GATE_LABEL[name] || name.replaceAll("_", " ") || "Decision";
}

// Block-triage reject and land, and review approval, dispose of work the
// normal path cannot undo. Authority-tier grants are not this tier and keep
// their direct write.
function answerIsIrreversible(answer) {
  return Boolean(answer) && typeof answer === "object" && answer.risk_tier === "irreversible";
}

// The task decision card uses this prompt. Cancel sends nothing. A host
// without confirm cannot show the consequence, so the write is refused.
function confirmIrreversibleDecisionAnswer(label, consequence) {
  if (typeof window.confirm !== "function") return false;
  const detail = consequence ? `\n\n${consequence}` : "";
  return window.confirm(`Answer this decision with “${label}”?${detail}`);
}

function requestRows(payload, mandate) {
  const requests = openDecisionList(mandate);
  if (!requests.length) return null;
  return el("div", { class: "resident-requests" }, requests.map(request => {
    const ask = request.resident_grant;
    const text = ask
      ? `Asks for +${list(ask.rights)}${ask.workspaces?.length ? ` in ${list(ask.workspaces)}` : ""}, relayed by ${ask.relay?.by || request.raised_by}`
      : request.summary || gateLabel(request.gate);
    const answers = (request.answers || []).map(answer => {
      const id = answer.id || answer;
      const label = answer.label || id;
      const action = button(label, `${label} request ${request.id}`);
      if (answer.consequence && answer.consequence !== label) action.setAttribute("aria-description", answer.consequence);
      action.addEventListener("click", () => {
        if (answerIsIrreversible(answer) && !confirmIrreversibleDecisionAnswer(label, answer.consequence)) {
          if (typeof window.confirm !== "function") {
            feedback(FEEDBACK, "error", "Refused: an irreversible answer needs a confirmation dialog before it is sent.");
          }
          return;
        }
        write(
          `/api/decisions/${encodeURIComponent(request.id)}/answer`,
          { answer: id, note: "Answered from the dashboard Residents view" },
          `Answered ${request.id}: ${id}.`,
          [action],
        );
      });
      return gated(payload, "resident_write", action);
    });
    const state = request.state || "open";
    const pill = el("span", { class: "pill", text: state });
    pill.dataset.status = state;
    return el("div", { class: "resident-request" }, [
      el("span", { class: "resident-request-head" }, [
        pill,
        el("span", { class: "resident-request-gate", text: gateLabel(request.gate) }),
        el("span", { class: "resident-request-id", text: request.id }),
        request.task_id ? el("a", { class: "resident-id", href: taskHref(request.task_id), text: request.task_id }) : null,
      ]),
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
    el("div", { class: "resident-actions resident-controls" }, buttons),
    pending ? composer(payload, mandate, pending) : null,
  ]);
}

// The grant the live cycle is running with, or the last cycle's grant when
// none is live. An explicit null means the server looked and found nothing:
// do not invent it from an older checkpoint. A missing field is an older
// payload, and the checkpoint's own report is then the only record.
function recordedCycle(mandate) {
  const recorded = mandate.cycle_authority;
  if (recorded && typeof recorded === "object") return recorded;
  if (Object.prototype.hasOwnProperty.call(mandate, "cycle_authority")) return null;
  const authority = mandate.latest_checkpoint?.authority;
  if (!authority || typeof authority !== "object" || Array.isArray(authority)) return null;
  return {
    cycle: mandate.latest_checkpoint.cycle,
    live: false,
    rights: authority.rights || [],
    observe_only: Boolean(authority.observe_only),
    narrowed_by: authority.narrowed_by || null,
    reported: true,
  };
}

function sameAuthority(current, cycle) {
  const currentObserve = !(current.rights || []).length;
  const cycleObserve = Boolean(cycle.observe_only) || !(cycle.rights || []).length;
  if (currentObserve !== cycleObserve) return false;
  const sameList = (left, right) => {
    const a = [...(left || [])].sort();
    const b = [...(right || [])].sort();
    return a.length === b.length && a.every((value, index) => value === b[index]);
  };
  if (!sameList(current.rights, cycle.rights)) return false;
  if (!Array.isArray(cycle.workspaces)) return true;
  return sameList(current.workspaces, cycle.workspaces);
}

function cycleRan(cycle) {
  const observe = cycle.observe_only || !(cycle.rights || []).length;
  if (observe) return "observe-only";
  const rights = `with ${cycle.rights.join(", ")}`;
  if (!Array.isArray(cycle.workspaces)) return rights;
  const spaces = cycle.workspaces.length ? cycle.workspaces.join(", ") : "no workspaces";
  return `${rights} in ${spaces}`;
}

function narrowedNote(cycle) {
  const by = cycle?.narrowed_by;
  if (!by) return "";
  if (by === "deadline") return " The deadline had passed.";
  if (String(by).startsWith("notice:")) return ` Narrowed by notice ${String(by).slice("notice:".length)}.`;
  return ` Narrowed by ${by}.`;
}

function authorityEffect(mandate, current) {
  const cycle = recordedCycle(mandate);
  if (!cycle) {
    if (mandate.live_cycle) return "A cycle is running. Its granted authority is not recorded here.";
    if (mandate.latest_checkpoint) return "The last cycle's granted authority is not recorded. The next cycle uses this authority.";
    return "No cycle has run yet. The next cycle uses this authority.";
  }
  const same = sameAuthority(current, cycle);
  if (cycle.live && same) return "In effect for the cycle that is running.";
  if (cycle.live) return `Takes effect next cycle. This cycle is running ${cycleRan(cycle)}.${narrowedNote(cycle)}`;
  if (same) return "Next cycle uses this authority.";
  return `Last cycle ran ${cycleRan(cycle)}.${narrowedNote(cycle)} Next cycle uses this authority.`;
}

function rightsLabel(rights, ended) {
  if (rights?.length) return rights.join(", ");
  return ended ? "none" : "Observes only";
}

function cycleFact(mandate) {
  const live = mandate.live_cycle;
  if (live) return fact("Cycle", `Running until ${clock(live.expires_at)}`, `${live.cycle} · run ${live.run_id}`);
  const checkpoint = mandate.latest_checkpoint;
  if (checkpoint?.cycle) return fact("Cycle", `Last · ${checkpoint.cycle}`);
  return fact("Cycle", "No cycle running");
}

function statusExtra(mandate) {
  const line = mandate.status_line;
  if (!line?.title) return [];
  const status = statusLine(mandate);
  const nodes = [];
  const posted = line.posted_at ? `Posted ${clock(line.posted_at)}` : "";
  const lapse = line.expires_at ? `${status.stale ? "Lapsed" : "Lapses"} ${clock(line.expires_at)}` : "";
  const text = [posted, lapse].filter(Boolean).join(" · ");
  if (text) nodes.push(el("p", { class: "resident-muted", text }));
  if (line.body) {
    const panel = detailsPanel(`resident-status:${mandate.task_id}`, { class: "resident-disclosure" });
    panel.append(el("summary", { text: "Show full status" }), el("p", { class: "resident-notes", text: line.body }));
    nodes.push(panel);
  }
  return nodes;
}

function authorityBlock(mandate, effective) {
  const cycle = recordedCycle(mandate);
  return el("div", { class: "resident-authority-now" }, [
    el("h4", { text: "Current authority" }),
    el("div", { class: "resident-facts" }, [
      fact("Rights", rightsLabel(effective.rights, mandate.state === "ended")),
      fact("Workspaces", list(effective.workspaces)),
      el("div", { class: "resident-fact" }, [
        el("span", { class: "resident-fact-label", text: "Effect" }),
        el("span", { class: "resident-fact-value resident-fact-prose" }, [
          el("span", { text: authorityEffect(mandate, effective) }),
          cycle?.reported ? infoHint("Taken from the checkpoint. The cycle grant was not recorded.") : null,
        ]),
      ]),
    ]),
  ]);
}

function mandateDetail(payload, mandate) {
  const effective = mandate.effective_authority || {};
  const body = mandate.mandate || {};
  const wake = nextWake(payload, mandate);
  const last = mandate.control?.last;
  const warnings = [
    mandate.mandate_error,
    mandate.authority_error,
    mandate.cycle_routine_disabled ? `Cycle routine off: ${mandate.cycle_routine_name || "the resident cycle routine"} is disabled, so no cycle will fire until it is enabled.` : null,
    mandate.stalled ? `Stalled: ${mandate.stalled_reason || "no checkpoint within the heartbeat"}` : null,
  ].filter(Boolean);
  const facts = [
    fact("Next wake", wake.text, wake.title),
    fact("Deadline", clock(body.deadline || mandate.authority?.deadline) || "No deadline"),
    cycleFact(mandate),
    fact("Crew", body.crew || "No crew"),
    fact("Cadence", body.cadence ? `floor ${body.cadence.floor_minutes}m · heartbeat ${body.cadence.heartbeat_minutes}m` : "No cadence"),
  ];
  if (last) facts.push(fact("Last control", `${last.action} by ${last.by || "unknown"} · ${clock(last.at)}${last.reason ? ` · ${last.reason}` : ""}`));
  const id = mandate.task_id;
  // detailsPanel keeps each disclosure's open state across refreshes.
  let authority = null;
  if (mandate.state !== "ended") {
    authority = detailsPanel(`resident-authority:${id}`, { class: "resident-disclosure resident-authority" });
    authority.append(el("summary", { text: "Change authority" }), authorityForm(payload, mandate));
  }
  const history = detailsPanel(`resident-history:${id}`, { class: "resident-disclosure" });
  history.append(...[
    el("summary", { text: "Exit criteria and history" }),
    body.exit_criteria?.length ? el("h4", { text: "Exit criteria" }) : null,
    body.exit_criteria?.length ? el("ul", {}, body.exit_criteria.map(item => el("li", { text: item }))) : null,
    el("h4", { text: "Authority history" }),
    historyList(mandate.authority),
  ].filter(node => node != null));
  return el("section", { class: "resident-detail", "aria-label": `Detail for ${id}` }, [
    mandate.state_reason ? el("p", { class: "resident-muted", text: mandate.state_reason }) : null,
    ...warnings.map(text => el("p", { class: "resident-stalled", text })),
    el("div", { class: "resident-facts" }, facts),
    ...statusExtra(mandate),
    requestRows(payload, mandate),
    controlRow(payload, mandate),
    authorityBlock(mandate, effective),
    el("h4", { text: "Latest checkpoint" }),
    checkpointBlock(mandate.latest_checkpoint),
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
    items.push(mandateRow(mandate));
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
  const count = document.getElementById("dock-residents-state");
  if (!body) return;
  resetUiFor(captureWorkspaceVisit());
  ui.payload = payload;
  const mandates = payload?.mandates || [];
  const live = sortedLive(mandates.filter(mandate => mandate.state !== "ended"));
  const ended = mandates.filter(mandate => mandate.state === "ended");
  if (count) count.textContent = live.length ? String(live.length) : "";
  // A form the operator is typing in survives a background refresh; the
  // start dialog keeps its own node, so its values survive every render.
  // A deliberate render (Cancel, a submitted write) must not be dropped.
  if (!deliberate && body.querySelector("form :focus")) return;
  if (ui.selected && !mandates.some(mandate => mandate.task_id === ui.selected)) ui.selected = null;
  const toolbar = el("div", { class: "resident-toolbar" }, [routineStatus(payload), startButton(payload)]);
  const parts = [toolbar];
  if (live.length) {
    parts.push(el("div", { class: "resident-list" }, mandateList(payload, live)));
  } else {
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
  if (isAggregateView()) {
    renderPanelPlaceholder("residents-body");
    return;
  }
  await requestPanel("residents-body", "residents", () => fetchJson("/api/residents?all=true"), payload => renderResidents(payload, deliberate));
}
