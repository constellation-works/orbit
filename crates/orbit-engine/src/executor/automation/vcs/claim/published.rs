use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{HandoffCandidate, HandoffDelivery};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::git::{git_command_success, git_output, git_success};
use super::super::pr::{DeliveryPin, PrMergeState, classify_pr_state};
use super::super::review_gate::revision;
use super::input::refused;
use super::slug;

/// Resolve one accepted revision in the *owner's* checkout, fetching it first
/// when the object only exists on the remote [ORB-12500].
///
/// The tree comparison is what makes this an observation rather than a
/// restatement: a commit id the owner can read but whose tree disagrees with
/// the accepted identity is not the object the evidence was produced against.
/// Both owner-side consumers — handoff acceptance here and the landing
/// attempt — resolve accepted revisions through this one rule.
pub(in crate::executor::automation::vcs) fn observe_accepted_revision(
    workspace_path: &Path,
    accepted: &SourceRevision,
    label: &str,
    context: &str,
) -> Result<SourceRevision, OrbitError> {
    if revision(workspace_path, &accepted.commit).is_err() {
        let _ = git_success(
            workspace_path,
            &[
                "fetch",
                "--quiet",
                "--end-of-options",
                "origin",
                &accepted.commit,
            ],
        );
    }
    let observed = revision(workspace_path, &accepted.commit).map_err(|error| {
        OrbitError::Execution(format!(
            "{context}: {label} commit {} is not readable in the owner checkout: {error}",
            accepted.commit
        ))
    })?;
    if observed.tree != accepted.tree {
        return Err(OrbitError::Execution(format!(
            "{context}: {label} commit {} has tree {} but the accepted handoff recorded {}",
            accepted.commit, observed.tree, accepted.tree
        )));
    }
    Ok(observed)
}

/// The owner's independent observation of a *published* candidate [ORB-12500].
///
/// A follower's word that it opened a pull request is not evidence that one
/// exists, points at the candidate it validated, or belongs to this
/// repository. This is the owner reading all three for itself before its claim
/// journal is allowed to promote anything:
///
/// 1. **The provider names the delivery.** The pull request's head branch,
///    base branch and head commit are read from the provider and pinned
///    against the submitted candidate, so a branch repointed after the handoff
///    was written is a refusal rather than an acceptance of stale identity.
/// 2. **A closed or self-contradictory pull request delivers nothing.** A
///    merged one is observable: the external write already happened and
///    refusing it here would only strand the settlement — completion still
///    requires separately recorded authority.
/// 3. **The objects are resolved in the owner's own checkout**, with the same
///    tree-identity and ancestry rules the executor applied, so the owner is
///    never quoting the worker's arithmetic back to itself.
///
/// `landing_branch` is the one field carried from the submitted candidate: it
/// is owner-resolved ship configuration captured at admission, not anything a
/// pull request reports.
pub fn observe_published_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    submitted: &HandoffCandidate,
) -> Result<HandoffCandidate, OrbitError> {
    let HandoffDelivery::PullRequest { number } = submitted.delivery else {
        return Err(refused(
            "owner observation of a published candidate requires a pull-request delivery",
        ));
    };
    let pr_number = number.to_string();
    let response = host.run_private_vcs_operation(
        super::super::operations::PR_STATUS,
        json!({
            "pr": pr_number,
            "workspace_path": workspace_path.to_string_lossy(),
        }),
    )?;
    let status = response.get("pull_request").cloned().unwrap_or(Value::Null);

    match classify_pr_state(&status) {
        PrMergeState::Closed => {
            return Err(refused(format!(
                "pull request #{pr_number} was closed without merging; it delivers no \
                 candidate for this handoff"
            )));
        }
        PrMergeState::Contradictory(reason) => {
            return Err(refused(format!(
                "pull request #{pr_number} reports a contradictory merge state ({reason}); \
                 the owner accepts a delivery it can read unambiguously"
            )));
        }
        // Merged, mergeable, blocked, conflicted and pending all describe a
        // live delivery. Whether it may *land* is the landing attempt's
        // question, asked again against authority this acceptance does not
        // grant.
        PrMergeState::Merged
        | PrMergeState::Blocked(_)
        | PrMergeState::Conflict
        | PrMergeState::Mergeable
        | PrMergeState::Pending => {}
    }

    DeliveryPin::pinned(
        &submitted.source_branch,
        &submitted.base_branch,
        &submitted.candidate.commit,
    )
    .ensure_pinned_candidate(&status, &pr_number)?;

    // `gh` resolves a bare PR number against the *checkout's* own remote, so
    // the delivery is already scoped to this repository. Comparing the URL it
    // reports is still worth doing when the owner's origin is a provider
    // remote whose slug is comparable: that catches a handoff pointing at a
    // fork's pull request.
    let origin_url = git_output(workspace_path, &["remote", "get-url", "origin"]).ok();
    let repository = origin_url
        .as_deref()
        .and_then(slug)
        .unwrap_or_else(|| submitted.repository.clone());
    if origin_url
        .as_deref()
        .is_some_and(|url| url.contains("github.com"))
        && let Some(published) = published_repository(&status)
        && published != repository
    {
        return Err(refused(format!(
            "pull request #{pr_number} belongs to repository '{published}', not this \
             owner's '{repository}'"
        )));
    }

    let context = format!("handoff acceptance for pull request #{pr_number}");
    let candidate =
        observe_accepted_revision(workspace_path, &submitted.candidate, "candidate", &context)?;
    let base = observe_accepted_revision(workspace_path, &submitted.base, "base", &context)?;
    if !git_command_success(
        workspace_path,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            &base.commit,
            &candidate.commit,
        ],
    )? {
        return Err(refused(format!(
            "{context}: candidate '{}' does not descend from validated base '{}' in \
             the owner checkout",
            candidate.commit, base.commit
        )));
    }

    Ok(HandoffCandidate {
        repository,
        source_branch: reported(&status, "headRefName").ok_or_else(|| {
            refused(format!(
                "pull request #{pr_number} did not report its head branch"
            ))
        })?,
        base_branch: reported(&status, "baseRefName").ok_or_else(|| {
            refused(format!(
                "pull request #{pr_number} did not report its base branch"
            ))
        })?,
        landing_branch: submitted.landing_branch.clone(),
        candidate,
        base,
        delivery: submitted.delivery.clone(),
    })
}

fn reported(status: &Value, field: &str) -> Option<String> {
    status
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// The repository a pull request URL names, when the provider reported one.
fn published_repository(status: &Value) -> Option<String> {
    let url = reported(status, "url")?;
    let trimmed = url.trim_end_matches('/');
    let (repository, _) = trimmed.rsplit_once("/pull/")?;
    slug(repository)
}
