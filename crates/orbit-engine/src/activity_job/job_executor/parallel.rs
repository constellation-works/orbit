use std::collections::HashSet;

use super::*;

pub(super) fn run_parallel(
    step: &JobV2Step,
    block: &ParallelBlock,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let branches = &block.branches;
    let inherited_parent_stack = ctx
        .audit
        .parent_stack_snapshot()
        .map_err(|err| DispatchError::AuditFailed(format!("{err:?}")))?;

    let results: Vec<(String, Result<StepOutcome, DispatchError>)> = thread::scope(|scope| {
        let handles: Vec<_> = branches
            .iter()
            .map(|branch| {
                let branch_id = branch.id.clone();
                let join_branch_id = branch_id.clone();
                let ctx_ref = ctx;
                let audit = ctx.audit.clone();
                let inherited_parent_stack = inherited_parent_stack.clone();
                let handle = scope.spawn(move || {
                    let _parent_guard = match audit.install_parent_stack(inherited_parent_stack) {
                        Ok(guard) => guard,
                        Err(err) => {
                            return (
                                branch_id,
                                Err(DispatchError::AuditFailed(format!("{err:?}"))),
                            );
                        }
                    };
                    if let Some(restored) = restored_branch(step, branch, ctx_ref) {
                        return (branch_id, Ok(restored));
                    }
                    let result = run_step(branch, ctx_ref);
                    (branch_id, checkpoint_branch(step, branch, ctx_ref, result))
                });
                (join_branch_id, handle)
            })
            .collect();
        handles
            .into_iter()
            .map(|(branch_id, handle)| match handle.join() {
                Ok(result) => result,
                Err(payload) => (
                    branch_id,
                    Err(DispatchError::JobExecution(format!(
                        "branch thread panicked: {}",
                        panic_payload_message(payload.as_ref())
                    ))),
                ),
            })
            .collect()
    });

    let mut outcomes: Vec<BranchOutcome> = Vec::new();
    let mut successes = 0u32;
    let mut failures = 0u32;
    let mut first_error: Option<DispatchError> = None;
    let mut first_failure: Option<String> = None;
    // [ORB-20158] Branches waiting on an auto-task step: neither a success
    // nor a failure. The join decides once they settle.
    let mut parks = Vec::new();
    for (branch_id, res) in &results {
        let outcome_str = match res {
            Ok(o) if o.success => "success",
            Ok(_) => "failed",
            Err(DispatchError::AutoTaskParked(_)) => "parked",
            Err(_) => "error",
        };
        match res {
            Ok(o) if o.success => successes += 1,
            Err(DispatchError::AutoTaskParked(park)) => parks.push((**park).clone()),
            Ok(o) => {
                failures += 1;
                let provider_failure = |message: Option<&str>| {
                    orbit_types::workflow::ProviderFailureClass::of(None, message).is_some()
                };
                if first_failure.is_none()
                    || (provider_failure(o.message.as_deref())
                        && !provider_failure(first_failure.as_deref()))
                {
                    first_failure.clone_from(&o.message);
                }
            }
            Err(e) => {
                failures += 1;
                if first_error.is_none() {
                    first_error = Some(e.clone());
                }
            }
        }
        outcomes.push(BranchOutcome {
            branch_id: branch_id.clone(),
            outcome: outcome_str.to_string(),
        });
    }

    let mode_label = match &block.join {
        JoinMode::All => "all",
        JoinMode::Any => "any",
        JoinMode::Quorum { .. } => "quorum",
    };
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::StepJoin {
            step_id: step.id.clone(),
            mode: mode_label.to_string(),
            branch_outcomes: outcomes.clone(),
        },
    );

    let block_ok = match &block.join {
        JoinMode::All => failures == 0,
        JoinMode::Any => successes > 0,
        JoinMode::Quorum { n } => successes >= *n,
    };
    // A join that pending branches could still satisfy parks the block; one
    // they can no longer rescue keeps the existing failure semantics. `all`
    // fails on any failure; `any`/`quorum` fail once the parked branches
    // cannot make up the shortfall.
    let pending = parks.len() as u32;
    let can_still_pass = match &block.join {
        JoinMode::All => failures == 0,
        JoinMode::Any => successes + pending > 0,
        JoinMode::Quorum { n } => successes + pending >= *n,
    };
    let join_satisfied = match &block.join {
        JoinMode::All => block_ok && pending == 0,
        JoinMode::Any | JoinMode::Quorum { .. } => block_ok,
    };
    if !join_satisfied
        && can_still_pass
        && let Some(park) = merge_parked(parks)
    {
        return Err(DispatchError::AutoTaskParked(Box::new(park)));
    }

    if !block_ok && let Some(err) = first_error {
        // Surface the first branch error if no branch succeeded and any
        // errored — gives the caller a structural reason, not a bare false.
        return Err(err);
    }

    let branch_json: Vec<Value> = outcomes
        .into_iter()
        .map(|b| serde_json::json!({"branch_id": b.branch_id, "outcome": b.outcome}))
        .collect();
    let out = Value::Array(branch_json);
    record_pipeline(ctx, &step.id, out.clone());

    Ok(StepOutcome {
        success: block_ok,
        output: out,
        // A failed join keeps the first branch message, preferring a provider
        // diagnostic, so provider-limit and blocker markers reach retry, final
        // recovery and run finalization like a failed fan-out's.
        message: (!block_ok).then_some(first_failure).flatten(),
    })
}

/// Marker on a compound branch checkpoint. A target branch stores its raw
/// output; a loop, fan-out, or nested parallel stores this envelope so a
/// replay can put back every pipeline entry the branch exposed. A compound
/// checkpoint written before the envelope existed has no marker and is
/// replayed, because that raw output does not carry the nested entries.
const COMPOUND_BRANCH_CHECKPOINT_KIND: &str = "compound_branch";

/// The recorded output of a succeeded branch a resumed auto-task job
/// restores. Target and compound branches are both restored whole. A
/// compound branch also puts back the nested outputs and fan-in aliases it
/// exposed. A branch with no success checkpoint still runs.
fn restored_branch(step: &JobV2Step, branch: &JobV2Step, ctx: &ExecCtx<'_>) -> Option<StepOutcome> {
    let stored = ctx
        .restored_branches
        .as_ref()?
        .get(&step.id)?
        .get(&branch.id)?
        .clone();
    let (output, exposed) = match &branch.body {
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => (stored, serde_json::Map::new()),
        _ => restored_compound_branch(&stored)?,
    };
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::StepSkipped {
            step_id: branch.id.clone(),
            reason: format!(
                "resume: branch already succeeded in parallel step `{}`",
                step.id
            ),
        },
    );
    for (key, value) in &exposed {
        record_pipeline(ctx, key, value.clone());
    }
    record_pipeline(ctx, &branch.id, output.clone());
    Some(StepOutcome {
        success: true,
        output,
        message: None,
    })
}

/// Split a compound branch checkpoint into its own output and the other
/// pipeline entries it exposed. `None` when the value is not the envelope,
/// so an older raw checkpoint replays the branch.
fn restored_compound_branch(stored: &Value) -> Option<(Value, serde_json::Map<String, Value>)> {
    let object = stored.as_object()?;
    if object.get("kind").and_then(Value::as_str) != Some(COMPOUND_BRANCH_CHECKPOINT_KIND) {
        return None;
    }
    let output = object.get("output")?.clone();
    let exposed = object.get("exposed")?.as_object()?.clone();
    Some((output, exposed))
}

/// Checkpoint value for a succeeded compound branch: its output, plus the
/// other parent-pipeline entries the branch exposed.
///
/// Step ids are unique, but a fan-in alias may equal another step id, and
/// `collect_step_keys` also names each fan-out worker and its descendants.
/// Those workers write a private copy-on-write pipeline, so a parent entry
/// under a worker id belongs to another producer. Skip that private
/// namespace unless this branch's own fan-in alias uses the id: that alias
/// is a parent write, even when it collides with a worker id.
fn compound_branch_checkpoint_value(
    branch: &JobV2Step,
    ctx: &ExecCtx<'_>,
    output: &Value,
) -> Value {
    let mut keys = Vec::new();
    super::execute::collect_step_keys(branch, &mut keys);
    let private_worker_keys = fan_out_worker_local_keys(branch);
    let owned_aliases = parent_fan_in_alias_keys(branch);
    let pipeline = ctx.pipeline_snapshot();
    let mut exposed = serde_json::Map::new();
    for key in keys {
        if key == branch.id {
            continue;
        }
        if private_worker_keys.contains(key) && !owned_aliases.contains(key) {
            continue;
        }
        if let Some(value) = pipeline.get(key) {
            exposed.insert(key.to_string(), unwrap_step_output(value));
        }
    }
    serde_json::json!({
        "kind": COMPOUND_BRANCH_CHECKPOINT_KIND,
        "output": output,
        "exposed": Value::Object(exposed),
    })
}

/// Step ids a fan-out worker and its descendants write only into that
/// worker's copy-on-write pipeline. A parent entry under one of these ids
/// was produced by someone else.
fn fan_out_worker_local_keys(step: &JobV2Step) -> HashSet<&str> {
    let mut keys = HashSet::new();
    collect_fan_out_worker_local_keys(step, &mut keys);
    keys
}

fn collect_fan_out_worker_local_keys<'a>(step: &'a JobV2Step, keys: &mut HashSet<&'a str>) {
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                collect_fan_out_worker_local_keys(branch, keys);
            }
        }
        JobV2StepBody::Loop { loop_ } => {
            for nested in &loop_.steps {
                collect_fan_out_worker_local_keys(nested, keys);
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            let mut worker_keys = Vec::new();
            // The worker's own fan-outs, including their aliases, record into
            // this private map. Do not walk them as parent-visible writers.
            super::execute::collect_step_keys(&fan_out.worker, &mut worker_keys);
            keys.extend(worker_keys);
        }
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => {}
    }
}

/// Fan-in `collect` and `errors` aliases this step records on the parent
/// pipeline. A fan-out nested inside a worker is omitted: that fan-out
/// records into the worker map. An alias may equal a worker id.
fn parent_fan_in_alias_keys(step: &JobV2Step) -> HashSet<&str> {
    let mut aliases = HashSet::new();
    collect_parent_fan_in_alias_keys(step, &mut aliases);
    aliases
}

fn collect_parent_fan_in_alias_keys<'a>(step: &'a JobV2Step, aliases: &mut HashSet<&'a str>) {
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                collect_parent_fan_in_alias_keys(branch, aliases);
            }
        }
        JobV2StepBody::Loop { loop_ } => {
            for nested in &loop_.steps {
                collect_parent_fan_in_alias_keys(nested, aliases);
            }
        }
        JobV2StepBody::FanOut { fan_in, .. } => {
            if let Some(alias) = fan_in.collect.as_deref() {
                aliases.insert(alias);
            }
            if let Some(alias) = fan_in.errors.as_deref() {
                aliases.insert(alias);
            }
        }
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => {}
    }
}

/// Checkpoint a succeeded branch of a job with auto-task steps before the
/// join, so a sibling that parks or fails cannot make a replay rerun it.
/// Fail-closed: an unrecorded success fails the branch.
fn checkpoint_branch(
    step: &JobV2Step,
    branch: &JobV2Step,
    ctx: &ExecCtx<'_>,
    result: Result<StepOutcome, DispatchError>,
) -> Result<StepOutcome, DispatchError> {
    let outcome = result?;
    if !outcome.success || ctx.restored_branches.is_none() {
        return Ok(outcome);
    }
    let recorded = match &branch.body {
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => outcome.output.clone(),
        _ => compound_branch_checkpoint_value(branch, ctx, &outcome.output),
    };
    ctx.host
        .checkpoint_parallel_branch(&ctx.run_id, &step.id, &branch.id, &recorded)
        .map_err(|error| {
            DispatchError::JobExecution(format!(
                "parallel step `{}`: branch `{}` succeeded but its checkpoint failed: {error}",
                step.id, branch.id
            ))
        })?;
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Fan-out / fan-in
// ---------------------------------------------------------------------------
