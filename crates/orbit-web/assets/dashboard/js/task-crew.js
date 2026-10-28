// Orbit dashboard task crew helpers and inline mutation feedback.
// Split out of tasks.js; imports task-shared for the crew payload state and
// the list re-render hook, and never imports tasks.js.

import { el } from './common.js';
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
  return {
    default_crew: payload && payload.default_crew ? String(payload.default_crew) : null,
    crews,
  };
}

export function cacheCrewPayload(payload) {
  taskState.lastCrewPayload = normalizeCrewPayload(payload);
  return taskState.lastCrewPayload;
}

export function hasCrewOptions() {
  return Array.isArray(taskState.lastCrewPayload.crews) && taskState.lastCrewPayload.crews.length > 0;
}

export function crewOptionsSignature() {
  return JSON.stringify(taskState.lastCrewPayload);
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

export function isConfiguredCrewValue(value) {
  const name = value ? String(value) : "";
  return Boolean(name) && taskState.lastCrewPayload.crews.some((crew) => crew.name === name);
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
