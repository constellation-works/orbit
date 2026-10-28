// Routine-definition, host clock, and auto-task operations [ORB-10875, ORB-10876].

import { fetchJson, requestPanel } from './common.js';
import { editingIn } from './schedule-editor.js';

import {
  $,
  configureOperationsRefresh,
  noteCronZone,
  operationsState,
  selectedWorkspace,
  selectedWorkspaceName,
  setRailSubtabCount,
  withInactivePlugins
} from './operations-shared.js';
import { configureOperationsRoutines, renderClock, renderRoutineRows, scopedRoutines, updateRoutineFailureBadge } from './operations-routines.js';
import { fetchAndRenderAutoTasks, syncAutoTaskSchedulerNote } from './operations-auto-tasks.js';
import { lastJobs, renderJobs } from './operations-jobs.js';
import { startFleetIndicatorRefresh } from './operations-fleet.js';

export { OPERATION_FEEDBACK_CLEAR_MS, initOperations, triggerFeedbackClear } from './operations-shared.js';
export { startFleetIndicatorRefresh };
export { fetchAndRenderAutoDrainPane } from './operations-drain.js';
export { drainHolderCounts } from './operations-drain-capacity.js';
export { peekRoutineFailures } from './operations-routines.js';
export { fetchAndRenderJobs } from './operations-jobs.js';

function renderOperations(payload, { force = false } = {}) {
  noteCronZone(payload);
  operationsState.lastOperations = payload;
  if ($("operations-session")) $("operations-session").textContent = payload.session_explanation || "Operations actions require the capabilities granted to this dashboard server. Refresh to load session access details.";
  const workspace = selectedWorkspaceName();
  const workspaceId = selectedWorkspace()?.id || null;
  const routines = scopedRoutines(payload);
  updateRoutineFailureBadge(routines);
  const inactive = workspace
    ? (payload.retired || []).filter((routine) => routine.plugin_inactive && routine.source === workspace)
    : [];
  // A replica schedules only worktree GC and store GC on a cron trigger; its owner schedules the rest.
  const ownerOnly = workspace
    ? (payload.owner_only || []).filter((routine) => routine.source === workspace)
    : [];
  const body = $("routines-body");
  // An open trigger editor holds a draft: a background refresh leaves the
  // rows alone until it closes.
  const frozen = !force && editingIn("routine") && body.querySelector(".schedule-editor");
  if (!frozen) renderRoutineRows(payload, body, { workspace, workspaceId, routines, inactive, ownerOnly });
  const activeCount = routines.filter((routine) => routine.enabled).length;
  $("routines-count").textContent = workspace ? `${activeCount} active of ${routines.length} · ${workspace}` : "read-only";
  setRailSubtabCount("rail-count-ops-routines", workspace && routines.length ? `${activeCount}/${routines.length}` : "");
  renderClock(payload);
  syncAutoTaskSchedulerNote();
  if (lastJobs) renderJobs(lastJobs);
}

// The chrome always names the fleet, including when an idle default workspace
// is selected. The status card describes the selected workspace.
function throwFirstPanelError(results) {
  const errors = results.filter(result => result.status === "rejected").map(result => result.reason);
  // Preserve transport classification even if a different panel also fails.
  if (errors.length) throw errors.find(error => error.networkFailure) || errors[0];
}

export async function fetchAndRenderOperations(subtab = operationsState.context.getOperationsSubtab()) {
  if (subtab === "auto-tasks") {
    await fetchAndRenderAutoTasks();
    return;
  }
  const routines = fetchJson(withInactivePlugins("/api/routines"));
  throwFirstPanelError(await Promise.allSettled([
    requestPanel("routines-body", "routines", () => routines, renderOperations, "routines-count"),
    requestPanel("clock-body", "clock", () => routines, renderClock, "clock-host"),
  ]));
}

configureOperationsRoutines({ renderOperations, fetchOperations: fetchAndRenderOperations });
configureOperationsRefresh(fetchAndRenderOperations);
