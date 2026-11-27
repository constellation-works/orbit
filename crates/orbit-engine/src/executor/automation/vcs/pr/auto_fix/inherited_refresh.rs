//! [ORB-26860] After an admitted no-edit repair's one rerun on the unchanged
//! head failed again, refresh the run's own pull request to its base tip when
//! the failure is base-inherited, or hold saying why not.
//!
//! The refresh is reserved first in the shared per-PR base-update ledger
//! (`evidence.cause = "base_inherited_refresh"`), so a head is refreshed at
//! most once, and the write is the host's one refresh operation (the one
//! behind `orbit.workflow` action `refresh`): a single conflict-free merge
//! commit of the base tip, never a rebase or force-push. The completion then
//! adopts the refreshed head only after verifying it carries base commits
//! alone, and the forge reruns the required checks on it.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    DeliveryRefreshRequest, PR_BASE_INHERITED_REFRESH_CAUSE, PrBaseUpdateAdmission,
    PrBaseUpdateRefusal, PrBaseUpdateReservation, PullRequestRefreshOutcome,
};
use serde_json::json;

use crate::context::{HostEnvironment, ReviewGate, RuntimeHostProcessEnvironment};

use super::super::super::super::input::input_string_field;
use super::super::super::checks::{
    self, CheckReadGap, DeferredLogRead, InheritedFailure, verify_refreshed_head_from_base,
};
use super::super::super::git::{self, git_output};
use super::checks::{AutoFixPull, required_failures};
use crate::executor::automation::vcs::pr::answers::read_pr_status;

/// What the base-inherited refresh step decided for one failed head.
pub(in crate::executor::automation::vcs::pr) enum InheritedRefresh {
    /// The base tip was merged into the head; the completion repolls and
    /// adopts the verified refreshed head.
    Refreshed {
        /// The head the rerun failed on.
        from: String,
        /// The refreshed head the forge reported.
        to: String,
    },
    /// A failed job's log read was throttled; repoll after the wait.
    Deferred(Box<DeferredLogRead>),
    /// No refresh ran or it was refused; hold with this reason.
    Hold(String),
}

/// Decide, after `rerun_failure` (the no-edit rerun's failure reason),
/// whether the failure on `status`'s head is base-inherited and, if so,
/// reserve and run the head's one base refresh.
pub(in crate::executor::automation::vcs::pr) fn refresh_inherited_failure<
    H: ReviewGate + HostEnvironment + RuntimeHostProcessEnvironment + ?Sized,
>(
    host: &H,
    pull: AutoFixPull<'_>,
    rerun_failure: &str,
    deadline_ms: u64,
) -> Result<InheritedRefresh, OrbitError> {
    let AutoFixPull {
        auto_fix,
        input,
        workspace_path,
        pr_number,
        status,
    } = pull;
    let hold = |why: String| {
        Ok(InheritedRefresh::Hold(format!(
            "{rerun_failure}; the head is not refreshed to its base tip: {why}"
        )))
    };
    let (Some(head), Some(head_branch), Some(base_branch)) = (
        status.head_sha.as_deref(),
        status.head_branch.as_deref(),
        status.base_branch.as_deref(),
    ) else {
        return hold(
            "the pull request did not report its head, head branch and base branch".to_string(),
        );
    };
    let Ok(number) = pr_number.parse::<u64>() else {
        return hold(format!("'{pr_number}' is not a pull request number"));
    };
    let workspace = Path::new(workspace_path);
    let failed = checks::failed_checks(status);
    let (failed, _) = match required_failures(host, workspace_path, pr_number, head, &failed) {
        Ok(found) => found,
        Err(reason) => return hold(reason),
    };
    let Some(base_name) = input_string_field(input, "base") else {
        return hold("the input names no base".to_string());
    };
    let (_, branch, tip) = match git::resolve_pr_base(
        &host.process_env(),
        workspace,
        input,
        &base_name,
        Some(head),
    ) {
        Ok(resolved) => resolved,
        Err(reason) => return hold(reason),
    };
    let evidence = match checks::base_inherited_failure(
        host,
        workspace,
        head,
        &branch,
        &tip,
        &failed,
        Some(deadline_ms),
    ) {
        Ok(InheritedFailure::Inherited(evidence)) => evidence,
        Ok(InheritedFailure::NotInherited(why)) => return hold(why),
        Err(CheckReadGap::Deferred(deferred)) => return Ok(InheritedRefresh::Deferred(deferred)),
        Err(CheckReadGap::Unavailable(why)) => return hold(why),
    };
    let Some(repository) = git_output(
        &host.process_env(),
        workspace,
        &["remote", "get-url", "origin"],
    )
    .ok()
    .and_then(|url| super::super::super::claim::slug(&url)) else {
        return hold("the origin repository cannot be named".to_string());
    };
    let reservation = PrBaseUpdateReservation {
        execution: auto_fix.run_id.clone(),
        repository,
        pr_number: pr_number.to_string(),
        head_sha: head.to_string(),
        head_branch: head_branch.to_string(),
        base_branch: base_branch.to_string(),
        base_sha: tip.clone(),
        deadline_ms,
        evidence: json!({
            "cause": PR_BASE_INHERITED_REFRESH_CAUSE,
            "trigger": "no_edit_rerun_failed",
            "rerun_failure": rerun_failure,
            "fork_point": evidence["fork_point"],
            "landed_base": evidence["landed_base"],
            "checks": evidence["checks"],
        }),
    };
    match host.admit_pr_base_update(&auto_fix.task_ids, &reservation)? {
        PrBaseUpdateAdmission::Reserved => {}
        PrBaseUpdateAdmission::Observe(previous) => {
            return hold(format!(
                "the one base refresh of head {} was already reserved toward {} and the head \
                 has not moved, so it is not sent again",
                previous.head_sha, previous.base_sha
            ));
        }
        PrBaseUpdateAdmission::Refused(PrBaseUpdateRefusal::BudgetSpent { .. }) => {
            return hold(format!(
                "pull request #{pr_number} already spent its one base update, and a head is \
                 refreshed at most once"
            ));
        }
        PrBaseUpdateAdmission::Refused(refusal) => {
            return hold(format!("the base refresh was not admitted: {refusal}"));
        }
    }
    let request = DeliveryRefreshRequest {
        run_id: auto_fix.run_id.clone(),
        task_ids: auto_fix.task_ids.clone(),
        pr_number: number,
        head_branch: head_branch.to_string(),
        base_branch: base_branch.to_string(),
        expected_head_sha: head.to_string(),
    };
    let base_only = |to: &str| {
        verify_refreshed_head_from_base(
            &host.process_env(),
            workspace,
            head,
            to,
            base_branch,
            Some(head_branch),
            Some(number),
        )
    };
    match host.refresh_delivery_pull_request(&request) {
        Ok(PullRequestRefreshOutcome::Updated { head_sha: to }) => match base_only(&to) {
            Ok(()) => Ok(InheritedRefresh::Refreshed {
                from: head.to_string(),
                to,
            }),
            Err(why) => hold(format!(
                "the refreshed head {to} is not a base-only merge of {head}: {why}"
            )),
        },
        Ok(PullRequestRefreshOutcome::AlreadyUpToDate { .. }) => hold(format!(
            "head {head} already contains the tip of '{base_branch}'"
        )),
        Ok(PullRequestRefreshOutcome::Conflicting {
            conflicting_paths,
            reason,
        }) => {
            let paths = if conflicting_paths.is_empty() {
                "(paths unavailable)".to_string()
            } else {
                conflicting_paths.join(", ")
            };
            hold(format!(
                "the base refresh was refused because merging '{base_branch}' into head {head} \
                 conflicts ({reason}); conflicting paths: {paths}; the head is unchanged"
            ))
        }
        Ok(PullRequestRefreshOutcome::Refused { reason }) => hold(format!(
            "the base refresh was refused: {reason}; the head is unchanged"
        )),
        Err(error) => {
            // The write may have applied with its reply lost. Only a verified
            // base-only head counts as refreshed; anything else holds.
            if let Ok(observed) = read_pr_status(host, workspace, pr_number)
                && let Some(to) = observed.head_sha.filter(|to| to != head)
                && base_only(&to).is_ok()
            {
                return Ok(InheritedRefresh::Refreshed {
                    from: head.to_string(),
                    to,
                });
            }
            hold(format!("the base refresh failed: {error}"))
        }
    }
}
