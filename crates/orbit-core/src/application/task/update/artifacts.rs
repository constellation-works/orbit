use orbit_common::OrbitError;
use orbit_types::task::{
    EXPLORE_BRIEF_ARTIFACT, MUTATION_PROPOSAL_ARTIFACT_DIR, MUTATION_RECEIPT_ARTIFACT_DIR, Task,
    TaskArtifact, canonical_artifact_path, is_explore_brief_artifact,
    is_mutation_proposal_artifact, is_mutation_receipt_artifact,
};
use orbit_types::workflow::{REVIEW_REPORT_ARTIFACT, ReviewReport, is_reserved_review_artifact};

use crate::OrbitRuntime;

/// Admit only review decisions understood by the merge gate. Keep the caller's
/// case and alias, trimming only the surrounding whitespace; empty clears.
pub(super) fn validate_pr_status(value: &str) -> Result<Option<String>, OrbitError> {
    let value = value.trim();
    match orbit_engine::normalize_review_decision(value).as_str() {
        "" => Ok(None),
        "APPROVED" | "CHANGES_REQUESTED" => Ok(Some(value.to_string())),
        _ => Err(OrbitError::InvalidInput(format!(
            "unknown PR status '{value}': expected `approve` or `request-changes` (empty string clears)"
        ))),
    }
}

/// Admit an artifact a caller other than Orbit's system writer supplies for
/// an owner task it can read: [`admit_agent_artifact`], except that a
/// `mutation-proposals/` path is admitted as a validated proposal for one of
/// `task`'s granted tools [granted-tools §4].
pub(crate) fn admit_task_artifact(
    runtime: &OrbitRuntime,
    task: &Task,
    artifact: &mut TaskArtifact,
) -> Result<(), OrbitError> {
    artifact.path = canonical_artifact_path(&artifact.path)?;
    if is_mutation_proposal_artifact(&artifact.path) {
        return runtime.admit_mutation_proposal(task, artifact);
    }
    admit_agent_artifact(artifact)
}

/// Canonicalize an artifact a caller other than Orbit's system writer
/// supplies, and refuse it in a reserved name: the review namespace, the
/// task-pilot's explore brief and mutation application receipts. Gate records
/// are certificates and authority, the brief is the pilot's validated
/// product and a receipt suppresses re-application through its key, not
/// agent claims, so every agent- or operator-originated write applies this
/// one rule: task updates, task starts and a claimed worker's claim evidence,
/// which a remote owner's broker also reaches. The only exception is the live
/// reviewer's schema-checked report; the store retains its revisions and
/// settlement certifies it. A mutation proposal is refused here too: only
/// [`admit_task_artifact`], on an owner task update, can validate one against
/// the task's grant.
pub(crate) fn admit_agent_artifact(artifact: &mut TaskArtifact) -> Result<(), OrbitError> {
    artifact.path = canonical_artifact_path(&artifact.path)?;
    if is_explore_brief_artifact(&artifact.path) {
        return Err(OrbitError::InvalidInput(format!(
            "{} is reserved for the task-pilot's system writer: only an applied pilot assessment writes {EXPLORE_BRIEF_ARTIFACT}; rename ordinary supporting evidence to a nonreserved name",
            artifact.path
        )));
    }
    if is_mutation_receipt_artifact(&artifact.path) {
        return Err(OrbitError::InvalidInput(format!(
            "{} is reserved for the pipeline's system writer: only applying a reviewed mutation proposal writes `{MUTATION_RECEIPT_ARTIFACT_DIR}/` receipts",
            artifact.path
        )));
    }
    if is_mutation_proposal_artifact(&artifact.path) {
        return Err(OrbitError::InvalidInput(format!(
            "{} is a mutation proposal: `{MUTATION_PROPOSAL_ARTIFACT_DIR}/` artifacts are admitted only through an owner task update, never as claimed-worker evidence",
            artifact.path
        )));
    }
    if !is_reserved_review_artifact(&artifact.path) {
        return Ok(());
    }
    if artifact.path != REVIEW_REPORT_ARTIFACT {
        return Err(OrbitError::InvalidInput(format!(
            "{} is reserved for the review gate's system writer: the entire `review-` prefix (any ASCII case) is reserved; rename ordinary supporting evidence to a nonreserved name (for example `examination-checks.json`)",
            artifact.path
        )));
    }
    ReviewReport::parse_attachment(&artifact.content).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "{REVIEW_REPORT_ARTIFACT} does not match the review report contract: {error}"
        ))
    })?;
    Ok(())
}
