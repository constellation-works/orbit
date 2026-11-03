// Automation › Residents: set up, steer and stop resident mandates, and turn
// the workspace's resident cycle routine on or off. Every read and write goes
// through the `orbit.resident.*` tools behind /api/residents; the server keeps
// the mandate rules, so a refused form shows the server's own message.

import { captureWorkspaceVisit, el, fetchJson, requestJson, requestPanel } from './common.js';
import { actionReason, explainUnavailable, feedback, selectedWorkspaceName, setRailSubtabCount } from './operations-shared.js';

const FEEDBACK = "resident-operation-feedback";
const RIGHTS = ["dispatch", "promote", "merge", "plugin_upgrade"];
const STATE_STATUS = { active: "done", paused: "todo", taken_over: "in_progress", ended: "archived" };

const list = (values) => (values?.length ? values.join(", ") : "none");
const lines = (text) => String(text || "").split("\n").map(line => line.trim()).filter(Boolean);
const csv = (text) => String(text || "").split(",").map(part => part.trim()).filter(Boolean);
const when = (value) => (value ? String(value).replace("T", " ").replace(/\.\d+Z$/, "Z") : "none");

// `el` sets only class, text, title, role, type and aria-*; form controls
// also need their name, value and constraints.
const EL_HANDLED = new Set(["id", "class", "text", "title", "style", "role", "type"]);
function node(tag, attrs = {}, children = []) {
  const made = el(tag, attrs, children);
  for (const [name, value] of Object.entries(attrs)) {
    if (value == null || EL_HANDLED.has(name) || name.startsWith("aria-")) continue;
    made.setAttribute(name, String(value));
  }
  return made;
}

function fact(label, value) {
  return el("div", { class: "resident-fact" }, [
    el("span", { class: "resident-fact-label", text: label }),
    el("span", { class: "resident-fact-value", text: value }),
  ]);
}

// One write: pinned to the visit the operator acted in, refreshing the view
// on success and showing the server's message on refusal.
async function write(path, body, success, controls = []) {
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
  try {
    await fetchAndRenderResidents();
  } catch (error) {
    if (visit.isCurrent()) {
      document.getElementById(FEEDBACK)?.appendChild(el("span", {
        text: `\nRefresh failed: ${error.message || error}. The change was applied; refresh before acting again.`,
      }));
    }
  }
  return result;
}

function gated(payload, action, button) {
  const reason = actionReason(payload, action);
  if (!reason) return button;
  button.disabled = true;
  return explainUnavailable(button, reason);
}

function button(text, label) {
  return el("button", { class: "operation-button", type: "button", text, "aria-label": label || text });
}

function field(label, control, hint) {
  return el("label", { class: "resident-field" }, [
    el("span", { class: "resident-field-label", text: label }),
    control,
    hint ? el("span", { class: "resident-field-hint", text: hint }) : null,
  ]);
}

function input(name, attrs = {}) {
  return node("input", { name, type: "text", ...attrs });
}

function rightBoxes(prefix, legend, checked = []) {
  return el("fieldset", { class: "resident-rights" }, [
    el("legend", { text: legend }),
    ...RIGHTS.map(right => {
      const box = node("input", { type: "checkbox", name: `${prefix}-${right}`, value: right });
      box.checked = checked.includes(right);
      return el("label", { class: "resident-right" }, [box, el("span", { text: right })]);
    }),
  ]);
}

const checkedRights = (form, prefix) => RIGHTS.filter(right => form.elements[`${prefix}-${right}`]?.checked);

// --- routine -----------------------------------------------------------------

function routineRow(payload) {
  const routine = payload.routine;
  if (!routine) {
    return el("div", { class: "resident-routine" }, [
      el("span", { class: "resident-fact-label", text: "Cycle routine" }),
      el("span", { class: "resident-fact-value", text: "No resident cycle routine is loaded for this workspace. Without it, no cycle wakes a mandate." }),
    ]);
  }
  const state = el("span", { class: "pill", text: routine.enabled ? "enabled" : "disabled" });
  state.dataset.status = routine.enabled ? "done" : "todo";
  const toggle = button(routine.enabled ? "Disable" : "Enable", `${routine.enabled ? "Disable" : "Enable"} the resident cycle routine`);
  toggle.addEventListener("click", () => write(
    "/api/routines/toggle",
    {
      name: routine.name,
      source: routine.source,
      target: routine.target,
      machine_name: routine.machine_name,
      expected_enabled: routine.enabled,
      enabled: !routine.enabled,
    },
    `Resident cycle routine ${routine.enabled ? "disabled" : "enabled"}.`,
    [toggle],
  ));
  return el("div", { class: "resident-routine" }, [
    el("span", { class: "resident-fact-label", text: "Cycle routine" }),
    el("span", { class: "resident-routine-name", text: routine.name }),
    state,
    gated(payload, "routine_toggle", toggle),
  ]);
}

// --- create ------------------------------------------------------------------

function createForm(payload) {
  const crew = node("select", { name: "crew" }, (payload.crews || []).map(name => {
    const option = node("option", { value: name, text: name });
    option.selected = name === payload.default_crew;
    return option;
  }));
  const continuity = node("select", { name: "continuity" }, [
    node("option", { value: "prefer", text: "prefer" }),
    node("option", { value: "off", text: "off" }),
  ]);
  const complexity = node("select", { name: "complexity" }, ["low", "medium", "high"].map(level => {
    const option = node("option", { value: level, text: level });
    option.selected = level === "medium";
    return option;
  }));
  const submit = el("button", { class: "operation-button", type: "submit", text: "Create mandate" });
  const form = node("form", { class: "resident-form", "aria-label": "New resident mandate" }, [
    field("Goal", input("goal", { maxlength: "240", placeholder: "One line: what the resident keeps true" })),
    field("Exit criteria", node("textarea", { name: "exit_criteria", rows: "3" }), "One per line, 1–8."),
    field("Workspaces", input("workspaces", { value: selectedWorkspaceName() || "" }), "Comma-separated workspace names."),
    field("Watch tags", input("watch_tags"), "Comma-separated."),
    field("Watch tasks", input("watch_tasks"), "Comma-separated task ids."),
    field("Crew", crew),
    field("Floor minutes", input("floor_minutes", { type: "number", min: "1", value: "15" })),
    field("Heartbeat minutes", input("heartbeat_minutes", { type: "number", min: "1", value: "60" }), "At least the floor."),
    field("Deadline", input("deadline", { type: "datetime-local" }), "Optional."),
    field("Continuity", continuity),
    field("Title", input("title"), "Optional; defaults to the goal."),
    field("Complexity", complexity),
    el("div", { class: "resident-form-actions" }, [gated(payload, "resident_write", submit)]),
  ]);
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const values = form.elements;
    const mandate = {
      schema: "resident.mandate/v1",
      goal: values.goal.value.trim(),
      exit_criteria: lines(values.exit_criteria.value),
      workspaces: csv(values.workspaces.value),
      watch: { tasks: csv(values.watch_tasks.value), tags: csv(values.watch_tags.value) },
      crew: values.crew.value,
      cadence: {
        floor_minutes: Number(values.floor_minutes.value),
        heartbeat_minutes: Number(values.heartbeat_minutes.value),
      },
      continuity: values.continuity.value,
    };
    if (values.deadline.value) mandate.deadline = new Date(values.deadline.value).toISOString();
    write(
      "/api/residents",
      { title: values.title.value.trim(), complexity: values.complexity.value, mandate },
      (result) => `Created mandate ${result.task_id}. Grant it rights to let it act.`,
      [submit],
    ).then(result => { if (result) form.reset(); });
  });
  return el("details", { class: "resident-create" }, [el("summary", { text: "New mandate" }), form]);
}

// --- one mandate ---------------------------------------------------------------

function historyList(authority) {
  const entries = [
    ...(authority?.granted || []).map(entry => ({ ...entry, kind: "granted" })),
    ...(authority?.narrowed || []).map(entry => ({ ...entry, kind: "narrowed" })),
  ].sort((a, b) => (a.revision || 0) - (b.revision || 0));
  if (!entries.length) return el("p", { class: "resident-muted", text: "No authority changes yet. The mandate observes only." });
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
    fact("Wake digest", checkpoint.wake?.digest || "-"),
    fact("Wake reasons", list(checkpoint.wake?.reasons)),
    fact("Actions", String(checkpoint.actions?.length || 0)),
    fact("Next wake", checkpoint.next_wake ? `${checkpoint.next_wake.reason} after ${when(checkpoint.next_wake.not_before)}` : "-"),
  ];
  return el("div", { class: "resident-checkpoint" }, [
    el("div", { class: "resident-facts" }, facts),
    checkpoint.notes ? el("p", { class: "resident-notes", text: checkpoint.notes }) : null,
  ]);
}

function requestRows(payload, mandate) {
  const requests = mandate.open_requests || [];
  if (!requests.length) return null;
  return el("div", { class: "resident-requests" }, requests.map(request => {
    const ask = request.resident_grant;
    const text = ask
      ? `${request.id}: +${list(ask.rights)}; workspaces ${list(ask.workspaces)}, relayed by ${ask.relay?.by || request.raised_by}`
      : `${request.id}: ${request.summary || request.gate}`;
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
      el("span", { class: "resident-fact-label", text: "Open request" }),
      el("span", { class: "resident-fact-value", text }),
      ask?.quote ? el("q", { class: "resident-quote", text: ask.quote }) : null,
      el("span", { class: "resident-actions" }, answers),
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
  const form = node("form", { class: "resident-form resident-authority-form", "aria-label": `Authority for ${mandate.task_id}` }, [
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
    await write(path, { add, workspaces, quote: quoteValue }, `Widened ${mandate.task_id}; recorded as your answered request.`, [grant, narrow]);
  });
  narrow.addEventListener("click", () => {
    const values = form.elements;
    const body = { remove: checkedRights(form, "remove"), drop_workspaces: csv(values.drop_workspaces.value) };
    if (values.deadline.value) body.deadline = new Date(values.deadline.value).toISOString();
    write(path, body, `Narrowed ${mandate.task_id}.`, [grant, narrow]);
  });
  return el("details", { class: "resident-authority" }, [el("summary", { text: "Widen or narrow authority" }), form]);
}

const CONTROL_ACTIONS = {
  active: ["pause", "wake", "takeover", "stop"],
  paused: ["resume", "takeover", "stop"],
  taken_over: ["release", "stop"],
  ended: [],
};

function controlRow(payload, mandate) {
  const actions = CONTROL_ACTIONS[mandate.state] || [];
  if (!actions.length) return null;
  const reason = input("reason", { placeholder: "Reason (kept in the history)", "aria-label": `Reason for ${mandate.task_id}` });
  const buttons = actions.map(action => {
    const label = action === "wake" ? "Wake for status" : action[0].toUpperCase() + action.slice(1);
    const control = button(label, `${label} ${mandate.task_id}`);
    control.addEventListener("click", () => {
      const body = { action, reason: reason.value.trim() || `${action} from the dashboard` };
      if (action === "wake") body.status_request = true;
      if (action === "takeover") body.by_session = "dashboard";
      write(
        `/api/residents/${encodeURIComponent(mandate.task_id)}/control`,
        body,
        (result) => `${mandate.task_id} is now ${result.state || action}.`,
        [control],
      );
    });
    return gated(payload, "resident_write", control);
  });
  return el("div", { class: "resident-control" }, [reason, ...buttons]);
}

function relayRow(payload, mandate) {
  if (mandate.state === "ended") return null;
  const quote = input("relay", { placeholder: "Relay a direction to the resident", "aria-label": `Relay to ${mandate.task_id}` });
  const send = button("Relay", `Relay to ${mandate.task_id}`);
  send.addEventListener("click", () => write(
    `/api/residents/${encodeURIComponent(mandate.task_id)}/relay`,
    { quote: quote.value.trim() },
    `Relayed to ${mandate.task_id}.`,
    [send],
  ));
  return el("div", { class: "resident-control" }, [quote, gated(payload, "resident_write", send)]);
}

function mandateCard(payload, mandate) {
  const state = el("span", { class: "pill", text: mandate.state.replace("_", " ") });
  state.dataset.status = STATE_STATUS[mandate.state] || "todo";
  const effective = mandate.effective_authority || {};
  const body = mandate.mandate || {};
  const live = mandate.live_cycle;
  const facts = [
    fact("Rights", list(effective.rights)),
    fact("Workspaces", list(effective.workspaces)),
    fact("Deadline", when(body.deadline || mandate.authority?.deadline)),
    fact("Crew", body.crew || "-"),
    fact("Cadence", body.cadence ? `floor ${body.cadence.floor_minutes}m · heartbeat ${body.cadence.heartbeat_minutes}m` : "-"),
    fact("Open requests", String(mandate.open_requests?.length || 0)),
    fact("Live cycle", live ? `${live.cycle} (run ${live.run_id}) until ${when(live.expires_at)}` : "none"),
  ];
  const stalled = mandate.stalled
    ? el("p", { class: "resident-stalled", text: `Stalled: ${mandate.stalled_reason || "no checkpoint within the heartbeat"}` })
    : null;
  return el("article", { class: "resident-card", "aria-label": `Resident mandate ${mandate.task_id}` }, [
    el("header", { class: "resident-card-head" }, [
      el("span", { class: "resident-id", text: mandate.task_id }),
      state,
      mandate.stalled ? el("span", { class: "pill resident-stalled-pill", text: "stalled" }) : null,
    ]),
    el("p", { class: "resident-goal", text: mandate.goal || body.goal || mandate.title }),
    mandate.state_reason ? el("p", { class: "resident-muted", text: mandate.state_reason }) : null,
    mandate.mandate_error ? el("p", { class: "resident-stalled", text: mandate.mandate_error }) : null,
    stalled,
    el("div", { class: "resident-facts" }, facts),
    requestRows(payload, mandate),
    controlRow(payload, mandate),
    relayRow(payload, mandate),
    mandate.state === "ended" ? null : authorityForm(payload, mandate),
    el("details", { class: "resident-detail" }, [
      el("summary", { text: "Authority history and latest checkpoint" }),
      el("h4", { text: "Authority history" }),
      historyList(mandate.authority),
      el("h4", { text: "Latest checkpoint" }),
      checkpointBlock(mandate.latest_checkpoint),
      body.exit_criteria?.length ? el("h4", { text: "Exit criteria" }) : null,
      body.exit_criteria?.length ? el("ul", {}, body.exit_criteria.map(item => el("li", { text: item }))) : null,
    ]),
  ]);
}

function renderResidents(payload) {
  const body = document.getElementById("residents-body");
  const count = document.getElementById("residents-count");
  if (!body) return;
  const mandates = payload?.mandates || [];
  const attention = mandates.filter(mandate => mandate.stalled || mandate.open_requests?.length).length;
  if (count) count.textContent = String(mandates.length);
  setRailSubtabCount("rail-count-ops-residents", attention ? String(attention) : "");
  // A half-filled form survives a background refresh.
  if (body.querySelector(".resident-form :focus")) return;
  body.replaceChildren(routineRow(payload), createForm(payload));
  if (!mandates.length) {
    body.append(el("div", { class: "empty-state", text: "No resident mandates. Create one above, then grant it rights and enable the cycle routine." }));
    return;
  }
  for (const mandate of mandates) body.append(mandateCard(payload, mandate));
}

export async function fetchAndRenderResidents() {
  await requestPanel("residents-body", "residents", () => fetchJson("/api/residents"), renderResidents, "residents-count");
}
