use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskStatus, push_external_ref_if_missing};

use crate::OrbitRuntime;
use crate::application::task::TaskRecordUpdateParams as StoreTaskUpdateParams;
use crate::application::task::{
    SYSTEM_ACTOR_LABEL, TaskAttributionInput, TaskUpdateParams, assemble_task_attribution,
};

/// Apply an automation update while holding the task write lock across the
/// whole read-modify-write.
///
/// ORB-11623: delivery automation previously read the task, derived
/// attribution and a replacement `external_refs` vector, then wrote through
/// `task_records().update` with no lock and `expected_status: None`. The
/// store locks each write, not the read that decided it, so an operator
/// transition or another ref writer in that gap was overwritten. The lock
/// is re-entrant per thread, so the store's own per-write locking still
/// holds underneath.
pub(super) fn apply_locked_task_automation_update(
    runtime: &OrbitRuntime,
    task_id: &str,
    update: TaskAutomationUpdate,
) -> Result<(), OrbitError> {
    let mut update = Some(update);
    let mut updated: Option<Task> = None;
    runtime
        .stores()
        .tasks()
        .with_task_write_lock(task_id, &mut || {
            let update = update.take().ok_or_else(|| {
                OrbitError::Execution(
                    "task automation update body was invoked more than once".to_string(),
                )
            })?;
            updated = Some(apply_task_automation_update_under_lock(
                runtime, task_id, update,
            )?);
            Ok(())
        })?;
    let task = updated.ok_or_else(|| {
        OrbitError::Execution(
            "task automation update body did not run under the task lock".to_string(),
        )
    })?;
    if task.status == TaskStatus::Done {
        runtime.record_resolves_side_effects(&task)?;
    }
    Ok(())
}

fn apply_task_automation_update_under_lock(
    runtime: &OrbitRuntime,
    task_id: &str,
    update: TaskAutomationUpdate,
) -> Result<Task, OrbitError> {
    let existing_task = runtime.get_task(task_id)?;
    #[cfg(test)]
    runtime.invoke_after_locked_state_read(&existing_task);
    if update.status == Some(TaskStatus::InProgress)
        && crate::application::task::in_progress_transition_requires_plan(existing_task.status)
    {
        crate::application::task::ensure_task_has_execution_plan(
            task_id,
            existing_task.plan.as_str(),
        )?;
    }
    if update.status == Some(TaskStatus::Done) && existing_task.status != TaskStatus::Done {
        runtime.ensure_resolves_are_workspace_local(&existing_task)?;
    }
    let (agent, model) =
        crate::context::trusted_write_identity(update.agent.as_deref(), update.model.as_deref());
    let runtime_model_identity = <OrbitRuntime as RuntimeHost>::actor_model_identity(runtime);
    let attribution = assemble_task_attribution(
        &existing_task,
        TaskAttributionInput {
            default_actor_label: SYSTEM_ACTOR_LABEL,
            actor_override: Some(SYSTEM_ACTOR_LABEL),
            agent: agent.as_deref(),
            model: model.as_deref(),
            runtime_model_identity: runtime_model_identity.as_deref(),
            plan_changed: update.plan.is_some(),
            target_status: update.status,
            explicit_planned_by: None,
            explicit_implemented_by: None,
        },
    )?;
    runtime.with_mutation(|| {
        let external_refs = if update.external_refs.is_empty() {
            None
        } else {
            // Merge against the latest locked state. `external_refs` is a
            // wholesale replacement in the store, so a re-entrant writer that
            // landed a ref after the decision snapshot would otherwise be lost.
            let mut refs = runtime.get_task(task_id)?.external_refs;
            for external_ref in update.external_refs.clone() {
                push_external_ref_if_missing(&mut refs, external_ref);
            }
            Some(refs)
        };
        let task = runtime.stores().task_records().update(
            task_id,
            StoreTaskUpdateParams {
                job_run_machine: update
                    .job_run_id
                    .as_ref()
                    .map(|run_id| {
                        runtime
                            .stores()
                            .jobs()
                            .get_job_run(run_id)
                            .map(|run| run.and_then(|run| run.executed_on))
                    })
                    .transpose()?,
                actor: attribution.actor.clone(),
                planned_by: attribution.planned_by.clone(),
                implemented_by: attribution.implemented_by.clone(),
                external_refs,
                status_event: update.status_event.clone(),
                status_note: update.status_note.clone(),
                append_comments: update.append_comments.clone(),
                expected_status: Some(vec![existing_task.status]),
                ..StoreTaskUpdateParams::from(TaskUpdateParams {
                    execution_summary: update.execution_summary.clone(),
                    plan: update.plan.clone(),
                    context_files: update.context_files.clone(),
                    status: update.status,
                    job_run_id: update.job_run_id.clone().map(Some),
                    ..Default::default()
                })
            },
        )?;
        Ok((
            task.clone(),
            OrbitEvent::TaskUpdated {
                id: task_id.to_string(),
            },
        ))
    })
}
