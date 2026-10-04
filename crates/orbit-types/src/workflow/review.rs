//! Independent review policy contracts [ORB-11333].
//!
//! A managed PR delivery may hold PR creation for a fresh reviewer. The
//! records here are the durable evidence that gate produces: the admission
//! snapshot a run captures, the manifest handed to the reviewer, the honest
//! verdict, the certificate that binds a passed verdict to exact base and
//! candidate trees, the mapping to the commit that actually landed, and the
//! per-lineage budget ledger. None of these is a task status, a human
//! approval, or merge permission; they only describe what was examined.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::automation::SourceRevision;

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

/// Default budgets per delivery run lineage. One reviewer that runs the
/// repository's full validation must fit several times over, so a retried or
/// re-reviewed candidate is not starved by the first invocation.
pub const DEFAULT_REVIEW_REVIEWER_STARTS: u32 = 3;
pub const DEFAULT_REVIEW_REPAIR_CYCLES: u32 = 2;
pub const DEFAULT_REVIEW_MINUTES: u32 = 90;

/// When automatic code review applies to a managed delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewTiming {
    /// No automatic review managed by this policy.
    None,
    /// Hold PR creation for a fresh reviewer and scoped repairs.
    BeforePr,
    /// Accumulate uncovered landed deliveries for a scheduled review.
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

/// Limits captured for one delivery run lineage: the run that first admitted
/// the candidate and every resume of it. They cover retries, interruptions,
/// and candidate invalidations within that lineage; a fresh delivery run
/// starts a new lineage with a fresh budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewBudget {
    /// Fresh reviewer invocations allowed for the lineage.
    pub reviewer_starts: u32,
    /// Repair/validation cycles allowed for the lineage.
    pub repair_cycles: u32,
    /// Aggregate reviewer runtime after which no further start is admitted.
    pub minutes: u32,
}

impl Default for ReviewBudget {
    fn default() -> Self {
        Self {
            reviewer_starts: DEFAULT_REVIEW_REVIEWER_STARTS,
            repair_cycles: DEFAULT_REVIEW_REPAIR_CYCLES,
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
    /// Captured lineage budgets.
    pub budget: ReviewBudget,
    /// When the snapshot was captured.
    pub captured_at: DateTime<Utc>,
}

impl ReviewAdmission {
    /// Read the snapshot carried by a run input. A present but malformed
    /// snapshot, or one captured under a contract version this build does
    /// not support, is an error, never silently ignored or reinterpreted.
    pub fn from_run_input(input: &Value) -> Result<Option<Self>, String> {
        let Some(raw) = input.get(REVIEW_ADMISSION_KEY) else {
            return Ok(None);
        };
        if raw.is_null() {
            return Ok(None);
        }
        let admission: Self = serde_json::from_value(raw.clone())
            .map_err(|error| format!("invalid `{REVIEW_ADMISSION_KEY}` run input: {error}"))?;
        if admission.contract_version != REVIEW_CONTRACT_VERSION {
            return Err(format!(
                "unsupported `{REVIEW_ADMISSION_KEY}` run input: contract_version {} is not the \
                 supported review contract version {REVIEW_CONTRACT_VERSION}",
                admission.contract_version
            ));
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

/// The reviewer's structured decision. Both pass variants require every
/// finding resolved or explicitly disposed and final validation satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// The candidate passed as implemented.
    PassedWithoutRepairs,
    /// The candidate passed after reviewer-authored repairs, which did not
    /// receive an independent second review.
    PassedWithRepairs,
    /// Findings remain that the reviewer could not or may not repair.
    ChangesRequired,
    /// The review could not be completed honestly: missing evidence,
    /// unavailable validation, exhausted budget, or a failed invocation.
    Incomplete,
}

impl ReviewVerdict {
    /// Stable label for projections.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewVerdict::PassedWithoutRepairs => "passed_without_repairs",
            ReviewVerdict::PassedWithRepairs => "passed_with_repairs",
            ReviewVerdict::ChangesRequired => "changes_required",
            ReviewVerdict::Incomplete => "incomplete",
        }
    }

    /// Whether the verdict lets the candidate open a PR.
    pub fn passed(self) -> bool {
        matches!(
            self,
            ReviewVerdict::PassedWithoutRepairs | ReviewVerdict::PassedWithRepairs
        )
    }

    /// The assurance label a passed verdict carries.
    pub fn assurance(self) -> Option<ReviewAssurance> {
        match self {
            ReviewVerdict::PassedWithoutRepairs => Some(ReviewAssurance::IndependentReview),
            ReviewVerdict::PassedWithRepairs => {
                Some(ReviewAssurance::IndependentReviewWithSelfAuthoredRepairs)
            }
            ReviewVerdict::ChangesRequired | ReviewVerdict::Incomplete => None,
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
    /// A negative control that must fail on the candidate: the superseded
    /// assertion, the pre-fix reproduction, the counterfactual. The failure
    /// is the positive evidence, and a pass contradicts the claim.
    ExpectedFailure,
    /// An action outside the authorized scope, deliberately not performed.
    /// It supplies no coverage and imposes no requirement.
    Excluded,
    /// A superseded attempt kept for history: a diagnostic run that a later
    /// required check on the final candidate replaced. Replacement is the
    /// later record that names the same check, not any later required pass.
    /// It never erases the observation and never substitutes for that later
    /// check.
    Superseded,
}

impl ValidationRole {
    /// Stable label for projections and escalation reasons.
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationRole::Required => "required",
            ValidationRole::ExpectedFailure => "expected_failure",
            ValidationRole::Excluded => "excluded",
            ValidationRole::Superseded => "superseded",
        }
    }
}

/// One validation record in the reviewer report or the certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewValidation {
    pub command: String,
    pub outcome: ValidationOutcome,
    /// What the record is evidence of; absent in legacy evidence, which is
    /// then read as a required check.
    #[serde(default)]
    pub role: ValidationRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Identity of the logical check this record belongs to. A superseded
    /// attempt is replaced only by a later required passing record with the
    /// same identity. When omitted, the command string is the identity, so a
    /// same-command rerun still binds. An explicit value lets a corrected
    /// command or environment replace the attempt without quoting the old
    /// command. Empty or whitespace-only values match nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
}

/// How a finding was closed, if at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FindingDisposition {
    /// Still open; blocks a pass verdict.
    Open,
    /// Repaired by the reviewer in this attempt.
    Repaired,
    /// Disposed by an authorized decision with a recorded reason.
    Disposed { reason: String },
}

/// One reviewer finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub id: String,
    pub severity: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub disposition: FindingDisposition,
}

/// The structured report the reviewer persists as
/// [`REVIEW_REPORT_ARTIFACT`]. The gate validates it against the candidate
/// and the repository state; it is a claim, not a certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReport {
    pub schema_version: u32,
    /// Must name the attempt the manifest was issued for.
    pub attempt_id: String,
    pub verdict: ReviewVerdict,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
    #[serde(default)]
    pub validation: Vec<ReviewValidation>,
    /// Why the review stopped when the verdict is not a pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<String>,
}

impl ReviewReport {
    /// Read a persisted report, tolerating benign shape drift that leaves its
    /// meaning unambiguous: a bare-string `disposition`, enum labels in other
    /// case or with `-`/space separators, `pass`/`fail`/`skipped` outcomes,
    /// a single path string, a numeric finding id or string `schema_version`,
    /// a missing `schema_version`, `summary` or finding `severity`, and
    /// `null` lists. Anything else that does not match the contract is
    /// refused with the offending field's path named.
    pub fn parse(content: &[u8]) -> Result<Self, String> {
        let mut value: Value = serde_json::from_slice(content)
            .map_err(|error| format!("review report is not JSON: {error}"))?;
        normalize_report(&mut value);
        serde_json::from_value(value.clone()).map_err(|error| locate_report_error(&value, error))
    }
}

/// Name the first field of a normalized report that fails its type, so a
/// reviewer can fix exactly that field; a missing field already names
/// itself.
fn locate_report_error(report: &Value, error: serde_json::Error) -> String {
    fn check<T: serde::de::DeserializeOwned>(path: &str, value: Option<&Value>) -> Option<String> {
        let value = value.cloned().unwrap_or(Value::Null);
        serde_json::from_value::<T>(value)
            .err()
            .map(|error| format!("{path}: {error}"))
    }
    if let Some(located) = check::<ReviewVerdict>("verdict", report.get("verdict")) {
        return located;
    }
    if let Some(Value::Array(findings)) = report.get("findings") {
        for (index, finding) in findings.iter().enumerate() {
            let path = format!("findings[{index}]");
            let located = check::<FindingDisposition>(
                &format!("{path}.disposition"),
                finding.get("disposition"),
            )
            .or_else(|| check::<ReviewFinding>(&path, Some(finding)));
            if let Some(located) = located {
                return located;
            }
        }
    }
    if let Some(Value::Array(records)) = report.get("validation") {
        for (index, record) in records.iter().enumerate() {
            let path = format!("validation[{index}]");
            let located =
                check::<ValidationOutcome>(&format!("{path}.outcome"), record.get("outcome"))
                    .or_else(|| {
                        record.get("role").and_then(|role| {
                            check::<ValidationRole>(&format!("{path}.role"), Some(role))
                        })
                    })
                    .or_else(|| check::<ReviewValidation>(&path, Some(record)));
            if let Some(located) = located {
                return located;
            }
        }
    }
    error.to_string()
}

fn normalize_report(report: &mut Value) {
    let Some(report) = report.as_object_mut() else {
        return;
    };
    match report.get("schema_version") {
        None | Some(Value::Null) => {
            report.insert(
                "schema_version".to_string(),
                Value::from(REVIEW_CONTRACT_VERSION),
            );
        }
        Some(Value::String(raw)) => {
            if let Ok(version) = raw.trim().parse::<u32>() {
                report.insert("schema_version".to_string(), Value::from(version));
            }
        }
        Some(_) => {}
    }
    normalize_label_field(report, "verdict", &[]);
    if matches!(report.get("summary"), None | Some(Value::Null)) {
        report.insert("summary".to_string(), Value::String(String::new()));
    }
    for list in ["findings", "validation"] {
        if report.get(list).is_some_and(Value::is_null) {
            report.remove(list);
        }
    }
    if let Some(Value::Array(findings)) = report.get_mut("findings") {
        findings
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .for_each(normalize_finding);
    }
    if let Some(Value::Array(records)) = report.get_mut("validation") {
        for record in records.iter_mut().filter_map(Value::as_object_mut) {
            normalize_label_field(
                record,
                "outcome",
                &[
                    ("pass", "passed"),
                    ("success", "passed"),
                    ("fail", "failed"),
                    ("failure", "failed"),
                    ("skipped", "not_run"),
                ],
            );
            if record.get("role").is_some_and(Value::is_null) {
                record.remove("role");
            }
            normalize_label_field(record, "role", &[]);
        }
    }
}

fn normalize_finding(finding: &mut serde_json::Map<String, Value>) {
    if let Some(Value::Number(id)) = finding.get("id") {
        let id = id.to_string();
        finding.insert("id".to_string(), Value::String(id));
    }
    if matches!(finding.get("severity"), None | Some(Value::Null)) {
        finding.insert(
            "severity".to_string(),
            Value::String("unspecified".to_string()),
        );
    }
    if matches!(finding.get("summary"), None | Some(Value::Null))
        && let Some(title) = finding.get("title").cloned()
    {
        finding.insert("summary".to_string(), title);
    }
    match finding.get("paths") {
        Some(Value::Null) => {
            finding.remove("paths");
        }
        Some(Value::String(path)) => {
            let paths = Value::Array(vec![Value::String(path.clone())]);
            finding.insert("paths".to_string(), paths);
        }
        _ => {}
    }
    let disposition = match finding.remove("disposition") {
        Some(Value::String(kind)) => {
            let mut object = serde_json::Map::new();
            object.insert("kind".to_string(), Value::String(kind));
            Some(object)
        }
        Some(Value::Object(mut object)) => {
            if !object.contains_key("kind")
                && let Some(status) = object.remove("status")
            {
                object.insert("kind".to_string(), status);
            }
            Some(object)
        }
        Some(other) => {
            finding.insert("disposition".to_string(), other);
            None
        }
        None => None,
    };
    if let Some(mut disposition) = disposition {
        normalize_label_field(&mut disposition, "kind", &[("fixed", "repaired")]);
        finding.insert("disposition".to_string(), Value::Object(disposition));
    }
}

/// Canonicalize an enum label: trimmed, lower case, `-` and spaces as `_`,
/// then mapped through `aliases`. Non-string values are left for serde to
/// refuse.
fn normalize_label_field(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    aliases: &[(&str, &str)],
) {
    let Some(Value::String(raw)) = object.get(key) else {
        return;
    };
    let label = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let label = aliases
        .iter()
        .find(|(alias, _)| *alias == label)
        .map_or(label.clone(), |(_, canonical)| (*canonical).to_string());
    object.insert(key.to_string(), Value::String(label));
}

/// The pinned, immutable input handed to the reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewManifest {
    pub schema_version: u32,
    pub attempt_id: String,
    pub lineage_key: String,
    pub task_ids: Vec<String>,
    /// Task-meaning digests per task, plus the combined digest the
    /// certificate binds to.
    pub task_digests: BTreeMap<String, String>,
    pub task_meaning_digest: String,
    pub repository: String,
    pub base: SourceRevision,
    pub candidate: SourceRevision,
    pub implementation_commits: Vec<CommitIdentity>,
    /// The implementer's persisted execution summaries, by task.
    pub implementer_summaries: BTreeMap<String, String>,
    pub reviewer_crew: String,
    pub contract_version: u32,
    pub policy_version: u32,
    pub budget: ReviewBudget,
    pub remaining: ReviewConsumption,
    pub issued_at: DateTime<Utc>,
}

/// The reviewer identity actually used for an attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerIdentity {
    pub crew: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// The implementer's resolved model, when known, so a same-model review
    /// is reported as such rather than presented as model diversity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementer_model: Option<String>,
    pub same_model_as_implementer: bool,
}

/// Consumed or remaining lineage limits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewConsumption {
    pub reviewer_starts: u32,
    pub repair_cycles: u32,
    pub seconds: u64,
}

/// The settled gate result for one attempt. A passed certificate is the
/// only thing that can later exclude a delivery from redundant review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCertificate {
    pub schema_version: u32,
    pub attempt_id: String,
    pub lineage_key: String,
    pub task_ids: Vec<String>,
    pub task_meaning_digest: String,
    pub repository: String,
    pub base: SourceRevision,
    /// The implementation the reviewer examined, before any repair.
    pub reviewed_candidate: SourceRevision,
    /// The final candidate the verdict binds to, after repairs.
    pub final_candidate: SourceRevision,
    pub implementation_commits: Vec<CommitIdentity>,
    pub repair_commits: Vec<CommitIdentity>,
    pub verdict: ReviewVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assurance: Option<ReviewAssurance>,
    pub findings: Vec<ReviewFinding>,
    pub validation: Vec<ReviewValidation>,
    /// Whether every validation record passed on the final candidate.
    pub validation_complete: bool,
    pub reviewer: ReviewerIdentity,
    pub consumed: ReviewConsumption,
    pub budget: ReviewBudget,
    /// The reason the gate stopped, for non-pass verdicts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<String>,
    /// Selectors the gate appended because a repaired finding declared an
    /// out-of-scope repair path. Empty when the reviewer already widened
    /// through the task API or no such repair occurred. Absent on certificates
    /// issued before this field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selectors_widened: Vec<String>,
    /// Whether a `changes_required` verdict sent its open findings back to
    /// the implementer for rework within the same delivery run, charging one
    /// repair cycle, instead of stopping delivery [ORB-13891]. Absent on
    /// certificates issued before rework existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rework_requested: bool,
    pub issued_at: DateTime<Utc>,
}

/// How the reviewed candidate became the landed commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingTransformation {
    /// The landed commit is the reviewed candidate.
    FastForward,
    /// One squash commit onto the reviewed base with the same tree.
    Squash,
    /// A merge commit whose result tree equals the reviewed candidate.
    MergeCommit,
    /// The candidate commits were replayed onto the same base tree.
    Rebase,
    /// The mapping could not be verified.
    Unknown,
}

/// The verified relation between a certificate and an actual landing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewLanding {
    pub attempt_id: String,
    pub repository: String,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<String>,
    pub landed: SourceRevision,
    /// The base the landing was applied onto (first parent).
    pub base_at_landing: SourceRevision,
    pub transformation: LandingTransformation,
    /// Whether the certificate still covers the landed content.
    pub covered: bool,
    /// Why coverage did not carry, when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// Why a previously issued gate no longer covers a candidate or landing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewInvalidation {
    VerdictNotPassed,
    ValidationIncomplete,
    TaskMeaningChanged,
    CandidateChanged,
    BaseChanged,
    ObjectsMissing,
    ExternalLandingRace,
    MappingUnknown,
}

impl ReviewInvalidation {
    /// Stable reason label.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewInvalidation::VerdictNotPassed => "verdict_not_passed",
            ReviewInvalidation::ValidationIncomplete => "validation_incomplete",
            ReviewInvalidation::TaskMeaningChanged => "task_meaning_changed",
            ReviewInvalidation::CandidateChanged => "candidate_changed",
            ReviewInvalidation::BaseChanged => "base_changed",
            ReviewInvalidation::ObjectsMissing => "objects_missing",
            ReviewInvalidation::ExternalLandingRace => "external_landing_race",
            ReviewInvalidation::MappingUnknown => "mapping_unknown",
        }
    }
}

/// The state of one reviewer attempt in a lineage ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ReviewAttemptState {
    /// Admitted; the reviewer may be running or the run was interrupted.
    Open,
    /// Settled with a verdict.
    Settled { verdict: ReviewVerdict },
}

/// One reviewer start recorded against a lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAttempt {
    pub attempt_id: String,
    /// One-based index within the lineage.
    pub index: u32,
    pub run_id: String,
    pub task_meaning_digest: String,
    pub candidate: SourceRevision,
    pub started_at: DateTime<Utc>,
    pub state: ReviewAttemptState,
    #[serde(default)]
    pub repair_cycles: u32,
    /// Reviewer runtime charged to the lineage for this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
    /// Set when the attempt was closed without a reviewer verdict — its
    /// reviewer step failed or its run ended first — and settled
    /// `incomplete` with the reviewer runtime spent so far. A resumed run of
    /// the same lineage may still settle it with a verdict; the charge is
    /// then replaced by the attempt's total reviewer runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<DateTime<Utc>>,
    /// Runtime of the reviewer invocations that finished for this attempt,
    /// summed across retries and resumed runs. Retry backoff, recovery
    /// activities and time no reviewer process ran are never part of it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reviewer_seconds: u64,
    /// The reviewer invocation running for this attempt, if one started and
    /// has not reported its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_running: Option<ReviewerInvocation>,
}

/// A reviewer invocation that started for an attempt and has not finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerInvocation {
    /// The run executing the reviewer.
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    /// The invocation's own wall-clock bound; no reviewer process outlives it.
    pub deadline: DateTime<Utc>,
}

/// A reviewer invocation starting or ending for an attempt, as the engine
/// observes it around the reviewer step's dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerInvocationEvent {
    /// The reviewer process is about to start under its own wall-clock bound.
    Started { timeout_seconds: u64 },
    /// The reviewer process ended, successfully or not, after running this
    /// long.
    Finished { runtime_seconds: u64 },
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl ReviewAttempt {
    /// Recorded charge once settled, otherwise the reviewer runtime so far
    /// (see [`Self::reviewer_runtime_at`]).
    pub fn elapsed_at(&self, now: DateTime<Utc>) -> u64 {
        self.elapsed_seconds
            .unwrap_or_else(|| self.reviewer_runtime_at(now))
    }

    /// Reviewer process runtime spent on this attempt, counting a running
    /// invocation up to `bound` — the latest instant it can still have been
    /// running, such as the end of a run that died with it — and never past
    /// its own deadline.
    pub fn reviewer_runtime_at(&self, bound: DateTime<Utc>) -> u64 {
        let running = self.reviewer_running.as_ref().map_or(0, |running| {
            seconds_between(running.started_at, bound.min(running.deadline))
        });
        self.reviewer_seconds.saturating_add(running)
    }

    /// The run that still holds this attempt: the one running its reviewer,
    /// else the admitting run while the attempt is open.
    pub fn holder_run_id(&self) -> Option<&str> {
        match (&self.reviewer_running, &self.state) {
            (Some(running), _) => Some(running.run_id.as_str()),
            (None, ReviewAttemptState::Open) => Some(self.run_id.as_str()),
            (None, ReviewAttemptState::Settled { .. }) => None,
        }
    }
}

/// Whole seconds from `start` to `end`; a clock behind `start` counts as zero.
pub fn seconds_between(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    u64::try_from(end.signed_duration_since(start).num_seconds()).unwrap_or(0)
}

/// An operator decision starting a fresh budget while retaining attempt history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewResetDecision {
    /// All attempts through this index belong to the previous budget.
    pub after_attempt_index: u32,
    pub reason: String,
    pub actor: String,
    pub recorded_at: DateTime<Utc>,
    pub previous_budget: ReviewBudget,
    pub previous_consumption: ReviewConsumption,
    pub budget: ReviewBudget,
}

/// Aggregate review consumption for one delivery candidate lineage. Retry,
/// interruption, candidate invalidation, and delivery lineage share it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewLedger {
    pub lineage_key: String,
    pub task_ids: Vec<String>,
    pub budget: ReviewBudget,
    pub attempts: Vec<ReviewAttempt>,
    pub consumed_seconds: u64,
    /// Audited budget resets; absent in ledgers written before reset support.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<ReviewResetDecision>,
    /// Compare-and-set handle.
    pub revision: u32,
    pub updated_at: DateTime<Utc>,
}

impl ReviewLedger {
    /// A fresh ledger.
    pub fn new(
        lineage_key: &str,
        task_ids: Vec<String>,
        budget: ReviewBudget,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            lineage_key: lineage_key.to_string(),
            task_ids,
            budget,
            attempts: Vec::new(),
            consumed_seconds: 0,
            decisions: Vec::new(),
            revision: 0,
            updated_at: now,
        }
    }

    /// Last attempt retired by an operator reset, or zero for the original budget.
    pub fn reset_through(&self) -> u32 {
        self.decisions
            .last()
            .map_or(0, |decision| decision.after_attempt_index)
    }

    /// What the current budget has consumed so far.
    pub fn consumed(&self) -> ReviewConsumption {
        ReviewConsumption {
            reviewer_starts: u32::try_from(
                self.attempts
                    .iter()
                    .filter(|a| a.index > self.reset_through())
                    .count(),
            )
            .unwrap_or(u32::MAX),
            repair_cycles: self
                .attempts
                .iter()
                .filter(|a| a.index > self.reset_through())
                .map(|a| a.repair_cycles)
                .fold(0, u32::saturating_add),
            seconds: self.consumed_seconds,
        }
    }

    /// What the lineage may still spend after settled consumption. An open
    /// attempt's reviewer runtime is not included; use [`Self::remaining_at`]
    /// for a live leftover.
    pub fn remaining(&self) -> ReviewConsumption {
        Self::remaining_from(self.budget, self.consumed())
    }

    /// Remaining allowance at `now`, counting an open attempt's reviewer
    /// time so a resumed invocation sees leftover seconds rather than the
    /// full captured budget.
    pub fn remaining_at(&self, now: DateTime<Utc>) -> ReviewConsumption {
        let mut consumed = self.consumed();
        if let Some(open) = self.open_attempt() {
            consumed.seconds = consumed.seconds.saturating_add(open.elapsed_at(now));
        }
        Self::remaining_from(self.budget, consumed)
    }

    fn remaining_from(budget: ReviewBudget, consumed: ReviewConsumption) -> ReviewConsumption {
        ReviewConsumption {
            reviewer_starts: budget
                .reviewer_starts
                .saturating_sub(consumed.reviewer_starts),
            repair_cycles: budget.repair_cycles.saturating_sub(consumed.repair_cycles),
            seconds: u64::from(budget.minutes)
                .saturating_mul(60)
                .saturating_sub(consumed.seconds),
        }
    }

    /// The ledger as `attempt_id`'s settlement left it: attempts admitted
    /// later are dropped, so a settlement finished after a restart reports
    /// what the lineage had consumed then. `None` when the attempt is not
    /// part of this lineage.
    pub fn as_of(&self, attempt_id: &str) -> Option<ReviewLedger> {
        let position = self
            .attempts
            .iter()
            .position(|attempt| attempt.attempt_id == attempt_id)?;
        let mut ledger = self.clone();
        ledger.attempts.truncate(position.saturating_add(1));
        let index = ledger.attempts.last()?.index;
        let original_budget = self
            .decisions
            .first()
            .map_or(self.budget, |d| d.previous_budget);
        ledger.decisions.retain(|d| d.after_attempt_index < index);
        ledger.budget = ledger
            .decisions
            .last()
            .map_or(original_budget, |d| d.budget);
        let reset_through = ledger.reset_through();
        // Only settlement charges seconds, so settled consumption is the
        // sum of the kept attempts' recorded elapsed time.
        ledger.consumed_seconds = ledger
            .attempts
            .iter()
            .filter(|attempt| attempt.index > reset_through)
            .filter_map(|attempt| attempt.elapsed_seconds)
            .fold(0, u64::saturating_add);
        Some(ledger)
    }

    /// The ledger while `attempt_id` was still open: [`Self::as_of`] with
    /// the attempt's own settlement undone. A settlement resumed after the
    /// ledger recorded it judges against this view, so its own charge is
    /// not counted against it twice.
    pub fn before_settling(&self, attempt_id: &str) -> Option<ReviewLedger> {
        let mut ledger = self.as_of(attempt_id)?;
        let attempt = ledger.attempts.last_mut()?;
        let charged = attempt.elapsed_seconds.take().unwrap_or(0);
        attempt.state = ReviewAttemptState::Open;
        attempt.released_at = None;
        attempt.repair_cycles = 0;
        ledger.consumed_seconds = ledger.consumed_seconds.saturating_sub(charged);
        Some(ledger)
    }

    /// The run that still holds an attempt of this lineage, if any.
    pub fn holder_run_id(&self) -> Option<&str> {
        self.attempts.iter().find_map(ReviewAttempt::holder_run_id)
    }

    /// The still-open attempt, if any.
    pub fn open_attempt(&self) -> Option<&ReviewAttempt> {
        self.attempts
            .iter()
            .find(|attempt| attempt.state == ReviewAttemptState::Open)
    }
}

/// The outcome of asking the ledger for a reviewer start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReviewReservation {
    /// A new reviewer start was reserved.
    Reserved { attempt: ReviewAttempt },
    /// An open attempt for the same candidate and task meaning is resumed
    /// after an interruption; no new start is consumed.
    Resumed { attempt: ReviewAttempt },
    /// The lineage budget is spent; the caller must escalate.
    Exhausted {
        /// `review_starts_exhausted` or `review_minutes_exhausted`.
        reason: &'static str,
        consumed: ReviewConsumption,
    },
}
