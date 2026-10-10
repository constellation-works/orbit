// Orbit dashboard runs-domain (Recent Runs table, cancel/replay, friction badges, sort).
// Pure vanilla JS, split into ES module with no build step.
//
// Extracted from app.js (ORB-00180). Owns the runSort state, all friction-row
// classification helpers (coerce*, first*, frictionRow*, empty*, add*), merge*,
// sort*, header/cell renderers, and the table renderer.
// Also owns the action button builders and cancel/replay/resume fns (used by both the
// runs table and the run-detail meta in app.js).
//
// Receives one-time context via initRuns(runsContext()) containing the
// callbacks (fetchAndRender*, navigateToRun) and getters (activeRunId, lastRuns,
// formatters) that the actions and render depend on. No direct import from app.js.

import { captureWorkspaceVisit, getWorkspace, hostWriteRefusal, onWorkspaceChange, panelCanRender, describePullSettlements, makeCopyButton, el, stateCell, syncNodes, postJson, fetchJson, makeRowDisclosure, enableRovingRows, formatDateTime, elapsedDurationInfo, getWindow, setWindow, persistScopeToUrl, notifyScopeChange, DASHBOARD_WINDOWS } from './common.js';

const $ = (id) => document.getElementById(id);

const CANCELLABLE_RUN_STATES = new Set(["pending", "running"]);
const RESUMABLE_RUN_STATES = new Set(["failed", "interrupted", "timeout"]);
const ACTIVE_RUN_STATES = new Set(["pending", "running"]);
const RUN_FILTERS = new Set(["all", "active", "failed"]);
// One delivered task runs these three pipelines back to back; the list folds
// them into one group per task.
const PIPELINE_GROUP_JOBS = new Set(["task_auto_pipeline", "task_gate_pipeline", "task_pr_pipeline"]);
// Worst first: a group reads as the most urgent state among its runs.
const GROUP_STATE_PRIORITY = ["failed", "timeout", "interrupted", "running", "pending"];
const FAILED_AT_EXCERPT_CHARS = 80;

const RUN_SORT_DEFAULT_DIR = {
  when: "desc",
  job: "asc",
  task: "asc",
  run_id: "asc",
  denials: "desc",
  tool_fails: "desc",
  duration: "desc",
  state: "asc",
};

let runSort = { key: "when", dir: "desc" };
const resumedRunIdsBySource = new Map();
// Resume requests awaiting a response, by run identity. The server refuses a
// second live resume of a lineage; this keeps the button disabled even when
// the row re-renders mid-request.
const resumeRequestsInFlight = new Set();
const cancelRequestsInFlight = new Set();
// Pipeline groups the operator has opened, by group key. Groups start folded.
const expandedRunGroups = new Set();
const replayRequestsInFlight = new Set();
let runFilter = (() => {
  const value = new URL(window.location.href).searchParams.get("run_state") || "all";
  return RUN_FILTERS.has(value) ? value : "all";
})();
// A hand-built URL may set both. The search field edits one of them and
// clears the other; until that edit, the API applies them together.
let runTaskId = (new URL(window.location.href).searchParams.get("task_id") || "").trim();
let runJobId = (new URL(window.location.href).searchParams.get("job_id") || "").trim();
let runQueryTimer = null;

// Task ids are PREFIX-digits with a 2-5 letter prefix outside the ADR, L,
// and F artifact namespaces. Anything else in the search field is a job id.
function isTaskId(value) {
  const match = /^([A-Z]{2,5})-(\d+)$/.exec(value);
  return Boolean(match) && !["ADR", "L", "F"].includes(match[1]);
}

export function runsQueryKey() {
  return `${runFilter}\n${runTaskId}\n${runJobId}\n${getWindow()}`;
}

// Query string for /api/job-runs and /api/job-runs/all. `all` omits since
// so the list is not time-bounded, matching the rail's window.
export function jobRunsQuery(limit) {
  const params = new URLSearchParams();
  params.set("limit", String(limit));
  params.set("state", runFilter);
  if (runTaskId) params.set("task_id", runTaskId);
  if (runJobId) params.set("job_id", runJobId);
  const selected = getWindow();
  if (selected && selected !== "all") params.set("since", selected);
  return params.toString();
}

let _runsCtx = null;

function hasCtx(key) {
  const c = _runsCtx;
  return !!(c && typeof c[key] === "function");
}

function getActiveRunId() {
  return hasCtx("getActiveRunId") ? _runsCtx.getActiveRunId() : null;
}

function doNavigateToRun(runId, workspaceId = null) {
  if (hasCtx("navigateToRun")) {
    try { _runsCtx.navigateToRun(runId, workspaceId); } catch (_) {}
  }
}

function doFetchAndRenderRuns() {
  if (hasCtx("fetchAndRenderRuns")) {
    try { return _runsCtx.fetchAndRenderRuns(); } catch (_) { return Promise.resolve(); }
  }
  return Promise.resolve();
}

function doFetchAndRenderRunDetail() {
  if (hasCtx("fetchAndRenderRunDetail")) {
    try { return _runsCtx.fetchAndRenderRunDetail(); } catch (_) { return Promise.resolve(); }
  }
  return Promise.resolve();
}

function doFetchAndRenderRunEvents() {
  if (hasCtx("fetchAndRenderRunEvents")) {
    try { return _runsCtx.fetchAndRenderRunEvents(); } catch (_) { return Promise.resolve(); }
  }
  return Promise.resolve();
}

function fmtTimestampValue(v) {
  return hasCtx("fmtTimestamp") ? _runsCtx.fmtTimestamp(v) : (v || "-");
}

function fmtDurationValue(v) {
  return hasCtx("fmtDuration") ? _runsCtx.fmtDuration(v) : (v == null ? "-" : String(v));
}

export function initRuns(ctx) {
  _runsCtx = ctx;
}

function runIsCancellable(run) {
  return CANCELLABLE_RUN_STATES.has(run && run.state);
}

function runIsResumable(run) {
  return RESUMABLE_RUN_STATES.has(run && run.state);
}

// A row's button reads "cancel" or "Resume", which names nothing once a screen
// reader lists the controls on their own; the run (and its workspace, when the
// table spans several) makes each one distinguishable.
function runActionLabel(verb, run) {
  const workspace = run && (run.workspace_name || run.workspace_id);
  return `${verb} run ${run && run.run_id}${workspace ? ` in ${workspace}` : ""}`;
}

// A selected host that refuses forwarded writes disables every run action
// and says why, rather than letting the click fail.
function refuseOnReadOnlyHost(btn) {
  const refusal = hostWriteRefusal();
  if (!refusal) return;
  btn.disabled = true;
  btn.title = refusal;
}

function buildCancelRunButton(run, host) {
  const btn = el("button", {
    class: "action reject run-cancel",
    text: "Cancel",
    title: `Cancel ${run.run_id}`,
  });
  btn.disabled = !runIsCancellable(run) || cancelRequestsInFlight.has(runIdentity(run));
  btn.setAttribute("aria-label", runActionLabel("Cancel", run));
  refuseOnReadOnlyHost(btn);
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    cancelRun(run, btn, host);
  });
  return btn;
}

function buildReplayRunButton(run, host) {
  const btn = el("button", {
    class: "action archive run-replay",
    text: "Replay run",
    title: `Replay ${run.run_id}`,
  });
  btn.disabled = replayRequestsInFlight.has(runIdentity(run));
  btn.setAttribute("aria-label", runActionLabel("Replay", run));
  refuseOnReadOnlyHost(btn);
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    replayRun(run, btn, host);
  });
  return btn;
}

function buildResumeRunButton(run, host) {
  const btn = el("button", {
    class: "action approve run-resume",
    text: "Resume",
    title: `Resume ${run.run_id} from its first non-successful step`,
  });
  btn.disabled = resumeRequestsInFlight.has(runIdentity(run));
  btn.setAttribute("aria-label", runActionLabel("Resume", run));
  refuseOnReadOnlyHost(btn);
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    resumeRun(run, btn, host);
  });
  return btn;
}

function runScopedPath(path, run) {
  if (!run || !run.workspace_id) return path;
  const separator = path.includes("?") ? "&" : "?";
  return `${path}${separator}workspace=${encodeURIComponent(run.workspace_id)}`;
}

function runIdentity(run) {
  return `${run && run.workspace_id ? run.workspace_id : getWorkspace() || ""}:${run && run.run_id ? run.run_id : ""}`;
}

// Jobs whose runs are drain coordinators: the local `auto` window and a
// follower's pull drain. Cancelling one ends the coordinator, not just the
// admissions, so it gets its own prompt and settlement report.
const DRAIN_JOB_IDS = new Set(["workspace_auto_pipeline", "workspace_pull_pipeline"]);

function runIsPullDrain(run) {
  return run && run.job_id === "workspace_pull_pipeline";
}

function runIsDrain(run) {
  return !!run && DRAIN_JOB_IDS.has(run.job_id);
}

// Leaf definitions only a follower's claim may run. Their run rows come from
// list endpoints that carry no claim (a per-row admission read would make every
// list an N+1), so the cancel action asks the run detail for the claim instead.
const CLAIMED_LEAF_JOB_IDS = new Set(["task_claimed_local_pipeline", "task_claimed_pr_pipeline"]);

// A claimed leaf executes another machine's task, and cancelling it fails that
// claim on the owner and blocks the owner's task. `claim` is the run detail's
// `pull_claim`; null or absent means the run executes no owner claim.
function claimedLeafSentence(claim) {
  if (!claim) return "";
  const task = claim.task_id ? ` (task ${claim.task_id})` : "";
  const owner = claim.owner ? ` on ${claim.owner}` : " on the owner machine";
  return `This run executes a claimed task${task} for its owner. Cancelling it fails that claim${owner} and the owner's task is blocked.`;
}

// The claim to warn about: the row's own when the payload carried one (run
// detail), else one detail read for a claimed-leaf row of a list. A failed read
// shows no warning rather than blocking the cancel, like `orbit run show`.
async function resolvePullClaim(run) {
  if (run.pull_claim !== undefined) return run.pull_claim;
  if (!CLAIMED_LEAF_JOB_IDS.has(run.job_id)) return null;
  try {
    const detail = await fetchJson(runScopedPath(`/api/runs/${encodeURIComponent(run.run_id)}`, run));
    return (detail && detail.pull_claim) || null;
  } catch (e) {
    console.error(e);
    return null;
  }
}

function cancelPromptText(run) {
  const runId = run.run_id;
  if (!runIsDrain(run)) {
    const claimed = claimedLeafSentence(run.pull_claim);
    if (!claimed) return `Cancel ${runId}? Add a reason (optional):`;
    return [`Cancel ${runId}?`, claimed, "Add a reason (optional):"].join("\n\n");
  }
  const consequence = runIsPullDrain(run)
    ? "Cancelling stops new requests now and returns claims it has not launched to the owner's backlog. Leaves already running finish and deliver their result; the drain ends once they have. You choose next whether to stop them instead."
    : "Cancelling ends the drain coordinator now instead of letting admitted work wind down. Task runs it started keep going unless you choose next to stop them too.";
  return [
    `Cancel drain ${runId}?`,
    consequence,
    "To only stop taking new work, press Stop on the Auto-drain card (Tasks tab) instead — nothing is wasted.",
    "Enter a reason (optional) and press OK to cancel the drain, or press Cancel to leave it running:",
  ].join("\n\n");
}

// A drain's cancel can also stop the work already in flight (`force`): a pull
// drain's running leaves, whose claims go back to the owner's backlog, or a
// local drain's task runs. Asked only for a drain; OK forces.
function confirmForceText(run) {
  if (runIsPullDrain(run)) {
    return [
      `Also stop the leaves ${run.run_id} has running?`,
      "OK stops them now and returns their tasks to the owner's backlog with a comment naming this drain.",
      "Cancel lets them finish and deliver first; the drain shows as cancelling until then.",
    ].join("\n\n");
  }
  return [
    `Also cancel the task runs ${run.run_id} started?`,
    "OK cancels them now. Cancel leaves them running to finish on their own.",
  ].join("\n\n");
}

// The latest failed action on the runs table. The table is re-synced from
// keyed nodes on every poll, and a node appended to it without a key is
// dropped by the next one, so an error written straight into the host would
// vanish within seconds, usually before it was read. It is held here and
// rendered with the rows until dismissed or the next action starts. Run detail
// supplies a stable feedback host separate from its rebuilt header.
let runActionError = null;
const RUN_ACTION_ERROR_KEY = "run-action-error";

function runActionErrorNode(build, dismiss) {
  const node = build();
  node.setAttribute("role", "alert");
  node.dataset.key = RUN_ACTION_ERROR_KEY;
  if (dismiss) {
    const button = el("button", { class: "action", text: "dismiss", title: "Dismiss this error" });
    button.addEventListener("click", (e) => {
      e.stopPropagation();
      dismiss();
    });
    node.appendChild(button);
  }
  return node;
}

function currentRuns() {
  return hasCtx("getLastRuns") ? _runsCtx.getLastRuns() : [];
}

function showRunActionError(host, text, build = () => el("div", { class: "action-error", text })) {
  if (host && host.id === "runs-body") {
    runActionError = { hash: text, build };
    renderRuns(currentRuns());
    return;
  }
  if (host) {
    const node = runActionErrorNode(build, () => node.remove());
    host.appendChild(node);
  }
}

function clearRunActionError(host) {
  runActionError = null;
  if (!host) return;
  for (const node of Array.from(host.children || [])) {
    if (node.dataset && node.dataset.key === RUN_ACTION_ERROR_KEY) node.remove();
  }
}

// The latest cancel's settlement report, kept across re-renders of the runs
// table so the poll that follows a cancel does not wipe it before it is read.
let cancelNotice = null;
onWorkspaceChange(() => {
  runActionError = null;
  cancelNotice = null;
});

function cancelNoticeFor(run, result) {
  const settlements = describePullSettlements(result && result.pull_settlements);
  const waiting = Array.isArray(result && result.waiting_leaves) ? result.waiting_leaves.length : 0;
  const forced = Array.isArray(result && result.forced_runs) ? result.forced_runs.length : 0;
  const unstopped = Array.isArray(result && result.unstopped_leaves) ? result.unstopped_leaves.length : 0;
  const children = Array.isArray(result && result.unstopped_children) ? result.unstopped_children : [];
  const cancelling = result && result.outcome === "cancelling";
  if (!settlements.text && !cancelling && forced === 0 && unstopped === 0 && children.length === 0) return null;
  const notStopped = (unstopped > 0
    ? ` Could not confirm ${unstopped} leaves stopped; their claims stay with the owner.`
    : "") + (children.length > 0
      ? ` Could not confirm detached children stopped: ${children.map((child) => `${child.child_run_id}: ${child.reason}`).join("; ")}.`
      : "");
  const head = cancelling
    ? `${run.run_id} cancelling: waiting for ${waiting} leaves.`
    : `${run.run_id} cancelled.${forced > 0 ? ` Stopped ${forced} running.` : ""}${notStopped}`;
  return {
    key: runIdentity(run),
    text: settlements.text ? `${head} Settlements: ${settlements.text}.` : head,
    attention: settlements.attention || unstopped > 0 || children.length > 0,
  };
}

function buildCancelNotice(notice, onDismiss) {
  const dismiss = el("button", { class: "action", text: "dismiss", title: "Dismiss this settlement report" });
  dismiss.addEventListener("click", (e) => {
    e.stopPropagation();
    onDismiss();
  });
  const node = el("div", { class: `operation-feedback run-cancel-notice ${notice.attention ? "error" : "success"}` }, [
    el("span", { text: notice.text }),
    dismiss,
  ]);
  node.setAttribute("role", "status");
  node.dataset.key = "run-cancel-notice";
  node.dataset.hash = `${notice.key}-${notice.text}`;
  return node;
}

async function cancelRun(run, btn, host) {
  if (!run || !run.run_id) return;
  const key = runIdentity(run);
  if (cancelRequestsInFlight.has(key)) return;
  const visit = captureWorkspaceVisit();
  cancelRequestsInFlight.add(key);
  try {
    await cancelRunInVisit(run, btn, host, visit);
  } finally {
    cancelRequestsInFlight.delete(key);
  }
}

async function cancelRunInVisit(run, btn, host, visit) {
  const runId = run && run.run_id;
  if (!runId) return;
  const requestPath = visit.path(runScopedPath(`/api/runs/${encodeURIComponent(runId)}/cancel`, run));
  const old = btn.textContent;
  btn.disabled = true;
  const pullClaim = await resolvePullClaim(run);
  if (!visit.isCurrent()) return;
  btn.disabled = !runIsCancellable(run);
  const reason = window.prompt(cancelPromptText({ ...run, pull_claim: pullClaim }), "");
  if (reason === null) return;
  const force = runIsDrain(run) && window.confirm(confirmForceText(run));
  let cancelled = false;
  btn.disabled = true;
  btn.innerHTML = `<span class="spinner"></span>cancel`;
  clearRunActionError(host);
  let notice = null;
  try {
    const result = await postJson(requestPath,
      { reason: reason.trim() || null, force });
    cancelled = true;
    if (!visit.isCurrent()) return;
    // The button must not offer a second cancel while the refresh is in flight.
    btn.textContent = result && result.outcome === "cancelling" ? "cancelling" : "cancelled";
    notice = cancelNoticeFor(run, result);
    // On the runs table the report renders at the top of the list (below);
    // elsewhere (run detail) it is appended to the host once the view refreshes.
    if (notice && host && host.id === "runs-body") cancelNotice = notice;
  } catch (e) {
    if (!visit.isCurrent()) return;
    showRunActionError(host, e.message || "cancel failed");
    console.error(e);
  } finally {
    // A cancelled run stays marked cancelled; only a failed attempt re-arms.
    if (!cancelled) {
      btn.disabled = false;
      btn.textContent = old;
    }
  }
  if (!cancelled) return;
  // The cancel is done. A refresh that then fails is a stale list, not a
  // failed cancel, and must not read as one: an operator who believes the
  // run is still live would cancel it again or go looking for what it wastes.
  try {
    const refreshActiveDetail = !run.workspace_id && getActiveRunId() === runId;
    await Promise.all([
      doFetchAndRenderRuns(),
      refreshActiveDetail ? doFetchAndRenderRunDetail() : Promise.resolve(),
      refreshActiveDetail ? doFetchAndRenderRunEvents() : Promise.resolve(),
    ]);
  } catch (e) {
    if (!visit.isCurrent()) return;
    showRunActionError(host, `${runId} was cancelled, but the view could not refresh: ${e.message || e}. Use Refresh to update it.`);
    console.error(e);
  }
  if (visit.isCurrent() && notice && host && host.id !== "runs-body") {
    const node = buildCancelNotice(notice, () => node.remove());
    host.appendChild(node);
  }
}

async function replayRun(run, btn, host) {
  if (!run || !run.run_id) return;
  const key = runIdentity(run);
  if (replayRequestsInFlight.has(key)) return;
  const visit = captureWorkspaceVisit();
  replayRequestsInFlight.add(key);
  try {
    await replayRunInVisit(run, btn, host, visit);
  } finally {
    replayRequestsInFlight.delete(key);
  }
}

function runTaskIds(run) {
  const ids = Array.isArray(run && run.task_ids) ? run.task_ids : [];
  const taskRows = Array.isArray(run && run.tasks) ? run.tasks : [];
  return [...new Set([
    ...ids,
    ...taskRows.map((task) => task && task.id),
    run && run.task_id,
  ].filter((id) => typeof id === "string" && id.trim()).map((id) => id.trim()))];
}

function runTaskLabels(run, taskIds = runTaskIds(run)) {
  const tasks = Array.isArray(run && run.tasks) ? run.tasks : [];
  return taskIds.map((id) => {
    const title = tasks.find((task) => task && task.id === id)?.title;
    return typeof title === "string" && title.trim() ? `${title.trim()} (${id})` : id;
  });
}

function activeRunForTask(run, taskIds, activeRuns) {
  if (taskIds.length === 0) return null;
  const matchingTasks = new Set(taskIds);
  return activeRuns.find((candidate) => candidate
    && candidate.run_id !== run.run_id
    && ["pending", "running", "retrying"].includes(candidate.state)
    && runTaskIds(candidate).some((id) => matchingTasks.has(id))) || null;
}

async function replayRunInVisit(run, btn, host, visit) {
  const runId = run && run.run_id;
  if (!runId) return;
  let activeRuns;
  try {
    const payload = await fetchJson(visit.path(runScopedPath("/api/job-runs?limit=200&state=active", run)));
    if (!Array.isArray(payload && payload.items) || payload.truncated) {
      throw new Error("the active run list is incomplete");
    }
    activeRuns = payload.items;
  } catch (e) {
    if (!visit.isCurrent()) return;
    showRunActionError(host, `Could not check for another live run, so replay was not started. ${e.message || e}`);
    console.error(e);
    return;
  }
  if (!visit.isCurrent()) return;
  const taskIds = runTaskIds(run);
  const task = runTaskLabels(run, taskIds).join(", ") || `the task recorded for ${runId}`;
  const job = run.job_id || "unknown job";
  let message = `Start a new ${job} run for ${task}? The original run ${runId} stays as is.`;
  const liveRun = activeRunForTask(run, taskIds, activeRuns);
  if (liveRun) {
    message += `\n\nThis task already has live run ${liveRun.run_id}; replay will start another run for the same task.`;
  }
  if (!window.confirm(message)) return;
  const old = btn.textContent;
  btn.disabled = true;
  btn.innerHTML = `<span class="spinner"></span>Replay run`;
  clearRunActionError(host);
  try {
    const payload = await postJson(visit.path(runScopedPath(`/api/runs/${encodeURIComponent(runId)}/replay`, run)));
    if (!payload.run_id) throw new Error("replay response did not include run_id");
    if (!visit.isCurrent()) return;
    doNavigateToRun(payload.run_id, run.workspace_id || visit.workspace);
    doFetchAndRenderRuns().catch(console.error);
  } catch (e) {
    if (!visit.isCurrent()) return;
    showRunActionError(host, e.message || "replay failed");
    console.error(e);
  } finally {
    btn.disabled = false;
    btn.textContent = old;
  }
}

// A 409 `resume_run_in_flight` names the run already carrying this lineage;
// link to it rather than showing the refusal as a bare failure.
function resumeErrorNode(error, run) {
  const liveRunId = error && error.code === "resume_run_in_flight" && error.body && error.body.run_id;
  if (!liveRunId) {
    return el("div", { class: "action-error", text: (error && error.message) || "resume failed" });
  }
  const open = el("button", {
    class: "action approve resume-live-run",
    text: `Open ${liveRunId}`,
    title: `Open live run ${liveRunId}`,
  });
  open.addEventListener("click", (e) => {
    e.stopPropagation();
    doNavigateToRun(liveRunId, run.workspace_id);
  });
  return el("div", { class: "action-error resume-conflict" }, [
    `Not resumed: ${liveRunId} is already resuming this run and is still live. `,
    open,
  ]);
}

async function resumeRun(run, btn, host) {
  const visit = captureWorkspaceVisit();
  const runId = run && run.run_id;
  if (!runId) return;
  const key = runIdentity(run);
  if (resumeRequestsInFlight.has(key)) return;
  const message = `Resume ${runId}? This creates a new run that re-runs the failed step and all subsequent steps. It succeeds only if the underlying cause is resolved.`;
  if (!window.confirm(message)) return;
  resumeRequestsInFlight.add(key);
  const old = btn.textContent;
  btn.disabled = true;
  btn.innerHTML = `<span class="spinner"></span>Resume`;
  clearRunActionError(host);
  let resumed = false;
  try {
    const payload = await postJson(visit.path(runScopedPath(`/api/job-runs/${encodeURIComponent(runId)}/resume`, run)));
    if (!payload.run_id) throw new Error("resume response did not include run_id");
    resumedRunIdsBySource.set(key, payload.run_id);
    resumed = true;
    if (!visit.isCurrent()) return;
  } catch (e) {
    if (!visit.isCurrent()) return;
    showRunActionError(host, (e && e.message) || "resume failed", () => resumeErrorNode(e, run));
    console.error(e);
  } finally {
    resumeRequestsInFlight.delete(key);
    btn.disabled = false;
    btn.textContent = old;
  }
  if (!resumed) return;
  try {
    await doFetchAndRenderRuns();
  } catch (e) {
    if (!visit.isCurrent()) return;
    // The resume was accepted; only the list is stale.
    showRunActionError(host, `${runId} was resumed, but the list could not refresh: ${(e && e.message) || e}. Use Refresh to update it.`);
    console.error(e);
  }
}

function coerceNumber(value) {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value !== "string" || value.trim() === "") return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function firstNumericField(row, keys) {
  for (const key of keys) {
    const value = coerceNumber(row && row[key]);
    if (value != null) return value;
  }
  return null;
}

function firstBooleanField(row, keys) {
  for (const key of keys) {
    const value = row && row[key];
    if (typeof value === "boolean") return value;
    if (typeof value === "string") {
      const normalized = value.trim().toLowerCase();
      if (normalized === "true") return true;
      if (normalized === "false") return false;
    }
  }
  return false;
}

function frictionRunId(row) {
  return (
    (row && (row.run_id || row.job_run || row.runId || row.jobRun)) ||
    ""
  );
}

function frictionRowText(row) {
  return [
    row && (row.kind || row.body_kind || row.event_kind || row.type || row.category),
    row && (row.command || row.tool || row.tool_name || row.status || row.outcome),
    row && (row.stderr || row.message || row.reason || row.error),
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
}

function frictionRowLooksDenied(row) {
  const text = frictionRowText(row).replace(/[_-]/g, " ");
  return /\bden(?:y|ied|ial|ials)\b/.test(text) || text.includes("policy deny");
}

function frictionRowLooksToolFail(row) {
  const exitCode = coerceNumber(row && row.exit_code);
  if (exitCode != null && exitCode !== 0) return true;
  if (firstBooleanField(row, ["timed_out", "timeout"])) return true;
  const text = frictionRowText(row).replace(/[_-]/g, " ");
  return text.includes("tool") && /\b(fail|failed|failure|timeout|timed out)\b/.test(text);
}

function frictionRowLooksLongRun(row) {
  if (firstBooleanField(row, ["long_run", "is_long_run", "long_running"])) return true;
  const text = frictionRowText(row).replace(/[_-]/g, " ");
  return text.includes("long run") || text.includes("long running");
}

function emptyRunFrictionSummary(run) {
  return {
    denials: 0,
    toolFails: 0,
    durationMs: coerceNumber(run && run.duration_ms),
    longRun: false,
  };
}

function addFrictionRowToSummary(summary, row) {
  const denials = firstNumericField(row, [
    "denials",
    "denial_count",
    "policy_denials",
  ]);
  if (denials != null) {
    summary.denials += denials;
  } else if (frictionRowLooksDenied(row)) {
    summary.denials += 1;
  }

  const toolFails = firstNumericField(row, [
    "tool_fails",
    "tool_failures",
    "tool_fail_count",
    "failed_tool_calls",
  ]);
  if (toolFails != null) {
    summary.toolFails += toolFails;
  } else if (frictionRowLooksToolFail(row)) {
    summary.toolFails += 1;
  }

  const durationMs = firstNumericField(row, [
    "duration_ms",
    "run_duration_ms",
    "wall_clock_ms",
    "elapsed_ms",
  ]);
  if (durationMs != null) {
    summary.durationMs = Math.max(summary.durationMs || 0, durationMs);
  }
  summary.longRun = summary.longRun || frictionRowLooksLongRun(row);
}

export function mergeRunsWithFriction(runs, frictionRows) {
  const byRun = new Map();
  for (const row of frictionRows || []) {
    const runId = frictionRunId(row);
    if (!runId) continue;
    if (!byRun.has(runId)) byRun.set(runId, emptyRunFrictionSummary());
    addFrictionRowToSummary(byRun.get(runId), row);
  }
  return (runs || []).map((run) => ({
    ...run,
    diagnostics_friction: mergeRunFrictionSummary(run, byRun.get(run.run_id)),
  }));
}

function mergeRunFrictionSummary(run, friction) {
  const base = emptyRunFrictionSummary(run);
  if (!friction) return base;
  return {
    ...base,
    ...friction,
    durationMs: friction.durationMs == null ? base.durationMs : friction.durationMs,
  };
}

function runTimestampValue(run) {
  const ts = run.finished_at || run.started_at || run.scheduled_at || run.created_at;
  const time = ts ? new Date(ts).getTime() : 0;
  return Number.isFinite(time) ? time : 0;
}

function runFriction(run) {
  return run.diagnostics_friction || emptyRunFrictionSummary(run);
}

function runDurationInfo(run) {
  const friction = runFriction(run);
  return { ...elapsedDurationInfo(run, friction.durationMs), longRun: friction.longRun };
}

function runSortValue(run, key) {
  const friction = runFriction(run);
  switch (key) {
    case "when":
      return runTimestampValue(run);
    case "job":
      return run.job_id || "";
    case "task":
      return (run.task_ids || []).join(" ");
    case "run_id":
      return run.run_id || "";
    case "denials":
      return friction.denials || 0;
    case "tool_fails":
      return friction.toolFails || 0;
    case "duration": {
      const { durationMs } = runDurationInfo(run);
      return durationMs || 0;
    }
    case "state":
      return run.state || "";
    default:
      return "";
  }
}

function compareRunValues(left, right) {
  if (typeof left === "number" && typeof right === "number") return left - right;
  return String(left).localeCompare(String(right));
}

function sortedRunsForDisplay(runs) {
  const rows = (runs || []).slice();
  rows.sort((a, b) => {
    const primary = compareRunValues(runSortValue(a, runSort.key), runSortValue(b, runSort.key));
    const directed = runSort.dir === "asc" ? primary : -primary;
    if (directed !== 0) return directed;
    return runTimestampValue(b) - runTimestampValue(a);
  });
  return rows;
}

function runMatchesFilter(run) {
  if (runFilter === "active") return ACTIVE_RUN_STATES.has(run && run.state);
  if (runFilter === "failed") return RESUMABLE_RUN_STATES.has(run && run.state);
  return true;
}

function persistRunFilters() {
  const url = new URL(window.location.href);
  if (runFilter === "all") url.searchParams.delete("run_state");
  else url.searchParams.set("run_state", runFilter);
  if (runTaskId) url.searchParams.set("task_id", runTaskId);
  else url.searchParams.delete("task_id");
  if (runJobId) url.searchParams.set("job_id", runJobId);
  else url.searchParams.delete("job_id");
  if (url.href !== window.location.href) history.replaceState(null, "", url);
}

function clearRunsFetchTimer() {
  if (runQueryTimer) {
    clearTimeout(runQueryTimer);
    runQueryTimer = null;
  }
}

function showRunsLoading() {
  if (hasCtx("markRunsLoading")) _runsCtx.markRunsLoading();
  renderRuns(currentRuns());
}

export function setRunFilter(value) {
  runFilter = RUN_FILTERS.has(value) ? value : "all";
  persistRunFilters();
  clearRunsFetchTimer();
  showRunsLoading();
  doFetchAndRenderRuns().catch((error) => console.error(error));
}

function setRunWindow(value) {
  if (!DASHBOARD_WINDOWS.includes(value) || value === getWindow()) return;
  setWindow(value);
  persistScopeToUrl();
  clearRunsFetchTimer();
  showRunsLoading();
  // The scope listener refreshes every open panel, including this list.
  notifyScopeChange();
}

function applyRunQuery(raw) {
  const value = String(raw || "").trim();
  if (!value) {
    runTaskId = "";
    runJobId = "";
  } else if (isTaskId(value)) {
    runTaskId = value;
    runJobId = "";
  } else {
    runTaskId = "";
    runJobId = value;
  }
  persistRunFilters();
  showRunsLoading();
  clearRunsFetchTimer();
  runQueryTimer = setTimeout(() => {
    runQueryTimer = null;
    doFetchAndRenderRuns().catch((error) => console.error(error));
  }, 250);
  if (runQueryTimer && typeof runQueryTimer.unref === "function") runQueryTimer.unref();
}

export function getRunFilter() {
  return runFilter;
}

export function formatRunCount(shown, fetched, meta) {
  const capped = hasCtx("getRunsLimitCapped") && _runsCtx.getRunsLimitCapped();
  const limit = `${capped ? "server limit" : "limit"} ${meta?.limit || fetched}`;
  if (meta && Number.isFinite(meta.total)) {
    const base = shown === fetched
      ? `${shown} shown`
      : `${shown} shown (of ${fetched} fetched)`;
    return meta.truncated
      ? `${base} · ${meta.total} total · ${limit}`
      : `${base} · ${meta.total} total`;
  }
  if (meta && meta.truncated) {
    return `${shown} shown · ${limit} · older matching runs not loaded`;
  }
  return shown === fetched
    ? `${shown} shown`
    : `${shown} shown of ${fetched} fetched`;
}

function runsAreLoading(runs, meta) {
  if (!(hasCtx("getRunsLoading") && _runsCtx.getRunsLoading())) return false;
  if (!runs || runs.length === 0) return true;
  if (meta && meta.runsQuery && meta.runsQuery !== runsQueryKey()) return true;
  const loadedState = meta && meta.state;
  return Boolean(loadedState && loadedState !== runFilter);
}

function windowPhrase() {
  const selected = getWindow();
  if (!selected || selected === "all") return "with no time window";
  return `in the ${selected} window`;
}

function runsEmptyText() {
  const qualifiers = [];
  if (runTaskId) qualifiers.push(`task ${runTaskId}`);
  if (runJobId) qualifiers.push(`job ${runJobId}`);
  const extra = qualifiers.length ? ` for ${qualifiers.join(", ")}` : "";
  if (runFilter === "failed") {
    return `No failed, timed-out, or interrupted job runs${extra} (${windowPhrase()}).`;
  }
  if (runFilter === "active") {
    return `No pending or running job runs${extra} (${windowPhrase()}).`;
  }
  return `No job runs${extra} in this workspace (${windowPhrase()}).`;
}

function runsScopeText() {
  let lead = "Every job run";
  if (runFilter === "failed") lead = "Failed, timed-out, and interrupted job runs";
  else if (runFilter === "active") lead = "Pending and running job runs";
  const qualifiers = ["newest first", windowPhrase()];
  if (runTaskId) qualifiers.push(`task ${runTaskId}`);
  if (runJobId) qualifiers.push(`job ${runJobId}`);
  return `${lead}, ${qualifiers.join(", ")}.`;
}

function runsScopeNote() {
  const note = el("div", {
    class: "runs-scope-note",
    text: runsScopeText(),
    title: "The count beside Runs uses these same Failed, Timeout, and Interrupted outcomes for the selected window. Click that count to open this list on Failed for that window. Health › Errors lists step and event failures in the selected window.",
  });
  note.dataset.key = "runs-scope-note";
  note.dataset.hash = `scope-${runFilter}-${getWindow()}-${runTaskId}-${runJobId}`;
  return note;
}

function runsLimitNote(meta) {
  if (!meta || !meta.truncated) return null;
  const total = Number.isFinite(meta.total) ? ` of ${meta.total}` : "";
  const container = el("div", {
    class: "runs-limit-note",
  });
  container.dataset.key = "runs-limit-note";
  const capped = hasCtx("getRunsLimitCapped") && _runsCtx.getRunsLimitCapped();
  const isLoading = hasCtx("getRunsLoading") && _runsCtx.getRunsLoading();
  container.dataset.hash = `limit-note-${meta.limit}-${meta.total || ""}-${capped ? "capped" : ""}-${isLoading ? "loading" : ""}`;

  const textSpan = el("span", {
    class: "runs-limit-text",
    text: `Showing newest ${meta.limit} matching runs${total}.`,
  });
  container.appendChild(textSpan);

  // The server caps one request; past that cap Load more would return the same rows.
  if (capped) {
    textSpan.textContent = `Showing newest ${meta.limit} matching runs${total}, the most one request returns.`;
    return container;
  }

  const button = el("button", {
    class: "action runs-load-more",
    text: isLoading ? "Loading…" : "Load more",
    title: "Load more runs",
  });
  button.type = "button";
  button.disabled = Boolean(isLoading);
  button.addEventListener("click", (e) => {
    e.stopPropagation();
    if (hasCtx("loadMoreRuns")) {
      button.disabled = true;
      button.textContent = "Loading…";
      _runsCtx.loadMoreRuns().catch((error) => console.error("Failed to load more runs", error));
    }
  });
  container.appendChild(button);
  return container;
}

function runsLoadingSkeleton() {
  return el("div", { class: "skeleton-state" }, [
    el("div", { class: "skeleton skeleton-row" }),
    el("div", { class: "skeleton skeleton-row", style: { width: "80%" } }),
    el("div", { class: "skeleton skeleton-row", style: { width: "90%" } }),
  ]);
}

function runFilterControls() {
  const controls = el("div", { class: "runs-filter", title: "Filter runs by state" });
  controls.setAttribute("role", "group");
  controls.setAttribute("aria-label", "Filter runs by state");
  for (const [value, label] of [["all", "All"], ["active", "Live"], ["failed", "Failed"]]) {
    const button = el("button", {
      class: `runs-filter-button${runFilter === value ? " active" : ""}`,
      text: label,
    });
    button.type = "button";
    button.setAttribute("aria-pressed", runFilter === value ? "true" : "false");
    button.addEventListener("click", () => setRunFilter(value));
    controls.appendChild(button);
  }
  return controls;
}

function runsQueryInput() {
  const input = el("input", { class: "runs-query" });
  input.type = "search";
  input.placeholder = "Task or job id";
  input.setAttribute("aria-label", "Filter runs by task or job id");
  input.title = "A task id such as ORB-15159 filters by task. Any other id filters by job. The address can set both.";
  input.autocomplete = "off";
  input.spellcheck = false;
  input.tabIndex = 0;
  input.value = runTaskId || runJobId;
  input.addEventListener("input", () => applyRunQuery(input.value));
  return input;
}

function runsWindowControls() {
  const controls = el("div", { class: "runs-window", title: "Time window" });
  controls.setAttribute("role", "group");
  controls.setAttribute("aria-label", "Time window");
  const selected = getWindow();
  for (const value of DASHBOARD_WINDOWS) {
    const button = el("button", {
      class: `runs-filter-button${selected === value ? " active" : ""}`,
      text: value,
    });
    button.type = "button";
    button.dataset.window = value;
    button.setAttribute("aria-pressed", selected === value ? "true" : "false");
    button.addEventListener("click", () => setRunWindow(value));
    controls.appendChild(button);
  }
  return controls;
}

function runsToolbar() {
  // The search field's node stays while the state and window segments are
  // rebuilt: its identity is the toolbar, and this hash ignores the typed
  // query so a keystroke does not replace the focused input.
  const toolbar = el("div", { class: "runs-toolbar" });
  toolbar.dataset.key = "runs-toolbar";
  toolbar.dataset.hash = `runs-toolbar-${runFilter}-${getWindow()}`;
  toolbar.appendChild(runFilterControls());
  toolbar.appendChild(runsQueryInput());
  toolbar.appendChild(runsWindowControls());
  return toolbar;
}

function unavailableSourcesNode(unavailable) {
  if (!Array.isArray(unavailable) || unavailable.length === 0) return null;
  const names = unavailable.map((source) => source.workspace_name || source.workspace_id || "unknown");
  const node = el("div", {
    class: "runs-source-warning",
    text: `Unavailable workspace${names.length === 1 ? "" : "s"}: ${names.join(", ")}`,
    title: unavailable.map((source) => `${source.workspace_name || source.workspace_id}: ${source.error || "unavailable"}`).join("\n"),
  });
  node.dataset.key = "runs-source-warning";
  node.dataset.hash = `${names.join("|")}-${unavailable.map((source) => source.error || "").join("|")}`;
  return node;
}

function setRunSort(key) {
  if (runSort.key === key) {
    runSort = { key, dir: runSort.dir === "asc" ? "desc" : "asc" };
  } else {
    runSort = { key, dir: RUN_SORT_DEFAULT_DIR[key] || "asc" };
  }
  const data = hasCtx("getLastRuns") ? _runsCtx.getLastRuns() : [];
  renderRuns(data);
}

function runHeaderCell(label, key, opts = {}) {
  const classes = [opts.class, opts.num ? "num" : ""].filter(Boolean).join(" ");
  const cell = el("span", { class: classes, style: opts.style });
  const button = el("button", {
    class: `runs-sort${runSort.key === key ? " active" : ""}`,
    text: label,
    title: `Sort Recent Runs by ${label}`,
  });
  button.type = "button";
  if (runSort.key === key) {
    button.appendChild(el("span", {
      class: "sort-arrow",
      text: runSort.dir === "asc" ? " ▲" : " ▼",
    }));
  }
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    setRunSort(key);
  });
  cell.appendChild(button);
  return cell;
}

function runCountCell(value, kind) {
  return el("span", {
    class: `num ${kind}${value > 0 ? " hot" : ""}`,
    text: String(value || 0),
  });
}

function runDurationCell(run) {
  const { durationMs, isLive, longRun } = runDurationInfo(run);
  const formatted = fmtDurationValue(durationMs);
  const text = (isLive && formatted !== "-") ? `${formatted} ↻` : formatted;
  return el("span", { class: "duration" }, [
    text,
    longRun
      ? el("span", { class: "long-run-flag", text: "!", title: "Long run" })
      : null,
  ]);
}

// Keep workspace scope in the URL, including when an aggregate row opens a task.
export function runTaskLinks(run) {
  const tasks = Array.isArray(run.tasks) ? run.tasks : (run.task_ids || []).map(id => ({ id }));
  const cell = el("span", { class: "run-tasks" });
  if (tasks.length === 0) cell.textContent = "—";
  for (const task of tasks) {
    const link = el("a", { class: "run-task-link", title: task.title ? `${task.id}: ${task.title}` : task.id }, [
      el("span", { class: "run-task-id", text: task.id }),
      task.title ? el("span", { class: "run-task-title", text: task.title }) : null,
    ]);
    const url = new URL(window.location.href);
    url.searchParams.set("workspace", run.workspace_id || getWorkspace() || "");
    url.hash = `tasks?status=all&q=${encodeURIComponent(task.id)}`;
    link.href = `${url.search}${url.hash}`;
    link.addEventListener("click", event => event.stopPropagation());
    cell.appendChild(link);
  }
  return cell;
}

// The step whose error explains a failed run: the newest step that ran and
// recorded an error, else the newest step that ran. Mirrors the server's
// `run_error_step`, which the list payload does not name.
function runErrorStep(run) {
  const steps = Array.isArray(run && run.steps) ? run.steps.filter(Boolean) : [];
  const ran = steps.filter((step) => step.state !== "skipped");
  return ran.slice().reverse().find((step) => step.error_code || step.error_message)
    || ran[ran.length - 1]
    || steps[steps.length - 1]
    || null;
}

function truncateText(text, max) {
  return text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text;
}

// Why a failed, timed-out or interrupted run ended; null for any other state.
function runFailure(run) {
  if (!runIsResumable(run)) return null;
  const step = runErrorStep(run);
  const stepId = step && step.target_id ? String(step.target_id) : "";
  const code = (step && step.error_code) || run.error_code || "";
  const message = (step && step.error_message) || run.error_message || "";
  const detail = [code, message].filter(Boolean).join(": ").replace(/\s+/g, " ").trim();
  return {
    stepId,
    excerpt: truncateText(detail, FAILED_AT_EXCERPT_CHARS),
    title: [stepId && `Step: ${stepId}`, code && `Code: ${code}`, message && `Message: ${message}`]
      .filter(Boolean).join("\n"),
  };
}

// The pipeline name without its shared affixes: "gate" for task_gate_pipeline.
function pipelineShortName(jobId) {
  return String(jobId || "").replace(/^task_/, "").replace(/_pipeline$/, "");
}

function runFailedAtCell(run, { withJob = false } = {}) {
  const cell = el("span", { class: "run-failed-at" });
  const failure = runFailure(run);
  if (!failure) return cell;
  if (!failure.stepId && !failure.excerpt) {
    cell.classList.add("empty");
    cell.textContent = "no error recorded";
    return cell;
  }
  const where = [withJob ? pipelineShortName(run.job_id) : "", failure.stepId].filter(Boolean).join(" › ");
  if (where) cell.appendChild(el("span", { class: "run-failed-step", text: where }));
  if (failure.excerpt) cell.appendChild(el("span", { class: "run-failed-excerpt", text: failure.excerpt }));
  cell.title = failure.title;
  return cell;
}

function runFailureHash(run) {
  const failure = runFailure(run);
  return failure ? `${failure.stepId}|${failure.excerpt}` : "";
}

// Runs of one task's pipelines share a group; a task with a single such run in
// the list stays a plain row. Groups sit where their first run sorts. Only the
// loaded page is grouped: a triplet split across Load more pages folds once its
// other runs arrive.
function groupRunsForDisplay(sorted) {
  const entries = [];
  const groups = new Map();
  for (const run of sorted) {
    const taskIds = PIPELINE_GROUP_JOBS.has(run.job_id) ? runTaskIds(run) : [];
    if (taskIds.length !== 1) {
      entries.push({ run });
      continue;
    }
    const key = `${run.workspace_id || getWorkspace() || ""}:${taskIds[0]}`;
    let group = groups.get(key);
    if (!group) {
      group = { key, runs: [] };
      groups.set(key, group);
      entries.push(group);
    }
    group.runs.push(run);
  }
  return entries.map((entry) => (entry.runs && entry.runs.length === 1 ? { run: entry.runs[0] } : entry));
}

function runGroupState(runs) {
  const states = new Set(runs.map((run) => run.state));
  return GROUP_STATE_PRIORITY.find((state) => states.has(state)) || runs[0].state;
}

function runGroupRow(group, attributed) {
  const { runs, key } = group;
  const expanded = expandedRunGroups.has(key);
  const first = runs[0];
  const latest = runs.reduce((a, b) => (runTimestampValue(b) > runTimestampValue(a) ? b : a));
  const ts = latest.finished_at || latest.started_at || latest.scheduled_at || latest.created_at;
  const failed = runs.filter(runIsResumable);
  const frictions = runs.map(runFriction);
  const denials = frictions.reduce((sum, f) => sum + (f.denials || 0), 0);
  const toolFails = frictions.reduce((sum, f) => sum + (f.toolFails || 0), 0);
  const jobNames = runs.map((run) => run.job_id).join(", ");
  const toggle = el("button", {
    class: "id run-group-toggle",
    text: `${expanded ? "▾" : "▸"} ${runs.length} pipeline runs`,
    title: `${expanded ? "Hide" : "Show"} ${jobNames}`,
  });
  const failedAt = failed.length ? runFailedAtCell(failed[0], { withJob: true }) : el("span", { class: "run-failed-at" });
  if (failed.length > 1) failedAt.title = failed.map((run) => `${run.job_id}\n${runFailure(run).title}`).join("\n\n");
  const row = el("div", {
    class: `runs-row runs-group${attributed ? " workspace-attributed" : ""}`,
    title: `${runs.length} pipeline runs for this task: ${jobNames}`,
  }, [
    el("span", { class: "state" }, [stateCell(runGroupState(runs))]),
    attributed ? el("span", { class: "run-workspace", text: first.workspace_name || first.workspace_id, title: first.workspace_id }) : null,
    toggle,
    runTaskLinks(first),
    failedAt,
    el("span", { class: "run-id-cell" }, [el("span", { class: "run-group-count", text: `${runs.length} runs` })]),
    el("span", { class: "when", text: fmtTimestampValue(ts), title: ts ? formatDateTime(ts) : "" }),
    runCountCell(denials, "denials"),
    runCountCell(toolFails, "tool-fails"),
    el("span", { class: "duration", text: "—" }),
    el("span", { class: "run-actions" }),
  ]);
  row.dataset.key = `group-${key}`;
  row.dataset.hash = `group-${key}-${expanded}-${ts}-${denials}-${toolFails}-${JSON.stringify(first.tasks)}-${runs.map((run) => `${runIdentity(run)}:${run.state}:${runFailureHash(run)}`).join("|")}`;
  row.style.cursor = "pointer";
  makeRowDisclosure(row, toggle, {
    expanded,
    onToggle: () => {
      if (expandedRunGroups.has(key)) expandedRunGroups.delete(key);
      else expandedRunGroups.add(key);
      renderRuns(currentRuns());
    },
  });
  return row;
}

function runRow(r, attributed, body, { child = false } = {}) {
  const ts = r.finished_at || r.started_at || r.scheduled_at || r.created_at;
  const friction = runFriction(r);
  const { durationMs, isLive } = runDurationInfo(r);
  const formattedDuration = fmtDurationValue(durationMs);
  const runIdSpan = makeCopyButton(r.run_id, { class: "run-id", title: "Copy run ID" });
  const runIdCell = el("span", { class: "run-id-cell" }, [runIdSpan]);
  if (r.retry_source_run_id) {
    const sourceId = r.retry_source_run_id;
    const lineage = el("button", {
      class: "run-lineage",
      text: `from ${sourceId}`,
      title: `Open source run ${sourceId}`,
    });
    lineage.addEventListener("click", (e) => {
      e.stopPropagation();
      doNavigateToRun(sourceId, r.workspace_id);
    });
    runIdCell.appendChild(lineage);
  }
  const resumedAsId = resumedRunIdsBySource.get(runIdentity(r));
  if (resumedAsId) {
    const lineage = el("button", {
      class: "run-lineage resumed-as",
      text: `resumed as ${resumedAsId}`,
      title: `Open resumed run ${resumedAsId}`,
    });
    lineage.addEventListener("click", (e) => {
      e.stopPropagation();
      doNavigateToRun(resumedAsId, r.workspace_id);
    });
    runIdCell.appendChild(lineage);
  }
  // The job cell is the row's button: the row also holds the copy-id,
  // lineage and run actions, so it cannot be a button itself. Its text is
  // clipped in a narrow column, so its tooltip is the full job name.
  const openButton = el("button", { class: "id", text: r.job_id, title: r.job_id });
  const rowCells = [
    el("span", { class: "state" }, [stateCell(r.state)]),
    attributed ? el("span", { class: "run-workspace", text: r.workspace_name || r.workspace_id, title: r.workspace_id }) : null,
    openButton,
    runTaskLinks(r),
    runFailedAtCell(r),
    runIdCell,
    el("span", { class: "when", text: fmtTimestampValue(ts), title: ts ? formatDateTime(ts) : "" }),
    runCountCell(friction.denials, "denials"),
    runCountCell(friction.toolFails, "tool-fails"),
    runDurationCell(r),
    el("span", { class: "run-actions" }, [
      runIsCancellable(r) ? buildCancelRunButton(r, body) : null,
      runIsResumable(r) ? buildResumeRunButton(r, body) : null,
    ]),
  ];
  const row = el("div", { class: `runs-row${attributed ? " workspace-attributed" : ""}${child ? " runs-group-child" : ""}`, title: `${r.run_id} (click to inspect)` }, rowCells);
  row.dataset.key = `run-${runIdentity(r)}`;
  row.dataset.hash = `${runIdentity(r)}-${ts}-${r.duration_ms}-${r.state}-${r.retry_source_run_id || ""}-${resumedAsId || ""}-${resumeRequestsInFlight.has(runIdentity(r)) ? "resuming" : ""}-${friction.denials}-${friction.toolFails}-${durationMs}-${formattedDuration}-${isLive ? "live" : ""}-${friction.longRun}-${child ? "child" : ""}-${runFailureHash(r)}`;
  row.style.cursor = "pointer";
  row.dataset.hash += `-${JSON.stringify(r.tasks)}-${JSON.stringify(r.task_ids)}`;
  // A run row opens the run detail view rather than disclosing inline, so its
  // button has no expansion state.
  makeRowDisclosure(row, openButton, { onToggle: () => doNavigateToRun(r.run_id, r.workspace_id) });
  return row;
}

export function renderRuns(runs) {
  if (!panelCanRender("runs-body")) return;
  const body = $("runs-body");
  enableRovingRows(body);
  const frag = document.createDocumentFragment();
  const unavailable = hasCtx("getRunSourcesUnavailable") ? _runsCtx.getRunSourcesUnavailable() : [];
  const meta = hasCtx("getRunsMeta") ? _runsCtx.getRunsMeta() : null;
  const attributed = (runs || []).some((run) => !!run.workspace_id);
  const loading = runsAreLoading(runs, meta);
  const filtered = (runs || []).filter(runMatchesFilter);
  const sorted = sortedRunsForDisplay(filtered);
  const top = sorted;
  if ($("diag-count")) {
    $("diag-count").textContent = loading ? "…" : formatRunCount(top.length, sorted.length, meta);
  }
  frag.appendChild(runsToolbar());
  frag.appendChild(runsScopeNote());
  if (cancelNotice) {
    frag.appendChild(buildCancelNotice(cancelNotice, () => {
      cancelNotice = null;
      renderRuns(hasCtx("getLastRuns") ? _runsCtx.getLastRuns() : runs);
    }));
  }
  if (runActionError) {
    const errorNode = runActionErrorNode(runActionError.build, () => {
      runActionError = null;
      renderRuns(currentRuns());
    });
    errorNode.dataset.hash = runActionError.hash;
    frag.appendChild(errorNode);
  }
  const unavailableNode = unavailableSourcesNode(unavailable);
  if (unavailableNode) frag.appendChild(unavailableNode);
  const limitNote = loading ? null : runsLimitNote(meta);
  if (limitNote) frag.appendChild(limitNote);
  if (loading) {
    frag.appendChild(runsLoadingSkeleton());
    syncNodes(body, Array.from(frag.children));
    return;
  }
  if (top.length === 0) {
    frag.appendChild(el("div", { class: "empty-state" }, [
      el("div", { class: "icon", text: "✧" }),
      el("div", { class: "text", text: runsEmptyText() }),
    ]));
    syncNodes(body, Array.from(frag.children));
    return;
  }
  const headerCells = [
    runHeaderCell("State", "state"),
    attributed ? el("span", { class: "runs-workspace-header", text: "Workspace" }) : null,
    runHeaderCell("Job", "job"),
    runHeaderCell("Task", "task"),
    el("span", { class: "run-failed-at-header", text: "Failed at", title: "The step a failed, timed-out or interrupted run stopped at, and its error" }),
    runHeaderCell("Run ID", "run_id"),
    runHeaderCell("When", "when"),
    runHeaderCell("Denials", "denials", { num: true }),
    runHeaderCell("Tool fails", "tool_fails", { num: true }),
    runHeaderCell("Duration", "duration", { class: "duration", style: { textAlign: "right" } }),
    el("span", { class: "run-actions-header", text: "Actions", style: { textAlign: "right" } }),
  ];
  const header = el("div", { class: `runs-row runs-header col-head${attributed ? " workspace-attributed" : ""}` }, headerCells);
  header.dataset.key = "runs-header";
  header.dataset.hash = `header-${runSort.key}-${runSort.dir}-${attributed ? "workspace" : "scoped"}`;
  frag.appendChild(header);
  for (const entry of groupRunsForDisplay(top)) {
    if (!entry.runs) {
      frag.appendChild(runRow(entry.run, attributed, body));
      continue;
    }
    frag.appendChild(runGroupRow(entry, attributed));
    if (expandedRunGroups.has(entry.key)) {
      for (const r of entry.runs) frag.appendChild(runRow(r, attributed, body, { child: true }));
    }
  }
  syncNodes(body, Array.from(frag.children));
}

export {
  runIsCancellable,
  runIsResumable,
  buildCancelRunButton,
  buildReplayRunButton,
  buildResumeRunButton,
};
