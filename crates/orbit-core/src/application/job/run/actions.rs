//! Cancellation, archive, delete, and run-state helpers for job runs.

#[cfg(unix)]
use std::collections::{HashMap, HashSet};

use chrono::Utc;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{NotFoundKind, OrbitError};
#[cfg(unix)]
use orbit_store::contracts::AuditEventFilter;
use orbit_store::contracts::TaskReservationReleaseReason;
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    ChildCancellation, ChildCancellationPolicy, JobRun, JobRunState, PipelineState,
};
use serde_json::Value;

use crate::OrbitRuntime;

use super::owner::signal_run_owner_process;
use super::types::JobRunCancelResult;

/// `source` recorded on a cancellation a parent propagated to its child.
const CHILD_CASCADE_SOURCE: &str = "parent_cascade";
/// Backstop against a cycle in persisted dispatch records.
const MAX_CHILD_CANCEL_CASCADE_DEPTH: usize = 8;
pub(crate) const CANCELLATION_REQUEST_AUDIT: &str = "pipeline.run.cancel.requested";
pub(crate) const CANCELLATION_SIGNAL_AUDIT: &str = "pipeline.run.cancel.signal_acknowledged";
pub(crate) const CANCELLATION_COMPLETION_AUDIT: &str = "pipeline.run.cancel.completed";
#[cfg(unix)]
pub(crate) const CANCELLATION_WORKER_EXIT_AUDIT: &str = "pipeline.run.cancel.worker_exit";

impl OrbitRuntime {
    pub fn cancel_job_run(&self, run_id: &str) -> Result<JobRunCancelResult, OrbitError> {
        self.cancel_job_run_with_context(run_id, "system", "runtime")
    }

    pub fn cancel_job_run_with_context(
        &self,
        run_id: &str,
        actor: &str,
        source: &str,
    ) -> Result<JobRunCancelResult, OrbitError> {
        self.cancel_job_run_with_signaller(run_id, actor, source, signal_run_owner_process)
    }

    /// Internal cancellation seam so tests can model a failed post-signal
    /// liveness check without signalling a process they do not own.
    pub(super) fn cancel_job_run_with_signaller<F>(
        &self,
        run_id: &str,
        actor: &str,
        source: &str,
        signal: F,
    ) -> Result<JobRunCancelResult, OrbitError>
    where
        F: FnOnce(&JobRun) -> Result<String, OrbitError>,
    {
        self.cancel_job_run_cascading(run_id, actor, source, signal, 0)
    }

    fn cancel_job_run_cascading<F>(
        &self,
        run_id: &str,
        actor: &str,
        source: &str,
        signal: F,
        depth: usize,
    ) -> Result<JobRunCancelResult, OrbitError>
    where
        F: FnOnce(&JobRun) -> Result<String, OrbitError>,
    {
        let run = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        let request_id = audit_execution_id("cancel");

        if run.state.is_terminal() {
            self.record_cancellation_request(&run, &request_id, actor, source)?;
            self.record_cancellation_completion(&run, &request_id, "already_terminal", None, None)?;
            return Ok(cancellation_result(
                &run,
                "already_terminal",
                run.state,
                false,
                None,
                actor,
                source,
            ));
        }
        run.state
            .try_transition(orbit_types::workflow::RunEvent::Cancel)
            .map_err(|msg| {
                OrbitError::JobValidation(format!("cannot cancel job run '{}': {}", run_id, msg))
            })?;
        self.record_cancellation_request(&run, &request_id, actor, source)?;
        let signal_attempted = run.state == JobRunState::Running && run.pid.is_some();
        let signal_outcome = if signal_attempted {
            match signal(&run) {
                Ok(outcome) => {
                    self.record_cancellation_signal_acknowledgement(&run, &request_id, &outcome)?;
                    Some(outcome)
                }
                Err(error) => {
                    let _ = self.record_cancellation_completion(
                        &run,
                        &request_id,
                        "failed",
                        None,
                        Some(&error.to_string()),
                    );
                    return Err(error);
                }
            }
        } else {
            None
        };

        // The worker can reach a real terminal outcome while its owner is
        // being signalled. That outcome is authoritative: cancellation lost
        // the race and is an idempotent already-terminal result, not a second
        // terminalization attempt or a terminal-outcome conflict.
        let after_signal = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        if after_signal.state.is_terminal() {
            self.record_cancellation_completion(
                &after_signal,
                &request_id,
                "already_terminal",
                signal_outcome.as_deref(),
                None,
            )?;
            return Ok(cancellation_result(
                &run,
                "already_terminal",
                after_signal.state,
                signal_attempted,
                signal_outcome,
                actor,
                source,
            ));
        }

        let now = chrono::Utc::now();
        let duration_ms = run
            .started_at
            .map(|s| now.signed_duration_since(s).num_milliseconds().max(0) as u64);
        self.finalize_job_run_with_reservation_cleanup(
            run_id,
            JobRunState::Cancelled,
            now,
            duration_ms,
            TaskReservationReleaseReason::RunTerminal,
        )?;
        let cancelled_run = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        if cancelled_run.state != JobRunState::Cancelled {
            if cancelled_run.state.is_terminal() {
                self.record_cancellation_completion(
                    &cancelled_run,
                    &request_id,
                    "already_terminal",
                    signal_outcome.as_deref(),
                    None,
                )?;
                return Ok(cancellation_result(
                    &run,
                    "already_terminal",
                    cancelled_run.state,
                    signal_attempted,
                    signal_outcome,
                    actor,
                    source,
                ));
            }
            let detail = cancelled_run
                .state
                .try_transition(orbit_types::workflow::RunEvent::Cancel)
                .err()
                .unwrap_or_else(|| {
                    format!(
                        "stored state remained {} after cancellation",
                        cancelled_run.state
                    )
                });
            return Err(OrbitError::JobValidation(format!(
                "cannot cancel job run '{}': {}",
                run_id, detail
            )));
        }
        self.mark_cancelled_pipeline_state(&cancelled_run)?;
        self.settle_child_dispatches_on_cancel(&cancelled_run, actor, depth)?;
        self.record_event(OrbitEvent::JobRunCancelled {
            job_id: run.job_id.clone(),
            run_id: run_id.to_string(),
            previous_state: Some(run.state.to_string()),
            final_state: Some(JobRunState::Cancelled.to_string()),
            actor: Some(actor.to_string()),
            source: Some(source.to_string()),
            signal_attempted: Some(signal_attempted),
            signal_outcome: signal_outcome.clone(),
        })?;
        self.record_cancellation_completion(
            &cancelled_run,
            &request_id,
            "cancelled",
            signal_outcome.as_deref(),
            None,
        )?;
        Ok(cancellation_result(
            &run,
            "cancelled",
            JobRunState::Cancelled,
            signal_attempted,
            signal_outcome,
            actor,
            source,
        ))
    }

    fn record_cancellation_request(
        &self,
        run: &JobRun,
        request_id: &str,
        actor: &str,
        source: &str,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_audit(
            CANCELLATION_REQUEST_AUDIT,
            Some(&run.run_id),
            Some(actor),
            AuditEventStatus::Success,
            serde_json::json!({
                "request_id": request_id,
                "run_id": run.run_id,
                "requested_state": run.state.to_string(),
                "owner_pid": run.pid,
                "owner_pid_start_time": run.pid_start_time,
                "actor": actor,
                "source": source,
                "requested_at": Utc::now().to_rfc3339(),
            }),
            None,
        )
    }

    fn record_cancellation_signal_acknowledgement(
        &self,
        run: &JobRun,
        request_id: &str,
        signal_outcome: &str,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_audit(
            CANCELLATION_SIGNAL_AUDIT,
            Some(&run.run_id),
            None,
            AuditEventStatus::Success,
            serde_json::json!({
                "request_id": request_id,
                "run_id": run.run_id,
                "signal_outcome": signal_outcome,
                "acknowledged_at": Utc::now().to_rfc3339(),
            }),
            None,
        )
    }

    fn record_cancellation_completion(
        &self,
        run: &JobRun,
        request_id: &str,
        outcome: &str,
        signal_outcome: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_audit(
            CANCELLATION_COMPLETION_AUDIT,
            Some(&run.run_id),
            None,
            if outcome == "failed" {
                AuditEventStatus::Failure
            } else {
                AuditEventStatus::Success
            },
            serde_json::json!({
                "request_id": request_id,
                "run_id": run.run_id,
                "outcome": outcome,
                "observed_state": run.state.to_string(),
                "signal_outcome": signal_outcome,
                "completed_at": Utc::now().to_rfc3339(),
            }),
            error.map(str::to_string),
        )
    }

    /// The newest cancellation request that has not conclusively failed or
    /// observed a pre-existing terminal outcome. Worker observers use this to
    /// avoid converting an expected TERM/KILL exit into a run failure before
    /// the signalling caller verifies that every owned target stopped.
    #[cfg(unix)]
    pub(crate) fn active_job_run_cancellation_request(
        &self,
        run_id: &str,
    ) -> Result<Option<String>, OrbitError> {
        let audits = self.list_audit_events_filtered(&AuditEventFilter {
            job_run_id: Some(run_id.to_string()),
            limit: 200,
            ..AuditEventFilter::default()
        })?;
        let mut completions = HashMap::<String, String>::new();
        let mut requests = Vec::new();
        for audit in audits {
            let Some(tool) = audit.tool_name.as_deref() else {
                continue;
            };
            if !matches!(
                tool,
                CANCELLATION_REQUEST_AUDIT | CANCELLATION_COMPLETION_AUDIT
            ) {
                continue;
            }
            let Some(arguments) = audit
                .arguments_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            else {
                continue;
            };
            let Some(request_id) = arguments.get("request_id").and_then(Value::as_str) else {
                continue;
            };
            if tool == CANCELLATION_COMPLETION_AUDIT {
                if let Some(outcome) = arguments.get("outcome").and_then(Value::as_str) {
                    completions
                        .entry(request_id.to_string())
                        .or_insert_with(|| outcome.to_string());
                }
            } else {
                requests.push(request_id.to_string());
            }
        }
        let inactive: HashSet<&str> = completions
            .iter()
            .filter_map(|(request_id, outcome)| {
                matches!(outcome.as_str(), "failed" | "already_terminal")
                    .then_some(request_id.as_str())
            })
            .collect();
        Ok(requests
            .into_iter()
            .find(|request_id| !inactive.contains(request_id.as_str())))
    }

    pub fn archive_job_run(&self, run_id: &str) -> Result<(), OrbitError> {
        let run = self.show_job_run(run_id)?;
        if matches!(run.state, JobRunState::Pending | JobRunState::Running) {
            return Err(OrbitError::JobValidation(format!(
                "job run '{}' is active and cannot be archived",
                run_id
            )));
        }
        let job_id = self.stores().jobs().archive_job_run(run_id)?;
        self.record_event(OrbitEvent::JobRunArchived {
            job_id,
            run_id: run_id.to_string(),
        })
    }

    pub fn delete_job_run(&self, run_id: &str) -> Result<(), OrbitError> {
        if let Some(run) = self.get_job_run_backend(run_id)? {
            self.reconcile_stale_job_run(&run)?;
        }
        if let Some(run) = self.get_job_run_backend(run_id)?
            && matches!(run.state, JobRunState::Pending | JobRunState::Running)
        {
            return Err(OrbitError::JobValidation(format!(
                "job run '{}' is active and cannot be deleted",
                run_id
            )));
        }
        let job_id = self.stores().jobs().delete_job_run(run_id)?;
        self.record_event(OrbitEvent::JobRunDeleted {
            job_id,
            run_id: run_id.to_string(),
        })
    }

    pub fn read_run_state(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::PipelineState>, OrbitError> {
        self.stores().jobs().read_run_state(run_id)
    }

    /// Persist a run's pipeline state. The write side of [`Self::read_run_state`],
    /// for callers outside the store layer that own a read-modify-write of the
    /// run's own state — dispatch checkpoints, waiting reasons, cancellation.
    pub fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError> {
        self.stores().jobs().write_run_state(run_id, state)
    }

    /// Apply this run's child-cancellation policy and close every dispatch it
    /// still holds open [ORB-10971].
    ///
    /// **The policy.** A child the parent was *blocking* on cascades: the
    /// parent's wait was that child's only consumer, so leaving it running
    /// would produce work nobody joins and no operator expects. A child
    /// dispatched *detached* does not: it was submitted precisely to outlive
    /// the parent's step (`workspace_auto_pipeline` starts an epic that way),
    /// and its own drain re-observes it. The dispatch record carries which
    /// shape it was, so the rule is decided by how the child was dispatched
    /// rather than by whoever happens to be cancelling.
    ///
    /// Either way the linkage survives and the wait step stops rendering as
    /// `running`: a cancelled parent must still name the child it left behind,
    /// which is the only handle on that work.
    ///
    /// Cascading is best effort. A child that already terminalized, or that
    /// refuses cancellation, is recorded as such and never blocks the parent's
    /// own cancellation from completing.
    fn settle_child_dispatches_on_cancel(
        &self,
        run: &JobRun,
        actor: &str,
        depth: usize,
    ) -> Result<(), OrbitError> {
        let Some(state) = self.read_run_state(&run.run_id)? else {
            return Ok(());
        };
        let open: Vec<(String, ChildCancellationPolicy)> = state
            .open_child_dispatches()
            .map(|dispatch| {
                (
                    dispatch.child_run_id.clone(),
                    dispatch.cancellation_policy(),
                )
            })
            .collect();
        if open.is_empty() {
            return Ok(());
        }

        let settled: Vec<(String, ChildCancellation)> = open
            .into_iter()
            .map(|(child_run_id, policy)| {
                let (outcome, error) = match policy {
                    ChildCancellationPolicy::Detach => ("detached".to_string(), None),
                    ChildCancellationPolicy::Cascade => {
                        self.cascade_cancel_child(&child_run_id, actor, depth)
                    }
                };
                (
                    child_run_id,
                    ChildCancellation {
                        policy,
                        outcome,
                        error,
                        at: Utc::now(),
                    },
                )
            })
            .collect();

        // Re-read: cascading a child re-enters cancellation, and the parent's
        // own state must be stamped from whatever that left behind.
        let Some(mut state) = self.read_run_state(&run.run_id)? else {
            return Ok(());
        };
        for (child_run_id, cancellation) in settled {
            state.terminalize_child_dispatch(&child_run_id, cancellation);
        }
        self.write_run_state(&run.run_id, &state)
    }

    /// Cancel one blocking child, reporting what happened rather than failing
    /// the parent's cancellation.
    fn cascade_cancel_child(
        &self,
        child_run_id: &str,
        actor: &str,
        depth: usize,
    ) -> (String, Option<String>) {
        // A dispatch graph is a DAG in practice, so this bound is a backstop
        // against a corrupted state file, not an expected limit.
        if depth >= MAX_CHILD_CANCEL_CASCADE_DEPTH {
            return (
                "skipped".to_string(),
                Some(format!(
                    "child cancellation cascade stopped at depth {depth}"
                )),
            );
        }
        match self.cancel_job_run_cascading(
            child_run_id,
            actor,
            CHILD_CASCADE_SOURCE,
            signal_run_owner_process,
            depth + 1,
        ) {
            Ok(result) if result.outcome == "already_terminal" => (
                result.outcome,
                Some(format!(
                    "job run '{}' was already terminal ({})",
                    result.run_id, result.final_state
                )),
            ),
            Ok(result) => (result.outcome, None),
            // A child that reached a terminal state on its own is the ordinary
            // race, not a fault: the parent simply lost it to the finish line.
            Err(OrbitError::JobValidation(message))
            | Err(OrbitError::JobRunStateTransition(message)) => {
                ("already_terminal".to_string(), Some(message))
            }
            Err(error) => ("failed".to_string(), Some(error.to_string())),
        }
    }

    /// Cancelling a run preserves its child lineage. `clear_waiting_reasons`
    /// deliberately does not touch `child_dispatches`: dependency and lock
    /// waits are momentary and meaningless once terminal, but the child a
    /// parent dispatched outlives the parent's own record of waiting for it.
    fn mark_cancelled_pipeline_state(&self, run: &JobRun) -> Result<(), OrbitError> {
        if let Some(mut state) = self.read_run_state(&run.run_id)? {
            if let Some(object) = state.pipeline.as_object_mut() {
                object.insert(
                    "status".to_string(),
                    Value::String(JobRunState::Cancelled.to_string()),
                );
                object.insert(
                    "state".to_string(),
                    Value::String(JobRunState::Cancelled.to_string()),
                );
                object.insert("cancelled".to_string(), Value::Bool(true));
            }
            state.clear_waiting_reasons();
            state.updated_at = Utc::now();
            self.write_run_state(&run.run_id, &state)?;
        } else if run.input.is_some() {
            let mut state = PipelineState::new(
                run.run_id.clone(),
                run.job_id.clone(),
                run.input
                    .clone()
                    .unwrap_or_else(|| Value::Object(Default::default())),
            );
            if let Some(object) = state.pipeline.as_object_mut() {
                object.insert(
                    "status".to_string(),
                    Value::String(JobRunState::Cancelled.to_string()),
                );
                object.insert(
                    "state".to_string(),
                    Value::String(JobRunState::Cancelled.to_string()),
                );
                object.insert("cancelled".to_string(), Value::Bool(true));
            }
            self.write_run_state(&run.run_id, &state)?;
        }
        Ok(())
    }
}

fn cancellation_result(
    run: &JobRun,
    outcome: &str,
    final_state: JobRunState,
    signal_attempted: bool,
    signal_outcome: Option<String>,
    actor: &str,
    source: &str,
) -> JobRunCancelResult {
    JobRunCancelResult {
        run_id: run.run_id.clone(),
        outcome: outcome.to_string(),
        previous_state: run.state.to_string(),
        final_state: final_state.to_string(),
        actor: actor.to_string(),
        source: source.to_string(),
        signal_attempted,
        signal_outcome,
    }
}
