//! The opt-in completion contract an auto-task definition may declare, and the
//! typed shapes around it: the candidate evidence a run leaves as a task
//! artifact, a registered adapter's verdict, the trusted receipt the owner
//! builds from both, and the per-definition history of accepted receipts.
//!
//! A receipt's identity, digest, source and time come from the owner's own
//! records; candidate evidence and an adapter's verdict are inputs to validate,
//! never a certificate. What counts as a passing result is the workspace
//! adapter's policy, so `form` is opaque here.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::task::validate_relative_artifact_path;
use crate::workflow::WorkflowError;
use crate::workflow::activity_job::is_valid_script_name;

/// Candidate evidence's and a verdict's `schema_version`.
pub const AUTO_TASK_EVIDENCE_SCHEMA_VERSION: u32 = 1;
/// The accepted-result history file's `schema_version`.
pub const AUTO_TASK_ACCEPTED_HISTORY_SCHEMA_VERSION: u32 = 1;

/// A definition's declared completion contract. Omitted, a definition keeps
/// ordinary structured task completion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskCompletionContract {
    /// The registered result contract, e.g. `release-readiness/v1`. A job step
    /// must name the same one in `require_result`.
    pub contract: String,
    /// The granted workspace script that judges the evidence.
    pub adapter: String,
    /// Which evidence form the adapter applies. Opaque to Orbit.
    pub form: String,
    /// The task artifact holding the [`AutoTaskEvidenceCandidate`].
    pub evidence_artifact: String,
    /// How old an accepted result may be when a later step relies on it.
    pub max_age_minutes: u32,
    /// Tags whose tasks the owner reads back for the adapter, so a policy can
    /// judge a task the evidence only names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_task_tags: Vec<String>,
    /// What this form's evidence must hold, added to a task a job step mints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_instructions: Option<String>,
}

impl AutoTaskCompletionContract {
    /// Shape checks for a declaration; whether the contract is registered is
    /// judged where the registry lives, when a step requires it.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        let invalid = |detail: String| Err(WorkflowError::Invalid(format!("completion: {detail}")));
        if self.contract.trim().is_empty() {
            return invalid("contract must not be empty".to_string());
        }
        if !is_valid_script_name(&self.adapter) {
            return invalid(format!(
                "adapter '{}' is not a valid script name",
                self.adapter
            ));
        }
        if self.form.trim().is_empty() {
            return invalid("form must not be empty".to_string());
        }
        if let Err(error) = validate_relative_artifact_path(&self.evidence_artifact) {
            return invalid(format!("evidence_artifact: {error}"));
        }
        if self.max_age_minutes == 0 {
            return invalid("max_age_minutes must be positive".to_string());
        }
        if self.read_task_tags.iter().any(|tag| tag.trim().is_empty()) {
            return invalid("read_task_tags must not hold an empty tag".to_string());
        }
        Ok(())
    }
}

/// What the evidence says it covers; every field must equal the owner's value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskEvidenceBinding {
    pub workspace_id: String,
    pub definition: String,
    pub definition_digest: String,
    pub task_id: String,
    pub run_id: String,
    pub source_revision: String,
}

/// One task artifact, by path and SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskEvidenceRef {
    pub path: String,
    pub sha256: String,
}

/// The evidence manifest a run writes as a task artifact. It carries no time
/// and no status: the owner supplies the first and the adapter judges the
/// second, so a manifest claiming either is malformed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskEvidenceCandidate {
    pub schema_version: u32,
    pub contract: String,
    pub binding: AutoTaskEvidenceBinding,
    pub artifacts: Vec<AutoTaskEvidenceRef>,
    /// Tasks the evidence cites, read back for the adapter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_refs: Vec<String>,
}

/// A normalized contract result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AutoTaskResultStatus {
    Passed,
    Blocked,
    Unavailable,
}

impl AutoTaskResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Blocked => "blocked",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One reason an adapter gives for its verdict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskVerdictReason {
    pub code: String,
    pub detail: String,
}

/// What a registered adapter prints. It echoes what it judged so the owner can
/// refuse an answer about something else.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskAdapterVerdict {
    pub schema_version: u32,
    pub contract: String,
    pub form: String,
    pub task_id: String,
    pub source_revision: String,
    pub status: AutoTaskResultStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<AutoTaskVerdictReason>,
}

/// The trusted receipt of an accepted result. Field names follow the typed
/// `result` a job's `reuse_when` reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskCompletionReceipt {
    pub contract: String,
    pub status: AutoTaskResultStatus,
    pub workspace_id: String,
    pub definition: String,
    pub definition_digest: String,
    pub task_id: String,
    pub run_id: String,
    /// The exact source revision the evidence covers.
    pub revision: String,
    /// The owner's recorded settlement of the task.
    pub completed_at: DateTime<Utc>,
    pub evidence: Vec<AutoTaskEvidenceRef>,
    pub adapter: String,
    /// The digest of the granted script that judged the evidence.
    pub adapter_digest: String,
    /// When the owner accepted the result.
    pub accepted_at: DateTime<Utc>,
}

/// The stored latest accepted receipt per definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskAcceptedHistoryFile {
    pub schema_version: u32,
    #[serde(default)]
    pub definitions: BTreeMap<String, AutoTaskCompletionReceipt>,
}

impl Default for AutoTaskAcceptedHistoryFile {
    fn default() -> Self {
        Self {
            schema_version: AUTO_TASK_ACCEPTED_HISTORY_SCHEMA_VERSION,
            definitions: BTreeMap::new(),
        }
    }
}

/// A definition's accepted-result history as read now. A definition with no
/// accepted result and a history that cannot be read are different answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoTaskAcceptedHistory {
    Latest(Box<AutoTaskCompletionReceipt>),
    Missing,
    Unreadable(String),
}
