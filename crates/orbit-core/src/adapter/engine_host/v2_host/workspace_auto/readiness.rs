use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{TaskStatus, unmet_task_dependencies_with_index};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::admission::auto_admission::{
    AdmissionHolders, DeferralReason, select_admissions,
};
use crate::adapter::engine_host::v2_host::admission::backlog_exclusion::{
    BacklogTaskExclusionReason, backlog_snapshot, sort_tasks_for_automatic_dispatch,
};
use crate::adapter::engine_host::v2_host::admission::cpu_light::{
    CPU_LIGHT_BUDGET_FULL, LightBudget, ResourceGate, is_cpu_light,
};
use crate::adapter::engine_host::v2_host::admission::leaf_occupancy::{
    occupancy_json, read_leaf_occupancy,
};
use crate::adapter::engine_host::v2_host::admission::unknown_footprint::{
    FootprintGuard, whole_tree_holder,
};
use crate::application::distributed::RESOURCE_THROTTLED;
use crate::runtime::engine::crew::CrewAllowlist;
use crate::runtime::host_signal::HOST_SHUTDOWN_SCHEDULED;

use super::approvals::readiness_approvals;
use super::classify::{DEFAULT_CANDIDATE_POOL, candidate_pool_limit};
use super::drains::{
    DEFAULT_MAX_ACTIVE_LEAF_RUNS, DRAIN_JOB_NAME, LEAF_JOB_NAME, live_pull_drain,
    read_live_leaf_runs, shared_leaf_occupancy, workspace_drains,
};

const MAX_READINESS_LIMIT: usize = 500;

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
    // ceiling that decides `capacity_saturated`.
    let (active_drain, queued_drains) = workspace_drains(runtime)?;
    let pull_drain = live_pull_drain(runtime)?;
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
        .as_ref()
        .map(|drain| drain.run_id.as_str())
        .or_else(|| recent_drain.as_ref().map(|run| run.run_id.as_str()));
    let status_state = status_run_id
        .map(|run_id| runtime.stores().jobs().read_run_state(run_id))
        .transpose()?
        .flatten();
    // The open-window checkpoint is the actual server-stamped deadline, not
    // the submission time plus a browser's chosen duration.
    let ends_at = status_state.as_ref().and_then(|state| {
        state
            .step_output_entries()
            .find_map(|(_, output)| output.get("deadline").and_then(Value::as_str))
    });
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
        .and_then(|deadline| DateTime::parse_from_rfc3339(deadline).ok())
        .is_none_or(|deadline| deadline > Utc::now());
    let drain_phase = if active_drain
        .as_ref()
        .is_some_and(|drain| !drain.admissions_stopped())
        && window_open
    {
        "draining"
    } else if running_admitted_workers > 0 {
        "winding_down"
    } else {
        "idle"
    };
    let (max_active_leaf_runs, limit_source) = match (max_active_leaf_runs, &active_drain) {
        (Some(requested), _) => (u64::from(requested), "requested"),
        (None, Some(drain)) => (
            u64::from(drain.effective_max_active_leaf_runs()),
            if drain.limit.is_some() {
                "run_control"
            } else {
                "run_input"
            },
        ),
        (None, None) => (DEFAULT_MAX_ACTIVE_LEAF_RUNS, "default"),
    };

    // Validated here, before any snapshot work, so a typo reads the same way
    // it would on `orbit run auto --allow-crew`.
    let pool_input = active_drain
        .as_ref()
        .map_or_else(|| json!({}), |drain| json!({"run_id": drain.run_id}));
    let pools = runtime.auto_crew_pools_for_input(&pool_input)?;
    let allowlist = runtime.crew_allowlist(allowed_crews)?;
    let snapshot = backlog_snapshot(
        runtime,
        "explain_workspace_auto_readiness",
        allowlist.as_ref(),
        &pools,
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
    let admissions_stopped = active_drain
        .as_ref()
        .is_some_and(|drain| drain.admissions_stopped());
    let host_shutdown = runtime.scheduled_host_shutdown();
    // [ORB-13901] The live drain's own throttle when this process has not
    // sampled long enough to judge sustained pressure itself.
    let resource = runtime.admission_resource_throttle();
    let shared_occupancy = shared_leaf_occupancy(runtime)?;
    let unthrottled_slots = if admissions_stopped || host_shutdown.is_some() {
        0
    } else {
        usize::try_from(max_active_leaf_runs)
            .unwrap_or(usize::MAX)
            .saturating_sub(shared_occupancy.occupied)
    };
    // [ORB-14624] The classifier's own gate: under a CPU-only throttle,
    // CPU-light leaves fill the reserved light slots and nothing else starts.
    let light_budget = LightBudget::new(
        runtime
            .context
            .settings()
            .resource_throttle()
            .cpu_light_leaves,
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
        &holders,
        free_slots,
        footprint,
    );
    let candidate_pool_truncated =
        pending.len() > examined.len() && selection.selected.len() < free_slots;
    let admitted = selection.selected.iter().cloned().collect::<BTreeSet<_>>();
    let occupancy = read_leaf_occupancy(
        runtime,
        &live_leaves
            .iter()
            .map(|run| (run.run_id.clone(), run.task_ids.clone()))
            .collect::<Vec<_>>(),
        &snapshot.lock_holders,
    )?;
    let excluded_by_id = snapshot
        .excluded
        .iter()
        .map(|excluded| (excluded.id.as_str(), excluded))
        .collect::<BTreeMap<_, _>>();
    let selected_ids = if task_ids.is_empty() {
        let mut backlog = snapshot
            .task_lookup
            .values()
            .filter(|task| task.status == TaskStatus::Backlog)
            .collect::<Vec<_>>();
        sort_tasks_for_automatic_dispatch(&mut backlog, &snapshot.expiring_batches);
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

    let tasks = selected_ids
        .iter()
        .filter_map(|id| snapshot.task_lookup.get(id))
        .map(|task| {
            let mut entry = json!({
                "task_id": task.id,
                "status": task.status.to_string(),
                "eligible": false,
                "reason": "not_backlog",
            });
            let Some(object) = entry.as_object_mut() else {
                unreachable!("readiness task entry is an object");
            };
            if task.status != TaskStatus::Backlog {
                return Value::Object(object.clone());
            }
            // [ORB-14624] Which tasks a CPU-only throttle still admits, and
            // which sort ahead because their frozen batch nears its deadline.
            let cpu_light = is_cpu_light(task);
            if cpu_light {
                object.insert("cpu_light".to_string(), Value::Bool(true));
            }
            if let Some(deadline) = snapshot.expiring_batches.get(&task.id) {
                object.insert("frozen_batch_deadline".to_string(), json!(deadline));
            }
            let unmet = unmet_task_dependencies_with_index(
                task,
                &snapshot.status_by_id,
                &snapshot.reference_index,
            );
            if !unmet.is_empty() {
                object.insert("reason".to_string(), Value::String("unmet_dependency".to_string()));
                object.insert(
                    "dependencies".to_string(),
                    json!(unmet
                        .into_iter()
                        .map(|dependency| json!({ "task_id": dependency.id, "status": dependency.status }))
                        .collect::<Vec<_>>()),
                );
                return Value::Object(object.clone());
            }
            if let Some(excluded) = excluded_by_id.get(task.id.as_str()) {
                match excluded.reason {
                    BacklogTaskExclusionReason::ActivePilotPreparation => {
                        object.insert("reason".to_string(), json!("active_pilot_preparation"));
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::PilotDuplicate
                    | BacklogTaskExclusionReason::PilotAlreadyLanded
                    | BacklogTaskExclusionReason::OperatorValidationHandoff
                    | BacklogTaskExclusionReason::HostOperationalHandoff
                    | BacklogTaskExclusionReason::NativeOsRequired
                    | BacklogTaskExclusionReason::PrForgeRemoteMissing
                    | BacklogTaskExclusionReason::AwaitingFootprint
                    | BacklogTaskExclusionReason::AwaitingExclusiveSlot => {
                        object.insert("reason".to_string(), json!(excluded.reason));
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::UnassessedComplexity => {
                        object.insert(
                            "reason".to_string(),
                            Value::String("task_pilot_preparation_required".to_string()),
                        );
                    }
                    BacklogTaskExclusionReason::DeliveryJobUnavailable => {
                        object.insert(
                            "reason".to_string(),
                            Value::String("delivery_job_unavailable".to_string()),
                        );
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::InheritedOnlyEpicRoot => {
                        object.insert(
                            "reason".to_string(),
                            Value::String("inherited_only_epic_root".to_string()),
                        );
                        // The one exclusion no later drain clears by itself, so
                        // readiness repeats the repair the drain recorded rather
                        // than leaving the reason to be interpreted.
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::HostOsMismatch => {
                        // Retagging, or a host of the named OS, clears it; the
                        // detail names which.
                        object.insert("reason".to_string(), Value::String("host_os_mismatch".to_string()));
                        object.insert("detail".to_string(), json!(excluded.detail));
                        object.insert("host_os".to_string(), json!(runtime.host_os()));
                    }
                    BacklogTaskExclusionReason::LocalRouteBeforePr => {
                        // [ORB-14168] The drain records the same snake_case
                        // reason. The detail names the deciding layer and the
                        // remedy the admission refusal already uses.
                        object.insert(
                            "reason".to_string(),
                            Value::String("local_route_before_pr".to_string()),
                        );
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::LocalRouteBeforeLanding => {
                        // [ORB-14849] As above, under its own code.
                        object.insert(
                            "reason".to_string(),
                            Value::String("local_route_before_landing".to_string()),
                        );
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::BaselineRedHold => {
                        // [ORB-14258] Lifts by itself once the command passes
                        // on a new base tip; the detail names the base and
                        // the command.
                        object.insert(
                            "reason".to_string(),
                            Value::String("baseline_red_hold".to_string()),
                        );
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::ProviderBackoff => {
                        // [ORB-14266] Lifts by itself at the hold's
                        // `not_before`; the detail names the run, the
                        // failure, the excluded crews and the time.
                        object.insert(
                            "reason".to_string(),
                            Value::String("provider_backoff".to_string()),
                        );
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::ProviderLimit => {
                        // [ORB-14697] Lifts by itself at the reading's reset;
                        // the detail names the provider, window, used
                        // percent, threshold, reset and skipped crews.
                        object.insert(
                            "reason".to_string(),
                            Value::String("provider_limit".to_string()),
                        );
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::CrewNotAllowed => {
                        object.insert("reason".to_string(), Value::String("crew_not_allowed".to_string()));
                        object.insert("crew".to_string(), json!(excluded.crew));
                        object.insert("allowed_crews".to_string(), json!(allowlist.as_ref().map(CrewAllowlist::names)));
                    }
                    BacklogTaskExclusionReason::SurfaceReserved => {
                        // [ORB-14310] Clears by itself once the reserving task
                        // is admitted or leaves backlog; `blocking_task_ids`
                        // names it so the text view prints `blocked-by`.
                        let reserved_for = excluded
                            .conflicts
                            .iter()
                            .map(|conflict| conflict.locking_task_id.as_str())
                            .collect::<BTreeSet<_>>();
                        object.insert("reason".to_string(), json!(excluded.reason));
                        object.insert("blocking_task_ids".to_string(), json!(reserved_for));
                        object.insert(
                            "conflicts".to_string(),
                            json!(excluded.conflicts.iter().map(|conflict| json!({
                                "requested_file": conflict.requested_file,
                                "reserved_by_task_id": conflict.locking_task_id,
                            })).collect::<Vec<_>>()),
                        );
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::ContextLockConflict
                    | BacklogTaskExclusionReason::GroupMemberConflict => {
                        object.insert(
                            "reason".to_string(),
                            Value::String(match excluded.reason {
                                BacklogTaskExclusionReason::ContextLockConflict => "context_lock_conflict",
                                BacklogTaskExclusionReason::GroupMemberConflict => "group_member_conflict",
                                _ => unreachable!("lock exclusions are handled above"),
                            }.to_string()),
                        );
                        object.insert(
                            "conflicts".to_string(),
                            json!(excluded.conflicts.iter().map(|conflict| json!({
                                "requested_file": conflict.requested_file,
                                "locking_task_id": conflict.locking_task_id,
                            })).collect::<Vec<_>>()),
                        );
                        // [ORB-14310] Present when this task reserves its
                        // surface against lower-ranked overlapping work.
                        if let Some(detail) = &excluded.detail {
                            object.insert("detail".to_string(), json!(detail));
                        }
                    }
                }
                return Value::Object(object.clone());
            }
            if let Some(run_ids) = claimed_by_task.get(&task.id) {
                object.insert("reason".to_string(), Value::String("claimed_by_live_child".to_string()));
                object.insert("run_ids".to_string(), json!(run_ids));
            } else if let Some(shutdown) = host_shutdown.as_ref() {
                object.insert(
                    "reason".to_string(),
                    Value::String(HOST_SHUTDOWN_SCHEDULED.to_string()),
                );
                object.insert("detail".to_string(), json!(shutdown.describe()));
            } else if let Some(throttle) = resource
                .throttle
                .as_ref()
                .filter(|_| !(gate == ResourceGate::LightOnly && cpu_light))
            {
                object.insert(
                    "reason".to_string(),
                    Value::String(RESOURCE_THROTTLED.to_string()),
                );
                object.insert("detail".to_string(), json!(throttle.describe()));
            } else if admissions_stopped {
                object.insert(
                    "reason".to_string(),
                    Value::String("admissions_stopped".to_string()),
                );
            } else if admitted.contains(&task.id) {
                object.insert("eligible".to_string(), Value::Bool(true));
                object.insert("reason".to_string(), Value::String("ready".to_string()));
            } else if let Some(deferred) = selection
                .deferred_for(&task.id)
                .filter(|deferred| deferred.reason == DeferralReason::AwaitingFootprint)
            {
                // [ORB-15191] Reported ahead of capacity: the task waits for
                // its pilot whether or not a slot is free.
                object.insert("reason".to_string(), json!(deferred.reason.as_str()));
                object.insert("detail".to_string(), json!(deferred.detail));
            } else if gate == ResourceGate::LightOnly && light_budget.remaining() == 0 {
                object.insert(
                    "reason".to_string(),
                    Value::String(CPU_LIGHT_BUDGET_FULL.to_string()),
                );
                object.insert("detail".to_string(), json!(light_budget.full_detail()));
            } else if !examined.contains(&task.id) {
                object.insert(
                    "reason".to_string(),
                    Value::String("outside_candidate_pool".to_string()),
                );
            } else if let Some(deferred) = selection.deferred_for(&task.id) {
                // [ORB-11973] A slot was free and this task did not take it,
                // which is a different problem from having no slot at all.
                // [ORB-15191] A task with no footprint waiting to go alone
                // says so, with the fix.
                object.insert("reason".to_string(), json!(deferred.reason.as_str()));
                if let Some(detail) = &deferred.detail {
                    object.insert("detail".to_string(), json!(detail));
                }
                if deferred.reason == DeferralReason::Conflict {
                    object.insert("blocking_task_ids".to_string(), json!(deferred.blocking_task_ids()));
                    object.insert("conflicts".to_string(), deferred.to_json()["conflicts"].clone());
                }
            } else {
                object.insert("reason".to_string(), Value::String("capacity_saturated".to_string()));
                object.insert("active_run_ids".to_string(), json!(live_leaves.iter().map(|run| &run.run_id).collect::<Vec<_>>()));
            }
            Value::Object(object.clone())
        })
        .collect::<Vec<_>>();

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
    let approvals = readiness_approvals(
        runtime,
        status_run_id,
        active_drain
            .as_ref()
            .map(|drain| &drain.input)
            .or_else(|| recent_drain.as_ref().and_then(|run| run.input.as_ref())),
    )?;

    // [ORB-14880] Build budget inspection is advisory: an invalid setting or
    // unreadable slots file must not fail readiness.
    let (build_budget_warnings, build_budget_error) = match runtime.build_budget_capacity_warnings()
    {
        Ok(warnings) => (warnings, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };

    // [ORB-14698] Every live provider usage reading, with whether it keeps
    // its crews out of admission and until when.
    let provider_limits = runtime.provider_limits_view(Utc::now());
    if let Some(error) = &provider_limits.error {
        tracing::warn!("readiness shows no provider limits: {error}");
    }

    Ok(json!({
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
            // because it admits leaves the owner assigned, under the owner's
            // ceiling, and never reads this workspace's auto ceiling.
            "pull_drain_run_id": pull_drain.as_ref().map(|drain| &drain.run_id),
            "pull_drain_admissions_stopped": pull_drain
                .as_ref()
                .is_some_and(|drain| drain.stop.is_some()),
            "pull_drain_admissions_stop": pull_drain
                .as_ref()
                .and_then(|drain| drain.stop.clone()),
            // [ORB-12968] A pending host shutdown or reboot; while present no
            // drain, sweep, or routine starts new work.
            "host_shutdown": host_shutdown,
            // [ORB-13901] Sustained host resource pressure holding every new
            // admission, and readings that could not be used (which admit).
            "resource_throttle": resource.throttle,
            "resource_telemetry_unknown": resource.unknown,
            // [ORB-14624] Light slots a CPU-only throttle still admits into;
            // `applies` is true while it is spending them.
            "cpu_light_budget": light_budget.to_json(gate == ResourceGate::LightOnly),
        },
        "approvals": approvals,
        "provider_limits": provider_limits.readings,
        "total": total,
        "tasks": tasks,
    }))
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
        explain_workspace_auto_readiness(self, task_ids, max_active_leaf_runs, limit, allowed_crews)
    }
}
