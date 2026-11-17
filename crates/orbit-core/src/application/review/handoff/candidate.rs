//! The candidate identity an operator surface expects to act on.

use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_types::workflow::handoff::HandoffCandidate;

/// The candidate identity an operator surface believed it was acting on.
///
/// Deliberately narrower than [`HandoffCandidate`]: a handoff is immutable, so
/// the commit pair is enough to prove the caller and the owner are looking at
/// the same accepted record, and a browser never has to round-trip a
/// `deny_unknown_fields` struct it did not author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedCandidate {
    pub candidate_commit: String,
    pub base_commit: String,
}

impl ExpectedCandidate {
    pub(super) fn check(&self, candidate: &HandoffCandidate) -> Result<(), OrbitError> {
        if self.candidate_commit == candidate.candidate.commit
            && self.base_commit == candidate.base.commit
        {
            return Ok(());
        }
        Err(OrbitError::ClaimRefused {
            kind: ClaimRefusalKind::StaleClaim,
            message: format!(
                "this action was prepared against candidate {} on base {}; the owner now holds \
                 candidate {} on base {} (stale_claim)",
                self.candidate_commit,
                self.base_commit,
                candidate.candidate.commit,
                candidate.base.commit,
            ),
        })
    }
}
