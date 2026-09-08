use super::*;

/// Dual-write entry point for job-lifecycle audit events.
///
/// Emits a `tracing::*!` event with a stable, target-keyed projection of
/// `kind` (so the global JSONL feed and `orbit log tail` see live job
/// activity) and persists the same `kind` through the canonical audit writer.
/// Tracing comes first so the live feed reflects the event before the audit
/// store's in-memory snapshot lock is taken; the audit store remains the
/// authoritative trail.
///
/// `task_id` is the optional task identifier extracted from the surrounding
/// activity input (via [`super::super::cli_runner::task_id_from_input`]) so
/// job-step tracing events correlate with cli_runner subprocess events that
/// already emit `task_id` and `job_run_id`. Pass `None` when no task id is in
/// scope.
///
/// This helper is the only place in the `job_executor` module that pairs an
/// `audit.emit(...)` with a `tracing::*!` for job-lifecycle events. Adding a
/// new `V2AuditEventKind` variant requires touching only `emit_job_tracing`
/// to get tracing coverage.
pub(super) fn emit_job_event(
    audit: &V2AuditWriter,
    task_id: Option<&str>,
    kind: V2AuditEventKind,
) -> Result<String, WriteError> {
    emit_job_tracing(audit.run_id(), task_id, &kind);
    audit.emit(kind)
}

/// [ORB-00414] Non-fatal variant of [`emit_job_event`]: on an audit-write
/// failure it records the failure through the writer's per-run counter +
/// `tracing::error!` rather than discarding it with `let _ =`. Used at the
/// job-lifecycle emission sites whose returned event id is not load-bearing;
/// `StepStarted` (whose id parents nested events) keeps using the fatal
/// [`emit_job_event`] directly.
pub(super) fn emit_job_event_lossy(
    audit: &V2AuditWriter,
    task_id: Option<&str>,
    kind: V2AuditEventKind,
) {
    let event_kind = kind.event_type();
    if let Err(error) = emit_job_event(audit, task_id, kind) {
        audit.note_audit_failure(event_kind, &error);
    }
}

/// Project a job-lifecycle `V2AuditEventKind` onto the unified tracing feed
/// using the per-variant target/level rules documented in T20260427-27.
///
/// Common fields on every emission:
/// - `job_run_id` — the `V2AuditWriter::run_id()` (matches cli_runner's field
///   naming so subprocess and step events correlate in JSONL consumers).
/// - `step_id` — present on every job-lifecycle variant.
/// - `task_id` — emitted only when the calling context resolved a task id
///   from the activity input; omitted otherwise.
///
/// Variants that are emitted by other layers (e.g. `RunStarted`,
/// `ActivityStarted`, `FsCall*`, `ToolDenied`, `CliInvocation*`) are
/// intentionally not projected here — those producers run their own dual
/// writes and we do not want job_executor to double-emit on their behalf.
pub(super) fn emit_job_tracing(job_run_id: &str, task_id: Option<&str>, kind: &V2AuditEventKind) {
    match kind {
        V2AuditEventKind::StepStarted { step_id } => {
            tracing::info!(
                target: "orbit.job.step_started",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                "step started",
            );
        }
        V2AuditEventKind::StepFinished {
            step_id, outcome, ..
        } => {
            let success = outcome == "success";
            if success {
                tracing::info!(
                    target: "orbit.job.step_finished",
                    job_run_id = job_run_id,
                    task_id = task_id,
                    step_id = step_id.as_str(),
                    outcome = outcome.as_str(),
                    success = success,
                    "step finished",
                );
            } else {
                tracing::error!(
                    target: "orbit.job.step_finished",
                    job_run_id = job_run_id,
                    task_id = task_id,
                    step_id = step_id.as_str(),
                    outcome = outcome.as_str(),
                    success = success,
                    "step finished",
                );
            }
        }
        V2AuditEventKind::StepSkipped { step_id, reason } => {
            tracing::warn!(
                target: "orbit.job.step_skipped",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                reason = reason.as_str(),
                "step skipped",
            );
        }
        V2AuditEventKind::StepRetry {
            step_id,
            attempt,
            next_backoff_ms,
        } => {
            tracing::warn!(
                target: "orbit.job.step_retry",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                attempt = *attempt,
                next_backoff_ms = *next_backoff_ms,
                "step retry",
            );
        }
        V2AuditEventKind::StepRecoveryAttempted {
            step_id,
            recovery_activity,
            recovery_succeeded,
            failure_phase,
            error_message,
        } => {
            if *recovery_succeeded {
                tracing::info!(
                    target: "orbit.job.step_recovery_attempted",
                    job_run_id = job_run_id,
                    task_id = task_id,
                    step_id = step_id.as_str(),
                    recovery_activity = recovery_activity.as_str(),
                    recovery_succeeded = *recovery_succeeded,
                    failure_phase = failure_phase.as_deref(),
                    error_message = error_message.as_deref(),
                    "step recovery attempted",
                );
            } else {
                tracing::warn!(
                    target: "orbit.job.step_recovery_attempted",
                    job_run_id = job_run_id,
                    task_id = task_id,
                    step_id = step_id.as_str(),
                    recovery_activity = recovery_activity.as_str(),
                    recovery_succeeded = *recovery_succeeded,
                    failure_phase = failure_phase.as_deref(),
                    error_message = error_message.as_deref(),
                    "step recovery attempted",
                );
            }
        }
        V2AuditEventKind::StepPostRecoveryAttempt {
            step_id,
            recovery_activity,
            outcome,
            error_message,
        } => {
            if outcome == "success" {
                tracing::info!(
                    target: "orbit.job.step_post_recovery_attempt",
                    job_run_id = job_run_id,
                    task_id = task_id,
                    step_id = step_id.as_str(),
                    recovery_activity = recovery_activity.as_str(),
                    outcome = outcome.as_str(),
                    error_message = error_message.as_deref(),
                    "step post-recovery attempt",
                );
            } else {
                tracing::warn!(
                    target: "orbit.job.step_post_recovery_attempt",
                    job_run_id = job_run_id,
                    task_id = task_id,
                    step_id = step_id.as_str(),
                    recovery_activity = recovery_activity.as_str(),
                    outcome = outcome.as_str(),
                    error_message = error_message.as_deref(),
                    "step post-recovery attempt",
                );
            }
        }
        V2AuditEventKind::StepDenied { step_id, reason } => {
            tracing::error!(
                target: "orbit.job.step_denied",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                reason = reason.as_str(),
                "step denied",
            );
        }
        V2AuditEventKind::StepJoin {
            step_id,
            mode,
            branch_outcomes,
        } => {
            tracing::info!(
                target: "orbit.job.step_join",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                mode = mode.as_str(),
                branch_count = branch_outcomes.len(),
                "step join",
            );
        }
        V2AuditEventKind::FanoutDispatched {
            step_id,
            worker_count,
        } => {
            tracing::info!(
                target: "orbit.job.fanout",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                phase = "dispatched",
                worker_count = *worker_count,
                "fanout dispatched",
            );
        }
        V2AuditEventKind::FaninJoined {
            step_id,
            collected,
            failed,
        } => {
            tracing::info!(
                target: "orbit.job.fanout",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                phase = "joined",
                collected = *collected,
                failed = *failed,
                "fanin joined",
            );
        }
        V2AuditEventKind::WorkerState {
            step_id,
            worker_index,
            state,
        } => {
            tracing::info!(
                target: "orbit.job.worker_state",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                worker_index = *worker_index,
                state = state.as_str(),
                "worker state",
            );
        }
        V2AuditEventKind::LoopIterationStart { step_id, iteration } => {
            tracing::info!(
                target: "orbit.job.loop_iteration",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                phase = "start",
                iteration = *iteration,
                "loop iteration start",
            );
        }
        V2AuditEventKind::LoopIterationEnd {
            step_id,
            iteration,
            broke,
        } => {
            tracing::info!(
                target: "orbit.job.loop_iteration",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                phase = "end",
                iteration = *iteration,
                broke = *broke,
                "loop iteration end",
            );
        }
        V2AuditEventKind::LoopDidNotConverge {
            step_id,
            max_iterations,
        } => {
            tracing::warn!(
                target: "orbit.job.loop_did_not_converge",
                job_run_id = job_run_id,
                task_id = task_id,
                step_id = step_id.as_str(),
                max_iterations = *max_iterations,
                "loop did not converge",
            );
        }
        // Other V2AuditEventKind variants (RunStarted, RunFinished,
        // ActivityStarted, ActivityFinished, FsCall*, ToolDenied,
        // ToolAllowlistHarnessDelegated, CliInvocationStarted,
        // CliInvocationFinished) are emitted by other producers, which own
        // their own tracing dual-write story. We intentionally do not project
        // them here so call sites outside `job_executor.rs` don't double-emit.
        _ => {}
    }
}
