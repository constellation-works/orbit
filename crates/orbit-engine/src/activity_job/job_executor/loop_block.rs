use super::*;

pub(super) fn run_loop(
    step: &JobV2Step,
    block: &LoopBlock,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let loop_items = match &block.items {
        Some(expression) => Some(render_items_expression(
            expression,
            &ctx.template_ctx(),
            "loop.items",
        )?),
        None => None,
    };
    if let Some(items) = &loop_items
        && items.len() > block.max_iterations as usize
    {
        return Err(DispatchError::JobExecution(format!(
            "loop.items produced {} entries, exceeding max_iterations {}",
            items.len(),
            block.max_iterations
        )));
    }

    let mut broke = false;
    let mut last_iter: u32 = 0;
    let planned_iterations = loop_items
        .as_ref()
        .map(|items| items.len() as u32)
        .unwrap_or(block.max_iterations);
    for iter in 1..=planned_iterations {
        let iteration_index = iter - 1;
        last_iter = iter;
        emit_job_event_lossy(
            &ctx.audit,
            ctx.task_id(),
            V2AuditEventKind::LoopIterationStart {
                step_id: step.id.clone(),
                iteration: iter,
            },
        );
        let loop_ctx = ExecCtx {
            run_id: ctx.run_id.clone(),
            audit: ctx.audit.clone(),
            host: ctx.host,
            input: ctx.input.clone(),
            pipeline: ctx.pipeline.clone(),
            recovery_activity: ctx.recovery_activity.clone(),
            failure_activity: ctx.failure_activity.clone(),
            final_recovery_activity: None,
            item: loop_items
                .as_ref()
                .and_then(|items| items.get(iteration_index as usize).cloned()),
            iteration: Some(iteration_index),
        };

        for body in &block.steps {
            let outcome = run_step(body, &loop_ctx)?;
            if !outcome.success {
                return Ok(StepOutcome {
                    success: false,
                    output: outcome.output,
                    message: outcome.message,
                });
            }
        }

        // Evaluate break_when after the body runs so the body can populate
        // pipeline state that the expression references.
        let should_break = if let Some(expr) = &block.break_when {
            let tctx = loop_ctx.template_ctx();
            evaluate_bool_expr(expr, &tctx)
                .map_err(|err| DispatchError::JobExecution(format!("break_when: {err}")))?
        } else {
            false
        };

        emit_job_event_lossy(
            &ctx.audit,
            ctx.task_id(),
            V2AuditEventKind::LoopIterationEnd {
                step_id: step.id.clone(),
                iteration: iter,
                broke: should_break,
            },
        );

        if should_break {
            broke = true;
            break;
        }
    }

    if !broke && block.break_when.is_some() {
        emit_job_event_lossy(
            &ctx.audit,
            ctx.task_id(),
            V2AuditEventKind::LoopDidNotConverge {
                step_id: step.id.clone(),
                max_iterations: block.max_iterations,
            },
        );
    }

    let out = serde_json::json!({
        "iterations": last_iter,
        "broke": broke,
    });
    record_pipeline(ctx, &step.id, out.clone());

    Ok(StepOutcome {
        success: true,
        output: out,
        message: None,
    })
}
