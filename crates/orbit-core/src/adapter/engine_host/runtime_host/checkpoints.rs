use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::recovery_authority::RecoveryAuthority;

/// [ORB-10002] Persist a per-step checkpoint into the run's
/// `PipelineState` so an interrupted run can be resumed without
/// re-executing completed steps. The step's output lands in
/// `step_outputs[step_index]` (what resume seeds from) and is merged into
/// `pipeline[step_id]` (what mid-run readers such as step recovery and
/// `orbit run show` key by). A missing run row (direct `execute_job`
/// callers that never persisted a run) is a silent no-op — there is
/// nothing durable to checkpoint into.
pub(super) fn checkpoint_step(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_index: u32,
    step_id: &str,
    output: &Value,
) -> Result<(), DispatchError> {
    // [ORB-11253] Read-modify-write in one transaction rather than a
    // separate read and write: an operator run control written into this
    // same state document between the two would otherwise be silently
    // discarded by the checkpoint that follows it.
    runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            state.record_step(
                step_index,
                orbit_types::workflow::JobRunState::Success,
                Some(output.clone()),
                None,
            );
            state.record_pipeline_output(step_id, output.clone());
            Ok(())
        })
        .map(|_| ())
        .map_err(|error| {
            DispatchError::JobExecution(format!(
                "persist step checkpoint (run {run_id}, step {step_index} `{step_id}`): {error}"
            ))
        })
}

pub(super) fn checkpoint_failure_activity(
    runtime: &OrbitRuntime,
    run_id: &str,
    activity_name: &str,
    failed_step_id: &str,
    output: &Value,
) -> Result<(), DispatchError> {
    runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            state.record_failure_activity(
                activity_name.to_string(),
                failed_step_id.to_string(),
                output.clone(),
            );
            Ok(())
        })
        .map(|_| ())
        .map_err(|error| {
            DispatchError::JobExecution(format!(
                "persist terminal failure activity checkpoint (run {run_id}, step \
                 `{failed_step_id}`, activity `{activity_name}`): {error}"
            ))
        })
}

/// Certify the host's completion first, then persist the advisory copy.
///
/// The certificate lives outside every leaf write grant; the run-state
/// entry that follows it is progress data a leaf can rewrite. Either half
/// failing leaves the run without usable evidence rather than with
/// unauthenticated evidence, so both orders are fail-closed.
pub(super) fn checkpoint_rebase_recovery(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    output: &Value,
) -> Result<(), DispatchError> {
    RecoveryAuthority::open(&runtime.global_root())
        .and_then(|authority| authority.issue(run_id, step_id, output))
        .map_err(|error| {
            DispatchError::JobExecution(format!(
                "certify rebase recovery (run {run_id}, step `{step_id}`): {error}"
            ))
        })?;

    runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |run_state, state| {
            if run_state != orbit_types::workflow::JobRunState::Running {
                return Err(orbit_common::OrbitError::Execution(
                    "rebase recovery run is no longer running".to_string(),
                ));
            }
            state
                .rebase_recovery_checkpoints
                .insert(step_id.to_string(), output.clone());
            state.updated_at = Utc::now();
            Ok(())
        })
        .and_then(|updated| {
            if matches!(updated, orbit_types::workflow::RunStateUpdate::Updated) {
                Ok(())
            } else {
                Err(orbit_common::OrbitError::Execution(
                    "rebase recovery has no durable run state".to_string(),
                ))
            }
        })
        .map_err(|error| {
            DispatchError::JobExecution(format!(
                "persist rebase recovery checkpoint (run {run_id}, step `{step_id}`): {error}"
            ))
        })
}

pub(super) fn verify_rebase_recovery(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    checkpoint: &Value,
) -> Result<bool, OrbitError> {
    RecoveryAuthority::open(&runtime.global_root())?.verify(run_id, step_id, checkpoint)
}
