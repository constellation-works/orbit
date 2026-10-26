//! Classifying forge merge guards without turning unreadable or red checks into holds.

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;

use super::answers::{CheckState, PullRequestStatus, ReviewDecision, StatusCheck};

/// What GitHub's reported PR state means for a completion or landing attempt.
pub(in crate::executor::automation::vcs) enum PrMergeState {
    Merged,
    Closed,
    /// The provider's answer disagrees with itself about whether this PR
    /// merged. Completion refuses rather than picking the convenient half.
    Contradictory(String),
    /// Merging is refused by a gate this run must not bypass.
    Blocked(PrMergeBlock),
    /// GitHub reports a content conflict. The local rebase boundary must prove
    /// actual unmerged entries before an agent may be launched.
    Conflict,
    /// Ready to merge now.
    Mergeable,
    /// Required checks are still in flight.
    Pending,
}

/// A merge guard and, only for a deliberate parked outcome, its typed hold.
pub(in crate::executor::automation::vcs) struct PrMergeBlock {
    reason: String,
    pub(in crate::executor::automation::vcs) refusal: Option<DeliveryRefusal>,
}

impl PrMergeBlock {
    fn held(reason: impl Into<String>, refusal: DeliveryRefusal) -> Self {
        Self {
            reason: reason.into(),
            refusal: Some(refusal),
        }
    }

    pub(super) fn into_error(self, message: String) -> OrbitError {
        match self.refusal {
            Some(refusal) => OrbitError::DeliveryRefused { refusal, message },
            None => OrbitError::Execution(message),
        }
    }
}

impl From<String> for PrMergeBlock {
    fn from(reason: String) -> Self {
        Self {
            reason,
            refusal: None,
        }
    }
}

impl From<&str> for PrMergeBlock {
    fn from(reason: &str) -> Self {
        reason.to_string().into()
    }
}

impl std::fmt::Display for PrMergeBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

pub(in crate::executor::automation::vcs) fn classify_pr_state(
    pull_request: &PullRequestStatus,
) -> PrMergeState {
    let state = pull_request
        .state
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if state == "MERGED" {
        return PrMergeState::Merged;
    }
    // [ORB-11982] A merge timestamp on a pull request the provider does not
    // report as merged is the shape F2026-09-102 recorded: an open PR that
    // nonetheless looks delivered. Only the authoritative `state` may close a
    // run, so the disagreement itself becomes the refusal.
    if let Some(merged_at) = &pull_request.merged_at {
        return PrMergeState::Contradictory(format!(
            "state '{state}' alongside merge timestamp {merged_at}"
        ));
    }
    if state == "CLOSED" {
        return PrMergeState::Closed;
    }

    // [ORB-16342] GitHub can report a draft with a merge state other than
    // DRAFT, including BLOCKED, UNKNOWN and CLEAN. Completion used to request
    // a merge from those states and spend recovery on HTTP 405. An explicit
    // draft is refused before that match. The DRAFT arm below still covers an
    // answer that omits `isDraft`.
    if pull_request.is_draft {
        return PrMergeState::Blocked(PrMergeBlock::held(
            "the pull request is still a draft",
            DeliveryRefusal::PullRequestDraft,
        ));
    }

    let merge_state = pull_request
        .merge_state_status
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase();
    match merge_state.as_str() {
        // Mergeable now: no gate outstanding, or only non-required signals.
        "CLEAN" | "HAS_HOOKS" | "UNSTABLE" => PrMergeState::Mergeable,
        // Required checks still running.
        "PENDING" => PrMergeState::Pending,
        "BLOCKED" => classify_blocked_pr(pull_request),
        "DIRTY" => PrMergeState::Conflict,
        "BEHIND" => {
            PrMergeState::Blocked("the branch is behind its base and must be updated".into())
        }
        "DRAFT" => PrMergeState::Blocked(PrMergeBlock::held(
            "the pull request is still a draft",
            DeliveryRefusal::PullRequestDraft,
        )),
        // An empty or unrecognized merge state is treated as still settling:
        // GitHub reports UNKNOWN while it computes mergeability.
        _ => PrMergeState::Pending,
    }
}

/// Review and readable-rollup refusals shared by landing and auto-fix.
/// Display is diagnostic only; recovery authority matches the variants.
#[derive(Debug, thiserror::Error)]
pub(super) enum BlockedCheckRefusal {
    #[error("review is required ({0})")]
    ReviewRequired(String),
    #[error("review decision is unavailable")]
    ReviewUnavailable,
    #[error("status check rollup is unreadable{suffix}", suffix = .0.as_ref().map(|name| format!(" for '{name}'")).unwrap_or_default())]
    RollupUnreadable(Option<String>),
}

impl BlockedCheckRefusal {
    pub(super) fn delivery_refusal(&self) -> Option<DeliveryRefusal> {
        matches!(self, Self::ReviewRequired(_)).then_some(DeliveryRefusal::RequirementsUnsatisfied)
    }
}

/// Review and readable-rollup guards precede any pending-check or
/// source-repair authority.
pub(super) fn blocked_checks(
    pull_request: &PullRequestStatus,
) -> Result<&[StatusCheck], BlockedCheckRefusal> {
    match &pull_request.review_decision {
        ReviewDecision::ReviewRequired | ReviewDecision::ChangesRequested => {
            return Err(BlockedCheckRefusal::ReviewRequired(
                pull_request
                    .review_decision
                    .provider_spelling()
                    .unwrap_or_default()
                    .to_string(),
            ));
        }
        ReviewDecision::NotApplicable | ReviewDecision::Approved => {}
        ReviewDecision::Other(_) | ReviewDecision::Unavailable => {
            return Err(BlockedCheckRefusal::ReviewUnavailable);
        }
    }
    let checks = pull_request
        .status_checks
        .as_deref()
        .ok_or(BlockedCheckRefusal::RollupUnreadable(None))?;
    for check in checks {
        let name = check
            .name
            .as_deref()
            .filter(|name| !name.is_empty())
            .ok_or(BlockedCheckRefusal::RollupUnreadable(None))?;
        if check.state.is_none() {
            return Err(BlockedCheckRefusal::RollupUnreadable(Some(
                name.to_string(),
            )));
        }
    }
    Ok(checks)
}

fn classify_blocked_pr(pull_request: &PullRequestStatus) -> PrMergeState {
    let checks = match blocked_checks(pull_request) {
        Ok(checks) => checks,
        Err(reason) => {
            let block = match reason
                .delivery_refusal()
                .filter(|_| !has_failed_pr_check(pull_request))
            {
                Some(refusal) => PrMergeBlock::held(reason.to_string(), refusal),
                None => reason.to_string().into(),
            };
            return PrMergeState::Blocked(block);
        }
    };
    for check in checks {
        if check.state == Some(CheckState::Failed) {
            return PrMergeState::Blocked(
                format!(
                    "status check '{}' failed",
                    check.name.as_deref().unwrap_or_default()
                )
                .into(),
            );
        }
    }
    // A rollup alone cannot say which pending checks protection requires.
    PrMergeState::Blocked(PrMergeBlock::held(
        "required reviews or checks are not satisfied",
        DeliveryRefusal::RequirementsUnsatisfied,
    ))
}

/// A red rollup cannot establish an administrative-only hold.
pub(super) fn has_failed_pr_check(status: &PullRequestStatus) -> bool {
    status.status_checks.as_ref().is_some_and(|checks| {
        checks
            .iter()
            .any(|check| check.state == Some(CheckState::Failed))
    })
}
