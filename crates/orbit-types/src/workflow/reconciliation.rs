//! Review reconciliation of an already-merged foreign delivery head.
//!
//! A recovered follower delivery whose pull request merged at a head other
//! than the handed-off candidate has no review or validation for that head:
//! the candidate's evidence never carries to a changed head. A reconciliation
//! is the owner's own, operator-admitted record of deterministic validation
//! and an independent read-only review of exactly that merged head. It is
//! separate from review-gate certificates and has its own identity; it never
//! rewrites the original run's identity or the merged pull request.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::automation::SourceRevision;
use super::{ReviewFinding, ReviewVerdict};

/// Shipped job that runs one reconciliation attempt.
pub const REVIEW_RECONCILIATION_JOB: &str = "task_review_reconciliation_pipeline";

/// Reserved run-input key carrying an operator's reconciliation admission.
///
/// Every ordinary submission path refuses input that contains it and replay
/// strips it, so a run carrying it was admitted by the governed submission.
pub const REVIEW_RECONCILIATION_ADMISSION_KEY: &str = "review_reconciliation_admission";

/// Persisted record schema version. Version 4 binds the provider's landed
/// commit; an older record cannot authorize a baseline disposition.
pub const REVIEW_RECONCILIATION_SCHEMA_VERSION: u32 = 4;

/// One operator's admission of one reconciliation attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationAdmission {
    pub reconciliation_id: String,
    /// 1-based attempt number the admission was issued for.
    pub attempt: u32,
    pub authorized_by: String,
    pub authorizer_provenance: String,
    pub authorized_at: DateTime<Utc>,
}

impl ReconciliationAdmission {
    /// Read an admission out of a run input. A malformed value reads as absent.
    pub fn from_run_input(input: &Value) -> Option<Self> {
        serde_json::from_value(input.get(REVIEW_RECONCILIATION_ADMISSION_KEY)?.clone()).ok()
    }
}

/// Whether `input` carries the reserved reconciliation admission key at all.
pub fn run_input_declares_review_reconciliation(input: &Value) -> bool {
    input
        .as_object()
        .is_some_and(|object| object.contains_key(REVIEW_RECONCILIATION_ADMISSION_KEY))
}

/// Remove the reserved admission key, returning whether one was present.
pub fn strip_review_reconciliation_admission(input: &mut Value) -> bool {
    input
        .as_object_mut()
        .is_some_and(|object| object.remove(REVIEW_RECONCILIATION_ADMISSION_KEY).is_some())
}

/// The stopped foreign execution whose delivery is reconciled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledExecution {
    pub run_id: String,
    pub machine_id: String,
    pub claim_id: String,
    pub handoff_id: String,
    /// The handed-off candidate the merged head differs from.
    pub candidate_commit: String,
}

/// The merged pull request named by the accepted handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledPullRequest {
    pub number: u64,
    pub url: String,
    pub repository: String,
    pub landing_branch: String,
    /// The exact head the pull request merged at.
    pub merged_head: SourceRevision,
    /// The merge base of that head with the landing branch: the baseline a
    /// failing command is reproduced on.
    pub base: SourceRevision,
    /// The commit the provider reports the pull request landed as: its merge
    /// commit, squash commit or last rebased commit. A baseline remediation
    /// must contain it. Absent only on a record that predates binding it,
    /// which can never authorize a baseline disposition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed: Option<SourceRevision>,
}

/// Everything a reconciliation is about. Any change between observation and
/// recording refuses the reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationBinding {
    pub workspace_id: String,
    pub task_id: String,
    pub task_meaning_digest: String,
    pub execution: ReconciledExecution,
    pub pull_request: ReconciledPullRequest,
}

/// Where a reconciliation's required commands came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationCommandSource {
    /// The owner's commands captured when it accepted the handoff.
    AcceptedHandoff,
    /// The accepted handoff explicitly required no command, so the operator's
    /// submission adopted the owner's configuration at that moment as this
    /// reconciliation's own contract. It is not a historical requirement.
    OwnerConfigurationAtSubmission,
}

impl ReconciliationCommandSource {
    /// Stable label for projections.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AcceptedHandoff => "accepted_handoff",
            Self::OwnerConfigurationAtSubmission => "owner_configuration_at_submission",
        }
    }
}

/// The validation and reviewer contract frozen when the operator submitted.
/// Every attempt of the record runs under it, whatever the owner's
/// configuration says by then; it never changes once recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationContract {
    /// The accepted handoff's captured command list, verbatim. An empty list
    /// is the acceptance's explicit no-check contract, never a missing one.
    pub accepted_commands: Vec<String>,
    /// The commands every attempt runs at the merged head.
    pub required_commands: Vec<String>,
    pub commands_source: ReconciliationCommandSource,
    /// The independent review crew every attempt's reviewer runs on.
    pub review_crew: String,
    /// The configuration layer that selected `review_crew`.
    pub review_crew_source: String,
    pub frozen_at: DateTime<Utc>,
}

/// One persisted log, addressed by task artifact path and content digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationLog {
    pub path: String,
    pub sha256: String,
}

/// One run of a required command at one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledCommandRun {
    pub commit: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    pub log: ReconciliationLog,
}

/// A required command's result at the merged head and, when it failed there,
/// its reproduction at the base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledCommand {
    pub command: String,
    pub head: ReconciledCommandRun,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<ReconciledCommandRun>,
}

impl ReconciledCommand {
    /// A failure the base already had: the head's failure is attributable to
    /// the baseline rather than to this delivery.
    pub fn reproduced_on_base(&self) -> bool {
        !self.head.passed
            && self.head.failure_kind.as_deref() == Some("candidate")
            && self
                .baseline
                .as_ref()
                .is_some_and(|run| !run.passed && run.failure_kind.as_deref() == Some("candidate"))
    }
}

/// Deterministic validation of the merged head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledValidation {
    pub run_id: String,
    pub commands: Vec<ReconciledCommand>,
    /// True only when every required command passed at the merged head. An
    /// operator disposition never sets it.
    pub complete: bool,
    pub recorded_at: DateTime<Utc>,
}

/// The independent read-only review of the merged head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledReview {
    pub run_id: String,
    pub crew: String,
    pub verdict: ReviewVerdict,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
    pub report: ReconciliationLog,
    pub recorded_at: DateTime<Utc>,
}

/// Where a reconciliation ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReconciliationOutcome {
    /// The review accepted the head and every required command passed.
    Accepted,
    /// The review accepted the head; every failing command also fails at the
    /// base and waits for an evidence-bound operator disposition.
    AwaitingDisposition { commands: Vec<String> },
    /// Every baseline failure was disposed by an operator. Validation stays
    /// incomplete.
    AcceptedWithDisposition,
    /// The reconciliation cannot accept this head.
    Refused { reason: String, next_step: String },
}

impl ReconciliationOutcome {
    /// Stable label for projections.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::AwaitingDisposition { .. } => "awaiting_disposition",
            Self::AcceptedWithDisposition => "accepted_with_disposition",
            Self::Refused { .. } => "refused",
        }
    }
}

/// An operator's audited acceptance of one baseline failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineDisposition {
    pub command: String,
    pub head_commit: String,
    pub failure_log_sha256: String,
    pub baseline_log_sha256: String,
    /// Landed commit that remediates the baseline failure.
    pub remediation_commit: String,
    /// The bound landed delivery the remediation commit contains. A
    /// disposition without it cannot authorize completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_commit: Option<String>,
    /// Passing execution of the same required command at the remediation
    /// commit. Older dispositions do not establish that the failure was
    /// actually fixed, so they cannot authorize completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation_check: Option<ReconciliationLog>,
    pub reason: String,
    pub actor: String,
    pub provenance: String,
    pub recorded_at: DateTime<Utc>,
}

/// An operator's recorded test of a landed remediation for a baseline failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineRemediationCheck {
    pub command: String,
    pub head_commit: String,
    pub remediation_commit: String,
    /// The bound landed delivery the remediation commit contained when it was
    /// tested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_commit: Option<String>,
    pub run: ReconciledCommandRun,
    pub actor: String,
    pub provenance: String,
    pub checked_at: DateTime<Utc>,
}

/// One admitted run of a reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationAttempt {
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub admitted_by: String,
    pub admitted_at: DateTime<Utc>,
}

/// The host-owned reconciliation record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReconciliation {
    pub schema_version: u32,
    pub reconciliation_id: String,
    /// Operator-chosen key: resubmitting it replays this record.
    pub request_key: String,
    pub binding: ReconciliationBinding,
    pub binding_digest: String,
    pub contract: ReconciliationContract,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    #[serde(default)]
    pub attempts: Vec<ReconciliationAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<ReconciledValidation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReconciledReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ReconciliationOutcome>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispositions: Vec<BaselineDisposition>,
    /// Outcomes of testing proposed baseline remediations. Failed runs are
    /// retained as evidence and cannot be converted into a disposition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remediation_checks: Vec<BaselineRemediationCheck>,
    pub revision: u32,
    pub updated_at: DateTime<Utc>,
}

impl ReviewReconciliation {
    /// Whether this record lets the desktop consumer complete its task.
    pub fn accepts_completion(&self) -> bool {
        match &self.outcome {
            Some(ReconciliationOutcome::Accepted) => true,
            Some(ReconciliationOutcome::AcceptedWithDisposition)
                if self.schema_version == REVIEW_RECONCILIATION_SCHEMA_VERSION =>
            {
                let (Some(validation), Some(landed)) =
                    (&self.validation, &self.binding.pull_request.landed)
                else {
                    return false;
                };
                let landed = Some(landed.commit.as_str());
                let baseline: Vec<_> = validation
                    .commands
                    .iter()
                    .filter(|command| command.reproduced_on_base())
                    .collect();
                !baseline.is_empty()
                    && baseline.iter().all(|command| {
                        self.dispositions.iter().any(|disposition| {
                            disposition.command == command.command
                                && disposition.head_commit
                                    == self.binding.pull_request.merged_head.commit
                                && disposition.landed_commit.as_deref() == landed
                                && disposition.failure_log_sha256 == command.head.log.sha256
                                && disposition.baseline_log_sha256
                                    == command
                                        .baseline
                                        .as_ref()
                                        .map(|run| run.log.sha256.as_str())
                                        .unwrap_or_default()
                                && disposition.remediation_check.as_ref().is_some_and(|log| {
                                    self.remediation_checks.iter().any(|check| {
                                        check.command == command.command
                                            && check.head_commit == disposition.head_commit
                                            && check.remediation_commit
                                                == disposition.remediation_commit
                                            && check.landed_commit.as_deref() == landed
                                            && check.run.passed
                                            && check.run.exit_code == Some(0)
                                            && check.run.failure_kind.is_none()
                                            && check.run.commit == disposition.remediation_commit
                                            && check.run.log.path == log.path
                                            && check.run.log.sha256 == log.sha256
                                    })
                                })
                        })
                    })
            }
            _ => false,
        }
    }

    /// Whether no further run may be admitted for this record.
    pub fn runs_settled(&self) -> bool {
        self.outcome.is_some()
    }

    /// The newest admitted attempt.
    pub fn current_attempt(&self) -> Option<&ReconciliationAttempt> {
        self.attempts.last()
    }
}
