//! Bounded run evidence for built-in step and final recovery activities.

use super::*;

/// Project the current run's audit attempts and worker-log tail without
/// granting an agent access to operator-only run observation.
pub(super) fn inject_recovery_evidence(ctx: &ExecCtx<'_>, input: &mut Value) -> Result<(), String> {
    let log_tail = ctx
        .host
        .final_recovery_log_tail(&ctx.run_id)
        .map_err(|error| format!("read recovery log tail: {error}"))?
        .unwrap_or_default();
    let log_tail = &log_tail[orbit_common::text::ceil_char_boundary(
        &log_tail,
        log_tail
            .len()
            .saturating_sub(MAX_RECOVERY_ERROR_MESSAGE_BYTES),
    )..];
    input["step_recovery_attempts"] = Value::Array(step_recovery_attempts(ctx)?);
    input["log_tail"] = Value::String(bounded_recovery_text(
        "log_tail",
        &ctx.run_id,
        log_tail,
        MAX_RECOVERY_ERROR_MESSAGE_BYTES,
    ));
    Ok(())
}

/// Keep each recovery and post-recovery record, oldest first, with bounded
/// diagnostics, dispatch output and the read-back recovery decision. Filter
/// the run even when a host's audit sink returns a broader snapshot.
/// Historical records have no output or decision.
fn step_recovery_attempts(ctx: &ExecCtx<'_>) -> Result<Vec<Value>, String> {
    let mut events = ctx
        .audit
        .events_snapshot()
        .map_err(|error| format!("read recovery audit evidence: {error}"))?;
    events.retain(|event| event.envelope.run_id == ctx.run_id);
    events.sort_by(|left, right| {
        left.envelope
            .ts
            .cmp(&right.envelope.ts)
            .then_with(|| left.envelope.event_id.cmp(&right.envelope.event_id))
    });
    let attempts = events
        .into_iter()
        .filter_map(|event| {
            let decision = match &event.kind {
                V2AuditEventKind::StepRecoveryAttempted { decision, .. } => decision.clone(),
                _ => None,
            };
            let (step_id, activity, phase, outcome, failure_phase, error_message, output) =
                match event.kind {
                    V2AuditEventKind::StepRecoveryAttempted {
                        step_id,
                        recovery_activity,
                        recovery_succeeded,
                        failure_phase,
                        error_message,
                        output,
                        ..
                    } => (
                        step_id,
                        recovery_activity,
                        "recovery",
                        if recovery_succeeded {
                            "success"
                        } else {
                            "failed"
                        }
                        .to_string(),
                        failure_phase,
                        error_message,
                        output,
                    ),
                    V2AuditEventKind::StepPostRecoveryAttempt {
                        step_id,
                        recovery_activity,
                        outcome,
                        error_message,
                        output,
                    } => (
                        step_id,
                        recovery_activity,
                        "post_recovery",
                        outcome,
                        None,
                        error_message,
                        output,
                    ),
                    _ => return None,
                };
            Some(bounded_recovery_input(
                &ctx.run_id,
                serde_json::json!({
                    "event_id": event.envelope.event_id,
                    "attempted_at": event.envelope.ts,
                    "failed_step_id": step_id,
                    "activity": activity,
                    "phase": phase,
                    "outcome": outcome,
                    "failure_phase": failure_phase,
                    "error_message": error_message,
                    "output": output,
                    "decision": decision,
                }),
            ))
        })
        .collect::<Vec<_>>();
    // Preserve the array contract and every record. The ordinary leaf bound
    // fits all evidence; a run with too much remaining metadata escalates
    // instead of replacing attempts with a preview or dispatching an
    // unbounded provider envelope.
    match bounded_recovery_input(&ctx.run_id, Value::Array(attempts)) {
        Value::Array(attempts) => Ok(attempts),
        _ => Err("step-recovery evidence exceeds the bounded recovery input; an operator must inspect the full audit trail".to_string()),
    }
}
