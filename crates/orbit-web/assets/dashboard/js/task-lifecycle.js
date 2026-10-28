// Orbit dashboard task detail metadata: the meta block, the lifecycle and
// delivery-stage strips, the hands chain, the delivery card and the OS readout.

import { el, infoHint, isAggregateView, makeCopyButton, runHref } from './common.js';
import { elapsedLabel } from './task-runtime.js';
import { buildExecutionProvenance, formatExecutionLocation } from './distributed.js';
import { absTimeNode, canMutateTask, pullRequestLink } from './task-shared.js';
import { explicitCrewValue, resolvedCrewName } from './task-crew.js';
import { buildRiskChip, buildRiskDetail } from './task-riskiness.js';
import { buildCrewUpdateControl, buildStatusUpdateControl } from './task-status-controls.js';
import { buildRelationRow, drainDependencyIds, relationTargets } from './task-relations.js';
import * as taskFields from './task-fields.js';
import * as taskReadiness from './task-readiness.js';

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
  const levers = el("div", { class: "task-levers" });
  levers.appendChild(buildStatusUpdateControl(task, context));
  levers.appendChild(taskFields.buildComplexityUpdateControl(task, context));
  if (canMutateTask(task) && !isAggregateView()) {
    const control = buildCrewUpdateControl(task, context, { withChip: false });
    control.classList.add("task-chip-crew");
    const select = control.querySelector("select");
    select.removeAttribute("aria-label");
    select.id = `detail-${task.id}-crew`;
    const label = el("label", { text: "Crew" });
    label.htmlFor = select.id;
    const chip = el("span", { class: "task-chip task-crew-control" }, [label, select]);
    control.prepend(chip);
    levers.appendChild(control);
  } else {
    const crew = explicitCrewValue(task);
    levers.appendChild(el("span", { class: "task-chip task-chip-crew", title: "Crew" }, [
      el("span", { text: "Crew: " }),
      ...(crew ? [crew] : [resolvedCrewName(task), el("span", { class: "fact-note", text: " · default" })]),
    ]));
  }
  levers.appendChild(el("span", { class: "task-chip task-chip-host", title: "Runs on" }, [
    el("span", { class: "visually-hidden", text: "Runs on: " }),
    buildOsReadout(task),
  ]));
  const risk = buildRiskChip(task);
  if (risk) levers.appendChild(risk);
  if (showsEditor("tags")) levers.appendChild(el("div", { class: "task-levers-tags" }, [editor("tags")]));

  const head = el("div", { class: "task-meta-block" }, [levers]);
  const riskDetail = buildRiskDetail(task);
  if (riskDetail) head.appendChild(riskDetail);
  const lifecycle = buildLifecycleStrip(task.status);
  if (lifecycle) head.appendChild(lifecycle);
  const delivery = buildDeliveryCard(task, drain, context, { editor, showsEditor });
  if (delivery) head.appendChild(delivery);
  return head;
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
  const list = el("ol", { class: "task-lifecycle", "aria-label": "Lifecycle" });
  TASK_LIFECYCLE.forEach(([key, label], index) => {
    const state = index < at ? "past" : index === at ? "current" : "future";
    const step = el("li", { class: `task-lifecycle-step ${state}` }, [
      el("span", { class: "task-lifecycle-bar" }),
      el("span", { class: "task-lifecycle-label", text: label }),
    ]);
    if (state === "current") {
      step.dataset.status = key;
      step.setAttribute("aria-current", "step");
    }
    list.appendChild(step);
  });
  return list;
}
// The delivery stage the task's run is on, as a thinner second strip in the
// lifecycle strip's past/current/future language. The detail API derives
// `job_run_stage` from durable run and step state; a task with no readable
// run carries none and shows no strip. Rendered full-width at the end of the
// shared band, just above the tabs; at 375px a one-column strip cannot keep
// the current stage readable.
const RUN_ENDED_STATES = ["failed", "cancelled", "timeout", "interrupted"];

export function buildStageStrip(task) {
  const info = task.job_run_stage;
  if (!info || !["in-progress", "review"].includes(task.status)) return null;
  const ended = RUN_ENDED_STATES.includes(info.run_state);
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
  const classes = ["task-stage"];
  if (ended) classes.push("ended");
  if (awaiting) classes.push("awaiting");
  const list = el("ol", { class: classes.join(" "), "aria-label": "Delivery stage" });
  list.dataset.runState = info.run_state || "";
  const tooltip = [
    info.step_id,
    info.step_index != null && info.step_count != null ? `step ${info.step_index} of ${info.step_count}` : "",
  ].filter(Boolean).join(" · ");
  for (const stage of stages) {
    let text = stage.label;
    if (stage.state === "current" && !awaiting) {
      if (ended) {
        const stateLabel = info.run_state.charAt(0).toUpperCase() + info.run_state.slice(1);
        text = `${stateLabel} at ${stage.label}`;
      } else if (info.started_at) {
        const elapsed = elapsedLabel(Date.now() - Date.parse(info.started_at));
        if (elapsed) text = `${stage.label} · ${elapsed}`;
      }
    }
    let label = el("span", { class: "task-stage-label", text });
    if (stage.state === "current" && ended && task.job_run_id && task.job_run_navigable !== false) {
      const link = el("a", { class: "task-stage-label task-stage-link", text });
      link.href = runHref(String(task.job_run_id));
      label = link;
    }
    const step = el("li", { class: `task-stage-step ${stage.state}` }, [
      el("span", { class: "task-stage-bar" }),
      label,
    ]);
    if (stage.state === "current") {
      step.setAttribute("aria-current", "step");
      if (tooltip) step.title = tooltip;
    }
    list.appendChild(step);
  }
  return list;
}
// The verb each recorded hand did, in the order the task met them.
const TASK_HAND_VERBS = {
  created_by: "created",
  planned_by: "planned",
  implemented_by: "implemented",
  orchestrator: "orchestrated",
};
const AGENT_FAMILIES = ["claude", "codex", "gemini", "grok"];

function buildHandsChain(task) {
  const hands = TASK_PEOPLE_FIELDS
    .filter(([key]) => task[key] != null && task[key] !== "")
    .map(([key, label]) => {
      const name = String(task[key]);
      const family = AGENT_FAMILIES.find((agent) => name.toLowerCase().includes(agent));
      const hand = el("span", { class: "task-hand", title: `${label} ${name}` }, [
        el("span", { class: "task-hand-name", text: name }),
        el("span", { class: "task-hand-verb", text: ` ${TASK_HAND_VERBS[key]}` }),
      ]);
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
  if (showsEditor("not_before")) rows.push(row("schedule", "Scheduled", editor("not_before")));
  if (task.started_at) rows.push(row("started-at task-started-at", "Actual start", absTimeNode(context, task.started_at)));
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
    // Detail-only: the list row carries no `job_run_worktree`, so these rows
    // appear once the task detail read lands.
    const worktree = task.job_run_worktree;
    if (worktree) {
      rows.push(row("run-branch", "Branch", buildRunWorktreeValue(worktree.branch, "branch", worktree.unrecorded)));
      rows.push(row("run-worktree", "Worktree", buildRunWorktreeValue(worktree.worktree_path, "worktree path", worktree.unrecorded)));
    }
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
  if (readiness || drain) rows.push(row("readiness", "Readiness", taskReadiness.buildReadinessBlock(readiness, drain, task.workspace_id)));
  // ORB-00037: in the aggregate ("All workspaces") view each task carries its
  // owning workspace's filesystem location (home-abbreviated to ~ server-side);
  // show it in full here since the row only has room for the short name badge.
  if (task.workspace_root) {
    rows.push(row("location", "Location", el("span", { class: "value ws-location mono", text: task.workspace_root, title: task.workspace_root })));
  }
  return rows.length ? el("div", { class: "task-delivery-card" }, rows) : null;
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
    link.href = runHref(runId);
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

// The run's branch or worktree path on its execution machine, as the detail
// read's `job_run_worktree` reports it from the run's own record; a click
// copies the exact value. A value the run never recorded reads
// "unavailable", with the reason as the hint, never a derived guess.
function buildRunWorktreeValue(recorded, noun, unrecorded) {
  const value = el("span", { class: "value run-worktree-value" });
  if (recorded) {
    // Isolate the label's LTR path order while CSS uses RTL to elide its start.
    const copy = makeCopyButton(recorded, { class: "mono run-worktree-copy", text: `\u2066${recorded}\u2069`, title: recorded });
    copy.setAttribute("aria-label", `Copy the run's ${noun}: ${recorded}`);
    value.appendChild(copy);
    return value;
  }
  value.appendChild(el("span", { class: "fact-note", text: "unavailable" }));
  if (unrecorded) value.appendChild(infoHint(unrecorded));
  return value;
}
