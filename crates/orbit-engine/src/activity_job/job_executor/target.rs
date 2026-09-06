// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use super::*;

pub(super) fn run_target(
    step: &JobV2Step,
    t: &TargetStep,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let tctx = ctx.template_ctx();
    let rendered_input = render_input(
        t.default_input.as_ref(),
        &ctx.input,
        &tctx,
        t.input_schema_json.as_ref(),
    )?;
    // [ORB-10902] Rebind before dispatch so `system_crew: true` reaches the
    // activity input, not only the local copy used to resolve crew settings.
    // Recovery does the same; injection is independent of target spec type.
    let rendered_input = inject_system_crew_input(ctx.host, &rendered_input)?;

    // A rendered activity `crew` selects a non-default assignment; otherwise
    // dispatch inherits the run's resolved crew.
    let crew_override = crew_overridden_spec(t, ctx, &rendered_input)?;
    if t.session.is_some() {
        // [ORB-10801] Cross-iteration sessions were provided only by the
        // retired HTTP agent loop. `validate_job_retired_sessions` refuses the
        // asset at load; this is the structural backstop for a job built in
        // memory, which never passes through the loader.
        return Err(DispatchError::JobValidation(format!(
            "step `{}`: `session:` bindings are no longer supported; {}",
            step.id,
            orbit_types::workflow::activity_job::RETIRED_BACKEND_MIGRATION
        )));
    }

    // Swap in the crew-resolved clone when the host has a configuration layer;
    // isolated hosts may retain inline values.
    let dispatched_spec_storage = crew_override
        .as_ref()
        .map(|spec| ActivityV2Spec::AgentLoop(spec.clone()));
    let dispatched_spec = dispatched_spec_storage.as_ref().unwrap_or(&t.spec);
    let dispatch = dispatch_v2_activity(V2DispatchInput {
        activity_name: &step.id,
        spec: dispatched_spec,
        fs_profile: t.fs_profile.as_deref(),
        input: rendered_input.clone(),
        audit: ctx.audit.clone(),
        run_id: &ctx.run_id,
        host: Some(ctx.host),
    })?;
    persist_dispatch_invocation(ctx, &step.id, &rendered_input, &dispatch);
    let out = dispatch.output.clone();
    record_pipeline(ctx, &step.id, out.clone());
    Ok(StepOutcome {
        success: dispatch.success,
        output: out,
        message: dispatch.message,
    })
}

/// Persist the invocation trace for a dispatched step.
///
/// [ORB-10367] **Non-fatal by contract.** This is telemetry: a failed write
/// (schema drift, a locked or unwritable database, a full disk) must never
/// discard agent work that already completed. The failure is logged loudly
/// and surfaced on the run record as `telemetry.persist_failed`; the step's
/// success stays decided solely by its own work.
pub(super) fn persist_dispatch_invocation(
    ctx: &ExecCtx<'_>,
    step_id: &str,
    input: &Value,
    dispatch: &super::super::dispatcher::DispatchOutcome,
) {
    let Some(invocation) = dispatch.invocation.as_ref() else {
        return;
    };

    if let Err(error) = ctx.host.persist_invocation_trace(
        &ctx.run_id,
        step_id,
        &invocation.provider,
        invocation.model.as_deref(),
        input,
        &invocation.trace,
    ) {
        ctx.audit
            .note_telemetry_failure("invocation_trace", Some(step_id), &error);
    }
}

/// Build a crew-overridden clone of an [`AgentLoopSpec`]. An explicit rendered
/// `crew` wins; otherwise the run input supplies the resolved fallback crew.
pub(super) fn crew_overridden_spec(
    t: &TargetStep,
    ctx: &ExecCtx<'_>,
    rendered_input: &Value,
) -> Result<Option<AgentLoopSpec>, DispatchError> {
    let ActivityV2Spec::AgentLoop(inline_spec) = &t.spec else {
        return Ok(None);
    };
    let rendered_input = inject_system_crew_input(ctx.host, rendered_input)?;
    let Some(resolved) = resolve_crew_settings(ctx.host, inline_spec, &rendered_input, &ctx.input)?
    else {
        return Ok(None);
    };
    let mut spec = inline_spec.clone();
    apply_resolved_settings(&mut spec, &resolved);
    Ok(Some(spec))
}
