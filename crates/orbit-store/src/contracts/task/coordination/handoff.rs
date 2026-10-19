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
    /// [ORB-15427] The owner's own judgement of each validation log the
    /// handoff reports as known red. Acceptance requires one per such log;
    /// approval and landing recheck the logs the accepted handoff pinned.
    pub known_red: Vec<KnownRedObservation>,
    /// [ORB-16855] The owner's own judgement of the candidate for each
    /// light-delivery tag its copy of the task carries, against its
    /// `workflow.light_delivery_paths`. An applying `skip-review` decision
    /// admits a handoff without review evidence, an applying
    /// `skip-validation` one admits it without validation logs, while the
    /// owner's task still carries the tag; acceptance records each decision
    /// in the task's history. Empty for a task without either tag.
    pub light_delivery: Vec<orbit_types::task::LightDeliveryDecision>,
}

/// What the owner's own run of a required command on the handed-off base
/// showed about a leaf's known-red report [ORB-15427]. Built by trusted owner
/// code that ran, or read its cached run of, an owner-required command; the
/// leaf's report only names what to run. When the follower was admitted on
/// another host OS, the owner does not run the base: [`Self::accepted_across_os`]
/// names that scope and [`Self::verdict`] carries the follower's reported
/// failures [ORB-16432].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownRedObservation {
    /// The owner-required command the log reports.
    pub command: String,
    /// The base commit the report names. The owner ran it there only when
    /// [`Self::accepted_across_os`] is absent.
    pub base_sha: String,
    /// The reported failures when they stand, or why the owner could not
    /// establish that. A cross-OS acceptance carries the follower's failures
    /// without an owner base run.
    pub verdict: Result<std::collections::BTreeSet<String>, String>,
    /// Set when the follower's admitted host OS differs from the owner's, so
    /// the owner's base run is a different validation environment and was not
    /// used. The first OS is the follower's; the second is the owner's.
    pub accepted_across_os: Option<KnownRedOsAcceptance>,
}

/// The two host operating systems of a known-red report the owner accepted
/// without running the base, because they differ [ORB-16432].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownRedOsAcceptance {
    /// The OS the follower was admitted on, where it observed the base failures.
    pub follower: orbit_types::task::HostOs,
    /// The OS this owner runs. Its base run neither confirms nor contradicts.
    pub owner: orbit_types::task::HostOs,
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
