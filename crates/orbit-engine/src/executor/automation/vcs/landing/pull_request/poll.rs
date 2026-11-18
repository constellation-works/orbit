//! The landing poll: the observation loop, its deadlines, and the status
//! and completion-hold reads that gate later phases.

use std::path::PathBuf;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::base::refresh::RefreshStep;
use super::super::super::checks::{CheckRecovery, failed_checks};
use super::super::super::completion_hold::pull_request_changed_paths;
use super::super::super::git::{fetch_remote_base, git_command_success};
use super::super::super::pr::diagnostics;
use super::super::super::pr::{
    DeliveryPin, OwedHostedChecks, PrMergeState, PullRequestStatus, UnreportedChecks,
    classify_pr_state, owed_hosted_checks, read_pr_status, read_pr_status_before,
};
use super::super::super::remote_probe::fixed_wait as sleep;
use super::super::intent::LandingView;
use super::super::observe::{adopt_base_update, pin_for};
use super::super::record::{completion_hold_stop, describe, hold_pr, stop_pr};
use super::super::run::{LandingBudget, LandingPass};

pub(in crate::executor::automation::vcs::landing) fn land_pull_request<H: RuntimeHost + ?Sized>(
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
pub(super) enum Step {
    /// Re-read the pull request from the top of the loop.
    Continue,
    /// Go on to the next phase of this observation.
    Proceed,
    /// The landing completed with this record.
    Done(Value),
}

/// One landing's poll: the state carried across observations of the pull
/// request, plus the borrowed pass it lands.
pub(super) struct LandingPoll<'a, H: RuntimeHost + ?Sized> {
    pub(super) host: &'a H,
    pub(super) context: &'a LandingPass,
    pub(super) view: &'a LandingView,
    run_id: &'a str,
    pub(super) number: u64,
    pub(super) pr_number: String,
    pub(super) workspace: PathBuf,
    pub(super) workspace_path: String,
    pub(super) max_wait_seconds: u64,
    poll_interval_seconds: u64,
    started_ms: u64,
    pub(super) deadline_ms: u64,
    pub(super) pin: DeliveryPin,
    pub(super) recovery: CheckRecovery,
    pub(super) unreported: UnreportedChecks,
    pub(super) waited_seconds: u64,
    pub(super) intent: Option<String>,
    pub(super) waiting_for_checks: bool,
    // [ORB-23365] The completion hold is decided once, on the first
    // observation that shows the pull request still unmerged.
    completion_hold_checked: bool,
    // Merge requests the provider refused because the base moved, against the
    // same ceiling pr_complete uses.
    pub(super) base_modified_refusals: u32,
    pub(super) diagnostic: Value,
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

    pub(super) fn stop(&self, repair: bool, reason: &str) -> Result<OrbitError, OrbitError> {
        stop_pr(self.host, self.context, &self.diagnostic, repair, reason)
    }

    pub(super) fn hold(
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
    pub(super) fn stop_on(&self, error: OrbitError) -> OrbitError {
        self.stop(false, &error.to_string()).unwrap_or(error)
    }

    pub(super) fn deadline_passed(&self) -> Result<bool, OrbitError> {
        Ok(self.max_wait_seconds > 0
            && super::super::super::completion_deadline::now_ms()? >= self.deadline_ms)
    }

    fn note_elapsed(&mut self) -> Result<(), OrbitError> {
        self.waited_seconds = self.waited_seconds.max(
            super::super::super::completion_deadline::now_ms()?.saturating_sub(self.started_ms)
                / 1000,
        );
        Ok(())
    }

    /// Sleeps one normal poll, bounded by what remains of the wait budget.
    pub(super) fn wait_one_poll(&mut self) {
        let delay = self
            .poll_interval_seconds
            .min(self.max_wait_seconds.saturating_sub(self.waited_seconds));
        sleep(Duration::from_secs(delay));
        self.waited_seconds = self.waited_seconds.saturating_add(delay);
    }

    fn ensure_within_deadline(&self) -> Result<(), OrbitError> {
        if self.max_wait_seconds > 0
            && (self.waited_seconds >= self.max_wait_seconds
                || super::super::super::completion_deadline::now_ms()? >= self.deadline_ms)
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
        super::super::super::checks::rerun_diagnostic::observe_ci_rerun_head(
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
    pub(super) fn timeout(&self, owed: &[String]) -> Result<OrbitError, OrbitError> {
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
            super::super::record::stop_waiting_checks(
                self.host,
                self.context,
                &self.diagnostic,
                &reason,
            )
        } else {
            self.stop(false, &reason)
        }
    }
}
