//! Code-scanning alert evidence readers.

use serde::{Deserialize, Serialize};

use super::TaskDecision;
use crate::task::TaskHistoryEntry;

/// Typed Code scanning alert evidence carried on a per-alert task's
/// creation history. The task description renders the same fields for
/// people; consolidation matches on this struct, never on that prose.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeScanningAlertEvidence {
    pub repository: String,
    pub number: u64,
    pub rule_id: String,
    pub rule_name: String,
    pub security_severity: String,
    pub tool_name: String,
    pub tool_version: String,
    pub tool_guid: String,
    pub message: String,
    pub git_ref: String,
    pub commit_sha: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: String,
}

/// A creation history entry carries a decision that is not Code scanning
/// alert evidence, so no legacy text may stand in for it.
#[derive(Debug, thiserror::Error)]
#[error("creation history carries a mismatched decision")]
pub struct CodeScanningAlertEvidenceError;

impl CodeScanningAlertEvidence {
    /// Read the evidence one history entry carries. `Ok(None)` means the
    /// entry carries no decision, so a caller may consult its isolated
    /// legacy reader; a mismatched decision is an error and must never
    /// fall through to text.
    pub fn from_entry(
        entry: &TaskHistoryEntry,
    ) -> Result<Option<Self>, CodeScanningAlertEvidenceError> {
        match &entry.decision {
            Some(TaskDecision::CodeScanningAlert(evidence)) => Ok(Some(evidence.clone())),
            Some(_) => Err(CodeScanningAlertEvidenceError),
            None => Ok(None),
        }
    }

    /// Read the evidence a task's creation history carries: the decision
    /// on its `created` entry. Later entries carry other decisions and
    /// are not consulted. `Ok(None)` when that entry (or the history)
    /// carries no decision.
    pub fn from_history(
        history: &[TaskHistoryEntry],
    ) -> Result<Option<Self>, CodeScanningAlertEvidenceError> {
        let Some(created) = history.iter().find(|entry| entry.event == "created") else {
            return Ok(None);
        };
        Self::from_entry(created)
    }
}
