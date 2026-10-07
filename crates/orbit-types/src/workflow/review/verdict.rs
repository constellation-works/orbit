//! Review verdicts and validation records [ORB-11333].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The reviewer's structured decision [ORB-13989]. Both accepting variants
/// require every finding fixed or explicitly disposed and final validation
/// satisfied.
///
/// Evidence written before [ORB-13989] spells the same decisions
/// `passed_without_repairs`, `passed_with_repairs` and `changes_required`;
/// those labels still read, so stored ledgers and certificates stay readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// No findings: the candidate is accepted as implemented.
    #[serde(alias = "passed_without_repairs")]
    Accept,
    /// The reviewer fixed every finding in its own commit on the candidate.
    /// Its fixes were validated but did not receive a second review.
    #[serde(alias = "passed_with_repairs")]
    AcceptWithFixes,
    /// Findings remain that cannot be fixed in review: a wrong approach, a
    /// scope or criteria mismatch, a safety issue, or fixes that fail
    /// validation.
    #[serde(alias = "changes_required")]
    Reject,
    /// The review could not be completed honestly: missing evidence,
    /// unavailable validation, exhausted budget, or a failed invocation.
    Incomplete,
}

impl ReviewVerdict {
    /// Stable label for projections.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewVerdict::Accept => "accept",
            ReviewVerdict::AcceptWithFixes => "accept_with_fixes",
            ReviewVerdict::Reject => "reject",
            ReviewVerdict::Incomplete => "incomplete",
        }
    }

    /// Whether the verdict lets the candidate open a PR.
    pub fn passed(self) -> bool {
        matches!(self, ReviewVerdict::Accept | ReviewVerdict::AcceptWithFixes)
    }

    /// The assurance label a passed verdict carries.
    pub fn assurance(self) -> Option<ReviewAssurance> {
        match self {
            ReviewVerdict::Accept => Some(ReviewAssurance::IndependentReview),
            ReviewVerdict::AcceptWithFixes => {
                Some(ReviewAssurance::IndependentReviewWithSelfAuthoredRepairs)
            }
            ReviewVerdict::Reject | ReviewVerdict::Incomplete => None,
        }
    }
}

/// What a passed verdict actually assures. Both labels qualify for
/// automatic patch-review exclusion; they never claim the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAssurance {
    /// The delivered content was inspected by a fresh reviewer.
    IndependentReview,
    /// The implementation was inspected by a fresh reviewer; the reviewer's
    /// own repairs were validated but not independently reviewed.
    IndependentReviewWithSelfAuthoredRepairs,
}

impl ReviewAssurance {
    /// Stable label for projections.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewAssurance::IndependentReview => "independent_review",
            ReviewAssurance::IndependentReviewWithSelfAuthoredRepairs => {
                "independent_review_with_self_authored_repairs"
            }
        }
    }
}

/// Outcome of one validation command the reviewer ran or could not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationOutcome {
    Passed,
    Failed,
    /// The runner refused the command; this is unavailable validation, not
    /// a defect and not a pass.
    Denied,
    NotRun,
}

impl ValidationOutcome {
    /// Stable label for projections and escalation reasons.
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationOutcome::Passed => "passed",
            ValidationOutcome::Failed => "failed",
            ValidationOutcome::Denied => "denied",
            ValidationOutcome::NotRun => "not_run",
        }
    }
}

/// What a recorded validation command is evidence of.
///
/// An outcome alone cannot say whether a failure was a defect or the point
/// of the check, and an honest `not_run` entry for an action nobody
/// authorized must not become a requirement merely by being listed. The
/// reviewer therefore classifies each record and settlement judges the
/// outcome against that claim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationRole {
    /// A check the final candidate must pass. Records written before this
    /// classification existed carry no role and are read as required, so
    /// older evidence keeps its conservative meaning.
    #[default]
    Required,
    /// A deliberate negative control that must fail: the superseded
    /// assertion, the pre-fix reproduction, the counterfactual. The failure
    /// is the positive evidence, and a pass contradicts the claim. The record
    /// names its [`NegativeControl`] kind and the in-scope `sources` it
    /// exercises; an unrelated failure is a [`Self::Diagnostic`], never a
    /// control.
    ExpectedFailure,
    /// An action outside the authorized scope, deliberately not performed.
    /// It supplies no coverage and imposes no requirement.
    Excluded,
    /// A superseded attempt kept for history: a diagnostic run that a required
    /// passing check on the final candidate replaced. Replacement requires
    /// the same effective identity, regardless of report order. It never
    /// erases the observation or substitutes for the required check.
    Superseded,
    /// A nonrequired observation of the final candidate kept as observed,
    /// such as a workspace-wide suite whose failures lie outside the task's
    /// scope. It supplies no coverage and imposes no requirement. A failed
    /// diagnostic names the `sources` of its failures, every one outside the
    /// candidate's scope; it never stands in for a check the task requires.
    Diagnostic,
}

impl ValidationRole {
    /// Stable label for projections and escalation reasons.
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationRole::Required => "required",
            ValidationRole::ExpectedFailure => "expected_failure",
            ValidationRole::Excluded => "excluded",
            ValidationRole::Superseded => "superseded",
            ValidationRole::Diagnostic => "diagnostic",
        }
    }
}

/// What kind of deliberate negative control an `expected_failure` record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NegativeControl {
    /// The reproduction run on the pre-fix tree: the base, or the candidate
    /// with the fix reverted. The same check may pass on the candidate.
    PreFix,
    /// An assertion the change deliberately retires, run on the candidate.
    SupersededAssertion,
    /// A deliberately broken input or mutation the candidate's checks must
    /// reject, run on the candidate.
    Counterfactual,
}

impl NegativeControl {
    /// Stable label for projections and escalation reasons.
    pub fn as_str(self) -> &'static str {
        match self {
            NegativeControl::PreFix => "pre_fix",
            NegativeControl::SupersededAssertion => "superseded_assertion",
            NegativeControl::Counterfactual => "counterfactual",
        }
    }

    /// Whether the control runs on the final candidate itself, so the same
    /// check cannot also pass there.
    pub fn runs_on_candidate(self) -> bool {
        !matches!(self, NegativeControl::PreFix)
    }
}

/// One validation record in the reviewer report or the certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewValidation {
    /// Stable id of a required-check record across the attempt's report
    /// revisions [ORB-14370], such as `V1`. A later revision carries it
    /// forward with the record's current command and outcome, or retires it
    /// in [`ReviewReport::retired_validation`]; earlier records are matched
    /// by this id, never by command text. A superseded attempt and its
    /// replacing required pass may share it. Absent in evidence written
    /// before it existed, which keeps the command-identity rules.
    ///
    /// [`ReviewReport::retired_validation`]: super::ReviewReport::retired_validation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub command: String,
    pub outcome: ValidationOutcome,
    /// What the record is evidence of; absent in legacy evidence, which is
    /// then read as a required check.
    #[serde(default)]
    pub role: ValidationRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Identity of the logical check this record belongs to. A superseded
    /// attempt is replaced only by a required passing record anywhere in the
    /// report with the same effective identity. A non-empty, trimmed value takes precedence
    /// over the command; otherwise the normalized command is the identity
    /// (whitespace collapsed and leading environment assignments removed).
    /// An explicit identity can match another record's normalized command,
    /// letting a wrapped or corrected command replace the attempt. Empty or
    /// whitespace-only values supply no identity, so the command is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// The kind of negative control an `expected_failure` record is; absent
    /// for every other role and in evidence written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<NegativeControl>,
    /// Repository-relative paths (or `file:`/`dir:` selectors) the outcome is
    /// about: the code a negative control exercises, or where a diagnostic's
    /// failures lie. Settlement and coverage judge them against the
    /// candidate's scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

/// A required-check record an earlier revision of an attempt's report made,
/// kept so a replacement report cannot silently drop the obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedObligation {
    /// SHA-256 of the report revision that recorded it.
    pub report_sha256: String,
    /// When the host accepted that revision.
    pub observed_at: DateTime<Utc>,
    pub validation: ReviewValidation,
}
