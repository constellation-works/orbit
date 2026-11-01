//! Dispatch sub-phases of the PR completion poll: blocked, conflict,
//! mergeable and pending states.

use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::workflow::{DeliveryRefusal, PrAutoFixBlocker};
use serde_json::json;

use crate::context::RuntimeHost;

use super::super::super::landing::checks::CheckReadGap;
use super::super::super::push::push_batch_changes;
use super::super::super::remote_probe::fixed_wait as sleep;
use super::super::answers::{PullRequestStatus, ReviewedMergeOutcome};
use super::super::auto_fix::{
    BlockedStopReason, BlockedVerdict, CheckRepair, base_tip_checks, blocked_verdict,
    defer_throttled_read, hold_auto_fix_stop, record_base_path, record_landed_repair,
    record_published_repair, red_base_wait, repair_failed_checks, reserve_base_refresh, stopped,
};
use super::super::delivery::PrMergeState;
use super::conflict::{
    admit_conflict_repair, completion_conflict_error, has_completion_recovery_checkpoint,
    is_blocked_merge_state, re_review_on_conflict, rebase_conflicting_pr_branch,
    refresh_conflicting_pr_branch,
};
use super::drive::{DeliveredMerge, Delivery};
use super::drive_phases::{
    BASE_MODIFIED_MERGE_ATTEMPTS, DispatchFlow, ObservedPoll, PollDriver, SubFlow,
};
use super::mutation::{observation_failure, request_merge, resolved_capabilities};
use super::reviewed::merge_condition_sha;

impl<H: RuntimeHost + ?Sized> PollDriver<'_, H> {
    pub(super) fn dispatch_state(
        &mut self,
        observed: ObservedPoll,
    ) -> Result<DispatchFlow, OrbitError> {
        let ObservedPoll {
            status,
            state,
            pending_required,
            infrastructure_wait,
            base_red_reason,
        } = observed;
        let mut base_red_reason = base_red_reason;
        if base_red_reason.is_some() || infrastructure_wait {
            return Ok(DispatchFlow::WaitPoll(base_red_reason));
        }
        let flow = match state {
            PrMergeState::Merged if self.watch_only => {
                return Ok(DispatchFlow::Deliver(Box::new(Delivery::Watched(json!({
                    "merged": true,
                    "reason": "auto_fix_merged_elsewhere",
                    "pr_number": self.pr_number,
                    "head_sha": status.head_sha,
                    "waited_seconds": self.waited_seconds,
                })))));
            }
            PrMergeState::Merged => {
                let evidence = self.pin.ensure_delivered(&status, self.pr_number)?;
                let landed_commit = status.exact_merge_commit.as_deref();
                let managed_merge = self
                    .requested_landed_commit
                    .as_deref()
                    .is_some_and(|requested| Some(requested) == landed_commit);
                return Ok(DispatchFlow::Deliver(Box::new(Delivery::Merged(
                    DeliveredMerge {
                        outcome: json!({
                            "merged": true,
                            "pr_number": self.pr_number,
                            "strategy": self.merge_capabilities.as_ref().map(|capabilities| capabilities.strategy.as_str()),
                            "auto_merge_requested": self.auto_merge_requested,
                            "waited_seconds": self.waited_seconds,
                            "max_wait_seconds": self.max_wait_seconds,
                            "poll_interval_seconds": self.poll_interval_seconds,
                            "landed_commit": landed_commit,
                            "managed_merge": managed_merge,
                            "base_modified_refusals": self.base_modified_refusals,
                            "stale_head_observations": self.head_lag.observations,
                            "reviewed_head_sha": self.reviewed_head_sha,
                            "delivery_evidence": evidence.as_json(),
                        }),
                        evidence,
                    },
                ))));
            }
            PrMergeState::Closed => {
                return Err(OrbitError::DeliveryRefused {
                    refusal: DeliveryRefusal::PullRequestClosed,
                    message: format!(
                        "pr_complete: pull request #{} was closed without being merged; the task stays in review",
                        self.pr_number
                    ),
                });
            }
            PrMergeState::Contradictory(reason) => {
                return Err(OrbitError::DeliveryRefused {
                    refusal: DeliveryRefusal::EvidenceStale,
                    message: format!(
                        "delivery_evidence_stale: pull request #{} reports a contradictory merge state ({reason}); completion needs an unambiguous merged state, so the task stays in review",
                        self.pr_number
                    ),
                });
            }
            PrMergeState::Blocked(reason) => {
                self.dispatch_blocked(status, reason, pending_required, &mut base_red_reason)?
            }
            PrMergeState::Conflict => self.dispatch_conflict(status)?,
            PrMergeState::Mergeable if self.watch_only => {
                return Ok(DispatchFlow::Deliver(Box::new(Delivery::Watched(json!({
                    "merged": false,
                    "reason": "auto_fix_mergeable",
                    "pr_number": self.pr_number,
                    "head_sha": status.head_sha,
                    "waited_seconds": self.waited_seconds,
                    "stale_head_observations": self.head_lag.observations,
                })))));
            }
            PrMergeState::Mergeable => self.dispatch_mergeable(status)?,
            PrMergeState::Pending => self.dispatch_pending(status)?,
        };
        match flow {
            SubFlow::ContinuePoll => Ok(DispatchFlow::ContinuePoll),
            SubFlow::Deliver(delivery) => Ok(DispatchFlow::Deliver(delivery)),
            SubFlow::WaitPoll => Ok(DispatchFlow::WaitPoll(base_red_reason)),
        }
    }

    fn dispatch_blocked(
        &mut self,
        status: PullRequestStatus,
        reason: super::super::merge_state::PrMergeBlock,
        pending_required: bool,
        base_red_reason: &mut Option<String>,
    ) -> Result<SubFlow, OrbitError> {
        let host = self.host;
        let input = self.input;
        let workspace_path = self.workspace_path;
        let pr_number = self.pr_number;
        let auto_fix = self.auto_fix;
        let observation_budget = self.observation_budget;
        let completion_deadline_ms = self.completion_deadline_ms;
        let mutated = self.mutated;
        let Some(auto_fix) = auto_fix.filter(|_| is_blocked_merge_state(&status)) else {
            let diagnosis = self
                .pin
                .base_advance_note(input, workspace_path)
                .unwrap_or_default();
            let message = format!(
                "pr_complete: pull request #{pr_number} cannot be merged ({reason}); \
                     branch protection or required reviews are unsatisfied and this run does \
                     not bypass them — the task stays in review.{diagnosis}"
            );
            return Err(reason.into_error(message));
        };
        match blocked_verdict(&status, self.unsettled_polls, pending_required) {
            BlockedVerdict::Wait => self.unsettled_polls += 1,
            BlockedVerdict::Stop(reason) => {
                let error = stopped(pr_number, &reason.to_string());
                return Err(match reason {
                    BlockedStopReason::CheckRefusal(refusal) => match refusal.delivery_refusal() {
                        Some(code) => hold_auto_fix_stop(error, code),
                        None => error,
                    },
                    BlockedStopReason::UnsatisfiedChecks => {
                        hold_auto_fix_stop(error, DeliveryRefusal::RequirementsUnsatisfied)
                    }
                });
            }
            // A repair commit on a reviewed head is reviewed again
            // before it is published; the last completion round has
            // no re-review left to give it.
            BlockedVerdict::RepairChecks(_)
                if self.reviewed_head_sha.is_some() && !re_review_on_conflict(input) =>
            {
                return Err(stopped(
                    pr_number,
                    "its head is bound to a review and this completion round has no \
                         re-review left for a repair",
                ));
            }
            BlockedVerdict::RepairChecks(failed) => {
                let base_evidence = match base_tip_checks(
                    host,
                    input,
                    workspace_path,
                    pr_number,
                    &status,
                    &failed,
                    observation_budget,
                ) {
                    Ok(evidence) => evidence,
                    Err(CheckReadGap::Deferred(deferred)) => {
                        let wait_ms = defer_throttled_read(
                            host,
                            auto_fix,
                            pr_number,
                            &deferred,
                            observation_budget,
                            mutated,
                        )?;
                        sleep(Duration::from_millis(wait_ms));
                        return Ok(SubFlow::ContinuePoll);
                    }
                    Err(CheckReadGap::Unavailable(reason)) => {
                        return Err(hold_auto_fix_stop(
                            stopped(pr_number, &reason),
                            DeliveryRefusal::CheckEvidenceInconclusive,
                        ));
                    }
                };
                if base_evidence
                    .as_ref()
                    .is_some_and(|evidence| evidence["path"] == "known_red_base")
                {
                    let evidence = base_evidence
                        .as_ref()
                        .ok_or_else(|| stopped(pr_number, "missing base evidence"))?;
                    record_base_path(host, auto_fix, "pr_auto_fix_base_red", evidence)?;
                    *base_red_reason = Some(format!(
                        "base_red: required failures also fail on current base {}, so no repair agent runs",
                        evidence["landed_base"]
                    ));
                } else if !self.landed_refresh_attempted
                    && let Some(evidence) = base_evidence
                {
                    let from = status
                        .head_sha
                        .as_deref()
                        .ok_or_else(|| stopped(pr_number, "missing failed head"))?;
                    let candidate = self.base_candidate.as_ref().ok_or_else(|| {
                        stopped(
                            pr_number,
                            "the base refresh lacks an owned candidate identity",
                        )
                    })?;
                    reserve_base_refresh(
                        host,
                        auto_fix,
                        candidate,
                        completion_deadline_ms,
                        &evidence,
                    )?;
                    record_landed_repair(host, auto_fix, &evidence)?;
                    let mut refresh_input = input.clone();
                    refresh_input["published_head_sha"] = json!(from);
                    // Pin the diagnosed landed base; a second base advance
                    // must be diagnosed on a later bounded attempt.
                    refresh_input["landed_repair_base_sha"] = evidence["landed_base"].clone();
                    // Keep PR identity checking against the named base.
                    let rebased = rebase_conflicting_pr_branch(
                        host,
                        &refresh_input,
                        workspace_path,
                        pr_number,
                        &status,
                    )?;
                    if self.reviewed_head_sha.is_some() {
                        return Ok(SubFlow::Deliver(Box::new(Delivery::RebasedForReview {
                            rebased,
                            reason: "auto_fix_landed_check_repair",
                        })));
                    }
                    push_batch_changes(
                        host,
                        &json!({
                            "job_run_id":input["job_run_id"], "completed_task_ids":input["completed_task_ids"],
                            "workspace_path":workspace_path, "branch":rebased["head"],
                            "rewrite_performed":rebased["rewritten"], "rewrite_head_before":rebased["head_sha_before"],
                            "expected_remote_sha":rebased["remote_sha_before"],
                            "lease_remote_sha":rebased["remote_sha_before"],
                        }),
                    )?;
                    let to = rebased["head_sha"]
                        .as_str()
                        .ok_or_else(|| stopped(pr_number, "missing refreshed head"))?;
                    record_published_repair(
                        host,
                        auto_fix,
                        pr_number,
                        from,
                        to,
                        PrAutoFixBlocker::FailedChecks,
                    )?;
                    self.pin.adopt_refreshed_candidate(Some(to));
                    self.landed_refresh_attempted = true;
                    return Ok(SubFlow::ContinuePoll);
                } else if let Some(reason) = {
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
                            return Ok(SubFlow::ContinuePoll);
                        }
                    }
                } {
                    record_base_path(
                        host,
                        auto_fix,
                        "pr_auto_fix_base_red",
                        &json!({"failed_head":status.head_sha,"reason":reason}),
                    )?;
                    *base_red_reason = Some(reason);
                } else {
                    match repair_failed_checks(
                        host,
                        auto_fix,
                        workspace_path,
                        pr_number,
                        &status,
                        &failed,
                        observation_budget,
                    ) {
                        CheckRepair::Ended(error) => return Err(error),
                        // Wait out the provider's throttle within the
                        // original deadline, then observe the pull
                        // request again before reading any evidence.
                        CheckRepair::ReadDeferred(deferred) => {
                            let wait_ms = defer_throttled_read(
                                host,
                                auto_fix,
                                pr_number,
                                &deferred,
                                observation_budget,
                                mutated,
                            )?;
                            sleep(Duration::from_millis(wait_ms));
                            return Ok(SubFlow::ContinuePoll);
                        }
                    }
                }
            }
        }
        Ok(SubFlow::WaitPoll)
    }

    fn dispatch_conflict(&mut self, status: PullRequestStatus) -> Result<SubFlow, OrbitError> {
        let host = self.host;
        let input = self.input;
        let workspace_path = self.workspace_path;
        let pr_number = self.pr_number;
        let auto_fix = self.auto_fix;
        // The self.pin may have adopted a contiguous publication from this
        // run or an authenticated retry ancestor. Lease that exact
        // candidate, retaining the original input as provenance. Neither
        // local HEAD nor a provider observation grants this authority.
        let mut refresh_input = input.clone();
        if let Some(candidate) = self.pin.candidate_sha() {
            refresh_input["published_head_sha"] = json!(candidate);
        }
        // [ORB-11333] Repairing a conflict rewrites the candidate; a
        // reviewed head cannot be merged as unreviewed content.
        // [ORB-13890] The pipeline may ask for the rebase so it can
        // review the new head before republishing it.
        if self.reviewed_head_sha.is_some() {
            if !re_review_on_conflict(input) || !has_completion_recovery_checkpoint(input) {
                return Err(OrbitError::Execution(format!(
                    "review_gate_stale: pull request #{pr_number} has merge conflicts and \
                         its head is bound to a before-PR review; a conflict repair needs a \
                         fresh review before managed completion, so the task stays in review"
                )));
            }
            if let Some(auto_fix) = auto_fix {
                admit_conflict_repair(host, auto_fix, pr_number, &status)?;
            }
            let rebased = rebase_conflicting_pr_branch(
                host,
                &refresh_input,
                workspace_path,
                pr_number,
                &status,
            )?;
            return Ok(SubFlow::Deliver(Box::new(Delivery::RebasedForReview {
                rebased,
                reason: "reviewed_head_conflicted",
            })));
        }
        if self.conflict_refresh_attempted || !has_completion_recovery_checkpoint(input) {
            return Err(completion_conflict_error(pr_number));
        }
        if let Some(auto_fix) = auto_fix {
            admit_conflict_repair(host, auto_fix, pr_number, &status)?;
        }
        let refreshed = refresh_conflicting_pr_branch(
            host,
            &refresh_input,
            workspace_path,
            pr_number,
            &status,
        )?;
        if let (Some(auto_fix), Some(from), Some(to)) =
            (auto_fix, self.pin.candidate_sha(), refreshed.as_deref())
        {
            record_published_repair(
                host,
                auto_fix,
                pr_number,
                from,
                to,
                PrAutoFixBlocker::Conflict,
            )?;
        }
        self.pin.adopt_refreshed_candidate(refreshed.as_deref());
        self.conflict_refresh_attempted = true;
        // The existing PR remains authoritative. Re-read its state
        // after the branch update rather than inferring mergeability
        // from a successful push.
        Ok(SubFlow::ContinuePoll)
    }

    fn dispatch_mergeable(&mut self, _status: PullRequestStatus) -> Result<SubFlow, OrbitError> {
        let host = self.host;
        let input = self.input;
        let workspace_path = self.workspace_path;
        let pr_number = self.pr_number;
        let observation_budget = self.observation_budget;
        let mutated = self.mutated;
        let observed = |error| observation_failure(host, input, mutated, observation_budget, error);
        if !self.merge_requested {
            let capabilities = resolved_capabilities(
                host,
                workspace_path,
                pr_number,
                &mut self.merge_capabilities,
            )
            .map_err(observed)?;
            let condition = merge_condition_sha(
                if self.base_adopted {
                    None
                } else {
                    self.reviewed_head_sha.as_deref()
                },
                &self.pin,
            );
            match request_merge(
                host,
                input,
                workspace_path,
                pr_number,
                capabilities.strategy,
                false,
                condition,
            )
            .map_err(|error| {
                OrbitError::Execution(format!(
                    "pr_complete: could not request {} merge on pull request \
                             #{pr_number}: {error}; the task stays in review",
                    capabilities.strategy.as_str()
                ))
            })? {
                ReviewedMergeOutcome::Accepted { landed_commit } => {
                    self.requested_landed_commit = landed_commit;
                    self.merge_requested = true;
                    // Re-read rather than assuming the request landed.
                    return Ok(SubFlow::ContinuePoll);
                }
                ReviewedMergeOutcome::BaseModified { diagnostic } => {
                    self.base_modified_refusals += 1;
                    let race = format!(
                        "synchronous merge attempt {} of \
                             {BASE_MODIFIED_MERGE_ATTEMPTS} conditioned on {} was refused \
                             because the base branch was modified ({diagnostic})",
                        self.base_modified_refusals,
                        condition.unwrap_or_default()
                    );
                    if self.base_modified_refusals >= BASE_MODIFIED_MERGE_ATTEMPTS {
                        return Err(OrbitError::Execution(format!(
                            "pr_complete: on pull request #{pr_number}, {race}; that is \
                                 the attempt ceiling, so the task stays in review"
                        )));
                    }
                    tracing::warn!(
                        pr_number,
                        attempt = self.base_modified_refusals,
                        "pr_complete: merge refused by a concurrent base change; \
                             re-reading the pull request before asking again"
                    );
                    self.base_race = Some(race);
                    // The base moved, so its merge policy may have too.
                    self.merge_capabilities = None;
                    // Wait one poll, then re-read: nothing here proves
                    // the next request is permitted.
                }
            }
        }
        Ok(SubFlow::WaitPoll)
    }

    fn dispatch_pending(&mut self, _status: PullRequestStatus) -> Result<SubFlow, OrbitError> {
        let host = self.host;
        let input = self.input;
        let workspace_path = self.workspace_path;
        let pr_number = self.pr_number;
        let observation_budget = self.observation_budget;
        let mutated = self.mutated;
        let observed = |error| observation_failure(host, input, mutated, observation_budget, error);
        // Auto-merge cannot retain a head condition. A reviewed head
        // or a published candidate waits for the synchronous mutation
        // once checks settle, including on queue-only branches where
        // that mutation refuses the unsupported merge. Only an
        // unpinned ungated run may hand the merge to GitHub.
        if !self.watch_only
            && self.reviewed_head_sha.is_none()
            && self.pin.candidate_sha().is_none()
            && !self.auto_merge_requested
        {
            // Required checks are still running. Hand the merge to
            // GitHub's auto-merge when this repository allows it, then
            // keep polling: enabling it is not success. Repositories
            // with auto-merge disabled instead wait for a normally
            // mergeable state, at which point the ordinary merge path
            // above makes the same permitted request.
            let capabilities = resolved_capabilities(
                host,
                workspace_path,
                pr_number,
                &mut self.merge_capabilities,
            )
            .map_err(observed)?;
            if capabilities.auto_merge_allowed {
                request_merge(
                    host,
                    input,
                    workspace_path,
                    pr_number,
                    capabilities.strategy,
                    true,
                    None,
                )
                .map_err(|error| {
                    OrbitError::Execution(format!(
                        "pr_complete: could not enable auto-merge using {} on pull request \
                                 #{pr_number}: {error}; the task stays in review",
                        capabilities.strategy.as_str()
                    ))
                })?;
                self.auto_merge_requested = true;
            }
        }
        Ok(SubFlow::WaitPoll)
    }
}
