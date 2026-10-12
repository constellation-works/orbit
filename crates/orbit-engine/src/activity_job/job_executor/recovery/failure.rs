//! Terminal failure hook dispatch, preserving the original step error.
use super::*;

/// Invoke the job's terminal failure hook once, preserving the original step
/// error regardless of the hook outcome. ADR-0246 keeps this separate from
/// retry recovery: the hook may publish a recoverable candidate, but it never
/// claims that the failed step succeeded.
pub(in crate::activity_job::job_executor) fn attempt_failure_activity(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    original_err: &DispatchError,
) {
    let Some(failure) = &ctx.failure_activity else {
        return;
    };
    let pipeline = ctx.pipeline_value();
    let error_code = original_err.failure_code();
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
