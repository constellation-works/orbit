use std::collections::BTreeSet;

use chrono::Utc;
use orbit_engine::DispatchError;
use orbit_types::workflow::{DrainAdmissionPass, DrainWaitingTask};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::admission::auto_admission::{
    AdmissionHolders, select_admissions,
};
use crate::adapter::engine_host::v2_host::admission::backlog_exclusion::{
    BacklogTaskExclusion, allowlist_from_input, backlog_snapshot,
};

use super::action_failed;
use super::drains::{
    DEFAULT_MAX_ACTIVE_LEAF_RUNS, live_admissions_stop, live_leaf_runs, live_worker_limit,
    shared_leaf_occupancy,
};

/// Wait before re-listing when the backlog has admissible work but every slot
/// is occupied. This is the latency a freed slot sits idle, so it is much
/// shorter than the idle wait.
const DEFAULT_POLL_SLEEP_SECONDS: u64 = 30;

/// Wait before re-listing when nothing is admissible at all. Long, because the
/// only thing that can change is a task arriving or a live child finishing.
const DEFAULT_IDLE_SLEEP_SECONDS: u64 = 60;

/// Default and ceiling for the backlog prefix one iteration examines, matching
/// `list_backlog_tasks`'s `max_tasks` bound.
pub(super) const DEFAULT_CANDIDATE_POOL: u64 = 50;
const MAX_CANDIDATE_POOL: u64 = 500;

/// The admissible work for one drain iteration [ORB-10819].
///
/// This answers "what may start right now", not "what is the one action for
/// this tick". Every answer is an ordinary leaf: a conflict-free chore ships in
/// the same iteration that a large task is running, because the active task
/// reserves its own declared `context_files` and `backlog_snapshot` drops
/// exactly the leaves that overlap it. That reservation is why the former
/// `hold` decision is gone — a blanket freeze excluded conflict-free work the
/// lock surface had no reason to exclude.
///
/// Leaves are offered up to the number of *free* slots rather than in one
/// batch, because the drain no longer waits on them. The whole backlog is
/// re-listed every iteration and the free slots are topped up from it, so a
/// task that entered `backlog` a minute ago starts as soon as any one child
/// finishes — not after the slowest member of the batch that was running when
/// it arrived. One task per dispatch: a multi-task child bought nothing but a
/// coarser refill unit, and a one-task child is crew-homogeneous by
/// construction.
///
/// [ORB-11973] Which leaves fill those slots is `select_admissions`' answer,
/// not a prefix of the queue: the wave is pairwise conflict-free, so a cluster
/// of overlapping candidates costs one slot instead of all of them. Readiness
/// reads the same snapshot and calls the same routine, which is what keeps the
/// diagnostic and the drain from disagreeing about who starts.
pub(in super::super) fn classify_workspace_auto_tasks(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let submitted_max_active_leaf_runs = templated_u64(
        action,
        input,
        "max_active_leaf_runs",
        DEFAULT_MAX_ACTIVE_LEAF_RUNS,
    )?;
    // [ORB-11253] The submitted ceiling is a snapshot; the run's own control is
    // the live one. Reading it here, per iteration, is what makes an adjustment
    // take effect on the next admission without replacing the coordinator.
    let worker_limit = live_worker_limit(runtime, input);
    let max_active_leaf_runs = worker_limit
        .as_ref()
        .map_or(submitted_max_active_leaf_runs, |limit| {
            u64::from(limit.max_active_leaf_runs)
        });
    let poll_sleep_seconds = templated_u64(
        action,
        input,
        "poll_sleep_seconds",
        DEFAULT_POLL_SLEEP_SECONDS,
    )?;
    let idle_sleep_seconds = templated_u64(
        action,
        input,
        "idle_sleep_seconds",
        DEFAULT_IDLE_SLEEP_SECONDS,
    )?;

    let live_leaves = live_leaf_runs(runtime, action)?;
    let claimed: BTreeSet<String> = live_leaves
        .iter()
        .flat_map(|run| run.task_ids.iter().cloned())
        .collect();
    let admissions_stop = live_admissions_stop(runtime, input);
    let admissions_stopped = admissions_stop.is_some();
    // [ORB-12968] A pending host shutdown would kill anything started now, so
    // the wave admits nothing while one is scheduled. Live children are not
    // touched, and the next iteration after the schedule clears admits again.
    let host_shutdown = runtime.scheduled_host_shutdown();
    if let Some(shutdown) = host_shutdown.as_ref() {
        tracing::warn!(
            target: "orbit.core.host_signal",
            mode = shutdown.mode.as_str(),
            scheduled_at = %shutdown.scheduled_at,
            "drain admits no new leaves: {}",
            shutdown.describe(),
        );
    }
    // [ORB-13901] Sustained host pressure holds the wave the same way until
    // every resource is back below its resume mark. Live children are never
    // touched, and unknown telemetry admits.
    let resource = runtime.resource_admission();

    // [ORB-12617] Slots are shared with pull-mode admission, so the occupancy
    // that decides this wave is the store's one reading of both paths — live
    // wrappers, every leaf definition they or a claim bind, and pending
    // admissions no run represents yet — not this classifier's own wrapper
    // count.
    let occupancy = shared_leaf_occupancy(runtime)
        .map_err(|error| action_failed(action, format!("read shared leaf occupancy: {error}")))?;
    // Between iterations nothing of this drain is in flight in-process: a
    // drain worker yields to a pending generation switch or hands itself over
    // to a replaced installation here.
    runtime.drain_upgrade_boundary();
    let free_slots = if admissions_stopped || host_shutdown.is_some() || resource.throttle.is_some()
    {
        0
    } else {
        usize::try_from(max_active_leaf_runs)
            .unwrap_or(usize::MAX)
            .saturating_sub(occupancy.occupied)
    };

    let pools = runtime
        .auto_crew_pools_for_input(input)
        .map_err(|error| action_failed(action, error.to_string()))?;
    let allowlist = allowlist_from_input(runtime, action, input)?;
    // The same snapshot readiness reads, so the two cannot disagree about the
    // eligible population before they even reach the selection rule.
    let snapshot = backlog_snapshot(runtime, action, allowlist.as_ref(), &pools)?;
    // Priority/age order is the snapshot's, and everything below preserves it:
    // the slots are scarce, so they go to the front of the queue rather than to
    // whichever tasks happen to sort last.
    let pending: Vec<String> = snapshot
        .admissible_leaves
        .iter()
        .filter(|task_id| !claimed.contains(*task_id))
        .cloned()
        .collect();

    // [ORB-11973] The wave used to be `pending[..free_slots]`, which could hand
    // every slot to one cluster of overlapping tasks and leave independent work
    // queued behind a contention it created itself. Select a mutually
    // compatible set instead, walking past a blocked candidate to the next
    // compatible one rather than stopping at it.
    let max_tasks = candidate_pool_limit(action, input)?;
    let examined = &pending[..pending.len().min(max_tasks)];
    let holders = AdmissionHolders::new(
        &snapshot.lock_holders,
        &claimed,
        &snapshot.task_lookup,
        runtime.paths().repo_root.as_path(),
    );
    let selection = select_admissions(
        examined,
        &snapshot.task_lookup,
        runtime.paths().repo_root.as_path(),
        &holders,
        free_slots,
    );
    // `max_tasks` bounds how much of the backlog one iteration expands lock
    // footprints for. It only hides work when the wave ran out of *examined*
    // candidates rather than out of slots, so report exactly that case instead
    // of leaving a short wave looking like an empty backlog.
    let candidate_pool_truncated =
        pending.len() > examined.len() && selection.selected.len() < free_slots;
    let admitted = &selection.selected;
    let loose_task_dispatches: Vec<Value> = admitted
        .iter()
        .map(|task_id| json!({ "task_ids": [task_id] }))
        .collect();

    let has_leaves = !loose_task_dispatches.is_empty();
    // Idle means "this iteration started nothing", which is not the same as
    // "there is nothing to do": a saturated drain with a full backlog behind
    // it is idle in this sense and waits the short poll, while a genuinely
    // empty workspace waits the long one.
    let idle = !has_leaves;
    // A throttled drain polls for recovery rather than settling into the
    // long wait an empty backlog gets.
    let sleep_seconds = if pending.is_empty() && resource.throttle.is_none() {
        idle_sleep_seconds
    } else {
        poll_sleep_seconds
    };

    record_last_pass(
        runtime,
        input,
        DrainAdmissionPass {
            recorded_at: Utc::now(),
            queued: (pending.len() - admitted.len()) as u64,
            deferred: selection
                .deferred
                .iter()
                .map(|deferred| DrainWaitingTask {
                    task_id: deferred.task_id.clone(),
                    reason: None,
                    blocked_by: deferred.blocking_task_ids(),
                    detail: None,
                })
                .collect(),
            excluded: waiting_excluded(&snapshot.excluded),
            excluded_total: snapshot.excluded.len() as u64,
            resource_throttle: resource.throttle.clone(),
            last_pass_error_code: None,
            last_pass_error: None,
            consecutive_pass_failures: 0,
            degraded: false,
        },
    );

    Ok(json!({
        "loose_task_ids": admitted,
        "loose_task_dispatches": loose_task_dispatches,
        "has_leaves": has_leaves,
        "idle": idle,
        "sleep_seconds": sleep_seconds,
        "pending_backlog": pending.len(),
        // [ORB-11973] Why a wave is shorter than its free slots. A deferral
        // names the tasks and selectors it would have collided with, so a drain
        // that looks under-filled can be read as contention rather than as an
        // empty backlog.
        "deferred_conflicts": selection.deferred_json(),
        "candidate_pool_size": examined.len(),
        "candidate_pool_truncated": candidate_pool_truncated,
        "active_leaf_runs": occupancy.occupied,
        "wrapper_leaf_runs": live_leaves.len(),
        "leaf_occupancy_by_pipeline": occupancy.per_pipeline,
        "free_slots": free_slots,
        "max_active_leaf_runs": max_active_leaf_runs,
        "submitted_max_active_leaf_runs": submitted_max_active_leaf_runs,
        "worker_limit_source": if worker_limit.is_some() { "run_control" } else { "run_input" },
        "worker_limit": worker_limit,
        "admissions_stopped": admissions_stopped,
        "admissions_stop": admissions_stop,
        "host_shutdown": host_shutdown,
        "resource_throttle": resource.throttle,
        "resource_telemetry_unknown": resource.unknown,
    }))
}

/// How many excluded tasks one pass records; the total is recorded beside the
/// list so a truncated one still reads as truncated.
const EXCLUDED_BACKLOG_RECORDED: usize = 20;

fn waiting_excluded(excluded: &[BacklogTaskExclusion]) -> Vec<DrainWaitingTask> {
    excluded
        .iter()
        .take(EXCLUDED_BACKLOG_RECORDED)
        .map(|entry| DrainWaitingTask {
            task_id: entry.id.clone(),
            reason: serde_json::to_value(entry.reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string)),
            blocked_by: entry
                .conflicts
                .iter()
                .map(|conflict| conflict.locking_task_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            detail: entry.detail.clone(),
        })
        .collect()
}

/// Persist what this pass left waiting on the drain's own run state, so
/// `orbit run show` can report it after the drain ends. Best effort: a run
/// that cannot record it must still admit work.
fn record_last_pass(runtime: &OrbitRuntime, input: &Value, pass: DrainAdmissionPass) {
    let Some(run_id) = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let mut pass = Some(pass);
    if let Err(error) = runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            state.drain_last_pass = pass.take();
            Ok(())
        })
    {
        tracing::warn!(
            target: "orbit.core.job_run",
            run_id,
            %error,
            "drain could not record its admission pass; run show will not list what it left waiting"
        );
    }
}

/// A numeric loop input, tolerating the string a template renders. A step's
/// `default_input` value goes through the template engine, so `5` arrives as
/// `"5"`; an input the caller omitted renders as an empty string rather than
/// JSON `null`, which is the default case.
fn templated_u64(
    action: &str,
    input: &Value,
    name: &str,
    default: u64,
) -> Result<u64, DispatchError> {
    let Some(raw) = input.get(name) else {
        return Ok(default);
    };
    match raw {
        Value::Null => Ok(default),
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| action_failed(action, format!("`{name}` must be a whole number"))),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(default);
            }
            text.parse::<u64>().map_err(|err| {
                action_failed(action, format!("`{name}` '{text}' is not a number: {err}"))
            })
        }
        other => Err(action_failed(
            action,
            format!("`{name}` must be a number, got {other}"),
        )),
    }
}

/// How many backlog candidates one iteration may expand lock footprints for.
///
/// Mirrors `list_backlog_tasks`'s `max_tasks` bound — same default, same
/// ceiling — because the two describe the same scan. It goes through
/// [`templated_u64`] rather than `as_u64` because the drain job templates the
/// value, which renders `50` as `"50"`.
pub(super) fn candidate_pool_limit(action: &str, input: &Value) -> Result<usize, DispatchError> {
    let requested = templated_u64(action, input, "max_tasks", DEFAULT_CANDIDATE_POOL)?;
    Ok(usize::try_from(requested.clamp(1, MAX_CANDIDATE_POOL)).unwrap_or(usize::MAX))
}
