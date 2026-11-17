//! Landing a handoff through its pull request: check waits, recovery and the merge.

use std::path::PathBuf;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;
use serde_json::{Value, json};

use crate::context::{HandoffLandingStep, RuntimeHost};

use super::super::base::refresh::RefreshStep;
use super::super::checks::{
    CheckRecovery, CheckRecoveryStep, ReadWait, failed_checks, landing_log_read_wait,
    landing_read_wait, recover_failed_checks,
};
use super::super::commit::squash_merge_fields;
use super::super::completion_hold::pull_request_changed_paths;
use super::super::git::{fetch_remote_base, git_command_success};
use super::super::operations;
use super::super::pr::{
    BlockedCheckWait, DeliveryPin, OwedHostedChecks, PrMergeState, PullRequestStatus,
    UnreportedChecks, checked_pr_state, classify_pr_state, owed_hosted_checks, read_pr_status,
    read_pr_status_before, resolve_permitted_merge,
};
use super::super::pr::{ReviewedMergeOutcome, diagnostics, draft_recovery};
use super::super::remote_probe::fixed_wait as sleep;
use super::intent::{LandingView, reconcile_intent};
use super::observe::{adopt_base_update, landing_tip, observe_candidate, pin_for};
use super::record::{
    behind_base, complete, completion_hold_stop, describe, hold_pr, record, refuse_pr, stop_pr,
};
use super::run::{LandingBudget, LandingPass, transport_failure};

pub(super) fn land_pull_request<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    number: u64,
    budget: &LandingBudget,
    view: &LandingView,
    run_id: &str,
) -> Result<Value, OrbitError> {
    let mut poll = LandingPoll::new(host, context, number, budget, view, run_id);
    loop {
        poll.ensure_within_deadline()?;
        let status = poll.read_status()?;
        let state = poll.observe_status(&status)?;
        poll.decide_completion_hold(&state)?;
        // [ORB-15956] A branch update this landing (or an earlier attempt)
        // requested moved the head onto a verified merge with the base.
        let updated_head = adopt_base_update(&mut poll.pin, context, &status);
        let refresh = poll.refresh_base(&status, false)?;
        if matches!(refresh, RefreshStep::Changed) {
            continue;
        }
        let mut refresh_wait = matches!(
            refresh,
            RefreshStep::Wait | RefreshStep::Adopted { waiting: true }
        );
        if let PrMergeState::Merged = state {
            return poll.settle_merged(&status, updated_head);
        }
        poll.ensure_pinned(&status)?;
        poll.refuse_draft(&status)?;
        let state = poll.read_required_checks(&status)?;
        if let Step::Continue = poll.diagnose_blocked_base(&status, &state, &mut refresh_wait)? {
            continue;
        }
        poll.note_elapsed()?;
        // [ORB-15677] [ORB-15812] The review owed these checks to the pull
        // request's hosted CI instead of running them: nothing merges until
        // each reports an explicit success on the pinned head. A skipped or
        // neutral result keeps waiting. Branch protection is separate.
        let owed = owed_hosted_checks(&status, &context.owed_hosted_checks);
        let mut waiting_on_recovery = refresh_wait;
        if let Step::Continue =
            poll.recover_failed_checks(&status, &state, &owed, &mut waiting_on_recovery)?
        {
            continue;
        }
        if !waiting_on_recovery {
            let mergeable = matches!(state, PrMergeState::Mergeable);
            poll.stop_on_terminal_state(&status, state)?;
            if mergeable && poll.intent.is_none() && owed == OwedHostedChecks::Passed {
                match poll.request_merge(&status, updated_head.is_some())? {
                    Step::Continue => continue,
                    Step::Done(completed) => return Ok(completed),
                    Step::Proceed => {}
                }
            }
        }
        // Requested already, or still waiting for required or owed checks.
        poll.wait_or_time_out(&owed)?;
    }
}

/// What a landing phase tells the poll loop to do next. Errors stay `Err`.
enum Step {
    /// Re-read the pull request from the top of the loop.
    Continue,
    /// Go on to the next phase of this observation.
    Proceed,
    /// The landing completed with this record.
    Done(Value),
}

/// One landing's poll: the state carried across observations of the pull
/// request, plus the borrowed pass it lands.
struct LandingPoll<'a, H: RuntimeHost + ?Sized> {
    host: &'a H,
    context: &'a LandingPass,
    view: &'a LandingView,
    run_id: &'a str,
    number: u64,
    pr_number: String,
    workspace: PathBuf,
    workspace_path: String,
    max_wait_seconds: u64,
    poll_interval_seconds: u64,
    started_ms: u64,
    deadline_ms: u64,
    pin: DeliveryPin,
    recovery: CheckRecovery,
    unreported: UnreportedChecks,
    waited_seconds: u64,
    intent: Option<String>,
    waiting_for_checks: bool,
    // [ORB-23365] The completion hold is decided once, on the first
    // observation that shows the pull request still unmerged.
    completion_hold_checked: bool,
    // Merge requests the provider refused because the base moved, against the
    // same ceiling pr_complete uses.
    base_modified_refusals: u32,
    diagnostic: Value,
}

impl<'a, H: RuntimeHost + ?Sized> LandingPoll<'a, H> {
    fn new(
        host: &'a H,
        context: &'a LandingPass,
        number: u64,
        budget: &LandingBudget,
        view: &'a LandingView,
        run_id: &'a str,
    ) -> Self {
        let LandingBudget {
            max_wait_seconds,
            max_base_ci_cycles,
            poll_interval_seconds,
            started_ms,
            deadline_ms,
        } = *budget;
        let workspace = context.workspace_path.clone();
        let workspace_path = workspace.to_string_lossy().into_owned();
        Self {
            host,
            context,
            view,
            run_id,
            number,
            pr_number: number.to_string(),
            workspace,
            workspace_path,
            max_wait_seconds,
            poll_interval_seconds,
            started_ms,
            deadline_ms,
            pin: pin_for(&context.candidate),
            recovery: CheckRecovery::before(deadline_ms, max_base_ci_cycles),
            unreported: UnreportedChecks::new(),
            waited_seconds: 0,
            intent: None,
            waiting_for_checks: false,
            completion_hold_checked: false,
            base_modified_refusals: 0,
            diagnostic: json!({"pr":{"state":"not_collected"},
                "required_checks":{"state":"not_collected"}, "status_observations":0,
                "classification":{"state":"not_collected"}}),
        }
    }

    fn stop(&self, repair: bool, reason: &str) -> Result<OrbitError, OrbitError> {
        stop_pr(self.host, self.context, &self.diagnostic, repair, reason)
    }

    fn hold(
        &self,
        repair: bool,
        refusal: DeliveryRefusal,
        reason: &str,
    ) -> Result<OrbitError, OrbitError> {
        hold_pr(
            self.host,
            self.context,
            &self.diagnostic,
            repair,
            refusal,
            reason,
        )
    }

    /// Stops on `error`, or returns the stop's own failure in its place.
    fn stop_on(&self, error: OrbitError) -> OrbitError {
        self.stop(false, &error.to_string()).unwrap_or(error)
    }

    fn deadline_passed(&self) -> Result<bool, OrbitError> {
        Ok(self.max_wait_seconds > 0
            && super::super::completion_deadline::now_ms()? >= self.deadline_ms)
    }

    fn note_elapsed(&mut self) -> Result<(), OrbitError> {
        self.waited_seconds = self.waited_seconds.max(
            super::super::completion_deadline::now_ms()?.saturating_sub(self.started_ms) / 1000,
        );
        Ok(())
    }

    /// Sleeps one normal poll, bounded by what remains of the wait budget.
    fn wait_one_poll(&mut self) {
        let delay = self
            .poll_interval_seconds
            .min(self.max_wait_seconds.saturating_sub(self.waited_seconds));
        sleep(Duration::from_secs(delay));
        self.waited_seconds = self.waited_seconds.saturating_add(delay);
    }

    fn ensure_within_deadline(&self) -> Result<(), OrbitError> {
        if self.max_wait_seconds > 0
            && (self.waited_seconds >= self.max_wait_seconds
                || super::super::completion_deadline::now_ms()? >= self.deadline_ms)
        {
            return Err(self.timeout(&self.context.owed_hosted_checks)?);
        }
        Ok(())
    }

    fn read_status(&self) -> Result<PullRequestStatus, OrbitError> {
        if self.max_wait_seconds == 0 {
            read_pr_status(self.host, &self.workspace, &self.pr_number)
        } else {
            read_pr_status_before(
                self.host,
                &self.workspace,
                &self.pr_number,
                self.deadline_ms,
            )
        }
    }

    /// Records this observation in the diagnostic and classifies it.
    fn observe_status(&mut self, status: &PullRequestStatus) -> Result<PrMergeState, OrbitError> {
        let (host, context) = (self.host, self.context);
        let has_failures = !failed_checks(status).is_empty();
        // [model ranking §5.1] A check recorded failing with no known cause
        // that now passes on the same head was flaky.
        self.recovery.failures.observe_passes(host, context, status);
        super::super::checks::rerun_diagnostic::observe_ci_rerun_head(
            host,
            std::slice::from_ref(&context.task_id),
            &context.handoff_id,
            status.head_sha.as_deref(),
            has_failures,
        );
        if self
            .recovery
            .rerun_diagnostic
            .as_ref()
            .is_some_and(|previous| previous.head_sha != status.head_sha || !has_failures)
        {
            self.recovery.rerun_diagnostic = None;
            if let Some(diagnostic) = self.diagnostic.as_object_mut() {
                diagnostic.remove("ci_rerun_diagnostic");
            }
        }
        let diagnostic = &mut self.diagnostic;
        diagnostic["pr"] = diagnostics::status(status);
        diagnostic["base_suspicion"] = Value::Null;
        diagnostic["required_checks"] = json!({"state":"not_collected"});
        diagnostic["unreported_wait"] = json!({"state":"not_collected"});
        diagnostic["status_observations"] = json!(
            diagnostic["status_observations"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(1)
        );
        diagnostic["status_history_truncated"] =
            json!(diagnostic["status_observations"].as_u64().unwrap_or(0) > 1);
        let state = classify_pr_state(status);
        diagnostic["classification"] = diagnostics::text(&describe(&state));
        self.note_elapsed()?;
        Ok(state)
    }

    /// [ORB-23365] Decide a completion hold before any recovery write or
    /// merge request. An already-merged pull request is settled as the fact
    /// it is; it cannot be held after the fact. The candidate's objects reach
    /// this checkout by fetching its source branch, best effort: a diff that
    /// still cannot be read holds under a `paths` rule rather than landing
    /// unchecked, as in `pr_complete`.
    fn decide_completion_hold(&mut self, state: &PrMergeState) -> Result<(), OrbitError> {
        if self.completion_hold_checked || matches!(state, PrMergeState::Merged) {
            return Ok(());
        }
        self.completion_hold_checked = true;
        let context = self.context;
        if !context.owner_distributed_completion_done
            || self.host.completion_hold_rules().is_empty()
        {
            return Ok(());
        }
        let candidate = &context.candidate;
        if git_command_success(&self.workspace, &["remote", "get-url", "origin"]).unwrap_or(false) {
            let _ = fetch_remote_base(&self.workspace, &candidate.source_branch);
        }
        let changed = pull_request_changed_paths(
            &self.workspace,
            Some(&candidate.base_branch),
            false,
            Some(&candidate.candidate.commit),
        );
        match completion_hold_stop(self.host, context, &changed, self.run_id)? {
            Some(held) => Err(held),
            None => Ok(()),
        }
    }

    fn refresh_base(
        &self,
        status: &PullRequestStatus,
        diagnose: bool,
    ) -> Result<RefreshStep, OrbitError> {
        let context = self.context;
        super::super::base::refresh::recover(
            self.host,
            &self.workspace,
            std::slice::from_ref(&context.task_id),
            &context.handoff_id,
            &context.candidate,
            status,
            self.deadline_ms,
            diagnose,
            Some(&context.handoff_id),
        )
        .map_err(|error| self.stop_on(error))
    }

    /// Settles a merged pull request as this handoff's delivery.
    fn settle_merged(
        &mut self,
        status: &PullRequestStatus,
        updated_head: Option<String>,
    ) -> Result<Value, OrbitError> {
        let (host, context) = (self.host, self.context);
        let delivered = match self.pin.ensure_delivered(status, &self.pr_number) {
            Ok(delivered) => delivered,
            // A merged pull request whose identity is not ours is not this
            // handoff's delivery, whatever it merged.
            Err(error) => return Err(self.stop(false, &error.to_string())?),
        };
        let mut evidence = json!({
            "delivery": delivered.as_json(),
            "pull_request": self.number,
            "candidate": context.candidate.candidate.commit,
        });
        if let Some(head) = &updated_head {
            evidence["updated_head"] = json!(head);
        }
        if let Some(actions) = self.recovery.evidence() {
            evidence["check_recovery"] = actions;
        }
        let observed =
            observe_candidate(context, Some(status), self.view).map_err(|e| self.stop_on(e))?;
        // A merge with no intent of ours is the operator's; the completion
        // record carries that in its history note.
        evidence["external_merge"] = json!(self.intent.is_none());
        if let Some(intent_id) = self.intent.take() {
            record(
                host,
                context,
                HandoffLandingStep::ResolveIntent {
                    intent_id,
                    merged: true,
                },
                Some(observed.clone()),
                &evidence,
            )?;
        }
        complete(host, context, observed, &evidence)
    }

    /// Every non-merged state is read against the pinned identity first: a
    /// repointed branch or base must never receive a merge request.
    fn ensure_pinned(&self, status: &PullRequestStatus) -> Result<(), OrbitError> {
        match self.pin.ensure_pinned_candidate(status, &self.pr_number) {
            Ok(_) => Ok(()),
            Err(error) => Err(self.stop(false, &error.to_string())?),
        }
    }

    /// A follower can hand off a PR drafted for missing evidence. Send that
    /// candidate through the existing repair flow, whose pr_open refreshes
    /// the body and marks it ready only once every gap is filled. Configured
    /// and unrelated drafts still require the operator.
    fn refuse_draft(&self, status: &PullRequestStatus) -> Result<(), OrbitError> {
        if !(status.is_draft || status.merge_state_status.as_deref() == Some("DRAFT")) {
            return Ok(());
        }
        let context = self.context;
        let pr_number = &self.pr_number;
        let configured = self.host.pr_config().conventions.draft;
        // Only an explicit draft carries a body to repair. A DRAFT merge
        // state without isDraft is an older answer without body evidence,
        // so refuse without an extra view.
        if !configured && status.is_draft {
            let missing = draft_recovery::handed_off_section_gaps(
                self.host,
                &self.workspace,
                &context.task_id,
                pr_number,
            )?;
            if !missing.is_empty() {
                let action = if context.automatic_repair_spent {
                    "return the preserved candidate to the backlog for a repair round"
                } else {
                    "return the claim for one automatic repair round"
                };
                let reason = format!(
                    "pull request #{pr_number} is still a section-gap draft; missing required \
                     sections: {}; next action: {action}. Fill the named sections or attach \
                     their required evidence, then resume pr_open to refresh the body and \
                     mark the PR ready before a new validated handoff",
                    missing.join(", ")
                );
                return Err(if context.automatic_repair_spent {
                    refuse_pr(
                        self.host,
                        context,
                        &self.diagnostic,
                        HandoffLandingStep::StopForRequeue,
                        &reason,
                    )?
                } else {
                    self.stop(true, &reason)?
                });
            }
        }
        let reason = if configured {
            "the delivery conventions open every pull request as a draft; next action: \
             explicitly mark the PR ready on the forge, then retry landing"
        } else {
            "no section-gap recovery evidence; next action: mark the PR ready on the \
             forge, then retry landing"
        };
        Err(self.hold(
            false,
            DeliveryRefusal::PullRequestDraft,
            &format!(
                "pull request #{pr_number} cannot be merged (the pull request is still a draft); {reason}"
            ),
        )?)
    }

    /// Refines the state with the required checks and records whether the
    /// landing is only waiting for them.
    fn read_required_checks(
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
                || status
                    .status_checks
                    .iter()
                    .flatten()
                    .any(|check| check.state == Some(super::super::pr::CheckState::Pending)))
            && matches!(
                status.review_decision,
                super::super::pr::ReviewDecision::Approved
                    | super::super::pr::ReviewDecision::NotApplicable
            );
        Ok(state)
    }

    /// A required context the head has not reported may be one only the
    /// advanced base emits, so absent-context recovery still diagnoses it.
    fn diagnose_blocked_base(
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
    fn recover_failed_checks(
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
                let now = super::super::completion_deadline::now_ms()?;
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
                let now = super::super::completion_deadline::now_ms()?;
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

    /// Stops on a state the landing cannot merge from; mergeable and pending
    /// states proceed.
    fn stop_on_terminal_state(
        &self,
        status: &PullRequestStatus,
        state: PrMergeState,
    ) -> Result<(), OrbitError> {
        let pr_number = &self.pr_number;
        match state {
            PrMergeState::Closed => Err(self.hold(
                false,
                DeliveryRefusal::PullRequestClosed,
                &format!(
                    "pull request #{pr_number} was closed without merging; the candidate \
                     stays in review"
                ),
            )?),
            PrMergeState::Contradictory(reason) => Err(self.stop(
                false,
                &format!(
                    "pull request #{pr_number} reports a contradictory merge state \
                     ({reason}); landing needs an unambiguous merged state"
                ),
            )?),
            PrMergeState::Blocked(reason) => {
                let refusal = reason.refusal;
                let reason = format!(
                    "pull request #{pr_number} cannot be merged ({reason}); branch \
                     protection and required checks are not bypassed"
                );
                // A branch that is only behind its base is stale, not refused:
                // the repair brings it up to date and revalidates it.
                Err(if behind_base(status) {
                    self.stop(true, &reason)?
                } else if let Some(refusal) = refusal {
                    self.hold(false, refusal, &reason)?
                } else {
                    self.stop(false, &reason)?
                })
            }
            PrMergeState::Conflict => Err(self.hold(
                true,
                DeliveryRefusal::LandingConflict,
                &format!(
                    "pull request #{pr_number} conflicts with its base; the owner does not \
                     rebase unvalidated code, so a repair needs fresh validation and a new \
                     handoff"
                ),
            )?),
            // A merged pull request is settled before the pinned-identity
            // read, so it never reaches this phase.
            PrMergeState::Merged | PrMergeState::Mergeable | PrMergeState::Pending => Ok(()),
        }
    }

    /// After a branch update, a newly required context that has not
    /// succeeded is still a gate, including on a provider-reported CLEAN.
    fn wait_for_current_gates(&mut self, status: &PullRequestStatus) -> Result<Step, OrbitError> {
        let current = self
            .host
            .run_private_vcs_operation(
                operations::CI_CURRENT_GATES,
                json!({"workspace_path":self.workspace,"pr":self.pr_number,
                    "head_sha":status.head_sha,"head_branch":status.head_branch,
                    "base_branch":status.base_branch,"deadline_ms":self.deadline_ms}),
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

    /// Requests the merge under a durable intent, and re-reads after a
    /// refusal caused by a concurrent base change.
    fn request_merge(
        &mut self,
        status: &PullRequestStatus,
        updated_head: bool,
    ) -> Result<Step, OrbitError> {
        let (host, context) = (self.host, self.context);
        let candidate = &context.candidate;
        let number = self.number;
        let pr_number = self.pr_number.clone();
        if self.deadline_passed()? {
            return Err(self.timeout(&context.owed_hosted_checks)?);
        }
        if updated_head && let Step::Continue = self.wait_for_current_gates(status)? {
            return Ok(Step::Continue);
        }
        let capabilities = resolve_permitted_merge(host, &self.workspace_path, &pr_number)
            .map_err(|error| {
                operations::carry_class(
                    &error,
                    OrbitError::Execution(format!(
                        "handoff_land: no permitted merge method for pull request \
                         #{pr_number}: {error}"
                    )),
                )
            })?;
        if self.deadline_passed()? {
            return Err(self.timeout(&context.owed_hosted_checks)?);
        }
        let observed =
            observe_candidate(context, Some(status), self.view).map_err(|e| self.stop_on(e))?;
        if observed.repository != capabilities.repository {
            return Err(self.stop(
                false,
                &format!(
                    "pull request #{pr_number} belongs to repository \
                     '{}', not the handoff's '{}'",
                    capabilities.repository, candidate.repository
                ),
            )?);
        }
        // A refused request never reached the merge, so each request after
        // the first carries its own intent id: the store replays a mutation
        // it has already recorded under the same id.
        let intent_id = if self.base_modified_refusals == 0 {
            format!("{}:{pr_number}", context.handoff_id)
        } else {
            format!(
                "{}:{pr_number}:{}",
                context.handoff_id,
                self.base_modified_refusals + 1
            )
        };
        // Durable before the external mutation: a lost reply after this
        // point is uncertainty the next attempt reconciles.
        let head = self
            .pin
            .candidate_sha()
            .unwrap_or(&candidate.candidate.commit)
            .to_string();
        record(
            host,
            context,
            HandoffLandingStep::PublishIntent {
                intent_id: intent_id.clone(),
            },
            Some(observed),
            &json!({
                "sending": "pull_request_merge",
                "pull_request": number,
                "strategy": capabilities.strategy.as_str(),
                "candidate": candidate.candidate.commit,
                "head": head,
            }),
        )?;
        self.intent = Some(intent_id.clone());
        // The synchronous mutation carries the pinned head, so the
        // provider itself refuses a candidate that moved. A base-modified
        // refusal comes back as a value, so the intent is resolved below.
        let mut request = json!({
            "pr": pr_number,
            "strategy": capabilities.strategy.as_str(),
            "auto": false,
            "reviewed_head_sha": head,
            "report_base_modified": true,
            "workspace_path": self.workspace_path,
        });
        // An unreadable task still lands: the squash takes its subject from
        // the candidate's own commits, which were composed from the task.
        let tasks = match host.get_task(&context.task_id) {
            Ok(task) => vec![task],
            Err(error) => {
                tracing::warn!(
                    task_id = %context.task_id,
                    %error,
                    "handoff_land: squash message composed from the candidate's commits"
                );
                Vec::new()
            }
        };
        if let Value::Object(request) = &mut request {
            request.extend(squash_merge_fields(
                host,
                &self.workspace,
                &tasks,
                Some(&candidate.base_branch),
                &head,
                &pr_number,
            )?);
        }
        let response = host.run_private_vcs_operation(operations::PR_MERGE, request)?;
        let refusal = match ReviewedMergeOutcome::from_response(&response) {
            // Merged is what the provider reports, not what we asked for.
            ReviewedMergeOutcome::Accepted { .. } => return Ok(Step::Continue),
            ReviewedMergeOutcome::BaseModified { diagnostic: text } => text,
            ReviewedMergeOutcome::InProgress { diagnostic } => {
                if let Some(completed) = reconcile_intent(host, context, &intent_id, self.view)? {
                    return Ok(Step::Done(completed));
                }
                self.intent = None;
                return Err(self.stop(
                    false,
                    &format!(
                        "pull request #{pr_number} merge is already in progress ({diagnostic}); \
                         one re-read did not confirm it merged; retry landing after checking its status"
                    ),
                )?);
            }
            // This caller does not opt into completion's reconciliation.
            ReviewedMergeOutcome::Ambiguous { diagnostic } => {
                return Err(OrbitError::Execution(diagnostic));
            }
        };
        self.base_modified_refusals += 1;
        let refusals = self.base_modified_refusals;
        tracing::warn!(
            pull_request = number,
            attempt = refusals,
            "handoff_land: merge refused by a concurrent base change; re-reading the \
             pull request before asking again"
        );
        // The provider refused, so nothing merged: resolve the intent as
        // unmerged by reading the pull request, as reconciliation does. A
        // pull request that did merge still completes here.
        if let Some(completed) = reconcile_intent(host, context, &intent_id, self.view)? {
            return Ok(Step::Done(completed));
        }
        self.intent = None;
        if refusals >= operations::BASE_MODIFIED_MERGE_ATTEMPTS {
            return Err(self.stop(
                false,
                &format!(
                    "pull request #{pr_number} was refused {refusals} \
                     merge requests because the base branch was modified ({refusal}); \
                     the attempt ceiling is reached"
                ),
            )?);
        }
        // Wait one normal poll, then re-read before asking again.
        self.wait_one_poll();
        Ok(Step::Continue)
    }

    fn wait_or_time_out(&mut self, owed: &OwedHostedChecks) -> Result<(), OrbitError> {
        if self.waited_seconds >= self.max_wait_seconds {
            let waiting = match owed {
                OwedHostedChecks::Waiting(names) if self.intent.is_none() => names.as_slice(),
                _ => &[],
            };
            return Err(self.timeout(waiting)?);
        }
        self.wait_one_poll();
        Ok(())
    }

    /// The stop for a landing wait whose deadline passed, naming the owed
    /// hosted checks still waited on.
    fn timeout(&self, owed: &[String]) -> Result<OrbitError, OrbitError> {
        let (number, max_wait_seconds) = (self.number, self.max_wait_seconds);
        let mut waiting = if owed.is_empty() {
            String::new()
        } else {
            format!(" (owed hosted checks not yet passed: {})", owed.join(", "))
        };
        waiting.push_str(&self.unreported.timeout_note());
        if let Some(gap) = self.diagnostic["log_read_gap"]["reason"].as_str() {
            return self.hold(
                false,
                DeliveryRefusal::ForgeObservationUnavailable,
                &format!(
                    "pull request #{number} is waiting for failed-job logs after \
                     {max_wait_seconds}s: {gap}; the owner will re-read the logs and \
                     resume when evidence is available; the candidate stays in review"
                ),
            );
        }
        if let Some(suspicion) = self.diagnostic["base_suspicion"].as_str() {
            return self.hold(
                false,
                DeliveryRefusal::BaseChecksPending,
                &format!(
                    "pull request #{number} is waiting for the base's own check result after \
                     {max_wait_seconds}s: the base never recovered before the landing deadline; {suspicion}; the candidate stays in review"
                ),
            );
        }
        let reason = format!(
            "pull request #{number} did not reach a merged state within {max_wait_seconds}s{waiting}; the candidate stays in review"
        );
        if self.waiting_for_checks {
            super::record::stop_waiting_checks(self.host, self.context, &self.diagnostic, &reason)
        } else {
            self.stop(false, &reason)
        }
    }
}
