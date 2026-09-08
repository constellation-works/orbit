// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use super::*;

pub(super) fn run_fan_out(
    step: &JobV2Step,
    block: &FanOutBlock,
    fan_in: &FanInSpec,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let tctx = ctx.template_ctx();
    let items = render_items_expression(&block.items, &tctx, "fan_out.items")?;
    let worker_count = items.len() as u32;

    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::FanoutDispatched {
            step_id: step.id.clone(),
            worker_count,
        },
    );

    if items.is_empty() {
        emit_job_event_lossy(
            &ctx.audit,
            ctx.task_id(),
            V2AuditEventKind::FaninJoined {
                step_id: step.id.clone(),
                collected: 0,
                failed: 0,
            },
        );
        let collected_value = Value::Array(Vec::new());
        if let Some(collect_key) = &fan_in.collect {
            record_pipeline(ctx, collect_key, collected_value.clone());
        }
        record_pipeline(ctx, &step.id, collected_value.clone());
        return Ok(StepOutcome {
            success: true,
            output: collected_value,
            message: None,
        });
    }

    let inherited_parent_stack = ctx
        .audit
        .parent_stack_snapshot()
        .map_err(|err| DispatchError::AuditFailed(format!("{err:?}")))?;
    let semaphore = Arc::new(Semaphore::new(block.max_workers.max(1) as usize));
    let results: Mutex<Vec<(u32, Result<StepOutcome, DispatchError>)>> =
        Mutex::new(Vec::with_capacity(items.len()));

    let panicked_workers = thread::scope(|scope| {
        let mut handles = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            let idx = idx as u32;
            let sem = Arc::clone(&semaphore);
            let item = item.clone();
            let worker_step = block.worker.clone();
            let run_id = ctx.run_id.clone();
            let audit = ctx.audit.clone();
            let host = ctx.host;
            let base_input = ctx.input.clone();
            let pipeline_snapshot = ctx.pipeline.lock().expect("pipeline poisoned").clone();
            let results_ref = &results;
            let inherited_parent_stack = inherited_parent_stack.clone();

            handles.push(scope.spawn(move || {
                let _parent_guard = match audit.install_parent_stack(inherited_parent_stack) {
                    Ok(guard) => guard,
                    Err(err) => {
                        results_ref
                            .lock()
                            .expect("results poisoned")
                            .push((idx, Err(DispatchError::AuditFailed(format!("{err:?}")))));
                        return;
                    }
                };
                let _permit = sem.acquire();
                // Resolve task_id once so both worker_state emissions stamp
                // the same value; `base_input` will be moved into worker_ctx
                // below and is no longer accessible afterwards.
                let worker_task_id =
                    super::super::cli_runner::task_id_from_input(&base_input).map(str::to_string);
                emit_job_event_lossy(
                    &audit,
                    worker_task_id.as_deref(),
                    V2AuditEventKind::WorkerState {
                        step_id: worker_step.id.clone(),
                        worker_index: idx,
                        state: "dispatched".to_string(),
                    },
                );
                let worker_ctx = ExecCtx {
                    run_id,
                    audit: audit.clone(),
                    host,
                    input: base_input,
                    pipeline: Arc::new(Mutex::new(pipeline_snapshot)),
                    recovery_activity: ctx.recovery_activity.clone(),
                    failure_activity: ctx.failure_activity.clone(),
                    item: Some(item),
                    iteration: Some(idx),
                };
                let res = run_step(&worker_step, &worker_ctx);
                let state = match &res {
                    Ok(o) if o.success => "finished",
                    Ok(_) => "failed",
                    Err(_) => "failed",
                };
                emit_job_event_lossy(
                    &audit,
                    worker_task_id.as_deref(),
                    V2AuditEventKind::WorkerState {
                        step_id: worker_step.id.clone(),
                        worker_index: idx,
                        state: state.to_string(),
                    },
                );
                results_ref
                    .lock()
                    .expect("results poisoned")
                    .push((idx, res));
            }));
        }
        let mut panicked_workers = Vec::new();
        for (idx, h) in handles.into_iter().enumerate() {
            if let Err(payload) = h.join() {
                let idx = idx as u32;
                panicked_workers.push(idx);
                let message = panic_payload_message(payload.as_ref());
                emit_job_event_lossy(
                    &ctx.audit,
                    ctx.task_id(),
                    V2AuditEventKind::WorkerState {
                        step_id: block.worker.id.clone(),
                        worker_index: idx,
                        state: "failed".to_string(),
                    },
                );
                results.lock().expect("results poisoned").push((
                    idx,
                    Err(DispatchError::JobExecution(format!(
                        "worker {idx} panicked: {message}"
                    ))),
                ));
            }
        }

        panicked_workers
    });

    let mut collected: Vec<Value> = Vec::new();
    let mut collected_count = 0u32;
    let mut failed_count = 0u32;
    let mut first_error: Option<DispatchError> = None;
    let mut sorted = results.into_inner().expect("results poisoned");
    if sorted.len() != items.len() {
        return Err(DispatchError::JobExecution(format!(
            "fan-out collected {} results for {} items",
            sorted.len(),
            items.len()
        )));
    }
    sorted.sort_by_key(|(idx, _)| *idx);
    for (idx, res) in sorted {
        match res {
            Ok(o) if o.success => {
                collected_count += 1;
                collected.push(o.output);
            }
            Ok(_) => {
                failed_count += 1;
                collected.push(Value::Null);
            }
            Err(e) => {
                failed_count += 1;
                if !panicked_workers.contains(&idx) && first_error.is_none() {
                    first_error = Some(e);
                }
                collected.push(Value::Null);
            }
        }
    }

    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::FaninJoined {
            step_id: step.id.clone(),
            collected: collected_count,
            failed: failed_count,
        },
    );

    let block_ok = match &fan_in.join {
        JoinMode::All => failed_count == 0,
        JoinMode::Any => collected_count > 0,
        JoinMode::Quorum { n } => collected_count >= *n,
    };

    if !block_ok && let Some(err) = first_error {
        return Err(err);
    }

    let collected_value = Value::Array(collected);
    if let Some(collect_key) = &fan_in.collect {
        record_pipeline(ctx, collect_key, collected_value.clone());
    }
    record_pipeline(ctx, &step.id, collected_value.clone());

    Ok(StepOutcome {
        success: block_ok,
        output: collected_value,
        message: None,
    })
}

// ---------------------------------------------------------------------------
// Loop
// ---------------------------------------------------------------------------
