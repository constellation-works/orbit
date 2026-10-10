// What the Tasks list says about running work beyond the task row itself:
// when each running job run started (`/api/job-runs?state=running`) and which
// files each task holds locked (`/api/tasks/locks`). app.js fetches both with
// the task list and hands them here; the rows read them back per task and
// repaint only when what a row shows changed.

import { onWorkspaceChange } from './common.js';

let runStarts = new Map();
let lockFiles = new Map();
let lockTotal = null;
let signature = "";
const listeners = new Set();

function publish() {
  const next = [
    [...runStarts].map(([id, started]) => `${id}@${started}`).join(","),
    JSON.stringify([...lockFiles].sort(([left], [right]) => left.localeCompare(right))),
    lockTotal ?? "",
  ].join("|");
  if (next === signature) return;
  signature = next;
  for (const listener of listeners) listener();
}

/// Take a `/api/job-runs?state=running` page. A run without a readable start
/// time is left out, so its row shows no elapsed time rather than a wrong one.
export function setRunningRuns(payload) {
  const runs = Array.isArray(payload?.items) ? payload.items : Array.isArray(payload) ? payload : [];
  const next = new Map();
  for (const run of runs) {
    const started = Date.parse(run?.started_at || "");
    if (typeof run?.run_id === "string" && Number.isFinite(started)) next.set(run.run_id, started);
  }
  runStarts = next;
  publish();
}

/// Take a `/api/tasks/locks` payload: each task's locked files and the total.
export function setTaskLocks(payload) {
  const next = new Map();
  for (const task of Array.isArray(payload?.by_task) ? payload.by_task : []) {
    const files = Array.isArray(task?.context_files) ? task.context_files.map(String) : [];
    if (task?.id) next.set(String(task.id), files);
  }
  lockFiles = next;
  const total = Number(payload?.total_locked);
  lockTotal = Number.isFinite(total) ? total : null;
  publish();
}

export function onTaskRuntimeChange(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/// When `runId` started, in epoch milliseconds, or null when unknown here: a
/// run recorded on another host is not in this host's running list.
export function runStartedAt(runId) {
  return runId ? runStarts.get(runId) ?? null : null;
}

/// The files `taskId` holds locked (empty when it holds none or locks are unknown).
export function taskLockFiles(taskId) {
  return lockFiles.get(taskId) || [];
}

/// Every file locked in the workspace, or null before the first read.
export function lockedFileTotal() {
  return lockTotal;
}

/// Elapsed time at minute resolution (`42m`, `3h 05m`, `2d 4h`), so a row
/// redraws once a minute rather than on every poll.
export function elapsedLabel(ms) {
  if (!Number.isFinite(ms) || ms < 0) return "";
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 1) return "<1m";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

// Both reads belong to one workspace; drop them when the scope moves.
onWorkspaceChange(() => {
  runStarts = new Map();
  lockFiles = new Map();
  lockTotal = null;
  publish();
});
