//! Required-check reads, blocked-base diagnosis, failed-check recovery and
//! the post-update gate wait for one landing poll.

use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;
use serde_json::json;

use crate::context::{HandoffLandingStep, RuntimeHost};

use super::super::super::base::refresh::RefreshStep;
use super::super::super::checks::{
    CheckRecoveryStep, ReadWait, failed_checks, landing_log_read_wait, landing_read_wait,
    recover_failed_checks,
};
use super::super::super::operations;
use super::super::super::pr::diagnostics;
use super::super::super::pr::{
    BlockedCheckWait, OwedHostedChecks, PrMergeState, PullRequestStatus, checked_pr_state,
};
use super::super::super::remote_probe::fixed_wait as sleep;
use super::super::observe::landing_tip;
use super::super::record::{behind_base, describe, refuse_pr};
use super::super::run::transport_failure;
use super::poll::{LandingPoll, Step};

impl<'a, H: RuntimeHost + ?Sized> LandingPoll<'a, H> {
    /// Refines the state with the required checks and records whether the
    /// landing is only waiting for them.
    pub(super) fn read_required_checks(
        &mut self,
        status: &PullRequestStatus,
    ) -> Result<PrMergeState, OrbitError> {
        let context = self.context;
        let pr_number = &self.pr_number;
        let checked = checked_pr_state(
            self.host,
            &self.workspace,
            pr_number,
            status,
            BlockedCheckWait::Owner(&context.owed_hosted_checks),
            &mut self.unreported,
            (self.max_wait_seconds > 0).then_some(self.deadline_ms),
        );
        self.diagnostic["required_checks"] = self.unreported.required_observation.clone();
        self.diagnostic["unreported_wait"] = self.unreported.diagnostic();
        let state = match checked {
            Ok(state) => state,
            Err(reason) => {
                let message = format!("pull request #{pr_number} cannot be merged ({reason})");
                let error = reason
                    .into_error(|_| OrbitError::Execution(format!("handoff_land: {message}")));
                // Keep the provider's transport class until the outer loop
                // applies the shared deadline and the external-write fence.
                if transport_failure(&error) {
                    return Err(error);
                }
                self.diagnostic["classification"] = diagnostics::text("required_check_refusal");
                return Err(match error {
                    OrbitError::DeliveryRefused { refusal, .. } => {
                        self.hold(false, refusal, &message)?
                    }
                    _ => self.stop(false, &message)?,
                });
            }
        };
        self.diagnostic["classification"] = diagnostics::text(&describe(&state));
        self.waiting_for_checks = matches!(state, PrMergeState::Pending)
            && failed_checks(status).is_empty()
            && (self.unreported.awaiting_policy_checks()
                || status.status_checks.iter().flatten().any(|check| {
                    check.state == Some(super::super::super::pr::CheckState::Pending)
                }))
            && matches!(
                status.review_decision,
                super::super::super::pr::ReviewDecision::Approved
                    | super::super::super::pr::ReviewDecision::NotApplicable
            );
        Ok(state)
    }

    /// A required context the head has not reported may be one only the
    /// advanced base emits, so absent-context recovery still diagnoses it.
    pub(super) fn diagnose_blocked_base(
        &self,
        status: &PullRequestStatus,
        state: &PrMergeState,
        refresh_wait: &mut bool,
    ) -> Result<Step, OrbitError> {
        if *refresh_wait
            || !(matches!(state, PrMergeState::Blocked(_))
                || self.unreported.awaiting_policy_checks())
        {
            return Ok(Step::Proceed);
        }
        match self.refresh_base(status, true)? {
            RefreshStep::Changed => return Ok(Step::Continue),
            RefreshStep::Wait | RefreshStep::Adopted { waiting: true } => *refresh_wait = true,
            _ => {}
        }
        Ok(Step::Proceed)
    }

    /// [ORB-15956] A failed check, owed or required, is classified and
    /// recovered before anything stops on it.
    pub(super) fn recover_failed_checks(
        &mut self,
        status: &PullRequestStatus,
        state: &PrMergeState,
        owed: &OwedHostedChecks,
        waiting_on_recovery: &mut bool,
    ) -> Result<Step, OrbitError> {
        let (host, context) = (self.host, self.context);
        let pr_number = &self.pr_number;
        let failed = failed_checks(status);
        let check_reason = match (owed, state) {
            _ if matches!(
                state,
                PrMergeState::Closed | PrMergeState::Contradictory(_) | PrMergeState::Conflict
            ) || behind_base(status)
                || status.is_draft
                || status.merge_state_status.as_deref() == Some("DRAFT") =>
            {
                None
            }
            (OwedHostedChecks::Failed(reason), _) => Some(format!(
                "pull request #{pr_number}: {reason}; the review owed it to that check"
            )),
            (_, PrMergeState::Blocked(reason)) if !failed.is_empty() && !behind_base(status) => {
                Some(format!(
                    "pull request #{pr_number} cannot be merged ({reason})"
                ))
            }
            _ => None,
        };
        let Some(reason) = check_reason else {
            return Ok(Step::Proceed);
        };
        if failed.is_empty() {
            return Err(self.stop(
                false,
                &format!("{reason}, so the candidate stays in review"),
            )?);
        }
        let tip = landing_tip(context, self.view);
        // A rerun or branch update is an external write, and one that
        // failed may have been applied, so neither retries the pass.
        let recovered = recover_failed_checks(
            host,
            context,
            status,
            &failed,
            tip.as_deref(),
            &mut self.recovery,
            &reason,
        )
        .inspect_err(|_| context.wrote.set(true))?;
        if matches!(recovered, CheckRecoveryStep::Acted) {
            context.wrote.set(true);
        }
        if let Some(observation) = self.recovery.required_observation.take() {
            self.diagnostic["required_checks"] = observation;
        }
        if let Some(observation) = &self.recovery.rerun_diagnostic {
            self.diagnostic["ci_rerun_diagnostic"] = json!(observation);
        } else if let Some(diagnostic) = self.diagnostic.as_object_mut() {
            diagnostic.remove("ci_rerun_diagnostic");
        }
        if let Some(diagnostic) = self.diagnostic.as_object_mut() {
            diagnostic.remove("log_read_gap");
        }
        match recovered {
            CheckRecoveryStep::Acted => {
                self.recovery.waiting_for_base = None;
                return Ok(Step::Continue);
            }
            CheckRecoveryStep::Wait => *waiting_on_recovery = true,
            CheckRecoveryStep::WaitForBase(reason) => {
                self.diagnostic["base_suspicion"] = json!(reason);
                if self.recovery.waiting_for_base.as_deref() != Some(&reason) {
                    let note = json!({
                        "state": "waiting_for_base",
                        "reason": reason,
                        "head": status.head_sha,
                        "deadline_ms": self.deadline_ms,
                    });
                    host.record_handoff_landing_wait(&context.handoff_id, &note.to_string())?;
                    self.recovery.waiting_for_base = Some(reason);
                }
                *waiting_on_recovery = true;
            }
            CheckRecoveryStep::Repair(evidence) => return Err(self.stop(true, &evidence)?),
            CheckRecoveryStep::Requeue(evidence) => {
                return Err(refuse_pr(
                    host,
                    context,
                    &self.diagnostic,
                    HandoffLandingStep::StopForRequeue,
                    &evidence,
                )?);
            }
            CheckRecoveryStep::HoldForBase(evidence) => {
                return Err(self.hold(false, DeliveryRefusal::BaseChecksPending, &evidence)?);
            }
            CheckRecoveryStep::HoldInReview(evidence) => {
                return Err(self.hold(
                    false,
                    DeliveryRefusal::CheckEvidenceInconclusive,
                    &evidence,
                )?);
            }
            CheckRecoveryStep::Stop(evidence) => return Err(self.stop(false, &evidence)?),
            CheckRecoveryStep::LogsUnavailable(reason) => {
                self.diagnostic["log_read_gap"] = json!({"reason": reason});
                let now = super::super::super::completion_deadline::now_ms()?;
                let deadline_ms = if self.max_wait_seconds == 0 {
                    now
                } else {
                    self.deadline_ms
                };
                match landing_log_read_wait(
                    &reason,
                    &mut self.recovery.unavailable_log_waits,
                    status.head_sha.as_deref(),
                    now,
                    deadline_ms,
                ) {
                    ReadWait::Wait { delay_ms, note } => {
                        self.diagnostic["log_read_gap"] = note.clone();
                        host.record_handoff_landing_wait(&context.handoff_id, &note.to_string())?;
                        sleep(Duration::from_millis(delay_ms));
                        self.waited_seconds =
                            self.waited_seconds.saturating_add(delay_ms.div_ceil(1000));
                        return Ok(Step::Continue);
                    }
                    ReadWait::Hold(evidence) => {
                        return Err(self.hold(
                            false,
                            DeliveryRefusal::ForgeObservationUnavailable,
                            &evidence,
                        )?);
                    }
                }
            }
            CheckRecoveryStep::ReadDeferred(read) => {
                let now = super::super::super::completion_deadline::now_ms()?;
                match landing_read_wait(
                    &read,
                    &mut self.recovery.throttled_read_waits,
                    status.head_sha.as_deref(),
                    now,
                    self.deadline_ms,
                ) {
                    ReadWait::Wait { delay_ms, note } => {
                        self.diagnostic["read_deferral"] = note.clone();
                        host.record_handoff_landing_wait(&context.handoff_id, &note.to_string())?;
                        sleep(Duration::from_millis(delay_ms));
                        self.waited_seconds =
                            self.waited_seconds.saturating_add(delay_ms.div_ceil(1000));
                        return Ok(Step::Continue);
                    }
                    ReadWait::Hold(evidence) => {
                        return Err(self.hold(
                            false,
                            DeliveryRefusal::ForgeObservationUnavailable,
                            &evidence,
                        )?);
                    }
                }
            }
        }
        Ok(Step::Proceed)
    }

    /// After a branch update, a newly required context that has not
    /// succeeded is still a gate, including on a provider-reported CLEAN.
    pub(super) fn wait_for_current_gates(
        &mut self,
        status: &PullRequestStatus,
    ) -> Result<Step, OrbitError> {
        let current = self
            .host
            .run_private_vcs_operation(
                operations::CI_CURRENT_GATES,
                json!({"workspace_path":self.workspace,"pr":self.pr_number,
                    "head_sha":status.head_sha,"head_branch":status.head_branch,
                    "base_branch":status.base_branch,"deadline_ms":self.deadline_ms,
                    "poll_interval_seconds":self.poll_interval_seconds}),
            )
            .map_err(|error| self.stop_on(error))?;
        if current["gates_passed"] == true {
            return Ok(Step::Proceed);
        }
        if self.max_wait_seconds == 0 {
            return Err(self.timeout(&self.context.owed_hosted_checks)?);
        }
        self.wait_one_poll();
        Ok(Step::Continue)
    }
}
