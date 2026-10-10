//! Trusted owner observations for handoff acceptance.

use serde::{Deserialize, Serialize};

/// Owner observations from Git/provider identity and repository validation policy.
/// Not deserializable: adapters must obtain these independently of handoff JSON.
/// For already-landed delivery, the adapter must run the existing typed evidence,
/// scope, ancestry, delivery-marker and clean-tree checks before constructing this.
/// For NoDiff it must re-run the clean-tree checkpoint verifier against the
/// pinned report and the base the run synchronized onto, which the owner's
/// current base must contain, without trusting an executor branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffObservation {
    /// Owner-observed eligible additions outside the original footprint.
    pub footprint_widening: Vec<String>,
    pub candidate: orbit_types::workflow::handoff::HandoffCandidate,
    pub required_commands: Vec<String>,
    /// The completion authority the owner's configuration grants claimed
    /// handoffs at the moment of this decision, read by trusted owner code
    /// from its own settings. `None` means every handoff waits for an
    /// operator's approval.
    pub owner_completion_authority: Option<String>,
    /// What the owner observed about a before-PR handoff's review evidence.
    /// Required to accept one; `None` for a handoff carrying none.
    pub review: Option<HandoffReviewObservation>,
}

/// The owner's own reading of the facts a before-PR certificate stands on
/// that the claim journal cannot check itself [ORB-13895].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffReviewObservation {
    /// The reviewed base the owner checked; it must be the handoff's.
    pub reviewed_base_sha: String,
    /// Whether that base is the owner-observed candidate base or one of its
    /// ancestors, in the owner's checkout.
    pub reviewed_base_is_ancestor: bool,
    /// The owner's repository identity, which after-landing coverage matches
    /// certificates against.
    pub repository: String,
}

/// Why an owner refused a handoff's review disposition [ORB-13895]. The code
/// leads the refusal message, so a caller reading only the error can tell
/// which check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffReviewRefusal {
    /// The claim captured `review.before_pr` or `review.before_landing` and
    /// the handoff carries no review evidence for that timing.
    ReviewEvidenceMissing,
    /// The claim captured no review and the handoff claims one.
    ReviewEvidenceUnexpected,
    /// The reviewer's verdict does not let the candidate open a PR.
    ReviewNotPassed,
    /// The reviewed head is not the handed-off candidate.
    ReviewedHeadMismatch,
    /// The reviewed base is not the owner's candidate base or an ancestor of it.
    ReviewedBaseNotAncestor,
    /// The certificate artifact is missing, changed, unreadable, or disagrees
    /// with the evidence, the candidate, or the owner's repository.
    ReviewCertificateMismatch,
    /// The reviewer or the certificate's contract is not the one the claim
    /// captured.
    ReviewContractMismatch,
}

impl HandoffReviewRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReviewEvidenceMissing => "review_evidence_missing",
            Self::ReviewEvidenceUnexpected => "review_evidence_unexpected",
            Self::ReviewNotPassed => "review_not_passed",
            Self::ReviewedHeadMismatch => "reviewed_head_mismatch",
            Self::ReviewedBaseNotAncestor => "reviewed_base_not_ancestor",
            Self::ReviewCertificateMismatch => "review_certificate_mismatch",
            Self::ReviewContractMismatch => "review_contract_mismatch",
        }
    }
}
