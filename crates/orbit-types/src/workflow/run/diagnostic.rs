//! Operator context appended to CLI and orphaned-worker failures.

use super::agent_blocker::{TASK_BLOCKED_BY_AGENT_MARKER, UPGRADE_PENDING_MARKER};
use super::baseline::{BASELINE_RED_MARKER, KNOWN_RED_MARKER};
use super::flaky::MARKER as FLAKY_VALIDATION_MARKER;
use super::forge_hold::FORGE_UNAVAILABLE_MARKER;
use super::provider_hold::PROVIDER_FAILURE_HOLD_MARKER;
use super::state::{
    CONTRACT_REFUSAL_MARKER, OWNER_BUSY_MARKER, OWNER_ROUTE_UNAVAILABLE_MARKER,
    OWNER_SIGNOFF_REQUIRED_DIAGNOSTIC_TOKEN, PROVIDER_CAPACITY_MARKER, PROVIDER_LIMIT_MARKER,
    PROVIDER_REFUSAL_MARKER, PROVIDER_UNAVAILABLE_MARKER, STALLED_MARKER, TRANSIENT_FAILURE_MARKER,
    VALIDATION_ENVIRONMENT_MARKER, VALIDATION_ROUTE_UNAVAILABLE_MARKER,
};
use crate::workflow::{
    IMAGE_NOT_SEEN_MARKER, PR_AUTO_FIX_CHECK_REPAIR_MARKER, PR_CANDIDATE_WORKFLOW_FAILURE_MARKER,
    REVIEW_ABANDONED_MARKER, REVIEW_LANDING_DECISION_PENDING, REVIEW_PR_MERGED_OUTSIDE_ORBIT,
    REVIEW_REPAIR_AFTER_MERGE_MARKER,
};

/// Separates a generic CLI exit failure from its untrusted stderr context.
pub const CLI_STDERR_TAIL_SEPARATOR: &str = "\nstderr tail:\n";

/// Separates a generic CLI exit from an unclassified provider error frame.
pub const CLI_PROVIDER_ERROR_SEPARATOR: &str = "\nprovider error:\n";

/// Diagnostic-only worker log provenance appended to an orphan failure.
pub const WORKER_PROVENANCE_SEPARATOR: &str = "\nworker provenance:\n";

/// Control-plane markers and diagnostic tokens paired with the ASCII byte
/// offset where a zero-width joiner breaks classifier matching while keeping
/// the visible wording intact.
const CONTROL_PLANE_MARKER_SPLITS: &[(&str, usize)] = &[
    (VALIDATION_ENVIRONMENT_MARKER, 1),
    (VALIDATION_ROUTE_UNAVAILABLE_MARKER, 1),
    (BASELINE_RED_MARKER, 1),
    (KNOWN_RED_MARKER, 1),
    (TASK_BLOCKED_BY_AGENT_MARKER, 1),
    (UPGRADE_PENDING_MARKER, 1),
    (OWNER_ROUTE_UNAVAILABLE_MARKER, 1),
    (PROVIDER_UNAVAILABLE_MARKER, 1),
    (PROVIDER_LIMIT_MARKER, 1),
    (PROVIDER_CAPACITY_MARKER, 1),
    (PROVIDER_REFUSAL_MARKER, 1),
    (PROVIDER_FAILURE_HOLD_MARKER, 1),
    (STALLED_MARKER, 1),
    (TRANSIENT_FAILURE_MARKER, 1),
    (FORGE_UNAVAILABLE_MARKER, 1),
    (OWNER_BUSY_MARKER, 1),
    (FLAKY_VALIDATION_MARKER, 1),
    (IMAGE_NOT_SEEN_MARKER, 1),
    (PR_AUTO_FIX_CHECK_REPAIR_MARKER, 3),
    (OWNER_SIGNOFF_REQUIRED_DIAGNOSTIC_TOKEN, 11),
    (CONTRACT_REFUSAL_MARKER, 11),
    (PR_CANDIDATE_WORKFLOW_FAILURE_MARKER, 10),
    (REVIEW_LANDING_DECISION_PENDING, 16),
    (REVIEW_ABANDONED_MARKER, 7),
    (REVIEW_PR_MERGED_OUTSIDE_ORBIT, 16),
    (REVIEW_REPAIR_AFTER_MERGE_MARKER, 6),
];

/// Neutralize Orbit control-plane markers inside untrusted child output so
/// embedded diagnostics remain readable without triggering failure classifiers.
#[must_use]
pub fn neutralize_control_plane_markers(text: &str) -> String {
    if !CONTROL_PLANE_MARKER_SPLITS
        .iter()
        .any(|(marker, _)| text.contains(marker))
    {
        return text.to_string();
    }
    let mut neutralized = text.to_string();
    for &(marker, split) in CONTROL_PLANE_MARKER_SPLITS {
        if neutralized.contains(marker) {
            let (prefix, suffix) = marker.split_at(split);
            let replacement = format!("{prefix}\u{200d}{suffix}");
            neutralized = neutralized.replace(marker, &replacement);
        }
    }
    neutralized
}

/// The authoritative failure text, excluding generic CLI diagnostic context.
///
/// Wrappers may prefix a step or run name. Only generic CLI failures can
/// introduce this suffix: a bare exit or signal termination, or either form
/// followed by a provider terminal diagnostic [ORB-24697]. Provider-specific
/// diagnostics retain their existing classification. Stderr can quote denial
/// words, typed failure markers or the check-repair handoff without acquiring
/// control-plane meaning.
/// Orphan diagnostics likewise exclude their log-backed worker provenance:
/// a returned error there describes evidence, not a new settlement decision.
#[must_use]
pub fn failure_classification_text(message: &str) -> &str {
    if let Some((failure, _)) = message.split_once(WORKER_PROVENANCE_SEPARATOR)
        && failure.contains("worker exit:")
    {
        return failure;
    }
    let Some(boundary) = [CLI_STDERR_TAIL_SEPARATOR, CLI_PROVIDER_ERROR_SEPARATOR]
        .into_iter()
        .filter_map(|separator| message.find(separator))
        .min()
    else {
        return message;
    };
    let failure = &message[..boundary];
    let generic_exit = failure
        .rsplit_once("cli subprocess exited with code ")
        .is_some_and(|(_, code)| {
            code.split_whitespace()
                .next()
                .is_some_and(|code| code.parse::<i32>().is_ok())
        });
    let generic_signal = failure
        .rsplit_once("cli subprocess was terminated by a signal and reported no exit code")
        .is_some_and(|(_, diagnostic)| diagnostic.is_empty() || diagnostic.starts_with(' '));
    if generic_exit || generic_signal {
        failure
    } else {
        message
    }
}
