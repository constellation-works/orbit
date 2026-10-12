use super::reviewer::ReviewerInvocation;
use super::*;

pub(super) fn run_target(
    step: &JobV2Step,
    t: &TargetStep,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    // Scoped so the shared step snapshot is released before this step's own
    // output is recorded, letting `record_pipeline` write in place.
    let mut rendered_input = {
        let tctx = ctx.template_ctx();
        // Recovery's rationale and refused output are opaque data. An
        // inherited input must not render template tokens inside that data;
        // the engine-owned context is inserted unchanged after rendering.
        let mut inherited_input = ctx.input.clone();
        if let Some(input) = inherited_input.as_object_mut() {
            input.remove("final_recovery_resume");
        }
        render_input(
            t.default_input.as_ref(),
            &inherited_input,
            &tctx,
            t.input_schema_json.as_ref(),
        )?
    };
    if let Some(context) = ctx.input.get("final_recovery_resume")
        && let Some(input) = rendered_input.as_object_mut()
    {
        input.insert("final_recovery_resume".to_string(), context.clone());
    }
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
        outcome: mut dispatch,
        timed_out,
        ..
    } = dispatched;
    persist_dispatch_invocation(ctx, &step.id, &dispatched_input, &dispatch);
    if timed_out {
        record_pipeline(ctx, &step.id, dispatch.output.clone());
        return Err(DispatchError::ReviewGateRefused {
            action: step.id.clone(),
            refusal: orbit_types::workflow::ReviewGateRefusal::TimeoutIncomplete,
            message: "review_timeout_incomplete: reviewer exceeded its wall clock; partial report retained for continuation".into(),
        });
    }
    let (success, message) = apply_implementer_blocker(
        step,
        t,
        dispatch.success,
        dispatch.message.take(),
        &dispatch.output,
    );
    let (success, message) =
        reject_malformed_implementer_findings(step, t, success, message, &dispatch.output);
    let (success, message) = accept_implementer_deferral(
        step,
        t,
        ctx,
        &dispatched_input,
        success,
        message,
        &mut dispatch,
    );
    // Response envelopes belong to provider invocations. A deterministic
    // target already returns its structured output directly and can implement
    // a custom delivery activity without the provider's stdout contract.
    let (success, message) = if success
        && matches!(dispatched_spec, ActivityV2Spec::AgentLoop(_))
        && is_implementer_step(step, t)
    {
        match crate::executor::automation::vcs::require_implementer_no_diff_artifacts(
            ctx.host,
            &dispatched_input,
            &dispatch.output,
        ) {
            Ok(()) => (success, message),
            Err(error) => {
                let diagnostic = format!("response_envelope_error: {error}");
                if let Some(output) = dispatch.output.as_object_mut() {
                    output.insert(
                        "response_envelope_error".to_string(),
                        Value::String(diagnostic.clone()),
                    );
                }
                (false, Some(diagnostic))
            }
        }
    } else {
        (success, message)
    };
    if let Some(output) = dispatch.output.as_object_mut() {
        if let Some(diagnostics) = ctx.resumed_diagnostics.get(&step.id) {
            for key in CONFLICT_DIAGNOSTIC_KEYS {
                if let Some(value) = diagnostics.get(key) {
                    output
                        .entry(key.to_string())
                        .or_insert_with(|| value.clone());
                }
            }
        }
        // A conflict encountered in this execution replaces the resumed
        // diagnostics, including an empty list after a newer conflict.
        let prior = ctx.pipeline.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(prior_output) = prior.get(&step.id).and_then(|step| step.get("output")) {
            for key in CONFLICT_DIAGNOSTIC_KEYS {
                if let Some(diagnostics) = prior_output.get(key) {
                    output.insert(key.to_string(), diagnostics.clone());
                }
            }
        }
    }
    record_pipeline(ctx, &step.id, dispatch.output.clone());
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
/// reported around it, and its wall clock is the deadline the review's
/// remaining minutes allow; `None` when those minutes are already spent, so no
/// reviewer is started.
fn dispatch_once(
    step: &JobV2Step,
    t: &TargetStep,
    ctx: &ExecCtx<'_>,
    spec: &ActivityV2Spec,
    input: &Value,
) -> Result<Option<Dispatched>, DispatchError> {
    let mut reviewer = ReviewerInvocation::start(ctx, t, input);
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
    if let Err(DispatchError::RecoverableVcsConflict(conflict)) = &dispatch {
        let diagnostic = serde_json::json!({
            "conflicting_paths": conflict.conflicting_paths,
            "conflicting_files": conflict.conflicting_files,
        });
        record_pipeline(ctx, &step.id, diagnostic.clone());
        if let Err(error) = ctx
            .host
            .checkpoint_step_diagnostic(&ctx.run_id, &step.id, &diagnostic)
        {
            ctx.audit
                .note_telemetry_failure("step_diagnostic", Some(&step.id), &error);
        }
    }
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

/// Fail an implementer step whose `unfiled_findings` breaks the shape
/// `agent_implement` declares [ORB-14927].
///
/// The engine does not enforce `output_schema_json`, and the claim handoff
/// would otherwise be the first to reject the field, after the candidate is
/// committed, pushed and opened. Failing here keeps those side effects from
/// running; the step's retry and recovery are the repair attempt.
fn reject_malformed_implementer_findings(
    step: &JobV2Step,
    target: &TargetStep,
    success: bool,
    message: Option<String>,
    output: &Value,
) -> (bool, Option<String>) {
    if !success || !is_implementer_step(step, target) {
        return (success, message);
    }
    match orbit_types::workflow::unfiled_findings_shape_error(output) {
        Some(defect) => (
            false,
            Some(format!(
                "{}: {defect}. Return `unfiled_findings` \
                 as `[{{\"title\": ..., \"description\": ...}}]`.",
                orbit_types::workflow::CONTRACT_REFUSAL_MARKER
            )),
        ),
        None => (success, message),
    }
}

/// Derive an implementer step's deferred sandbox validation from the gate
/// summary it names and Orbit's own probes in its lane, failing the step
/// unless it is the sanctioned case: only deferral notices, of sandboxes the
/// lane refuses [ORB-15287] [ORB-15516] [ORB-15692]. The
/// derived record replaces the handoff in the step's output, and an owner run
/// keeps it for the native replay before delivery. Like malformed findings,
/// it fails here, before anything is committed.
fn accept_implementer_deferral(
    step: &JobV2Step,
    target: &TargetStep,
    ctx: &ExecCtx<'_>,
    input: &Value,
    success: bool,
    message: Option<String>,
    dispatch: &mut super::super::dispatcher::DispatchOutcome,
) -> (bool, Option<String>) {
    if !success || !is_implementer_step(step, target) {
        return (success, message);
    }
    let lane = crate::executor::automation::vcs::ImplementerLane {
        provider: dispatch
            .invocation
            .as_ref()
            .map(|invocation| invocation.provider.as_str()),
        fs_profile: target.fs_profile.as_deref(),
    };
    match crate::executor::automation::vcs::accept_implementer_deferral(
        ctx.host,
        &ctx.run_id,
        input,
        &dispatch.output,
        &lane,
    ) {
        Ok(Some(record)) => {
            if let (Some(output), Ok(record)) = (
                dispatch.output.as_object_mut(),
                serde_json::to_value(record),
            ) {
                output.insert("deferred_sandbox_validation".to_string(), record);
            }
            (success, message)
        }
        Ok(None) => (success, message),
        Err(refusal) => (false, Some(refusal)),
    }
}

/// Turn a well-formed `blocker` on an implementer step into
/// [`TASK_BLOCKED_BY_AGENT_ERROR_CODE`](orbit_types::workflow::TASK_BLOCKED_BY_AGENT_ERROR_CODE).
///
/// Detection is here, before retry and recovery see the outcome. A malformed
/// blocker stays the dispatch's own outcome. The step id is `implement_one`
/// in the shipped pipelines; `activity_name` covers a resolved
/// `agent_implement` target whose id differs.
///
/// A blocker of the [upgrade-pending](orbit_types::workflow::AgentBlockerClass::UpgradePending)
/// class fails any agent step, a reviewer's too, under
/// [`UPGRADE_PENDING_ERROR_CODE`](orbit_types::workflow::UPGRADE_PENDING_ERROR_CODE):
/// every `orbit` command the agent runs is refused until the upgrade
/// settles, so nothing the step reports can be trusted.
fn apply_implementer_blocker(
    step: &JobV2Step,
    target: &TargetStep,
    success: bool,
    message: Option<String>,
    output: &Value,
) -> (bool, Option<String>) {
    let Some(blocker) = orbit_types::workflow::agent_blocker_from_output(output) else {
        return (success, message);
    };
    let upgrade = blocker.class() == orbit_types::workflow::AgentBlockerClass::UpgradePending;
    if !upgrade && !is_implementer_step(step, target) {
        return (success, message);
    }
    (false, Some(blocker.step_failure_message()))
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
    // Crew selection precedes dispatch. A stateful pool draw needs the same
    // executing identity here that dispatch supplies when `run_id` names a
    // different, originating run (such as a failed follower claim).
    let rendered_input = super::super::dispatcher::inject_run_id(rendered_input, &ctx.run_id);
    let Some(resolved) = resolve_crew_settings(ctx.host, inline_spec, &rendered_input, &ctx.input)?
    else {
        return Ok(None);
    };
    let mut spec = inline_spec.clone();
    apply_resolved_settings(&mut spec, &resolved);
    Ok(Some(spec))
}
