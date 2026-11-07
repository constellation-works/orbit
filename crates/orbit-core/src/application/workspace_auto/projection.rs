//! Task eligibility and recorded admission-forecast projections.

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::{TaskStatus, unmet_task_dependencies_with_index};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::admission::auto_admission::{AdmissionHolders, DeferralReason};
use crate::application::admission::backlog_exclusion::BacklogTaskExclusionReason;
use crate::application::admission::claim_holders::ClaimHolders;
use crate::application::admission::cpu_light::{
    CPU_LIGHT_BUDGET_FULL, LightBudget, ResourceGate, is_cpu_light,
};
use crate::application::distributed::RESOURCE_THROTTLED;
use crate::runtime::engine::crew::CrewAllowlist;
use crate::runtime::host_signal::HOST_SHUTDOWN_SCHEDULED;

use super::readiness_hosts::{self, FollowerEvidence};

use super::drains::LiveLeafRun;
use crate::application::admission::auto_admission::AdmissionSelection;
use crate::application::admission::backlog_exclusion::BacklogSnapshot;
use crate::application::job::crew_pools::CapturedCrewPools;
use crate::runtime::host_resource::ResourceAdmission;
use crate::runtime::host_signal::ScheduledShutdown;

pub(super) struct ReadinessProjection<'a> {
    pub(super) snapshot: &'a BacklogSnapshot,
    pub(super) selected_ids: &'a [String],
    pub(super) pools: &'a CapturedCrewPools,
    pub(super) allowlist: &'a Option<CrewAllowlist>,
    pub(super) holders: &'a AdmissionHolders,
    pub(super) claim_holders: &'a ClaimHolders,
    pub(super) claimed_by_task: &'a BTreeMap<String, Vec<String>>,
    pub(super) host_shutdown_holding: &'a Option<ScheduledShutdown>,
    pub(super) resource: &'a ResourceAdmission,
    pub(super) gate: ResourceGate,
    pub(super) admissions_stopped: bool,
    pub(super) admitted: &'a BTreeSet<String>,
    pub(super) selection: &'a AdmissionSelection,
    pub(super) light_budget: &'a LightBudget,
    pub(super) examined: &'a [String],
    pub(super) live_leaves: &'a [LiveLeafRun],
}

pub(super) fn project_readiness_tasks(
    runtime: &OrbitRuntime,
    inputs: ReadinessProjection<'_>,
) -> Vec<Value> {
    let ReadinessProjection {
        snapshot,
        selected_ids,
        pools,
        allowlist,
        holders,
        claim_holders,
        claimed_by_task,
        host_shutdown_holding,
        resource,
        gate,
        admissions_stopped,
        admitted,
        selection,
        light_budget,
        examined,
        live_leaves,
    } = inputs;
    let excluded_by_id = snapshot
        .excluded
        .iter()
        .map(|excluded| (excluded.id.as_str(), excluded))
        .collect::<BTreeMap<_, _>>();
    // Advisory: unreadable executor definitions only drop the OS wait that a
    // dependency wait would otherwise hide.
    let host_os_limits = runtime.executor_host_os_limits().unwrap_or_default();
    let follower_evidence = FollowerEvidence::read(runtime);
    selected_ids
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
            // The next run adopts and completes a preserved publication, so
            // no context lock holds it back; whatever else it waits on is
            // reported as for any task.
            if let Some(resume) = snapshot.published_resumes.get(&task.id) {
                object.insert(
                    "published_delivery".to_string(),
                    json!({
                        "source_run_id": resume.source_run_id,
                        "pull_request": resume.pull_request,
                        "detail": "Resumes its published delivery: the next run adopts the \
                                   existing branch and pull request before it edits, so other \
                                   tasks' context locks do not hold it back. It takes them \
                                   before any edit.",
                    }),
                );
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
                // Admission checks dependencies first, so a task that also
                // waits for another host's OS would show only its dependency.
                if let Some(wait) = runtime.task_host_os_wait(task, pools, &host_os_limits) {
                    object.insert("host_scope".to_string(), json!("local"));
                    object.insert("host_os_wait".to_string(), json!(format!(
                        "On this {} host: {wait}", runtime.host_os().map_or("unknown", |os| os.as_str())
                    )));
                    object.insert("executor_evidence".to_string(), follower_evidence.task_follower_evidence(task));
                    if let Some(fleet) = runtime.task_host_fleet_note(task) {
                        object.insert("host_fleet".to_string(), json!(fleet));
                    }
                }
                if let Some(wait) = runtime.task_host_labels_wait(task) {
                    object.insert("host_labels_wait".to_string(), json!(wait));
                }
                return Value::Object(object.clone());
            }
            if let Some(excluded) = excluded_by_id.get(task.id.as_str()) {
                match excluded.reason {
                    BacklogTaskExclusionReason::ActivePilotPreparation
                    | BacklogTaskExclusionReason::PilotAssessmentStale
                    | BacklogTaskExclusionReason::PilotDuplicate
                    | BacklogTaskExclusionReason::PilotAlreadyLanded
                    | BacklogTaskExclusionReason::OperatorValidationHandoff
                    | BacklogTaskExclusionReason::HostOperationalHandoff
                    | BacklogTaskExclusionReason::NativeOsRequired
                    | BacklogTaskExclusionReason::UnverifiedReviewFinding
                    | BacklogTaskExclusionReason::PrForgeRemoteMissing
                    | BacklogTaskExclusionReason::AwaitingFootprint
                    | BacklogTaskExclusionReason::AwaitingExclusiveSlot
                    | BacklogTaskExclusionReason::DeliveryJobUnavailable
                    // The one exclusion no later drain clears by itself, so
                    // readiness repeats the repair the drain recorded rather
                    // than leaving the reason to be interpreted.
                    | BacklogTaskExclusionReason::InheritedOnlyEpicRoot
                    // A verified kit on this host, or a host that has
                    // one, clears it; the detail names the install.
                    | BacklogTaskExclusionReason::BrowserKitMissing
                    // [ORB-14168] The drain records the same snake_case
                    // reason. The detail names the deciding layer and the
                    // remedy the admission refusal already uses.
                    | BacklogTaskExclusionReason::LocalRouteBeforePr
                    // [ORB-14849] As above, under its own code.
                    | BacklogTaskExclusionReason::LocalRouteBeforeLanding
                    // [ORB-14258] Lifts by itself once the command passes
                    // on a new base tip; the detail names the base and
                    // the command.
                    | BacklogTaskExclusionReason::BaselineRedHold
                    // [ORB-14266] Lifts by itself at the hold's
                    // `not_before`; the detail names the run, the
                    // failure, the excluded crews and the time.
                    | BacklogTaskExclusionReason::ProviderBackoff
                    // [ORB-14697] Lifts by itself at the reading's reset;
                    // the detail names the provider, window, used
                    // percent, threshold, reset and skipped crews.
                    | BacklogTaskExclusionReason::ProviderLimit
                    // Tagged `no-drain`: no automatic drain admits it; the
                    // detail names explicit ship / tag removal.
                    | BacklogTaskExclusionReason::NoDrainTag => {
                        object.insert("reason".to_string(), json!(excluded.reason));
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    // Waits, never runnable: lifts by itself on the first
                    // pass after `not_before`, which the entry names.
                    BacklogTaskExclusionReason::Scheduled => {
                        object.insert("reason".to_string(), json!(excluded.reason));
                        object.insert("detail".to_string(), json!(excluded.detail));
                        if let Some(not_before) = snapshot
                            .task_lookup
                            .get(&excluded.id)
                            .and_then(|task| task.not_before)
                        {
                            object.insert(
                                "not_before".to_string(),
                                json!(orbit_types::task::format_not_before(not_before)),
                            );
                        }
                    }
                    BacklogTaskExclusionReason::UnassessedComplexity => {
                        object.insert(
                            "reason".to_string(),
                            Value::String("task_pilot_preparation_required".to_string()),
                        );
                    }
                    BacklogTaskExclusionReason::HostOsMismatch => {
                        // Retagging, or a host of the named OS, clears it; the
                        // detail names which.
                        object.insert("reason".to_string(), json!(excluded.reason));
                        object.insert("detail".to_string(), json!(excluded.detail));
                        object.insert("host_os".to_string(), json!(runtime.host_os()));
                        object.insert("host_scope".to_string(), json!("local"));
                        object.insert("host_os_wait".to_string(), json!(format!(
                            "On this {} host: {}", runtime.host_os().map_or("unknown", |os| os.as_str()),
                            excluded.detail.as_deref().unwrap_or("OS requirement is not satisfied")
                        )));
                        let evidence = follower_evidence.task_follower_evidence(task);
                        let compatible = evidence["status"] == "compatible_recent";
                        object.insert("executor_evidence".to_string(), evidence);
                        if compatible {
                            // Diagnostic only: the local exclusion remains in
                            // the snapshot and no follower is declared ready.
                            object.insert("reason".to_string(), json!("follower_admission_pending"));
                            if !snapshot.published_resumes.contains_key(&task.id) {
                                let conflicts = readiness_hosts::follower_footprint_conflicts(runtime, task, holders);
                                if !conflicts.is_empty() {
                                    let blocking_ids = conflicts.iter().map(|conflict| conflict.blocking_task_id.as_str()).collect::<BTreeSet<_>>();
                                    let claims = claim_holders.json_for(blocking_ids.iter().copied());
                                    let run_ids = blocking_ids.iter()
                                        .flat_map(|id| claimed_by_task.get(*id).into_iter().flatten().cloned())
                                        .chain(claims.iter().filter_map(|claim| claim["run_id"].as_str().map(str::to_string)))
                                        .collect::<BTreeSet<_>>();
                                    object.insert("reason".to_string(), json!("context_lock_conflict"));
                                    object.insert("blocking_claims".to_string(), json!(claims));
                                    object.insert("run_ids".to_string(), json!(run_ids));
                                    object.insert("conflicts".to_string(), json!(conflicts.iter().map(|conflict| json!({
                                        "requested_file": conflict.requested_selector,
                                        "blocking_selector": conflict.blocking_selector,
                                        "locking_task_id": conflict.blocking_task_id,
                                        "provenance": conflict.provenance.as_str(),
                                    })).collect::<Vec<_>>()));
                                }
                            }
                        }
                        // Which registered hosts run the OS it needs, and
                        // whether each is pulling here.
                        let fleet = snapshot
                            .task_lookup
                            .get(&excluded.id)
                            .and_then(|task| runtime.task_host_fleet_note(task));
                        if let Some(fleet) = fleet {
                            object.insert("host_fleet".to_string(), json!(fleet));
                        }
                    }
                    BacklogTaskExclusionReason::HostLabelsMissing => {
                        // A host carrying the labels clears it: this one once
                        // `machine.labels` declares them, or a follower that
                        // does. The detail names the labels this host lacks.
                        object.insert(
                            "reason".to_string(),
                            Value::String("host_labels_missing".to_string()),
                        );
                        object.insert("detail".to_string(), json!(excluded.detail));
                        if let Some(wait) = snapshot
                            .task_lookup
                            .get(&excluded.id)
                            .and_then(|task| runtime.task_placement_wait(task))
                        {
                            object.insert("placement_wait".to_string(), json!(wait));
                        }
                    }
                    // Operator work: the human crew it waits for, never
                    // `crew_not_allowed`, whatever the allowlist says.
                    BacklogTaskExclusionReason::HumanCrew => {
                        object.insert("reason".to_string(), json!(excluded.reason));
                        object.insert("crew".to_string(), json!(excluded.crew));
                        object.insert("detail".to_string(), json!(excluded.detail));
                    }
                    BacklogTaskExclusionReason::CrewNotAllowed | BacklogTaskExclusionReason::CrewEnvUnset => {
                        object.insert("detail".to_string(), json!(excluded.detail));
                        object.insert("reason".to_string(), json!(excluded.reason));
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
                        object.insert("reason".to_string(), json!(excluded.reason));
                        object.insert(
                            "conflicts".to_string(),
                            json!(excluded.conflicts.iter().map(|conflict| json!({
                                "requested_file": conflict.requested_file,
                                "locking_task_id": conflict.locking_task_id,
                            })).collect::<Vec<_>>()),
                        );
                        let blocking_claims = claim_holders.json_for(
                            excluded.conflicts.iter().map(|conflict| conflict.locking_task_id.as_str()),
                        );
                        if !blocking_claims.is_empty() {
                            object.insert("blocking_claims".to_string(), json!(blocking_claims));
                        }
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
            } else if let Some(shutdown) = host_shutdown_holding.as_ref() {
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
                    let blocking_task_ids = deferred.blocking_task_ids();
                    let blocking_claims =
                        claim_holders.json_for(blocking_task_ids.iter().map(String::as_str));
                    if !blocking_claims.is_empty() {
                        object.insert("blocking_claims".to_string(), json!(blocking_claims));
                    }
                    object.insert("blocking_task_ids".to_string(), json!(blocking_task_ids));
                    object.insert("conflicts".to_string(), deferred.to_json()["conflicts"].clone());
                }
            } else {
                object.insert("reason".to_string(), Value::String("capacity_saturated".to_string()));
                object.insert("active_run_ids".to_string(), json!(live_leaves.iter().map(|run| &run.run_id).collect::<Vec<_>>()));
            }
            Value::Object(object.clone())
        })
        .collect::<Vec<_>>()
}

pub(super) fn project_readiness_forecasts(
    runtime: &OrbitRuntime,
    snapshot: &BacklogSnapshot,
    status_state: &Option<PipelineState>,
    status_run_id: Option<&str>,
    drain_live: bool,
    drain_phase: &str,
    tasks: &mut [Value],
) -> (Option<Value>, Option<Value>) {
    // Observations come from the drain's recorded pass, not a new forecast:
    // readiness never takes the admission lock or writes reservations. Keep
    // shadow decisions separate from actual eligibility, and retain any live
    // non-forecast hold that now takes precedence over the last pass.
    let admission_forecast = status_state
        .as_ref()
        .and_then(|state| state.pull_drain.drain_last_pass.as_ref())
        .and_then(|pass| pass.admission_forecast.as_ref());
    let project_forecast = |forecast: &orbit_types::workflow::DrainAdmissionForecast| {
        let settings = runtime.auto_concurrency_settings();
        let mut view = json!(forecast);
        view["headroom"] = json!({
            "cpu_percent": forecast.cpu_peak.as_ref().and_then(|peak| peak.percent)
                .map(|peak| f64::from(settings.admit_cpu_percent) - peak),
            "memory_percent": forecast.memory_peak.as_ref().and_then(|peak| peak.percent)
                .map(|peak| f64::from(settings.admit_memory_percent) - peak),
            "admit_cpu_percent": settings.admit_cpu_percent,
            "admit_memory_percent": settings.admit_memory_percent,
        });
        // Titles come from the admission population, which omits terminal
        // tasks. Statuses also come from the registry projection the snapshot
        // already holds, so a done task the forecast still names keeps its
        // current status (`done`, not the backlog population's silence).
        let mut titles = BTreeMap::new();
        let mut statuses = BTreeMap::new();
        for id in forecast
            .admitted
            .iter()
            .map(|task| &task.task_id)
            .chain(forecast.skipped.iter().map(|task| &task.task_id))
            .chain(forecast.head.iter().map(|head| &head.task_id))
        {
            if let Some(task) = snapshot.task_lookup.get(id) {
                titles.insert(id.clone(), task.title.clone());
                statuses.insert(id.clone(), task.status.to_string());
            } else if let Some(status) = snapshot.status_by_id.get(id) {
                statuses.insert(id.clone(), status.to_string());
            }
        }
        view["task_titles"] = json!(titles);
        view["task_statuses"] = json!(statuses);
        // The drain whose passes recorded it, and whether that drain is still
        // live: a finished drain's forecast is history, not this host's load.
        view["drain_run_id"] = json!(status_run_id);
        view["drain_live"] = json!(drain_live);
        view
    };
    let forecast_view = admission_forecast.map(&project_forecast);
    let last_forecast_view = status_state
        .as_ref()
        .and_then(|state| state.pull_drain.drain_last_forecast.as_ref())
        .map(project_forecast);
    if let Some(forecast) = admission_forecast {
        for task in tasks {
            let Some(id) = task["task_id"].as_str() else {
                continue;
            };
            let skipped = forecast
                .skipped
                .iter()
                .find(|candidate| candidate.task_id == id);
            let source = skipped
                .map(|candidate| &candidate.profile_source)
                .or_else(|| {
                    forecast
                        .admitted
                        .iter()
                        .find(|candidate| candidate.task_id == id)
                        .map(|candidate| &candidate.profile_source)
                });
            let Some(source) = source else { continue };
            let mut observation = skipped.map_or_else(|| json!({}), |skip| json!(skip));
            observation["mode"] = json!(forecast.mode);
            observation["forecast_at"] = json!(forecast.forecast_at);
            observation["profile_source"] = json!(source);
            observation["decision"] = json!(if skipped.is_some() {
                "skipped"
            } else {
                "admitted"
            });
            task["admission_forecast"] = observation;
            if forecast.mode == "enforce"
                && drain_phase == "draining"
                && skipped.is_some()
                && task["reason"] == "ready"
            {
                task["eligible"] = json!(false);
                task["reason"] = json!("gap_fit");
            }
        }
    }

    (forecast_view, last_forecast_view)
}
