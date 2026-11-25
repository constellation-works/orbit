//! [ORB-26860] The completion's base-inherited refresh: after the no-edit
//! repair's one rerun failed again, refresh the pull request to its base tip
//! once when the failure is base-inherited, and on every poll adopt that
//! refreshed head as the candidate only after verifying it carries nothing
//! but base commits.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{PR_BASE_INHERITED_REFRESH_CAUSE, pr_base_update_reservations};

use crate::context::RuntimeHost;

use super::super::super::checks::verify_refreshed_head_from_base;
use super::super::super::git::git_output;
use super::super::answers::PullRequestStatus;
use super::super::auto_fix::{AutoFix, InheritedRefresh, refresh_inherited_failure};
use super::drive_phases::PollDriver;

/// What the completion does after the no-edit rerun failed again.
pub(super) enum RerunFailureStep {
    /// The head was refreshed, or a throttled read deferred; observe again.
    Repoll,
    /// Hold as `RepairDispatched` with this reason.
    Hold(String),
}

impl<H: RuntimeHost + ?Sized> PollDriver<'_, H> {
    /// Accept an out-of-run refresh only when the exact first-parent path
    /// matches successful Orbit refreshes of this task and PR.
    pub(super) fn adopt_audited_refresh(&mut self, status: &PullRequestStatus) {
        let (Some(head), Some(candidate), Some(head_branch), Some(base_branch), Ok(number)) = (
            status.head_sha.as_deref(),
            self.pin.candidate_sha(),
            self.pin.head(),
            self.pin.base(),
            self.pr_number.parse::<u64>(),
        ) else {
            return;
        };
        if self.base_adopted
            || head == candidate
            || status.number != Some(number)
            || status.head_branch.as_deref() != Some(head_branch)
            || status.base_branch.as_deref() != Some(base_branch)
            || self
                .reviewed_head_sha
                .as_deref()
                .is_some_and(|reviewed| reviewed != candidate)
        {
            return;
        }
        for task_id in self.task_ids {
            let audited = match self.host.audited_delivery_refreshes(
                task_id,
                self.pr_number,
                head_branch,
                base_branch,
                candidate,
                head,
            ) {
                Ok(audited) if audited.is_empty() => continue,
                Ok(audited) => audited,
                Err(error) => {
                    tracing::warn!(pr_number = self.pr_number, %error, "could not read PR refresh audit");
                    continue;
                }
            };
            let workspace = Path::new(self.workspace_path);
            if let Err(why) = verify_refreshed_head_from_base(
                &self.host.process_env(),
                workspace,
                candidate,
                head,
                base_branch,
                Some(head_branch),
                Some(number),
            ) {
                tracing::warn!(pr_number = self.pr_number, %why, "PR head is not a clean base refresh");
                return;
            }
            // The graph proof alone allows a chain of clean merges. Each
            // commit in that chain must also have its own successful audit.
            let range = format!("{candidate}..{head}");
            match git_output(
                &self.host.process_env(),
                workspace,
                &["rev-list", "--first-parent", &range],
            ) {
                Ok(history) if history.lines().eq(audited.iter().map(String::as_str)) => {
                    self.pin.adopt_refreshed_candidate(Some(head));
                    self.base_adopted = true;
                    self.audited_refresh_adopted = true;
                    self.base_candidate = None;
                    return;
                }
                Ok(_) => tracing::warn!(
                    pr_number = self.pr_number,
                    "PR head does not match its audited clean base refreshes"
                ),
                Err(error) => {
                    tracing::warn!(pr_number = self.pr_number, %error, "could not read PR refresh history");
                    return;
                }
            }
        }
    }

    /// Refresh the head once when the failure `rerun_failure` names is
    /// base-inherited, or hold saying why not.
    pub(super) fn refresh_after_failed_rerun(
        &mut self,
        auto_fix: &AutoFix,
        status: &PullRequestStatus,
        rerun_failure: &str,
    ) -> Result<RerunFailureStep, OrbitError> {
        match refresh_inherited_failure(
            self.host,
            auto_fix,
            self.input,
            self.workspace_path,
            self.pr_number,
            status,
            rerun_failure,
            self.completion_deadline_ms,
        )? {
            InheritedRefresh::Refreshed { from, to } => {
                tracing::info!(
                    pr_number = self.pr_number,
                    from = %from,
                    to = %to,
                    "pr_complete: refreshed a base-inherited failure's head to its base tip"
                );
                self.inherited_refresh_attempted = true;
                self.ci_rerun_hold.clear();
                Ok(RerunFailureStep::Repoll)
            }
            InheritedRefresh::Deferred(deferred) => {
                self.repoll_after_throttled_read(auto_fix, &deferred)?;
                Ok(RerunFailureStep::Repoll)
            }
            InheritedRefresh::Hold(reason) => Ok(RerunFailureStep::Hold(reason)),
        }
    }

    /// Pin a moved head as the candidate when it is this pull request's
    /// reserved base-inherited refresh of the pinned candidate and
    /// [`verify_refreshed_head_from_base`] accepts it. Any other moved head
    /// is left to the delivery identity checks, which refuse it.
    pub(super) fn adopt_inherited_refresh(
        &mut self,
        auto_fix: &AutoFix,
        status: &PullRequestStatus,
    ) {
        if self.base_adopted {
            return;
        }
        let (Some(head), Some(candidate), Some(base_branch)) = (
            status.head_sha.as_deref(),
            self.pin.candidate_sha(),
            status.base_branch.as_deref(),
        ) else {
            return;
        };
        if head == candidate || !refresh_reserved(self.host, auto_fix, self.pr_number, candidate) {
            return;
        }
        if let Err(why) = verify_refreshed_head_from_base(
            &self.host.process_env(),
            Path::new(self.workspace_path),
            candidate,
            head,
            base_branch,
            status.head_branch.as_deref(),
            self.pr_number.parse().ok(),
        ) {
            tracing::warn!(
                pr_number = self.pr_number,
                head,
                candidate,
                %why,
                "pr_complete: a moved head is not the reserved base-only refresh of the candidate"
            );
            return;
        }
        self.pin.adopt_refreshed_candidate(Some(head));
        self.base_adopted = true;
    }
}

/// Whether a task this run watches reserved the base-inherited refresh of
/// `head` on this pull request. An unreadable history is no reservation.
fn refresh_reserved<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    pr_number: &str,
    head: &str,
) -> bool {
    auto_fix.task_ids.iter().any(|task| {
        host.get_task_history(task)
            .ok()
            .and_then(|history| pr_base_update_reservations(&history).ok())
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row.pr_number == pr_number
                        && row.head_sha == head
                        && row.evidence["cause"] == PR_BASE_INHERITED_REFRESH_CAUSE
                })
            })
    })
}
