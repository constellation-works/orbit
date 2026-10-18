//! Review admission, budget and timing [ORB-11333].

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::{HostGateResult, ReviewConsumption};
use crate::workflow::{ReviewAdmissionError, ReviewEvidenceRequirement};

/// The reserved run-input key carrying the captured review admission. Like
/// the operation snapshot, only the trusted submission path writes it; a
/// child inherits its parent's snapshot and ordinary input naming it is
/// refused, so a later preference edit cannot weaken an active gate.
pub const REVIEW_ADMISSION_KEY: &str = "review";

/// The run-input key carrying the review layer an operator asked one run to
/// add with `--review` [ORB-16572]. Unlike [`REVIEW_ADMISSION_KEY`] it is a
/// request, not a decision: submission resolves it against workspace policy,
/// and it can only add a layer the policy does not apply.
pub const REVIEW_REQUEST_KEY: &str = "review_request";

/// [`ReviewAdmission::timing_source`] when a run's `--review` option, not
/// configuration, decided its timing.
pub const REVIEW_RUN_OPTION_SOURCE: &str = "run-option";

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

/// When a managed delivery's reviewer runs. A run captures `before-pr`
/// exactly when `review.before_pr` was on at submission, `before-landing`
/// exactly when `review.before_landing` was [ORB-14849], and `none`
/// otherwise [ORB-13992]. Configuration never turns both on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewTiming {
    /// The run delivers without a before-PR review.
    None,
    /// Hold PR creation for a fresh reviewer that fixes what it finds.
    BeforePr,
    /// Open the PR, then have a fresh reviewer review and fix it while
    /// hosted CI runs; the PR lands only at the head that review settled.
    BeforeLanding,
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
            ReviewTiming::BeforeLanding => "before-landing",
            ReviewTiming::AfterLanding => "after-landing",
        }
    }
}

/// A review layer before landing that one run may add with `--review`
/// [ORB-16572]. It strengthens workspace policy only: the layer the policy
/// already applies is a no-op, and the other layer is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum ReviewLayer {
    /// Hold PR creation for a reviewer, as `review.before_pr` does.
    BeforePr,
    /// Review the open PR before it lands, as `review.before_landing` does.
    BeforeLanding,
}

impl ReviewLayer {
    /// The option spelling.
    pub fn as_str(self) -> &'static str {
        self.timing().as_str()
    }

    /// The timing this layer captures.
    pub fn timing(self) -> ReviewTiming {
        match self {
            ReviewLayer::BeforePr => ReviewTiming::BeforePr,
            ReviewLayer::BeforeLanding => ReviewTiming::BeforeLanding,
        }
    }

    /// The layer a captured timing is, if it is one before landing.
    pub fn from_timing(timing: ReviewTiming) -> Option<Self> {
        match timing {
            ReviewTiming::BeforePr => Some(ReviewLayer::BeforePr),
            ReviewTiming::BeforeLanding => Some(ReviewLayer::BeforeLanding),
            ReviewTiming::None | ReviewTiming::AfterLanding => None,
        }
    }

    /// The workspace configuration key that applies this layer.
    pub fn config_key(self) -> &'static str {
        match self {
            ReviewLayer::BeforePr => "review.before_pr",
            ReviewLayer::BeforeLanding => "review.before_landing",
        }
    }

    /// Read the layer a run input requests under [`REVIEW_REQUEST_KEY`]. A
    /// present value that names no layer is an error, never ignored.
    pub fn from_run_input(input: &Value) -> Result<Option<Self>, ReviewAdmissionError> {
        match input.get(REVIEW_REQUEST_KEY) {
            None | Some(Value::Null) => Ok(None),
            Some(raw) => serde_json::from_value(raw.clone()).map(Some).map_err(|_| {
                ReviewAdmissionError::MalformedRequest {
                    found: raw.to_string(),
                }
            }),
        }
    }
}

impl std::str::FromStr for ReviewLayer {
    type Err = &'static str;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "before-pr" => Ok(ReviewLayer::BeforePr),
            "before-landing" => Ok(ReviewLayer::BeforeLanding),
            _ => Err("review must be before-pr or before-landing"),
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

/// One weighted member of a captured reviewer pool [ORB-15195].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCrewPoolMember {
    /// Canonical crew name.
    pub name: String,
    /// Relative tickets this crew holds in the reviewer draw.
    pub weight: u32,
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
    /// The configured reviewer pool when `operation.review_crew` names more
    /// than one crew [ORB-15195]; `crew` is then unset and each review
    /// chooses one member when it is admitted. Empty for a single crew, so
    /// such an admission is captured exactly as before pools existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crew_pool: Vec<ReviewCrewPoolMember>,
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

    /// Whether this admission reviews the open PR before it lands.
    pub fn gates_landing(&self) -> bool {
        self.timing == ReviewTiming::BeforeLanding
    }

    /// Whether a reviewer must settle the head this run merges: either
    /// review layer before landing.
    pub fn gates_merge(&self) -> bool {
        self.gates_pr() || self.gates_landing()
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

/// What a review gate's admission step (`review_gate_admit` and its
/// landing and re-review siblings) returns. The pipeline checkpoints it as
/// the step's output in run state, job templates read its fields, and the
/// settlement step receives it whole as `admission`, so its serde shape is
/// a persisted contract: every case carries a boolean `applies`, and the
/// admitted case keeps its field order and explicit `null`s.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewGateAdmissionOutput {
    /// A reviewer was admitted, resumed or, for a held review whose owed
    /// evidence arrived, skipped, on a pinned candidate.
    Admitted(Box<ReviewGateAdmitted>),
    /// The budget preflight found the lineage able to review; nothing was
    /// reserved.
    PreflightPassed {
        /// The lineage the preflight checked.
        lineage_key: String,
    },
    /// The gate does not apply, and the pipeline continues without it.
    NotApplicable(ReviewGateNotApplicable),
}

impl ReviewGateAdmissionOutput {
    /// Parse a recorded admission output.
    pub fn from_value(value: &Value) -> Result<Self, serde_json::Error> {
        Self::deserialize(value)
    }

    /// The admitted attempt, when the gate admitted one.
    pub fn admitted(&self) -> Option<&ReviewGateAdmitted> {
        match self {
            Self::Admitted(admitted) => Some(admitted),
            Self::PreflightPassed { .. } | Self::NotApplicable(_) => None,
        }
    }

    /// Whether the gate applies to the run.
    pub fn applies(&self) -> bool {
        !matches!(self, Self::NotApplicable(_))
    }

    /// The recorded `decision`; a gate that does not apply records none.
    pub fn decision(&self) -> Option<&'static str> {
        match self {
            Self::Admitted(admitted) => Some(admitted.decision.as_str()),
            Self::PreflightPassed { .. } => Some(PREFLIGHT_PASSED_DECISION),
            Self::NotApplicable(_) => None,
        }
    }
}

/// The `decision` a passed budget preflight records.
const PREFLIGHT_PASSED_DECISION: &str = "preflight_passed";

/// How an admitted review gate proceeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewGateDecision {
    /// A fresh reviewer attempt was reserved.
    Admitted,
    /// The lineage's open attempt on this candidate resumed.
    Resumed,
    /// A held review's owed evidence arrived for this exact candidate, so it
    /// settles under its original reviewer without starting one; job
    /// templates skip the reviewer step on it.
    EvidenceReceived,
}

impl ReviewGateDecision {
    /// The recorded spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Resumed => "resumed",
            Self::EvidenceReceived => "evidence_received",
        }
    }
}

/// An admitted review gate: the pinned candidate, the reviewer and the
/// attempt the settlement step must close. Field order is the recorded
/// order. Fields added after the gate first shipped default when absent, so
/// run state recorded before them still resumes and settles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewGateAdmitted {
    /// How the gate proceeds.
    pub decision: ReviewGateDecision,
    /// The held attempt a [`ReviewGateDecision::EvidenceReceived`] admission
    /// settles; `null` otherwise.
    #[serde(default)]
    pub held_attempt_id: Option<String>,
    /// The first reviewed task.
    pub first_task_id: String,
    /// The reserved attempt.
    pub attempt_id: String,
    /// The attempt's one-based index within its lineage.
    pub attempt_index: u32,
    /// The ledger lineage the attempt was reserved in.
    pub lineage_key: String,
    /// The run's captured review timing.
    pub timing: ReviewTiming,
    /// Which layer decided the timing.
    pub timing_source: String,
    /// The admitted reviewer.
    pub reviewer: ReviewGateReviewer,
    /// External checks the candidate owes the reviewer.
    #[serde(default)]
    pub owed_evidence: Vec<ReviewEvidenceRequirement>,
    /// External checks owed to the owner and the pull request's hosted
    /// checks instead.
    #[serde(default)]
    pub owner_owed_evidence: Vec<ReviewEvidenceRequirement>,
    /// The host's runs of the owner's required gates on the candidate.
    #[serde(default)]
    pub host_validation: Vec<HostGateResult>,
    /// The pinned base commit.
    pub base_sha: String,
    /// The pinned base tree.
    pub base_tree: String,
    /// The admitted candidate head.
    pub head_sha: String,
    /// The admitted candidate tree.
    pub head_tree: String,
    /// Implementation commits the candidate adds over the base.
    pub implementation_commit_count: usize,
    /// The combined task-meaning digest the attempt was reserved under.
    pub task_meaning_digest: String,
    /// Each reviewed task's `context_files` at admission.
    pub task_selectors: BTreeMap<String, Vec<String>>,
    /// Whether external evidence carried onto this candidate; recorded as
    /// the evidence-carry outcome and `null` when nothing was compared.
    #[serde(default)]
    pub evidence_carry: Value,
    /// The task artifact holding the reviewer manifest.
    pub manifest_artifact: String,
    /// The task artifact the reviewer writes its report to.
    pub report_artifact: String,
    /// The lineage's captured review limit.
    pub budget: ReviewBudget,
    /// Reviewer runtime the candidate's review has left.
    pub remaining: ReviewConsumption,
    /// When the attempt started.
    #[serde(serialize_with = "serialize_rfc3339")]
    pub started_at: DateTime<Utc>,
}

/// The reviewer an admitted gate names. A held review names the reviewer
/// that reviewed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewGateReviewer {
    /// Canonical crew name.
    pub crew: String,
    /// Where the crew came from (`held_review` for a held review).
    pub crew_source: String,
    /// The crew's provider.
    pub provider: String,
    /// The crew's model.
    pub model: String,
    /// The crew's reasoning effort; `null` when it sets none.
    pub reasoning_effort: Option<String>,
}

impl ReviewGateReviewer {
    /// The reviewer as `provider / model`, which authors its repair commits;
    /// `None` when either is blank.
    pub fn model_label(&self) -> Option<String> {
        let provider = self.provider.trim();
        let model = self.model.trim();
        (!provider.is_empty() && !model.is_empty()).then(|| format!("{provider} / {model}"))
    }
}

/// Why a review gate does not apply to a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewGateNotApplicable {
    /// The reason code, such as `review_before_pr_off`. Older binaries
    /// recorded codes this one no longer produces, so it stays open text.
    #[serde(default)]
    pub reason: String,
    /// The run's captured timing; `null` when no admission was captured.
    #[serde(default)]
    pub timing: Option<ReviewTiming>,
    /// Which layer decided the timing; `null` with it.
    #[serde(default)]
    pub timing_source: Option<String>,
}

impl ReviewGateNotApplicable {
    /// Not applicable for `reason`, under the run's captured `admission`.
    pub fn new(reason: &str, admission: Option<&ReviewAdmission>) -> Self {
        Self {
            reason: reason.to_string(),
            timing: admission.map(|admission| admission.timing),
            timing_source: admission.map(|admission| admission.timing_source.clone()),
        }
    }
}

/// `started_at` as the gate first recorded it, `+00:00` offset included.
fn serialize_rfc3339<S: Serializer>(at: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&at.to_rfc3339())
}

impl Serialize for ReviewGateAdmissionOutput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        /// `applies` first, then the case's own fields in order.
        #[derive(Serialize)]
        struct Tagged<'a, T> {
            applies: bool,
            #[serde(flatten)]
            body: &'a T,
        }
        #[derive(Serialize)]
        struct Preflight<'a> {
            decision: &'static str,
            lineage_key: &'a str,
        }
        match self {
            Self::Admitted(admitted) => Tagged {
                applies: true,
                body: admitted.as_ref(),
            }
            .serialize(serializer),
            Self::PreflightPassed { lineage_key } => Tagged {
                applies: true,
                body: &Preflight {
                    decision: PREFLIGHT_PASSED_DECISION,
                    lineage_key,
                },
            }
            .serialize(serializer),
            Self::NotApplicable(not_applicable) => Tagged {
                applies: false,
                body: not_applicable,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ReviewGateAdmissionOutput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// The fields that choose the case.
        #[derive(Deserialize)]
        struct Head {
            applies: bool,
            #[serde(default)]
            decision: Option<HeadDecision>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum HeadDecision {
            PreflightPassed,
            #[serde(other)]
            Review,
        }
        #[derive(Deserialize)]
        struct Preflight {
            lineage_key: String,
        }
        let value = Value::deserialize(deserializer)?;
        let head = Head::deserialize(&value).map_err(D::Error::custom)?;
        let output = match (head.applies, head.decision) {
            (false, _) => Self::NotApplicable(
                ReviewGateNotApplicable::deserialize(&value).map_err(D::Error::custom)?,
            ),
            (true, Some(HeadDecision::PreflightPassed)) => Self::PreflightPassed {
                lineage_key: Preflight::deserialize(&value)
                    .map_err(D::Error::custom)?
                    .lineage_key,
            },
            (true, _) => Self::Admitted(Box::new(
                ReviewGateAdmitted::deserialize(&value).map_err(D::Error::custom)?,
            )),
        };
        Ok(output)
    }
}
