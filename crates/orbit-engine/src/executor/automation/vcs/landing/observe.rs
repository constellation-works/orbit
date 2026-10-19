//! Observing the candidate, its revisions and the landing ref, and adopting base updates.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{HandoffCandidate, HandoffDelivery};

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::git::{fetch_remote_base, git_command_success, git_output};
use super::super::pr::{DeliveryPin, PullRequestStatus};
use super::checks::verify_published_base_update;
use super::intent::LandingView;
use super::record::stop;
use super::run::LandingPass;

/// Observe the candidate, recording a durable stop when it cannot be confirmed.
///
/// A rewritten candidate, an unreachable base or a landing branch that no
/// longer resolves are all the same answer: this owner will not land what it
/// cannot verify, and the evidence says why.
pub(super) fn observe_or_stop<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    status: Option<&PullRequestStatus>,
    view: &LandingView,
) -> Result<HandoffCandidate, OrbitError> {
    match observe_candidate(context, status, view) {
        Ok(observed) => Ok(observed),
        Err(error) => Err(stop(host, context, &error.to_string())?),
    }
}

/// What this activity actually read about the candidate.
///
/// The candidate and base objects are resolved in the owner checkout and their
/// trees compared with the accepted identity, so a rewritten or re-pointed
/// commit cannot pass as the validated one. For a pull request the head and
/// base branch names come from the provider's own answer; `landing_branch` is
/// confirmed to resolve as a ref here. `repository` is the provider's
/// `owner/name` when a pull request is involved, and is carried from the
/// accepted candidate for a checkout with no remote.
pub(super) fn observe_candidate(
    context: &HandoffLandingContext,
    status: Option<&PullRequestStatus>,
    view: &LandingView,
) -> Result<HandoffCandidate, OrbitError> {
    let candidate = &context.candidate;
    let workspace = &context.workspace_path;
    let observed_candidate = observe_revision(workspace, &candidate.candidate, "candidate")?;
    let observed_base = observe_revision(workspace, &candidate.base, "base")?;
    let landing = resolve_landing_ref(workspace, &candidate.landing_branch, &candidate.delivery)?;
    // The validated base must still be reachable from the branch this lands on;
    // a rewritten history is not the base the evidence was produced against.
    if !matches!(candidate.delivery, HandoffDelivery::AlreadyLanded { .. })
        && !contains_commit(workspace, &landing, &observed_base.commit)?
    {
        return Err(OrbitError::Execution(format!(
            "handoff_land: validated base {} is no longer reachable from landing ref \
             '{landing}'{}",
            observed_base.commit,
            view.clause()
        )));
    }
    let (source_branch, base_branch) = match status {
        Some(status) => (
            status.head_branch.clone().ok_or_else(|| {
                OrbitError::Execution(
                    "handoff_land: pull request did not report its head branch".to_string(),
                )
            })?,
            status.base_branch.clone().ok_or_else(|| {
                OrbitError::Execution(
                    "handoff_land: pull request did not report its base branch".to_string(),
                )
            })?,
        ),
        None => (
            candidate.source_branch.clone(),
            candidate.base_branch.clone(),
        ),
    };
    Ok(HandoffCandidate {
        repository: candidate.repository.clone(),
        source_branch,
        base_branch,
        landing_branch: candidate.landing_branch.clone(),
        candidate: observed_candidate,
        base: observed_base,
        delivery: candidate.delivery.clone(),
    })
}

/// Resolve one accepted revision in the owner checkout.
///
/// The rule itself is shared with handoff acceptance [ORB-12500], so the
/// owner cannot read a candidate one way when it accepts the work and a
/// different way when it lands it.
fn observe_revision(
    workspace: &Path,
    accepted: &SourceRevision,
    label: &str,
) -> Result<SourceRevision, OrbitError> {
    super::super::claim::observe_accepted_revision(workspace, accepted, label, "handoff_land")
}

/// The ref a delivery actually lands on: `origin/<branch>` for published work
/// when the checkout has one, the local branch for an owner-local candidate. A
/// landing branch that resolves to neither cannot be verified.
pub(super) fn resolve_landing_ref(
    workspace: &Path,
    landing_branch: &str,
    delivery: &HandoffDelivery,
) -> Result<String, OrbitError> {
    // Owner-local delivery has no remote to land on: its target is the local
    // branch, and checking `origin/` would read a ref the merge never moves.
    let refs = match delivery {
        HandoffDelivery::LocalCandidate => vec![landing_branch.to_string()],
        _ => vec![
            format!("origin/{landing_branch}"),
            landing_branch.to_string(),
        ],
    };
    for candidate in refs {
        if git_command_success(
            workspace,
            &["rev-parse", "--verify", &format!("{candidate}^{{commit}}")],
        )? {
            return Ok(candidate);
        }
    }
    Err(OrbitError::Execution(format!(
        "handoff_land: landing branch '{landing_branch}' does not resolve in the owner checkout"
    )))
}

pub(super) fn contains_commit(
    workspace: &Path,
    reference: &str,
    commit: &str,
) -> Result<bool, OrbitError> {
    if !git_command_success(
        workspace,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )? {
        return Ok(false);
    }
    git_command_success(
        workspace,
        &["merge-base", "--is-ancestor", commit, reference],
    )
}

/// [ORB-15956] Pin the pull request's reported head instead of the accepted
/// candidate when it is a verified provider branch update of that candidate.
/// Returns the adopted head. Any other head is left to the pin to refuse.
///
/// The provider merges the base as it is when the update runs, which can be
/// past the landing ref this attempt fetched, so a published landing ref is
/// fetched again before a head not yet adopted is verified against it. A
/// fetch that fails refuses the head, even if the initial refresh failed and
/// the head and base are cached [ORB-15976]. Without `origin`, verification
/// intentionally uses the owner's local landing branch.
pub(super) fn adopt_base_update(
    pin: &mut DeliveryPin,
    context: &HandoffLandingContext,
    status: &PullRequestStatus,
) -> Option<String> {
    let candidate = &context.candidate;
    let head = status.head_sha.as_deref()?;
    if head == candidate.candidate.commit
        || status.head_branch.as_deref() != Some(candidate.source_branch.as_str())
    {
        return None;
    }
    if pin.candidate_sha() == Some(head) {
        return Some(head.to_string());
    }
    match verify_published_base_update(&context.workspace_path, candidate, status) {
        Ok(()) => {
            if pin.candidate_sha() != Some(head) {
                pin.adopt_refreshed_candidate(Some(head));
            }
            Some(head.to_string())
        }
        Err(refusal) => {
            tracing::warn!(
                head,
                %refusal,
                "handoff_land: the pull request's head is not a verified base update of the \
                 candidate"
            );
            None
        }
    }
}

/// The landing branch's current commit in the owner checkout, refreshed from
/// `origin` when the landing reads published delivery.
pub(super) fn landing_tip(context: &HandoffLandingContext, view: &LandingView) -> Option<String> {
    let candidate = &context.candidate;
    if view.refreshed {
        let _ = fetch_remote_base(&context.workspace_path, &candidate.landing_branch);
    }
    let landing = resolve_landing_ref(
        &context.workspace_path,
        &candidate.landing_branch,
        &candidate.delivery,
    )
    .ok()?;
    git_output(
        &context.workspace_path,
        &["rev-parse", &format!("{landing}^{{commit}}")],
    )
    .ok()
}

pub(super) fn pin_for(candidate: &HandoffCandidate) -> DeliveryPin {
    DeliveryPin::pinned(
        &candidate.source_branch,
        &candidate.base_branch,
        &candidate.candidate.commit,
    )
}
