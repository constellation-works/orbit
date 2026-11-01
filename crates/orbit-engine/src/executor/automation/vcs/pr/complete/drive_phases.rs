//! Named phases of the PR completion poll loop, split from `drive.rs`.
//!
//! Each phase returns a typed flow outcome so the poll loop only sequences
//! phases; hold, wait and merge state lives on [`PollDriver`] and is mutated
//! inside one phase at a time.

use std::path::Path;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;
use orbit_types::workflow::handoff::HandoffCandidate;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::remote_probe::fixed_wait as sleep;
use super::super::answers::{PullRequestStatus, read_pr_status, read_pr_status_before};
use super::super::auto_fix::{
    AutoFix, BlockedStopReason, BlockedVerdict, CiRerunRecovery, blocked_verdict,
    defer_throttled_read, hold_auto_fix_stop, owed_read_wait, record_base_path, red_base_wait,
    stopped,
};
use super::super::delivery::{
    BlockedCheckWait, DeliveryPin, PrMergeState, UnreportedChecks, checked_pr_state,
    classify_pr_state,
};
use super::super::head_lag::HeadLag;
use super::super::merge::PermittedMerge;
use super::ci_rerun_hold::{CiRerunHold, CiRerunHoldStep, CiRerunRecoveryOutcome};
use super::conflict::is_blocked_merge_state;
use super::drive::Delivery;
use super::mutation::observation_failure;
use super::reviewed::{
    ensure_pr_head_is_reviewed, ensure_pr_head_matches_selection, merge_wait_timeout,
};

/// Synchronous merge attempts ceiling shared with the merge phase.
pub(super) const BASE_MODIFIED_MERGE_ATTEMPTS: u32 = 3;

/// One observed poll: the PR status and the state derived from it.
pub(super) struct ObservedPoll {
    pub(super) status: PullRequestStatus,
    pub(super) state: PrMergeState,
    pub(super) pending_required: bool,
    pub(super) infrastructure_wait: bool,
    pub(super) base_red_reason: Option<String>,
}

/// Outcome of the observe phase.
pub(super) enum ObserveFlow {
    ContinuePoll,
    Ready(Box<ObservedPoll>),
}

/// Outcome of the recovery phase.
pub(super) enum RecoveryFlow {
    ContinuePoll,
    Ready,
}

/// Outcome of the dispatch phase.
pub(super) enum DispatchFlow {
    ContinuePoll,
    Deliver(Box<Delivery>),
    WaitPoll(Option<String>),
}

/// Outcome of a dispatch sub-phase.
pub(super) enum SubFlow {
    ContinuePoll,
    Deliver(Box<Delivery>),
    WaitPoll,
}

/// Mutable poll state shared by the phases, owned by one driver.
pub(super) struct PollDriver<'a, H: RuntimeHost + ?Sized> {
    pub(super) host: &'a H,
    pub(super) input: &'a Value,
    pub(super) workspace_path: &'a str,
    pub(super) pr_number: &'a str,
    pub(super) auto_fix: Option<&'a AutoFix>,
    pub(super) diagnostic:
        &'a mut Option<super::super::super::landing::checks::rerun_diagnostic::CiRerunDiagnostic>,
    pub(super) max_wait_seconds: u64,
    pub(super) poll_interval_seconds: u64,
    pub(super) completion_started_ms: u64,
    pub(super) completion_deadline_ms: u64,
    pub(super) waited_seconds: u64,
    pub(super) waited_before: u64,
    pub(super) observation_budget: Option<u64>,
    pub(super) pin: DeliveryPin,
    pub(super) head_lag: HeadLag,
    pub(super) base_candidate: Option<HandoffCandidate>,
    pub(super) base_adopted: bool,
    pub(super) reviewed_head_sha: Option<String>,
    pub(super) selective_head_sha: Option<String>,
    pub(super) watch_only: bool,
    pub(super) unsettled_polls: u32,
    pub(super) unreported: UnreportedChecks,
    pub(super) ci_rerun_hold: CiRerunHold,
    pub(super) auto_merge_requested: bool,
    pub(super) merge_requested: bool,
    pub(super) requested_landed_commit: Option<String>,
    pub(super) conflict_refresh_attempted: bool,
    pub(super) landed_refresh_attempted: bool,
    pub(super) merge_capabilities: Option<PermittedMerge>,
    pub(super) base_modified_refusals: u32,
    pub(super) base_race: Option<String>,
    pub(super) mutated: bool,
}

impl<H: RuntimeHost + ?Sized> PollDriver<'_, H> {
    /// Run the poll loop until a phase delivers or fails.
    pub(super) fn run(mut self) -> Result<Delivery, OrbitError> {
        loop {
            let mut observed = match self.observe_poll()? {
                ObserveFlow::ContinuePoll => continue,
                ObserveFlow::Ready(observed) => *observed,
            };
            match self.recover_blocked_checks(&mut observed)? {
                RecoveryFlow::ContinuePoll => continue,
                RecoveryFlow::Ready => {}
            }
            match self.dispatch_state(observed)? {
                DispatchFlow::ContinuePoll => continue,
                DispatchFlow::Deliver(delivery) => return Ok(*delivery),
                DispatchFlow::WaitPoll(base_red_reason) => {
                    self.wait_for_next_poll(base_red_reason.as_deref())?
                }
            }
        }
    }

    /// Phase 1: read the PR, pin its candidate and classify its merge state.
    fn observe_poll(&mut self) -> Result<ObserveFlow, OrbitError> {
        let host = self.host;
        let input = self.input;
        let workspace_path = self.workspace_path;
        let pr_number = self.pr_number;
        let auto_fix = self.auto_fix;
        let max_wait_seconds = self.max_wait_seconds;
        let completion_started_ms = self.completion_started_ms;
        let completion_deadline_ms = self.completion_deadline_ms;
        let observation_budget = self.observation_budget;
        let elapsed_seconds = super::super::super::completion_deadline::now_ms()?
            .saturating_sub(completion_started_ms)
            / 1000;
        self.waited_seconds = self
            .waited_seconds
            .max(self.waited_before.saturating_add(elapsed_seconds));
        // Every flag is set immediately before a `continue` or at the end of
        // an iteration, so this covers every read made in the iteration.
        // Durable write-ahead provenance covers mutations inside it.
        let mutated = self.merge_requested
            || self.auto_merge_requested
            || self.conflict_refresh_attempted
            || self.landed_refresh_attempted;
        self.mutated = mutated;
        let observed = |error| observation_failure(host, input, mutated, observation_budget, error);
        let status = if max_wait_seconds == 0 {
            read_pr_status(host, Path::new(workspace_path), pr_number)
        } else {
            read_pr_status_before(
                host,
                Path::new(workspace_path),
                pr_number,
                completion_deadline_ms,
            )
        }
        .map_err(observed)?;
        let has_failures = !super::super::super::landing::checks::failed_checks(&status).is_empty();
        if self
            .diagnostic
            .as_ref()
            .is_some_and(|previous| previous.head_sha != status.head_sha || !has_failures)
        {
            *self.diagnostic = None;
        }
        if let Some(auto_fix) = auto_fix {
            super::super::super::landing::checks::rerun_diagnostic::observe_ci_rerun_head(
                host,
                &auto_fix.task_ids,
                &auto_fix.run_id,
                status.head_sha.as_deref(),
                has_failures,
            );
        }
        let mut state = classify_pr_state(&status);
        let mut base_refresh_wait = false;
        if let Some(auto_fix) = auto_fix {
            self.pin.ensure_candidate_identity(&status, pr_number)?;
            // Honor a durable wait before any head/base log diagnosis, including
            // conflict and resumed paths. The freshly read head was pinned above.
            if has_failures
                && let Some(head) = status.head_sha.as_deref()
                && let Some(wait_ms) =
                    owed_read_wait(host, auto_fix, pr_number, head, observation_budget)?
            {
                sleep(Duration::from_millis(wait_ms));
                return Ok(ObserveFlow::ContinuePoll);
            }
            if self.base_candidate.is_none() {
                self.base_candidate = super::super::super::base::refresh::owner_candidate(
                    Path::new(workspace_path),
                    input,
                    &self.pin,
                    &status,
                )?;
            }
            if let Some(candidate) = &self.base_candidate {
                match super::super::super::base::refresh::recover(
                    host,
                    Path::new(workspace_path),
                    &auto_fix.task_ids,
                    &auto_fix.run_id,
                    candidate,
                    &status,
                    completion_deadline_ms,
                    false,
                    None,
                )
                .map_err(observed)?
                {
                    super::super::super::base::refresh::RefreshStep::Changed => {
                        return Ok(ObserveFlow::ContinuePoll);
                    }
                    super::super::super::base::refresh::RefreshStep::Wait => {
                        base_refresh_wait = true
                    }
                    super::super::super::base::refresh::RefreshStep::Adopted { waiting } => {
                        if !self.base_adopted {
                            self.pin
                                .adopt_refreshed_candidate(status.head_sha.as_deref());
                        }
                        self.base_adopted = true;
                        base_refresh_wait = waiting;
                    }
                    super::super::super::base::refresh::RefreshStep::None => {}
                }
            }
        }
        if matches!(
            state,
            PrMergeState::Mergeable
                | PrMergeState::Pending
                | PrMergeState::Conflict
                | PrMergeState::Blocked(_)
        ) {
            self.pin.ensure_candidate_identity(&status, pr_number)?;
            if let Some(delay) = self.head_lag.poll_delay(
                &status,
                &self.pin,
                self.reviewed_head_sha.as_deref(),
                workspace_path,
                pr_number,
                max_wait_seconds.saturating_sub(self.waited_seconds),
            )? {
                sleep(Duration::from_secs(delay));
                self.waited_seconds = self.waited_seconds.saturating_add(delay);
                return Ok(ObserveFlow::ContinuePoll);
            }
            self.pin.ensure_pinned_candidate(&status, pr_number)?;
            if !self.base_adopted {
                ensure_pr_head_is_reviewed(&status, pr_number, self.reviewed_head_sha.as_deref())?;
                ensure_pr_head_matches_selection(
                    &status,
                    pr_number,
                    self.selective_head_sha.as_deref(),
                )?;
            }
        }
        state = checked_pr_state(
            host,
            Path::new(workspace_path),
            pr_number,
            &status,
            if auto_fix.is_some() {
                BlockedCheckWait::AutoFix
            } else {
                BlockedCheckWait::Owner(&[])
            },
            &mut self.unreported,
            (max_wait_seconds > 0).then_some(completion_deadline_ms),
        )
        .map_err(|refusal| observed(refusal.into_error(|reason| stopped(pr_number, reason))))?;
        if !base_refresh_wait
            && !self.base_adopted
            && (matches!(state, PrMergeState::Blocked(_))
                || self.unreported.awaiting_policy_checks())
            && let (Some(auto_fix), Some(candidate)) = (auto_fix, self.base_candidate.as_ref())
        {
            match super::super::super::base::refresh::recover(
                host,
                Path::new(workspace_path),
                &auto_fix.task_ids,
                &auto_fix.run_id,
                candidate,
                &status,
                completion_deadline_ms,
                true,
                None,
            )
            .map_err(observed)?
            {
                super::super::super::base::refresh::RefreshStep::Changed => {
                    return Ok(ObserveFlow::ContinuePoll);
                }
                super::super::super::base::refresh::RefreshStep::Wait
                | super::super::super::base::refresh::RefreshStep::Adopted { waiting: true } => {
                    base_refresh_wait = true
                }
                _ => {}
            }
        }
        let pending_required = matches!(state, PrMergeState::Pending);
        let infrastructure_wait = base_refresh_wait;
        // Set only when this poll's failed checks are an inherited red base:
        // the base has not moved, or it moved to a tip that cannot prove the
        // failure fixed yet. A later poll that is no longer that hold must
        // not keep the reason.
        let mut base_red_reason: Option<String> = None;
        // A conflicting PR can still carry an inherited or base-suspect
        // check failure. Decide that hold before admitting a rebase repair.
        if matches!(state, PrMergeState::Conflict)
            && let Some(auto_fix) = auto_fix
        {
            let failed = super::super::super::landing::checks::failed_checks(&status);
            if !failed.is_empty()
                && let Some(reason) = {
                    match red_base_wait(
                        host,
                        auto_fix,
                        input,
                        workspace_path,
                        pr_number,
                        &status,
                        &failed,
                        observation_budget,
                    ) {
                        Ok(reason) => reason,
                        Err(deferred) => {
                            let wait_ms = defer_throttled_read(
                                host,
                                auto_fix,
                                pr_number,
                                &deferred,
                                observation_budget,
                                mutated,
                            )?;
                            sleep(Duration::from_millis(wait_ms));
                            return Ok(ObserveFlow::ContinuePoll);
                        }
                    }
                }
            {
                record_base_path(
                    host,
                    auto_fix,
                    "pr_auto_fix_base_red",
                    &json!({"failed_head":status.head_sha,"reason":reason}),
                )?;
                base_red_reason = Some(reason);
            }
        }
        Ok(ObserveFlow::Ready(Box::new(ObservedPoll {
            status,
            state,
            pending_required,
            infrastructure_wait,
            base_red_reason,
        })))
    }

    /// Phase 2: CI rerun recovery, infrastructure recovery and the hold.
    fn recover_blocked_checks(
        &mut self,
        observed_poll: &mut ObservedPoll,
    ) -> Result<RecoveryFlow, OrbitError> {
        let host = self.host;
        let input = self.input;
        let workspace_path = self.workspace_path;
        let pr_number = self.pr_number;
        let auto_fix = self.auto_fix;
        let completion_deadline_ms = self.completion_deadline_ms;
        let observation_budget = self.observation_budget;
        let mutated = self.mutated;
        let observed = |error| observation_failure(host, input, mutated, observation_budget, error);
        let status = &observed_poll.status;
        let pending_required = observed_poll.pending_required;
        let base_refresh_wait = observed_poll.infrastructure_wait;
        let mut infrastructure_wait = base_refresh_wait;
        if let Some(auto_fix) = auto_fix
            && !base_refresh_wait
            && matches!(
                observed_poll.state,
                PrMergeState::Mergeable | PrMergeState::Pending | PrMergeState::Blocked(_)
            )
        {
            let verdict = blocked_verdict(status, self.unsettled_polls, pending_required);
            let diagnose = is_blocked_merge_state(status)
                && matches!(verdict, BlockedVerdict::RepairChecks(_));
            if let BlockedVerdict::Stop(BlockedStopReason::CheckRefusal(reason)) = &verdict {
                let error = stopped(pr_number, &reason.to_string());
                return Err(match reason.delivery_refusal() {
                    Some(code) => hold_auto_fix_stop(error, code),
                    None => error,
                });
            }
            if diagnose {
                let retry = super::super::auto_fix::recover_ci_rerun(
                    host,
                    auto_fix,
                    input,
                    workspace_path,
                    pr_number,
                    status,
                    completion_deadline_ms,
                    &mut *self.diagnostic,
                );
                let waited_seconds = self.waited_seconds;
                let last_poll = waited_seconds.saturating_add(self.poll_interval_seconds)
                    >= self.max_wait_seconds;
                let outcome = match &retry {
                    Ok(CiRerunRecovery::Waiting(reason)) => {
                        CiRerunRecoveryOutcome::Waiting(reason.as_str())
                    }
                    Ok(CiRerunRecovery::Retried) => CiRerunRecoveryOutcome::Retried,
                    Ok(CiRerunRecovery::NotEligible) => CiRerunRecoveryOutcome::NotEligible,
                    Ok(CiRerunRecovery::Stopped(reason)) => {
                        CiRerunRecoveryOutcome::Stopped(reason.as_str())
                    }
                    Err(error) => {
                        let deferred = self
                            .diagnostic
                            .as_ref()
                            .and_then(|observed_diag| observed_diag.throttled_log_read(error));
                        match deferred {
                            Some(deferred) => {
                                let _ = self.ci_rerun_hold.apply_recovery(
                                    host,
                                    auto_fix,
                                    status.head_sha.as_deref(),
                                    CiRerunRecoveryOutcome::ThrottledReadDeferred,
                                    waited_seconds,
                                    last_poll,
                                )?;
                                let wait_ms = defer_throttled_read(
                                    host,
                                    auto_fix,
                                    pr_number,
                                    &deferred,
                                    observation_budget,
                                    mutated,
                                )?;
                                sleep(Duration::from_millis(wait_ms));
                                return Ok(RecoveryFlow::ContinuePoll);
                            }
                            None => {
                                // The existing evidence/hold route retains its
                                // typed refusal and any recorded base
                                // inheritance. A failed planner read grants no
                                // CI mutation permission.
                                tracing::debug!(%error, "CI retry read deferred to existing check evidence gates");
                                CiRerunRecoveryOutcome::ReadFailed
                            }
                        }
                    }
                };
                let step = self.ci_rerun_hold.apply_recovery(
                    host,
                    auto_fix,
                    status.head_sha.as_deref(),
                    outcome,
                    waited_seconds,
                    last_poll,
                )?;
                match step {
                    CiRerunHoldStep::Hold(holds) => {
                        infrastructure_wait |= holds;
                    }
                    CiRerunHoldStep::Stop(reason) => {
                        return Err(stopped(pr_number, &reason));
                    }
                    CiRerunHoldStep::PreserveHold => {}
                }
            }
            infrastructure_wait |= matches!(
                super::super::infrastructure::recover(
                    host,
                    auto_fix,
                    workspace_path,
                    pr_number,
                    status,
                    diagnose,
                    completion_deadline_ms.min(
                        super::super::super::completion_deadline::now_ms()?.saturating_add(
                            self.max_wait_seconds
                                .saturating_sub(self.waited_seconds)
                                .saturating_mul(1000)
                        )
                    ),
                )
                .map_err(observed)?,
                super::super::infrastructure::InfrastructureStep::Wait
            );
        }
        observed_poll.infrastructure_wait = infrastructure_wait;
        Ok(RecoveryFlow::Ready)
    }

    /// Phase 3: act on the classified merge state.
    fn wait_for_next_poll(&mut self, base_red_reason: Option<&str>) -> Result<(), OrbitError> {
        let host = self.host;
        let _ = host;
        let pr_number = self.pr_number;
        let max_wait_seconds = self.max_wait_seconds;
        let poll_interval_seconds = self.poll_interval_seconds;
        if self.waited_seconds >= max_wait_seconds {
            if let Some(reason) = base_red_reason {
                return Err(hold_auto_fix_stop(
                    stopped(pr_number, reason),
                    DeliveryRefusal::BaseChecksPending,
                ));
            }
            return Err(merge_wait_timeout(
                pr_number,
                self.waited_seconds,
                max_wait_seconds,
                self.base_race.as_deref(),
                &self.unreported,
            ));
        }
        let remaining_seconds = max_wait_seconds.saturating_sub(self.waited_seconds);
        let sleep_seconds = poll_interval_seconds.min(remaining_seconds);
        sleep(Duration::from_secs(sleep_seconds));
        self.waited_seconds = self.waited_seconds.saturating_add(sleep_seconds);

        if self.waited_seconds >= max_wait_seconds {
            if let Some(reason) = base_red_reason {
                return Err(hold_auto_fix_stop(
                    stopped(pr_number, reason),
                    DeliveryRefusal::BaseChecksPending,
                ));
            }
            return Err(merge_wait_timeout(
                pr_number,
                self.waited_seconds,
                max_wait_seconds,
                self.base_race.as_deref(),
                &self.unreported,
            ));
        }
        Ok(())
    }
}
