//! Check the reviewer's claims against the repository and the task scope,
//! and render the findings comment and the PR's review-fixes section.

mod comment;
mod construction;
mod repairs;
mod verdict;

pub(super) use comment::{review_fixes_section, verdict_comment, write_artifact};
pub(super) use repairs::repair_author_label;

use orbit_types::workflow::{RetainedObligation, RetiredValidation, ReviewVerdict};

/// The reviewer's claims, checked against the repository and the task scope.
pub(super) struct Judgement {
    pub(super) verdict: ReviewVerdict,
    pub(super) external_evidence: Vec<orbit_types::workflow::ReviewEvidenceRequirement>,
    pub(super) findings: Vec<orbit_types::workflow::ReviewFinding>,
    pub(super) validation: Vec<orbit_types::workflow::ReviewValidation>,
    pub(super) validation_complete: bool,
    pub(super) required_validation_commands: Option<Vec<String>>,
    /// The owner's `review.baseline_commands` the run was admitted under
    /// [ORB-14684]: with the required commands, what settlement may rerun on
    /// the base and what a failed diagnostic may not name.
    pub(super) baseline_commands: Vec<String>,
    /// Required-check records earlier report revisions of this attempt made
    /// that the final report does not repeat verbatim.
    pub(super) retained_obligations: Vec<RetainedObligation>,
    /// Retained record ids the final report retired, with their reasons.
    pub(super) retired_validation: Vec<RetiredValidation>,
    pub(super) escalation: Option<String>,
    summary: String,
    pub(super) task_meaning_digest: String,
    pub(super) selectors_widened: Vec<String>,
    /// [ORB-14450] Set when a requirement was satisfied by evidence on an
    /// earlier tree whose patch the final candidate carries unchanged.
    pub(super) evidence_carried: Option<orbit_types::workflow::ReviewEvidenceCarried>,
    /// [ORB-14434] Set once the host downgraded the review: a review the
    /// host found incomplete is never held for a red base.
    pub(super) host_refused: bool,
    /// [ORB-14478] `host_sandbox_test` requirements this host ran or refused.
    pub(super) host_evidence: Vec<orbit_types::workflow::HostEvidenceRecord>,
    /// [ORB-15122] Failed required checks the host passed on the final
    /// candidate after refuting the reviewer's red-base claim.
    pub(super) host_overrides: Vec<orbit_types::workflow::HostCandidateOverride>,
}
