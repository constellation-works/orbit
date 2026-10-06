use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::text::floor_char_boundary;
use orbit_types::workflow::ReviewTiming;
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition,
    HandoffReviewEvidence, TaskHandoff,
};
use serde_json::{Value, json};

use crate::context::RuntimeHost;
use crate::executor::automation::input::input_string_field;

use super::super::git::{git_output, git_output_raw};
use super::super::handoff::reports_failure;
use super::input::{refused, required_workspace};
use super::no_diff;
use super::observe::{observe, require_clean_candidate};

/// Build the typed handoff and record it as this claim's durable settlement.
pub(in crate::executor::automation) fn claim_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = host.claim_execution_context()?;
    let workspace_path = required_workspace(input)?;
    let candidate = observe(&workspace_path, &context, input)?;
    if let HandoffDelivery::NoDiff { evidence } = &candidate.delivery {
        no_diff::verify_leaf(host, &context, &workspace_path, evidence)?;
    }
    let validated: HandoffCandidate = input
        .get("candidate")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| OrbitError::InvalidInput(format!("invalid validated candidate: {error}")))?
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "claim_handoff requires the candidate its validation ran on".to_string(),
            )
        })?;
    if validated != candidate {
        return Err(refused(
            "the worktree moved after validation; the owner accepts only a handoff whose \
             evidence pins the candidate that is still checked out",
        ));
    }
    require_clean_candidate(&workspace_path, &candidate)?;
    let validation: Vec<HandoffArtifactRef> = input
        .get("validation")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            OrbitError::InvalidInput(format!("invalid validation references: {error}"))
        })?
        .unwrap_or_default();
    if validation.is_empty() && !context.required_commands.is_empty() {
        return Err(refused(
            "a typed handoff carries its captured required validation; none was supplied",
        ));
    }
    let execution_summary = handoff_execution_summary(input, &candidate, !validation.is_empty())?;
    let review = handoff_review(input, &candidate)?;

    let handoff = TaskHandoff {
        schema_version: 1,
        workspace_id: context.workspace_id.clone(),
        task_id: context.task_id.clone(),
        claim_id: context.claim_id.clone(),
        machine_id: context.machine_id.clone(),
        run_id: context.run_id.clone(),
        candidate: candidate.clone(),
        review,
        execution_summary,
        validation,
        footprint_widening: super::super::commit::validate_claim_new_paths(
            &workspace_path,
            &context.footprint,
            &candidate.base.commit,
            &candidate.candidate.commit,
        )?
        .1,
    };
    host.record_claim_handoff(&handoff)?;

    Ok(json!({
        "handed_off": true,
        "merged": false,
        "task_id": context.task_id,
        "candidate": candidate.candidate.commit,
        "base": candidate.base.commit,
        "delivery": match candidate.delivery {
            HandoffDelivery::LocalCandidate => "local_candidate",
            HandoffDelivery::PullRequest { .. } => "pull_request",
            HandoffDelivery::AlreadyLanded { .. } => "already_landed",
            HandoffDelivery::NoDiff { .. } => "no_diff",
        },
    }))
}

/// The review disposition the handoff reports. A leaf that ran the before-PR
/// gate passes its evidence as `review_evidence` [ORB-13895]; without it
/// there is no reviewed SHA, verdict or reviewer artifact to report and none
/// is invented. The owner judges the evidence against the review contract
/// the claim captured, so this only refuses evidence for another candidate.
fn handoff_review(
    input: &Value,
    candidate: &HandoffCandidate,
) -> Result<HandoffReview, OrbitError> {
    let Some(evidence) = input
        .get("review_evidence")
        .filter(|value| !value.is_null())
    else {
        return Ok(HandoffReview::not_required());
    };
    let evidence: HandoffReviewEvidence = serde_json::from_value(evidence.clone())
        .map_err(|error| OrbitError::InvalidInput(format!("invalid review evidence: {error}")))?;
    if evidence.reviewed_head_sha != candidate.candidate.commit {
        return Err(refused(format!(
            "the review settled head '{}' but the candidate is '{}'; the owner accepts review \
             evidence only for the candidate handed off",
            evidence.reviewed_head_sha, candidate.candidate.commit
        )));
    }
    Ok(HandoffReview {
        policy: ReviewTiming::BeforePr,
        disposition: HandoffReviewDisposition::BeforePr(Box::new(evidence)),
    })
}

/// Largest implementer summary a handoff carries. The summary is prose for a
/// reader, and the handoff travels to the owner in one coordination call.
const MAX_HANDOFF_SUMMARY_BYTES: usize = 64 * 1024;

/// The `execution_summary` the typed handoff carries to the owner.
///
/// Acceptance writes the handoff's summary over the owner's
/// `execution_summary`, and an implementer in claimed mode writes no owner
/// task state itself (distributed-drain design §3, "Claimed-mode
/// implementation"): it returns its summary in the implement step's output,
/// which the claimed pipelines pass here as `implementation`. In order:
///
/// 1. an explicit `execution_summary` input;
/// 2. `implementation.execution_summary`, else the step's short
///    `implementation.summary`;
/// 3. a generic delivery statement.
///
/// An implementer summary keeps its own words, gains the implementer's
/// `comment` and any `context_files_added` it reported (recorded as prose;
/// typed widening is recomputed from the candidate rather than this output), and ends with a delivery line naming the candidate. One whose
/// first line reports `Outcome: failed` is refused here, as the owner would
/// refuse it, so a failed implementation is never handed off as delivered
/// work.
pub(in crate::executor::automation::vcs) fn handoff_execution_summary(
    input: &Value,
    candidate: &HandoffCandidate,
    validated: bool,
) -> Result<String, OrbitError> {
    let validation = if validated {
        "required validation passed on the exact candidate and the owner holds every captured log"
    } else {
        "no required validation commands are configured, so no check ran"
    };
    let delivered = format!(
        "Claimed execution delivered candidate {} on base {}; {validation}.",
        candidate.candidate.commit, candidate.base.commit
    );
    let implementation = implementation_output(input);
    let Some(summary) = implementer_summary(input) else {
        return Ok(delivered);
    };
    if reports_failure(&summary) {
        return Err(refused(
            "the implementer's execution summary reports `Outcome: failed`; a claimed leaf \
             hands off only delivered work",
        ));
    }
    let mut composed = bounded_summary(&summary);
    if let Some(comment) = text(implementation.and_then(|output| output.get("comment"))) {
        composed.push_str("\n\nImplementer comment:\n");
        composed.push_str(&bounded_summary(&comment));
    }
    let added = implementation
        .and_then(|output| output.get("context_files_added"))
        .and_then(Value::as_array)
        .map(|selectors| {
            selectors
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|selector| !selector.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !added.is_empty() {
        composed.push_str(
            "\n\nContext selectors the implementer reported (advisory; owner-validated widening \
             is derived from Git):",
        );
        for selector in added {
            composed.push_str("\n- ");
            composed.push_str(selector);
        }
    }
    composed.push_str("\n\n");
    composed.push_str(&delivered);
    Ok(composed)
}

/// The implement step's output, when the pipeline passed it as `implementation`.
fn implementation_output(input: &Value) -> Option<&Value> {
    input
        .get("implementation")
        .filter(|value| value.is_object())
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

/// Whether this step was handed this run's implementer output at all: an
/// explicit `execution_summary` or the implement step's `implementation`.
pub(in crate::executor::automation::vcs) fn carries_implementer_output(input: &Value) -> bool {
    input_string_field(input, "execution_summary").is_some()
        || implementation_output(input).is_some()
}

/// This run's implementer summary, trimmed, in the precedence
/// [`handoff_execution_summary`] documents (items 1 and 2). The claimed
/// delivery gate
/// ([`reject_failed_attempt`](super::super::handoff::reject_failed_attempt)) judges
/// the same text the handoff carries [ORB-13755].
pub(in crate::executor::automation::vcs) fn implementer_summary(input: &Value) -> Option<String> {
    let implementation = implementation_output(input);
    input_string_field(input, "execution_summary")
        .or_else(|| text(implementation.and_then(|output| output.get("execution_summary"))))
        .or_else(|| text(implementation.and_then(|output| output.get("summary"))))
}

/// `text`, cut to [`MAX_HANDOFF_SUMMARY_BYTES`] with the cut reported.
fn bounded_summary(text: &str) -> String {
    let text = text.trim();
    if text.len() <= MAX_HANDOFF_SUMMARY_BYTES {
        return text.to_string();
    }
    let cut = floor_char_boundary(text, MAX_HANDOFF_SUMMARY_BYTES);
    format!(
        "{}\n\n[summary truncated to {cut} of {} bytes]",
        &text[..cut],
        text.len()
    )
}

/// The tree a validation result describes is the committed HEAD only while
/// the named branch still points there and no tracked or untracked input
/// differs from it. Ignored build output is intentionally outside this check.
pub(in crate::executor::automation::vcs) fn require_clean_checkout(
    workspace_path: &Path,
    branch: &str,
    commit: &str,
    subject: &str,
) -> Result<(), OrbitError> {
    let observed = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if observed != branch {
        return Err(OrbitError::PolicyDenied(format!(
            "the checked-out source branch moved from '{branch}' to '{observed}'; rerun validation"
        )));
    }
    let head = git_output(workspace_path, &["rev-parse", "HEAD"])?;
    if head != commit {
        return Err(OrbitError::PolicyDenied(format!(
            "the checked-out HEAD moved from validated candidate {commit} to {head}; rerun \
             validation"
        )));
    }
    if !git_output_raw(
        workspace_path,
        &["status", "--porcelain=v1", "--untracked-files=all", "-z"],
    )?
    .is_empty()
    {
        return Err(OrbitError::PolicyDenied(format!(
            "{subject} has staged, tracked, or untracked changes; commit or remove them and \
             rerun validation on the exact candidate"
        )));
    }
    Ok(())
}
