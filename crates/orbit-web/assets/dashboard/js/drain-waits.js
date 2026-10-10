// Why a backlog task is not starting: the drain readiness snapshot, shared by
// the Drain card (operations.js) and the Tasks list (tasks.js).
// The Drain card owns the fetch and hands each payload to `setDrainReadiness`;
// the Tasks list reads the waits back per task and repaints on a change.

import { onWorkspaceChange } from './common.js';

// Readiness reasons that mean "waiting on another task or run": a context lock, a
// grouped member, a same-wave deferral, a live child's claim, or a task with no
// footprint waiting for the other leaves to finish so it can run alone.
export const DRAIN_LOCK_REASONS = new Set(["context_lock_conflict", "group_member_conflict", "conflict_deferred", "claimed_by_live_child", "awaiting_exclusive_slot"]);

// Readiness reasons that mean "waiting on slots or host pressure": a full
// workspace, a throttle, a scheduled shutdown or stopped admissions.
export const DRAIN_CAPACITY_REASONS = new Set(["capacity_saturated", "resource_throttled", "host_shutdown_scheduled", "admissions_stopped", "cpu_light_budget_full"]);

// A task whose `os:` tags this host's OS does not satisfy waits for a host of
// its own (a pull-drain follower on that OS, say), and one a pilot found needs
// native evidence from another OS waits for the `os:` tag readiness names.
export const DRAIN_HOST_WAIT_REASONS = new Set(["host_os_mismatch", "native_os_required"]);

export function drainReason(task) {
  return typeof task.reason === "string" && task.reason.trim() ? task.reason : "unknown";
}

// Holder ids for a lock-blocked row. Context locks report
// `conflicts[].locking_task_id`; same-wave deferrals report
// `conflicts[].blocking_task_id` plus `blocking_task_ids`; live-child claims
// report only `run_ids`, which have no task holder.
export function drainHolders(task) {
  const holders = new Set();
  for (const conflict of Array.isArray(task.conflicts) ? task.conflicts : []) {
    const holder = conflict?.locking_task_id || conflict?.blocking_task_id;
    if (holder) holders.add(holder);
  }
  for (const holder of Array.isArray(task.blocking_task_ids) ? task.blocking_task_ids : []) {
    if (holder) holders.add(holder);
  }
  return [...holders].sort();
}

export function drainConflictSelectors(task, holder) {
  const selectors = [];
  for (const conflict of Array.isArray(task.conflicts) ? task.conflicts : []) {
    const conflictHolder = conflict?.locking_task_id || conflict?.blocking_task_id;
    if (holder && conflictHolder && conflictHolder !== holder) continue;
    const selector = conflict?.requested_file || conflict?.requested_selector || conflict?.blocking_selector;
    if (selector && !selectors.includes(selector)) selectors.push(selector);
  }
  return selectors;
}

// `file:crates/a/b/c.rs` → `…/b/c.rs`: the dock is 336px, so the lock line
// keeps the part of the path that tells files apart; the title has the rest.
export function drainShortSelector(selector) {
  const parts = String(selector).replace(/^[a-z]+:/, "").split("/");
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : parts.join("/");
}

// Which pool group a readiness entry falls in. Every entry lands in exactly one.
export function drainPoolKey(task) {
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
};

// The compact badge for one waiting readiness entry: `text` fits a row, `title`
// carries everything the entry reports.
export function drainWaitBadge(entry) {
  const reason = drainReason(entry);
  const detail = typeof entry.detail === "string" ? entry.detail.trim() : "";
  const lines = [];
  let text;
  if (DRAIN_LOCK_REASONS.has(reason)) {
    const holder = drainHolders(entry)[0] || null;
    const runId = Array.isArray(entry.run_ids) ? entry.run_ids[0] : null;
    const selectors = drainConflictSelectors(entry, holder);
    text = holder ? `waits on ${holder}` : runId ? "waits on live run" : "waits on lock";
    if (selectors.length > 0 || reason === "context_lock_conflict") text += " · lock";
    lines.push(holder ? `Waits on ${holder}` : runId ? `Waits on live run ${runId}` : "Waits on a lock");
    if (selectors.length > 0) lines.push(`Lock: ${selectors.join(", ")}`);
  } else if (DRAIN_HOST_WAIT_REASONS.has(reason)) {
    const os = /\bos:([a-z0-9_-]+)/i.exec(detail)?.[1] || /\b(macos|linux|windows)\b/i.exec(detail)?.[1];
    text = os ? `needs ${os.toLowerCase()} host` : "needs other host";
  } else {
    text = WAIT_LABELS[reason] || reason.replaceAll("_", " ");
  }
  if (detail) lines.push(detail);
  lines.push(`Reason: ${reason}`);
  return { text, title: lines.join("\n"), pool: drainPoolKey(entry) };
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
    next.set(entry.task_id, { pool: drainPoolKey(entry), badge: entry.eligible === true ? null : drainWaitBadge(entry) });
  }
  const nextSignature = [...next].map(([id, item]) => `${id}:${item.pool}:${item.badge ? `${item.badge.text}|${item.badge.title}` : ""}`).join("\n");
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

// The wait badge for a backlog task, or null when the snapshot has no wait for it.
export function drainWaitFor(taskId) {
  return waits.get(taskId)?.badge || null;
}

export function drainWaitSignature(taskId) {
  const badge = waits.get(taskId)?.badge;
  return badge ? `${badge.text}|${badge.title}` : "";
}

const POOL_PHRASES = [
  ["locks", (n) => `${n} waiting on locks`],
  ["capacity", (n) => `${n} waiting on capacity`],
  ["other", (n) => `${n} waiting, other`],
];

// The Backlog group hint: how many of these rows a drain window could take now
// versus how many wait, and on what. Without a snapshot it only says what the
// group is; it must not claim eligibility it does not know.
export function backlogGroupHint(tasks) {
  if (!haveSnapshot) return "Approved; Ship or a drain window picks them up once nothing holds them";
  const counts = { eligible: 0, locks: 0, capacity: 0, other: 0, unknown: 0 };
  for (const task of tasks) {
    const item = waits.get(task.id);
    counts[item ? item.pool : "unknown"] += 1;
  }
  const parts = [`${counts.eligible} eligible now`];
  for (const [key, phrase] of POOL_PHRASES) if (counts[key] > 0) parts.push(phrase(counts[key]));
  if (counts.unknown > 0) parts.push(`${counts.unknown} not in the drain snapshot`);
  return `${tasks.length} approved · ${parts.join(", ")}`;
}

// A snapshot belongs to one workspace; drop it when the scope moves so another
// workspace's rows never wear it.
onWorkspaceChange(clearWaits);
