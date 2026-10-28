// Job catalogue and live run operations [ORB-10875, ORB-10876].

import { copyText, el, fetchJson, hostWriteRefusal, postJson, requestPanel, relativeTime } from './common.js';

import {
  $,
  controlReason,
  cronText,
  durationText,
  explainUnavailable,
  feedback,
  noteCronZone,
  operationCell,
  operationColumns,
  operationDetails,
  operationGroup,
  operationsState,
  outcomeDot,
  pendingOperations,
  registerOperationsReset,
  runLink,
  runOperation,
  selectedWorkspace,
  selectionSnapshot,
  taskLink,
  time,
  withInactivePlugins,
  workspaceSelectionReason
} from './operations-shared.js';
import { jobIdFromTarget, scopedRoutines, updateRoutineFailureBadge } from './operations-routines.js';

// ---------------------------------------------------------------------------
// Jobs. The catalogue is projected from routine targets and recent runs.
// Manual Run submits catalog jobs; delivery pipelines use Ship or Drain.

const JOB_RUN_LIMIT = 100;
export let lastJobs = null;
registerOperationsReset(() => { lastJobs = null; });

function jobFamily(jobId) {
  if (/sweep|gc|pilot|triage|scheduler/.test(jobId)) return "sweep";
  if (/^(task|workspace|epic)_/.test(jobId)) return "delivery";
  return "other";
}

function jobCatalog(routinesPayload, runs, workspaceName) {
  const catalog = new Map();
  const entry = (jobId) => {
    if (!catalog.has(jobId)) catalog.set(jobId, { id: jobId, routines: [], runs: [], running: 0, lastRun: null });
    return catalog.get(jobId);
  };
  for (const routine of routinesPayload?.routines || []) {
    if (routine.source !== workspaceName) continue;
    const jobId = jobIdFromTarget(routine.target);
    if (jobId) entry(jobId).routines.push(routine);
  }
  const sorted = [...runs].sort((a, b) => new Date(b.created_at || 0) - new Date(a.created_at || 0));
  for (const run of sorted) {
    if (!run.job_id) continue;
    const job = entry(run.job_id);
    job.runs.push(run);
    if (run.state === "running" || run.state === "pending") job.running += 1;
    if (!job.lastRun) job.lastRun = run;
  }
  return catalog;
}

function jobRunningCard(run, workspaceId) {
  const startedAt = run.started_at || run.scheduled_at || run.created_at;
  return el("div", { class: "operation-running-card" }, [
    el("div", { class: "operation-running-head" }, [
      outcomeDot(run.state),
      el("strong", { class: "mono", text: run.job_id }),
      el("span", { class: "operation-state running", text: run.state }),
    ]),
    el("div", { class: "operation-running-meta" }, [
      el("span", { text: `started ${relativeTime(startedAt) || "—"}`, title: time(startedAt) }),
      run.run_role ? el("span", { class: "mono", text: run.run_role }) : null,
      run.resolved_crew ? el("span", { class: "mono", text: run.resolved_crew }) : null,
      runLink(run.run_id, workspaceId),
    ].filter(Boolean)),
  ]);
}

// A run names the tasks it works on: `tasks` carries titles when the server
// has them, `task_ids` is the fallback.
function runTaskRefs(run) {
  const refs = Array.isArray(run.tasks) ? run.tasks : (Array.isArray(run.task_ids) ? run.task_ids : []).map((id) => ({ id }));
  return refs.filter((task) => task && task.id);
}

// One group per task, so a task's auto, gate and pr runs share a card. A run
// naming several tasks appears under each. Runs with no task stay separate.
function runningGroups(running) {
  const byTask = new Map();
  const untasked = [];
  for (const run of running) {
    const tasks = runTaskRefs(run);
    if (!tasks.length) untasked.push(run);
    for (const task of tasks) {
      if (!byTask.has(task.id)) byTask.set(task.id, { task, runs: [] });
      const group = byTask.get(task.id);
      if (!group.task.title && task.title) group.task = task;
      group.runs.push(run);
    }
  }
  return { tasks: [...byTask.values()], untasked };
}

function runningTaskCard(group, workspaceId) {
  const { task, runs } = group;
  const startedAt = (run) => run.started_at || run.scheduled_at || run.created_at;
  const oldest = runs.reduce((earliest, run) => (new Date(startedAt(run) || 0) < new Date(startedAt(earliest) || 0) ? run : earliest));
  const card = el("div", { class: "operation-running-card operation-running-task" }, [
    el("div", { class: "operation-running-head" }, [
      outcomeDot(oldest.state),
      taskLink(task.id, workspaceId),
      task.title ? el("span", { class: "operation-running-task-title", text: task.title, title: task.title }) : null,
    ].filter(Boolean)),
    el("div", { class: "operation-running-meta" }, [
      el("span", { text: `started ${relativeTime(startedAt(oldest)) || "—"}`, title: time(startedAt(oldest)) }),
      el("span", { text: `${runs.length} in flight` }),
    ]),
    el("ul", { class: "operation-running-steps" }, runs.map((run) => el("li", { class: "operation-running-step" }, [
      el("span", { class: "mono", text: run.job_id }),
      run.run_role ? el("span", { class: "mono", text: run.run_role }) : null,
      run.resolved_crew ? el("span", { class: "mono", text: run.resolved_crew }) : null,
      runLink(run.run_id, workspaceId),
    ].filter(Boolean)))),
  ]);
  card.dataset.task = task.id;
  return card;
}

function shellQuoteJob(text) {
  return `'${text.replace(/'/g, "'\\''")}'`;
}
function shellWordJob(text) {
  return /^[A-Za-z0-9_.\/:@%+=,-]+$/.test(text) ? text : shellQuoteJob(text);
}
function jobCommand(jobId, workspace) {
  const target = workspace?.name || workspace?.id || '<workspace>';
  const targetWord = target === '<workspace>' ? target : shellWordJob(String(target));
  return `orbit run job ${shellWordJob(String(jobId))} --workspace ${targetWord}`;
}

function jobRow(job, workspace) {
  const detailsKey = `job:${job.id}`;
  const last = job.lastRun;
  const command = jobCommand(job.id, workspace);
  const key = `job:run:${workspace.id}:${job.id}`;
  const runReason = jobFamily(job.id) === "delivery"
    ? "Delivery jobs require task input or a delivery window. Use Ship or Drain."
    : controlReason(operationsState.lastOperations, "job_run");
  const pending = pendingOperations.has(key);
  const run = el("button", { class: "operation-button primary job-run", text: pending ? "Submitting…" : "Run ▸", title: runReason || `Run ${job.id} in ${workspace.name}` });
  run.type = "button";
  run.disabled = Boolean(runReason) || pending;
  run.addEventListener("click", () => {
    if (runReason) return;
    const selection = selectionSnapshot();
    runOperation({
      selection, key, feedbackId: "job-operation-feedback",
      pending: `Submitting ${job.id}…`,
      failure: `Could not run ${job.id}`,
      render: () => renderJobs(lastJobs),
      request: () => postJson(`/api/jobs/${encodeURIComponent(job.id)}/run`, {}),
      refresh: fetchAndRenderJobs,
      success: (result) => `Run ${result.run_id} ${result.state} for ${result.job_id}.`,
    });
  });
  const scheduledBy = job.routines.length
    ? job.routines.map((routine) => {
      const chip = el("a", { class: `operation-chip ${routine.enabled ? "" : "paused"}`.trim(), text: `${routine.name} · ${routine.trigger?.deliveries_landed ? "on delivery" : routine.trigger?.state ? "on task change" : cronText(routine.cron)}${routine.enabled ? "" : " · paused"}` });
      chip.href = "#operations/routines";
      return chip;
    })
    : [el("span", { class: "operation-cell-main muted", text: "manual only" })];
  const lastCell = last
    ? [
      el("span", { class: "operation-cell-main operation-outcome" }, [
        outcomeDot(last.state),
        el("span", { text: `${last.state} ${relativeTime(last.finished_at || last.started_at || last.created_at) || ""}`.trim(), title: time(last.finished_at || last.started_at || last.created_at) }),
      ]),
      el("span", { class: "operation-cell-sub" }, [
        durationText(last.duration_ms) ? el("span", { class: "mono", text: durationText(last.duration_ms) }) : null,
        runLink(last.run_id, workspace?.id),
      ].filter(Boolean)),
    ]
    : [el("span", { class: "operation-cell-main muted", text: "no recent run" })];
  const copy = el("button", { class: "operation-button secondary", text: "Copy command", title: "Copy the CLI command" });
  copy.type = "button";
  copy.addEventListener("click", async () => {
    copy.textContent = (await copyText(command)) ? "Copied" : "Copy failed";
    setTimeout(() => { copy.textContent = "Copy command"; }, 1500);
  });
  const recent = job.runs.slice(0, 5);
  const card = el("article", { class: `operation-card operation-row job-card ${jobFamily(job.id)}` });
  card.dataset.job = job.id;
  card.append(
    el("div", { class: "operation-row-head" }, [
      operationCell("Job", [
        el("div", { class: "operation-identity" }, [el("strong", { class: "mono", text: job.id })]),
        el("span", { class: "operation-cell-sub", text: `${job.runs.length} recent run${job.runs.length === 1 ? "" : "s"} in this window` }),
      ], "operation-cell-identity"),
      operationCell("Scheduled by", [el("span", { class: "operation-chips" }, scheduledBy)]),
      operationCell("Last run", lastCell),
      operationCell("Active", [el("span", { class: `operation-cell-main mono${job.running ? " live" : " muted"}`, text: job.running ? `${job.running} running` : "idle" })]),
      operationCell("", [explainUnavailable(run, runReason)], "operation-cell-action"),
    ]),
    operationDetails(detailsKey, [
      el("div", { class: "operation-command" }, [
        el("code", { class: "mono", text: command }),
        copy,
      ]),
      el("p", { class: "operation-control-note", text: "Run submits without extra input. Delivery jobs need a task id and are started through Ship or Drain." }),
      recent.length
        ? el("ul", { class: "operation-recent-runs" }, recent.map((run) => el("li", {}, [
          outcomeDot(run.state),
          runLink(run.run_id, workspace?.id),
          el("span", { class: "muted", text: `${run.state} · ${relativeTime(run.finished_at || run.started_at || run.created_at) || ""}${durationText(run.duration_ms) ? ` · ${durationText(run.duration_ms)}` : ""}`, title: time(run.finished_at || run.started_at || run.created_at) }),
        ])))
        : null,
    ]),
  );
  return card;
}

export function renderJobs(payload) {
  if (payload.view !== "live") lastJobs = payload;
  const isLive = payload.view === "live";
  const body = $(isLive ? "jobs-live-body" : "jobs-body");
  const count = $(isLive ? "jobs-live-count" : "jobs-count");
  if (!body) return;
  body.textContent = "";
  const workspace = selectedWorkspace();
  const selectionReason = workspaceSelectionReason();
  if (selectionReason || !workspace) {
    body.appendChild(el("div", { class: "operations-readonly-note", text: selectionReason || "Select a workspace to list its jobs." }));
    body.appendChild(el("div", { class: "empty-state", text: "Select a workspace to list its jobs." }));
    count.textContent = "read-only";
    return;
  }
  // A host that refuses writes still lists its jobs; Run says why it is off.
  const hostRefusal = hostWriteRefusal();
  if (hostRefusal) body.appendChild(el("div", { class: "operations-readonly-note", text: hostRefusal }));
  const runs = Array.isArray(payload.runs?.items) ? payload.runs.items : Array.isArray(payload.runs) ? payload.runs : [];
  const running = runs.filter((run) => run.state === "running" || run.state === "pending");
  if (isLive) {
    const groups = runningGroups(running);
    const taskCount = groups.tasks.length;
    const strip = el("section", { class: "operation-running" });
    strip.setAttribute("aria-label", "Running now");
    strip.appendChild(el("div", { class: "operation-running-title" }, [
      el("strong", { text: "Running now" }),
      el("span", { class: "mono", text: `${taskCount} task${taskCount === 1 ? "" : "s"}` }),
      el("span", { class: "muted", text: running.length ? `(${running.length} run${running.length === 1 ? "" : "s"} in flight)` : "nothing in flight" }),
    ]));
    if (running.length) {
      strip.appendChild(el("div", { class: "operation-running-grid" }, [
        ...groups.tasks.map((group) => runningTaskCard(group, workspace.id)),
        ...groups.untasked.map((run) => jobRunningCard(run, workspace.id)),
      ]));
    }
    body.appendChild(strip);
    if (payload.runs?.truncated) body.appendChild(el("p", { class: "operation-control-note", text: `${running.length} shown of ${payload.runs.total} runs in flight.` }));
    count.textContent = `${running.length} run${running.length === 1 ? "" : "s"} in flight · ${workspace.name}`;
    return;
  }
  const catalog = jobCatalog(operationsState.lastOperations, runs, workspace.name);
  if (catalog.size === 0) {
    body.appendChild(el("div", { class: "empty-state", text: "No job has been referenced by a routine or run in this workspace recently." }));
  } else {
    const jobs = [...catalog.values()].sort((a, b) => a.id.localeCompare(b.id));
    const groups = [
      ["accent", "Sweeps", "housekeeping and intake · safe to run by hand", jobs.filter((job) => jobFamily(job.id) === "sweep")],
      ["delivery", "Delivery", "task and workspace pipelines · started by a ship or a drain, by hand only with a task_id", jobs.filter((job) => jobFamily(job.id) === "delivery")],
      ["", "Other", "", jobs.filter((job) => jobFamily(job.id) === "other")],
    ];
    const columns = ["Job", "Scheduled by", "Last run", "Active", ""];
    for (const [tone, title, hint, members] of groups) {
      if (!members.length) continue;
      const group = operationGroup(tone, title, members.length, hint);
      group.appendChild(operationColumns(columns));
      for (const job of members) group.appendChild(jobRow(job, workspace));
      body.appendChild(group);
    }
    body.appendChild(el("p", { class: "operation-control-note", text: `Catalogue projected from routine targets and the last ${JOB_RUN_LIMIT} runs.` }));
  }
  $("jobs-count").textContent = `${catalog.size} job${catalog.size === 1 ? "" : "s"} · ${running.length} running · ${workspace.name}`;
}

export function fetchAndRenderJobs() {
  const live = operationsState.context.getJobsSubtab?.() === "live";
  const bodyId = live ? "jobs-live-body" : "jobs-body";
  const countId = live ? "jobs-live-count" : "jobs-count";
  if (!selectedWorkspace()) {
    return requestPanel(bodyId, "unselected", () => Promise.resolve({ runs: [], view: live ? "live" : "catalogue" }), renderJobs, countId);
  }
  return requestPanel(bodyId, "jobs",
    () => Promise.all([
      fetchJson(withInactivePlugins("/api/routines")),
      fetchJson(`/api/job-runs?limit=${JOB_RUN_LIMIT}${live ? "&state=active" : ""}`),
    ]).then(([routines, runs]) => ({ routines, runs, view: live ? "live" : "catalogue" })),
    (payload) => {
      // Only a fresh catalogue/live read adopts its routines snapshot. A
      // repaint from cached jobs must retain a newer Routines readback.
      operationsState.lastOperations = payload.routines;
      noteCronZone(operationsState.lastOperations);
      updateRoutineFailureBadge(scopedRoutines(operationsState.lastOperations));
      renderJobs(payload);
    }, countId);
}
