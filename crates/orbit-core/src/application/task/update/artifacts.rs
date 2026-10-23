//! Admission of agent attachments at the task write boundary.

use orbit_common::OrbitError;
use orbit_types::task::{TaskArtifact, canonical_artifact_path};
use orbit_types::workflow::{REVIEW_REPORT_ARTIFACT, ReviewReport, is_reserved_review_artifact};

/// Canonicalize an artifact a caller other than the review gate's system
/// writer supplies, and refuse it in the reserved review namespace. Gate
/// records are certificates and authority, not agent claims, so every
/// agent-originated write applies this one rule: task updates, task starts and
/// a claimed worker's claim evidence, which a remote owner's broker also
/// reaches. The only exception is the live reviewer's schema-checked report;
/// the store retains its revisions and settlement certifies it.
pub(crate) fn admit_agent_artifact(artifact: &mut TaskArtifact) -> Result<(), OrbitError> {
    artifact.path = canonical_artifact_path(&artifact.path)?;
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
