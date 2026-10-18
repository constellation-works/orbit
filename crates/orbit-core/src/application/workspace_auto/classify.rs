use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, Utc};
use orbit_engine::DispatchError;
use orbit_store::contracts::{AdmissionForecastRecord, OwnerWaveCandidate};
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task};
use orbit_types::workflow::{
    DrainAdmissionForecast, DrainAdmissionPass, DrainCapacity, DrainPassObservation,
    DrainPassOutcome, DrainWaitingTask, GapFitHead, RunStateUpdate,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::admission::auto_admission::{
    AdmissionHolders, AdmissionSelection, DeferralReason, select_admissions,
};
use crate::application::admission::backlog_exclusion::{
    BacklogTaskExclusion, allowlist_from_input, backlog_snapshot,
};
use crate::application::admission::claim_holders::ClaimHolders;
use crate::application::admission::cpu_light::{LightBudget, ResourceGate};
use crate::application::admission::gap_fit::{GapFit, GapFitMode, HeadWait, task_lookup};
use crate::application::admission::pass_error::PassError;
use crate::application::admission::stuck_head::report_stuck_head;
use crate::application::admission::unknown_footprint::{FootprintGuard, whole_tree_holder};
use crate::runtime::host_resource::forecast::{ForecastDecision, ForecastPoints};
use crate::runtime::host_signal::HOST_SHUTDOWN_SCHEDULED;
use crate::runtime::task::locks::lock_context_files_for_task;

use super::drains::{
    DEFAULT_MAX_ACTIVE_LEAF_RUNS, drain_run_id, live_admissions_stop, live_leaf_runs,
    live_worker_limit,
};

/// Wait before re-listing when the backlog has admissible work but every slot
/// is occupied. This is the latency a freed slot sits idle, so it is much
/// shorter than the idle wait.
const DEFAULT_POLL_SLEEP_SECONDS: u64 = 30;

/// Wait before re-listing when nothing is admissible at all. Long, because the
/// only thing that can change is a task arriving or a live child finishing.
const DEFAULT_IDLE_SLEEP_SECONDS: u64 = 60;

/// Consecutive admission passes a drain defers on a busy store lock before it
/// fails with that cause. A deferred pass has already waited out one lock
/// acquisition deadline (30 s) and then waits the drain's poll interval
/// (`poll_sleep_seconds`, 30 s by default) before the next one, so ten in a
/// row are five to ten minutes in which no pass could read or write the task
/// store: long enough to outlast a burst of admission sections, short enough
/// that a store that stays locked still ends the drain visibly.
const TRANSIENT_PASS_FAILURE_LIMIT: u32 = 10;

/// The stable code a deferred pass records as its `last_pass_error_code`.
const LOCK_BUSY_PASS_CODE: &str = orbit_common::LOCK_BUSY_ERROR_CODE;

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
///
/// A pass that loses only to a busy task-store lock (`lock_busy`) is deferred
/// rather than failed: see [`defer_lock_busy_pass`].
pub(crate) fn classify_workspace_auto_tasks(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let snapshot = runtime.admission_policy_snapshot();
    let runtime = &snapshot;
    match admission_pass(runtime, action, input) {
        Ok(output) => Ok(output),
        Err(error) if error.is_lock_busy() => defer_lock_busy_pass(runtime, action, input, error),
        Err(error) => Err(error.into()),
    }
}

fn admission_pass(runtime: &OrbitRuntime, action: &str, input: &Value) -> Result<Value, PassError> {
    let mode = super::mode::effective_mode(runtime, input)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    runtime
        .record_backlog_pilot_operator_handoffs()
        .map_err(|error| PassError::store(action, "record pilot operator handoffs", error))?;

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
    // A delivery PR an operator merged outside Orbit settles its task once
    // stale runs are reconciled. The settlement starts nothing, so a forge or
    // store failure only logs and admission goes on.
    if runtime.coordination_task_write_refusal().is_none()
        && let Err(error) = runtime.settle_externally_merged_tasks()
    {
        tracing::warn!(
            target: "orbit.core.external_merge",
            error = %error,
            "the admission pass could not settle tasks from merged delivery pull requests"
        );
    }
    let claimed: BTreeSet<String> = live_leaves
        .iter()
        .flat_map(|run| run.task_ids.iter().cloned())
        .collect();
    let admissions_stop = live_admissions_stop(runtime, input);
    let admissions_stopped = admissions_stop.is_some();
    // [ORB-12968] A host shutdown inside the hold horizon would kill anything
    // started now, so the wave admits nothing while one is that close. A
    // later schedule is reported and does not hold the wave. Live children
    // are not touched, and the next iteration after the horizon lifts admits
    // again.
    let host_shutdown = runtime.scheduled_host_shutdown();
    let host_shutdown_holding = runtime.host_shutdown_admission_hold();
    if let Some(shutdown) = host_shutdown_holding.as_ref() {
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
    // touched, and unknown telemetry admits. [ORB-14624] CPU pressure alone
    // still admits CPU-light leaves within their reserved budget, below.
    runtime.reclaim_worktrees_on_admission();
    let resource = runtime.resource_admission();

    // [ORB-12617] Slots are shared with pull-mode admission, so the occupancy
    // that decides this wave is the store's one reading of both paths — live
    // wrappers, every leaf definition they or a claim bind, and pending
    // admissions no run represents yet — not this classifier's own wrapper
    // count.
    let jobs = runtime.stores().jobs();
    let occupancy = drain_run_id(input)
        .map_or_else(
            || jobs.drain_leaf_occupancy(),
            |id| jobs.drain_leaf_occupancy_for_run(id),
        )
        .map_err(|error| PassError::store(action, "read shared leaf occupancy", error))?;
    // Between iterations nothing of this drain is in flight in-process: a
    // drain worker yields to a pending generation switch or hands itself over
    // to a replaced installation here.
    runtime.drain_upgrade_boundary();
    let unthrottled_slots = if admissions_stopped || host_shutdown_holding.is_some() {
        0
    } else {
        usize::try_from(max_active_leaf_runs)
            .unwrap_or(usize::MAX)
            .saturating_sub(occupancy.occupied)
    };

    let pools = runtime
        .auto_crew_pools_for_input(input)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let allowlist = allowlist_from_input(runtime, action, input)?;
    // The same snapshot readiness reads, so the two cannot disagree about the
    // eligible population before they even reach the selection rule.
    let review = crate::application::review::input_run_review_admission(runtime, input)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let mut snapshot =
        backlog_snapshot(runtime, action, allowlist.as_ref(), &pools, review.as_ref())?;
    // A live leaf may carry a task that has since closed; its claim and its
    // light slot still count.
    snapshot
        .include_tasks(runtime, &claimed)
        .map_err(|error| PassError::store(action, "read claimed tasks", error))?;
    // [ORB-14624] A CPU-only throttle leaves the reserved light slots open.
    let light_budget = LightBudget::new(
        runtime
            .context
            .settings()
            .resource_throttle()
            .cpu_light_leaves,
        &claimed,
        &snapshot.task_lookup,
    );
    let gate = ResourceGate::new(resource.throttle.as_ref(), &light_budget);
    let free_slots = gate.free_slots(unthrottled_slots, &light_budget);
    // Priority/age order is the snapshot's, and everything below preserves it:
    // the slots are scarce, so they go to the front of the queue rather than to
    // whichever tasks happen to sort last.
    let pending: Vec<String> = snapshot
        .admissible_leaves
        .iter()
        .filter(|task_id| !claimed.contains(*task_id))
        .cloned()
        .collect();
    // Under a CPU-only throttle the wave is chosen from CPU-light leaves
    // alone, in the same order, so heavier work ahead of them in the queue
    // does not hide them from the candidate pool.
    let gated: Vec<String>;
    let candidates: &[String] = if gate == ResourceGate::Open {
        &pending
    } else {
        gated = pending
            .iter()
            .filter(|task_id| {
                snapshot
                    .task_lookup
                    .get(*task_id)
                    .is_some_and(|task| gate.admits(task))
            })
            .cloned()
            .collect();
        &gated
    };

    // [ORB-11973] The wave used to be `pending[..free_slots]`, which could hand
    // every slot to one cluster of overlapping tasks and leave independent work
    // queued behind a contention it created itself. Select a mutually
    // compatible set instead, walking past a blocked candidate to the next
    // compatible one rather than stopping at it.
    let max_tasks = candidate_pool_limit(action, input)?;
    let examined = &candidates[..candidates.len().min(max_tasks)];
    let holders = AdmissionHolders::new(
        &snapshot.lock_holders,
        &claimed,
        &snapshot.task_lookup,
        runtime.paths().repo_root.as_path(),
        runtime.unlocked_context_paths(),
    );
    // [ORB-15191] Work with no footprint waits for its pilot or goes alone,
    // unless this drain runs one leaf at a time.
    let whole_tree = whole_tree_holder(&claimed, &snapshot.task_lookup);
    let select = |gap_fit: Option<&mut GapFit<'_>>| {
        select_admissions(
            examined,
            &snapshot.task_lookup,
            runtime.paths().repo_root.as_path(),
            runtime.unlocked_context_paths(),
            &holders,
            free_slots,
            FootprintGuard {
                enabled: max_active_leaf_runs > 1,
                leaves_in_flight: occupancy.occupied > 0,
                whole_tree_holder: whole_tree.clone(),
                waits: &snapshot.footprint_waits,
            },
            gap_fit,
        )
    };
    // Gap fit (the effective per-drain mode) judges the wave against the
    // host load forecast after every check above, so it only removes
    // candidates; the ceiling and the throttle have already set
    // `free_slots`. `off` never builds a forecast.
    let gap_fit_mode = GapFitMode::from_setting(mode.as_str());
    let GapFitPass {
        mut selection,
        record: admission_forecast,
        forecast,
        reserved,
    } = match gap_fit_mode {
        None => GapFitPass {
            selection: select(None),
            record: None,
            forecast: None,
            reserved: Vec::new(),
        },
        Some(mode) => {
            let can_admit = free_slots > 0 && !examined.is_empty();
            gap_fit_selection(runtime, action, input, mode, &pending, can_admit, &select)?
        }
    };
    // [ORB-16376] `enforce` has already committed this wave's reservations.
    // A pass that stops here admits nothing, so it gives them back: a pass
    // deferred on a busy lock is retried, and every retry would otherwise
    // stack another copy of the same reservations onto the host's forecast.
    if let Err(error) =
        admit_through_section(runtime, action, &mut selection, &snapshot.task_lookup)
    {
        return Err(release_unadmitted_reservations(runtime, &reserved, error));
    }
    if let (Some(forecast), Some(record)) = (forecast, admission_forecast.as_ref()) {
        record_admission_forecast(
            runtime,
            input,
            forecast,
            record,
            &selection.selected,
            &snapshot.task_lookup,
        );
    }
    // What `enforce` kept out is waiting like a deferral; `shadow` only
    // records what it would have done.
    let gap_fit_waiting: Vec<DrainWaitingTask> = admission_forecast
        .as_ref()
        .filter(|_| gap_fit_mode == Some(GapFitMode::Enforce))
        .map(|record| {
            record
                .skipped
                .iter()
                .map(|skip| DrainWaitingTask {
                    task_id: skip.task_id.clone(),
                    reason: Some(GAP_FIT_REASON.to_string()),
                    blocked_by: Vec::new(),
                    detail: Some(skip.detail.clone()),
                })
                .collect()
        })
        .unwrap_or_default();
    // [ORB-15514] A lock wait whose blocker a follower's claim holds says so.
    let claim_holders = ClaimHolders::read(runtime);
    // `max_tasks` bounds how much of the backlog one iteration expands lock
    // footprints for. It only hides work when the wave ran out of *examined*
    // candidates rather than out of slots, so report exactly that case instead
    // of leaving a short wave looking like an empty backlog.
    let candidate_pool_truncated =
        candidates.len() > examined.len() && selection.selected.len() < free_slots;
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
    // long wait an empty backlog gets, and so does one whose candidates gap
    // fit refused: they are still pending. The forecast is per minute, so no
    // gap it shows can open sooner than the poll.
    let sleep_seconds = if pending.is_empty() && resource.throttle.is_none() {
        idle_sleep_seconds
    } else {
        poll_sleep_seconds
    };

    let mut pass = DrainAdmissionPass {
        recorded_at: Utc::now(),
        capacity: occupancy.inherited.map(|inherited| DrainCapacity {
            active_leaf_runs: occupancy.occupied as u64,
            inherited_leaf_runs: inherited as u64,
            max_active_leaf_runs,
        }),
        queued: (pending.len() - admitted.len()) as u64,
        deferred: selection
            .deferred
            .iter()
            .map(|deferred| {
                let blocked_by = deferred.blocking_task_ids();
                DrainWaitingTask {
                    task_id: deferred.task_id.clone(),
                    reason: (deferred.reason != DeferralReason::Conflict)
                        .then(|| deferred.reason.as_str().to_string()),
                    detail: deferred.detail.clone().or_else(|| {
                        claim_holders.detail_for(blocked_by.iter().map(String::as_str))
                    }),
                    blocked_by,
                }
            })
            .chain(gap_fit_waiting.iter().cloned())
            .collect(),
        deferred_total: (selection.deferred.len() + gap_fit_waiting.len()) as u64,
        excluded: waiting_excluded(&snapshot.excluded, &claim_holders),
        excluded_total: snapshot.excluded.len() as u64,
        waiting_recorded_at: None,
        waiting_by_reason: BTreeMap::new(),
        consecutive_idle_passes: 0,
        resource_throttle: resource.throttle.clone(),
        admission_refusal: None,
        last_pass_error_code: None,
        last_pass_error: None,
        consecutive_pass_failures: 0,
        degraded: false,
        admission_forecast,
    };
    let observation = LocalPass {
        admitted: !admitted.is_empty(),
        admissions_stopped,
        host_shutdown_holding: host_shutdown_holding.is_some(),
        throttle: resource.throttle.as_ref(),
        pending: !pending.is_empty(),
        ceiling_full: unthrottled_slots == 0,
        gap_fit_held: !gap_fit_waiting.is_empty(),
        // From the full lists: the pass records a bounded excluded list.
        waiting_reasons: snapshot
            .excluded
            .iter()
            .map(|entry| waiting_reason(entry.reason))
            .chain(
                selection
                    .deferred
                    .iter()
                    .map(|deferred| deferred.reason.as_str().to_string()),
            )
            .chain(
                gap_fit_waiting
                    .iter()
                    .filter_map(|task| task.reason.clone()),
            )
            .collect(),
    }
    .observe();
    // Persist the stall's identity before its external friction effect. At
    // threshold one, a first protected pass or a progress reset creates the
    // identity now; losing it after filing would give a retry a different key.
    let pass_recorded = record_last_pass(runtime, input, pass.clone(), Some(observation));
    // Only `enforce` idles the drain behind a stuck head, so only it files;
    // `shadow` records the stall as what `enforce` would have reported.
    let friction_recorded = if let (true, Some(GapFitMode::Enforce), Some(run_id), Some(head)) = (
        pass_recorded,
        gap_fit_mode,
        drain_run_id(input),
        pass.admission_forecast
            .as_mut()
            .and_then(|record| record.head.as_mut()),
    ) {
        let title = snapshot
            .task_lookup
            .get(&head.task_id)
            .map(|task| task.title.as_str());
        let unreported = head
            .stall
            .as_ref()
            .is_some_and(|stall| stall.friction_id.is_none());
        report_stuck_head(runtime, run_id, head, title);
        unreported
            && head
                .stall
                .as_ref()
                .is_some_and(|stall| stall.friction_id.is_some())
    } else {
        false
    };
    if friction_recorded {
        // Best effort: even if this id write fails, the durable stall start
        // makes the next pass reuse the friction that was just filed.
        record_last_pass(runtime, input, pass.clone(), None);
    }

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
        // Gap fit's decisions under `shadow` or `enforce`; absent with `off`.
        "admission_forecast": pass.admission_forecast,
        "candidate_pool_size": examined.len(),
        "candidate_pool_truncated": candidate_pool_truncated,
        "active_leaf_runs": occupancy.occupied,
        "inherited_leaf_runs": occupancy.inherited,
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
        "host_shutdown_holding": host_shutdown_holding.is_some(),
        "resource_throttle": resource.throttle,
        "resource_telemetry_unknown": resource.unknown,
        // [ORB-14624] Light slots a CPU-only throttle still admits into.
        "cpu_light_budget": light_budget.to_json(gate == ResourceGate::LightOnly),
    }))
}

/// The waiting reason of a candidate `enforce` gap fit kept out of a pass.
const GAP_FIT_REASON: &str = "gap_fit";

/// What decided a local pass's outcome, for its drain's pass counters.
struct LocalPass<'a> {
    admitted: bool,
    admissions_stopped: bool,
    host_shutdown_holding: bool,
    throttle: Option<&'a orbit_types::workflow::ResourceThrottle>,
    /// Admissible candidates not already claimed remained.
    pending: bool,
    /// Every slot under the live ceiling was occupied.
    ceiling_full: bool,
    /// `enforce` gap fit kept a candidate out.
    gap_fit_held: bool,
    waiting_reasons: BTreeSet<String>,
}

impl LocalPass<'_> {
    /// The pass's one outcome, in [`DrainPassOutcome`]'s precedence.
    fn observe(self) -> DrainPassObservation {
        let mut observation = DrainPassObservation {
            waiting_reasons: self.waiting_reasons,
            ..DrainPassObservation::default()
        };
        observation.outcome = if self.admitted {
            DrainPassOutcome::Admitted
        } else if self.admissions_stopped || self.host_shutdown_holding {
            observation.held_reason = Some(
                if self.admissions_stopped {
                    "admissions_stopped"
                } else {
                    HOST_SHUTDOWN_SCHEDULED
                }
                .to_string(),
            );
            DrainPassOutcome::Held
        } else if let Some(throttle) = self.throttle {
            observation.throttled = throttle.kinds();
            DrainPassOutcome::Throttle
        } else if !self.pending {
            DrainPassOutcome::NoCandidate
        } else if self.ceiling_full {
            DrainPassOutcome::CeilingFull
        } else if self.gap_fit_held {
            DrainPassOutcome::GapFitHeld
        } else {
            DrainPassOutcome::Deferred
        };
        observation
    }
}

/// A pass's wave and, under `shadow` or `enforce`, its gap fit.
struct GapFitPass {
    selection: AdmissionSelection,
    record: Option<DrainAdmissionForecast>,
    /// The forecast the pass decided against, when it built one.
    forecast: Option<ForecastPoints>,
    /// The `(workspace, task)` reservations `enforce` wrote for the wave.
    reserved: Vec<(String, String)>,
}

/// Give back the reservations a pass wrote for a wave it then failed to
/// admit, and return the pass's failure unchanged.
///
/// Reservations are released by task, which is how the host store keys them.
/// A release that fails is logged and the pass's own failure still decides
/// what happens: a pass that lost only to a busy lock is still deferred, so a
/// contended host store does not end the drain on its first pass. The
/// reservations it could not release only hold admission back, expire with
/// the reservation TTL, and the deferral limit bounds how many such passes
/// can stack them.
fn release_unadmitted_reservations(
    runtime: &OrbitRuntime,
    reserved: &[(String, String)],
    error: PassError,
) -> PassError {
    if reserved.is_empty() {
        return error;
    }
    let keys: Vec<(&str, &str)> = reserved
        .iter()
        .map(|(workspace, task)| (workspace.as_str(), task.as_str()))
        .collect();
    if let Err(release_error) = release_reservations(runtime, &keys) {
        tracing::warn!(
            target: "orbit.core.job_run",
            error = %error.reason(),
            release_error = %release_error,
            reservations = ?reserved,
            "drain could not release the reservations of a wave it did not admit; they expire with their TTL"
        );
    }
    error
}

fn release_reservations(
    runtime: &OrbitRuntime,
    keys: &[(&str, &str)],
) -> Result<(), orbit_common::OrbitError> {
    #[cfg(feature = "test-support")]
    if let Some(error) = release_failure_hook::take() {
        return Err(error);
    }
    runtime.release_admission_reservations(keys)
}

/// Fail the next reservation release on this thread, for a test that needs
/// the release to lose to the host store while the pass's own failure stands.
#[cfg(feature = "test-support")]
pub(crate) mod release_failure_hook {
    use std::cell::RefCell;

    use orbit_common::OrbitError;

    type Failure = Box<dyn FnOnce() -> OrbitError>;

    thread_local! {
        static HOOK: RefCell<Option<Failure>> = RefCell::new(None);
    }

    pub(super) fn take() -> Option<OrbitError> {
        HOOK.with(|slot| slot.borrow_mut().take())
            .map(|fail| fail())
    }

    /// Run `operation` with the first admission-reservation release a drain
    /// pass attempts on this thread failing with `fail()`.
    pub fn with_reservation_release_failure<T>(
        fail: impl FnOnce() -> OrbitError + 'static,
        operation: impl FnOnce() -> T,
    ) -> T {
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                HOOK.with(|slot| *slot.borrow_mut() = None);
            }
        }
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(fail)));
        let _guard = Clear;
        operation()
    }
}

/// Select the wave with gap fit, under the host admission lock.
///
/// The forecast is built, the wave chosen and, in `enforce`, a reservation
/// written for each admitted task, all in one host lock, so another drain on
/// the host counts them before their runs exist. `shadow` reserves nothing
/// and admits what `select(None)` admits, as `off` does. A pass that can
/// admit nothing takes no lock and carries only the starvation guard's head.
///
/// A candidate the pull admission section refuses afterwards keeps its
/// reservation until it expires, which can only hold back admission. A
/// section that fails outright admits none of the wave, and the pass
/// releases every reservation it wrote ([`release_unadmitted_reservations`]).
fn gap_fit_selection(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    mode: GapFitMode,
    pending: &[String],
    can_admit: bool,
    select: &dyn Fn(Option<&mut GapFit<'_>>) -> AdmissionSelection,
) -> Result<GapFitPass, PassError> {
    let settings = runtime.auto_concurrency_settings();
    let previous = previous_head(runtime, input);
    let head_wait = HeadWait {
        previous: previous.as_ref(),
        max_wait: Duration::minutes(i64::from(settings.head_max_wait_minutes)),
        stuck_passes: settings.head_stuck_passes,
        now: Utc::now(),
    };
    if !can_admit {
        return Ok(GapFitPass {
            selection: select(None),
            record: Some(head_wait.record_without_forecast(mode, pending)),
            forecast: None,
            reserved: Vec::new(),
        });
    }
    let workspace_id = runtime
        .workspace_id()
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let decided = runtime.admit_against_forecast(|forecast| {
        let mut gap_fit = GapFit::new(
            mode,
            forecast,
            settings.admit_cpu_percent,
            settings.admit_memory_percent,
            &workspace_id,
            head_wait,
        );
        let selection = select(Some(&mut gap_fit));
        let (record, reservations) = gap_fit.finish(pending);
        let admitted = match mode {
            GapFitMode::Enforce => reservations,
            GapFitMode::Shadow => Vec::new(),
        };
        let reserved = admitted
            .iter()
            .map(|request| (request.lookup.workspace_id.clone(), request.task_id.clone()))
            .collect::<Vec<_>>();
        Ok(ForecastDecision {
            outcome: (selection, record, forecast.accuracy_points(), reserved),
            admitted,
        })
    });
    match (mode, decided) {
        (GapFitMode::Enforce, Ok((selection, record, forecast, reserved))) => Ok(GapFitPass {
            selection,
            record: Some(record),
            forecast: Some(forecast),
            reserved,
        }),
        (GapFitMode::Shadow, Ok((_, record, forecast, _))) => Ok(GapFitPass {
            selection: select(None),
            record: Some(record),
            forecast: Some(forecast),
            reserved: Vec::new(),
        }),
        (GapFitMode::Enforce, Err(error)) => Err(PassError::store(
            action,
            "admit against the host load forecast",
            error,
        )),
        (GapFitMode::Shadow, Err(error)) => {
            tracing::warn!(
                target: "orbit.core.job_run",
                %error,
                "shadow gap fit could not forecast the host; the pass admits as it would without it"
            );
            Ok(GapFitPass {
                selection: select(None),
                record: None,
                forecast: None,
                reserved: Vec::new(),
            })
        }
    }
}

/// The starvation guard's head as the drain's previous pass recorded it.
fn previous_head(runtime: &OrbitRuntime, input: &Value) -> Option<GapFitHead> {
    let run_id = drain_run_id(input)?;
    runtime
        .read_run_state(run_id)
        .ok()
        .flatten()?
        .pull_drain
        .drain_last_pass?
        .admission_forecast?
        .head
}

/// How many excluded tasks one pass records; the total is recorded beside the
/// list so a truncated one still reads as truncated.
const EXCLUDED_BACKLOG_RECORDED: usize = 20;

/// An exclusion's reason code, as the drain records it.
fn waiting_reason(
    reason: crate::application::admission::backlog_exclusion::BacklogTaskExclusionReason,
) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "excluded".to_string())
}

fn waiting_excluded(
    excluded: &[BacklogTaskExclusion],
    claim_holders: &ClaimHolders,
) -> Vec<DrainWaitingTask> {
    excluded
        .iter()
        .take(EXCLUDED_BACKLOG_RECORDED)
        .map(|entry| {
            let blocked_by = entry
                .conflicts
                .iter()
                .map(|conflict| conflict.locking_task_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            // [ORB-15514] The claim behind a lock wait, beside whatever else
            // the exclusion already says.
            let claims = claim_holders.detail_for(
                blocked_by
                    .iter()
                    .map(String::as_str)
                    .filter(|blocker| *blocker != entry.id),
            );
            DrainWaitingTask {
                task_id: entry.id.clone(),
                reason: Some(waiting_reason(entry.reason)),
                blocked_by,
                detail: match (entry.detail.clone(), claims) {
                    (Some(detail), Some(claims)) => Some(format!("{detail} {claims}")),
                    (detail, claims) => detail.or(claims),
                },
            }
        })
        .collect()
}

/// [ORB-15514] Pass the wave through the pull admission section. The wave
/// was chosen from a snapshot and its gates reserve only later, so without
/// this a pull grant could take an admitted footprint in between, or have
/// claimed one just before the snapshot that missed it. The section refuses
/// a candidate a live claim holds and makes every other admitted footprint
/// visible to pull grants before it ends.
fn admit_through_section(
    runtime: &OrbitRuntime,
    action: &str,
    selection: &mut AdmissionSelection,
    task_lookup: &BTreeMap<String, Task>,
) -> Result<(), PassError> {
    let workspace_root = runtime.paths().repo_root.as_path();
    let candidates = selection
        .selected
        .iter()
        .filter_map(|task_id| task_lookup.get(task_id))
        .map(|task| OwnerWaveCandidate {
            task_id: task.id.clone(),
            // [ORB-14247] No-diff work holds no lock, so it excludes no
            // overlapping pull either; the row still keeps the task itself.
            footprint: if task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG) {
                Vec::new()
            } else {
                lock_context_files_for_task(task, workspace_root, runtime.unlocked_context_paths())
            },
        })
        .collect::<Vec<_>>();
    let admissions = runtime
        .stores()
        .tasks()
        .admit_owner_wave(&candidates)
        .map_err(|error| {
            PassError::store(
                action,
                "admit the wave through the admission section",
                error,
            )
        })?;
    for admission in admissions {
        if let (Some(claim), Some(task)) =
            (admission.blocked_by, task_lookup.get(&admission.task_id))
        {
            selection.refuse_claimed(
                task,
                &claim,
                workspace_root,
                runtime.unlocked_context_paths(),
            );
        }
    }
    Ok(())
}

/// Record the pass's forecast, counting what it admitted, with its gap-fit
/// decisions, for the forecast accuracy report. Best effort, like the last
/// pass: a drain that cannot record it must still admit work.
fn record_admission_forecast(
    runtime: &OrbitRuntime,
    input: &Value,
    mut forecast: ForecastPoints,
    record: &DrainAdmissionForecast,
    admitted: &[String],
    tasks: &BTreeMap<String, Task>,
) {
    let Some(run_id) = drain_run_id(input) else {
        return;
    };
    let result = runtime.workspace_id().and_then(|workspace_id| {
        for task in admitted.iter().filter_map(|task_id| tasks.get(task_id)) {
            forecast.admit(&task.id, &task_lookup(&workspace_id, task));
        }
        runtime.record_admission_forecast(&AdmissionForecastRecord {
            workspace_id,
            run_id: run_id.to_string(),
            forecast_at: forecast.at,
            mode: record.mode.clone(),
            cpu_count: forecast.cpu_count,
            memory_total_bytes: forecast.memory_total_bytes,
            baseline_cpu_cores: Some(forecast.baseline_cpu_cores),
            baseline_memory_bytes: Some(forecast.baseline_memory_bytes),
            covered_deliveries: Some(forecast.covered_deliveries.clone()),
            points: forecast.into_points(),
            admitted: admitted.to_vec(),
            pull: None,
            fit_admitted: record
                .admitted
                .iter()
                .map(|admission| admission.task_id.clone())
                .collect(),
            fit_skipped: record
                .skipped
                .iter()
                .map(|skip| skip.task_id.clone())
                .collect(),
        })
    });
    if let Err(error) = result {
        tracing::warn!(
            target: "orbit.core.job_run",
            run_id,
            %error,
            "drain could not record its admission forecast; the accuracy report will miss this pass"
        );
    }
}

/// Persist what this pass left waiting on the drain's own run state, so
/// `orbit run show` can report it after the drain ends. Best effort: a run
/// that cannot record it must still admit work. Returns whether the pass
/// was persisted, so an external report can rely on its durable identity.
///
/// `observation`, when given, counts the pass in the drain's pass counters
/// in the same write; a rewrite of an already counted pass passes `None`.
fn record_last_pass(
    runtime: &OrbitRuntime,
    input: &Value,
    pass: DrainAdmissionPass,
    observation: Option<DrainPassObservation>,
) -> bool {
    let Some(run_id) = drain_run_id(input) else {
        return false;
    };
    let mut pass = Some((pass, observation));
    match runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            if let Some((pass, observation)) = pass.take() {
                if let Some(observation) = observation {
                    state
                        .pull_drain
                        .count_admission_pass(&observation, pass.recorded_at);
                }
                state.pull_drain.record_admission_pass(pass);
            }
            Ok(())
        }) {
        Ok(RunStateUpdate::Updated) => true,
        Ok(RunStateUpdate::NotFound | RunStateUpdate::NoState) => false,
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                %error,
                "drain could not record its admission pass; run show will not list what it left waiting"
            );
            false
        }
    }
}

/// Defer a pass that lost only to a busy lock instead of failing the drain
/// [ORB-16376].
///
/// A drain is a long-lived supervisor: one admission section held past a
/// waiter's 30 s deadline used to fail this step, which failed the drain's
/// loop and ended its window while its children kept running, so nothing
/// was admitted until an operator restarted it. Nothing was decided under
/// the lock, so the pass is skipped instead: it admits nothing, records the
/// lock error (which names the holder) as the drain's last pass with
/// `last_pass_error_code` `lock_busy`, and returns an idle answer that waits
/// the drain's poll interval before the next pass retries. The waiting
/// backlog the previous pass recorded is kept, with the time it was read.
///
/// The count of consecutive deferred passes lives on that record, and any
/// pass that completes resets it. At [`TRANSIENT_PASS_FAILURE_LIMIT`] the
/// pass fails as before, naming the lock and the count, so a store that
/// stays locked still ends the drain. A pass outside a drain, or one whose
/// deferral cannot be recorded, has no count to bound it and fails at once.
fn defer_lock_busy_pass(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    error: PassError,
) -> Result<Value, DispatchError> {
    let Some(run_id) = drain_run_id(input) else {
        return Err(error.into());
    };
    let backoff_seconds = templated_u64(
        action,
        input,
        "poll_sleep_seconds",
        DEFAULT_POLL_SLEEP_SECONDS,
    )?;
    let reason = error.reason();
    let mut failures = None;
    let recorded = runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            let pass = deferred_pass(state.pull_drain.drain_last_pass.as_ref(), &reason);
            failures = Some(pass.consecutive_pass_failures);
            state.pull_drain.count_admission_pass(
                &DrainPassObservation {
                    outcome: DrainPassOutcome::Failed,
                    ..DrainPassObservation::default()
                },
                pass.recorded_at,
            );
            state.pull_drain.record_admission_pass(pass);
            Ok(())
        });
    let failures = match (recorded, failures) {
        (Ok(RunStateUpdate::Updated), Some(failures)) => failures,
        (Ok(_), _) => return Err(error.into()),
        (Err(record_error), _) => {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                error = %record_error,
                "drain could not record a deferred admission pass; the pass fails"
            );
            return Err(error.into());
        }
    };
    if failures >= TRANSIENT_PASS_FAILURE_LIMIT {
        tracing::error!(
            target: "orbit.core.job_run",
            run_id,
            consecutive_pass_failures = failures,
            error = %reason,
            "drain admission failed on a busy task-store lock for too many consecutive passes"
        );
        return Err(DispatchError::action_failed(
            action,
            format!("{reason} ({failures} consecutive admission passes failed on a busy lock)"),
        ));
    }
    tracing::warn!(
        target: "orbit.core.job_run",
        run_id,
        consecutive_pass_failures = failures,
        limit = TRANSIENT_PASS_FAILURE_LIMIT,
        backoff_seconds,
        error = %reason,
        "drain admission pass deferred on a busy task-store lock; the next pass retries"
    );
    Ok(json!({
        "loose_task_ids": [],
        "loose_task_dispatches": [],
        "has_leaves": false,
        "idle": true,
        "sleep_seconds": backoff_seconds,
        "pass_deferred": {
            "code": LOCK_BUSY_PASS_CODE,
            "reason": reason,
            "consecutive_pass_failures": failures,
            "limit": TRANSIENT_PASS_FAILURE_LIMIT,
        },
    }))
}

/// The record of a deferred pass: the previous pass's view of the backlog,
/// read when that pass ran, with this pass's failure and its count.
fn deferred_pass(previous: Option<&DrainAdmissionPass>, reason: &str) -> DrainAdmissionPass {
    let now = Utc::now();
    let consecutive_pass_failures = previous
        .map_or(0, |pass| pass.consecutive_pass_failures)
        .saturating_add(1);
    let mut pass = previous.cloned().unwrap_or_else(|| DrainAdmissionPass {
        recorded_at: now,
        capacity: None,
        queued: 0,
        deferred: Vec::new(),
        deferred_total: 0,
        excluded: Vec::new(),
        excluded_total: 0,
        waiting_recorded_at: None,
        waiting_by_reason: BTreeMap::new(),
        consecutive_idle_passes: 0,
        resource_throttle: None,
        admission_refusal: None,
        last_pass_error_code: None,
        last_pass_error: None,
        consecutive_pass_failures: 0,
        degraded: false,
        admission_forecast: None,
    });
    if previous.is_some() {
        pass.waiting_recorded_at = pass.waiting_recorded_at.or(Some(pass.recorded_at));
    }
    pass.recorded_at = now;
    pass.last_pass_error_code = Some(LOCK_BUSY_PASS_CODE.to_string());
    pass.last_pass_error = Some(reason.to_string());
    pass.consecutive_pass_failures = consecutive_pass_failures;
    pass
}

/// A numeric loop input, tolerating the string a template renders. A step's
/// `default_input` value goes through the template engine, so `5` arrives as
/// `"5"`; an input the caller omitted renders as an empty string rather than
/// JSON `null`, which is the default case.
pub(super) fn templated_u64(
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
        Value::Number(number) => number.as_u64().ok_or_else(|| {
            DispatchError::action_failed(action, format!("`{name}` must be a whole number"))
        }),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(default);
            }
            text.parse::<u64>().map_err(|err| {
                DispatchError::action_failed(
                    action,
                    format!("`{name}` '{text}' is not a number: {err}"),
                )
            })
        }
        other => Err(DispatchError::action_failed(
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
