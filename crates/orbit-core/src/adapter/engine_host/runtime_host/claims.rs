//! Claimed-leaf admission, execution context and durable handoffs.

use orbit_common::OrbitError;
use orbit_engine::TaskActivityUpdate;
use orbit_types::task::{ContextWideningStep, Task, TaskStatus};

use crate::OrbitRuntime;

pub(super) fn admit_task_for_workflow(
    runtime: &OrbitRuntime,
    task_id: &str,
    workflow: &str,
) -> Result<Task, OrbitError> {
    if let Some(binding) = runtime.worker_invocation() {
        if task_id != binding.task_id {
            return Err(OrbitError::PolicyDenied(format!(
                "worker task binding mismatch: this leaf is bound to '{}', {workflow} \
                 requested '{task_id}'",
                binding.task_id
            )));
        }
        let task: Task = runtime.read_owner(task_id, "task")?;
        if task.status != TaskStatus::InProgress {
            return Err(OrbitError::PolicyDenied(format!(
                "claimed task '{task_id}' is '{}' on the owner; only an admitted \
                 in-progress claim may build a worktree",
                task.status
            )));
        }
        return Ok(task);
    }
    OrbitRuntime::admit_task_for_workflow_as_system(runtime, task_id, workflow)
}

pub(super) fn update_task_from_activity(
    runtime: &OrbitRuntime,
    task_id: &str,
    update: TaskActivityUpdate,
) -> Result<Task, OrbitError> {
    if runtime.worker_invocation().is_some() {
        if update.status == TaskStatus::Done {
            let task = runtime.get_task(task_id)?;
            crate::application::task::ensure_post_deploy_criteria_met(runtime, &task)?;
            crate::application::task::ensure_completion_run_stopped(
                runtime,
                &task,
                None,
                update.calling_run_id.as_deref(),
            )?;
        }
        runtime.route_worker_host_tool("orbit.task.update", serde_json::json!({
            "id": task_id,
            "_worker_update": orbit_store::contracts::ClaimWorkerUpdate {
                status: Some(update.status), expected_status: Some(update.expected_status),
                status_note: update.note,
                evidence: orbit_store::contracts::ClaimEvidence {summary: update.execution_summary, comment: update.comment, artifacts: vec![], provider_unavailable: None, baseline_red: None, final_recovery: None, failure: None, evidence_hold: None, held_pull_request: None, forge_hold: None, claimed_review: None, crew: None, crew_model: None},
                ..Default::default()
            }
        }))?;
        return runtime.get_task(task_id);
    }
    OrbitRuntime::update_task_from_activity(runtime, task_id, update)
}

pub(super) fn claim_execution_context(
    runtime: &OrbitRuntime,
) -> Result<orbit_engine::ClaimExecutionContext, OrbitError> {
    let leaf = runtime.current_claimed_leaf()?;
    let ship = &leaf.admission.request.ship;
    Ok(orbit_engine::ClaimExecutionContext {
        footprint: leaf.claim.footprint.clone(),
        workspace_id: leaf.binding.owner_workspace_id.clone(),
        task_id: leaf.claim.task_id.clone(),
        claim_id: leaf.claim.claim_id.clone(),
        machine_id: leaf.claim.executed_on.machine_id.clone(),
        run_id: leaf.binding.bound_run_id.clone(),
        ship_mode: ship.mode.clone(),
        base_branch: ship.base_branch.clone(),
        landing_branch: ship.landing_branch.clone(),
        // The owner's list its admission captured [ORB-15622]. The owner
        // still re-derives its own requirements when it accepts the
        // handoff, so a list changed since admission produces evidence
        // the owner refuses, which is the fail-closed direction.
        required_commands: runtime
            .claim_validation_gates(Ok(leaf.owner_gates()))
            .gates
            .required_validation_commands,
    })
}

pub(super) fn record_claim_handoff(
    runtime: &OrbitRuntime,
    handoff: &orbit_types::workflow::handoff::TaskHandoff,
) -> Result<(), OrbitError> {
    let leaf = runtime.current_claimed_leaf()?;
    if handoff.task_id != leaf.claim.task_id
        || handoff.claim_id != leaf.claim.claim_id
        || handoff.run_id != leaf.binding.bound_run_id
        || handoff.machine_id != leaf.claim.executed_on.machine_id
        || handoff.workspace_id != leaf.binding.owner_workspace_id
    {
        return Err(OrbitError::PolicyDenied(
            "handoff identity does not match this claim's trusted binding".into(),
        ));
    }
    // Durable before any owner call: a disconnect here leaves exactly one
    // immutable settlement for the drain to retry idempotently.
    runtime.stores().jobs().mutate_local_pull(
        &leaf.admission.destination,
        &leaf.admission.request.request_id,
        &orbit_store::contracts::LocalPullMutation::Settle(Box::new(
            orbit_store::contracts::ClaimMutation::AcceptHandoff(handoff.clone()),
        )),
    )?;
    Ok(())
}

pub(super) fn kept_claim_candidate(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Option<orbit_store::contracts::KeptClaimCandidate>, OrbitError> {
    // Without an identity this host is no machine a candidate lives on,
    // so only a durable one is offered.
    runtime.stores().tasks().kept_claim_candidate(
        task_id,
        runtime.automation_machine_identity().unwrap_or_default(),
    )
}

pub(super) fn widen_task_context_files(
    runtime: &OrbitRuntime,
    task_id: &str,
    run_id: &str,
    step: ContextWideningStep,
    activity: &str,
    paths: &[String],
) -> Result<Vec<String>, OrbitError> {
    // A claimed leaf cannot write the owner's task; the owner widens the
    // declaration when it accepts the handoff.
    if runtime.worker_invocation().is_some() {
        return Ok(Vec::new());
    }
    runtime.widen_context_files_for_paths(task_id, run_id, step, activity, paths)
}
