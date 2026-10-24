//! Apply one validated assessment to its task under the task and dependency-graph locks.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_store::contracts::{AtomicTaskMutationOutcome, AtomicTaskMutationParams};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    Task, TaskOsRequirement, TaskRelation, TaskRelationType, TaskStatus, normalize_task_tags,
};
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::super::apply::admission::{
    Admission, AppliedTaskMaterial, PreparedTaskSnapshot, ValidatedTask,
};
use super::atomic::{
    applied_material, apply_atomic_with_retries, resulting_fingerprint, with_task_locks,
};
use super::drift::task_snapshot_drift;
use super::history::{assessment_history_summary, material_change_detail, status_only_fingerprint};
#[cfg(test)]
use super::lock_hook;
use super::reports::{source_superseded, superseded_task};

/// Lock rounds before an assessment whose dependency graph keeps growing
/// between discovery and its locks is refused as stale.
const GRAPH_LOCK_ATTEMPTS: usize = 4;
const DEPENDENCY_GRAPH_CHANGED: &str = "dependency_graph_changed";
const DEPENDENCY_GRAPH_CHANGED_DETAIL: &str =
    "the dependency graph kept changing while task-pilot locked it";

/// Stale reason when the task's durable context creation grant is no longer
/// the one preparation recorded.
pub(super) const CONTEXT_CREATION_CHANGED: &str = "context_creation_changed";
pub(super) const CONTEXT_CREATION_CHANGED_DETAIL: &str =
    "task context creation authorization changed after preparation";

pub(in super::super) enum ApplyTaskOutcome {
    Applied(Option<String>),
    AlreadyApplied(Option<String>),
    Stale(&'static str, String),
    Superseded(Value),
    AdmissionRefused(Value),
    /// The final assessed tags and crew cannot run on any host, so the
    /// assessment is refused as invalid without writing any task material.
    Invalid(String),
}

/// Share the durable receipt check between replay admission and the locked
/// persistence check.
pub(in super::super) fn operation_was_applied(
    runtime: &OrbitRuntime,
    task_id: &str,
    operation_id: &str,
) -> Result<bool, OrbitError> {
    Ok(runtime
        .get_task_history(task_id)?
        .iter()
        .any(|event| event.event == "task_pilot_applied" && event.receipt() == Some(operation_id)))
}

pub(in super::super) fn apply_task(
    runtime: &OrbitRuntime,
    snapshot: &PreparedTaskSnapshot,
    task: &mut ValidatedTask,
    prepared: &Value,
    policy: &PreparationPolicy,
    ci_sweep: bool,
    mut admit: impl FnMut(
        &Task,
        &ValidatedTask,
        &super::super::blockers::PilotDependencyGraph,
    ) -> Result<Option<Admission>, Value>,
) -> Result<ApplyTaskOutcome, OrbitError> {
    if !matches!(snapshot.status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Ok(ApplyTaskOutcome::Stale(
            "status_not_mutable",
            "task-pilot does not rewrite in-progress, review, or terminal work".to_string(),
        ));
    }
    let named_blockers = crate::application::task::pilot_blocker_task_ids(&task.assessment);
    let mut snapshot = snapshot.clone();
    // Local named targets are locked before their status is read at
    // publication. Missing and other-workspace IDs remain warning text.
    let mut local_blockers = BTreeSet::new();
    for id in &named_blockers {
        if runtime.stores().tasks().get_task(id)?.is_some() {
            local_blockers.insert(id.clone());
        }
    }
    let mut attempt = 0;
    let mut graph_locks = 0;
    loop {
        // Discover the graph the cycle check reads while holding no task
        // lock, then lock all of it in one sorted order. A walk under fewer
        // locks would take a later task's lock out of order and could wait
        // forever on another application doing the same [ORB-15408].
        let graph = runtime
            .discover_pilot_dependency_graph(&runtime.get_task(&task.task_id)?, &local_blockers)?;
        let mut lock_ids = vec![task.task_id.clone()];
        lock_ids.extend(graph.task_ids().cloned());
        lock_ids.sort();
        lock_ids.dedup();
        #[cfg(test)]
        lock_hook::fire(lock_hook::Point::Discovered);
        let mut outcome = None;
        let mut retry_fingerprint = None;
        let mut graph_changed = false;
        let write_task_id = task.task_id.clone();
        let mut entered = false;
        let mut operation = || {
            entered = true;
            #[cfg(test)]
            lock_hook::fire(lock_hook::Point::Locked);
            let claim = crate::application::automation::members::claim(runtime, prepared)?;
            // Re-checked under the task locks: the branch may have moved
            // under this task's material since apply's own check [ORB-14476].
            if let Some(claim) = &claim
                && let Some(detail) = crate::application::automation::members::stale_tasks(
                    runtime,
                    claim,
                    policy,
                    prepared,
                    std::slice::from_ref(&task.task_id),
                    &BTreeMap::from([(task.task_id.clone(), task.after.clone())]),
                )?
                .remove(&task.task_id)
            {
                outcome = Some(ApplyTaskOutcome::Superseded(source_superseded(
                    &task.task_id,
                    &detail,
                )));
                return Ok(());
            }
            let mut current = match runtime.get_task(&task.task_id) {
                Ok(current) => current,
                Err(OrbitError::NotFound { .. }) => {
                    outcome = Some(ApplyTaskOutcome::Stale(
                        "task_deleted",
                        "task no longer exists at the write boundary".to_string(),
                    ));
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            // Re-read under the discovered locks even on replay. A replay
            // may approve only dependencies actually stored, never edges the
            // original write skipped or edges it would add on a new apply.
            let Some(graph) =
                runtime.reread_pilot_dependency_graph(&current, &local_blockers, &graph)?
            else {
                graph_changed = true;
                return Ok(());
            };
            if operation_was_applied(runtime, &task.task_id, &task.operation_id)? {
                match admit(&current, task, &graph) {
                    Ok(admission) => task.set_admission(admission),
                    Err(refusal) => {
                        outcome = Some(ApplyTaskOutcome::AdmissionRefused(refusal));
                        return Ok(());
                    }
                }
                task.applied_material =
                    applied_material(runtime, &task.task_id, &task.operation_id)?;
                outcome = Some(ApplyTaskOutcome::AlreadyApplied(resulting_fingerprint(
                    runtime,
                    &task.task_id,
                    &snapshot,
                    policy,
                )?));
                return Ok(());
            }
            if let Some(superseded) =
                superseded_task(runtime, &task.task_id, &snapshot, policy, ci_sweep)?
            {
                outcome = Some(ApplyTaskOutcome::Superseded(superseded));
                return Ok(());
            }
            if ci_sweep && current.status != TaskStatus::Proposed {
                // A different CI operation must settle a prior promotion as
                // superseded before the pilot's own selector changes look
                // like drift. The receipt case above already handled replay.
                match admit(&current, task, &graph) {
                    Ok(admission) => task.set_admission(admission),
                    Err(refusal) => {
                        outcome = Some(ApplyTaskOutcome::AdmissionRefused(refusal));
                        return Ok(());
                    }
                }
            }
            if let Some(reason) = task_snapshot_drift(runtime, &current, &snapshot, policy) {
                if attempt == 0 && matches!(reason.0, "material_changed" | "status_changed") {
                    retry_fingerprint =
                        status_only_fingerprint(runtime, &current, &snapshot, policy)
                            .map(|fingerprint| (fingerprint, current.status));
                }
                if retry_fingerprint.is_none() {
                    let detail = if reason.0 == "material_changed" {
                        material_change_detail(runtime, &current, &snapshot, policy)
                    } else {
                        reason.1.to_string()
                    };
                    outcome = Some(ApplyTaskOutcome::Stale(reason.0, detail));
                }
                return Ok(());
            }
            if runtime.context_creation_state(&current)?.identity()
                != snapshot.context_creation_identity
            {
                outcome = Some(ApplyTaskOutcome::Stale(
                    CONTEXT_CREATION_CHANGED,
                    CONTEXT_CREATION_CHANGED_DETAIL.to_string(),
                ));
                return Ok(());
            }
            if attempt == 1 && !matches!(current.status, TaskStatus::Proposed | TaskStatus::Backlog)
            {
                outcome = Some(ApplyTaskOutcome::Stale(
                    "status_changed",
                    "task status changed after preparation; task-pilot does not rewrite active work"
                        .to_string(),
                ));
                return Ok(());
            }

            let add_dependencies = graph.blocker_dependencies(&current, &local_blockers)?;
            current
                .relations
                .extend(add_dependencies.iter().map(|target| TaskRelation {
                    relation_type: TaskRelationType::BlockedBy,
                    target: target.clone(),
                    regression: None,
                }));
            match admit(&current, task, &graph) {
                Ok(admission) => task.set_admission(admission),
                Err(refusal) => {
                    outcome = Some(ApplyTaskOutcome::AdmissionRefused(refusal));
                    return Ok(());
                }
            }
            let target_status = if task.promote {
                TaskStatus::Backlog
            } else {
                snapshot.status
            };
            // A finding that names one OS routes the task by its tag, but only
            // when the task has no `os:` tag yet: an operator's choice of
            // hosts stands, and the hold below still names a conflict.
            let mut add_tags = super::super::routing_os(&task.required_os)
                .filter(|_| TaskOsRequirement::from_tags(&current.tags).is_unrestricted())
                .map(|os| vec![os.tag()])
                .unwrap_or_default();
            // A UI finding only ever adds `ui`; a task without the finding
            // keeps whatever `ui` tag it has.
            if task.user_facing_ui && !current.tags.iter().any(|tag| tag == super::super::UI_TAG) {
                add_tags.push(super::super::UI_TAG.to_string());
            }
            // Markers and holds below seal the material as written.
            let tags_after =
                normalize_task_tags(current.tags.iter().chain(&add_tags).cloned().collect());
            let mut history_summary = assessment_history_summary(&task.assessment);
            for warning in task.assessment[super::super::PILOT_NORMALIZATIONS]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|normalization| normalization.get("dropped_self_reference").is_some())
                .filter_map(|normalization| normalization["note"].as_str())
            {
                history_summary.push_str("; ");
                history_summary.push_str(warning);
            }
            if task.after.is_empty() {
                // Record the meaning *after* the atomic write. A marker made
                // from the prepared task would immediately be stale when the
                // pilot changes its complexity or status.
                let mut assessed = current.clone();
                assessed.context_files = task.after.clone();
                assessed.tags = tags_after.clone();
                assessed.complexity = Some(task.complexity);
                assessed.status = target_status;
                let fingerprint = crate::application::automation::preparation::pilot_fingerprint(
                    runtime,
                    &assessed,
                    snapshot
                        .material
                        .as_ref()
                        .map(|(_, revision)| revision.as_str()),
                    policy,
                )
                .map_err(orbit_automation::automation_error_to_orbit)?;
                history_summary.push_str(&format!(
                    " [no-target-assessed:{}:{fingerprint}]",
                    target_status.cli_name()
                ));
            }
            let mut requirements =
                super::super::validation_tools::ImplementationLane::resolve(runtime)
                    .operator_requirements(&current);
            // A typed post-deploy measurement is the operator's work after
            // landing, not a tool requirement for candidate admission.
            requirements.retain(|requirement| {
                !task
                    .post_deploy
                    .iter()
                    .any(|owed| owed.criterion == requirement.criterion)
            });
            let mut assessed = current.clone();
            assessed.context_files = task.after.clone();
            assessed.tags = tags_after.clone();
            let crew_redraw_history =
                runtime.rerate_task_crew(&mut assessed, Some(task.complexity))?;
            assessed.complexity = Some(task.complexity);
            assessed.status = target_status;
            // The final crew (kept, or redrawn for the assessed complexity)
            // and tags (the task's own plus any native-OS tag added above)
            // must share a host. Refusing here, before the atomic mutation,
            // leaves every stored field and approval untouched.
            if let Some(crew) = assessed.crew.as_deref()
                && let Err(error) = runtime.refuse_unsatisfiable_os_crew(&assessed.tags, crew)
            {
                let detail = match error {
                    OrbitError::InvalidInput(detail) => detail,
                    error => error.to_string(),
                };
                outcome = Some(ApplyTaskOutcome::Invalid(detail));
                return Ok(());
            }
            // Seal the exact authorized output while the same task and graph
            // locks still cover it. Approval must never certify a later read.
            let fingerprints = snapshot
                .material
                .as_ref()
                .map(|(_, revision)| {
                    crate::application::automation::preparation::fingerprints(
                        runtime, &assessed, revision, policy,
                    )
                    .map_err(orbit_automation::automation_error_to_orbit)
                })
                .transpose()?;
            let written = AppliedTaskMaterial {
                tags: assessed.tags.clone(),
                crew: assessed.crew.clone(),
                crew_source: assessed.crew_source.clone(),
                relations: assessed.relations.clone(),
                material: fingerprints.as_ref().zip(snapshot.material.as_ref()).map(
                    |(fingerprints, (_, revision))| {
                        (fingerprints.material.clone(), revision.clone())
                    },
                ),
                status_neutral_fingerprint: fingerprints
                    .as_ref()
                    .map(|fingerprints| fingerprints.status_neutral.clone()),
                material_components: fingerprints.map(|fingerprints| fingerprints.components),
            };
            let operator_hold = (!requirements.is_empty()).then(|| {
                crate::application::task::OperatorValidationHold::new(&assessed, requirements)
            });
            let host_operational_hold = (task.assessment["disposition"] == "host_operational")
                .then(|| {
                    crate::application::task::HostOperationalHold::new(
                        &assessed,
                        task.assessment["evidence"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                });
            // Recorded whatever the task's tags are now: admission compares it
            // with the tags each time, so the matching `os:` tag satisfies an
            // OS requirement and removing that tag restores it.
            let native_os_hold = (!task.required_os.is_empty() || task.required_machine.is_some())
                .then(|| {
                    crate::application::task::NativeOsHold::new(
                        &assessed,
                        task.required_os.clone(),
                        task.required_machine.clone(),
                    )
                });
            let post_deploy = (!task.post_deploy.is_empty()).then(|| {
                super::super::post_deploy::PostDeployFinding::new(
                    &assessed,
                    task.post_deploy.clone(),
                )
            });
            let mutation_params = AtomicTaskMutationParams {
                actor: super::super::PILOT_ACTOR.to_string(),
                operation_id: task.operation_id.clone(),
                expected_context_files: snapshot.context_files.clone(),
                expected_status: snapshot.status,
                expected_complexity: snapshot.complexity,
                expected_crew: current.crew.clone(),
                expected_crew_source: current.crew_source.clone(),
                crew: assessed.crew.clone(),
                crew_source: assessed.crew_source.clone(),
                expected_context_creation: snapshot.context_creation_identity.clone(),
                context_files: task.after.clone(),
                add_tags,
                add_dependencies,
                status: target_status,
                complexity: task.complexity,
                event_type: "task_pilot_applied".to_string(),
                event_note: "task-pilot atomic application".to_string(),
                history_summary,
                decision: task.history_decision.clone(),
                append_history: operator_hold
                    .as_ref()
                    .map(|hold| hold.history(&task.operation_id))
                    .into_iter()
                    .chain(
                        host_operational_hold
                            .as_ref()
                            .map(|hold| hold.history(&task.operation_id)),
                    )
                    .chain(crew_redraw_history)
                    .collect(),
                audit_note: serde_json::to_string(&json!({
                    "assessment": task.assessment,
                    "operator_validation_hold": operator_hold,
                    "host_operational_hold": host_operational_hold,
                    "native_os_hold": native_os_hold,
                    "post_deploy": post_deploy,
                    "context_files_before": snapshot.context_files,
                    "complexity_before": snapshot.complexity,
                    "complexity_after": task.complexity,
                    "applied_material": written,
                    "pilot_run_id": task.pilot_run_id,
                }))
                .map_err(|error| {
                    OrbitError::Execution(format!("serialize task-pilot audit: {error}"))
                })?,
            };
            let mutation = apply_atomic_with_retries(runtime, &task.task_id, &mutation_params)?;
            match mutation {
                AtomicTaskMutationOutcome::Applied => {
                    task.applied_material = Some(written);
                    runtime.record_event(OrbitEvent::TaskUpdated {
                        id: task.task_id.clone(),
                    })?;
                    outcome = Some(ApplyTaskOutcome::Applied(resulting_fingerprint(
                        runtime,
                        &task.task_id,
                        &snapshot,
                        policy,
                    )?));
                }
                AtomicTaskMutationOutcome::AlreadyApplied => {
                    task.applied_material =
                        applied_material(runtime, &task.task_id, &task.operation_id)?;
                    outcome = Some(ApplyTaskOutcome::AlreadyApplied(resulting_fingerprint(
                        runtime,
                        &task.task_id,
                        &snapshot,
                        policy,
                    )?));
                }
                AtomicTaskMutationOutcome::Stale => {
                    outcome = Some(ApplyTaskOutcome::Stale(
                        "write_boundary_changed",
                        "task changed between validation and the atomic write boundary".to_string(),
                    ));
                }
            }
            Ok(())
        };
        if let Err(error) = with_task_locks(runtime, &write_task_id, &lock_ids, 0, &mut operation) {
            if !entered && matches!(error, OrbitError::NotFound { .. }) {
                // A discovered prerequisite disappeared before its read lock.
                // No mutation ran: rediscover without that bundle so its
                // unedged finding becomes a warnings hold under the locks.
                graph_changed = true;
            } else {
                // Claim admission fences ordinary writers before the closure can
                // run. Re-read durable ownership instead of matching error prose.
                if let Some(outcome) =
                    superseded_task(runtime, &task.task_id, &snapshot, policy, ci_sweep)?
                {
                    return Ok(ApplyTaskOutcome::Superseded(outcome));
                }
                return Err(error);
            }
        }
        if graph_changed {
            graph_locks += 1;
            if graph_locks == GRAPH_LOCK_ATTEMPTS {
                return Ok(ApplyTaskOutcome::Stale(
                    DEPENDENCY_GRAPH_CHANGED,
                    DEPENDENCY_GRAPH_CHANGED_DETAIL.to_string(),
                ));
            }
            continue;
        }
        if let Some((fingerprint, status)) = retry_fingerprint {
            // Admit this fresh fingerprint only once. The next pass releases
            // and reacquires the task/dependency locks, then reads them again.
            snapshot.material = snapshot
                .material
                .map(|(_, revision)| (fingerprint, revision));
            snapshot.status = status;
            attempt += 1;
            continue;
        }
        return outcome
            .ok_or_else(|| OrbitError::Execution("task-pilot operation did not run".to_string()));
    }
}
