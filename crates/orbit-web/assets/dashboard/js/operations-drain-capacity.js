import { cpuReadingLabel, cpuReadingValue } from './host-resources.js';
import { drainReason, drainReadinessView, DRAIN_LOCK_REASONS } from './drain-waits.js';
import { drainCheckout } from './drain-start.js';
import { autoDrainNumber, $, operationsState, runLink, time, workspaceReadOnlyReason, workspaceSelectionReason } from './operations-shared.js';
import { el, isAggregateView } from './common.js';

// ORB-11250: bounded backlog auto-drain window ("orbit run auto --for
// <duration> [--complete]" from the dashboard). One workspace-scoped action,
// not a per-row one, so it follows the mint/clock in-flight idiom (a single
// fixed `pendingOperations` key, guard released in `finally`) rather than
// tasks.js's per-task Ship guard.
//
// [ORB-12728] The live coordinator readiness reports, if any. The server
// nests it under `capacity` (where the slot picture comes from); an older
// payload shape with the fields at the top level is read the same way so a
// mixed-version dashboard never hides a running window.
export function autoDrainLiveWindow(payload) {
  const capacity = payload.capacity || {};
  const source = payload.capacity && "drain_run_id" in payload.capacity ? payload.capacity : payload;
  const runId = source?.drain_run_id ? String(source.drain_run_id) : "";
  return {
    runId,
    admissionsStopped: source?.admissions_stopped === true,
    stop: source?.admissions_stop && typeof source.admissions_stop === "object" ? source.admissions_stop : null,
    // A replica's pull drain is a second coordinator the same Stop acts on. It
    // is reported beside `drain_run_id`, never in place of it.
    pullRunId: capacity.pull_drain_run_id ? String(capacity.pull_drain_run_id) : "",
    pullAdmissionsStopped: capacity.pull_drain_admissions_stopped === true,
    pullStop: capacity.pull_drain_admissions_stop && typeof capacity.pull_drain_admissions_stop === "object" ? capacity.pull_drain_admissions_stop : null,
  };
}

// The status card and Start card share workspace and authority refusals.
export function autoDrainReasons(payload) {
  const workspaceReason = workspaceReadOnlyReason();
  return {
    submit: workspaceReason,
    complete: workspaceReason || (drainCheckout(payload).replica
      ? "A pull replica does not complete work; its owner completes what it ships."
      : payload.controls_authorized === false
      ? "Automatic completion requires an authorized operator session; the window can still start with default review completion."
      : ""),
    // A pull replica cannot approve (`--pull` conflicts with
    // `--approve-proposed`): the owner's window does that.
    approve: workspaceReason || (payload.replica === true
      ? "A pull replica cannot approve proposed tasks; start the window on the owner machine."
      : payload.controls_authorized === false
        ? "Approving proposed tasks requires an authorized operator session; the window can still start and leave them for you."
        : ""),
    autoFix: workspaceReason || (payload.replica === true
      ? "A pull replica cannot repair owner pull requests; start the window on the owner machine."
      : payload.controls_authorized !== true
        ? "Auto-fix requires an authorized operator session."
        : ""),
    // A pull replica's owner decides the review of the claims it hands out
    // (`--pull` refuses `--review`).
    review: workspaceReason || (payload.replica === true
      ? "A pull replica's owner decides the review of its claims; add review on the owner machine."
      : ""),
    // Stop is also the settle-only pass (`orbit run auto --stop`): it delivers
    // recorded settlements and needs no live window, so only a read-only view
    // or a missing operator session blocks it.
    stop: workspaceReason || (payload.controls_authorized === false
      ? "Stopping admissions and settling requires an authorized operator session."
      : ""),
  };
}

// The server takes concurrency as a whole number from 1 (a `u32`). Anything
// else typed into the field would be refused by the readiness read the next
// poll makes (a 400 that blanks the card) and by Start (a 422), so it is held
// back here with the reason shown beside the field.
export function autoDrainCounts(payload) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  const eligible = tasks.filter((task) => task.eligible === true).length;
  return { eligible, waiting: tasks.length - eligible };
}

export function autoDrainBlocked(payload) {
  const tasks = Array.isArray(payload.tasks) ? payload.tasks : [];
  return tasks.filter((task) => task.eligible !== true && DRAIN_LOCK_REASONS.has(drainReason(task)));
}

export function autoDrainShortRunId(runId) {
  const match = /^jrun-\d{8}-(.+)$/.exec(runId);
  return match ? `jrun-…${match[1]}` : runId;
}

// The server-stamped window deadline is shared by CLI and browser starts.
export function autoDrainTimeLeft(endsAt) {
  const deadline = Date.parse(endsAt || "");
  if (!Number.isFinite(deadline)) return "";
  const minutes = Math.max(0, Math.ceil((deadline - Date.now()) / 60_000));
  if (minutes === 0) return "window closed";
  return minutes < 60 ? `${minutes}m left` : `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m left`;
}

export function updateDrainIndicators(phase, label) {
  const aggregate = isAggregateView();
  const indicatorPhase = aggregate ? "fleet" : phase;
  const indicatorLabel = aggregate ? "Fleet drains" : label;
  const tab = document.querySelector?.('#dock-mode-toggle [data-mode="drain"]');
  const tabState = $("dock-drain-state");
  if (tab) {
    tab.dataset.drainState = indicatorPhase;
    // The tab's own name differs by page (Overview on Drain, Drain on Tasks);
    // the dock relabels from `drainLabel` when it moves.
    tab.dataset.drainLabel = indicatorLabel;
    const name = tab.querySelector?.(".dock-seg-label")?.textContent || "Overview";
    tab.setAttribute("aria-label", `${name}: ${indicatorLabel}`);
    tab.title = indicatorLabel;
  }
  if (tabState) tabState.textContent = aggregate ? "fleet" : phase === "winding_down" ? label : "";
}

// The card and dock tab describe the selected workspace. The top-bar pill
// separately describes the fleet, including work outside that selection.
export function autoDrainPhase(payload) {
  const live = autoDrainLiveWindow(payload);
  const capacity = payload.capacity || {};
  const phase = capacity.drain_phase || (live.runId && !live.admissionsStopped ? "draining" : "idle");
  const running = Number(capacity.running_admitted_workers) || 0;
  const label = phase === "draining" ? "Draining"
    : phase === "winding_down" ? "Winding down" : "idle";
  // A replica's pull drain has its own indicator; the owner's phase stays
  // "idle" here, so the header and the announcement below keep their wording.
  const pulling = phase === "idle" && Boolean(live.pullRunId) && !workspaceSelectionReason();
  const pullLabel = live.pullAdmissionsStopped ? "Pull drain · admissions stopped" : "Pull drain";
  return { live, capacity, phase, running, label, pulling, pullLabel };
}

export function autoDrainCapacity(capacity, counts, blocked) {
  const busy = autoDrainNumber(capacity.active_leaf_runs ?? capacity.occupancy?.active_leaf_runs);
  const limit = autoDrainNumber(capacity.max_active_leaf_runs);
  const free = autoDrainNumber(capacity.free_slots);
  const known = Number.isFinite(busy) && Number.isFinite(limit) && limit > 0;
  const admits = Number.isFinite(free) ? Math.max(0, Math.min(free, counts.eligible)) : counts.eligible;
  let summary;
  if (capacity.host_shutdown && capacity.host_shutdown_holding !== false) {
    summary = "New tasks cannot start while a host shutdown is scheduled.";
  } else if (capacity.host_shutdown) {
    const mode = capacity.host_shutdown.mode || "shutdown";
    const at = capacity.host_shutdown.scheduled_at || "a later time";
    summary = `A host ${mode} is scheduled for ${at}. Admissions continue until the hold horizon.`;
  } else if (capacity.resource_throttle) {
    const resources = Array.isArray(capacity.resource_throttle.resources)
      ? capacity.resource_throttle.resources.map(item => item.resource).join(", ") : "";
    summary = `New tasks cannot start until the host resource throttle clears${resources ? ` (${resources})` : ""}.`;
  } else if (capacity.admissions_stopped === true) {
    summary = "This window has stopped starting new tasks. Tasks already admitted keep running.";
  } else if (Number.isFinite(free) && free <= 0 && known && busy >= limit) {
    const toClear = busy - limit + 1;
    summary = `The workspace leaf limit is reached; ${autoDrainPlural(toClear, "occupied slot")} must clear before another task can start.`;
  } else if (Number.isFinite(free) && free <= 0) {
    summary = "No admissions are available in this readiness snapshot.";
  } else if (counts.eligible === 0) {
    if (blocked > 0) {
      summary = `No tasks are eligible yet; ${autoDrainPlural(blocked, "pool task")} ${blocked === 1 ? "waits" : "wait"} on task locks or live claims.`;
    } else {
      summary = counts.waiting > 0
      ? `Nothing is eligible yet; ${autoDrainPlural(counts.waiting, "task")} ${counts.waiting === 1 ? "waits" : "wait"} in the pool.`
      : "Nothing is waiting in the backlog.";
    }
  } else {
    summary = `A window started now admits up to ${autoDrainPlural(admits, "task")}.`;
  }
  const label = known ? `Workspace: ${busy} of ${limit} leaf slots in use` : "Workspace: capacity unknown";
  const full = Number.isFinite(free) && free <= 0;
  const bar = el("div", { class: "drain-capacity-bar" });
  bar.setAttribute("role", "img");
  bar.setAttribute("aria-label", label);
  if (known) {
    const scale = Math.max(limit, busy);
    // One segment per slot while they stay wide enough to count at a glance;
    // past that a continuous fill reads better than slivers.
    if (scale <= 24) {
      bar.classList.add("segmented");
      for (let slot = 0; slot < scale; slot += 1) {
        const state = slot >= limit ? " over" : slot < busy ? " used" : "";
        bar.appendChild(el("span", { class: `drain-capacity-seg${state}` }));
      }
    } else {
      const within = Math.min(busy, limit);
      bar.append(
        el("span", { class: "drain-capacity-used", style: { width: `${(within / scale) * 100}%` } }),
        el("span", { class: "drain-capacity-over", style: { width: `${(Math.max(0, busy - limit) / scale) * 100}%` } }),
      );
    }
  }
  const pipelineCounts = Object.entries(capacity.leaf_occupancy_by_pipeline || {})
    .filter(([, count]) => Number(count) > 0);
  const pipelines = pipelineCounts.map(([pipeline, count]) => [pipeline, Number(count)]);
  // Legacy wrappers can occupy slots without appearing in the pipeline map.
  const other = busy - pipelineCounts.reduce((sum, [, count]) => sum + Number(count), 0);
  if (pipelines.length && other > 0) pipelines.push(["other", other]);
  const byPipeline = pipelines.map(([pipeline, count]) => `${pipeline}: ${autoDrainPlural(count, "slot")}`);
  // A shortfall the operator should notice reads amber; a stopped window is
  // the operator's own choice and does not.
  const held = full && capacity.admissions_stopped !== true;
  const count = el("span", {
    class: `drain-capacity-count mono${full ? " full" : ""}`,
    text: known ? `${busy}/${limit} slots` : "capacity unknown",
    title: [label, Number.isFinite(free) ? `${Math.max(0, free)} free` : "", ...byPipeline].filter(Boolean).join("\n"),
  });
  const result = el("div", { class: "drain-capacity" }, [
    bar,
    el("div", { class: "drain-capacity-line" }, [
      el("p", { class: `drain-slots${held ? " held" : ""}`, text: summary }),
      count,
    ]),
  ]);
  // One pipeline holding every slot is already in the count's tooltip; a mix
  // is worth a visible line.
  if (pipelines.length > 1) {
    const list = el("ul", { class: "drain-pipeline-occupancy" }, byPipeline.map(text => el("li", { class: "drain-pipeline mono", text })));
    list.setAttribute("aria-label", "Workspace slots by pipeline");
    result.appendChild(list);
  }
  return result;
}

export function autoDrainPlural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

// A throttled resource's reading in the unit the rail block uses: CPU per the
// host's CPU source (load per core, "load 1.6× cores", or utilization, "cpu
// 92%"), memory and disk as percentages. A threshold is bare (no leading
// label) and keeps two decimals so 0.75× does not round to 0.8×.
function throttleReading(item, percent, threshold = false) {
  if (item.resource === "cpu") {
    const value = cpuReadingValue(percent, item.cpu_source, threshold);
    return threshold ? value : `${throttleName(item)} ${value}`;
  }
  return threshold ? `${percent}%` : `${item.resource} ${Math.round(Number(percent))}%`;
}

// The resources a throttle holds, or an empty list. Each carries the host's
// CPU source, which names and scales a CPU reading.
export function throttledResources(capacity) {
  const resources = capacity?.resource_throttle?.resources;
  return Array.isArray(resources)
    ? resources.filter(item => item && item.resource).map(item => ({ ...item, cpu_source: capacity.resource_cpu_source }))
    : [];
}

// A throttled resource by the name the rail block gives it.
function throttleName(item) {
  return item.resource === "cpu" ? cpuReadingLabel(item.cpu_source) : String(item.resource);
}

export function throttleNames(resources) {
  const names = [...new Set(resources.map(throttleName))];
  return names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}` : names[0] || "";
}

// The current readings, stop thresholds and resume thresholds of a throttle.
function throttleSentence(resources) {
  const held = resources
    .map(item => `${throttleReading(item, item.percent)} (throttled at ≥ ${throttleReading(item, item.high_percent, true)} since ${time(item.since)}; resumes below ${throttleReading(item, item.resume_percent, true)})`)
    .join("; ");
  return `Admissions throttled: ${held}. Running tasks are not touched.`;
}

export function autoDrainThrottleLine(capacity) {
  const resources = throttledResources(capacity);
  if (resources.length === 0) return null;
  const note = el("p", { class: "operation-control-note drain-throttle-line", text: throttleSentence(resources) });
  note.setAttribute("role", "status");
  return note;
}

// How many waiting tasks name each task as their lock holder, from the last
// readiness snapshot of the selected workspace.
export function drainHolderCounts() {
  const counts = new Map();
  const tasks = Array.isArray(operationsState.lastAutoDrain?.tasks) ? operationsState.lastAutoDrain.tasks : [];
  for (const entry of tasks) {
    if (!entry || entry.eligible === true) continue;
    for (const holder of drainReadinessView(entry).badge?.holders || []) counts.set(holder, (counts.get(holder) || 0) + 1);
  }
  return counts;
}

// A dismissed provider-limit note stays hidden in this browser until its set
// of gated windows changes: another provider, account, scope or window gates,
// a reset time moves, or the skipped crews change. Only the most recent few
// are kept, so the list cannot grow without bound.
const DISMISSED_PROVIDER_LIMITS_KEY = "orbit.dashboard.dismissedProviderLimits";
const DISMISSED_PROVIDER_LIMITS_MAX = 20;

function providerLimitNoteKey(gated) {
  return JSON.stringify(gated.map(reading => [
    reading.provider, reading.account || "", reading.scope || "", reading.window || "", reading.until || "",
    Array.isArray(reading.crews) ? [...reading.crews].sort() : [],
  ]).sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b))));
}

function dismissedProviderLimits() {
  try {
    const stored = JSON.parse(window.localStorage.getItem(DISMISSED_PROVIDER_LIMITS_KEY) || "[]");
    return Array.isArray(stored) ? stored.filter(key => typeof key === "string") : [];
  } catch {
    return [];
  }
}

function dismissProviderLimits(key) {
  const kept = dismissedProviderLimits().filter(existing => existing !== key);
  kept.push(key);
  try {
    window.localStorage.setItem(DISMISSED_PROVIDER_LIMITS_KEY, JSON.stringify(kept.slice(-DISMISSED_PROVIDER_LIMITS_MAX)));
  } catch {
    /* localStorage unavailable: the note stays dismissed until the next repaint */
  }
}

// [ORB-14698] Each provider usage window that keeps crews out of admission,
// with its use, threshold, reset and the crews skipped until then. Readings
// below their threshold do not hold anything, so they are not shown here.
// The note can be dismissed; see dismissedProviderLimits.
export function autoDrainProviderLimitNote(readings) {
  const gated = (Array.isArray(readings) ? readings : []).filter(reading => reading?.gated === true);
  if (gated.length === 0) return null;
  const key = providerLimitNoteKey(gated);
  if (dismissedProviderLimits().includes(key)) return null;
  const lines = gated.map((reading) => {
    // A crew that sets `env` is its own account, named `provider/crew`.
    const account = reading.account ? `${reading.provider}/${reading.account}` : reading.provider;
    const label = `${account}${reading.scope ? ` [${reading.scope}]` : ""} ${reading.window || "usage window"}`;
    const used = reading.used_percent == null ? "exhausted" : `${reading.used_percent}% ≥ ${reading.threshold}%`;
    // A ledger budget in dollars leaves out invocations that recorded no cost.
    const usage = reading.partial === true ? `${used} (partial: some invocations report no cost)` : used;
    const crews = Array.isArray(reading.crews) && reading.crews.length ? reading.crews.join(", ") : "no configured crew";
    return `${label} ${usage} until ${time(reading.until)}: ${crews} skipped`;
  });
  const note = el("p", { class: "operation-control-note drain-provider-limit-note" }, [
    el("span", { class: "drain-provider-limit-text", text: `Provider limits: ${lines.join("; ")}.` }),
  ]);
  note.setAttribute("role", "status");
  const dismiss = el("button", {
    class: "drain-note-dismiss", type: "button", title: "Dismiss until these limits change",
    "aria-label": "Dismiss provider limits until they change",
  });
  dismiss.addEventListener("click", () => {
    dismissProviderLimits(key);
    note.remove();
  });
  note.append(dismiss);
  return note;
}

// One row per host and provider account whose login is still failing: the
// provider's default login, or a crew that sets `env` (`provider/crew`). A
// later successful run of that account on that host drops the row.
export function autoDrainProviderAuth(failures) {
  const items = Array.isArray(failures) ? failures : [];
  if (items.length === 0) return null;
  const list = el("ul", { class: "drain-provider-auth" });
  list.setAttribute("aria-label", "Provider authentication failures");
  for (const item of items) {
    const row = el("li", { class: "drain-provider-auth-item" }, [
      `${item.provider || "provider"}${item.account ? `/${item.account}` : ""} auth failed on ${item.host || "this host"}`,
    ]);
    const runs = Array.isArray(item.runs) ? item.runs : [];
    runs.forEach((run, index) => {
      row.append(index === 0 ? " " : ", ");
      if (run?.run_id) row.append(runLink(run.run_id));
    });
    if (item.fix_command) {
      row.append(" ");
      row.append(el("code", { class: "drain-provider-auth-fix", text: item.fix_command }));
    }
    list.append(row);
  }
  return list;
}

// A recorded forecast object, or null when the field is absent or not an object.
