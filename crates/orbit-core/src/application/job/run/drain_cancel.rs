//! Cancelling a drain whose leaves are still working.
//!
//! A follower pull drain's leaves are claims an owner is waiting on, so
//! cancelling the drain does not end them by default:
//!
//! - **Graceful** (the default for a running pull drain). The drain is marked
//!   `cancelling` (its state's `drain_cancel`): it stops requesting at once,
//!   its next pass releases every admission that never launched back to the
//!   owner's backlog, and it keeps passing until each launched leaf has
//!   finished and its settlement reached the owner. That pass ends the drain
//!   `cancelled` ([`OrbitRuntime::complete_graceful_drain_cancel`]). The
//!   request itself returns at once, naming the leaves it waits for.
//! - **Forced** (`--force`). The drain's process is stopped, each live leaf's
//!   claim is recorded as released before the leaf's process group is
//!   stopped, and every claim goes back to the owner's backlog with a comment
//!   naming the drain and the reason. Only the drain's own work is touched,
//!   never a leaf another live drain carries. A leaf whose stop cannot be
//!   confirmed keeps its claim (`unstopped_leaves`). If the drain worker
//!   itself cannot be confirmed stopped, cancellation fails before finalizing
//!   the drain or changing its carried claims. A local auto drain's
//!   `--force` also stops the children its cancel would otherwise detach.
//!
//! A drain that is queued, or whose worker is conclusively gone, has nothing
//! to wait for and is cancelled at once; its unlaunched claims are released
//! by the cancel's settle-only pass and its live leaves settle themselves.

use chrono::Utc;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::{
    ClaimMutation, LocalPullAdmission, LocalPullPhase, TaskReservationReleaseReason,
};
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState, RunStateUpdate};
use serde_json::json;

use super::actions::cancellation_result;
use super::owner::{
    RunOwnerLiveness, run_owner_liveness, run_owner_unstoppable_reason, signal_run_owner_confirmed,
};
use super::types::{JobRunCancelResult, UnstoppedLeaf};
use crate::OrbitRuntime;
use crate::application::distributed::PULL_DRAIN_JOB;

/// The local auto drain, whose leaves its cancel detaches.
const LOCAL_DRAIN_JOB: &str = "workspace_auto_pipeline";
const GRACEFUL_CANCEL_AUDIT: &str = "pipeline.run.cancel.graceful_requested";

impl OrbitRuntime {
    /// Cancel a run, choosing how a drain's in-flight leaves are treated.
    ///
    /// Without `force`, a running pull drain is cancelled gracefully (see the
    /// module docs) and the result's outcome is `cancelling`; repeating the
    /// request is harmless. With `force`, a pull drain's live claimed leaves
    /// are stopped and their claims released, and a local auto drain's
    /// detached children are cancelled with it. Every other run is cancelled
    /// as [`Self::cancel_job_run_with_reason`] does, whichever `force` says.
    pub fn cancel_job_run_with_options(
        &self,
        run_id: &str,
        actor: &str,
        source: &str,
        reason: Option<&str>,
        force: bool,
    ) -> Result<JobRunCancelResult, OrbitError> {
        let run = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
        if run.job_id == PULL_DRAIN_JOB {
            // Forcing an ended drain still stops the leaves it left running.
            if force {
                return self.force_cancel_pull_drain(&run, actor, source, reason);
            }
            if run.state == JobRunState::Running
                && run_owner_liveness(&run) != RunOwnerLiveness::Stopped
            {
                return self.request_graceful_drain_cancel(&run, actor, source, reason);
            }
        }
        if force && run.job_id == LOCAL_DRAIN_JOB {
            return self.force_cancel_local_drain(&run, actor, source, reason);
        }
        self.cancel_job_run_with_reason(run_id, actor, source, reason)
    }

    /// Mark a running pull drain `cancelling` and report what it waits for.
    fn request_graceful_drain_cancel(
        &self,
        run: &JobRun,
        actor: &str,
        source: &str,
        reason: Option<&str>,
    ) -> Result<JobRunCancelResult, OrbitError> {
        let mut requested = false;
        let mut terminal = false;
        let update = self.stores().jobs().update_run_state(
            &run.run_id,
            &mut |run_state: JobRunState, state: &mut PipelineState| {
                if run_state.is_terminal() {
                    terminal = true;
                    return Ok(());
                }
                requested =
                    state.set_drain_cancel(actor.into(), source.into(), reason.map(Into::into));
                Ok(())
            },
        )?;
        if terminal {
            // The drain finished while this request was on its way.
            return self.cancel_job_run_with_reason(&run.run_id, actor, source, reason);
        }
        match update {
            RunStateUpdate::Updated => {}
            RunStateUpdate::NotFound => {
                return Err(OrbitError::not_found(
                    NotFoundKind::JobRun,
                    run.run_id.clone(),
                ));
            }
            RunStateUpdate::NoState => {
                let mut state = PipelineState::new(
                    run.run_id.clone(),
                    run.job_id.clone(),
                    run.input.clone().unwrap_or_else(|| json!({})),
                );
                requested =
                    state.set_drain_cancel(actor.into(), source.into(), reason.map(Into::into));
                self.write_run_state(&run.run_id, &state)?;
            }
        }
        if requested {
            self.record_pipeline_audit(
                GRACEFUL_CANCEL_AUDIT,
                Some(&run.run_id),
                Some(actor),
                AuditEventStatus::Success,
                json!({
                    "run_id": run.run_id,
                    "actor": actor,
                    "source": source,
                    "reason": reason,
                    "requested_at": Utc::now().to_rfc3339(),
                }),
                None,
            )?;
        }
        let mut result =
            cancellation_result(run, "cancelling", run.state, false, None, actor, source);
        // Settlements already recorded go out now; the drain releases its
        // own unlaunched claims on its next pass.
        result.pull_settlements = self.settle_pending_pulls();
        result.waiting_leaves = self.pull_drain_claimed_leaves(&run.run_id)?;
        Ok(result)
    }

    /// End a gracefully cancelled pull drain `cancelled`, once its own pass
    /// found nothing left unsettled. Recorded like any cancellation, under
    /// the actor and reason the request carried.
    pub(crate) fn complete_graceful_drain_cancel(&self, run_id: &str) -> Result<(), OrbitError> {
        let run = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        if run.state.is_terminal() {
            return Ok(());
        }
        let cancel = self
            .read_run_state(run_id)?
            .and_then(|state| state.drain_cancel)
            .ok_or_else(|| {
                OrbitError::JobValidation(format!("job run '{run_id}' is not being cancelled"))
            })?;
        let (actor, source) = (cancel.actor.as_str(), cancel.source.as_str());
        let reason = cancel.reason.as_deref();
        let request_id = audit_execution_id("cancel");
        self.record_cancellation_request(&run, &request_id, actor, source)?;
        let now = Utc::now();
        let duration_ms = run
            .started_at
            .map(|started| now.signed_duration_since(started).num_milliseconds().max(0) as u64);
        let diagnostic = match reason {
            Some(reason) => format!("run cancelled by {actor}: {reason}"),
            None => format!("run cancelled by {actor}"),
        };
        self.finalize_job_run_with_reservation_cleanup_and_diagnostic(
            run_id,
            JobRunState::Cancelled,
            now,
            duration_ms,
            TaskReservationReleaseReason::RunTerminal,
            Some(("RUN_CANCELLED", &diagnostic)),
        )?;
        let cancelled = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        if cancelled.state != JobRunState::Cancelled {
            return self.record_cancellation_completion(
                &cancelled,
                &request_id,
                "already_terminal",
                None,
                None,
            );
        }
        self.record_run_cancelled_audit(&cancelled, &request_id, actor, source, reason, run.state)?;
        self.mark_cancelled_pipeline_state(&cancelled)?;
        self.record_event(OrbitEvent::JobRunCancelled {
            job_id: run.job_id.clone(),
            run_id: run_id.to_string(),
            previous_state: Some(run.state.to_string()),
            final_state: Some(JobRunState::Cancelled.to_string()),
            actor: Some(actor.to_string()),
            source: Some(source.to_string()),
            signal_attempted: Some(false),
            signal_outcome: None,
        })?;
        self.record_cancellation_completion(&cancelled, &request_id, "cancelled", None, None)
    }

    /// Stop a pull drain and every live leaf it carries, releasing each claim
    /// to the owner's backlog with a comment naming the drain and reason. A
    /// drain that already ended is reported `already_terminal`, and the
    /// leaves it left running are still stopped.
    ///
    /// Only the drain's own work is touched ([`Self::pull_drain_admissions`],
    /// read before the drain stops): never a leaf another live drain carries.
    /// A leaf whose stop cannot be confirmed keeps its claim on the owner and
    /// is reported in `unstopped_leaves`. If the drain worker cannot be
    /// confirmed stopped, the request fails before finalizing the drain or
    /// releasing any carried claim.
    fn force_cancel_pull_drain(
        &self,
        run: &JobRun,
        actor: &str,
        source: &str,
        reason: Option<&str>,
    ) -> Result<JobRunCancelResult, OrbitError> {
        let carried = self
            .pull_drain_admissions(&run.run_id)?
            .into_iter()
            .map(|record| (record.destination, record.request.request_id))
            .collect::<Vec<_>>();
        // Confirm the drain stopped before finalizing it or releasing claims:
        // a non-stopping signal outcome must not let it keep admitting work.
        let mut result = self.cancel_job_run_cascading(
            &run.run_id,
            actor,
            source,
            reason,
            signal_run_owner_confirmed,
            0,
        )?;
        let cause = match reason {
            Some(reason) => format!(
                "drain {} was cancelled with --force by {actor}: {reason}",
                run.run_id
            ),
            None => format!("drain {} was cancelled with --force by {actor}", run.run_id),
        };
        let jobs = self.stores().jobs();
        let still_carried = || -> Result<Vec<LocalPullAdmission>, OrbitError> {
            Ok(jobs
                .unsettled_local_pull_admissions()?
                .into_iter()
                .filter(|record| {
                    carried.iter().any(|(to, id)| {
                        *to == record.destination && *id == record.request.request_id
                    })
                })
                .collect())
        };
        for record in still_carried()? {
            let Some(leaf) = record.leaf_run_id.clone() else {
                continue;
            };
            let launched = matches!(
                record.phase,
                LocalPullPhase::Launching | LocalPullPhase::Launched
            );
            let Some(leaf_run) = self
                .get_job_run_backend(&leaf)?
                .filter(|leaf| !leaf.state.is_terminal())
            else {
                continue;
            };
            if !launched {
                continue;
            }
            match self.stop_released_leaf(&record, &leaf_run, actor, source, reason, &cause) {
                Ok(true) => result.forced_runs.push(leaf),
                Ok(false) => {}
                Err(why) => {
                    tracing::warn!(
                        target: "orbit.core.job_run",
                        drain = %run.run_id,
                        leaf = %leaf,
                        reason = %why,
                        "forced drain cancel could not confirm a claimed leaf stopped",
                    );
                    result.unstopped_leaves.push(UnstoppedLeaf {
                        leaf_run_id: leaf,
                        task_id: record
                            .receipt
                            .as_ref()
                            .and_then(|receipt| receipt.claim.as_ref())
                            .map(|claim| claim.task_id.clone()),
                        reason: why,
                    });
                }
            }
        }
        // Whatever never launched goes back to the owner's backlog under the
        // same cause; recorded settlements, the releases above among them,
        // are delivered.
        result.pull_settlements = self.carry_settlements_for(still_carried()?, &cause);
        Ok(result)
    }

    /// Release a live leaf's claim and stop the leaf. `Ok(false)` when the
    /// leaf had already recorded its own settlement (its handoff): that one
    /// is delivered and the leaf, which is finishing, is left alone.
    ///
    /// A leaf this process cannot stop and see gone is refused before
    /// anything is recorded, so its claim stays with the owner. The release
    /// is recorded before the stop, so the leaf's terminalization finds the
    /// claim decided rather than failing it; a stop that then cannot be
    /// confirmed leaves the release held — never delivered while the leaf
    /// still runs — until the leaf is seen to stop. Either is the `Err`
    /// reason.
    fn stop_released_leaf(
        &self,
        record: &LocalPullAdmission,
        leaf: &JobRun,
        actor: &str,
        source: &str,
        reason: Option<&str>,
        cause: &str,
    ) -> Result<bool, String> {
        if leaf.state == JobRunState::Running
            && let Some(why) = run_owner_unstoppable_reason(leaf)
        {
            return Err(format!(
                "not stopped: {why}; its claim stays with the owner"
            ));
        }
        let settling = self
            .record_forced_leaf_release(
                record,
                &format!("{cause}; its leaf {} was stopped", leaf.run_id),
            )
            .map_err(|error| {
                format!("not stopped: its release could not be recorded ({error}); its claim stays with the owner")
            })?;
        if !matches!(settling.settlement, Some(ClaimMutation::Release(_))) {
            return Ok(false);
        }
        self.cancel_job_run_cascading(
            &leaf.run_id,
            actor,
            source,
            reason,
            signal_run_owner_confirmed,
            0,
        )
        .map_err(|error| {
            format!(
                "stop unconfirmed: {error}; its release is held, and reaches the owner only \
                 once the leaf is seen to stop"
            )
        })?;
        Ok(true)
    }

    /// Cancel a local auto drain and the detached children its cancel would
    /// otherwise leave running.
    fn force_cancel_local_drain(
        &self,
        run: &JobRun,
        actor: &str,
        source: &str,
        reason: Option<&str>,
    ) -> Result<JobRunCancelResult, OrbitError> {
        let children = self
            .read_run_state(&run.run_id)?
            .map(|state| {
                state
                    .open_child_dispatches()
                    .map(|dispatch| dispatch.child_run_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut result = self.cancel_job_run_with_reason(&run.run_id, actor, source, reason)?;
        for child in children {
            let live = self
                .get_job_run_backend(&child)?
                .is_some_and(|child| !child.state.is_terminal());
            if !live {
                continue;
            }
            match self.cancel_job_run_with_reason(&child, actor, source, reason) {
                Ok(_) => result.forced_runs.push(child),
                Err(error) => tracing::warn!(
                    target: "orbit.core.job_run",
                    drain = %run.run_id,
                    child = %child,
                    %error,
                    "forced drain cancel could not cancel a detached child",
                ),
            }
        }
        Ok(result)
    }
}
