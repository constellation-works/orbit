// Why a backlog task is not starting: the drain readiness snapshot, shared by
// the Drain card (operations.js) and the Tasks list (tasks.js).
// The Drain card owns the fetch and hands each payload to `setDrainReadiness`;
// the Tasks list reads each task's pool and wait back for its readiness dot and
// the Backlog card's readiness strip, and repaints on a change. A task detail's
// Readiness card asks for its own task's entry and words it with the same
// `drainReadinessView`, so the dot and the card never disagree.

import { onWorkspaceChange } from './common.js';

// Readiness reasons that mean "waiting on another task or run": a context lock, a
// grouped member, a same-wave deferral, a live child's claim, or a task with no
// footprint waiting for the other leaves to finish so it can run alone.
export const DRAIN_LOCK_REASONS = new Set(["context_lock_conflict", "group_member_conflict", "conflict_deferred", "claimed_by_live_child", "awaiting_exclusive_slot"]);

// Readiness reasons that mean "waiting on slots or host pressure": a full
// workspace, a throttle, a scheduled shutdown or stopped admissions.
const DRAIN_CAPACITY_REASONS = new Set(["capacity_saturated", "resource_throttled", "host_shutdown_scheduled", "admissions_stopped", "cpu_light_budget_full"]);

// A task whose `os:` tags this host's OS does not satisfy waits for a host of
// its own (a pull-drain follower on that OS, say), and one a pilot found needs
// native evidence from another OS waits for the `os:` tag readiness names.
const DRAIN_HOST_WAIT_REASONS = new Set(["host_os_mismatch", "native_os_required"]);

export function drainReason(task) {
  return typeof task.reason === "string" && task.reason.trim() ? task.reason : "unknown";
}

// Holder ids for a lock-blocked row. Context locks report
// `conflicts[].locking_task_id`; same-wave deferrals report
// `conflicts[].blocking_task_id` plus `blocking_task_ids`; live-child claims
// report only `run_ids`, which have no task holder.
function drainHolders(task) {
  const holders = new Set();
  for (const conflict of Array.isArray(task.conflicts) ? task.conflicts : []) {
    for (const holder of [conflict?.locking_task_id, conflict?.blocking_task_id]) {
      if (typeof holder === "string" && holder) holders.add(holder);
    }
  }
  for (const holder of Array.isArray(task.blocking_task_ids) ? task.blocking_task_ids : []) {
    if (typeof holder === "string" && holder) holders.add(holder);
  }
  return [...holders].sort();
}

// Which pool group a readiness entry falls in. Every entry lands in exactly one.
function drainPoolKey(task) {
  const reason = drainReason(task);
  return task.eligible === true ? "eligible"
    : DRAIN_LOCK_REASONS.has(reason) ? "locks"
    : DRAIN_CAPACITY_REASONS.has(reason) ? "capacity" : "other";
}

const WAIT_LABELS = {
  capacity_saturated: "no free slot",
  resource_throttled: "throttled",
  host_shutdown_scheduled: "shutdown scheduled",
  admissions_stopped: "admissions stopped",
  cpu_light_budget_full: "cpu budget full",
  provider_limit: "provider limit",
  provider_backoff: "provider backoff",
  active_pilot_preparation: "pilot preparing",
  task_pilot_preparation_required: "needs pilot",
  unmet_dependency: "waits on deps",
  baseline_red_hold: "base red",
};

// Dependencies an `unmet_dependency` entry names, each with its status.
function drainDependencies(entry) {
  return (Array.isArray(entry.dependencies) ? entry.dependencies : [])
    .filter((dependency) => typeof dependency?.task_id === "string" && dependency.task_id)
    .map((dependency) => ({ taskId: dependency.task_id, status: typeof dependency.status === "string" ? dependency.status : "" }));
}

// The compact badge for one waiting readiness entry: `text` fits a row, `title`
// carries its cause without raw lock metadata. Structured task and run ids
// let the Readiness card link every blocker without interpreting file names.
export function drainWaitBadge(entry) {
  const reason = drainReason(entry);
  const detail = typeof entry.detail === "string" ? entry.detail.trim() : "";
  const lines = [];
  let text;
  let holders = [];
  let runIds = [];
  let dependencies = [];
  if (DRAIN_LOCK_REASONS.has(reason)) {
    holders = drainHolders(entry);
    runIds = [...new Set((Array.isArray(entry.run_ids) ? entry.run_ids : [])
      .filter(id => typeof id === "string" && id))];
    text = holders.length ? `waits on ${holders.join(", ")}` : runIds.length ? "waits on live run" : "waits on lock";
    const blockers = [...holders, ...runIds];
    lines.push(blockers.length ? `Waits on ${blockers.join(", ")}` : "Lock holder unavailable");
  } else if (DRAIN_HOST_WAIT_REASONS.has(reason)) {
    const os = /\bos:([a-z0-9_-]+)/i.exec(detail)?.[1] || /\b(macos|linux|windows)\b/i.exec(detail)?.[1];
    text = os ? `needs ${os.toLowerCase()} host` : "needs other host";
  } else if (reason === "not_backlog") {
    // Only an explicit request names a task outside the backlog; a proposed
    // one waits for approval before any drain can take it.
    text = entry.status === "proposed" ? "awaits approval" : "not in backlog";
  } else {
    text = WAIT_LABELS[reason] || reason.replaceAll("_", " ");
    if (reason === "unmet_dependency") {
      dependencies = drainDependencies(entry);
      if (dependencies.length > 0) {
        lines.push(`Waits on ${dependencies.map(({ taskId, status }) => (status ? `${taskId} (${status})` : taskId)).join(", ")}`);
      }
    }
  }
  const message = DRAIN_LOCK_REASONS.has(reason) ? "" : detail;
  if (!lines.length) lines.push(text);
  if (message && !lines.includes(message)) lines.push(message);
  return { text, title: lines.join("\n"), pool: drainPoolKey(entry), holders, runIds, dependencies, detail: message, reason };
}

// The readiness pools in the order the Backlog card lists them, each with the
// words its dot stands for.
export const DRAIN_POOLS = [
  { key: "eligible", label: "Eligible now" },
  { key: "locks", label: "Waiting on locks" },
  { key: "capacity", label: "Waiting on capacity" },
  { key: "other", label: "Waiting, other" },
];

const POOL_LABELS = Object.fromEntries(DRAIN_POOLS.map(({ key, label }) => [key, label]));

// How one readiness entry reads, for the row dot and the Readiness card alike:
// its pool, the pool's words (`poolLabel`), the badge text after them
// (`label`), the full wait (`title`), and the badge parts for a waiting entry
// (null when eligible).
export function drainReadinessView(entry) {
  const pool = drainPoolKey(entry);
  const poolLabel = POOL_LABELS[pool];
  const badge = entry.eligible === true ? null : drainWaitBadge(entry);
  return {
    pool,
    poolLabel,
    label: badge ? `${poolLabel}: ${badge.text}` : poolLabel,
    title: badge ? badge.title : "Nothing holds it: Ship it now, or the next drain window picks it up",
    badge,
  };
}

let waits = new Map();
let haveSnapshot = false;
let signature = "";
const listeners = new Set();

function clearWaits() {
  if (!haveSnapshot) return;
  waits = new Map();
  haveSnapshot = false;
  signature = "";
  for (const listener of listeners) listener();
}

// Take the latest readiness payload (the Drain card's, one workspace's).
// Listeners fire only when what a task row shows changed, so the 30 s poll
// does not repaint the list for nothing.
export function setDrainReadiness(payload) {
  const tasks = Array.isArray(payload?.tasks) ? payload.tasks : [];
  const next = new Map();
  for (const entry of tasks) {
    if (typeof entry?.task_id !== "string" || !entry.task_id) continue;
    next.set(entry.task_id, { entry, view: drainReadinessView(entry) });
  }
  const nextSignature = [...next].map(([id, { view }]) => `${id}:${view.pool}:${view.badge ? `${view.badge.text}|${view.badge.title}` : ""}`).join("\n");
  const changed = nextSignature !== signature;
  waits = next;
  signature = nextSignature;
  haveSnapshot = Array.isArray(payload?.tasks);
  if (changed) for (const listener of listeners) listener();
}

export function onDrainReadinessChange(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function drainWaitSignature(taskId) {
  const item = waits.get(taskId);
  if (!item) return "";
  const { view } = item;
  return view.badge ? `${view.pool}|${view.badge.text}|${view.badge.title}` : view.pool;
}

// The readiness dot for a backlog task: its `drainReadinessView`. Null when
// there is no snapshot or it does not mention the task, so a row never claims
// a readiness it does not know.
export function drainReadinessFor(taskId) {
  return waits.get(taskId)?.view || null;
}

// The snapshot's raw entry for a task, or null; a Readiness card falls back to
// it until its own read answers.
export function drainReadinessEntry(taskId) {
  return waits.get(taskId)?.entry || null;
}

// How the Backlog group's rows split across the pools. `known` is false until
// a snapshot arrives; a row the snapshot does not mention counts as `unknown`.
export function backlogReadiness(tasks) {
  const counts = { eligible: 0, locks: 0, capacity: 0, other: 0, unknown: 0 };
  for (const task of tasks) {
    const item = waits.get(task.id);
    counts[item ? item.view.pool : "unknown"] += 1;
  }
  return { known: haveSnapshot, counts };
}

// A snapshot belongs to one workspace; drop it when the scope moves so another
// workspace's rows never wear it.
onWorkspaceChange(clearWaits);
