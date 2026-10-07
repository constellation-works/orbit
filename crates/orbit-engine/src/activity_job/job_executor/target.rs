use super::reviewer::ReviewerInvocation;
use super::*;

pub(super) fn run_target(
    step: &JobV2Step,
    t: &TargetStep,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    // Scoped so the shared step snapshot is released before this step's own
    // output is recorded, letting `record_pipeline` write in place.
    let rendered_input = {
        let tctx = ctx.template_ctx();
        render_input(
            t.default_input.as_ref(),
            &ctx.input,
            &tctx,
            t.input_schema_json.as_ref(),
        )?
    };
    // [ORB-10902] Rebind before dispatch so `system_crew: true` reaches the
    // activity input, not only the local copy used to resolve crew settings.
    // Recovery does the same; injection is independent of target spec type.
    let rendered_input = inject_system_crew_input(ctx.host, &rendered_input)?;

    // A rendered activity `crew` or `crew_config_key` selects an assignment;
    // otherwise dispatch inherits the run's resolved crew.
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
    let Some(mut dispatched) = dispatch_once(step, t, ctx, dispatched_spec, &rendered_input)?
    else {
        return Err(DispatchError::DeterministicActionRefused {
            action: step.id.clone(),
            message: "review_minutes_exhausted: the candidate's review already spent its \
                      review.minutes; no reviewer is started"
                .to_string(),
        });
    };
    let mut dispatched_input = rendered_input;
    // [ORB-14616] A reviewer whose report settlement would refuse only for
    // its shape is asked once, in the same attempt, to correct the report
    // before the verdict settles. With no minutes left for that, settlement
    // judges the report as it stands.
    if dispatched.reviewer
        && !dispatched.timed_out
        && dispatched.outcome.success
        && let Some(correction) = report_correction(ctx, &dispatched_input)
    {
        let mut corrected_input = dispatched_input.clone();
        if let Some(object) = corrected_input.as_object_mut() {
            object.insert("report_correction".to_string(), Value::String(correction));
        }
        let corrected = dispatch_once(step, t, ctx, dispatched_spec, &corrected_input);
        if !matches!(corrected, Ok(None)) {
            persist_dispatch_invocation(ctx, &step.id, &dispatched_input, &dispatched.outcome);
        }
        if let Some(corrected) = corrected? {
            dispatched = corrected;
            dispatched_input = corrected_input;
        }
    }
    let Dispatched {
        outcome: dispatch,
        timed_out,
        ..
    } = dispatched;
    persist_dispatch_invocation(ctx, &step.id, &dispatched_input, &dispatch);
    record_pipeline(ctx, &step.id, dispatch.output.clone());
    if timed_out {
        return Err(DispatchError::DeterministicActionRefused {
            action: step.id.clone(),
            message: "review_timeout_incomplete: reviewer exceeded its wall clock; partial report retained for continuation".into(),
        });
    }
    let (success, message) = apply_implementer_blocker(
        step,
        t,
        dispatch.success,
        dispatch.message,
        &dispatch.output,
    );
    Ok(StepOutcome {
        success,
        output: dispatch.output,
        message,
    })
}

/// One dispatch of the step's activity.
struct Dispatched {
    outcome: super::super::dispatcher::DispatchOutcome,
    timed_out: bool,
    /// The dispatch was the reviewer of an admitted attempt.
    reviewer: bool,
}

/// Dispatch the step's activity once. A reviewer's start and end are
/// reported around it, and its wall clock is bounded by the review's
/// remaining minutes; `None` when those minutes are already spent, so no
/// reviewer is started.
fn dispatch_once(
    step: &JobV2Step,
    t: &TargetStep,
    ctx: &ExecCtx<'_>,
    spec: &ActivityV2Spec,
    input: &Value,
) -> Result<Option<Dispatched>, DispatchError> {
    let mut reviewer = ReviewerInvocation::start(ctx, t, spec, input);
    if let Some(reviewer) = reviewer.take_if(|reviewer| reviewer.exhausted()) {
        reviewer.finish(ctx, false);
        return Ok(None);
    }
    let bounded_spec = reviewer
        .as_ref()
        .and_then(|reviewer| reviewer.bounded_spec(spec));
    let spec = bounded_spec.as_ref().unwrap_or(spec);
    // Events keep the step id; policy and broker identity use the catalog
    // activity the step targets (a step `review` runs `agent_review_repair`).
    let dispatch = dispatch_v2_target_activity(
        V2DispatchInput {
            activity_name: &step.id,
            spec,
            fs_profile: t.fs_profile.as_deref(),
            input: input.clone(),
            audit: ctx.audit.clone(),
            run_id: &ctx.run_id,
            host: Some(ctx.host),
        },
        t.activity_name.as_deref(),
    );
    let is_reviewer = reviewer.is_some();
    let timed_out = is_reviewer
        && dispatch.as_ref().is_ok_and(|outcome| {
            outcome.output.get("timed_out").and_then(Value::as_bool) == Some(true)
        });
    if let Some(reviewer) = reviewer {
        reviewer.finish(ctx, timed_out);
    }
    Ok(Some(Dispatched {
        outcome: dispatch?,
        timed_out,
        reviewer: is_reviewer,
    }))
}

/// The defect the host finds in the report of the reviewer that just
/// returned, when the reviewer can still correct it [ORB-14616]. A failed
/// check is logged and leaves the report to settlement.
fn report_correction(ctx: &ExecCtx<'_>, input: &Value) -> Option<String> {
    let field = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    let request = crate::context::ReviewReportCorrectionRequest {
        run_id: ctx.run_id.clone(),
        lineage_key: field("lineage_key")?,
        attempt_id: field("attempt_id")?,
        task_ids: input
            .get("task_ids")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        workspace_path: field("workspace_path")?.into(),
    };
    ctx.host
        .review_report_correction(&request)
        .unwrap_or_else(|error| {
            tracing::warn!(
                target: "orbit.engine.job_executor",
                run_id = %request.run_id,
                attempt_id = %request.attempt_id,
                error = %error,
                "could not check the reviewer's report before settlement"
            );
            None
        })
}

/// Turn a well-formed `blocker` on an implementer step into
/// [`TASK_BLOCKED_BY_AGENT_ERROR_CODE`](orbit_types::workflow::TASK_BLOCKED_BY_AGENT_ERROR_CODE).
///
/// Detection is here, before retry and recovery see the outcome. A malformed
/// blocker stays the dispatch's own outcome. The step id is `implement_one`
/// in the shipped pipelines; `activity_name` covers a resolved
/// `agent_implement` target whose id differs.
fn apply_implementer_blocker(
    step: &JobV2Step,
    target: &TargetStep,
    success: bool,
    message: Option<String>,
    output: &Value,
) -> (bool, Option<String>) {
    if !is_implementer_step(step, target) {
        return (success, message);
    }
    let Some(blocker) = orbit_types::workflow::agent_blocker_from_output(output) else {
        return (success, message);
    };
    (
        false,
        Some(orbit_types::workflow::task_blocked_by_agent_message(
            &blocker,
        )),
    )
}

fn is_implementer_step(step: &JobV2Step, target: &TargetStep) -> bool {
    step.id == "implement_one" || target.activity_name.as_deref() == Some("agent_implement")
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
/// `crew` or `crew_config_key` selects the activity crew; otherwise the run
/// input supplies the resolved fallback crew.
pub(super) fn crew_overridden_spec(
    t: &TargetStep,
    ctx: &ExecCtx<'_>,
    rendered_input: &Value,
) -> Result<Option<AgentLoopSpec>, DispatchError> {
    let ActivityV2Spec::AgentLoop(inline_spec) = &t.spec else {
        return Ok(None);
    };
    let rendered_input = inject_system_crew_input(ctx.host, rendered_input)?;
    // Crew selection precedes dispatch. A stateful pool draw needs the same
    // executing identity here that dispatch supplies when `run_id` names a
    // different, originating run (such as a failed follower claim).
    let rendered_input = super::super::dispatcher::inject_run_id(&rendered_input, &ctx.run_id);
    let Some(resolved) = resolve_crew_settings(ctx.host, inline_spec, &rendered_input, &ctx.input)?
    else {
        return Ok(None);
    };
    let mut spec = inline_spec.clone();
    apply_resolved_settings(&mut spec, &resolved);
    Ok(Some(spec))
}
