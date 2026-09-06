use super::*;

pub fn validate_job(job: &JobV2) -> Result<(), DispatchError> {
    if let Some(name) = &job.recovery_activity
        && job.resolved_recovery_activity.is_none()
    {
        return Err(DispatchError::JobValidation(format!(
            "job recovery_activity `{name}` was not resolved — caller must run \
             `resolve_job_catalog_refs_for_execution` at load time before dispatch"
        )));
    }
    if let Some(name) = &job.failure_activity
        && job.resolved_failure_activity.is_none()
    {
        return Err(DispatchError::JobValidation(format!(
            "job failure_activity `{name}` was not resolved — caller must run \
             `resolve_job_catalog_refs_for_execution` at load time before dispatch"
        )));
    }
    for step in &job.steps {
        validate_step(step)?;
    }
    validate_step_output_readiness(job)?;
    Ok(())
}

/// [ORB-11325] Reject a job whose `when:` / `break_when:` reads
/// `steps.<id>.output` for a step that itself carries a `when:`.
///
/// `condition::evaluate_bool_expr` renders the whole expression before it
/// parses it, so both sides of `&&` / `||` render unconditionally; a step
/// skipped by `when:` records no output at all, so a reference to it fails
/// with `template.rs`'s "no data recorded for step" error — and fails on
/// exactly the branch where the referenced step would have been skipped,
/// which is the branch an author is least likely to exercise first. See
/// design doc §8.2 for the working alternative and the workarounds that do
/// not help.
fn validate_step_output_readiness(job: &JobV2) -> Result<(), DispatchError> {
    let mut carries_when = HashMap::new();
    for step in &job.steps {
        record_step_when(step, &mut carries_when);
    }
    for step in &job.steps {
        check_step_output_refs(step, &carries_when)?;
    }
    Ok(())
}

fn record_step_when(step: &JobV2Step, carries_when: &mut HashMap<String, bool>) {
    carries_when.insert(step.id.clone(), step.when.is_some());
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                record_step_when(branch, carries_when);
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => record_step_when(&fan_out.worker, carries_when),
        JobV2StepBody::Loop { loop_ } => {
            for body in &loop_.steps {
                record_step_when(body, carries_when);
            }
        }
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => {}
    }
}

fn check_step_output_refs(
    step: &JobV2Step,
    carries_when: &HashMap<String, bool>,
) -> Result<(), DispatchError> {
    if let Some(expr) = &step.when {
        check_expr_output_refs(&step.id, expr, carries_when)?;
    }
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                check_step_output_refs(branch, carries_when)?;
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            check_step_output_refs(&fan_out.worker, carries_when)?;
        }
        JobV2StepBody::Loop { loop_ } => {
            if let Some(expr) = &loop_.break_when {
                check_expr_output_refs(&step.id, expr, carries_when)?;
            }
            for body in &loop_.steps {
                check_step_output_refs(body, carries_when)?;
            }
        }
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => {}
    }
    Ok(())
}

fn check_expr_output_refs(
    referencing_step: &str,
    expr: &str,
    carries_when: &HashMap<String, bool>,
) -> Result<(), DispatchError> {
    for referenced_step in output_step_refs(expr) {
        if carries_when.get(&referenced_step).copied().unwrap_or(false) {
            return Err(DispatchError::JobValidation(format!(
                "step `{referencing_step}` reads `steps.{referenced_step}.output`, but step \
                 `{referenced_step}` carries its own `when:` and may be skipped — a `when:` or \
                 `break_when:` condition may only read the output of a step that always runs"
            )));
        }
    }
    Ok(())
}

/// Extract every `steps.<id>.output...` reference from the `{{ }}` template
/// tokens in a `when:` / `break_when:` expression, mirroring the token shape
/// `template::resolve_token` parses at render time.
fn output_step_refs(expr: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut remaining = expr;
    while let Some(start) = remaining.find("{{") {
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find("}}") else {
            break;
        };
        let token = after_start[..end].trim();
        let mut parts = token.split('.');
        if parts.next() == Some("steps")
            && let (Some(step_id), Some("output")) = (parts.next(), parts.next())
        {
            refs.push(step_id.to_string());
        }
        remaining = &after_start[end + 2..];
    }
    refs
}

/// [ORB-10385] Reject a job whose resolved activities name deterministic
/// actions the executing runtime cannot dispatch.
///
/// Catalog assets and the installed binary are independently versioned: a
/// workspace can load an activity introduced by a newer source tree while the
/// running binary has no arm for its `action:`. Without this pass the skew
/// only surfaces when the activity is finally dispatched — which, for a
/// terminal `failure_activity`, is after the job admitted a task, built a
/// worktree, and spent an agent run on it, stranding the candidate. Checking
/// every reachable action up front means the run fails before
/// `worktree_setup` performs workflow admission.
///
/// Unknown actions are never skipped: a miss is a hard
/// [`DispatchError::DeterministicActionUnavailable`] naming both the activity
/// and the action.
///
/// The scan covers the job's `recovery_activity` and `failure_activity`, every
/// step's `recovery_activity`, and every resolved deterministic target —
/// recursing through `parallel:`, `fan_out:`, and `loop:` bodies. `agent_loop`
/// activities have no action to check, and an unresolved `TargetRef` is left
/// to the structural error the dispatcher already raises.
pub fn validate_job_deterministic_actions(
    job: &JobV2,
    host: &dyn RuntimeHost,
) -> Result<(), DispatchError> {
    check_activity_action(
        job.resolved_recovery_activity.as_ref().map(|a| &a.spec),
        || activity_label(job.recovery_activity.as_deref(), "job recovery_activity"),
        host,
    )?;
    check_activity_action(
        job.resolved_failure_activity.as_ref().map(|a| &a.spec),
        || activity_label(job.failure_activity.as_deref(), "job failure_activity"),
        host,
    )?;
    for step in &job.steps {
        validate_step_deterministic_actions(step, host)?;
    }
    Ok(())
}

fn validate_step_deterministic_actions(
    step: &JobV2Step,
    host: &dyn RuntimeHost,
) -> Result<(), DispatchError> {
    check_activity_action(
        step.resolved_recovery_activity.as_ref().map(|a| &a.spec),
        || activity_label(step.recovery_activity.as_deref(), &step.id),
        host,
    )?;

    match &step.body {
        JobV2StepBody::Target(target) => check_activity_action(
            Some(&target.spec),
            || activity_label(target.activity_name.as_deref(), &step.id),
            host,
        )?,
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                validate_step_deterministic_actions(branch, host)?;
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            validate_step_deterministic_actions(&fan_out.worker, host)?;
        }
        JobV2StepBody::Loop { loop_ } => {
            for body in &loop_.steps {
                validate_step_deterministic_actions(body, host)?;
            }
        }
        JobV2StepBody::TargetRef(_) => {
            // Unresolved refs are already a structural error in `run_step_body`;
            // there is no spec here to read an action from.
        }
    }
    Ok(())
}

fn check_activity_action(
    spec: Option<&ActivityV2Spec>,
    label: impl Fn() -> String,
    host: &dyn RuntimeHost,
) -> Result<(), DispatchError> {
    let Some(ActivityV2Spec::Deterministic(deterministic)) = spec else {
        return Ok(());
    };
    if host.has_deterministic_action(&deterministic.action) {
        return Ok(());
    }
    Err(DispatchError::DeterministicActionUnavailable {
        activity: label(),
        action: deterministic.action.clone(),
    })
}

/// Prefer the catalog activity name. An inline spec has none, so fall back to
/// the owning step id — still a unique locator inside the job, and it keeps
/// the rendered diagnostic free of nested backticks.
fn activity_label(catalog_name: Option<&str>, fallback: &str) -> String {
    catalog_name.map_or_else(|| fallback.to_string(), ToString::to_string)
}

pub(super) fn validate_step(step: &JobV2Step) -> Result<(), DispatchError> {
    if let Some(name) = &step.recovery_activity
        && step.resolved_recovery_activity.is_none()
    {
        return Err(DispatchError::JobValidation(format!(
            "step `{}` recovery_activity `{name}` was not resolved — caller must run \
             `resolve_job_catalog_refs_for_execution` at load time before dispatch",
            step.id
        )));
    }

    if let Some(retry) = &step.retry {
        validate_retry_spec(&step.id, retry)?;
    }

    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                validate_step(branch)?;
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            validate_step(&fan_out.worker)?;
        }
        JobV2StepBody::Loop { loop_ } => {
            for body in &loop_.steps {
                validate_step(body)?;
            }
        }
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => {}
    }
    Ok(())
}

/// Enforce `retry:` block invariants before any step executes (ORB-10006).
///
/// - `max_attempts >= 1` — zero attempts is a contradiction (the executor
///   would silently clamp it to one, hiding the config error).
/// - `initial_backoff_ms >= 1` — a zero base stays zero under exponential
///   growth, degenerating into a hot retry loop.
/// - `backoff_cap_ms >= initial_backoff_ms` — an inverted cap silently
///   truncates every sleep to the cap.
pub(super) fn validate_retry_spec(step_id: &str, retry: &RetrySpec) -> Result<(), DispatchError> {
    if retry.max_attempts == 0 {
        return Err(DispatchError::RetryConfigInvalid {
            step_id: step_id.to_string(),
            field: "max_attempts",
            value: u64::from(retry.max_attempts),
            invariant: "max_attempts >= 1".to_string(),
        });
    }
    if retry.initial_backoff_ms == 0 {
        return Err(DispatchError::RetryConfigInvalid {
            step_id: step_id.to_string(),
            field: "initial_backoff_ms",
            value: retry.initial_backoff_ms,
            invariant: "initial_backoff_ms >= 1".to_string(),
        });
    }
    if retry.backoff_cap_ms < retry.initial_backoff_ms {
        return Err(DispatchError::RetryConfigInvalid {
            step_id: step_id.to_string(),
            field: "backoff_cap_ms",
            value: retry.backoff_cap_ms,
            invariant: format!(
                "backoff_cap_ms >= initial_backoff_ms ({})",
                retry.initial_backoff_ms
            ),
        });
    }
    Ok(())
}
