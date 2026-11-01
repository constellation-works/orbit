//! Run-record reads that step and final recovery admission depend on.

use orbit_common::OrbitError;

use crate::OrbitRuntime;

pub(super) fn source_repair_admission(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Option<orbit_types::workflow::SourceRepairAdmission>, OrbitError> {
    runtime
        .get_job_run_backend(run_id)?
        .and_then(|run| run.input)
        .and_then(|input| input.get(orbit_types::workflow::SOURCE_REPAIR_KEY).cloned())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| OrbitError::JobValidation(format!("source repair admission unreadable: {e}")))
}

pub(super) fn final_recovery_log_tail(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Option<String>, OrbitError> {
    Ok(runtime
        .read_pipeline_worker_log(run_id)?
        .and_then(|snapshot| snapshot.content))
}

pub(super) fn authorize_step_recovery(
    _runtime: &OrbitRuntime,
    _run_id: &str,
    _step_id: &str,
) -> Result<orbit_engine::StepRecoveryAdmission, OrbitError> {
    Ok(orbit_engine::StepRecoveryAdmission::Allowed)
}

pub(super) fn validate_step_recovery_mutation(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    task_ids: &[String],
    workspace_path: &std::path::Path,
) -> Result<(), OrbitError> {
    OrbitRuntime::validate_step_recovery_mutation(
        runtime,
        run_id,
        step_id,
        task_ids,
        workspace_path,
    )
}

pub(super) fn checkpoint_step(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_index: u32,
    step_id: &str,
    output: &serde_json::Value,
    compound_outputs: &std::collections::BTreeMap<String, serde_json::Value>,
    script_outputs: &std::collections::BTreeMap<
        String,
        orbit_types::workflow::activity_job::ScriptOutputSet,
    >,
) -> Result<(), orbit_engine::DispatchError> {
    super::checkpoints::checkpoint_step(
        runtime,
        run_id,
        step_index,
        step_id,
        output,
        compound_outputs,
        script_outputs,
    )?;
    // The step is durable, so this is a safe point to yield to an upgrade.
    runtime.yield_run_at_step_boundary(run_id);
    Ok(())
}

pub(super) fn acquire_deferred_footprint(
    runtime: &OrbitRuntime,
    run_id: &str,
    activity_name: &str,
) -> Result<(), orbit_engine::DispatchError> {
    // A footprint still held when the wait ends is held for the same
    // reason on an immediate retry, so the refusal is not retried.
    OrbitRuntime::acquire_deferred_footprint(runtime, run_id, activity_name)
        .map_err(|error| orbit_engine::DispatchError::CliInvocationPermanent(error.to_string()))
}

pub(super) fn admit_final_recovery(
    runtime: &OrbitRuntime,
    run_id: &str,
    request: &orbit_engine::FinalRecoveryAdmissionRequest,
) -> Result<orbit_engine::FinalRecoveryAdmission, OrbitError> {
    runtime.admit_run_final_recovery(run_id, request)
}

pub(super) fn completion_policy_digest(
    runtime: &OrbitRuntime,
    run_id: &str,
    task_ids: &[String],
) -> Result<Option<String>, OrbitError> {
    runtime.published_completion_policy(run_id, task_ids)
}

pub(super) fn completion_observation_admission(
    runtime: &OrbitRuntime,
    run_id: &str,
    task_id: &str,
    step_id: &str,
) -> Result<orbit_engine::CompletionObservationAdmission, OrbitError> {
    runtime.classify_published_completion(run_id, task_id, step_id, true)
}

pub(super) fn authenticate_completion_tail(
    runtime: &OrbitRuntime,
    run_id: &str,
    task_id: &str,
    step_id: &str,
) -> Result<bool, OrbitError> {
    runtime.authenticate_published_completion(run_id, task_id, step_id)
}

pub(super) fn apply_final_recovery(
    runtime: &OrbitRuntime,
    run_id: &str,
    application: &orbit_engine::FinalRecoveryApplication,
) -> Result<orbit_engine::FinalRecoveryApplied, OrbitError> {
    runtime.apply_run_final_recovery(run_id, application)
}

pub(super) fn allocate_step_recovery_decision(
    _runtime: &OrbitRuntime,
    request: &orbit_engine::StepRecoveryDecisionRequest,
) -> Result<Option<orbit_engine::StepRecoveryDecisionSlot>, OrbitError> {
    crate::runtime::recovery_decision::allocate(request).map(Some)
}

pub(super) fn read_step_recovery_decision(
    _runtime: &OrbitRuntime,
    slot: &orbit_engine::StepRecoveryDecisionSlot,
) -> Result<orbit_engine::StepRecoveryDecisionRead, OrbitError> {
    crate::runtime::recovery_decision::read(slot)
}

pub(super) fn owner_published_candidate(
    runtime: &OrbitRuntime,
    task_id: &str,
    source_run_id: Option<&str>,
) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError> {
    OrbitRuntime::owner_published_candidate(runtime, task_id, source_run_id)
}

pub(super) fn owner_publication_binding(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError> {
    OrbitRuntime::owner_publication_binding(runtime, run_id)
}

pub(super) fn validate_source_repair(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<(), OrbitError> {
    runtime.validate_operator_source_repair(run_id)
}
