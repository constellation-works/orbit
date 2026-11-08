//! Read-only observations for the owner's bounded stopped-landing retry loop.

use orbit_common::OrbitError;
use orbit_types::workflow::handoff::{HandoffDelivery, LandingWaitGate};
use serde_json::{Value, json};

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::git::{fetch_remote_base, git_output};
use super::super::operations::{self, CiCommitJobsAnswer, CiJobConclusion, CiJobStatus};
use super::super::pr::{
    BlockedCheckWait, OwedHostedChecks, PrMergeState, ReviewDecision, UnreportedChecks,
    checked_pr_state, classify_pr_state, owed_hosted_checks, read_pr_status,
    read_required_checks_observed,
};

/// Observe whether a stopped waiting gate has cleared without mutating the
/// forge or candidate. This grants only a new attempt: normal landing rechecks
/// every gate and identity before it may merge.
pub fn landing_wait_gate_cleared<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    gate: LandingWaitGate,
) -> Result<bool, OrbitError> {
    let HandoffDelivery::PullRequest { number } = context.candidate.delivery else {
        return Ok(false);
    };
    let workspace = &context.workspace_path;
    let pr = number.to_string();
    let status = read_pr_status(host, workspace, &pr)?;
    if status.head_sha.as_deref() != Some(&context.candidate.candidate.commit)
        && super::checks::verify_published_base_update(workspace, &context.candidate, &status)
            .is_err()
    {
        return Ok(false);
    }
    if status.head_branch.as_deref() != Some(&context.candidate.source_branch)
        || status.base_branch.as_deref() != Some(&context.candidate.base_branch)
        || status.is_draft
        || !matches!(
            status.review_decision,
            ReviewDecision::Approved | ReviewDecision::NotApplicable
        )
        || matches!(
            classify_pr_state(&status),
            PrMergeState::Closed | PrMergeState::Conflict | PrMergeState::Contradictory(_)
        )
    {
        return Ok(false);
    }
    if gate == LandingWaitGate::BaseChecksPending {
        // A base can turn green while the old candidate checks remain red.
        // Wake landing so its existing base-refresh machinery can act. Require
        // fresh exact-tip push evidence for every currently required check.
        fetch_remote_base(workspace, &context.candidate.base_branch)?;
        let tip = git_output(
            workspace,
            &[
                "rev-parse",
                &format!("refs/remotes/origin/{}", context.candidate.base_branch),
            ],
        )?;
        let required = read_required_checks_observed(host, workspace, &pr, &mut Value::Null)?;
        if Some(required.head_sha.as_str()) != status.head_sha.as_deref()
            || required.reported_head_sha.as_deref() != Some(&required.head_sha)
            || required.required.is_empty()
        {
            return Ok(false);
        }
        let response = host.run_private_vcs_operation(
            operations::CI_COMMIT_JOBS,
            json!({"commit":tip.trim(), "workspace_path":workspace,
                "latest_push_branch":context.candidate.base_branch}),
        )?;
        let Some(answer) = CiCommitJobsAnswer::decode_response(response) else {
            return Ok(false);
        };
        return Ok(required.required.iter().all(|(workflow, name)| {
            let matches = answer
                .jobs
                .iter()
                .filter(|job| {
                    job.name.text() == Some(name.as_str())
                        && workflow
                            .as_deref()
                            .is_none_or(|workflow| job.workflow.text() == Some(workflow))
                })
                .collect::<Vec<_>>();
            matches.len() == 1
                && matches[0].status.value() == Some(&CiJobStatus::Completed)
                && matches[0].conclusion.value() == Some(&CiJobConclusion::Success)
        }));
    }
    let mut unreported = UnreportedChecks::new();
    let state = checked_pr_state(
        host,
        workspace,
        &pr,
        &status,
        BlockedCheckWait::Owner(&context.owed_hosted_checks),
        &mut unreported,
        None,
    );
    Ok(matches!(state, Ok(PrMergeState::Mergeable))
        && owed_hosted_checks(&status, &context.owed_hosted_checks) == OwedHostedChecks::Passed)
}
