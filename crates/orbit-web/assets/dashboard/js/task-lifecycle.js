// Orbit dashboard task detail metadata: the meta block, the lifecycle and
// delivery-stage strips, the hands chain, the delivery card and the OS readout.

import { el, infoHint } from './common.js';
import { elapsedLabel } from './task-runtime.js';
import { buildExecutionProvenance, formatExecutionLocation } from './distributed.js';
import { absTimeNode, canMutateTask, pullRequestLink, taskRunHref } from './task-shared.js';
import { crewReferenceBase, explicitCrewValue, resolvedCrewName } from './task-crew.js';
import { buildRiskChip } from './task-riskiness.js';
import { buildCrewUpdateControl, buildStatusUpdateControl } from './task-status-controls.js';
import { buildRelationRow, drainDependencyIds, relationTargets } from './task-relations.js';
import * as taskFields from './task-fields.js';
import * as taskReadiness from './task-readiness.js';
import * as progressStrip from './progress-strip.js';

// Who made and carried the task, recorded by the pipeline and so read-only.
const TASK_PEOPLE_FIELDS = [
  ["created_by", "Created by"],
  ["planned_by", "Planned by"],
  ["implemented_by", "Implemented by"],
  ["orchestrator", "Orchestrator"],
];
// The detail's metadata head: what the task is and where it stands.
// The levers an operator sets (status, complexity, crew, host, tags) share
// one row of chips; the lifecycle strip places the status on the way to done;
// the delivery card gathers the pull request, the run, the hands that carried
// the task and its relations to other tasks. The delivery-stage strip is
// appended to the shared band immediately before its tabs. The pane head
// carries only the id and title, so nothing here is repeated there, and the
// timestamps close the Overview.
export function buildTaskMeta(task, context, { editor, showsEditor, drain }) {
  const levers = el("div", { class: "task-levers task-levers-task" });
  const controls = el("div", { class: "task-levers-controls" });
  controls.appendChild(buildStatusUpdateControl(task, context));
  controls.appendChild(taskFields.buildComplexityUpdateControl(task, context));
  if (canMutateTask(task)) {
    const control = buildCrewUpdateControl(task, context, { withChip: false });
    control.classList.add("task-chip-crew");
    const select = control.querySelector("select");
    select.setAttribute("aria-label", "Crew");
    select.id = `detail-${task.id}-crew`;
    const chip = el("span", { class: "task-chip task-crew-control" }, [select]);
    control.prepend(chip);
    controls.appendChild(control);
    const resizeSelect = () => sizeCrewSelect(select);
    resizeSelect();
    select.addEventListener("change", resizeSelect);
    document.fonts?.ready.then(resizeSelect);
    // Wait until the detail is mounted so the selected label is measured with
    // its inherited font and container-query styles. The query container can
    // change the select's font without changing the select's fixed width.
    requestAnimationFrame(() => {
      if (!select.isConnected) return;
      resizeSelect();
      const queryContainer = select.closest(".task-levers-task");
      if (!queryContainer || typeof ResizeObserver !== "function") return;
      const observer = new ResizeObserver(() => {
        if (!select.isConnected) {
          observer.disconnect();
          return;
        }
        resizeSelect();
      });
      observer.observe(queryContainer);
    });
  } else {
    const crew = explicitCrewValue(task);
    controls.appendChild(el("span", { class: "task-chip task-chip-crew", title: "Crew" }, [
      el("span", { class: "visually-hidden", text: "Crew: " }),
      ...(crew ? [crew] : [resolvedCrewName(task), el("span", { class: "fact-note", text: " · default" })]),
    ]));
  }
  const hostPill = buildHostPill(task);
  if (hostPill) controls.appendChild(hostPill);
  const risk = buildRiskChip(task);
  if (risk) controls.appendChild(risk);
  levers.appendChild(controls);
  if (showsEditor("tags")) levers.appendChild(el("div", { class: "task-levers-tags" }, [editor("tags")]));

  const head = el("div", { class: "task-meta-block" }, [levers]);
  const lifecycle = buildLifecycleStrip(task.status);
  if (lifecycle) head.appendChild(lifecycle);
  const delivery = buildDeliveryCard(task, drain, context, { editor, showsEditor });
  if (delivery) head.appendChild(delivery);
  return head;
}

// Native select auto-sizing follows its widest option in some browsers. Size
// the task-detail crew pill to its selected label and keep it in sync on edits.
export function sizeCrewSelect(select) {
  if (!select.isConnected) return;
  const selected = select.selectedOptions?.[0];
  const context = document.createElement("canvas").getContext("2d");
  if (!selected || !context) return;
  context.font = getComputedStyle(select).font;
  select.style.width = `${Math.ceil(context.measureText(selected.textContent || "").width + 22)}px`;
}
// The statuses a task passes through on its way to done. Any other status
// (blocked, rejected, archived, someday) sits off this line and shows no strip.
const TASK_LIFECYCLE = [
  ["proposed", "Proposed"],
  ["backlog", "Backlog"],
  ["in-progress", "In progress"],
  ["review", "Review"],
  ["done", "Done"],
];

function buildLifecycleStrip(status) {
  const at = TASK_LIFECYCLE.findIndex(([key]) => key === status);
  if (at < 0) return null;
  return progressStrip.buildProgressStrip(TASK_LIFECYCLE.map(([, label], index) => ({
    label,
    // The current lifecycle step is where the task stands, not work in
    // progress: only the In progress step itself is in progress, and its
    // label already says so.
    state: index < at || status === "done" ? "done" : index === at ? "current" : "pending",
    current: index === at,
  })), { className: "task-lifecycle", label: "Lifecycle" });
}
// The detail API derives the run's delivery stage from durable run and step
// state. It uses the same presentation as lifecycle and delivery progress.
const RUN_ENDED_STATES = ["failed", "cancelled", "timeout", "interrupted"];
// A held run stopped without failing: it waits for an operator or a gate, so
// it neither counts elapsed time nor reads as in progress or failed.
const RUN_HELD_STATE = "held";

export function buildStageStrip(task) {
  const info = task.job_run_stage;
  if (!info || !["in-progress", "review"].includes(task.status)) return null;
  const ended = RUN_ENDED_STATES.includes(info.run_state);
  const held = info.run_state === RUN_HELD_STATE;
  const awaiting = info.run_state === "success" && task.status === "review";
  let stages = Array.isArray(info.stages) ? info.stages.map((stage) => ({ ...stage })) : [];
  if (awaiting) {
    stages = stages.map((stage) => ({ ...stage, state: "past" }));
    stages.push({ id: "awaiting_approval", label: "Awaiting approval", state: "current" });
  }
  if (!stages.length) {
    const label = info.stage_label || info.stage || info.step_id;
    if (!label) return null;
    stages = [{ id: info.stage || info.step_id, label, state: "current" }];
  }
  const tooltip = [
    info.step_id,
    info.step_index != null && info.step_count != null ? `step ${info.step_index} of ${info.step_count}` : "",
  ].filter(Boolean).join(" · ");
  const steps = stages.map((stage) => {
    let text = stage.label;
    if (stage.state === "current" && !awaiting) {
      if (ended || held) {
        const stateLabel = info.run_state.charAt(0).toUpperCase() + info.run_state.slice(1);
        text = `${stateLabel} at ${stage.label}`;
      } else if (info.started_at) {
        const elapsed = elapsedLabel(Date.now() - Date.parse(info.started_at));
        if (elapsed) text = `${stage.label} · ${elapsed}`;
      }
    }
    const current = stage.state === "current";
    const state = stage.state === "past" ? "done" : !current ? "pending"
      : awaiting ? "awaiting" : held ? "held" : ended ? (info.run_state === "cancelled" ? "cancelled" : "failed") : "active";
    return {
      label: text, state, current,
      hint: current ? tooltip : null,
      href: current && (ended || held) && task.job_run_id && task.job_run_navigable !== false
        ? taskRunHref(task, String(task.job_run_id)) : null,
    };
  });
  const list = progressStrip.buildProgressStrip(steps, { className: "task-stage", label: "Delivery stage" });
  list.dataset.runState = info.run_state || "";
  return list;
}
// The verb each recorded hand did. The stored fields keep today's order:
// created, planned, implemented, then the orchestrator, who oversees it all.
const TASK_HAND_VERBS = {
  created_by: "created",
  planned_by: "planned",
  implemented_by: "implemented",
  orchestrator: "orchestrated",
};
const AGENT_FAMILIES = ["claude", "codex", "gemini", "grok", "muse"];
// What a derived recovered hand's record was, and a review's verdict, as the
// hand's tooltip names them.
const RECOVERY_VIA = {
  final_recovery: "final recovery",
  blocked_task_triage: "blocked-task triage",
  claim_recovery: "claim recovery",
  operator_requeue: "operator requeue",
};
const REVIEW_VERDICTS = {
  accept: "accept",
  accept_with_fixes: "accept with fixes",
  reject: "reject",
  incomplete: "incomplete",
};

const present = (value) => value != null && value !== "";

// A hand's tooltip subject: its crew name with the recorded model beside it,
// else the raw record when the stored attribution establishes no crew.
function handSubject(crew, model, raw) {
  if (crew) return present(model) && model !== crew ? `${crew} (${model})` : crew;
  return present(model) ? String(model) : String(raw || "");
}

// The stored hands, each at the time it is known by: the creator and a
// planner at creation, the implementer at the task's first start. The
// creator merges with the orchestrator when they are one actor: the same
// model, since no creator crew is recorded. Every visible name is the crew the
// detail's `recorded_crews` establishes for that field; a field it omits
// shows its action alone, never the stored provider or model label.
function storedHands(task) {
  const created = Date.parse(task.created_at);
  const hands = [];
  const people = Object.fromEntries(TASK_PEOPLE_FIELDS);
  const crews = task.recorded_crews || {};
  const merged = present(task.created_by_model) && present(task.orchestrator)
    && task.created_by_model === task.orchestrator_model;
  const hand = (field, crew, extra) => ({ name: crew ? String(crew) : "", key: crew ? String(crew) : String(task[field] || ""), ...extra });
  if (present(task.created_by)) {
    const crew = crews.orchestrator || crews.created_by;
    hands.push(merged
      ? hand("created_by", crew, { verbs: [TASK_HAND_VERBS.created_by, TASK_HAND_VERBS.orchestrator], model: task.created_by_model, family: task.created_by, at: created,
          title: `Created and orchestrated by ${handSubject(crew, task.created_by_model, task.orchestrator)}` })
      : hand("created_by", crews.created_by, { verbs: [TASK_HAND_VERBS.created_by], model: task.created_by_model, family: task.created_by, at: created,
          title: `${people.created_by} ${handSubject(crews.created_by, task.created_by_model, task.created_by)}` }));
  }
  if (present(task.planned_by)) {
    hands.push(hand("planned_by", crews.planned_by, { verbs: [TASK_HAND_VERBS.planned_by], family: task.planned_by, at: created,
      title: `${people.planned_by} ${handSubject(crews.planned_by, null, task.planned_by)}` }));
  }
  if (present(task.implemented_by)) {
    hands.push(hand("implemented_by", crews.implemented_by, { verbs: [TASK_HAND_VERBS.implemented_by], family: task.implemented_by, at: Date.parse(task.started_at), implemented: true,
      title: `${people.implemented_by} ${handSubject(crews.implemented_by, null, task.implemented_by)}` }));
  }
  if (present(task.orchestrator) && !(merged && present(task.created_by))) {
    hands.push(hand("orchestrator", crews.orchestrator || crewReferenceBase(task.orchestrator), { verbs: [TASK_HAND_VERBS.orchestrator], model: task.orchestrator_model, at: Infinity,
      title: `${people.orchestrator} ${handSubject(crews.orchestrator || crewReferenceBase(task.orchestrator), task.orchestrator_model)}` }));
  }
  return hands;
}

// The detail read's `handled_by`: who piloted, reviewed and recovered the
// task, each from a durable record, oldest first. The server names the crew;
// the one human a hand may show without a crew is an operator, and an
// unrecorded hand shows its action alone.
function derivedHands(task) {
  const list = Array.isArray(task.handled_by) ? task.handled_by : [];
  return list.map((hand) => {
    const name = hand.crew ? crewReferenceBase(hand.crew) : hand.actor === "operator" ? "operator" : "";
    return {
      verbs: [hand.verb],
      name,
      key: name || String(hand.actor || hand.model || ""),
      model: hand.model,
      family: hand.family,
      at: Date.parse(hand.at),
      occurrence: hand,
    };
  });
}

function handTime(at) {
  return Number.isFinite(at) ? new Date(at).toISOString().slice(0, 16).replace("T", " ") + " UTC" : "";
}

function occurrenceLine(hand) {
  const parts = [handTime(Date.parse(hand.at))];
  if (hand.run_id) parts.push(`run ${hand.run_id}`);
  if (hand.verdict) parts.push(REVIEW_VERDICTS[hand.verdict] || hand.verdict);
  if (hand.fixed_findings) parts.push(`${hand.fixed_findings} finding(s) fixed`);
  if (hand.via) parts.push(RECOVERY_VIA[hand.via] || hand.via);
  return parts.filter(Boolean).join(" · ");
}

// Every hand in time order. An implementer with no recorded start goes just
// before the first review or recovery; consecutive hands of one actor doing
// one thing collapse into one with a count.
function orderedHands(task) {
  const derived = derivedHands(task);
  const hands = [...storedHands(task), ...derived];
  const later = derived.find((hand) => hand.verbs[0] !== "piloted");
  for (const hand of hands) {
    if (hand.implemented && Number.isNaN(hand.at)) hand.at = later ? later.at - 0.5 : Infinity;
    if (Number.isNaN(hand.at)) hand.at = -Infinity;
  }
  const ordered = hands.map((hand, index) => ({ hand, index }))
    .sort((a, b) => (a.hand.at === b.hand.at ? 0 : a.hand.at - b.hand.at) || (a.index - b.index))
    .map(({ hand }) => hand);
  const groups = [];
  for (const hand of ordered) {
    const last = groups[groups.length - 1];
    if (last && hand.occurrence && last.occurrences.length && last.key === hand.key && last.verbs.join() === hand.verbs.join()) {
      last.occurrences.push(hand.occurrence);
      continue;
    }
    groups.push({ ...hand, occurrences: hand.occurrence ? [hand.occurrence] : [] });
  }
  return groups;
}

function buildHandsChain(task) {
  const hands = orderedHands(task).map((group) => {
    const count = group.occurrences.length;
    const verb = group.verbs.join(" · ") + (count > 1 ? ` ×${count}` : "");
    const title = group.title || [
      `${handSubject(group.name, group.model, group.key)} ${group.verbs.join(" · ")}`.trim(),
      ...group.occurrences.map(occurrenceLine),
    ].join("\n");
    // A hand with no established crew shows its action alone.
    const hand = el("span", { class: "task-hand", title }, [
      ...(group.name ? [el("span", { class: "task-hand-name", text: group.name })] : []),
      el("span", { class: "task-hand-verb", text: group.name ? ` ${verb}` : verb }),
    ]);
    // The colour follows the recorded provider or model, never the label.
    const family = AGENT_FAMILIES.find((agent) => agent === group.family)
      || AGENT_FAMILIES.find((agent) =>
        [group.family, group.model, group.name].some((value) => present(value) && String(value).toLowerCase().includes(agent)));
    if (family) hand.dataset.agent = family;
    return hand;
  });
  if (!hands.length) return null;
  const chain = hands.flatMap((hand, index) =>
    index ? [el("span", { class: "task-hand-sep", "aria-hidden": "true" }), hand] : [hand]);
  return el("span", { class: "value task-hands" }, chain);
}
function buildDeliveryCard(task, drain, context, { editor, showsEditor }) {
  const row = (kind, label, value) =>
    el("div", { class: `property-row fact task-delivery-row ${kind}` }, [el("span", { class: "label", text: label }), value]);
  const rows = [];
  if (task.started_at) {
    const schedule = el("span", { class: "value" });
    if (task.not_before) schedule.append("scheduled ", absTimeNode(task.not_before));
    else schedule.append("not scheduled");
    schedule.append(" · started ", absTimeNode(task.started_at));
    rows.push(row("schedule", "Scheduled", schedule));
  } else if (showsEditor("not_before")) {
    rows.push(row("schedule", "Scheduled", editor("not_before")));
  }
  const pullRequest = pullRequestLink(task);
  if (pullRequest || task.pr_number) {
    const value = el("span", { class: "value" });
    if (pullRequest) {
      const link = el("a", { class: "mono task-delivery-pr", text: pullRequest.number ? `#${pullRequest.number}` : "Pull request" });
      link.href = pullRequest.url;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      value.appendChild(link);
    } else {
      value.appendChild(el("span", { class: "mono", text: `#${task.pr_number}` }));
    }
    if (task.pr_status) value.appendChild(el("span", { class: "fact-note", text: ` · ${task.pr_status}` }));
    rows.push(row("pr", "Pull request", value));
  }
  if (task.job_run_id != null && task.job_run_id !== "") {
    rows.push(row("run", "Run", buildRunValue(task)));
  }
  const hands = buildHandsChain(task);
  if (hands) rows.push(row("hands", "Handled by", hands));
  // One row per relation type; its ids sit inline and each opens its task.
  // A blocked_by target the Readiness card already lists under "Depends on"
  // stays there only, so the same id is never shown twice.
  const covered = drainDependencyIds(drain);
  const relations = relationTargets(Array.isArray(task.relations) ? task.relations : []);
  for (const { key, label, targets } of relations) {
    const visible = key === "blocked_by" ? targets.filter((target) => !covered.has(target)) : targets;
    if (!visible.length) continue;
    rows.push(buildRelationRow(key, label, visible, context));
  }
  const readiness = taskReadiness.readinessOf(task);
  if (readiness || drain) rows.push(row("readiness", "Readiness", taskReadiness.buildReadinessBlock(readiness, drain, task.workspace_id, task, context)));
  // ORB-00037: in the aggregate ("All workspaces") view each task carries its
  // owning workspace's filesystem location (home-abbreviated to ~ server-side);
  // show it in full here since the row only has room for the short name badge.
  if (task.workspace_root) {
    rows.push(row("location", "Location", el("span", { class: "value ws-location mono", text: task.workspace_root, title: task.workspace_root })));
  }
  return rows.length ? el("div", { class: "card card-flush card-raised task-delivery-card" }, rows) : null;
}
// Where the task may run, as a word or two: "any", or the operating systems
// (macOS, Linux). The full sentence — which crew or tags decided it — is the
// readout's hint.
const OS_NAMES = { macos: "macOS", linux: "Linux", windows: "Windows" };

export function osReadoutText(task) {
  const requirement = task.os_requirement;
  if (!requirement) return "any";
  if (["unresolved", "conflict"].includes(requirement.state)) return requirement.state;
  const anyOf = Array.isArray(requirement.any_of) ? requirement.any_of : [];
  return anyOf.length ? anyOf.map((os) => OS_NAMES[os] || os).join(" or ") : "any";
}

export function osReadoutNote(task) {
  return task.os_requirement?.describe || (task.crew ? "any host" : "any host from OS tags (crew unassigned)");
}

function buildOsReadout(task) {
  const readout = el("span", { class: "value" }, [el("span", { class: "task-os-readout", text: osReadoutText(task) })]);
  readout.appendChild(infoHint(osReadoutNote(task)));
  return readout;
}

// A host pill appears only for a constrained task. Keep it shared between the
// initial detail and retained-draft refresh so both render the same value and
// explanatory hint.
export function buildHostPill(task) {
  if (osReadoutText(task).toLowerCase() === "any") return null;
  return el("span", { class: "task-chip task-chip-host", title: "Runs on" }, [
    el("span", { class: "visually-hidden", text: "Runs on: " }),
    buildOsReadout(task),
  ]);
}

// ORB-12516: a pulled task's run lives in the executing machine's own job
// store, so the id alone is a dangling reference. When the recorded execution
// machine is this one the link still resolves; otherwise name the machine to
// inspect rather than linking somewhere it is not.
function buildRunValue(task) {
  const runId = String(task.job_run_id);
  const machine = task.job_run_machine;
  const known = Boolean(machine && machine.machine_id);
  const value = el("span", { class: "value run-value" });
  if (task.job_run_navigable !== false) {
    const link = el("a", { class: "mono", text: runId });
    link.href = taskRunHref(task, runId);
    value.appendChild(link);
  } else {
    value.appendChild(el("span", { class: "mono", text: runId }));
  }
  // "on <machine>" follows the id on its line; what the id cannot do here (no
  // owner-local run) and an unrecorded machine are the hint.
  if (known) {
    // The space keeps the machine from reading as part of the run id.
    value.appendChild(document.createTextNode(" "));
    value.appendChild(buildExecutionProvenance(
      { known: true, machine_id: machine.machine_id, machine_name: machine.machine_name || null },
      { runId, workspace: task.workspace_id },
    ));
  }
  const notes = [];
  if (task.job_run_navigable === false) notes.push("No owner-local run for this id — inspect it on its execution machine.");
  if (!known) notes.push(`Execution machine ${formatExecutionLocation(null)}.`);
  if (notes.length) value.appendChild(infoHint(notes.join("\n")));
  return value;
}
