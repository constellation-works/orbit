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

/// The recorded output of a succeeded branch a resumed auto-task job
/// restores. Only target branches are restored whole; a compound branch
/// replays, and its own nested branches and auto-task records restore
/// themselves inside it.
fn restored_branch(step: &JobV2Step, branch: &JobV2Step, ctx: &ExecCtx<'_>) -> Option<StepOutcome> {
    if !matches!(
        branch.body,
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_)
    ) {
        return None;
    }
    let output = ctx
        .restored_branches
        .as_ref()?
        .get(&step.id)?
        .get(&branch.id)?
        .clone();
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
    record_pipeline(ctx, &branch.id, output.clone());
    Some(StepOutcome {
        success: true,
        output,
        message: None,
    })
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
    ctx.host
        .checkpoint_parallel_branch(&ctx.run_id, &step.id, &branch.id, &outcome.output)
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
