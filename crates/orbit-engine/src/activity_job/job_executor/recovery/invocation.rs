//! Recovery activity input preparation and dispatch.
use super::input::{bind_recovery_context, validate_bound_recovery_context};
use super::observation::{RecoveryObservation, recovery_base_ref};
use super::*;
use crate::context::{
    STEP_RECOVERY_DECISION_SCHEMA_VERSION, StepRecoveryDecisionRequest, StepRecoveryDecisionSlot,
};

/// A completed recovery dispatch, the decision slot its input named, and
/// what the engine observed before it ran.
pub(super) struct RecoveryDispatch {
    pub(super) dispatch: crate::activity_job::dispatcher::DispatchOutcome,
    pub(super) slot: Option<StepRecoveryDecisionSlot>,
    pub(super) observation: Option<RecoveryObservation>,
}

/// Ask the host for this invocation's decision slot and name it in the
/// recovery input. Only `step_failure_recovery` carries the contract.
fn allocate_recovery_decision(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    attempt: u32,
    input: &mut Value,
) -> Result<Option<StepRecoveryDecisionSlot>, orbit_common::OrbitError> {
    if recovery.name != STEP_FAILURE_RECOVERY_ACTIVITY {
        return Ok(None);
    }
    let workspace_path = input
        .get("workspace_path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(slot) = ctx
        .host
        .allocate_step_recovery_decision(&StepRecoveryDecisionRequest {
            run_id: ctx.run_id.clone(),
            failed_step_id: step.id.clone(),
            attempt,
            workspace_path,
        })?
    else {
        return Ok(None);
    };
    input["recovery_decision"] = serde_json::json!({
        "path": slot.path.display().to_string(),
        "schema_version": STEP_RECOVERY_DECISION_SCHEMA_VERSION,
        "run_id": slot.run_id,
        "failed_step_id": slot.failed_step_id,
        "attempt": slot.attempt,
        "nonce": slot.nonce,
    });
    Ok(Some(slot))
}

pub(super) fn dispatch_recovery(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    failure: &StepFailure,
    attempt: u32,
    max_attempts: u32,
) -> Result<RecoveryDispatch, (&'static str, String)> {
    let mut input = serde_json::json!({
        "failed_step_id": step.id,
        "activity_name": step_activity_name(step),
        "error_message": bounded_recovery_text(
            "error_message",
            &ctx.run_id,
            &failure.diagnostic(),
            MAX_RECOVERY_ERROR_MESSAGE_BYTES,
        ),
        "attempt": attempt,
        "max_attempts": max_attempts,
    });
    if let StepFailure::Error(DispatchError::RecoverableVcsConflict {
        operation,
        original_base_sha,
        target_base_sha,
        conflicting_paths,
        diagnostic,
    }) = failure
        && let Some(object) = input.as_object_mut()
    {
        object.insert(
            "recovery_kind".to_string(),
            Value::String("vcs_conflict".to_string()),
        );
        object.insert("operation".to_string(), Value::String(operation.clone()));
        object.insert(
            "original_base_sha".to_string(),
            Value::String(original_base_sha.clone()),
        );
        object.insert(
            "target_base_sha".to_string(),
            Value::String(target_base_sha.clone()),
        );
        object.insert(
            "conflicting_paths".to_string(),
            Value::Array(
                conflicting_paths
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        object.insert("diagnostic".to_string(), Value::String(diagnostic.clone()));
    }
    if matches!(
        recovery.name.as_str(),
        STEP_FAILURE_RECOVERY_ACTIVITY | PR_CONFLICT_RECOVERY_ACTIVITY
    ) {
        bind_recovery_context(step, ctx, &mut input)
            .map_err(|error| ("input", error.to_string()))?;
        if recovery.name == STEP_FAILURE_RECOVERY_ACTIVITY {
            validate_bound_recovery_context(&input)
                .map_err(|error| ("input", error.to_string()))?;
        }
        inject_recovery_evidence(ctx, &mut input).map_err(|error| ("input", error))?;
        input["system_crew"] = Value::Bool(true);
    }
    let slot = allocate_recovery_decision(step, ctx, recovery, attempt, &mut input)
        .map_err(|error| ("decision", error.to_string()))?;
    // Observed before the provider runs, so an absent decision can be judged
    // against what the failure saw [ORB-14268].
    let observation = slot.as_ref().map(|slot| {
        RecoveryObservation::take(
            ctx,
            slot.workspace_root.clone(),
            recovery_base_ref(ctx, &input),
        )
    });
    let input =
        inject_system_crew_input(ctx.host, &input).map_err(|error| ("crew", error.to_string()))?;
    let crew_overridden_spec = crew_overridden_recovery_spec(recovery, ctx, &input)
        .map_err(|error| ("crew", error.to_string()))?;
    let spec = crew_overridden_spec.as_ref().unwrap_or(&recovery.spec);
    let dispatch = dispatch_v2_activity_without_run_id_injection(V2DispatchInput {
        activity_name: &recovery.name,
        spec,
        fs_profile: step_fs_profile(step),
        input: input.clone(),
        audit: ctx.audit.clone(),
        run_id: &ctx.run_id,
        host: Some(ctx.host),
    });

    match dispatch {
        Ok(dispatch) => {
            persist_dispatch_invocation(ctx, &recovery.name, &input, &dispatch);
            Ok(RecoveryDispatch {
                dispatch,
                slot,
                observation,
            })
        }
        Err(error) => Err(("dispatch", error.to_string())),
    }
}

pub(in crate::activity_job::job_executor) fn crew_overridden_recovery_spec(
    recovery: &ResolvedRecoveryActivity,
    ctx: &ExecCtx<'_>,
    input: &Value,
) -> Result<Option<ActivityV2Spec>, DispatchError> {
    let ActivityV2Spec::AgentLoop(inline_spec) = &recovery.spec else {
        return Ok(None);
    };
    let input = inject_system_crew_input(ctx.host, input)?;
    let Some(resolved) = resolve_crew_settings(ctx.host, inline_spec, &input, &ctx.input)? else {
        return Ok(None);
    };
    let mut spec = inline_spec.clone();
    apply_resolved_settings(&mut spec, &resolved);
    Ok(Some(ActivityV2Spec::AgentLoop(spec)))
}
