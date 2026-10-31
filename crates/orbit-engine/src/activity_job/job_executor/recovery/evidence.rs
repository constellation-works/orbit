//! Bounded run evidence for built-in step and final recovery activities.

use super::*;
use orbit_types::identity::lower_hex;
use sha2::{Digest, Sha256};

/// Project the current run's audit attempts and worker-log tail without
/// granting an agent access to operator-only run observation.
pub(in crate::activity_job::job_executor) fn inject_recovery_evidence(
    ctx: &ExecCtx<'_>,
    input: &mut Value,
) -> Result<(), String> {
    inject_run_recovery_evidence(ctx, input, false)
}

/// Final recovery retains recent details and failure signatures when the
/// complete attempt history cannot fit; step recovery still refuses overflow.
pub(in crate::activity_job::job_executor) fn inject_final_recovery_evidence(
    ctx: &ExecCtx<'_>,
    input: &mut Value,
) -> Result<(), String> {
    inject_run_recovery_evidence(ctx, input, true)
}

fn inject_run_recovery_evidence(
    ctx: &ExecCtx<'_>,
    input: &mut Value,
    retain_latest: bool,
) -> Result<(), String> {
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
    input["step_recovery_attempts"] = Value::Array(step_recovery_attempts(ctx, retain_latest)?);
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
fn step_recovery_attempts(ctx: &ExecCtx<'_>, retain_latest: bool) -> Result<Vec<Value>, String> {
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
            Some(serde_json::json!({
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
            }))
        })
        .collect::<Vec<_>>();
    let bounded_attempts = attempts
        .iter()
        .cloned()
        .map(|attempt| bounded_recovery_input(&ctx.run_id, attempt))
        .collect::<Vec<_>>();
    // Preserve the existing under-budget representation and step-recovery
    // refusal contract. Only final recovery may summarize overflowing history.
    match bounded_recovery_input(&ctx.run_id, Value::Array(bounded_attempts.clone())) {
        Value::Array(attempts) => Ok(attempts),
        _ if retain_latest => Ok(retain_final_recovery_attempts(
            &ctx.run_id,
            &attempts,
            bounded_attempts,
        )),
        _ => Err("step-recovery evidence exceeds the bounded recovery input; an operator must inspect the full audit trail".to_string()),
    }
}

/// Keep signatures separate from detail retention so dropping old outputs does
/// not conceal repeated failures. Both projections are ordered newest first
/// while selecting, then the detailed suffix is emitted chronologically.
fn retain_final_recovery_attempts(
    run_id: &str,
    original: &[Value],
    mut attempts: Vec<Value>,
) -> Vec<Value> {
    let limit = MAX_RECOVERY_ERROR_MESSAGE_BYTES;
    let signature_budget = limit * 3 / 4;
    let mut signatures = recovery_failure_signatures(run_id, original);
    if serde_json::json!(&signatures).to_string().len() > signature_budget {
        for signature in &mut signatures {
            *signature = serde_json::json!({
                "sha256": signature["sha256"],
                "count": signature["count"],
            });
        }
    }
    let mut omitted_signatures = 0;
    let mut omitted_signature_attempts = 0;
    let mut signature_bytes = serde_json::json!(&signatures).to_string().len();
    while signature_bytes > signature_budget {
        let Some(signature) = signatures.pop() else {
            break;
        };
        signature_bytes = signature_bytes.saturating_sub(signature.to_string().len() + 1);
        omitted_signatures += 1;
        omitted_signature_attempts += signature["count"].as_u64().unwrap_or_default();
    }
    let mut summary = serde_json::json!({
        "truncated": true,
        "run_id": run_id,
        "audit_reference": {
            "run_id": run_id,
            "event_kinds": ["step.recovery_attempted", "step.post_recovery_attempt"],
        },
        "original_serialized_bytes": serde_json::json!(original).to_string().len(),
        "omitted_attempts": original.len(),
        "failure_signatures": signatures,
        "omitted_failure_signatures": omitted_signatures,
        "omitted_signature_attempts": omitted_signature_attempts,
    });
    // Reserve the maximum-width omission count, so shrinking it cannot push
    // the final serialized array past its bound. Count brackets and commas,
    // including JSON escaping rather than just the raw string lengths.
    let mut bytes = summary.to_string().len() + 2;
    let mut retained = Vec::new();
    while let Some(attempt) = attempts.pop() {
        let size = attempt.to_string().len() + 1;
        if bytes + size > limit {
            break;
        }
        bytes += size;
        retained.push(attempt);
    }
    summary["omitted_attempts"] = Value::from(original.len() - retained.len());
    let mut evidence = vec![summary];
    evidence.extend(retained.into_iter().rev());
    evidence
}

fn recovery_failure_signatures(run_id: &str, attempts: &[Value]) -> Vec<Value> {
    let mut indices = std::collections::HashMap::<String, usize>::new();
    let mut signatures = Vec::<Value>::new();
    for attempt in attempts.iter().rev() {
        let mut signature = serde_json::json!({
            "failed_step_id": attempt["failed_step_id"],
            "activity": attempt["activity"],
            "phase": attempt["phase"],
            "outcome": attempt["outcome"],
            "failure_phase": attempt["failure_phase"],
            "error_message": attempt["error_message"],
        });
        let identity = lower_hex(Sha256::digest(signature.to_string().as_bytes()));
        if let Some(index) = indices.get(&identity) {
            let count = signatures[*index]["count"].as_u64().unwrap_or_default();
            signatures[*index]["count"] = Value::from(count + 1);
        } else {
            if let Some(error) = signature["error_message"].as_str() {
                signature["error_message"] = Value::String(bounded_recovery_text(
                    "recovery failure signature",
                    run_id,
                    error,
                    256,
                ));
            }
            signature["sha256"] = Value::String(identity.clone());
            signature["count"] = Value::from(1);
            indices.insert(identity, signatures.len());
            signatures.push(signature);
        }
    }
    signatures
}
