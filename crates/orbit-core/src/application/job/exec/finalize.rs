use super::entry::V2JobRunResult;
use crate::OrbitRuntime;
use crate::application::job::log_best_effort;
use crate::application::job::resume::ResumePlan;
use crate::runtime::audit::run::AuditStepOutcome;
use orbit_common::OrbitError;
use orbit_engine::JobOutcome;
use orbit_store::contracts::{JobRunStepParams, TaskReservationReleaseReason};
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::{
    HeldFailure, JobRun, JobRunState, JobRunTrigger, JobTargetType, PipelineState,
};
use serde_json::{Value, json};

/// The hold an unsuccessful run of `job_id` ends on instead of failing, if
/// its typed `error_code` or failure `message` names one. Owner-local runs hold on provider
/// capacity, usage limits and unavailability; claimed leaves hold on usage limits.
fn held_failure(
    job_id: &str,
    error_code: Option<&str>,
    message: Option<&str>,
) -> Option<HeldFailure> {
    if orbit_types::workflow::is_claimed_delivery_leaf_job(job_id) {
        HeldFailure::of_claimed_leaf(error_code, message)
    } else {
        HeldFailure::of_local(error_code, message)
    }
}

/// The refusal identity retained across the engine/core error boundary.
fn delivery_refusal_code(error: &OrbitError) -> Option<&'static str> {
    match error {
        OrbitError::DeliveryRefused { refusal, .. } => Some(refusal.error_code()),
        _ => None,
    }
}

/// Path-specific durable records retained while finalizing a v2 run.
///
/// The foreground path predates per-step checkpointing, so its legacy summary
/// row and synthetic success step remain observable API. Detached workers
/// already persist their authoritative per-step checkpoints and must not add a
/// synthetic step 0 that can obscure them. Keeping that distinction explicit
/// lets both paths share the terminal-state, reservation, and event sequence.
#[derive(Debug, Clone, Copy)]
pub(crate) struct V2RunFinalizationOptions {
    write_legacy_summary_step: bool,
    record_synthetic_success_step: bool,
}

impl V2RunFinalizationOptions {
    pub(crate) const DIRECT: Self = Self {
        write_legacy_summary_step: true,
        record_synthetic_success_step: true,
    };

    pub(crate) const DETACHED_WORKER: Self = Self {
        write_legacy_summary_step: false,
        record_synthetic_success_step: false,
    };
}

impl OrbitRuntime {
    /// Seed a persisted run before either the foreground or detached path
    /// executes it. A resume re-keys checkpoints and reconciles the failed
    /// lineage's task ownership before admission evaluates the first step.
    pub(crate) fn seed_v2_pipeline_run(
        &self,
        run: &JobRun,
        input: &Value,
        resume: Option<&ResumePlan>,
        trigger: JobRunTrigger,
    ) -> Result<(), OrbitError> {
        let mut initial_state = match resume.and_then(|plan| plan.resume_state.as_ref()) {
            Some(source_state) => super::seeded_resume_state(source_state, run),
            None => PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone()),
        };
        initial_state.trigger = Some(trigger);
        // [ORB-11283] A queued drain can carry an operator stop (or worker
        // ceiling, or window extension) written before the worker seeded this
        // document. Replacing the whole state would silently resume
        // admissions.
        if let Some(existing) = self.read_run_state(&run.run_id)? {
            if initial_state.pull_drain.drain_worker_limit.is_none() {
                initial_state.pull_drain.drain_worker_limit =
                    existing.pull_drain.drain_worker_limit;
            }
            if initial_state.pull_drain.drain_admissions_stop.is_none() {
                initial_state.pull_drain.drain_admissions_stop =
                    existing.pull_drain.drain_admissions_stop;
            }
            if initial_state.pull_drain.drain_window_extension.is_none() {
                initial_state.pull_drain.drain_window_extension =
                    existing.pull_drain.drain_window_extension;
            }
            if initial_state.pull_drain.drain_cancel.is_none() {
                initial_state.pull_drain.drain_cancel = existing.pull_drain.drain_cancel;
            }
            if initial_state.child_dispatches.is_empty() {
                initial_state.child_dispatches = existing.child_dispatches;
            }
            if existing.trigger.is_some() {
                initial_state.trigger = existing.trigger;
            }
        }
        self.stores()
            .jobs()
            .write_run_state(&run.run_id, &initial_state)?;
        if let Some(plan) = resume
            && input
                .get(orbit_types::workflow::SOURCE_REPAIR_KEY)
                .is_none()
        {
            self.reconcile_resume_task_ownership(plan, &run.run_id)?;
        }
        Ok(())
    }

    /// Persist the common terminal lifecycle for foreground and detached v2
    /// execution without changing their established checkpoint conventions.
    pub(crate) fn finalize_v2_pipeline_run(
        &self,
        run: &JobRun,
        input: &Value,
        started_at: chrono::DateTime<chrono::Utc>,
        finished_at: chrono::DateTime<chrono::Utc>,
        outcome: Result<&V2JobRunResult, &OrbitError>,
        options: V2RunFinalizationOptions,
    ) -> Result<(), OrbitError> {
        // A gracefully cancelled pull drain ends itself `cancelled` from its
        // last pass; the engine finishes after that with nothing to record.
        if self
            .get_job_run_backend(&run.run_id)?
            .is_some_and(|current| current.state == JobRunState::Cancelled)
            && self
                .read_run_state(&run.run_id)?
                .is_some_and(|state| state.drain_cancelling())
        {
            return Ok(());
        }
        let duration_ms = Some(
            finished_at
                .signed_duration_since(started_at)
                .num_milliseconds()
                .max(0) as u64,
        );
        let final_state = match outcome {
            Ok(result) if result.evidence_hold.is_some() => {
                // Final summaries are best-effort for every outcome: a store
                // fault must not turn an evidence hold into a worker failure.
                log_best_effort(
                    "persist held run state",
                    &run.run_id,
                    self.persist_v2_run_state(run, input, result, JobRunState::Held, options),
                );
                log_best_effort(
                    "record held step",
                    &run.run_id,
                    self.record_pipeline_diagnostic_step(
                        run,
                        started_at,
                        finished_at,
                        Some("review_awaiting_evidence"),
                        "Delivery awaits named external evidence; receipt queues a fresh review.",
                        JobRunState::Held,
                    ),
                );
                JobRunState::Held
            }
            Ok(result) if result.forge_hold.is_some() => {
                // [ORB-14617] The forge, not the candidate, refused the push:
                // no failure step, no task block. The clock resumes the run.
                log_best_effort(
                    "persist held run state",
                    &run.run_id,
                    self.persist_v2_run_state(run, input, result, JobRunState::Held, options),
                );
                let fallback = "the forge refused the delivery push past its retry budget";
                log_best_effort(
                    "record held step",
                    &run.run_id,
                    self.record_pipeline_diagnostic_step(
                        run,
                        started_at,
                        finished_at,
                        Some(orbit_types::workflow::FORGE_UNAVAILABLE_ERROR_CODE),
                        result.message.as_deref().unwrap_or(fallback),
                        JobRunState::Held,
                    ),
                );
                JobRunState::Held
            }
            Ok(result) if result.child_cancelled => {
                // [ORB-15202] An operator cancelled the child this run waited
                // on. The parent did not fail; it ends with its child.
                log_best_effort(
                    "persist cancelled run state",
                    &run.run_id,
                    self.persist_v2_run_state(run, input, result, JobRunState::Cancelled, options),
                );
                let fallback = "a child run this run waited on was cancelled";
                log_best_effort(
                    "record cancelled step",
                    &run.run_id,
                    // Run-level errors are projected from recorded steps.
                    // Retain the cancellation explanation as step output, not
                    // an error; the activity audit still names the child too.
                    self.record_synthetic_v2_terminal_step(
                        run,
                        started_at,
                        finished_at,
                        json!({"message": result.message.as_deref().unwrap_or(fallback)}),
                        JobRunState::Cancelled,
                    ),
                );
                JobRunState::Cancelled
            }
            Ok(result) if result.success => {
                // Summary persistence follows completed execution. Its failure
                // must not replace success or escape before the terminal write.
                log_best_effort(
                    "persist successful run state",
                    &run.run_id,
                    self.persist_v2_run_state(run, input, result, JobRunState::Success, options),
                );
                if options.record_synthetic_success_step {
                    log_best_effort(
                        "record success step",
                        &run.run_id,
                        self.record_synthetic_v2_terminal_step(
                            run,
                            started_at,
                            finished_at,
                            result.pipeline.clone(),
                            JobRunState::Success,
                        ),
                    );
                } else {
                    log_best_effort(
                        "persist worker step summary",
                        &run.run_id,
                        self.persist_detached_worker_steps(run, started_at, finished_at, result),
                    );
                }
                JobRunState::Success
            }
            Ok(result) => {
                let fallback = "job completed with success=false but emitted no failure detail";
                let message = result.message.as_deref().unwrap_or(fallback);
                let held = held_failure(
                    &run.job_id,
                    result.failure_code.as_ref().map(|code| code.as_str()),
                    Some(message),
                );
                let final_state = held.map_or(JobRunState::Failed, |_| JobRunState::Held);
                let message =
                    held.map_or_else(|| message.to_string(), |held| held.diagnostic(message));
                log_best_effort(
                    if held.is_some() {
                        "persist held run state"
                    } else {
                        "persist failed run state"
                    },
                    &run.run_id,
                    self.persist_v2_run_state(run, input, result, final_state, options),
                );
                log_best_effort(
                    "record failure step",
                    &run.run_id,
                    self.record_pipeline_diagnostic_step(
                        run,
                        started_at,
                        finished_at,
                        held.map(HeldFailure::error_code).or_else(|| {
                            result
                                .failure_code
                                .as_ref()
                                .map(orbit_types::workflow::StepFailureCode::as_str)
                        }),
                        &message,
                        final_state,
                    ),
                );
                self.record_held_deferral(run, held, &message);
                final_state
            }
            Err(error) => {
                // [ORB-15202] A red base or a refusal awaiting a recorded
                // decision kept its candidate: the run holds, it did not fail.
                // So do local provider capacity/limit/unavailability holds and a claimed
                // leaf its provider's usage limit stopped [ORB-15517].
                let typed_code = error.step_failure_code();
                let held = held_failure(
                    &run.job_id,
                    delivery_refusal_code(error)
                        .or_else(|| typed_code.as_ref().map(|code| code.as_str())),
                    Some(&error.to_string()),
                );
                let final_state = held.map_or(JobRunState::Failed, |_| JobRunState::Held);
                let message = held.map_or_else(
                    || error.to_string(),
                    |held| held.diagnostic(&error.to_string()),
                );
                let code = match held {
                    Some(held) => Some(held.error_code()),
                    None => matches!(error, OrbitError::ProtocolSkew(_))
                        .then_some("protocol_skew")
                        .or_else(|| typed_code.as_ref().map(|code| code.as_str())),
                };
                log_best_effort(
                    "record failure step",
                    &run.run_id,
                    self.record_pipeline_diagnostic_step(
                        run,
                        started_at,
                        finished_at,
                        code,
                        &message,
                        final_state,
                    ),
                );
                self.record_held_deferral(run, held, &message);
                final_state
            }
        };
        self.finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            final_state,
            finished_at,
            duration_ms,
            TaskReservationReleaseReason::RunTerminal,
        )?;
        if final_state == JobRunState::Success {
            self.record_delivered_worktree_cleanup(&run.run_id);
        }
        self.record_event(OrbitEvent::JobRunCompleted {
            job_id: run.job_id.clone(),
            run_id: run.run_id.clone(),
            state: final_state.to_string(),
        })
    }

    /// Reap a delivered run only after its terminal state is durable. Cleanup
    /// is best-effort: a retained report makes a failed Git safety check
    /// visible to `run show`, while the hourly GC routine remains the backstop
    /// for an infrastructure error here.
    fn record_delivered_worktree_cleanup(&self, run_id: &str) {
        let cleanup = match self.cleanup_delivered_worktree(run_id) {
            Ok(Some(result)) => serde_json::to_value(result).unwrap_or_else(|error| {
                json!({
                    "dry_run": false,
                    "bytes_reclaimed": 0,
                    "reports": [],
                    "error": format!("failed to serialize delivery cleanup report: {error}"),
                })
            }),
            Ok(None) => return,
            Err(error) => json!({
                "dry_run": false,
                "bytes_reclaimed": 0,
                "reports": [],
                "error": error.to_string(),
            }),
        };

        let Ok(Some(mut state)) = self.read_run_state(run_id) else {
            tracing::warn!(
                run_id,
                "delivery cleanup completed but its report could not read the pipeline state",
            );
            return;
        };
        if let Some(object) = state.pipeline.as_object_mut() {
            object.insert("worktree_cleanup".to_string(), cleanup);
            if let Err(error) = self.write_run_state(run_id, &state) {
                tracing::warn!(
                    run_id,
                    %error,
                    "delivery cleanup completed but its report could not be persisted",
                );
            }
        }
    }

    fn persist_v2_run_state(
        &self,
        run: &JobRun,
        input: &Value,
        result: &V2JobRunResult,
        final_state: JobRunState,
        options: V2RunFinalizationOptions,
    ) -> Result<(), OrbitError> {
        let mut state = self.read_run_state(&run.run_id)?.unwrap_or_else(|| {
            PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone())
        });
        state.sync_pipeline(result.pipeline.clone());
        // A resumed run starts from its source's hold; its own outcome
        // replaces it, so only a run that is held carries one.
        state.pull_drain.forge_hold = result.forge_hold.clone();
        // [ORB-10002] Per-step checkpoints already maintain step records for
        // this run; only fall back to the legacy single-summary step record
        // when no checkpoint was ever written, so a later `resume` never sees
        // a whole-run summary clobbering step 0's real checkpoint.
        if options.write_legacy_summary_step && state.step_states.is_empty() {
            state.record_step(0, final_state, Some(result.pipeline.clone()), None);
        }
        self.stores().jobs().write_run_state(&run.run_id, &state)
    }

    /// Record a terminal summary without promoting its output to a run error.
    fn record_synthetic_v2_terminal_step(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<chrono::Utc>,
        finished_at: chrono::DateTime<chrono::Utc>,
        output: Value,
        state: JobRunState,
    ) -> Result<(), OrbitError> {
        let duration_ms = Some(
            finished_at
                .signed_duration_since(started_at)
                .num_milliseconds()
                .max(0) as u64,
        );
        // Cancellation diagnostics used to append after any stored steps;
        // keep that history while successful summaries retain step 0.
        let step_index = if state == JobRunState::Cancelled {
            self.get_job_run_backend(&run.run_id)?
                .and_then(|current| current.steps.iter().map(|step| step.step_index).max())
                .map_or(0, |index| index.saturating_add(1) as usize)
        } else {
            0
        };
        self.stores().jobs().complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                lock_wait_ms: None,
                step_index,
                target_type: JobTargetType::Job,
                target_id: run.job_id.clone(),
                started_at,
                finished_at,
                duration_ms,
                exit_code: (state == JobRunState::Success).then_some(0),
                agent_response_json: Some(output),
                state,
                error_code: None,
                error_message: None,
            },
        )?;
        Ok(())
    }

    /// Persist a step summary on the run record for the detached worker path
    /// [ORB-12255]. Replay already writes a synthetic step 0; workers used to
    /// leave `JobRun::steps` empty and only reconstruct from audit at `run
    /// show`. MCP readers never reconstructed, so they saw `steps: []`.
    ///
    /// Audit-derived rows are preferred so the stored summary matches the
    /// reconstructed view. The synthetic job-level step is the fallback when
    /// the trail is empty, matching replay.
    fn persist_detached_worker_steps(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<chrono::Utc>,
        finished_at: chrono::DateTime<chrono::Utc>,
        result: &V2JobRunResult,
    ) -> Result<(), OrbitError> {
        let current = self.get_job_run_backend(&run.run_id)?;
        if current.is_some_and(|stored| !stored.steps.is_empty()) {
            return Ok(());
        }

        let audit_steps = self
            .collect_run_audit_steps(&run.run_id)
            .unwrap_or_default();
        if audit_steps.is_empty() {
            return self.record_synthetic_v2_terminal_step(
                run,
                started_at,
                finished_at,
                result.pipeline.clone(),
                JobRunState::Success,
            );
        }

        for step in audit_steps {
            let step_started = step.started_at.unwrap_or(started_at);
            let step_finished = step.finished_at.unwrap_or(finished_at);
            let duration_ms = Some(
                step_finished
                    .signed_duration_since(step_started)
                    .num_milliseconds()
                    .max(0) as u64,
            );
            let state = AuditStepOutcome::job_run_state(step.outcome.as_ref());
            self.stores().jobs().complete_job_run_step(
                &run.run_id,
                &JobRunStepParams {
                    lock_wait_ms: step.lock_wait_ms,
                    step_index: step.step_index as usize,
                    target_type: JobTargetType::Activity,
                    target_id: step.step_id,
                    started_at: step_started,
                    finished_at: step_finished,
                    duration_ms,
                    exit_code: None,
                    agent_response_json: None,
                    state,
                    error_code: None,
                    error_message: step
                        .error_message
                        .map(|m| orbit_common::security::redaction::redact_all(&m)),
                },
            )?;
        }
        Ok(())
    }
}

/// The audit label of a finished v2 job, using the same typed failure code
/// finalization holds on. A code-only hold whose message lacks the matching
/// text is `held`, not `failed` or `error`.
pub(crate) fn audit_terminal_outcome(
    job_id: &str,
    outcome: &Result<JobOutcome, OrbitError>,
) -> (&'static str, Option<String>) {
    let typed_code = outcome
        .as_ref()
        .err()
        .and_then(OrbitError::step_failure_code);
    match outcome {
        Ok(o) if o.parked.is_some() => ("parked", o.message.clone()),
        Ok(o) if o.evidence_hold.is_some() => ("held", None),
        Ok(o) if o.forge_hold.is_some() => ("held", o.message.clone()),
        Ok(o) if o.child_cancelled => ("cancelled", None),
        Ok(o) if o.success => ("success", None),
        Ok(o)
            if held_failure(
                job_id,
                o.failure_code.as_ref().map(|code| code.as_str()),
                o.message.as_deref(),
            )
            .is_some() =>
        {
            ("held", o.message.clone())
        }
        Ok(o) => ("failed", o.message.clone()),
        Err(err)
            if held_failure(
                job_id,
                delivery_refusal_code(err)
                    .or_else(|| typed_code.as_ref().map(|code| code.as_str())),
                Some(&err.to_string()),
            )
            .is_some() =>
        {
            ("held", Some(err.to_string()))
        }
        Err(err) => ("error", Some(err.to_string())),
    }
}

/// [ORB-10002] Re-key a source run's checkpoint state onto the resumed run.
///
/// Only the pipeline document's run id changes. Ownership fields inside step
/// outputs and the copied input (`job_run_id`, `batch_id`) stay the batch that
/// created the worktree: delivery and validation keep naming that owner, and
/// lineage reconcile restamps the task onto it. Rewriting those fields onto
/// the new run would make validation name a run that does not own the task.
/// A binding outside the lineage never reaches this copy; planning refuses it.
/// Forge-hold expiry is run-local and starts unacknowledged.
/// Interrupted-review preservation is run-local too: the source keeps its
/// record and repair ref, but neither suppresses preservation in the new run.
pub(in crate::application::job) fn seeded_resume_state(
    source_state: &PipelineState,
    run: &JobRun,
) -> PipelineState {
    let mut seeded = source_state.clone();
    seeded.run_id = run.run_id.clone();
    // [ORB-20158] The resumed run keeps the frozen definitions, admission
    // time, deadline, bindings and receipts; only the park belonged to the
    // source run's continuation.
    if let Some(checkpoint) = seeded.auto_task.as_mut() {
        checkpoint.parked = None;
    }
    seeded.job_id = run.job_id.clone();
    seeded.pull_drain.forge_hold_expired_at = None;
    // The resumed run's own outcome decides whether it echoes a child.
    seeded.root_cause = None;
    // A steering watermark names what this run's own prompts carried; the
    // resumed run's agents have seen nothing yet.
    seeded.steering_watermarks.clear();
    // Steering rounds are the source run's budget, not the resumed run's.
    seeded.steering_rounds.clear();
    if let Some(pipeline) = seeded.pipeline.as_object_mut() {
        pipeline.remove(orbit_engine::review_gate::INTERRUPTED_REVIEW_KEY);
    }
    seeded.updated_at = chrono::Utc::now();
    seeded
}
