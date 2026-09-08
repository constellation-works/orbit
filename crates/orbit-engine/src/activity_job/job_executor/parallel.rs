// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use super::*;

pub(super) fn run_parallel(
    step: &JobV2Step,
    block: &ParallelBlock,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let branches = &block.branches;
    if branches.is_empty() {
        return Ok(StepOutcome {
            success: true,
            output: Value::Array(Vec::new()),
            message: None,
        });
    }
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
                    (branch_id, run_step(branch, ctx_ref))
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
    for (branch_id, res) in &results {
        let outcome_str = match res {
            Ok(o) if o.success => "success",
            Ok(_) => "failed",
            Err(_) => "error",
        };
        match res {
            Ok(o) if o.success => successes += 1,
            Ok(_) => failures += 1,
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
        message: None,
    })
}

// ---------------------------------------------------------------------------
// Fan-out / fan-in
// ---------------------------------------------------------------------------
