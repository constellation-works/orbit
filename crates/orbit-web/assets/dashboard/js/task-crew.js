// Orbit dashboard task crew helpers and inline mutation feedback.
// Split out of tasks.js; imports task-shared for the crew payload state and
// the list re-render hook, and never imports tasks.js.

import { el, fetchJson, getHostRevision, isAggregateView } from './common.js';
import { taskState, rerender } from './task-shared.js';

export function scheduleFeedbackExpiry(map, key, context, delay) {
  const scheduledEntry = map.get(key);
  setTimeout(() => {
    if (map.get(key) === scheduledEntry && scheduledEntry && scheduledEntry.kind !== "pending") {
      map.delete(key);
      rerender(context);
    }
  }, delay);
}

// Renders the inline pending/success/error text next to a status or crew
// select, plus a bounded "undo" button while `entry.undo` is still live. The
// wrapper is a live region so screen-reader users get the same feedback a
// sighted user reads from the text/color change.
export function buildMutationFeedback(entry, onUndo = () => {}) {
  if (!entry) return null;
  const wrap = el("span", { class: `mutation-feedback ${entry.kind}`, text: entry.text });
  wrap.setAttribute("role", "status");
  wrap.setAttribute("aria-live", entry.kind === "error" ? "assertive" : "polite");
  if (entry.undo) {
    const undoBtn = el("button", { class: "mutation-undo", text: "undo" });
    undoBtn.type = "button";
    undoBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      onUndo();
    });
    wrap.appendChild(undoBtn);
  }
  return wrap;
}

export function normalizeCrewPayload(payload) {
  const crews = Array.isArray(payload && payload.crews)
    ? payload.crews
      .filter((crew) => crew && crew.name)
      .map((crew) => ({
        name: String(crew.name),
        model: crew.model == null ? "" : String(crew.model),
        is_default: Boolean(crew.is_default),
      }))
    : [];
  crews.sort((a, b) => a.name.localeCompare(b.name));
  // Human crews name operators: tasks assigned to one wait for that person,
  // and no drain or task pilot picks them up.
  const humanCrews = Array.isArray(payload && payload.human_crews)
    ? payload.human_crews
      .filter((crew) => crew && crew.name)
      .map((crew) => ({
        name: String(crew.name),
        description: crew.description == null ? "" : String(crew.description),
      }))
    : [];
  humanCrews.sort((a, b) => a.name.localeCompare(b.name));
  const viewer = payload && payload.viewer_human_crew ? String(payload.viewer_human_crew) : null;
  return {
    default_crew: payload && payload.default_crew ? String(payload.default_crew) : null,
    crews,
    human_crews: humanCrews,
    viewer_human_crew: humanCrews.some((crew) => crew.name === viewer) ? viewer : null,
  };
}

// The human crew a task is assigned to, or null for an agent crew or none.
// The server classifies each task by its own workspace's crew registry and
// stamps `human_crew` on it, so the answer holds in the aggregate view (which
// has no registry) and for a crew name that is an agent crew in one workspace
// and a human crew in another. Never guess from the name or a cached registry.
export function humanCrewFor(task) {
  const human = task && task.human_crew;
  return human && human.name ? human : null;
}

// The human crews the "My work" filter keeps: the viewer's own when the
// server recognised it, else every human crew.
export function myWorkCrewNames() {
  const payload = taskState.lastCrewPayload;
  const humans = payload.human_crews || [];
  if (payload.viewer_human_crew) return [payload.viewer_human_crew];
  return humans.map((crew) => crew.name);
}

// Whether a task is the viewer's own work, from the server-stamped
// `human_crew` on the task itself. The aggregate view clears the workspace
// registry, so a name-based check loses every task there; the per-task
// stamp survives and keeps same-named human/agent crews apart. When the
// viewer is known only that crew counts, otherwise every human crew does.
export function isMyWorkTask(task) {
  const human = humanCrewFor(task);
  if (!human) return false;
  const viewer = taskState.lastCrewPayload && taskState.lastCrewPayload.viewer_human_crew;
  return viewer ? human.name === viewer : true;
}

export function cacheCrewPayload(payload) {
  taskState.lastCrewPayload = normalizeCrewPayload(payload);
  return taskState.lastCrewPayload;
}

// No crews, and not a loaded registry. Aggregate rows use this when the
// workspace read failed or has not been stored for the current host.
const EMPTY_CREW_PAYLOAD = {
  default_crew: null,
  crews: [],
  human_crews: [],
  viewer_human_crew: null,
};

// Crew registries for the All workspaces list, one per workspace on this host.
// A quiet refresh reuses an entry. A host change drops every other host's
// entries. The ambient `lastCrewPayload` stays cleared in that view so My work
// and comment authors do not borrow one workspace's registry.
const aggregateCrewsByWorkspace = new Map();

function aggregateCrewEntry(workspaceId) {
  if (!workspaceId) return null;
  const entry = aggregateCrewsByWorkspace.get(String(workspaceId));
  if (!entry || entry.hostRevision !== getHostRevision()) return null;
  return entry;
}

export function aggregateCrewPayload(task) {
  const entry = aggregateCrewEntry(task && task.workspace_id);
  if (!entry || entry.failed || !entry.payload) return EMPTY_CREW_PAYLOAD;
  return entry.payload;
}

// The aggregate select has nothing to list: the crew read failed, this host
// has no stored registry for the workspace, or the task names no workspace.
export function aggregateCrewUnavailable(task) {
  if (!isAggregateView()) return false;
  const entry = aggregateCrewEntry(task && task.workspace_id);
  return !entry || entry.failed || !entry.payload;
}

export function rememberAggregateCrew(workspaceId, hostRevision, payload) {
  if (!workspaceId || hostRevision !== getHostRevision()) return;
  aggregateCrewsByWorkspace.set(String(workspaceId), {
    hostRevision,
    payload: normalizeCrewPayload(payload),
    failed: false,
  });
}

export function rememberAggregateCrewFailure(workspaceId, hostRevision) {
  if (!workspaceId || hostRevision !== getHostRevision()) return;
  aggregateCrewsByWorkspace.set(String(workspaceId), {
    hostRevision,
    payload: null,
    failed: true,
  });
}

export function dropAggregateCrewsForOtherHosts(hostRevision) {
  for (const [id, entry] of aggregateCrewsByWorkspace) {
    if (entry.hostRevision !== hostRevision) aggregateCrewsByWorkspace.delete(id);
  }
}

function aggregateCrewSignature() {
  const hostRevision = getHostRevision();
  const rows = [];
  for (const [id, entry] of aggregateCrewsByWorkspace) {
    if (entry.hostRevision !== hostRevision) continue;
    rows.push([id, entry.failed, entry.payload]);
  }
  rows.sort((a, b) => String(a[0]).localeCompare(String(b[0])));
  return JSON.stringify(rows);
}

// Read `/api/crews` for each workspace that has a task on this page. A failed
// workspace is recorded and does not reject the caller, so the task list still
// renders. `force` refetches, matching a refresh that asks for crew options;
// otherwise a stored entry for this host is reused.
export async function loadAggregateCrews(tasks, { force = false } = {}) {
  const hostRevision = getHostRevision();
  dropAggregateCrewsForOtherHosts(hostRevision);
  const pending = [];
  const seen = new Set();
  for (const task of Array.isArray(tasks) ? tasks : []) {
    const id = task && task.workspace_id ? String(task.workspace_id) : "";
    if (!id || seen.has(id)) continue;
    seen.add(id);
    if (!force && aggregateCrewEntry(id)) continue;
    pending.push(id);
  }
  await Promise.all(pending.map(async (id) => {
    try {
      const payload = await fetchJson(`/api/crews?workspace=${encodeURIComponent(id)}`);
      rememberAggregateCrew(id, hostRevision, payload);
    } catch (error) {
      rememberAggregateCrewFailure(id, hostRevision);
      console.error(error);
    }
  }));
}

export function hasCrewOptions() {
  return Array.isArray(taskState.lastCrewPayload.crews) && taskState.lastCrewPayload.crews.length > 0;
}

export function crewOptionsSignature() {
  const ambient = JSON.stringify(taskState.lastCrewPayload);
  if (!isAggregateView()) return ambient;
  return `${ambient}|${aggregateCrewSignature()}`;
}

// A stable string for a task's current mutation-feedback entry (or "" when
// none), so a syncNodes row diff picks up pending/success/error/undo
// transitions the same way it picks up any other task field change.
export function feedbackSignature(map, taskId) {
  const entry = map.get(taskId);
  if (!entry) return "";
  return `${entry.kind}:${entry.text}:${entry.undo ? entry.undo.expiresAt : ""}`;
}

export function explicitCrewValue(task) {
  return task && task.crew ? String(task.crew) : "";
}

// Strip an `@<effort>` suffix from a crew reference (e.g. `luna@xhigh` -> `luna`).
export function crewReferenceBase(reference) {
  const str = reference ? String(reference).trim() : "";
  const at = str.indexOf("@");
  return at >= 0 ? str.slice(0, at).trim() : str;
}

export function findConfiguredCrew(value) {
  const name = value ? String(value).trim() : "";
  if (!name) return null;
  const base = crewReferenceBase(name);
  const crews = Array.isArray(taskState.lastCrewPayload.crews) ? taskState.lastCrewPayload.crews : [];
  const agentCrew = crews.find((crew) => crew.name === name || crew.name === base);
  if (agentCrew) return agentCrew;
  const humanCrews = Array.isArray(taskState.lastCrewPayload.human_crews) ? taskState.lastCrewPayload.human_crews : [];
  const humanCrew = humanCrews.find((crew) => crew.name === name || crew.name === base);
  if (humanCrew) return humanCrew;
  return null;
}

export function isConfiguredCrewValue(value) {
  return Boolean(findConfiguredCrew(value));
}

export function hasStaleExplicitCrew(task) {
  const currentValue = explicitCrewValue(task);
  return Boolean(currentValue) && !isConfiguredCrewValue(currentValue);
}

export function resolvedCrewName(task) {
  if (taskState.lastCrewPayload.default_crew) return taskState.lastCrewPayload.default_crew;
  if (!explicitCrewValue(task) && task && task.resolved_crew) {
    return String(task.resolved_crew);
  }
  return "workspace";
}

export function defaultCrewOptionText(task) {
  return `default: ${resolvedCrewName(task)}`;
}

export function crewOptionTitle(crew) {
  return `model=${crew.model || "-"}`;
}
