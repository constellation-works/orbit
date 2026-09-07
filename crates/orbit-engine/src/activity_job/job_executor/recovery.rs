use super::*;

const PR_CONFLICT_RECOVERY_ACTIVITY: &str = "pr_conflict_recovery";

pub(super) fn recover_or_return_original(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    original_err: DispatchError,
    attempt: u32,
    max_attempts: u32,
) -> Result<StepOutcome, DispatchError> {
    let Some(recovery) = recovery_activity_for_step(step, ctx) else {
        return Err(original_err);
    };

    if attempt_recovery_activity(step, ctx, &recovery, &original_err, attempt, max_attempts) {
        match run_step_body(step, ctx) {
            Ok(outcome) if outcome.success => return Ok(outcome),
            Ok(_) | Err(_) => {}
        }
    }

    Err(original_err)
}

pub(super) fn recovery_activity_for_step(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
) -> Option<ResolvedRecoveryActivity> {
    match (
        step.recovery_activity.as_ref(),
        step.resolved_recovery_activity.as_ref(),
    ) {
        (Some(name), Some(activity)) => Some(ResolvedRecoveryActivity {
            name: name.clone(),
            spec: activity.spec.clone(),
        }),
        (Some(_), None) => None,
        _ => ctx.recovery_activity.clone(),
    }
}

pub(super) fn attempt_recovery_activity(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    original_err: &DispatchError,
    attempt: u32,
    max_attempts: u32,
) -> bool {
    if recovery.name == PR_CONFLICT_RECOVERY_ACTIVITY
        && !matches!(original_err, DispatchError::RecoverableVcsConflict { .. })
    {
        return false;
    }

    let mut input = serde_json::json!({
        "failed_step_id": step.id,
        "activity_name": step_activity_name(step),
        "error_message": original_err.to_string(),
        "attempt": attempt,
        "max_attempts": max_attempts,
    });
    if let DispatchError::RecoverableVcsConflict {
        operation,
        original_base_sha,
        target_base_sha,
        conflicting_paths,
        diagnostic,
    } = original_err
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
        "step_failure_recovery" | PR_CONFLICT_RECOVERY_ACTIVITY
    ) && let Some(object) = input.as_object_mut()
    {
        object.insert("system_crew".to_string(), Value::Bool(true));
    }
    let input = match inject_system_crew_input(ctx.host, &input) {
        Ok(input) => input,
        Err(error) => {
            tracing::warn!(
                target: "orbit.engine.job_executor",
                run_id = %ctx.run_id,
                failed_step_id = %step.id,
                recovery_activity = %recovery.name,
                error = %error,
                "step recovery crew resolution failed; preserving original step outcome"
            );
            emit_job_event_lossy(
                &ctx.audit,
                ctx.task_id(),
                V2AuditEventKind::StepRecoveryAttempted {
                    step_id: step.id.clone(),
                    recovery_activity: recovery.name.clone(),
                    recovery_succeeded: false,
                },
            );
            return false;
        }
    };
    let crew_overridden_spec = match crew_overridden_recovery_spec(recovery, ctx, &input) {
        Ok(spec) => spec,
        Err(error) => {
            tracing::warn!(
                target: "orbit.engine.job_executor",
                run_id = %ctx.run_id,
                failed_step_id = %step.id,
                recovery_activity = %recovery.name,
                error = %error,
                "step recovery crew resolution failed; preserving original step outcome"
            );
            emit_job_event_lossy(
                &ctx.audit,
                ctx.task_id(),
                V2AuditEventKind::StepRecoveryAttempted {
                    step_id: step.id.clone(),
                    recovery_activity: recovery.name.clone(),
                    recovery_succeeded: false,
                },
            );
            return false;
        }
    };
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

    let recovery_succeeded = match dispatch {
        Ok(dispatch) if dispatch.success => {
            // [ORB-00414] Best-effort dispatch-invocation persistence (a DB
            // record, not an audit-envelope write); a failure here is
            // intentionally non-fatal and does not affect recovery outcome. The
            // audit trail of the recovery attempt is emitted separately below.
            persist_dispatch_invocation(ctx, &recovery.name, &input, &dispatch);
            true
        }
        Ok(dispatch) => {
            // [ORB-00414] See above: non-audit DB persistence, non-fatal.
            persist_dispatch_invocation(ctx, &recovery.name, &input, &dispatch);
            false
        }
        Err(_) => false,
    };

    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::StepRecoveryAttempted {
            step_id: step.id.clone(),
            recovery_activity: recovery.name.clone(),
            recovery_succeeded,
        },
    );

    recovery_succeeded
}

/// Invoke the job's terminal failure hook once, preserving the original step
/// error regardless of the hook outcome. ADR-0246 keeps this separate from
/// retry recovery: the hook may publish a recoverable candidate, but it never
/// claims that the failed step succeeded.
pub(super) fn attempt_failure_activity(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    original_err: &DispatchError,
) {
    let Some(failure) = &ctx.failure_activity else {
        return;
    };
    let pipeline = Value::Object(
        ctx.pipeline
            .lock()
            .expect("pipeline poisoned")
            .clone()
            .into_iter()
            .collect(),
    );
    let error_code = match original_err {
        DispatchError::WorktreeIntegrity { code, .. } => *code,
        DispatchError::RecoverableVcsConflict { .. } => "recoverable_vcs_conflict",
        _ => "pipeline_step_failed",
    };
    let input = serde_json::json!({
        "failed_step_id": step.id,
        "activity_name": step_activity_name(step),
        "error_code": error_code,
        "error_message": original_err.to_string(),
        "job_input": ctx.input,
        "pipeline": pipeline,
        "run_id": ctx.run_id,
    });
    let dispatch = dispatch_v2_activity_without_run_id_injection(V2DispatchInput {
        activity_name: &failure.name,
        spec: &failure.spec,
        fs_profile: step_fs_profile(step),
        input: input.clone(),
        audit: ctx.audit.clone(),
        run_id: &ctx.run_id,
        host: Some(ctx.host),
    });
    match dispatch {
        Ok(dispatch) => {
            persist_dispatch_invocation(ctx, &failure.name, &input, &dispatch);
            if dispatch.success
                && let Err(error) = ctx.host.checkpoint_failure_activity(
                    &ctx.run_id,
                    &failure.name,
                    &step.id,
                    &dispatch.output,
                )
            {
                tracing::warn!(
                    target: "orbit.engine.job_executor",
                    run_id = %ctx.run_id,
                    failed_step_id = %step.id,
                    failure_activity = %failure.name,
                    error = %error,
                    "terminal failure activity evidence could not be checkpointed; preserving original step error"
                );
            }
            if !dispatch.success {
                tracing::warn!(
                    target: "orbit.engine.job_executor",
                    run_id = %ctx.run_id,
                    failed_step_id = %step.id,
                    failure_activity = %failure.name,
                    "terminal failure activity completed without success"
                );
            }
        }
        Err(error) => tracing::warn!(
            target: "orbit.engine.job_executor",
            run_id = %ctx.run_id,
            failed_step_id = %step.id,
            failure_activity = %failure.name,
            error = %error,
            "terminal failure activity failed; preserving original step error"
        ),
    }
}

pub(super) fn crew_overridden_recovery_spec(
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
