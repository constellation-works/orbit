use orbit_types::workflow::FINAL_RECOVERY_CREWS_KEY;

use super::super::*;
use super::types;

/// The one task this run carries, from `task_id` or a one-element `task_ids`.
pub(super) fn single_task_id(input: &Value) -> Option<String> {
    let non_empty = |value: &Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    };
    match input.get("task_ids").and_then(Value::as_array) {
        Some(ids) if ids.len() == 1 => non_empty(&ids[0]),
        Some(ids) if !ids.is_empty() => None,
        _ => input.get("task_id").and_then(non_empty),
    }
}

/// The worktree the earliest completed step before the failure reported:
/// the first output carrying a non-empty `workspace_path`.
pub(super) fn run_worktree(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step_index: usize,
) -> Option<types::RunWorktree> {
    let pipeline = ctx.pipeline_snapshot();
    let text = |output: &Value, key: &str| {
        output
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    job.steps[..step_index].iter().find_map(|step| {
        let output = pipeline.get(&step.id)?.get("output")?;
        Some(types::RunWorktree {
            workspace_path: text(output, "workspace_path")?,
            base_ref: text(output, "base_ref"),
            base_sha: text(output, "base_sha"),
        })
    })
}

pub(super) enum FinalRecoveryInput {
    Ready(Value),
    Wait(String),
}

pub(super) fn final_recovery_input(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step_index: usize,
    task_id: &str,
    worktree: &types::RunWorktree,
    error_message: &str,
) -> Result<FinalRecoveryInput, String> {
    let step = &job.steps[step_index];
    let mut input = serde_json::json!({
        "task_id": task_id,
        "run_id": ctx.run_id,
        "workspace_path": worktree.workspace_path,
        "repo_root": worktree.workspace_path,
        "failed_step_id": step.id,
        "activity_name": step_activity_name(step),
        "error_message": bounded_recovery_text(
            "error_message",
            &ctx.run_id,
            error_message,
            MAX_RECOVERY_ERROR_MESSAGE_BYTES,
        ),
        "step_outputs": bounded_recovery_input(&ctx.run_id, ctx.pipeline_value()),
        "step_ids": job.steps.iter().map(|step| step.id.clone()).collect::<Vec<_>>(),
        "allowed_resume_step_ids": job.steps[1..=step_index]
            .iter().map(|step| step.id.clone()).collect::<Vec<_>>(),
        "crew_config_key": FINAL_RECOVERY_CREWS_KEY,
    });
    recovery::evidence::inject_final_recovery_evidence(ctx, &mut input)?;
    if let JobV2StepBody::Target(target) = &step.body {
        let completion = matches!(&target.spec, ActivityV2Spec::Deterministic(spec)
            if spec.action == "pr_complete");
        let rendered = render_input(
            target.default_input.as_ref(),
            &ctx.input,
            &ctx.template_ctx(),
            target.input_schema_json.as_ref(),
        );
        if completion {
            let rendered = rendered.as_ref().map_err(|error| {
                format!("completion recovery input cannot be rendered: {error}")
            })?;
            if rendered.get("no_diff_expected").and_then(Value::as_bool) != Some(true) {
                let checks = crate::executor::automation::vcs::completion_recovery_checks(
                    ctx.host,
                    std::path::Path::new(&worktree.workspace_path),
                    rendered,
                    task_id,
                )?;
                let checks = match checks {
                    crate::executor::automation::vcs::CompletionRecoveryChecks::Ready(checks) => {
                        checks
                    }
                    crate::executor::automation::vcs::CompletionRecoveryChecks::Wait(reason) => {
                        return Ok(FinalRecoveryInput::Wait(reason));
                    }
                };
                if checks.to_string().len() > MAX_RECOVERY_ERROR_MESSAGE_BYTES {
                    return Err("completion recovery check evidence exceeds its input budget; refusing an incomplete diagnosis".into());
                }
                input["required_check_failures"] = checks;
            }
        }
        if let Ok(rendered) = rendered {
            input["failed_step_input"] = bounded_recovery_input(&ctx.run_id, rendered);
        }
    }
    for (key, value) in [
        ("base_ref", &worktree.base_ref),
        ("base_sha", &worktree.base_sha),
    ] {
        if let Some(value) = value {
            input[key] = Value::String(value.clone());
        }
    }
    Ok(FinalRecoveryInput::Ready(input))
}

/// A compound failure keeps the nested outputs its resumed phase must repair.
pub(super) fn refused_step_output(ctx: &ExecCtx<'_>, step: &JobV2Step) -> Value {
    let outputs = ctx.pipeline_snapshot();
    if let Some(output) = outputs.get(&step.id) {
        return unwrap_step_output(output);
    }
    let mut keys = Vec::new();
    super::super::execute::collect_step_keys(step, &mut keys);
    let nested = keys
        .into_iter()
        .filter_map(|key| {
            outputs
                .get(key)
                .map(|output| (key.to_string(), unwrap_step_output(output)))
        })
        .collect::<serde_json::Map<_, _>>();
    if nested.is_empty() {
        Value::Null
    } else {
        Value::Object(nested)
    }
}
