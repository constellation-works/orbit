// Orbit dashboard task readiness, pilot warnings and operator records.

import { captureWorkspaceVisit, el, postJson, runHref, bindTaskLink } from './common.js';
import { drainReadinessFor, drainReadinessView } from './drain-waits.js';
import * as taskShared from './task-shared.js';
import * as taskDecision from './task-decision.js';

const OPERATOR_RECORD_REFUSAL_MS = 20000;

export function readinessOf(task) {
  const readiness = task && task.readiness;
  return readiness && typeof readiness === "object" ? readiness : null;
}

// Blocking gaps first. The sort is stable, so server order holds within a severity.
function readinessGaps(readiness) {
  const gaps = Array.isArray(readiness.gaps) ? readiness.gaps.filter((gap) => gap && gap.code) : [];
  return gaps.sort((a, b) => Number(b.severity === "blocking") - Number(a.severity === "blocking"));
}

export function isNotReady(task) {
  const readiness = readinessOf(task);
  return Boolean(readiness) && readiness.ready === false;
}

export function readinessSignature(task) {
  const readiness = readinessOf(task);
  if (!readiness) return "";
  const gaps = readinessGaps(readiness).map((gap) => `${gap.code}/${gap.severity}`).join(",");
  return `${readiness.ready}:${readiness.preparing ? "p" : ""}:${gaps}`;
}

// The row chip, in the os-badge slot: `preparing` while a pilot run holds the
// task, amber when a gap blocks automation, muted when every gap is advisory,
// and nothing for a task that lacks nothing. The tooltip lists each gap.
export function buildReadinessBadge(task) {
  const readiness = readinessOf(task);
  if (!readiness) return [];
  if (readiness.preparing) {
    return [el("span", {
      class: "readiness-badge readiness-preparing mono",
      text: "preparing",
      title: "A task-pilot run is preparing this task; its readiness settles when the run does",
    })];
  }
  const gaps = readinessGaps(readiness);
  if (gaps.length === 0) return [];
  const blocking = readiness.ready === false;
  return [el("span", {
    class: `readiness-badge ${blocking ? "readiness-blocking" : "readiness-advisory"} mono`,
    text: `${blocking ? "not ready" : "advisory"} · ${gaps.length}`,
    title: gaps.map((gap) => `${gap.severity}: ${gap.message}`).join("\n"),
  })];
}

// Core supplies only the findings that hold promotion; clients do not
// reinterpret pilot assessments or read the drain's latest-pass snapshot.
export function pilotWarnings(task) {
  return Array.isArray(task.pilot_warnings) ? task.pilot_warnings : [];
}

export function buildPilotWarningBadge(task) {
  const count = pilotWarnings(task).length;
  return count ? [el("span", {
    class: "chip pilot-warning-chip",
    role: "img",
    "aria-label": `Pilot warnings: ${count} holding findings`,
  }, [el("span", { text: "⚠", "aria-hidden": "true" }), ` pilot warnings · ${count}`])] : [];
}

// The server binds these runs to the task; ordinary delivery runs do not
// imply triage. Keep the workspace on links from an aggregate owner view.
export function buildActiveTriageBadges(task) {
  return (Array.isArray(task.active_triage) ? task.active_triage : []).map((triage) => {
    const title = triage.crew ? `Triaging with ${triage.crew}` : "Triaging · crew assignment pending";
    const link = el("a", {
      class: "chip active-triage-chip",
      text: "triaging",
      title,
      "aria-label": `${title}; view run`,
    });
    link.href = taskShared.taskRunHref(task, triage.run_id);
    link.addEventListener("click", (event) => event.stopPropagation());
    link.addEventListener("keydown", (event) => event.stopPropagation());
    return link;
  });
}

function deliveryAttentionLabel(attention) {
  if (!attention) return null;
  if (attention.blocker === "failed_checks") return "CI failed";
  if (attention.blocker === "conflict") return "conflict";
  if (attention.blocker) return String(attention.blocker).replaceAll("_", " ");
  if (attention.run_state) return "run failed";
  return "delivery stuck";
}

function deliveryAttentionTitle(task, attention) {
  const parts = [];
  if (attention.failed_step_id) parts.push(`failed step ${attention.failed_step_id}`);
  if (attention.blocker) parts.push(`blocker ${attention.blocker}`);
  if (attention.head_sha) parts.push(`head ${attention.head_sha}`);
  if (attention.attempt != null && attention.budget != null) parts.push(`auto-fix attempt ${attention.attempt} of ${attention.budget}`);
  if (attention.run_state) parts.push(`delivery run ${attention.run_state}`);
  if (attention.repairing) parts.push("a live run is repairing the pull request");
  if (attention.as_of) parts.push(`as of ${attention.as_of}`);
  // A live repair still names the blocker it is repairing.
  const label = deliveryAttentionLabel(attention);
  const state = attention.repairing ? `repairing ${label}` : label;
  return parts.length ? `${task.status} · ${state}: ${parts.join(", ")}` : `${task.status} · ${state}`;
}

// A live run repairing the pull request reads `repairing` with its auto-fix
// attempt where the blocker was; the tooltip and aria-label still name the blocker.
export function buildDeliveryAttentionBadge(task) {
  const attention = task.delivery_attention;
  if (!attention) return [];
  const repairing = Boolean(attention.repairing);
  const attempt = attention.attempt != null && attention.budget != null ? ` ${attention.attempt}/${attention.budget}` : "";
  const text = repairing ? `${task.status} · repairing${attempt}` : `${task.status} · ${deliveryAttentionLabel(attention)}`;
  return [el("span", {
    class: repairing ? "delivery-attention-badge repairing" : "delivery-attention-badge",
    role: "img",
    "aria-label": deliveryAttentionTitle(task, attention),
    title: deliveryAttentionTitle(task, attention),
    text,
  })];
}

export function buildDeliveryAttentionBanner(task) {
  const attention = task.delivery_attention;
  if (!attention) return null;
  const lines = [];
  if (attention.failed_step_id) lines.push(`Failed step: ${attention.failed_step_id}`);
  if (attention.blocker) lines.push(`Blocker: ${attention.blocker}`);
  if (attention.head_sha) lines.push(`Observed head: ${attention.head_sha}`);
  if (attention.attempt != null && attention.budget != null) lines.push(`Auto-fix attempt ${attention.attempt} of ${attention.budget}`);
  if (attention.run_state) lines.push(`Delivery run: ${attention.run_state}`);
  lines.push(attention.repairing ? "A live run is watching or repairing the pull request." : "No live run is repairing the pull request.");
  if (attention.as_of) lines.push(`As of ${attention.as_of}`);
  const actions = el("div", { class: "task-attention-actions" });
  if (attention.run_id) {
    if (task.job_run_navigable !== false) {
      const run = el("a", { class: "attention-action", text: "View run" });
      run.href = taskShared.taskRunHref(task, attention.run_id);
      run.addEventListener("click", (event) => event.stopPropagation());
      actions.appendChild(run);
    } else {
      actions.appendChild(el("span", { class: "attention-action", text: `Run ${attention.run_id}` }));
    }
  }
  const pullRequest = taskShared.pullRequestLink(task);
  if (pullRequest) {
    const pr = el("a", { class: "attention-action", text: pullRequest.number ? `PR #${pullRequest.number}` : "Pull request" });
    pr.href = pullRequest.url;
    pr.rel = "noopener noreferrer";
    pr.target = "_blank";
    pr.addEventListener("click", (event) => event.stopPropagation());
    actions.appendChild(pr);
  } else if (attention.pr_number) {
    actions.appendChild(el("span", { class: "attention-action", text: `PR #${attention.pr_number}` }));
  }
  const state = attention.repairing ? "Delivery repairing" : "Delivery needs attention";
  return el("section", { class: "task-attention warning delivery-attention", "aria-label": state }, [
    el("span", { class: "task-attention-icon", "aria-hidden": "true" }),
    el("div", { class: "task-attention-content" }, [
      el("h3", { class: "task-attention-title", text: `${state} · ${deliveryAttentionLabel(attention)}` }),
      ...lines.map((line) => el("p", { class: "task-attention-body", text: line })),
      actions.children.length ? actions : null,
    ]),
  ]);
}

// The pilot-hold callout on a proposed task: the pilot warning triage's summary
// and one button per option when it opened a decision, else the findings and
// the plain hold explanation. The raw findings sit under a collapsed "Details".
export function buildPilotWarnings(task, detail, context) {
  const findings = pilotWarnings(task);
  if (!findings.length) return null;
  const labels = {
    utility_warnings: "Utility", surface_warnings: "Surface",
    validation_tool_warnings: "Validation tool", blocked_by: "Prerequisite",
    adr_conflicts: "Conflict",
  };
  const decision = taskDecision.pilotHoldDecision(task);
  const list = el("ul", { class: "pilot-warning-findings" }, findings.map(finding => el("li", {}, [
    el("strong", { text: `${labels[finding.kind] || String(finding.kind).replaceAll("_", " ")}: ` }),
    el("span", { text: finding.text }),
  ])));
  const content = el("div", { class: "task-attention-content" }, [
    el("h3", { class: "task-attention-title", text: "Pilot warnings" }),
  ]);
  if (decision && detail && context) {
    content.appendChild(taskDecision.buildDecisionCard(task, decision, detail, context));
    content.appendChild(el("details", { class: "task-block-details" }, [
      el("summary", { text: "Details" }),
      list,
    ]));
  } else {
    content.appendChild(el("p", { class: "task-attention-body", text: "The drain holds this task until an operator approves it or edits it so the next pilot pass is clean." }));
    content.appendChild(list);
  }
  return el("section", { class: "task-attention warning pilot-warnings", "aria-label": "Pilot warnings" }, [
    el("span", { class: "task-attention-icon", "aria-hidden": "true" }),
    content,
  ]);
}

// Admission holds apply beyond proposed promotion warnings. When the decision
// channel carries this hold, its existing answer controls are the action set.
export function buildPilotAdmissionHold(task, detail, context) {
  if (!task.resolvable_pilot_admission_hold) return null;
  if ((task.open_decisions || []).some(request => request.gate === "pilot_admission")) return null;
  const content = el("div", { class: "task-attention-content" }, [
    el("h3", { class: "task-attention-title", text: "Pilot admission hold" }),
  ]);
  content.appendChild(el("p", {
    class: "task-attention-body operator-record-note",
    text: "Clear hold records that the held requirement is satisfied; Approve anyway records a deliberate override. Each records your evidence with the decision and releases the hold exactly as the same written record from the command line does.",
  }));
  const actions = el("div", { class: "task-attention-actions operator-record-actions" });
  const mutable = taskShared.canMutateTask(task);
  const disabledTitle = mutable ? "" : taskShared.aggregateRefusalTitle("Operator record", "record an operator decision");
  const clear = el("button", {
    class: "attention-action primary pilot-admission-clear",
    type: "button",
    text: "Clear hold",
    title: disabledTitle || "Record that the held requirement is satisfied, with your evidence; this clears the pilot admission hold",
  });
  clear.disabled = !mutable;
  clear.addEventListener("click", (e) => { e.stopPropagation(); runOperatorRecord(task, detail, clear, context, { kind: "pilot", verdict: "clear" }); });
  actions.appendChild(clear);
  const approve = el("button", {
    class: "attention-action pilot-admission-approve-anyway",
    type: "button",
    text: "Approve anyway",
    title: disabledTitle || "Record a deliberate override of the pilot admission hold, with your reason; the task is admitted despite the finding",
  });
  approve.disabled = !mutable;
  approve.addEventListener("click", (e) => { e.stopPropagation(); runOperatorRecord(task, detail, approve, context, { kind: "pilot", verdict: "approve-anyway" }); });
  actions.appendChild(approve);
  content.appendChild(actions);
  content.dataset.operatorRecord = "true";
  content.dataset.operatorRecordFeedback = "true";
  const refusal = operatorRecordRefusalNode(task);
  if (refusal) content.appendChild(refusal);
  return el("section", { class: "task-attention warning pilot-admission-hold", "aria-label": "Pilot admission hold" }, [
    el("span", { class: "task-attention-icon", "aria-hidden": "true" }),
    content,
  ]);
}

export function operatorRecordSignature(task) {
  return taskShared.taskState.operatorRecordRefusals.get(taskShared.taskDispatchIdentity(task)) || "";
}

// The typed refusal of this task's last operator record, or null.
export function operatorRecordRefusalNode(task) {
  const refusal = operatorRecordSignature(task);
  if (!refusal) return null;
  const node = taskShared.actionErrorNode(refusal);
  node.classList.add("operator-record-refusal");
  return node;
}

// A detail kept for another open draft is not rebuilt, so each operator-record
// host (the pilot hold and the owed-criteria list) repaints its refusal here:
// the note a refused record leaves appears, and a cleared one goes.
export function refreshOperatorRecordFeedback(wrap, task) {
  const refusal = operatorRecordSignature(task);
  for (const host of wrap.querySelectorAll("[data-operator-record-feedback]")) {
    const shown = host.querySelector(":scope > .operator-record-refusal");
    if ((shown ? shown.textContent : "") === refusal) continue;
    if (shown) shown.remove();
    const node = operatorRecordRefusalNode(task);
    if (node) host.appendChild(node);
  }
}

// One-click operator records: pilot-admission decisions and post-deploy
// measurements. Each explains what the record does in a confirmation, asks
// for the required evidence, then posts through the typed operator route,
// which writes the same record the command line writes. A refusal shows its
// typed code and message inline and changes nothing.
export async function runOperatorRecord(task, detail, btnNode, context, request) {
  const visit = captureWorkspaceVisit();
  const key = taskShared.taskDispatchIdentity(task);
  if (btnNode.disabled || !taskShared.canMutateTask(task)) return;
  const recordHost = btnNode.closest("[data-operator-record]");
  let suffix;
  let body;
  let successText;
  if (request.kind === "pilot") {
    const approveAnyway = request.verdict === "approve-anyway";
    const explain = approveAnyway
      ? `Approve anyway for ${task.id}? This records a deliberate override of the pilot admission hold with your reason, and the task is admitted despite the finding.`
      : `Clear hold for ${task.id}? This records that the held requirement is satisfied, with your evidence, and clears the pilot admission hold.`;
    if (typeof window.confirm !== "function" || !window.confirm(explain)) return;
    if (typeof window.prompt !== "function") return;
    const evidence = window.prompt(
      approveAnyway
        ? `Reason for approving ${task.id} anyway (recorded with the decision):`
        : `Evidence that the hold on ${task.id} is satisfied (recorded with the decision):`,
      "",
    );
    if (evidence === null) return;
    if (!evidence.trim()) {
      const message = "[pilot_admission_evidence_required] pilot admission needs evidence; it is recorded with the decision";
      taskShared.taskState.operatorRecordRefusals.set(key, message);
      taskShared.rerender(context);
      return;
    }
    suffix = "/pilot-admission";
    body = { verdict: request.verdict, evidence: evidence.trim() };
    successText = approveAnyway ? `${task.id}: pilot admission hold approved anyway` : `${task.id}: pilot admission hold cleared`;
  } else {
    const criterion = request.criterion;
    if (typeof window.confirm !== "function" || !window.confirm(
      `Mark criterion ${criterion} met for ${task.id}? This records that the deployed build met criterion ${criterion}, with your measurement evidence, and settles that owed criterion.`,
    )) return;
    if (typeof window.prompt !== "function") return;
    const evidence = window.prompt(`Measurement evidence for criterion ${criterion} on ${task.id} (recorded with the result):`, "");
    if (evidence === null) return;
    if (!evidence.trim()) {
      const message = "[post_deploy_evidence_required] post-deploy record needs evidence; it is recorded with the measurement";
      taskShared.taskState.operatorRecordRefusals.set(key, message);
      taskShared.rerender(context);
      return;
    }
    suffix = "/post-deploy";
    body = { criterion, evidence: evidence.trim() };
    successText = `${task.id}: criterion ${criterion} recorded met`;
  }
  const requestPath = visit.path(taskShared.taskMutationPath(task, suffix));
  taskShared.taskState.operatorRecordRefusals.delete(key);
  // The controls this request holds, and only those: a retained detail keeps
  // them after the request settles, so each is given back, never one a draft or
  // another request already had disabled.
  const held = Array.from(detail.querySelectorAll(".action, .attention-action")).filter((b) => !b.disabled);
  for (const b of held) b.disabled = true;
  try {
    await postJson(requestPath, body);
  } catch (error) {
    for (const b of held) b.disabled = false;
    if (!visit.isCurrent()) return;
    const code = error && error.code ? error.code : "operator_record_refused";
    const message = `[${code}] ${error.message || String(error)}`;
    taskShared.taskState.operatorRecordRefusals.set(key, message);
    taskShared.rerender(context);
    setTimeout(() => {
      if (taskShared.taskState.operatorRecordRefusals.get(key) !== message) return;
      taskShared.taskState.operatorRecordRefusals.delete(key);
      taskShared.rerender(context);
    }, OPERATOR_RECORD_REFUSAL_MS);
    return;
  }
  // A retained detail keeps only the settled record's controls disabled:
  // one criterion row, or both alternatives for the same pilot decision.
  // Release before refreshing so a failed read cannot strand unrelated actions.
  for (const b of held) if (!recordHost || b.closest("[data-operator-record]") !== recordHost) b.disabled = false;
  if (!visit.isCurrent()) return;
  taskShared.taskState.taskActionNotice = successText;
  await taskShared.refreshTasks(context);
}

// The readiness dot beside a backlog task's ID: the pool colour, its words as
// the dot's label and the full wait in the tooltip. A row the snapshot does
// not mention keeps a hidden dot and says nothing.
function buildReadinessDot(task) {
  const readiness = drainReadinessFor(task.id);
  if (!readiness) return el("span", { class: "ready-dot none", "aria-hidden": "true" });
  const dot = el("span", { class: "ready-dot", role: "img", "aria-label": readiness.label, title: readiness.title });
  dot.dataset.pool = readiness.pool;
  return dot;
}

// A backlog row's second line after its ID: the readiness dot and why the
// task is or is not starting, in the drain snapshot's own words. The dot's
// label already says it to assistive technology, so the visible words are
// hidden from it rather than read twice.
export function buildRowReason(task) {
  const dot = buildReadinessDot(task);
  const readiness = drainReadinessFor(task.id);
  if (!readiness) return el("span", { class: "row-reason none" }, [dot]);
  const words = readiness.badge ? readiness.badge.text : readiness.label;
  const text = `${words.charAt(0).toUpperCase()}${words.slice(1)}`.replace(/\bmacos\b/g, "macOS");
  return el("span", { class: "row-reason" }, [
    dot,
    el("span", { class: "row-reason-text", text, title: readiness.title, "aria-hidden": "true" }),
  ]);
}

// A task id in the Readiness card: a link to that task in the same workspace,
// which opens it even when the Tasks filter hides it.
function readinessTaskLink(taskId, workspace) {
  const link = el("a", { class: "relation-target mono", text: taskId, title: `Open ${taskId}` });
  return bindTaskLink(link, taskId, workspace ? { workspace } : {});
}

// The Readiness card leads with the pool, then links every reported blocker.
// Without an entry it says whether a read is pending, failed or found nothing,
// and never claims a pool.
function buildDrainReadiness(drain, workspace) {
  const entry = drain && drain.entry;
  if (!entry) {
    const text = drain && drain.pending
      ? "Checking whether the drain can take this task…"
      : `Drain readiness unknown${drain && drain.error ? `: ${drain.error}` : ""}.`;
    return el("div", { class: "readiness-drain readiness-note", text });
  }
  const view = drainReadinessView(entry);
  const badge = view.badge;
  const dot = el("span", { class: "ready-dot", "aria-hidden": "true" });
  dot.dataset.pool = view.pool;
  const head = el("div", { class: "readiness-drain-head" }, [
    dot,
    el("strong", { text: view.poolLabel }),
    ...(badge && view.pool !== "locks" ? [el("span", { class: "readiness-drain-reason", text: `· ${badge.text}` })] : []),
  ]);
  const wrap = el("div", { class: "readiness-drain" }, [head]);
  wrap.dataset.pool = view.pool;
  const line = (label, children) => el("div", { class: "readiness-drain-line" }, [
    el("span", { class: "label", text: label }),
    el("span", { class: "readiness-drain-value" }, children),
  ]);
  if (!badge) {
    wrap.appendChild(el("div", { class: "readiness-fix", text: view.title }));
    return wrap;
  }
  if (badge.holders.length) {
    wrap.appendChild(line("Held by", badge.holders.map(id => readinessTaskLink(id, workspace))));
  } else if (view.pool === "locks" && !badge.runIds.length) {
    wrap.appendChild(line("Held by", [el("span", { text: "Lock holder unavailable" })]));
  }
  if (badge.runIds.length) {
    wrap.appendChild(line("Live run", badge.runIds.map(runId => {
      const run = el("a", { class: "mono", text: runId });
      run.href = runHref(runId, workspace ? { workspace } : {});
      return run;
    })));
  }
  if (badge.dependencies.length > 0) {
    wrap.appendChild(line("Depends on", badge.dependencies.map(({ taskId, status }) =>
      el("span", { class: "readiness-dependency" }, [
        readinessTaskLink(taskId, workspace),
        status ? el("span", { class: "readiness-fix", text: status }) : null,
      ]))));
  }
  for (const selector of badge.selectors) {
    wrap.appendChild(line("Overlap", [el("span", { class: "mono", text: `${selector.requested}${selector.held ? ` overlaps ${selector.held}` : ""}` })]));
  }
  for (const text of badge.detail.split("\n").filter(Boolean)) wrap.appendChild(el("div", { class: "readiness-message", text }));
  return wrap;
}

// The detail's Readiness card: why the drain is or is not taking the task,
// then each preparation gap with its severity and its fix. "Ready" is claimed
// only for a task the drain reports eligible with nothing missing.
export function buildReadinessBlock(readiness, drain, workspace) {
  const wrap = el("div", { class: "value readiness-list" });
  if (drain) wrap.appendChild(buildDrainReadiness(drain, workspace));
  if (!readiness) return wrap;
  if (readiness.preparing) {
    wrap.appendChild(el("div", {
      class: "readiness-note",
      text: "A task-pilot run is preparing this task; these gaps settle when it finishes.",
    }));
  }
  const gaps = readinessGaps(readiness);
  if (gaps.length === 0) {
    const eligible = drain && drain.entry && drain.entry.eligible === true;
    wrap.appendChild(el("div", { class: "readiness-note", text: eligible ? "Ready: nothing is missing." : "No preparation gaps." }));
    return wrap;
  }
  wrap.appendChild(el("span", { class: "readiness-gap-count", text: `${gaps.length} ${gaps.length === 1 ? "gap" : "gaps"}` }));
  for (const gap of gaps) {
    const severity = gap.severity === "blocking" ? "blocking" : "advisory";
    wrap.appendChild(el("div", { class: "readiness-gap" }, [
      el("span", { class: `readiness-badge readiness-${severity} mono`, text: severity }),
      el("div", { class: "readiness-gap-body" }, [
        el("div", { class: "readiness-message", text: gap.message || gap.code }),
        ...(gap.fix ? [el("div", { class: "readiness-fix", text: `Fix: ${gap.fix}` })] : []),
      ]),
    ]));
  }
  return wrap;
}
