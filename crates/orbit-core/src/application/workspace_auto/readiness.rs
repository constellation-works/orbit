use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRun, PipelineState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::admission::auto_admission::{AdmissionHolders, select_admissions};
use crate::application::admission::backlog_exclusion::{
    backlog_snapshot_observed, sort_tasks_for_automatic_dispatch,
};
use crate::application::admission::claim_holders::ClaimHolders;
use crate::application::admission::cpu_light::{LightBudget, ResourceGate};
use crate::application::admission::leaf_occupancy::{occupancy_json, read_leaf_occupancy};
use crate::application::admission::unknown_footprint::{FootprintGuard, whole_tree_holder};
use crate::application::job::drain_deadline;
use crate::runtime::engine::crew::CrewAllowlist;

use super::approvals::readiness_approvals;
use super::classify::{DEFAULT_CANDIDATE_POOL, candidate_pool_limit};
use super::drains::{
    ActiveDrain, DRAIN_JOB_NAME, LEAF_JOB_NAME, drain_ceiling, live_pull_drain,
    read_live_leaf_runs, shared_leaf_occupancy, workspace_drains,
};

const MAX_READINESS_LIMIT: usize = 500;

/// Host-wide facts one fleet read fetches once and hands to every workspace's
/// row. A fact that could not be read is absent, and each row reads its own and
/// reports its own failure.
#[derive(Default)]
pub struct FleetReadinessFacts {
    status_index: Option<BTreeMap<String, TaskStatus>>,
}

/// How much of the readiness snapshot its reader consumes.
#[derive(Clone, Copy)]
enum ReadinessDepth<'a> {
    Full,
    /// A fleet row: admission facts only, without the claim, approval and
    /// validation diagnostics a row does not report.
    FleetRow(&'a FleetReadinessFacts),
}

/// Explain the same snapshot that auto-drain uses without performing its
/// stale-run reconciliation. This is deliberately an observation API: it
/// neither reserves work nor creates a pipeline run, and its answer can go
/// stale immediately after the stores are read.
pub fn explain_workspace_auto_readiness(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    max_active_leaf_runs: Option<u32>,
    limit: usize,
    allowed_crews: &[String],
) -> Result<Value, OrbitError> {
    readiness_snapshot(
        runtime,
        task_ids,
        max_active_leaf_runs,
        limit,
        allowed_crews,
        ReadinessDepth::Full,
    )
}

fn readiness_snapshot(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    max_active_leaf_runs: Option<u32>,
    limit: usize,
    allowed_crews: &[String],
    depth: ReadinessDepth<'_>,
) -> Result<Value, OrbitError> {
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
    } = drain_progress(runtime, active_drain.as_ref())?;
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
    // Ancestor and explicit-id loads (`include_tasks` below) still settle a
    // pending commit before they read.
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
    // [ORB-14624] The classifier's own gate: under a CPU-only throttle,
    // CPU-light leaves fill the reserved light slots and nothing else starts.
    let light_budget = LightBudget::new(
        crate::application::admission::cpu_light::CPU_LIGHT_LEAVES,
        claimed_by_task.keys(),
        &snapshot.task_lookup,
    );
    let gate = ResourceGate::new(resource.throttle.as_ref(), &light_budget);
    let free_slots = gate.free_slots(unthrottled_slots, &light_budget);
    let pending = snapshot
        .admissible_leaves
        .iter()
        .filter(|task_id| !claimed_by_task.contains_key(*task_id))
        .filter(|task_id| {
            gate == ResourceGate::Open
                || snapshot
                    .task_lookup
                    .get(*task_id)
                    .is_some_and(|task| gate.admits(task))
        })
        .cloned()
        .collect::<Vec<_>>();
    // [ORB-11973] Use the classifier's identical ordered prefix and admission
    // routine, so readiness explains the wave the drain would actually admit
    // rather than a second guess at it.
    let candidate_pool_size = match active_drain.as_ref() {
        Some(drain) => candidate_pool_limit("explain_workspace_auto_readiness", &drain.input)
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?,
        None => usize::try_from(DEFAULT_CANDIDATE_POOL).unwrap_or(usize::MAX),
    };
    let examined = &pending[..pending.len().min(candidate_pool_size)];
    let workspace_root = runtime.paths().repo_root.as_path();
    let claimed = claimed_by_task.keys().cloned().collect::<BTreeSet<_>>();
    let holders = AdmissionHolders::new(
        &snapshot.lock_holders,
        &claimed,
        &snapshot.task_lookup,
        workspace_root,
        runtime.unlocked_context_paths(),
        runtime.footprint_lock_modes(),
    );
    let footprint = FootprintGuard {
        enabled: max_active_leaf_runs > 1,
        leaves_in_flight: shared_occupancy.occupied > 0,
        whole_tree_holder: whole_tree_holder(&claimed, &snapshot.task_lookup),
        waits: &snapshot.footprint_waits,
    };
    let selection = select_admissions(
        examined,
        &snapshot.task_lookup,
        workspace_root,
        runtime.unlocked_context_paths(),
        &holders,
        &snapshot.published_resumes,
        free_slots,
        footprint,
        None,
    );
    let candidate_pool_truncated =
        pending.len() > examined.len() && selection.selected.len() < free_slots;
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
    let selected_ids = if task_ids.is_empty() {
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
        backlog
            .into_iter()
            .take(limit)
            .map(|task| task.id.clone())
            .collect()
    } else {
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
        ids
    };

    let mut tasks = super::projection::project_readiness_tasks(
        runtime,
        super::projection::ReadinessProjection {
            snapshot: &snapshot,
            selected_ids: &selected_ids,
            pools: &pools,
            allowlist: &allowlist,
            holders: &holders,
            claim_holders: &claim_holders,
            claimed_by_task: &claimed_by_task,
            host_shutdown_holding: &host_shutdown_holding,
            resource: &resource,
            gate,
            admissions_stopped,
            admitted: &admitted,
            selection: &selection,
            light_budget: &light_budget,
            examined,
            live_leaves: &live_leaves,
        },
    );

    let (forecast_view, last_forecast_view) = super::projection::project_readiness_forecasts(
        runtime,
        &snapshot,
        &status_state,
        status_run_id,
        active_drain.is_some(),
        drain_phase,
        &mut tasks,
    );

    // `total` describes the current view: the full backlog for the default
    // bounded listing, or the number of explicitly selected tasks.
    let total = if task_ids.is_empty() {
        snapshot
            .task_lookup
            .values()
            .filter(|task| task.status == TaskStatus::Backlog)
            .count()
    } else {
        selected_ids.len()
    };

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
    Ok(payload)
}

/// The status drain's progress: the most recent finished drain when no local
/// drain is live, the status drain's pipeline state, its window deadline and
/// workers, and the phase readiness reports. Readiness and the dashboard's
/// change stream read the phase from here, so they never disagree.
pub(super) struct DrainProgress {
    pub(super) recent_drain: Option<JobRun>,
    pub(super) status_state: Option<PipelineState>,
    pub(super) ends_at: Option<String>,
    pub(super) admitted_workers: usize,
    pub(super) running_admitted_workers: usize,
    pub(super) phase: &'static str,
}

pub(super) fn drain_progress(
    runtime: &OrbitRuntime,
    active_drain: Option<&ActiveDrain>,
) -> Result<DrainProgress, OrbitError> {
    let recent_drain = if active_drain.is_none() {
        runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(DRAIN_JOB_NAME.to_string()),
                terminal_only: true,
                limit: Some(1),
                include_steps: false,
                ..JobRunQuery::default()
            })?
            .into_iter()
            .next()
    } else {
        None
    };
    let status_run_id = active_drain
        .map(|drain| drain.run_id.as_str())
        .or_else(|| recent_drain.as_ref().map(|run| run.run_id.as_str()));
    let status_state = status_run_id
        .map(|run_id| runtime.stores().jobs().read_run_state(run_id))
        .transpose()?
        .flatten();
    // The open-window checkpoint is the actual server-stamped deadline, not
    // the submission time plus a browser's chosen duration, unless an
    // operator has since extended it [ORB-15383].
    let ends_at = status_state
        .as_ref()
        .and_then(drain_deadline)
        .map(|deadline| deadline.to_rfc3339_opts(SecondsFormat::Secs, true));
    let (admitted_workers, running_admitted_workers) = if let Some(state) = &status_state {
        let mut running = 0;
        let workers = state
            .child_dispatches
            .iter()
            .filter(|dispatch| dispatch.job_name == LEAF_JOB_NAME);
        let mut admitted = 0;
        for dispatch in workers {
            admitted += 1;
            if runtime
                .stores()
                .jobs()
                .get_job_run(&dispatch.child_run_id)?
                .is_some_and(|child| !child.state.is_terminal())
            {
                running += 1;
            }
        }
        (admitted, running)
    } else {
        (0, 0)
    };
    let window_open = ends_at
        .as_deref()
        .and_then(|deadline| DateTime::parse_from_rfc3339(deadline).ok())
        .is_none_or(|deadline| deadline > Utc::now());
    let phase = if active_drain.is_some_and(|drain| !drain.admissions_stopped()) && window_open {
        "draining"
    } else if running_admitted_workers > 0 {
        "winding_down"
    } else {
        "idle"
    };
    Ok(DrainProgress {
        recent_drain,
        status_state,
        ends_at,
        admitted_workers,
        running_admitted_workers,
        phase,
    })
}

impl OrbitRuntime {
    /// Read-only projection of the current auto-drain admission snapshot.
    pub fn workspace_auto_readiness(
        &self,
        task_ids: &[String],
        max_active_leaf_runs: Option<u32>,
        limit: usize,
        allowed_crews: &[String],
    ) -> Result<Value, OrbitError> {
        let mut snapshot = explain_workspace_auto_readiness(
            self,
            task_ids,
            max_active_leaf_runs,
            limit,
            allowed_crews,
        )?;
        snapshot["replica"] = json!(self.replica_owner_machine().is_some());
        snapshot["validation_env_warning"] = json!(self.readiness_validation_env_warning());
        Ok(snapshot)
    }

    /// The registry-global facts one fleet read shares across its workspaces.
    #[must_use]
    pub fn fleet_readiness_facts(&self) -> FleetReadinessFacts {
        // A replica's own reads hide task statuses, and that says nothing
        // about the other workspaces this fact would serve.
        let status_index = self
            .coordination_task_reads_visible()
            .then(|| self.task_status_index().ok())
            .flatten();
        FleetReadinessFacts { status_index }
    }

    /// The readiness snapshot a fleet row summarizes: the same admission
    /// decisions and holds as [`Self::workspace_auto_readiness`] over the
    /// default listing, without the diagnostics a row does not report
    /// (`approvals` is null; `blocking_claims`, `replica` and
    /// `validation_env_warning` are absent). Read-only like that snapshot.
    pub fn workspace_fleet_readiness(
        &self,
        facts: &FleetReadinessFacts,
    ) -> Result<Value, OrbitError> {
        readiness_snapshot(
            self,
            &[],
            None,
            MAX_READINESS_LIMIT,
            &[],
            ReadinessDepth::FleetRow(facts),
        )
    }

    /// The workspace's drain phase as readiness reports it (`draining`,
    /// `winding_down` or `idle`), without the rest of the readiness snapshot.
    pub fn workspace_drain_phase(&self) -> Result<&'static str, OrbitError> {
        let (active_drain, _) = workspace_drains(self)?;
        Ok(drain_progress(self, active_drain.as_ref())?.phase)
    }
}
