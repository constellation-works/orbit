use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::operations;
use super::super::answers::{PullRequestStatus, PullRequestSummary};

pub(super) fn view_pr<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &std::path::Path,
    selector: &str,
) -> Result<(String, Option<String>, PullRequestStatus), OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_VIEW,
        json!({ "pr": selector, "workspace_path": workspace_path }),
    )?;
    let pull_request = PullRequestStatus::from_view_response(&response)?;
    let pr_number = pull_request.number_selector.clone().ok_or_else(|| {
        OrbitError::Execution(
            "private automation VCS PR view did not return a PR number".to_string(),
        )
    })?;
    Ok((pr_number, pull_request.url.clone(), pull_request))
}

pub(super) fn find_pr_by_head<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &std::path::Path,
    head: &str,
    expected_base: &str,
) -> Result<Option<(String, Option<String>, PullRequestStatus)>, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_LIST,
        json!({ "head": head, "state": "open", "workspace_path": workspace_path }),
    )?;
    let pull_requests = PullRequestSummary::list_from_response(&response)?;

    let mut matching_pr_number = None;
    let mut matching_list_base = None;
    for pull_request in pull_requests {
        let listed_head = pull_request.head_branch.as_deref().ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS PR list returned a pull request without headRefName"
                    .to_string(),
            )
        })?;
        if listed_head != head {
            continue;
        }
        let pr_number = pull_request.number.ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS PR list returned a matching pull request without a number"
                    .to_string(),
            )
        })?;
        if matching_pr_number.replace(pr_number).is_some() {
            return Err(OrbitError::Execution(format!(
                "private automation VCS PR list returned multiple open pull requests for head branch '{head}'"
            )));
        }
        matching_list_base = pull_request.base_branch.clone();
    }

    let Some(pr_number) = matching_pr_number else {
        return Ok(None);
    };
    let (pr_number, pr_url, pull_request) = view_pr(host, workspace_path, &pr_number)?;
    match pull_request
        .head_branch
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(live_head) if live_head == head => {}
        Some(live_head) => {
            return Err(OrbitError::Execution(format!(
                "pr reuse refused: pull request #{pr_number} reports head '{live_head}' but \
                 this handoff prepared head '{head}'; no second pull request was created"
            )));
        }
        None => {
            return Err(OrbitError::Execution(format!(
                "pr reuse refused: pull request #{pr_number} did not report headRefName, so \
                 its live head cannot be confirmed against prepared head '{head}'"
            )));
        }
    }
    // Both provider reads must agree with the prepared base: a list/view
    // disagreement is a retarget race, not a match.
    if let Some(list_base) = matching_list_base.as_deref() {
        ensure_live_base(&pr_number, Some(list_base), expected_base)?;
    }
    ensure_live_base(
        &pr_number,
        pull_request.base_branch.as_deref(),
        expected_base,
    )?;
    Ok(Some((pr_number, pr_url, pull_request)))
}

/// Refuse reuse when the live PR does not target the prepared base.
///
/// An absent `baseRefName` is unknown, not a match: reusing it would report
/// the requested base for a PR whose actual base was never read.
fn ensure_live_base(
    pr_number: &str,
    live_base: Option<&str>,
    expected_base: &str,
) -> Result<(), OrbitError> {
    let expected = expected_base
        .trim()
        .strip_prefix("origin/")
        .unwrap_or(expected_base.trim());
    let Some(live) = live_base.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(OrbitError::Execution(format!(
            "pr reuse refused: pull request #{pr_number} did not report baseRefName, so its \
             live base cannot be confirmed against prepared base '{expected}'; restore the \
             pull request's base metadata before retrying"
        )));
    };
    if live != expected {
        return Err(OrbitError::Execution(format!(
            "pr reuse refused: pull request #{pr_number} targets base '{live}' but this \
             handoff prepared base '{expected}'; restore the pull request's base to \
             '{expected}' or close it before retrying — no second pull request was created"
        )));
    }
    Ok(())
}

pub(super) struct PrOutput<'a> {
    pub(super) decision: &'a str,
    pub(super) pr_created: bool,
    pub(super) pr_reused: bool,
    pub(super) pr_number: String,
    pub(super) pr_url: Option<String>,
    pub(super) base: &'a str,
    pub(super) head: &'a str,
    pub(super) base_ref: &'a str,
    pub(super) base_sha: &'a str,
    pub(super) freshness: &'a super::super::super::freshness::BranchFreshness,
}

pub(super) fn pr_output(output: PrOutput<'_>) -> Value {
    json!({
        "phase": "pr_open",
        "decision": output.decision,
        "pr_created": output.pr_created,
        "pr_reused": output.pr_reused,
        "pr_number": output.pr_number,
        "pr_url": output.pr_url,
        "base": output.base,
        "head": output.head,
        "base_ref": output.base_ref,
        "base_sha": output.base_sha,
        "commits_behind": output.freshness.commits_behind,
        "commits_ahead": output.freshness.commits_ahead,
    })
}
