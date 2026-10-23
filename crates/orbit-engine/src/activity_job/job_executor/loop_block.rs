use super::*;

pub(super) fn run_loop(
    step: &JobV2Step,
    block: &LoopBlock,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let timeout = render_duration(
        block.timeout_seconds.as_deref(),
        ctx,
        "loop.timeout_seconds",
    )?;
    let interval = render_duration(
        block.interval_seconds.as_deref(),
        ctx,
        "loop.interval_seconds",
    )?;
    let started = loop_clock::now();
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
    let mut last_output = Value::Null;
    let planned_iterations = loop_items
        .as_ref()
        .map(|items| items.len() as u32)
        .unwrap_or(block.max_iterations);
    for iter in 1..=planned_iterations {
        if timeout
            .is_some_and(|budget| loop_clock::now().saturating_duration_since(started) >= budget)
        {
            break;
        }
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
            resumed_diagnostics: ctx.resumed_diagnostics.clone(),
            recovery_activity: ctx.recovery_activity.clone(),
            failure_activity: ctx.failure_activity.clone(),
            final_recovery_activity: None,
            check_repair_activity: ctx.check_repair_activity.clone(),
            recovery_validation_continuations: Default::default(),
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
            last_output = outcome.output;
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
        if iter < planned_iterations
            && let Some(interval) = interval
        {
            let remaining = timeout.map(|budget| {
                budget.saturating_sub(loop_clock::now().saturating_duration_since(started))
            });
            let delay = remaining.map_or(interval, |remaining| interval.min(remaining));
            if !delay.is_zero() {
                loop_clock::sleep(delay);
            }
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
        "timed_out": timeout.is_some_and(|budget| loop_clock::now().saturating_duration_since(started) >= budget),
        "last_output": last_output,
    });
    record_pipeline(ctx, &step.id, out.clone());

    Ok(StepOutcome {
        success: true,
        output: out,
        message: None,
    })
}

fn render_duration(
    expression: Option<&str>,
    ctx: &ExecCtx<'_>,
    label: &str,
) -> Result<Option<Duration>, DispatchError> {
    expression
        .map(|expression| {
            let rendered = template::render(expression, &ctx.template_ctx())
                .map_err(|err| DispatchError::JobExecution(format!("{label}: {err}")))?;
            rendered
                .parse::<f64>()
                .ok()
                .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
                .ok_or_else(|| {
                    DispatchError::JobExecution(format!(
                        "{label} must be finite non-negative seconds"
                    ))
                })
        })
        .transpose()
}
