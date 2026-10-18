use super::*;
use std::collections::HashSet;

/// Rebase steps with a resolved, later host validator on the same candidate.
/// This is deliberately conservative: compound continuations and additional
/// guards cannot prove that validation runs whenever conflict recovery resumes.
pub(super) fn recovery_validation_continuations(
    job: &JobV2,
    resume: Option<&orbit_types::workflow::PipelineState>,
    worker: Option<&orbit_types::tool::WorkerInvocation>,
    run_id: &str,
) -> HashSet<String> {
    let mut proven = HashSet::new();
    for (index, step) in job.steps.iter().enumerate() {
        let Some((rebase, "git_rebase")) = deterministic_target(step) else {
            continue;
        };
        let Some(sync_input) = rebase.default_input.as_ref() else {
            continue;
        };
        for (offset, validator) in job.steps[index + 1..].iter().enumerate() {
            if !matches!(validator.body, JobV2StepBody::Target(_)) {
                // A compound body can overwrite inherited outputs through
                // collect aliases, or validate only one branch/iteration.
                break;
            }
            let Some((target, action)) = deterministic_target(validator) else {
                // A resolved agent target may repair the head; the validator
                // still observes that final head and cannot be bypassed by it.
                continue;
            };
            if !matches!(action, "candidate_validate" | "claim_validate") {
                continue;
            }
            if resume.is_some_and(|state| {
                state.step_states.get(&((index + 1 + offset) as u32))
                    == Some(&orbit_types::workflow::JobRunState::Success)
            }) {
                // A checkpointed validator will be skipped, not rerun on the
                // head produced by this continuation.
                continue;
            }
            if validator.when.is_some()
                && (validator.when != step.when
                    || validator
                        .when
                        .as_deref()
                        .is_some_and(|guard| guard.contains("input.final_recovery_resume")))
            {
                continue;
            }
            let Some(validation_input) = target.default_input.as_ref() else {
                continue;
            };
            let same_binding = |key: &str| {
                sync_input.get(key).is_some_and(|binding| {
                    let nonempty = match binding {
                        Value::String(value) => !value.trim().is_empty(),
                        Value::Array(values) => !values.is_empty(),
                        _ => false,
                    };
                    nonempty
                        && !binding.to_string().contains("input.final_recovery_resume")
                        && validation_input.get(key) == Some(binding)
                })
            };
            let output_binding = |key: &str| {
                validation_input.get(key).and_then(Value::as_str)
                    == Some(format!("{{{{ steps.{}.output.{key} }}}}", step.id).as_str())
            };
            if !same_binding("workspace_path")
                || !output_binding("base_sha")
                || (validation_input.get("base_ref").is_some() && !output_binding("base_ref"))
            {
                continue;
            }
            let bound = if action == "candidate_validate" {
                same_binding("job_run_id") && same_binding("completed_task_ids")
            } else {
                // Claim identity/run authority comes from the worker binding.
                // The publication-pinning and no-diff skip invocations do not
                // execute commands and cannot own conflict recovery's gates.
                worker.is_some_and(|worker| worker.bound_run_id == run_id)
                    && ["prevalidated", "pull_request", "skipped_no_diff_expected"]
                    .iter()
                    .all(|key| validation_input.get(key).is_none())
                    // A carried claim_validate result bypasses observing the
                    // recovered candidate unless revalidation is statically
                    // guaranteed. Dynamic expressions cannot prove that here.
                    && (validation_input.get("carry").is_none_or(Value::is_null)
                        || validation_input.get("revalidate").and_then(Value::as_bool)
                            == Some(true))
                    && ["job_run_id", "completed_task_ids", "task_id"]
                        .iter()
                        .all(|key| validation_input.get(key).is_none() || same_binding(key))
                    // Rebase consumes its prepared base_ref, while claims
                    // independently resolve the sync mode and compare the
                    // observed merge-base to sync_base's exact base_sha.
                    && (sync_input.get("base_sync").is_none()
                        || validation_input.get("base_sync").is_none()
                        || same_binding("base_sync"))
            };
            if bound {
                proven.insert(step.id.clone());
                break;
            }
        }
    }
    proven
}

fn deterministic_target(step: &JobV2Step) -> Option<(&TargetStep, &str)> {
    let JobV2StepBody::Target(target) = &step.body else {
        return None;
    };
    let ActivityV2Spec::Deterministic(spec) = &target.spec else {
        return None;
    };
    Some((target, &spec.action))
}

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
    if let Some(name) = &job.final_recovery_activity
        && job.resolved_final_recovery_activity.is_none()
    {
        return Err(DispatchError::JobValidation(format!(
            "job final_recovery_activity `{name}` was not resolved — caller must run \
             `resolve_job_catalog_refs_for_execution` at load time before dispatch"
        )));
    }
    if let Some(gate) = &job.steering_gate {
        let position = |id: &str| job.steps.iter().position(|step| step.id == id);
        if !matches!(
            (position(&gate.round), position(&gate.before)),
            (Some(round), Some(before)) if round < before
        ) {
            return Err(DispatchError::JobValidation(format!(
                "job steering_gate: `round` (`{}`) must name a top-level step before the \
                 top-level step `before` names (`{}`)",
                gate.round, gate.before
            )));
        }
        if let Some(candidate) = &gate.candidate
            && !matches!(
                (position(candidate), position(&gate.round)),
                (Some(candidate), Some(round)) if candidate < round
            )
        {
            return Err(DispatchError::JobValidation(format!(
                "job steering_gate: `candidate` (`{candidate}`) must name a top-level step \
                 before the top-level step `round` names (`{}`)",
                gate.round
            )));
        }
    }
    for step in &job.steps {
        validate_step(step)?;
    }
    validate_step_output_readiness(job)?;
    Ok(())
}

/// [ORB-11325] Reject a job whose `when:` / `break_when:` reads
/// `steps.<id>.output` for a step that may be skipped.
///
/// `condition::evaluate_bool_expr` renders the whole expression before it
/// parses it, so both sides of `&&` / `||` render unconditionally; a step
/// skipped by `when:` records no output at all, so a reference to it fails
/// with `template.rs`'s "no data recorded for step" error — and fails on
/// exactly the branch where the referenced step would have been skipped,
/// which is the branch an author is least likely to exercise first. See
/// design doc §8.2 for the working alternative and the workarounds that do
/// not help.
///
/// [ORB-11346] "May be skipped" is inherited, not local: `step::run_step`
/// returns before running any body when a guard is false, so every step
/// nested in a `parallel:`, `fan_out:`, or `loop:` block under a
/// `when:`-carrying ancestor is skipped with it and records nothing, however
/// unguarded the nested step looks on its own.
///
/// [ORB-11361] Shared-guard membership is not enough: `when:` is evaluated
/// before the body, so a step does not yet sit under its own id. A container
/// `when:` that reads a nested output, or a step `when:` that reads its own
/// output, is therefore unsafe even though `record_step_guards` stored the
/// reader id on the referenced step. `break_when` is the opposite — it runs
/// after the loop body — so the loop's own guard *is* covering there.
fn validate_step_output_readiness(job: &JobV2) -> Result<(), DispatchError> {
    let mut guards = HashMap::new();
    let mut step_ids = HashSet::new();
    for step in &job.steps {
        record_step_guards(step, &[], &mut guards, &mut step_ids)?;
    }
    for step in &job.steps {
        check_step_output_refs(step, &[], &guards)?;
    }
    Ok(())
}

#[derive(Clone)]
struct OutputGuards {
    step_id: String,
    chain: Vec<String>,
}

/// Every `when:` whose false branch skips this step — the step's own guard
/// plus each enclosing step's, outermost first.
fn guard_chain(step: &JobV2Step, inherited: &[String]) -> Vec<String> {
    let mut chain = inherited.to_vec();
    if step.when.is_some() {
        chain.push(step.id.clone());
    }
    chain
}

fn record_step_guards(
    step: &JobV2Step,
    inherited: &[String],
    guards: &mut HashMap<String, Vec<OutputGuards>>,
    step_ids: &mut HashSet<String>,
) -> Result<(), DispatchError> {
    if !step_ids.insert(step.id.clone()) {
        return Err(DispatchError::JobValidation(format!(
            "duplicate step id `{}` — step ids must be unique throughout the job",
            step.id
        )));
    }
    let chain = guard_chain(step, inherited);
    let output_guards = OutputGuards {
        step_id: step.id.clone(),
        chain: chain.clone(),
    };
    // Collect aliases share the output namespace with step ids. Keep every
    // producer's guards so a later writer cannot hide a skippable output.
    let aliases = match &step.body {
        JobV2StepBody::FanOut { fan_in, .. } => {
            [fan_in.collect.as_deref(), fan_in.errors.as_deref()]
        }
        _ => [None, None],
    };
    for alias in aliases
        .into_iter()
        .flatten()
        .filter(|alias| *alias != step.id)
    {
        guards
            .entry(alias.to_string())
            .or_default()
            .push(output_guards.clone());
    }
    guards
        .entry(step.id.clone())
        .or_default()
        .push(output_guards);
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                record_step_guards(branch, &chain, guards, step_ids)?;
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            record_step_guards(&fan_out.worker, &chain, guards, step_ids)?;
        }
        JobV2StepBody::Loop { loop_ } => {
            for body in &loop_.steps {
                record_step_guards(body, &chain, guards, step_ids)?;
            }
        }
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => {}
    }
    Ok(())
}

fn check_step_output_refs(
    step: &JobV2Step,
    inherited: &[String],
    guards: &HashMap<String, Vec<OutputGuards>>,
) -> Result<(), DispatchError> {
    let chain = guard_chain(step, inherited);
    if let Some(expr) = &step.when {
        // `when:` is evaluated before the body (`step.rs`), so this step's
        // own id is not yet a covering guard: nested outputs have not run,
        // and the step cannot read its own output. Only enclosing guards
        // skip the reader together with the referenced step.
        check_expr_output_refs(&step.id, expr, inherited, guards)?;
    }
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                check_step_output_refs(branch, &chain, guards)?;
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            check_step_output_refs(&fan_out.worker, &chain, guards)?;
        }
        JobV2StepBody::Loop { loop_ } => {
            if let Some(expr) = &loop_.break_when {
                // `break_when` is evaluated after the loop body
                // (`loop_block.rs`), so nested body outputs exist whenever
                // the loop itself ran. The loop's own `when:` is a covering
                // guard here — unlike the pre-body `when:` check above.
                check_expr_output_refs(&step.id, expr, &chain, guards)?;
            }
            for body in &loop_.steps {
                check_step_output_refs(body, &chain, guards)?;
            }
        }
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => {}
    }
    Ok(())
}

fn check_expr_output_refs(
    referencing_step: &str,
    expr: &str,
    reader_guards: &[String],
    guards: &HashMap<String, Vec<OutputGuards>>,
) -> Result<(), DispatchError> {
    for referenced_step in output_step_refs(expr) {
        let Some(producers) = guards.get(&referenced_step) else {
            continue;
        };
        // A guard the reader itself sits under skips both steps together, so
        // it can never strand the reader; only a guard outside the reader's
        // own chain can leave the reader running with nothing recorded.
        let Some((producer, guard)) = producers.iter().find_map(|producer| {
            producer
                .chain
                .iter()
                .find(|guard| !reader_guards.contains(guard))
                .map(|guard| (&producer.step_id, guard))
        }) else {
            continue;
        };
        let cause = if guard == producer {
            format!("step `{producer}` carries its own `when:` and may be skipped")
        } else {
            format!(
                "step `{producer}` runs inside step `{guard}`, which carries a `when:` \
                 and skips its whole body"
            )
        };
        return Err(DispatchError::JobValidation(format!(
            "step `{referencing_step}` reads `steps.{referenced_step}.output`, but {cause} — a \
             `when:` or `break_when:` condition may only read the output of a step that always \
             runs"
        )));
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
/// The scan covers the job's `recovery_activity`, `failure_activity` and
/// `final_recovery_activity`, every step's `recovery_activity`, and every
/// resolved deterministic target —
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
    check_activity_action(
        job.resolved_final_recovery_activity
            .as_ref()
            .map(|a| &a.spec),
        || {
            activity_label(
                job.final_recovery_activity.as_deref(),
                "job final_recovery_activity",
            )
        },
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
            if let JoinMode::Quorum { n } = &parallel.join
                && (*n == 0 || *n as usize > parallel.branches.len())
            {
                return Err(DispatchError::JobValidation(format!(
                    "step `{}` parallel quorum n={n} must be at least 1 and at most \
                     the branch count ({})",
                    step.id,
                    parallel.branches.len()
                )));
            }
            for branch in &parallel.branches {
                validate_step(branch)?;
            }
        }
        JobV2StepBody::FanOut { fan_out, fan_in } => {
            if let Some(errors) = &fan_in.errors
                && (errors.is_empty()
                    || errors == &step.id
                    || Some(errors) == fan_in.collect.as_ref())
            {
                return Err(DispatchError::JobValidation(format!(
                    "step `{}` fan-in errors key must be nonempty and distinct from its id and collect alias",
                    step.id
                )));
            }
            if matches!(fan_in.join, JoinMode::Quorum { n: 0 }) {
                return Err(DispatchError::JobValidation(format!(
                    "step `{}` fan-in quorum n=0 must be at least 1",
                    step.id
                )));
            }
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
