//! Review admission, budget and timing [ORB-11333].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::ReviewAdmissionError;

/// The reserved run-input key carrying the captured review admission. Like
/// the operation snapshot, only the trusted submission path writes it; a
/// child inherits its parent's snapshot and ordinary input naming it is
/// refused, so a later preference edit cannot weaken an active gate.
pub const REVIEW_ADMISSION_KEY: &str = "review";

/// Version of the review evidence contract. Bump when the manifest,
/// verdict, or certificate shape changes meaning so older evidence is never
/// reinterpreted as coverage.
pub const REVIEW_CONTRACT_VERSION: u32 = 1;

/// Task artifact carrying the pinned reviewer manifest for one attempt.
pub const REVIEW_MANIFEST_ARTIFACT: &str = "review-manifest.json";

/// Task artifact the reviewer writes with its structured report. The gate
/// reads it back with the task store's provenance, never the advisory
/// response envelope alone.
pub const REVIEW_REPORT_ARTIFACT: &str = "review-report.json";

/// Task artifact carrying the settled gate result for the latest attempt.
pub const REVIEW_GATE_ARTIFACT: &str = "review-gate.json";

/// Task artifact holding the host's runs for each baseline claim a review
/// report makes.
pub const REVIEW_BASELINE_ARTIFACT: &str = "review-baseline.json";

/// Whether a canonical task artifact belongs to the reserved review namespace.
/// Callers must first use [`crate::task::canonical_artifact_path`]. Reserving
/// the whole prefix also protects future gate artifacts; ASCII case folding
/// protects owners on case-insensitive filesystems.
pub fn is_reserved_review_artifact(path: &str) -> bool {
    const PREFIX: &str = "review-";
    path.get(..PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
}

/// Default `review.minutes`: reviewer runtime for one candidate's review,
/// its fix commit and final validation included [ORB-13992].
pub const DEFAULT_REVIEW_MINUTES: u32 = 30;

/// Whether a managed delivery holds PR creation for a reviewer. A run
/// captures `before-pr` exactly when `review.before_pr` was on at submission,
/// and `none` otherwise [ORB-13992].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewTiming {
    /// The run delivers without a before-PR review.
    None,
    /// Hold PR creation for a fresh reviewer that fixes what it finds.
    BeforePr,
    /// Captured by runs submitted under the retired
    /// `operation.review_policy = after-landing`; it never gated the run.
    /// After-landing review is the `delivery-code-review` auto-task, which
    /// no run captures, so nothing new records this value.
    AfterLanding,
}

impl ReviewTiming {
    /// The configuration spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewTiming::None => "none",
            ReviewTiming::BeforePr => "before-pr",
            ReviewTiming::AfterLanding => "after-landing",
        }
    }
}

/// The limit captured for one delivery run lineage: the run that first
/// admitted a candidate and every resume of it. Each candidate gets one
/// review [ORB-13992]: once an attempt on a candidate settles with a verdict
/// no further reviewer is started for it, and `minutes` bounds the reviewer
/// runtime that one review may spend across retries and interruptions. A
/// changed candidate, such as a completion rebase, is a new review.
///
/// Budgets captured before [ORB-13992] also carry a `reviewer_starts` limit,
/// and those before [ORB-13989] a `repair_cycles` limit; reading ignores both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReviewBudget {
    /// Reviewer runtime one candidate's review may spend.
    pub minutes: u32,
}

impl Default for ReviewBudget {
    fn default() -> Self {
        Self {
            minutes: DEFAULT_REVIEW_MINUTES,
        }
    }
}

/// The versioned effective review policy a run carries in its immutable
/// input under [`REVIEW_ADMISSION_KEY`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAdmission {
    /// [`REVIEW_CONTRACT_VERSION`] at capture.
    pub contract_version: u32,
    /// The operation policy version the snapshot was resolved from.
    pub policy_version: u32,
    /// Effective review timing.
    pub timing: ReviewTiming,
    /// Which layer decided the timing (`workspace`, `global`, `grant`, ...).
    pub timing_source: String,
    /// The separately configured reviewer crew, when one is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// Which layer decided the crew.
    pub crew_source: String,
    /// Captured review limit.
    pub budget: ReviewBudget,
    /// The workspace owner's required candidate checks captured with this
    /// run. `None` identifies a legacy admission that cannot establish the
    /// host validation contract; an empty list is an explicit no-check
    /// contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_validation_commands: Option<Vec<String>>,
    /// The workspace owner's `review.baseline_commands` captured with this
    /// run [ORB-14684]: checks settlement may rerun on the pinned base, and
    /// whose failure the reviewer cannot file as a `diagnostic`. Absent on
    /// admissions captured before the snapshot: no trusted baseline commands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baseline_commands: Vec<String>,
    /// The workspace's `review.host_evidence` rules: checks a claimed leaf's
    /// host owes for the paths it changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_evidence: Vec<super::super::HostEvidenceRule>,
    /// When the snapshot was captured.
    pub captured_at: DateTime<Utc>,
}

impl ReviewAdmission {
    /// Read the snapshot carried by a run input. A present but malformed
    /// snapshot, or one captured under a contract version this build does
    /// not support, is an error, never silently ignored or reinterpreted.
    pub fn from_run_input(input: &Value) -> Result<Option<Self>, ReviewAdmissionError> {
        let Some(raw) = input.get(REVIEW_ADMISSION_KEY) else {
            return Ok(None);
        };
        if raw.is_null() {
            return Ok(None);
        }
        let admission: Self = serde_json::from_value(raw.clone()).map_err(|error| {
            ReviewAdmissionError::Malformed {
                reason: error.to_string(),
            }
        })?;
        if admission.contract_version != REVIEW_CONTRACT_VERSION {
            return Err(ReviewAdmissionError::UnsupportedContractVersion {
                found: admission.contract_version,
            });
        }
        Ok(Some(admission))
    }

    /// Whether this admission holds PR creation for a reviewer.
    pub fn gates_pr(&self) -> bool {
        self.timing == ReviewTiming::BeforePr
    }
}

/// One commit in a candidate, with the attribution Git recorded for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitIdentity {
    pub commit: String,
    pub tree: String,
    pub author: String,
    pub committer: String,
    pub subject: String,
}
