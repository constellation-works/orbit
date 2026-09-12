//! Stale-run reconciliation, terminal timing repair, and audit helpers.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::process::identity::{ProcessLiveness, is_stable_token, probe_process_liveness};
use orbit_store::contracts::TaskReservationReleaseReason;
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::{JobRun, JobRunState};

use crate::OrbitRuntime;

use super::owner::{
    owner_identity_error_code, pending_run_stale_reason, running_run_owner_is_stale,
    running_run_owner_stale_reason, stale_job_run_message, stale_pending_run_message,
};

/// Call-scoped reuse of healthy owner classifications.
///
/// Keyed by run/owner identity so a later row with a different pid, token, or
/// state is classified again. Stale verdicts are never stored: orphan
/// finalization always rereads the durable row. The set lives only for one
/// list/history call.
#[derive(Default)]
pub(super) struct ReconcilePass {
    healthy: Vec<OwnerSnapshotKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnerSnapshotKey {
    run_id: String,
    state: JobRunState,
    pid: Option<u32>,
    pid_start_time: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderFinalizationGuard {
    Clear,
    Alive,
    Unknown,
}

impl ReconcilePass {
    fn key(run: &JobRun) -> OwnerSnapshotKey {
        OwnerSnapshotKey {
            run_id: run.run_id.clone(),
            state: run.state,
            pid: run.pid,
            pid_start_time: run.pid_start_time.clone(),
        }
    }

    fn has_healthy(&self, run: &JobRun) -> bool {
        self.healthy.contains(&Self::key(run))
    }

    fn remember_healthy(&mut self, run: &JobRun) {
        self.healthy.push(Self::key(run));
    }
}

impl OrbitRuntime {
    /// [ORB-10002] Best-effort orphan scan at workspace open. Marks stuck
    /// `running` runs with a conclusively-dead owner as `interrupted`; when
    /// owner liveness cannot be determined confidently the run is left alone
    /// (see `owner::classify_run_owner`). Never fails runtime construction.
    /// [ORB-10070] The same scan finalizes orphaned `pending` runs (claimed
    /// worker conclusively gone, or never claimed past the grace window) so a
    /// terminal parent's queued children cannot stay `pending` forever.
    pub(crate) fn reconcile_stale_job_runs_on_open(&self) {
        match self.reconcile_stale_job_runs(None) {
            Ok(0) => {}
            Ok(reconciled) => {
                tracing::info!(
                    target: "orbit.core.job_run",
                    reconciled,
                    "orphan scan at workspace open reconciled interrupted job runs",
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.job_run",
                    error = %error,
                    "orphan scan at workspace open failed; stale runs will be \
                     reconciled lazily by job run queries",
                );
            }
        }
    }

    /// Read-only orphan probe for `orbit doctor`: `running` runs whose
    /// recorded owner process is conclusively gone, without mutating any
    /// state. The mutating counterpart is [`Self::reconcile_stale_job_runs`].
    /// `pub` for the workspace doctor in `orbit-cmd` [ORB-10016].
    pub fn list_orphaned_running_job_runs(&self) -> Result<Vec<JobRun>, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .list_all_pending_or_running_runs()?
            .into_iter()
            .filter(|run| run.state == JobRunState::Running && running_run_owner_is_stale(run))
            .collect())
    }

    /// [ORB-10070] Read-only orphan probe for `orbit doctor`: `pending` runs
    /// with no live worker (claimed owner conclusively gone, or never claimed
    /// past the grace window), without mutating any state.
    pub fn list_orphaned_pending_job_runs(&self) -> Result<Vec<JobRun>, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .list_all_pending_or_running_runs()?
            .into_iter()
            .filter(|run| pending_run_stale_reason(run).is_some())
            .collect())
    }

    pub(crate) fn reconcile_stale_job_runs(
        &self,
        job_id: Option<&str>,
    ) -> Result<usize, OrbitError> {
        self.reconcile_stale_job_runs_with_pass(job_id, &mut ReconcilePass::default())
    }

    pub(super) fn reconcile_stale_job_runs_with_pass(
        &self,
        job_id: Option<&str>,
        pass: &mut ReconcilePass,
    ) -> Result<usize, OrbitError> {
        let runs = if let Some(job_id) = job_id {
            self.stores()
                .jobs()
                .list_pending_or_running_job_runs(job_id)?
        } else {
            self.stores().jobs().list_all_pending_or_running_runs()?
        };

        let mut reconciled = 0usize;
        for run in runs {
            if self.reconcile_stale_job_run_with_pass(&run, pass)? {
                reconciled += 1;
            }
        }
        Ok(reconciled)
    }

    pub(crate) fn reconcile_stale_job_run(&self, run: &JobRun) -> Result<bool, OrbitError> {
        self.reconcile_stale_job_run_with_pass(run, &mut ReconcilePass::default())
    }

    fn reconcile_stale_job_run_with_pass(
        &self,
        run: &JobRun,
        pass: &mut ReconcilePass,
    ) -> Result<bool, OrbitError> {
        self.reconcile_stale_job_run_before_revalidation(run, pass, || {})
    }

    fn reconcile_stale_job_run_before_revalidation<F>(
        &self,
        run: &JobRun,
        pass: &mut ReconcilePass,
        before_revalidation: F,
    ) -> Result<bool, OrbitError>
    where
        F: FnOnce(),
    {
        self.reconcile_stale_job_run_before_revalidation_with_provider_probe(
            run,
            pass,
            probe_process_liveness,
            before_revalidation,
        )
    }

    fn reconcile_stale_job_run_before_revalidation_with_provider_probe<F, P>(
        &self,
        run: &JobRun,
        pass: &mut ReconcilePass,
        provider_probe: P,
        before_revalidation: F,
    ) -> Result<bool, OrbitError>
    where
        F: FnOnce(),
        P: Fn(u32, Option<&str>) -> ProcessLiveness,
    {
        if terminal_run_timing_is_incomplete(run) {
            return self.repair_terminal_job_run_timing(run);
        }
        if pass.has_healthy(run) {
            return Ok(false);
        }
        if stale_job_run_diagnostic(run).is_none() {
            if !run.state.is_terminal() {
                pass.remember_healthy(run);
            }
            return Ok(false);
        }
        if !self.provider_evidence_allows_orphan_finalization(&run.run_id, &provider_probe) {
            return Ok(false);
        }

        before_revalidation();
        self.finalize_orphaned_job_run_with_provider_probe(&run.run_id, &provider_probe)
    }

    /// Deterministic seam for reproducing a terminal writer winning after the
    /// stale snapshot was classified but before reconciliation revalidates it.
    #[cfg(test)]
    pub(super) fn reconcile_stale_job_run_after_classification<F>(
        &self,
        run: &JobRun,
        concurrent_completion: F,
    ) -> Result<bool, OrbitError>
    where
        F: FnOnce(),
    {
        self.reconcile_stale_job_run_before_revalidation(
            run,
            &mut ReconcilePass::default(),
            concurrent_completion,
        )
    }

    /// Deterministic seam for provider evidence changing after the candidate
    /// snapshot is classified but before the final write-boundary reread.
    #[cfg(test)]
    pub(super) fn reconcile_stale_job_run_with_provider_probe_after_classification<F, P>(
        &self,
        run: &JobRun,
        provider_probe: P,
        concurrent_provider_change: F,
    ) -> Result<bool, OrbitError>
    where
        F: FnOnce(),
        P: Fn(u32, Option<&str>) -> ProcessLiveness,
    {
        self.reconcile_stale_job_run_before_revalidation_with_provider_probe(
            run,
            &mut ReconcilePass::default(),
            provider_probe,
            concurrent_provider_change,
        )
    }

    /// [ORB-10002] Orphaned runs (owner process conclusively gone) become
    /// `interrupted`, not `failed`: the job did not fail, its worker died.
    /// Interrupted runs are resumable from their step checkpoints via
    /// `orbit job resume <run_id>`.
    fn finalize_orphaned_job_run_with_provider_probe<P>(
        &self,
        run_id: &str,
        provider_probe: &P,
    ) -> Result<bool, OrbitError>
    where
        P: Fn(u32, Option<&str>) -> ProcessLiveness,
    {
        // [ORB-11116] The scan's candidate snapshot can go stale while a real
        // worker is terminalizing. Re-read at the interruption boundary and
        // re-run both state and owner classification so a completed run never
        // receives a later interruption attempt (or its conflict diagnostic).
        let Some(current) = self.get_job_run_backend(run_id)? else {
            return Ok(false);
        };
        let Some((error_code, message)) = stale_job_run_diagnostic(&current) else {
            return Ok(false);
        };
        if !self.provider_evidence_allows_orphan_finalization(&current.run_id, provider_probe) {
            return Ok(false);
        }

        let finished_at = self.orphaned_run_finished_at(&current);
        let duration_ms = current.started_at.map(|started_at| {
            finished_at
                .signed_duration_since(started_at)
                .num_milliseconds()
                .max(0) as u64
        });
        let changed = self.finalize_job_run_with_reservation_cleanup(
            &current.run_id,
            JobRunState::Interrupted,
            finished_at,
            duration_ms,
            TaskReservationReleaseReason::StaleRunReconciled,
        )?;
        if !changed {
            return Ok(false);
        }

        let Some(finalized) = self.get_job_run_backend(&current.run_id)? else {
            return Ok(false);
        };
        if finalized.state != JobRunState::Interrupted || finalized.finished_at.is_none() {
            return Ok(false);
        }

        let step_started_at = current.started_at.unwrap_or(current.scheduled_at);
        let _ = self.record_pipeline_diagnostic_step(
            &current,
            step_started_at,
            finished_at,
            Some(&error_code),
            &message,
            JobRunState::Interrupted,
        );
        self.record_event(OrbitEvent::JobRunCompleted {
            job_id: current.job_id.clone(),
            run_id: current.run_id.clone(),
            state: JobRunState::Interrupted.to_string(),
        })?;
        Ok(true)
    }

    /// A stale owner is not sufficient to condemn a run while any durable,
    /// still-open provider child is alive or cannot be verified. The audit
    /// collector reads the complete trail and process probing verifies the
    /// recorded start token and PID namespace before returning `Alive`.
    fn provider_evidence_allows_orphan_finalization<P>(
        &self,
        run_id: &str,
        provider_probe: &P,
    ) -> bool
    where
        P: Fn(u32, Option<&str>) -> ProcessLiveness,
    {
        let processes = match self.collect_run_provider_processes_with(run_id, provider_probe) {
            Ok(processes) => processes,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.job_run",
                    run_id,
                    owner = "stale",
                    error = %error,
                    decision = "defer",
                    reason = "provider_evidence_unreadable",
                    "orphan finalization provider guard deferred",
                );
                return false;
            }
        };

        let mut open = 0usize;
        let mut alive = 0usize;
        let mut unknown = 0usize;
        for process in &processes {
            if process.finished {
                continue;
            }
            open += 1;
            match process.liveness {
                ProcessLiveness::Alive
                    if process
                        .pid_start_time
                        .as_deref()
                        .is_some_and(is_stable_token) =>
                {
                    alive += 1;
                }
                ProcessLiveness::Alive => unknown += 1,
                ProcessLiveness::Unknown => unknown += 1,
                ProcessLiveness::Exited => {}
            }
        }
        let guard = if alive > 0 {
            ProviderFinalizationGuard::Alive
        } else if unknown > 0 {
            ProviderFinalizationGuard::Unknown
        } else {
            ProviderFinalizationGuard::Clear
        };
        let (decision, reason) = match guard {
            ProviderFinalizationGuard::Clear => ("finalize", "providers_closed_or_absent"),
            ProviderFinalizationGuard::Alive => ("defer", "provider_alive"),
            ProviderFinalizationGuard::Unknown => ("defer", "provider_liveness_unknown"),
        };
        tracing::debug!(
            target: "orbit.core.job_run",
            run_id,
            owner = "stale",
            providers = processes.len(),
            open,
            alive,
            unknown,
            decision,
            reason,
            "orphan finalization provider guard classified durable evidence",
        );

        guard == ProviderFinalizationGuard::Clear
    }

    /// When an orphaned run actually stopped doing work.
    ///
    /// [ORB-10594] The sweep can notice a dead owner arbitrarily long after the
    /// fact — orphan scans only run when some process opens the workspace — so
    /// stamping `Utc::now()` records the moment of *detection*, not the end of
    /// work, and inflates `duration_ms` by the whole detection lag. The run's
    /// own audit trail stops when its work stopped, so its last event is the
    /// better estimate. Falls back to now when the run left no trail, and
    /// never accepts a timestamp outside `[started_at, now]` so a clock skew or
    /// a back-dated event cannot produce a negative or absurd duration.
    fn orphaned_run_finished_at(&self, run: &JobRun) -> DateTime<Utc> {
        let now = Utc::now();
        let floor = run.started_at.unwrap_or(run.scheduled_at);
        self.last_run_activity_at(&run.run_id)
            .unwrap_or_else(|error| {
                tracing::debug!(
                    target: "orbit.core.job_run",
                    run_id = %run.run_id,
                    error = %error,
                    "orphaned run audit trail unreadable; stamping detection time",
                );
                None
            })
            .filter(|activity| *activity >= floor && *activity <= now)
            .unwrap_or(now)
    }

    /// Timestamp of the most recent audit event recorded for a run.
    fn last_run_activity_at(&self, run_id: &str) -> Result<Option<DateTime<Utc>>, OrbitError> {
        self.latest_run_audit_timestamp(run_id)
    }

    pub(super) fn reconcile_job_run_records_with_pass(
        &self,
        runs: &[JobRun],
        pass: &mut ReconcilePass,
    ) -> Result<usize, OrbitError> {
        let mut reconciled = 0usize;
        for run in runs {
            if self.reconcile_stale_job_run_with_pass(run, pass)? {
                reconciled += 1;
            }
        }
        Ok(reconciled)
    }

    pub(super) fn list_reconciled_job_history_backend(
        &self,
        job_id: &str,
        pass: &mut ReconcilePass,
    ) -> Result<Vec<JobRun>, OrbitError> {
        let runs = self.list_job_history_backend(job_id)?;
        if self.reconcile_job_run_records_with_pass(&runs, pass)? > 0 {
            self.list_job_history_backend(job_id)
        } else {
            Ok(runs)
        }
    }

    fn repair_terminal_job_run_timing(&self, run: &JobRun) -> Result<bool, OrbitError> {
        let finished_at = match run.finished_at {
            Some(value) => value,
            None => self
                .run_finished_at_from_audit(&run.run_id)?
                .unwrap_or_else(Utc::now),
        };
        let duration_ms = run.duration_ms.or_else(|| {
            run.started_at.map(|started_at| {
                finished_at
                    .signed_duration_since(started_at)
                    .num_milliseconds()
                    .max(0) as u64
            })
        });
        self.stores()
            .jobs()
            .repair_terminal_job_run_timing(&run.run_id, finished_at, duration_ms)
    }

    fn run_finished_at_from_audit(
        &self,
        run_id: &str,
    ) -> Result<Option<DateTime<Utc>>, OrbitError> {
        for event in self.collect_run_audit_events(run_id)? {
            if matches!(event.event_type.as_deref(), Some("run.finished"))
                || matches!(event.body_kind.as_deref(), Some("run_finished"))
            {
                return Ok(event.timestamp);
            }
        }
        Ok(None)
    }
}

/// The diagnostic for a currently stale non-terminal run. Keeping
/// classification and message construction together lets reconciliation
/// repeat the authoritative decision against a fresh durable row.
fn stale_job_run_diagnostic(run: &JobRun) -> Option<(String, String)> {
    // [ORB-10070] Orphaned queued runs (claimed worker conclusively gone, or
    // never claimed past the grace window) finalize exactly like orphaned
    // running runs. Each diagnostic is built from one classification result.
    if let Some(reason) = pending_run_stale_reason(run) {
        return Some((
            reason.error_code().to_string(),
            stale_pending_run_message(run, reason),
        ));
    }
    let stale_reason = running_run_owner_stale_reason(run)?;
    Some((
        owner_identity_error_code(Some(stale_reason)).to_string(),
        stale_job_run_message(run, Some(stale_reason)),
    ))
}

fn terminal_run_timing_is_incomplete(run: &JobRun) -> bool {
    run.state.is_terminal()
        && (run.finished_at.is_none() || (run.duration_ms.is_none() && run.started_at.is_some()))
}
