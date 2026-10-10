use thiserror::Error;

use super::activity_job::{Provider, RETIRED_BACKEND_MIGRATION, provider_sandbox_modes};
use super::final_recovery::MAX_DECISION_TEXT_CHARS;
use super::review::{
    REVIEW_ADMISSION_KEY, REVIEW_CONTRACT_VERSION, REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT, REVIEW_REPORT_HISTORY_LIMIT, REVIEW_REPORT_HISTORY_VERSION,
    RecordGap, ValidationOutcome,
};
use super::{JobRunState, RunEvent};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkflowError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    JobValidation(String),
    #[error("{0}")]
    SkillValidation(String),
}

/// A job run state change or step result the run lifecycle refuses.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum JobRunStateError {
    /// `state` is terminal, so no event moves it.
    #[error("invalid job run state transition: {state} + {event:?} (state is terminal)")]
    Terminal { state: JobRunState, event: RunEvent },
    /// `event` does not apply to the non-terminal `state`.
    #[error("invalid job run state transition: {state} + {event:?}")]
    Transition { state: JobRunState, event: RunEvent },
    /// `state` is not one of the write-once step result states.
    #[error(
        "invalid step result state: {state} (must be success, failed, timeout, skipped, \
         cancelled, interrupted, or held)"
    )]
    StepState { state: JobRunState },
}

/// A `provider_sandbox` override the provider does not offer.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderSandboxError {
    #[error(
        "`provider_sandbox` is empty; expected one of {}",
        provider_sandbox_modes(*.provider).join(", ")
    )]
    Empty { provider: Provider },
    #[error(
        "`provider_sandbox` `{mode}` is not supported for {}; expected one of {}",
        .provider.as_str(),
        provider_sandbox_modes(*.provider).join(", ")
    )]
    Unsupported { provider: Provider, mode: String },
}

/// A value of the retired `backend` key other than the inert `cli`.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error(
    "`backend: {value}` is no longer supported: {}",
    RETIRED_BACKEND_MIGRATION
)]
pub struct RetiredBackendError {
    pub value: String,
}

/// A `final_recovery` activity result that is not exactly one decision.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FinalRecoveryError {
    /// The result does not deserialize as a known decision.
    #[error("{reason}")]
    Malformed { reason: String },
    #[error("`{field}` must not be empty")]
    EmptyField { field: &'static str },
    #[error("`{field}` exceeds {} characters", MAX_DECISION_TEXT_CHARS)]
    FieldTooLong { field: &'static str },
    #[error(
        "`evidence_commit` must be a 7 to 64 character hexadecimal commit id, not \
         '{evidence_commit}'"
    )]
    EvidenceCommit { evidence_commit: String },
}

/// A review report that does not match the review report contract.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReviewReportError {
    #[error("review report is not JSON: {reason}")]
    NotJson { reason: String },
    /// `detail` names the first offending field when one can be located.
    #[error("{detail}")]
    Contract { detail: String },
}

/// A review report history that cannot be read, or a report revision it
/// refuses to retain.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReviewHistoryError {
    /// The stored history is not valid JSON of the history shape.
    #[error("{} is unreadable: {reason}", REVIEW_REPORT_HISTORY_ARTIFACT)]
    Unreadable { reason: String },
    #[error(
        "{} has schema_version {found}; this build reads version {}",
        REVIEW_REPORT_HISTORY_ARTIFACT,
        REVIEW_REPORT_HISTORY_VERSION
    )]
    UnsupportedVersion { found: u32 },
    /// One attempt filled the history; recording more would drop its own
    /// obligations.
    #[error(
        "attempt {attempt_id} already recorded {} report revisions; settle it or admit a fresh \
         review",
        REVIEW_REPORT_HISTORY_LIMIT
    )]
    AttemptFull { attempt_id: String },
    /// Required records without a stable id: each command with a free id to
    /// suggest for it.
    #[error(
        "{}: every required validation record needs a stable, non-empty `id` that later \
         revisions carry forward; these have none: {}. Add the ids and attach the report again",
        REVIEW_REPORT_ARTIFACT,
        missing_record_ids(.missing)
    )]
    RecordIdsMissing { missing: Vec<(String, String)> },
    /// More than one record uses `id`, other than one `superseded` attempt
    /// and its `required` replacement.
    #[error(
        "{}: validation record id `{id}` is used by multiple records ({}); give each check a \
         distinct id. Only one `superseded` attempt and its `required` replacement may share an \
         id; correct the report and attach it again",
        REVIEW_REPORT_ARTIFACT,
        backticked(.commands)
    )]
    RecordIdReused { id: String, commands: Vec<String> },
    /// The report fails to account for required record `id` that an
    /// earlier revision of `attempt_id` recorded with `outcome`.
    #[error(
        "{}: required validation record `{id}` (`{command}`) was recorded {} by an earlier \
         revision of attempt {attempt_id} and this report {}. Carry `\"id\": \"{id}\"` forward \
         on a `required` record (or a `superseded` one with its passing replacement) holding the \
         check's current command and outcome{}; then attach the report again",
        REVIEW_REPORT_ARTIFACT,
        .outcome.as_str(),
        .gap.describe(),
        retire_hint(.id, *.outcome)
    )]
    RecordDropped {
        id: String,
        command: String,
        outcome: ValidationOutcome,
        attempt_id: String,
        gap: RecordGap,
    },
}

fn missing_record_ids(missing: &[(String, String)]) -> String {
    missing
        .iter()
        .map(|(command, id)| format!("`{command}` -> `\"id\": \"{id}\"`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn backticked(commands: &[String]) -> String {
    commands
        .iter()
        .map(|command| format!("`{command}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn retire_hint(id: &str, outcome: ValidationOutcome) -> String {
    if outcome == ValidationOutcome::Failed {
        "; a failed record cannot be retired, so rerun it".to_string()
    } else {
        format!(
            ", or list it in `retired_validation` as \
             {{\"id\": \"{id}\", \"reason\": \"<why it no longer applies>\"}}"
        )
    }
}

/// A review admission snapshot in a run input that this build cannot use.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReviewAdmissionError {
    #[error("invalid `{}` run input: {reason}", REVIEW_ADMISSION_KEY)]
    Malformed { reason: String },
    #[error(
        "unsupported `{}` run input: contract_version {found} is not the supported review \
         contract version {}",
        REVIEW_ADMISSION_KEY,
        REVIEW_CONTRACT_VERSION
    )]
    UnsupportedContractVersion { found: u32 },
}
