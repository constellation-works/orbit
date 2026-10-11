//! Bounded recovery inputs and diagnostics.
use super::*;
use orbit_common::text::{ceil_char_boundary, floor_char_boundary};

/// Largest `error_message` the recovery input may carry, in bytes.
///
/// The CLI envelope serialises the recovery input twice — once as `input` and
/// once as the `prompt` rendering of it — and a provider such as `codex exec`
/// rejects the whole turn above 1,048,576 characters before the agent starts.
/// A `primary_checkout_drift` diagnostic reached 2.5 MB, so the envelope was
/// 5.1 MB and recovery exited 1 in 416 ms without running [ORB-12467]. The
/// 64 KiB limits bound diagnostics, rendered inputs and injected run evidence
/// while still showing the agent both ends of an oversized diagnostic.
pub(in crate::activity_job::job_executor) const MAX_RECOVERY_ERROR_MESSAGE_BYTES: usize = 64 * 1024;

/// Largest serialised `failed_step_input` the recovery input may carry.
const MAX_RECOVERY_FAILED_STEP_INPUT_BYTES: usize = 64 * 1024;

/// Largest string leaf kept verbatim inside a bounded `failed_step_input`.
///
/// Rendered target inputs are usually one oversized leaf (a prompt, a diff, a
/// captured log) beside many small ones, so truncating leaves preserves the
/// object shape the recovery activity's input schema requires.
const MAX_RECOVERY_INPUT_LEAF_BYTES: usize = 8 * 1024;

/// Project only execution context from the failed target. Its rendered input
/// retains candidate/PR checkpoints without forwarding unrelated job outputs.
/// Custom recovery activities keep their existing input contract.
pub(super) fn bind_recovery_context(
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

/// Keep the head and tail of an oversized recovery field with an explicit
/// truncation marker. The marker does not instruct an agent to use an
/// operator-only run observer.
///
/// This is not lossy for the operator: the untruncated error is already durable
/// in the run's step record, and a worktree-integrity diagnostic additionally
/// names the audit blob holding its full fingerprints. It is only the copy
/// handed to the recovery agent that is bounded, so the provider accepts the
/// turn at all [ORB-12467].
pub(in crate::activity_job::job_executor) fn bounded_recovery_text(
    field: &str,
    run_id: &str,
    text: &str,
    limit: usize,
) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let marker = format!(
        "\n… [truncated: {field} is {} B; the middle is omitted to bound recovery \
         input for run {run_id}] …\n",
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
pub(in crate::activity_job::job_executor) fn bounded_recovery_input(
    run_id: &str,
    input: Value,
) -> Value {
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

pub(super) fn validate_bound_recovery_context(input: &Value) -> Result<(), DispatchError> {
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

pub(in crate::activity_job::job_executor) fn redacted_recovery_diagnostic(message: &str) -> String {
    use orbit_common::security::redaction::{argv_redactor, redact_sensitive_env_text};

    let redacted = argv_redactor().apply_str(&redact_sensitive_env_text(message));
    let mut bounded: String = redacted.chars().take(4096).collect();
    if bounded.len() < redacted.len() {
        bounded.push('…');
    }
    bounded
}
