use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_engine::DispatchError;
use orbit_types::workflow::{
    DrainAdmissionPass, DrainCapacity, DrainPassObservation, DrainPassOutcome, DrainWaitingTask,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::admission::auto_admission::{
    AdmissionHolders, DeferralReason, select_admissions,
};
use crate::application::admission::backlog_exclusion::{allowlist_from_input, backlog_snapshot};
use crate::application::admission::claim_holders::ClaimHolders;
use crate::application::admission::cpu_light::{LightBudget, ResourceGate};
use crate::application::admission::gap_fit::{GapFit, GapFitMode};
use crate::application::admission::pass_error::PassError;
use crate::application::admission::stuck_head::report_stuck_head;
use crate::application::admission::unknown_footprint::{FootprintGuard, whole_tree_holder};
use crate::application::distributed::OwnerRepairDrain;
use crate::runtime::host_signal::HOST_SHUTDOWN_SCHEDULED;

use super::super::drains::{
    DEFAULT_MAX_ACTIVE_LEAF_RUNS, drain_run_id, live_admissions_stop, live_leaf_runs,
    live_worker_limit,
};

use super::forecast::{record_admission_forecast, record_last_pass};
use super::gap_fit::{
    GAP_FIT_REASON, GapFitPass, gap_fit_selection, release_refused_reservations,
    release_unadmitted_reservations,
};
use super::lock_busy::defer_lock_busy_pass;
use super::pool::{
    DEFAULT_IDLE_SLEEP_SECONDS, DEFAULT_POLL_SLEEP_SECONDS, candidate_pool_limit, templated_u64,
};
use super::waiting::{admit_through_section, waiting_excluded, waiting_reason};

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
/// rather than failed: see [`super::lock_busy::defer_lock_busy_pass`].
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
    let mode = super::super::mode::effective_mode(runtime, input)
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
    // [ORB-17304] The owner declares its census row on each pass of its
    // local drain, before the snapshot orders against it.
    if let Some(run_id) = drain_run_id(input) {
        runtime.record_owner_census(run_id);
    }
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
    // [ORB-17303] A stopped landing's repair is offered to its original
    // executor first. Once the fallback window has passed, this owner pulls
    // it into one of these slots; until then the pass names who it waits on.
    let repairs = runtime
        .owner_repair_fallback(&OwnerRepairDrain {
            drain_run_id: drain_run_id(input),
            action,
            held: if admissions_stopped {
                Some("admissions_stopped")
            } else if host_shutdown_holding.is_some() {
                Some(HOST_SHUTDOWN_SCHEDULED)
            } else {
                None
            },
            max_active_leaf_runs,
            free_slots,
        })
        .map_err(|error| PassError::store(action, "read pending landing repairs", error))?;
    let free_slots = free_slots.saturating_sub(repairs.reserved_slots);
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
            &snapshot.published_resumes,
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
    if let Err(error) = admit_through_section(runtime, action, &mut selection, &snapshot) {
        return Err(release_unadmitted_reservations(runtime, &reserved, error));
    }
    // A candidate the section then refused starts no run, and gap fit would
    // reserve it again on the next pass: give back every reservation of the
    // wave that the section did not admit.
    release_refused_reservations(runtime, &reserved, &selection.selected);
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
                    findings: Vec::new(),
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
    let idle = !has_leaves && repairs.started.is_none();
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
        deferred: repairs
            .waiting
            .iter()
            .cloned()
            .chain(selection.deferred.iter().map(|deferred| {
                let blocked_by = deferred.blocking_task_ids();
                DrainWaitingTask {
                    task_id: deferred.task_id.clone(),
                    reason: (deferred.reason != DeferralReason::Conflict)
                        .then(|| deferred.reason.as_str().to_string()),
                    detail: deferred.detail.clone().or_else(|| {
                        claim_holders.detail_for(blocked_by.iter().map(String::as_str))
                    }),
                    blocked_by,
                    findings: Vec::new(),
                }
            }))
            .chain(gap_fit_waiting.iter().cloned())
            .collect(),
        deferred_total: (repairs.waiting.len() + selection.deferred.len() + gap_fit_waiting.len())
            as u64,
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
        admitted: !admitted.is_empty() || repairs.started.is_some(),
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
                    .chain(&repairs.waiting)
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
        // [ORB-17303] The repair-only pull this pass started for pending
        // landing repairs past their fallback window.
        "owner_repair_pull": repairs.started,
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
