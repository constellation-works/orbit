use super::*;
use crate::context::StepRecoveryAdmission;

const PR_CONFLICT_RECOVERY_ACTIVITY: &str = "pr_conflict_recovery";

/// Largest `error_message` the recovery input may carry, in bytes.
///
/// The CLI envelope serialises the recovery input twice — once as `input` and
/// once as the `prompt` rendering of it — and a provider such as `codex exec`
/// rejects the whole turn above 1,048,576 characters before the agent starts.
/// A `primary_checkout_drift` diagnostic reached 2.5 MB, so the envelope was
/// 5.1 MB and recovery exited 1 in 416 ms without running [ORB-12467]. Two
/// 64 KiB fields leave the envelope an order of magnitude below that ceiling
/// while still showing the agent both ends of the real diagnostic.
const MAX_RECOVERY_ERROR_MESSAGE_BYTES: usize = 64 * 1024;

/// Largest serialised `failed_step_input` the recovery input may carry.
const MAX_RECOVERY_FAILED_STEP_INPUT_BYTES: usize = 64 * 1024;

/// Largest string leaf kept verbatim inside a bounded `failed_step_input`.
///
/// Rendered target inputs are usually one oversized leaf (a prompt, a diff, a
/// captured log) beside many small ones, so truncating leaves preserves the
/// object shape the recovery activity's input schema requires.
const MAX_RECOVERY_INPUT_LEAF_BYTES: usize = 8 * 1024;

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
        return post_recovery_attempt(step, ctx, &recovery, original_err);
    }

    Err(original_err)
}

fn post_recovery_attempt(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    original_err: DispatchError,
) -> Result<StepOutcome, DispatchError> {
    let reattempt = run_step_body(step, ctx);
    let (outcome, error_message) = match &reattempt {
        Ok(outcome) if outcome.success => ("success", None),
        Ok(outcome) => (
            "failed",
            Some(
                outcome
                    .message
                    .clone()
                    .unwrap_or_else(|| "step completed with success=false".to_string()),
            ),
        ),
        Err(error) => ("error", Some(error.to_string())),
    };
    let error_message = error_message.map(|message| redacted_recovery_diagnostic(&message));
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::StepPostRecoveryAttempt {
            step_id: step.id.clone(),
            recovery_activity: recovery.name.clone(),
            outcome: outcome.to_string(),
            error_message: error_message.clone(),
        },
    );

    match reattempt {
        Ok(outcome) if outcome.success => Ok(outcome),
        Ok(_) | Err(_) => Err(DispatchError::JobExecution(format!(
            "post-recovery attempt {outcome}: {}; original error before recovery: {original_err}",
            error_message.unwrap_or_else(|| "no diagnostic".to_string()),
        ))),
    }
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

    let recovery_started = std::time::Instant::now();
    let result = match ctx.host.authorize_step_recovery(&ctx.run_id, &step.id) {
        Ok(StepRecoveryAdmission::Allowed | StepRecoveryAdmission::Reserved { .. }) => {
            let result =
                dispatch_recovery(step, ctx, recovery, original_err, attempt, max_attempts);
            // Preparation failures spend the reserved episode too.
            if let Err(error) = ctx.host.settle_step_recovery(
                &ctx.run_id,
                &step.id,
                recovery_started.elapsed().as_secs(),
            ) {
                tracing::warn!(
                    target: "orbit.engine.job_executor",
                    run_id = %ctx.run_id,
                    failed_step_id = %step.id,
                    error = %error,
                    "step recovery settlement failed"
                );
            }
            result
        }
        Ok(StepRecoveryAdmission::Denied { reason }) => Err(("authorization", reason)),
        Err(error) => Err(("authorization", error.to_string())),
    };

    let (recovery_succeeded, failure_phase, error_message) = match result {
        Ok(()) => (true, None, None),
        Err((phase, message)) => (
            false,
            Some(phase.to_string()),
            Some(redacted_recovery_diagnostic(&message)),
        ),
    };
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::StepRecoveryAttempted {
            step_id: step.id.clone(),
            recovery_activity: recovery.name.clone(),
            recovery_succeeded,
            failure_phase,
            error_message,
        },
    );
    recovery_succeeded
}

fn dispatch_recovery(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    original_err: &DispatchError,
    attempt: u32,
    max_attempts: u32,
) -> Result<(), (&'static str, String)> {
    let mut input = serde_json::json!({
        "failed_step_id": step.id,
        "activity_name": step_activity_name(step),
        "error_message": bounded_recovery_text(
            "error_message",
            &ctx.run_id,
            &original_err.to_string(),
            MAX_RECOVERY_ERROR_MESSAGE_BYTES,
        ),
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
    ) {
        bind_recovery_context(step, ctx, &mut input)
            .map_err(|error| ("input", error.to_string()))?;
        if recovery.name == "step_failure_recovery" {
            validate_bound_recovery_context(&input)
                .map_err(|error| ("input", error.to_string()))?;
        }
        input["system_crew"] = Value::Bool(true);
    }
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
            if dispatch.success {
                Ok(())
            } else {
                let message = dispatch.message.unwrap_or_else(|| {
                    "recovery activity returned an unsuccessful outcome without a diagnostic"
                        .to_string()
                });
                Err(("activity", message))
            }
        }
        Err(error) => Err(("dispatch", error.to_string())),
    }
}

/// Project only execution context from the failed target. Its rendered input
/// retains candidate/PR checkpoints without forwarding unrelated job outputs.
/// Custom recovery activities keep their existing input contract.
fn bind_recovery_context(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    input: &mut Value,
) -> Result<(), DispatchError> {
    let failed_input = match &step.body {
        JobV2StepBody::Target(target) => render_input(
            target.default_input.as_ref(),
            &ctx.input,
            &ctx.template_ctx(),
            target.input_schema_json.as_ref(),
        )?,
        _ => Value::Null,
    };
    for key in ["task_id", "task_ids", "workspace_path", "repo_root"] {
        if let Some(value) = failed_input.get(key) {
            input[key] = value.clone();
        }
    }
    if input.get("task_id").is_none() && input.get("task_ids").is_none() {
        if let Some(ids) = failed_input
            .get("completed_task_ids")
            .or_else(|| ctx.input.get("task_ids"))
        {
            input["task_ids"] = ids.clone();
        } else if let Some(id) = ctx.input.get("task_id") {
            input["task_id"] = id.clone();
        }
    }
    if input.get("repo_root").is_none()
        && let Some(workspace) = input.get("workspace_path").cloned()
    {
        input["repo_root"] = workspace;
    }
    input["run_id"] = Value::String(ctx.run_id.clone());
    input["failed_step_input"] = bounded_recovery_input(&ctx.run_id, failed_input);
    Ok(())
}

/// Keep the head and tail of an oversized recovery field and name where the
/// whole text is, mirroring `elide_note_error`'s contract for run notes.
///
/// This is not lossy for the operator: the untruncated error is already durable
/// in the run's step record, and a worktree-integrity diagnostic additionally
/// names the audit blob holding its full fingerprints. It is only the copy
/// handed to the recovery agent that is bounded, so the provider accepts the
/// turn at all [ORB-12467].
fn bounded_recovery_text(field: &str, run_id: &str, text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let marker = format!(
        "\n… [truncated: {field} is {} B; the middle is omitted. Full text: \
         `orbit run show {run_id} --json`, field .run.steps[].error_message] …\n",
        text.len()
    );
    // `limit` is measured in KiB and the marker is a single short line, so the
    // budget below cannot underflow into an all-marker result in practice.
    let budget = limit.saturating_sub(marker.len());
    let head_budget = budget * 3 / 4;
    let head_end = floor_char_boundary(text, head_budget);
    let tail_start = ceil_char_boundary(text, text.len() - (budget - head_budget));
    format!("{}{marker}{}", &text[..head_end], &text[tail_start..])
}

/// Bound a rendered failed-target input without changing its JSON shape.
///
/// `failed_step_input` is declared `type: object` by both recovery activities,
/// so the bound truncates oversized string leaves in place rather than
/// replacing the value. An input that is still oversized once every leaf is
/// bounded — thousands of small keys rather than one big one — degrades to a
/// preview object, which is the only case that loses the shape.
fn bounded_recovery_input(run_id: &str, input: Value) -> Value {
    let Ok(serialized) = serde_json::to_string(&input) else {
        return input;
    };
    if serialized.len() <= MAX_RECOVERY_FAILED_STEP_INPUT_BYTES {
        return input;
    }
    let bounded = bound_string_leaves(run_id, input);
    let bounded_len = serde_json::to_string(&bounded).map_or(usize::MAX, |text| text.len());
    if bounded_len <= MAX_RECOVERY_FAILED_STEP_INPUT_BYTES {
        return bounded;
    }
    serde_json::json!({
        "truncated": true,
        "original_serialized_bytes": serialized.len(),
        "preview": bounded_recovery_text(
            "failed_step_input",
            run_id,
            &serialized,
            MAX_RECOVERY_FAILED_STEP_INPUT_BYTES,
        ),
    })
}

fn bound_string_leaves(run_id: &str, value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(bounded_recovery_text(
            "failed_step_input field",
            run_id,
            &text,
            MAX_RECOVERY_INPUT_LEAF_BYTES,
        )),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| bound_string_leaves(run_id, item))
                .collect(),
        ),
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .map(|(key, item)| (key, bound_string_leaves(run_id, item)))
                .collect(),
        ),
        other => other,
    }
}

/// Largest index at or below `index` that splits `text` between characters.
///
/// Stands in for the unstable `str::floor_char_boundary`. Diagnostics are
/// arbitrary text from a failing subprocess, so slicing one mid-codepoint
/// would panic on the very inputs this bound exists to handle.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut end = index;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Smallest index at or above `index` that splits `text` between characters.
fn ceil_char_boundary(text: &str, index: usize) -> usize {
    let mut start = index.min(text.len());
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

fn validate_bound_recovery_context(input: &Value) -> Result<(), DispatchError> {
    let has_task_identity = input
        .get("task_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
        || input
            .get("task_ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| {
                !ids.is_empty()
                    && ids
                        .iter()
                        .all(|id| id.as_str().is_some_and(|id| !id.trim().is_empty()))
            });
    let valid_checkout_field = |field: &str| {
        input
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|path| !path.trim().is_empty())
    };
    if has_task_identity
        && valid_checkout_field("workspace_path")
        && valid_checkout_field("repo_root")
    {
        return Ok(());
    }

    Err(DispatchError::JobExecution(
        "managed step recovery requires a task ID and non-empty workspace_path/repo_root from the failed step; the assigned worktree may not have been created or its context did not render, so recovery refuses primary-checkout or unrestricted execution"
            .to_string(),
    ))
}

fn redacted_recovery_diagnostic(message: &str) -> String {
    use orbit_common::security::redaction::{PatternRedactor, redact_sensitive_env_text};

    let redacted =
        PatternRedactor::with_argv_secrets().apply_str(&redact_sensitive_env_text(message));
    let mut bounded: String = redacted.chars().take(4096).collect();
    if bounded.len() < redacted.len() {
        bounded.push('…');
    }
    bounded
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
