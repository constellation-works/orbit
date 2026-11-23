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
