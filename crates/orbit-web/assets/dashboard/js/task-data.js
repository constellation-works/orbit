// Orbit dashboard task detail cache and loaders, and the drain-readiness
// fetch. Split out of tasks.js; re-renders through task-shared's `rerender`
// and never imports tasks.js.

import { fetchJson, isAggregateView } from './common.js';
import { drainReadinessEntry, drainReadinessFailure } from './drain-waits.js';
import { rerender, taskList, taskState, taskMutationPath, taskWorkspacePath, taskDispatchIdentity } from './task-shared.js';

// DANI-10391: a list row is a summary (`projection: "summary"`). It carries what
// the collapsed row renders and filters on, plus counts, but the bodies, the
// review gate, and the evidence a governed status transition needs live behind
// GET /api/tasks/:id. `taskDetails` keeps the last full projection per task so
// an open detail repaints from it across the 30 s refresh instead of flashing a
// placeholder; `taskDetailLoads` records one read per list payload, retaining
// its source summary object (every list fetch produces new objects), so a
// render never issues a second read for the same row and a failed read is not
// retried until the list refreshes or the row is reopened.
// All task-owned maps use the workspace-scoped dispatch identity: aggregate
// rows may share an id, but must never share projections or pending reads.
let taskDetails = new Map();
let taskDetailLoads = new Map();
// Drain readiness for an open proposed or backlog task: why the drain is or is
// not taking it, read per task (`GET /api/workflows/auto/readiness?task=<id>`)
// so the Readiness card answers without the Drain card's snapshot and for a
// task past its bound. One read per summary object, like `taskDetailLoads`; a
// read in flight keeps the last answer on screen.
const DRAIN_READINESS_STATUSES = new Set(["proposed", "backlog"]);
let taskDrainLoads = new Map();

export function clearTaskDataCaches() {
  taskDetails.clear();
  taskDetailLoads.clear();
  taskDrainLoads.clear();
}

function isTaskSummary(task) {
  return Boolean(task) && task.projection === "summary";
}

// The aggregate view's rows carry their owning workspace, which the detail and
// mutation endpoints do not echo back. A full projection replacing a row keeps
// those fields, or the row would lose its badge and, with it, its mutation
// target (canMutateTask) after the first edit.
const LIST_ONLY_TASK_FIELDS = ["workspace_id", "workspace_name", "workspace_root"];

export function mergeTaskRecord(previous, updated) {
  if (!previous || !updated) return updated;
  const merged = { ...updated };
  for (const key of LIST_ONLY_TASK_FIELDS) {
    if (merged[key] == null && previous[key] != null) merged[key] = previous[key];
  }
  return merged;
}

// Capture the cached object's identity before starting a list request.
export function taskDetailSnapshot(task) {
  return taskDetails.get(taskDispatchIdentity(task));
}

// The Tasks refresh can carry its selected task's full projection in the list
// response. Seed the same cache the detail endpoint owns so rendering that
// refreshed row does not issue a duplicate GET for the same task.
export function primeTaskDetail(summary, full, snapshot) {
  if (!summary?.id || full?.id !== summary.id || !isTaskSummary(summary)) return;
  // A save or detail read completed after this list request began. Keep that
  // newer projection; the new summary will trigger a fresh detail GET instead
  // of treating the list's potentially pre-save projection as already loaded.
  if (taskDetails.get(taskDispatchIdentity(summary)) !== snapshot) return;
  const merged = mergeTaskRecord(summary, full);
  taskDetails.set(taskDispatchIdentity(summary), merged);
  taskDetailLoads.set(taskDispatchIdentity(summary), {
    source: summary,
    pending: false,
    error: null,
    promise: Promise.resolve(merged),
  });
}

export function applyUpdatedTask(updatedTask, context, owner = updatedTask) {
  if (!updatedTask || !updatedTask.id) return;
  updatedTask = mergeTaskRecord(owner, updatedTask);
  // A pending read may contain a snapshot from before this completed update.
  if (taskDetailLoads.get(taskDispatchIdentity(updatedTask))?.pending) taskDetailLoads.delete(taskDispatchIdentity(updatedTask));
  const previous = taskList(context).find((task) => task && taskDispatchIdentity(task) === taskDispatchIdentity(updatedTask));
  const merged = mergeTaskRecord(previous, updatedTask);
  if (!isTaskSummary(merged)) taskDetails.set(taskDispatchIdentity(merged), merged);
  if (context && typeof context.replaceTask === "function") {
    context.replaceTask(merged);
  }
  if (taskState.pinnedExternalTask && taskState.pinnedExternalTask.task && taskDispatchIdentity(taskState.pinnedExternalTask.task) === taskDispatchIdentity(merged)) {
    taskState.pinnedExternalTask.task = merged;
  }
  return merged;
}

export function currentTaskRecord(task, context) {
  if (!task || !task.id) return task;
  return taskList(context).find((current) => current && taskDispatchIdentity(current) === taskDispatchIdentity(task)) ||
    (taskState.pinnedExternalTask && taskState.pinnedExternalTask.task && taskDispatchIdentity(taskState.pinnedExternalTask.task) === taskDispatchIdentity(task)
      ? taskState.pinnedExternalTask.task
      : null) ||
    taskDetails.get(taskDispatchIdentity(task)) ||
    task;
}

export function currentCrewTask(task, context) {
  return currentTaskRecord(task, context);
}

// Read the full projection behind a summary row. One read per summary object:
// a repeat call for the same row returns the same promise, settled or not, so
// renders and a status change racing on the same row share it.
export function loadTaskDetail(task, context) {
  const existing = taskDetailLoads.get(taskDispatchIdentity(task));
  if (existing && existing.source === task) return existing.promise;
  const load = { source: task, pending: true, error: null, promise: null };
  if (isAggregateView() && !task.workspace_id) {
    load.pending = false;
    load.error = "Task workspace is unknown";
    load.promise = Promise.reject(new Error(load.error));
    taskDetailLoads.set(taskDispatchIdentity(task), load);
    return load.promise;
  }
  load.promise = fetchJson(taskMutationPath(task)).then(
    (full) => {
      load.pending = false;
      if (taskDetailLoads.get(taskDispatchIdentity(task)) === load) {
        applyUpdatedTask(full, context, task);
        rerender(context);
      }
      return taskDetailLoads.get(taskDispatchIdentity(task)) === load ? full : (taskDetails.get(taskDispatchIdentity(task)) || full);
    },
    (error) => {
      load.pending = false;
      load.error = error.message || String(error);
      if (taskDetailLoads.get(taskDispatchIdentity(task)) === load) rerender(context);
      throw error;
    },
  );
  taskDetailLoads.set(taskDispatchIdentity(task), load);
  return load.promise;
}

// What an expanded row can render right now: the full projection when the row
// is one or the last one fetched, plus whether a fresh read is pending or
// failed. Starts the read for a summary row that has none yet.
export function taskDetailState(task, context) {
  if (!isTaskSummary(task)) return { task, pending: false, error: null };
  const load = taskDetailLoads.get(taskDispatchIdentity(task));
  if (!load || load.source !== task) {
    loadTaskDetail(task, context).catch(() => {});
  }
  const current = taskDetailLoads.get(taskDispatchIdentity(task));
  return {
    task: taskDetails.get(taskDispatchIdentity(task)) || null,
    pending: Boolean(current && current.pending),
    error: current ? current.error : null,
  };
}

// Drop the recorded read so the next render issues a fresh one.
export function forgetTaskDetailLoad(task) {
  taskDetailLoads.delete(taskDispatchIdentity(task));
  taskDrainLoads.delete(taskDispatchIdentity(task));
}

function loadTaskDrainReadiness(task, context) {
  const previous = taskDrainLoads.get(taskDispatchIdentity(task));
  const load = { source: task, pending: true, error: null, entry: previous ? previous.entry : null };
  taskDrainLoads.set(taskDispatchIdentity(task), load);
  if (isAggregateView() && !task.workspace_id) {
    load.pending = false;
    load.entry = null;
    load.error = "Task workspace is unknown";
    return;
  }
  fetchJson(taskWorkspacePath(task, `/api/workflows/auto/readiness?task=${encodeURIComponent(task.id)}`)).then(
    (payload) => {
      const entry = Array.isArray(payload?.tasks) ? payload.tasks.find((item) => item?.task_id === task.id) : null;
      load.entry = entry || null;
      load.error = entry ? null : "The readiness snapshot does not mention this task";
    },
    (error) => {
      load.entry = null;
      load.error = error.message || String(error);
    },
  ).finally(() => {
    load.pending = false;
    if (taskDrainLoads.get(taskDispatchIdentity(task)) === load) rerender(context);
  });
}

// What the Readiness card knows of the drain for an open task: its own read's
// entry, else the Drain card's snapshot entry, and whether a read is pending
// or failed. Null for a status the drain does not consider.
export function taskDrainState(task, context) {
  if (!DRAIN_READINESS_STATUSES.has(task.status)) return null;
  const sharedEntry = drainReadinessEntry(task);
  const sharedError = drainReadinessFailure(task);
  const load = taskDrainLoads.get(taskDispatchIdentity(task));
  if (sharedError) {
    if (!load || load.source !== task || load.error !== sharedError || load.pending) {
      taskDrainLoads.set(taskDispatchIdentity(task), {
        source: task,
        pending: false,
        error: sharedError,
        entry: sharedEntry,
        sharedSnapshot: true,
      });
    }
  } else if (sharedEntry) {
    if (!load || load.source !== task || load.entry !== sharedEntry || load.pending || load.error) {
      taskDrainLoads.set(taskDispatchIdentity(task), {
        source: task,
        pending: false,
        error: null,
        entry: sharedEntry,
        sharedSnapshot: true,
      });
    }
  } else if (!load || load.source !== task || load.sharedSnapshot) {
    loadTaskDrainReadiness(task, context);
  }
  const current = taskDrainLoads.get(taskDispatchIdentity(task));
  return {
    // A failed read keeps the snapshot entry standing in; without one the
    // caller shows the error as unknown.
    entry: current.entry || drainReadinessEntry(task),
    pending: current.pending,
    error: current.error,
  };
}

// An answer diffs on what it says; until one arrives, on the read's state.
export function taskDrainSignature(drain) {
  if (!drain) return "";
  return drain.entry ? JSON.stringify(drain.entry) : `${drain.pending}-${drain.error || ""}`;
}
