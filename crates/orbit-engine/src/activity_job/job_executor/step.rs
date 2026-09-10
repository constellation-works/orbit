use super::*;

pub(super) fn run_step(step: &JobV2Step, ctx: &ExecCtx<'_>) -> Result<StepOutcome, DispatchError> {
    // `when:` evaluated once, before retry. A skipped step doesn't retry.
    if let Some(expr) = &step.when {
        let tctx = ctx.template_ctx();
        let matched = evaluate_bool_expr(expr, &tctx)
            .map_err(|err| DispatchError::JobExecution(format!("when expr: {err}")))?;
        if !matched {
            emit_job_event_lossy(
                &ctx.audit,
                ctx.task_id(),
                V2AuditEventKind::StepSkipped {
                    step_id: step.id.clone(),
                    reason: format!("when:{expr} => false"),
                },
            );
            return Ok(StepOutcome {
                success: true,
                output: Value::Null,
                message: None,
            });
        }
    }

    let step_event_id = emit_job_event(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::StepStarted {
            step_id: step.id.clone(),
        },
    )
    .map_err(|e| DispatchError::AuditFailed(format!("{e:?}")))?;
    ctx.audit.push_parent_lossy(step_event_id);

    let result = run_step_with_retry(step, ctx);

    ctx.audit.pop_parent_lossy();
    let (outcome_str, error_message) = match &result {
        Ok(StepOutcome { success: true, .. }) => ("success", None),
        Ok(StepOutcome { message, .. }) => ("failed", message.clone()),
        Err(err) => ("error", Some(err.to_string())),
    };
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::StepFinished {
            step_id: step.id.clone(),
            outcome: outcome_str.to_string(),
            error_message,
        },
    );

    result
}

pub(super) fn run_step_with_retry(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let Some(retry) = &step.retry else {
        // No retry wrapper — single attempt.
        return match run_step_body(step, ctx) {
            Ok(outcome) => Ok(outcome),
            Err(err) if err.is_non_retryable() => {
                emit_denied_if_applicable(&err, &step.id, &ctx.audit, ctx.task_id());
                if err.allows_recovery() {
                    recover_or_return_original(step, ctx, err, 1, 1)
                } else {
                    Err(err)
                }
            }
            Err(err) => recover_or_return_original(step, ctx, err, 1, 1),
        };
    };

    let mut last_err: Option<DispatchError> = None;
    // [ORB-10449] Keep the last failing attempt's diagnostic. A step that fails
    // via `Ok(success: false)` — every CLI agent-loop failure, including the
    // step-completion protocol violation — used to have its message dropped on
    // retry exhaustion, leaving the run's terminal error as the generic
    // "completed with success=false" fallback.
    let mut last_failed_outcome: Option<StepOutcome> = None;
    let max_attempts = retry.max_attempts.max(1);
    // Full-jitter backoff (ORB-10006): parallel workers retrying the same
    // failing dependency draw sleeps from independent entropy-backed streams
    // instead of waking in lockstep.
    let mut jitter = JitterRng::from_entropy();
    for attempt in 0..max_attempts {
        match run_step_body(step, ctx) {
            Ok(outcome) => {
                if outcome.success {
                    return Ok(outcome);
                }
                // Treat a "not-success-but-no-error" outcome as retryable:
                // another attempt may succeed. This is the block-level outcome
                // contract, and the CLI agent-loop leaf reaches it
                // for every provider-level failure — timeout, nonzero exit, and
                // the [ORB-10449] step-completion protocol violation.
                last_err = None;
                last_failed_outcome = Some(outcome);
            }
            Err(err) if err.is_non_retryable() => {
                emit_denied_if_applicable(&err, &step.id, &ctx.audit, ctx.task_id());
                return if err.allows_recovery() {
                    recover_or_return_original(step, ctx, err, attempt + 1, max_attempts)
                } else {
                    Err(err)
                };
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
        if attempt + 1 >= max_attempts {
            break;
        }
        // `compute_backoff_ms` yields the deterministic cap-growth bound;
        // the actual sleep is a full-jitter draw in [0, bound].
        let backoff_ms = jitter.full_jitter(compute_backoff_ms(retry, attempt));
        emit_job_event_lossy(
            &ctx.audit,
            ctx.task_id(),
            V2AuditEventKind::StepRetry {
                step_id: step.id.clone(),
                attempt: attempt + 1,
                next_backoff_ms: backoff_ms,
            },
        );
        thread::sleep(Duration::from_millis(backoff_ms));
    }

    match last_err {
        Some(err) => recover_or_return_original(step, ctx, err, max_attempts, max_attempts),
        None => Ok(last_failed_outcome.unwrap_or(StepOutcome {
            success: false,
            output: Value::Null,
            message: None,
        })),
    }
}

pub(super) fn step_fs_profile(step: &JobV2Step) -> Option<&str> {
    match &step.body {
        JobV2StepBody::Target(target) => target.fs_profile.as_deref(),
        _ => None,
    }
}

pub(super) fn step_activity_name(step: &JobV2Step) -> String {
    match &step.body {
        JobV2StepBody::Target(target) => target
            .activity_name
            .clone()
            .unwrap_or_else(|| target_activity_label(target)),
        JobV2StepBody::TargetRef(target) => target.target.clone(),
        JobV2StepBody::Parallel { .. } => "parallel".to_string(),
        JobV2StepBody::FanOut { .. } => "fan_out".to_string(),
        JobV2StepBody::Loop { .. } => "loop".to_string(),
    }
}

pub(super) fn target_activity_label(target: &TargetStep) -> String {
    match &target.spec {
        ActivityV2Spec::AgentLoop(_) => "agent_loop".to_string(),
        ActivityV2Spec::Deterministic(spec) => spec.action.clone(),
    }
}

pub(super) fn compute_backoff_ms(retry: &RetrySpec, attempt_index: u32) -> u64 {
    match retry.backoff_strategy {
        BackoffStrategy::Exponential => {
            let shifted = retry
                .initial_backoff_ms
                .saturating_mul(1u64 << attempt_index.min(20));
            shifted.min(retry.backoff_cap_ms)
        }
        BackoffStrategy::Linear => retry
            .initial_backoff_ms
            .saturating_mul(attempt_index as u64 + 1)
            .min(retry.backoff_cap_ms),
    }
}

pub(super) fn emit_denied_if_applicable(
    err: &DispatchError,
    step_id: &str,
    audit: &Arc<V2AuditWriter>,
    task_id: Option<&str>,
) {
    if matches!(err, DispatchError::ToolDenied { .. }) {
        emit_job_event_lossy(
            audit,
            task_id,
            V2AuditEventKind::StepDenied {
                step_id: step_id.to_string(),
                reason: err.to_string(),
            },
        );
    }
}

pub(super) fn run_step_body(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    match &step.body {
        JobV2StepBody::Target(t) => run_target(step, t, ctx),
        JobV2StepBody::TargetRef(r) => Err(DispatchError::JobValidation(format!(
            "step `{}`: target ref `{}` was not resolved — caller must run \
             `resolve_job_target_refs` at load time before dispatch",
            step.id, r.target
        ))),
        JobV2StepBody::Parallel { parallel } => run_parallel(step, parallel, ctx),
        JobV2StepBody::FanOut { fan_out, fan_in } => run_fan_out(step, fan_out, fan_in, ctx),
        JobV2StepBody::Loop { loop_ } => run_loop(step, loop_, ctx),
    }
}

// ---------------------------------------------------------------------------
// Target
// ---------------------------------------------------------------------------
