use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::admission::backlog_exclusion::{
    BacklogSnapshot, backlog_snapshot_observed, sort_tasks_for_automatic_dispatch,
};
use crate::application::admission::claim_holders::ClaimHolders;
use crate::application::admission::cpu_light::ResourceGate;
use crate::application::admission::leaf_occupancy::{occupancy_json, read_leaf_occupancy};
use crate::application::admission::pass_error::PassError;
use crate::runtime::engine::crew::CrewAllowlist;

use super::super::approvals::readiness_approvals;
use super::super::classify::{
    DEFAULT_CANDIDATE_POOL, candidate_pool_limit, leaf_load_failure_holds,
    retain_leaf_load_failure_holds,
};
use super::super::drains::{
    DRAIN_JOB_NAME, drain_ceiling, live_pull_drain, read_live_leaf_runs, shared_leaf_occupancy,
    workspace_drains,
};
use super::super::live_agents;
use super::super::wave::{AdmissionWave, AdmissionWaveInput, unreserved_admission_slots};

use super::api::FleetReadinessFacts;
use super::drain_progress::{self, DrainProgress};
use super::{hosts, projection};

pub(super) const MAX_READINESS_LIMIT: usize = 500;

/// How much of the readiness snapshot its reader consumes.
#[derive(Clone, Copy)]
pub(super) enum ReadinessDepth<'a> {
    Full,
    /// A fleet row: admission facts only, without the claim, approval and
    /// validation diagnostics a row does not report.
    FleetRow(&'a FleetReadinessFacts),
}

/// The tasks a second projection of one shared snapshot produced, or the
/// dashboard's unavailable marker when that projection's id is not in the
/// workspace. Invalid selections stay on this path so a missing `also_task`
/// cannot fail the bounded listing.
pub(super) enum ReadinessDetail {
    Tasks(Vec<Value>),
    Unavailable { task_id: String },
}

struct ProjectedReadinessTasks {
    tasks: Vec<Value>,
    total: usize,
    forecast_view: Option<Value>,
    last_forecast_view: Option<Value>,
}

/// Ids the readiness `tasks` array explains: the bounded backlog when `task_ids`
/// is empty, otherwise those ids, sorted and de-duplicated.
fn readiness_selected_ids(
    snapshot: &BacklogSnapshot,
    task_ids: &[String],
    limit: usize,
) -> Result<Vec<String>, OrbitError> {
    if task_ids.is_empty() {
        let mut backlog = snapshot
            .task_lookup
            .values()
            .filter(|task| task.status == TaskStatus::Backlog)
            .collect::<Vec<_>>();
        sort_tasks_for_automatic_dispatch(
            &mut backlog,
            &snapshot.expiring_batches,
            &snapshot.dispatch_order,
        );
        return Ok(backlog
            .into_iter()
            .take(limit)
            .map(|task| task.id.clone())
            .collect());
    }
    let mut ids = task_ids.to_vec();
    ids.sort();
    ids.dedup();
    if let Some(missing) = ids
        .iter()
        .find(|id| !snapshot.task_lookup.contains_key(*id))
    {
        return Err(OrbitError::InvalidInput(format!(
            "task `{missing}` was not found in this workspace"
        )));
    }
    if ids.len() > limit {
        return Err(OrbitError::InvalidInput(format!(
            "readiness selection contains {} tasks; limit is {limit}",
            ids.len()
        )));
    }
    Ok(ids)
}

pub(super) fn readiness_snapshot(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    max_active_leaf_runs: Option<u32>,
    limit: usize,
    allowed_crews: &[String],
    depth: ReadinessDepth<'_>,
) -> Result<Value, OrbitError> {
    Ok(readiness_snapshot_with_detail(
        runtime,
        task_ids,
        &[],
        max_active_leaf_runs,
        limit,
        allowed_crews,
        depth,
    )?
    .0)
}

/// One admission snapshot, projected for `task_ids`, and when `detail_task_ids`
/// is non-empty a second projection of those ids from the same snapshot.
///
/// The detail ids are loaded only after the primary payload is built. A task
/// the bounded listing does not already hold — a closed task, or one this
/// workspace does not have — therefore cannot change that listing, its totals
/// or its forecast titles.
pub(super) fn readiness_snapshot_with_detail(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    detail_task_ids: &[String],
    max_active_leaf_runs: Option<u32>,
    limit: usize,
    allowed_crews: &[String],
    depth: ReadinessDepth<'_>,
) -> Result<(Value, Option<ReadinessDetail>), OrbitError> {
    let snapshot = runtime.admission_policy_snapshot();
    let runtime = &snapshot;
    if !(1..=MAX_READINESS_LIMIT).contains(&limit) {
        return Err(OrbitError::InvalidInput(format!(
            "readiness limit must be between 1 and {MAX_READINESS_LIMIT}"
        )));
    }
    if max_active_leaf_runs == Some(0) {
        return Err(OrbitError::InvalidInput(
            "concurrency must be at least 1".to_string(),
        ));
    }
    // [ORB-11253] Without an explicit `--concurrency`, report what the live
    // drain is admitting under — including an operator adjustment — rather than
    // the static default, so readiness and the drain cannot disagree about the
    // ceiling that decides `capacity_saturated`. A replica often has no local
    // drain: its pull drain's own ceiling is that number [ORB-16337]. A local
    // drain still wins when both are live.
    let (active_drain, queued_drains) = workspace_drains(runtime)?;
    let pull_drain = live_pull_drain(runtime)?;
    let DrainProgress {
        recent_drain,
        status_state,
        ends_at,
        admitted_workers,
        running_admitted_workers,
        phase: drain_phase,
    } = drain_progress::drain_progress(runtime, active_drain.as_ref())?;
    let status_run_id = active_drain
        .as_ref()
        .map(|drain| drain.run_id.as_str())
        .or_else(|| recent_drain.as_ref().map(|run| run.run_id.as_str()));
    let (max_active_leaf_runs, limit_source) = match max_active_leaf_runs {
        Some(requested) => (u64::from(requested), "requested"),
        None => drain_ceiling(active_drain.as_ref(), pull_drain.as_ref()),
    };

    let pull_state = pull_drain
        .as_ref()
        .map(|drain| runtime.read_run_state(&drain.run_id))
        .transpose()?
        .flatten();

    // Validated here, before any snapshot work, so a typo reads the same way
    // it would on `orbit run auto --allow-crew`.
    let pool_input = active_drain
        .as_ref()
        .map_or_else(|| json!({}), |drain| json!({"run_id": drain.run_id}));
    let pools = runtime.auto_crew_pools_for_input(&pool_input)?;
    // Without an explicit `--allow-crew`, apply the live drain's own crew
    // restriction, so a task it excludes as `crew_not_allowed` never reads as
    // eligible here. A replica's live pull drain applies when no local drain is
    // live [ORB-16439].
    let allowlist = if allowed_crews.is_empty() {
        match active_drain
            .as_ref()
            .map(|drain| &drain.input)
            .or_else(|| pull_drain.as_ref().map(|drain| &drain.input))
        {
            Some(input) => runtime.crew_allowlist_from_input(input)?,
            None => None,
        }
    } else {
        runtime.crew_allowlist(allowed_crews)?
    };
    // The live local drain's captured review, so a local-route task its
    // `--review` holds reads as held here too.
    let review = active_drain
        .as_ref()
        .map(|drain| orbit_types::workflow::ReviewAdmission::from_run_input(&drain.input))
        .transpose()?
        .flatten();
    // Readiness admits nothing, so its envelope selections are observed
    // without the commit boundary: they neither queue behind a drain's
    // exclusive commit sections nor hold the shared boundary against them.
    // Backlog row hydration takes short shared sections in batches. Ancestor
    // and explicit-id loads (`include_tasks` below) also settle pending commits
    // before reading.
    let mut snapshot = backlog_snapshot_observed(
        runtime,
        "explain_workspace_auto_readiness",
        allowlist.as_ref(),
        &pools,
        review.as_ref(),
        status_state
            .as_ref()
            .filter(|_| active_drain.is_some())
            .or(pull_state.as_ref())
            .map(|state| state.crew_env_unset.as_slice()),
        match depth {
            ReadinessDepth::Full => None,
            ReadinessDepth::FleetRow(facts) => facts.status_index.as_ref(),
        },
    )
    .map_err(|error| OrbitError::Execution(format!("read readiness snapshot: {error}")))?;
    let live_leaves = read_live_leaf_runs(runtime)?;
    let claimed_by_task =
        live_leaves
            .iter()
            .fold(BTreeMap::<String, Vec<String>>::new(), |mut claims, run| {
                for task_id in &run.task_ids {
                    claims
                        .entry(task_id.clone())
                        .or_default()
                        .push(run.run_id.clone());
                }
                claims
            });
    // A live leaf may carry a task that has since closed, and an explicit
    // selection may name one; the snapshot holds only open work otherwise.
    snapshot.include_tasks(runtime, claimed_by_task.keys().chain(task_ids))?;
    let admissions_stopped = active_drain
        .as_ref()
        .is_some_and(|drain| drain.admissions_stopped());
    let host_shutdown = runtime.scheduled_host_shutdown();
    let host_shutdown_holding = runtime.host_shutdown_admission_hold();
    // [ORB-13901] The live drain's own throttle when this process has not
    // sampled long enough to judge sustained pressure itself.
    let resource = runtime.admission_resource_throttle();
    let shared_occupancy = shared_leaf_occupancy(runtime)?;
    let unthrottled_slots = if admissions_stopped || host_shutdown_holding.is_some() {
        0
    } else {
        usize::try_from(max_active_leaf_runs)
            .unwrap_or(usize::MAX)
            .saturating_sub(shared_occupancy.occupied)
    };
    // The same wave the pass builds. Holds are this active drain's load
    // failures; with no live drain there is nothing to hold, and a read
    // never reserves repair slots or starts a pull.
    let claimed = claimed_by_task.keys().cloned().collect::<BTreeSet<_>>();
    let mut leaf_load_holds = match active_drain.as_ref() {
        Some(drain) => leaf_load_failure_holds(
            runtime,
            "explain_workspace_auto_readiness",
            Some(drain.run_id.as_str()),
        )
        .map_err(|error: PassError| {
            OrbitError::Execution(format!("read readiness leaf-load holds: {error}"))
        })?,
        None => BTreeMap::new(),
    };
    retain_leaf_load_failure_holds(
        &mut leaf_load_holds,
        &snapshot.admissible_leaves,
        &snapshot.task_lookup,
    );
    let held: BTreeSet<String> = leaf_load_holds.keys().cloned().collect();
    let leaf_load_failed: BTreeMap<String, String> = leaf_load_holds
        .iter()
        .map(|(task_id, hold)| (task_id.clone(), hold.leaf_load_failure_detail()))
        .collect();
    let slots = unreserved_admission_slots(
        claimed.iter(),
        &snapshot.task_lookup,
        resource.throttle.as_ref(),
        unthrottled_slots,
    );
    let pool_limit = match active_drain.as_ref() {
        Some(drain) => candidate_pool_limit("explain_workspace_auto_readiness", &drain.input)
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?,
        None => usize::try_from(DEFAULT_CANDIDATE_POOL).unwrap_or(usize::MAX),
    };
    let workspace_root = runtime.paths().repo_root.as_path();
    let wave = AdmissionWave::build(AdmissionWaveInput {
        snapshot: &snapshot,
        claimed: &claimed,
        held: &held,
        slots,
        reserved_slots: 0,
        candidate_pool_limit: pool_limit,
        max_active_leaf_runs,
        occupied: shared_occupancy.occupied,
        workspace_root,
        unlocked: runtime.unlocked_context_paths(),
        lock_modes: runtime.footprint_lock_modes(),
    });
    let selection = wave.select(None);
    let candidate_pool_truncated = wave.candidate_pool_truncated(selection.selected.len());
    let examined = &wave.examined;
    let free_slots = wave.free_slots;
    let gate = wave.gate;
    let light_budget = &wave.light_budget;
    let holders = &wave.holders;
    let admitted = selection.selected.iter().cloned().collect::<BTreeSet<_>>();
    // [ORB-15514] A held footprint a follower's claim holds names that claim.
    // Claims only annotate a wait with its holder; they never change its reason.
    let claim_holders = match depth {
        ReadinessDepth::Full => ClaimHolders::read(runtime),
        ReadinessDepth::FleetRow(_) => ClaimHolders::default(),
    };
    let occupancy = read_leaf_occupancy(
        runtime,
        &live_leaves
            .iter()
            .map(|run| (run.run_id.clone(), run.task_ids.clone()))
            .collect::<Vec<_>>(),
        &snapshot.lock_holders,
    )?;
    // Project the listing and, later, `detail_task_ids` from this one wave.
    // Admission does not depend on which ids the response explains: the wave
    // was selected from `admissible_leaves`, which `include_tasks` does not
    // change. Rebuilding it per id was the `also_task` request's second snapshot.
    // The follower census and the recorded forecast views are the same for both
    // projections; reading them again made `also_task` about twice the plain read.
    let host_os_limits = runtime.executor_host_os_limits().unwrap_or_default();
    let follower_evidence = hosts::FollowerEvidence::read(runtime);
    let project_tasks = |snapshot: &BacklogSnapshot,
                         ids: &[String],
                         build_views: bool|
     -> Result<ProjectedReadinessTasks, OrbitError> {
        let selected_ids = readiness_selected_ids(snapshot, ids, limit)?;
        let mut tasks = projection::project_readiness_tasks(
            runtime,
            projection::ReadinessProjection {
                snapshot,
                selected_ids: &selected_ids,
                pools: &pools,
                allowlist: &allowlist,
                holders,
                claim_holders: &claim_holders,
                claimed_by_task: &claimed_by_task,
                host_shutdown_holding: &host_shutdown_holding,
                resource: &resource,
                gate,
                admissions_stopped,
                admitted: &admitted,
                selection: &selection,
                light_budget,
                examined,
                leaf_load_failed: &leaf_load_failed,
                live_leaves: &live_leaves,
                host_os_limits: &host_os_limits,
                follower_evidence: &follower_evidence,
            },
        );
        let (forecast_view, last_forecast_view) = if build_views {
            projection::project_readiness_forecasts(
                runtime,
                snapshot,
                &status_state,
                status_run_id,
                active_drain.is_some(),
                drain_phase,
                &mut tasks,
            )
        } else {
            // Detail keeps the per-task annotation and skips the forecast views
            // the bounded payload already owns.
            projection::annotate_readiness_task_forecasts(&status_state, drain_phase, &mut tasks);
            (None, None)
        };
        // `total` describes the current view: the full backlog for the default
        // bounded listing, or the number of explicitly selected tasks.
        let total = if ids.is_empty() {
            snapshot
                .task_lookup
                .values()
                .filter(|task| task.status == TaskStatus::Backlog)
                .count()
        } else {
            selected_ids.len()
        };
        Ok(ProjectedReadinessTasks {
            tasks,
            total,
            forecast_view,
            last_forecast_view,
        })
    };
    let ProjectedReadinessTasks {
        tasks,
        total,
        forecast_view,
        last_forecast_view,
    } = project_tasks(&snapshot, task_ids, true)?;

    // [ORB-14117] The status drain's proposed-task approvals, when it was
    // started with `--approve-proposed`.
    let approvals = match depth {
        ReadinessDepth::Full => readiness_approvals(
            runtime,
            status_run_id,
            active_drain
                .as_ref()
                .map(|drain| &drain.input)
                .or_else(|| recent_drain.as_ref().and_then(|run| run.input.as_ref())),
            &snapshot.task_lookup,
        )?,
        ReadinessDepth::FleetRow(_) => Value::Null,
    };

    // [ORB-14880] Build budget inspection is advisory: an invalid setting or
    // unreadable slots file must not fail readiness.
    let (build_budget_warnings, build_budget_error) = match runtime.build_budget_capacity_warnings()
    {
        Ok(warnings) => (warnings, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };

    // [ORB-14698] Every live provider usage reading, with whether it keeps
    // its crews out of admission and until when.
    let now = Utc::now();
    let provider_limits = runtime.provider_limits_view(now);
    if let Some(error) = &provider_limits.error {
        tracing::warn!("readiness shows no provider limits: {error}");
    }
    // Recent authentication failures, grouped by host and provider, dropped
    // once a successful run of that provider on that host that started after
    // the failure is on record.
    let provider_auth_failures = runtime.provider_auth_failures(now);

    let mut payload = json!({
        "crew_pool_warnings": status_state.as_ref().filter(|_| active_drain.is_some() || pull_drain.is_none()).or(pull_state.as_ref()).map(|state| state.pull_drain.pool_crew_exclusion_warnings()).unwrap_or_default(),
        "auto_concurrency": runtime.auto_concurrency_view(
            active_drain.as_ref().map(|drain| &drain.input)
                .or_else(|| recent_drain.as_ref().and_then(|run| run.input.as_ref()))
        )?,
        "snapshot": {
            "read_only": true,
            "limitations": "Snapshot only: eligibility can change immediately and does not guarantee a task will start. No stale-run reconciliation, reservation, task mutation, or run submission was performed.",
        },
        "capacity": {
            "build_budget_warnings": build_budget_warnings,
            "build_budget_error": build_budget_error,
            "max_active_leaf_runs": max_active_leaf_runs,
            "default_max_active_leaf_runs": runtime.job_default_input(DRAIN_JOB_NAME)?.as_ref()
                .and_then(|input| input.get("max_active_leaf_runs")).and_then(Value::as_u64).unwrap_or(5),
            "last_drain_max_active_leaf_runs": active_drain.as_ref().map(|drain| u64::from(drain.effective_max_active_leaf_runs()))
                .or_else(|| recent_drain.as_ref().map(|run| {
                    let submitted = run.input.as_ref().and_then(|input| input.get("max_active_leaf_runs"))
                        .and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(5);
                    u64::from(status_state.as_ref().map_or(submitted, |state| state.effective_max_active_leaf_runs(submitted)))
                })),
            "active_leaf_runs": shared_occupancy.occupied,
            // [ORB-12617] The wrapper subset of that occupancy, and what the
            // rest of it is: a legacy drain reporting no free slots with no
            // wrapper running is saturated by claimed leaves, and the
            // breakdown is the only thing that says so.
            "wrapper_leaf_runs": live_leaves.len(),
            "leaf_occupancy_by_pipeline": shared_occupancy.per_pipeline,
            "free_slots": free_slots,
            // [ORB-11973] The same occupancy, broken down by what each slot is
            // doing. A drain with every slot parked in `task_gate_pipeline` is
            // indistinguishable from a busy one by the counts above alone.
            "occupancy": occupancy_json(&occupancy, free_slots),
            "deferred_conflicts": selection.deferred_json(),
            "candidate_pool_size": examined.len(),
            "candidate_pool_truncated": candidate_pool_truncated,
            "limit_source": limit_source,
            // The crews this snapshot admits under: the requested
            // `--allow-crew` set, else the live drain's; `null` when neither
            // restricts them.
            "allowed_crews": allowlist.as_ref().map(CrewAllowlist::names),
            "drain_run_id": active_drain.as_ref().map(|drain| &drain.run_id),
            "drain_status_run_id": if drain_phase == "idle" { None } else { status_run_id },
            "drain_phase": drain_phase,
            "ends_at": ends_at,
            "admitted_workers": admitted_workers,
            "running_admitted_workers": running_admitted_workers,
            "queued_drains": queued_drains,
            "worker_limit": active_drain.as_ref().and_then(|drain| drain.limit.clone()),
            "admissions_stopped": admissions_stopped,
            "admissions_stop": active_drain
                .as_ref()
                .and_then(|drain| drain.stop.clone()),
            // A replica's live pull drain is a second coordinator the same
            // stop control acts on; it is reported apart from `drain_run_id`
            // because it admits leaves the owner assigned. When no local drain
            // is live, `max_active_leaf_runs` above is this pull drain's own
            // ceiling, not this workspace's auto default [ORB-16337].
            "pull_drain_run_id": pull_drain.as_ref().map(|drain| &drain.run_id),
            "pull_drain_admissions_stopped": pull_drain
                .as_ref()
                .is_some_and(|drain| drain.stop.is_some()),
            "pull_drain_admissions_stop": pull_drain
                .as_ref()
                .and_then(|drain| drain.stop.clone()),
            // [ORB-15637] Why the pull drain's last pass requested no claim
            // although its window was open (`circuit_open`, ...). This host's
            // own `resource_throttle` is reported apart; the owner's never
            // holds a pull claim.
            "pull_drain_refusal": pull_drain
                .as_ref()
                .and_then(|drain| drain.refusal.clone()),
            // [ORB-12968] A pending host shutdown or reboot. `host_shutdown`
            // names it however far away it is. `host_shutdown_holding` is
            // true only inside the 180-minute hold, which is
            // when drains, the sweep and routines start no new work. A missing
            // flag on an older payload means holding.
            "host_shutdown": host_shutdown,
            "host_shutdown_holding": host_shutdown_holding.is_some(),
            // [ORB-13901] Sustained host resource pressure holding every new
            // admission, and readings that could not be used (which admit).
            "resource_cpu_source": runtime.context.settings().resource_throttle().cpu_source,
            "resource_throttle": resource.throttle,
            "resource_telemetry_unknown": resource.unknown,
            // [ORB-14624] Light slots a CPU-only throttle still admits into;
            // `applies` is true while it is spending them.
            "cpu_light_budget": light_budget.to_json(gate == ResourceGate::LightOnly),
            // The open local drain's latest pass found no task pilot enabled
            // on this machine, so its tasks that need an assessment wait for
            // one that never comes. Names the workspace, machine and fix.
            "task_pilot_warning": status_state
                .as_ref()
                .filter(|_| drain_phase == "draining")
                .and_then(|state| state.pull_drain.drain_last_pass.as_ref())
                .and_then(|pass| pass.task_pilot_warning.clone()),
        },
        "approvals": approvals,
        "provider_limits": provider_limits.readings,
        "provider_pressures": provider_limits.providers,
        "crew_pressures": provider_limits.crew_pressures,
        "provider_auth_failures": provider_auth_failures,
        "total": total,
        "tasks": tasks,
    });
    // The Auto-drain card visualizes coordinator task slots. Keep its scale
    // and fill separate from workspace leaf-run occupancy, where one admitted
    // task can hold several pipeline runs.
    payload["capacity"]["drain_concurrency"] = json!(
        active_drain
            .as_ref()
            .map(|drain| u64::from(drain.effective_max_active_leaf_runs()))
    );
    payload["capacity"]["drain_running_tasks"] =
        json!(active_drain.as_ref().map(|_| running_admitted_workers));
    // No block means the older/off response shape remains unchanged.
    if let Some(forecast) = forecast_view {
        payload["capacity"]["admission_forecast"] = forecast;
    }
    if let Some(forecast) = last_forecast_view {
        payload["capacity"]["last_admission_forecast"] = forecast;
    }
    // Keep the existing live-repair entries and add the latest completed
    // pass's refusals. This is recorded history, never a new forge scan.
    let mut repairs = match runtime.live_out_of_run_auto_fixes() {
        Ok(repairs) => repairs,
        Err(error) => {
            tracing::warn!("readiness shows no live out-of-run auto-fix: {error}");
            Vec::new()
        }
    };
    if let Some(state) = &status_state
        && let Some(pass) = state.pipeline.get("adopt_orphans")
    {
        for skipped in pass["auto_fix"]["skipped"].as_array().into_iter().flatten() {
            repairs.push(json!({
                "state": "skipped",
                "task_ids": [skipped["task_id"]],
                "pr_number": skipped["pr_number"],
                "blocker": skipped["blocker"],
                "hold_code": skipped.get("hold_code").cloned().unwrap_or(Value::Null),
                "reason": skipped["reason"],
                "detail": skipped["detail"],
                "source_run_id": skipped["source_run_id"],
                "drain_run_id": status_run_id,
                "drain_live": active_drain.is_some(),
                "observed_at": pass["observed_at"],
            }));
        }
    }
    if !repairs.is_empty() {
        payload["out_of_run_auto_fix"] = json!(repairs);
    }
    // After the primary payload owns its forecast titles. Loading a closed
    // detail task into the lookup before this point would add its title.
    let detail = if let Some(task_id) = detail_task_ids.first() {
        snapshot.include_tasks(runtime, detail_task_ids)?;
        match project_tasks(&snapshot, detail_task_ids, false) {
            Ok(detail) => Some(ReadinessDetail::Tasks(detail.tasks)),
            Err(OrbitError::InvalidInput(_)) => Some(ReadinessDetail::Unavailable {
                task_id: task_id.clone(),
            }),
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    // The drain card's three live-agent figures. Omitted when the runs cannot
    // be read, so an older client and a failed observation both leave the row
    // off. Fleet rows do not report it.
    if matches!(depth, ReadinessDepth::Full)
        && let Some(counts) = live_agents::observe_live_agents(runtime)
    {
        payload["live_agents"] = counts;
    }
    Ok((payload, detail))
}
