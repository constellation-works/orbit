//! Merge wait timeouts and reviewed-head checks.

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;
use serde_json::Value;

use super::super::super::super::input::input_string_field;
use super::super::answers::PullRequestStatus;
use super::super::delivery::{DeliveryPin, UnreportedChecks};

pub(super) fn merge_wait_timeout(
    pr_number: &str,
    waited_seconds: u64,
    max_wait_seconds: u64,
    base_race: Option<&str>,
    unreported: &UnreportedChecks,
) -> OrbitError {
    let base_race = base_race
        .map(|race| format!(" after {race}"))
        .unwrap_or_default();
    let unreported = unreported.timeout_note();
    OrbitError::DeliveryRefused {
        refusal: DeliveryRefusal::CompletionBudgetExpired,
        message: format!(
            "pr_complete: timed out after {waited_seconds}s waiting for pull request \
             #{pr_number} to merge (budget {max_wait_seconds}s){base_race}{unreported}; the task \
             stays in review"
        ),
    }
}

/// SHA the synchronous provider mutation must match.
///
/// A before-PR review wins when this run has one. Otherwise the published
/// candidate, including a head rewritten by the bounded conflict repair, is
/// the condition. With neither pin the merge stays ungated.
pub(super) fn merge_condition_sha<'a>(
    reviewed_head_sha: Option<&'a str>,
    pin: &'a DeliveryPin,
) -> Option<&'a str> {
    reviewed_head_sha.or(pin.candidate_sha())
}

/// The head a before-PR gate settled, when this run carried one.
pub(super) fn reviewed_head_sha(input: &Value) -> Option<String> {
    input_string_field(input, "reviewed_head_sha")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The head a selective before-landing selection receipt pinned, when the
/// landing gate recorded one for this run.
pub(super) fn selective_selection_head_sha(input: &Value) -> Option<String> {
    input_string_field(input, "selective_selection_head_sha")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// A selective decision — required or not — pins the head it evaluated.
/// An external push, auto-fix or rebase after it invalidates the decision:
/// completion refuses rather than merging a head no selection covers, and
/// a fresh gate evaluation on a new run re-decides.
pub(super) fn ensure_pr_head_matches_selection(
    status: &PullRequestStatus,
    pr_number: &str,
    selected_head_sha: Option<&str>,
) -> Result<(), OrbitError> {
    let Some(selected) = selected_head_sha else {
        return Ok(());
    };
    let reported = status.head_sha.as_deref().ok_or_else(|| {
        OrbitError::Execution(format!(
            "selection_stale: pull request #{pr_number} did not report its head commit, so \
             the selective selection at {selected} cannot be pinned before merging; the task \
             stays in review"
        ))
    })?;
    if reported != selected {
        return Err(OrbitError::Execution(format!(
            "selection_stale: pull request #{pr_number} head {reported} is not the head \
             {selected} the selective before-landing selection evaluated; the decision is \
             invalidated and must be re-evaluated before managed completion, so the task \
             stays in review"
        )));
    }
    Ok(())
}

/// Early diagnostic for an already moved head. The provider mutation must
/// also enforce the reviewed SHA atomically; this read cannot prevent a race.
pub(super) fn ensure_pr_head_is_reviewed(
    status: &PullRequestStatus,
    pr_number: &str,
    reviewed_head_sha: Option<&str>,
) -> Result<(), OrbitError> {
    let Some(reviewed) = reviewed_head_sha else {
        return Ok(());
    };
    let reported = status.head_sha.as_deref().ok_or_else(|| {
        OrbitError::Execution(format!(
            "review_gate_stale: pull request #{pr_number} did not report its head commit, so \
                 the reviewed candidate {reviewed} cannot be pinned before merging; the task stays \
                 in review"
        ))
    })?;
    if reported != reviewed {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: pull request #{pr_number} head {reported} is not the reviewed \
             candidate {reviewed}; later revisions need a fresh review before managed \
             completion, so the task stays in review"
        )));
    }
    Ok(())
}
