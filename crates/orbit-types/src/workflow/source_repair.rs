//! Owner-local authority for preserved published candidates.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ReviewAdmission;

/// Reserved run-input key, admitted only by the operator recovery path.
pub const SOURCE_REPAIR_KEY: &str = "__source_repair";
/// Append-only reservation, written before run admission or source effects.
pub const SOURCE_REPAIR_RESERVED: &str = "source_repair_reserved";
/// Append-only link to the one admitted repair run.
pub const SOURCE_REPAIR_LINKED: &str = "source_repair_linked";

/// Observed immutable source and current task scope the operator authorizes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRepairBinding {
    pub schema_version: u32,
    pub workspace_id: String,
    pub task_id: String,
    pub source_run_id: String,
    pub machine_id: String,
    pub source_digest: String,
    pub task_digest: String,
    pub repository: String,
    pub remote_url: String,
    pub branch: String,
    pub pull_request: String,
    pub pull_request_url: String,
    pub head_sha: String,
    pub base_sha: String,
    pub base_branch: String,
    /// The inspected task binding and its complete stopped execution chain.
    /// For kept inline publications this includes completion-only retries;
    /// the binding can still name the publisher when no replacement set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded: Option<SourceRepairSuccessor>,
    /// The original run whose worktree checkpoints a generic-resume source
    /// reused, when that source's own final recovery committed the repair its
    /// failure hook published. Absent for every other source shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_owner: Option<SourceRepairOwner>,
}

/// Exact original checkpoint owner of a resumed source, bound immutably.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRepairOwner {
    pub run_id: String,
    /// Digest of the owner's run, steps, checkpoints and definition.
    pub digest: String,
}

/// Task binding and stopped execution evidence acknowledged by source repair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRepairSuccessor {
    pub run_id: String,
    /// Digest of the complete stopped execution chain and its checkpoints.
    pub digest: String,
}

/// One request key consumes one bounded repair reservation, even if cancelled.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRepairAdmission {
    pub request_key: String,
    pub binding: SourceRepairBinding,
    /// Explicit remaining source work and observed failure identities.
    pub diagnosis: String,
    /// Operator evidence that the successful or kept inline publication did
    /// not meet acceptance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmet_acceptance: Option<String>,
    /// Current execution policy and operator identity observed before submission.
    /// Required for successful or kept inline publication rework; absent on
    /// older repairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_digest: Option<String>,
    pub authorized_by: String,
    pub authorizer_provenance: String,
    pub authorized_at: DateTime<Utc>,
    pub attempt: u32,
    /// Current policy captured for this attempt, never inherited from its source.
    pub review: ReviewAdmission,
}

/// Read every durable reservation, refusing unreadable history rather than
/// refunding its budget. Callers serialize access with the owning task lock.
pub fn source_repair_reservations(
    history: &[crate::task::TaskHistoryEntry],
) -> Result<Vec<SourceRepairAdmission>, serde_json::Error> {
    history
        .iter()
        .filter(|entry| entry.event == SOURCE_REPAIR_RESERVED)
        .map(|entry| serde_json::from_str(entry.note.as_deref().unwrap_or_default()))
        .collect()
}

/// Fresh authorization of an unbound reservation after correcting its plan.
/// The original reservation and its budget charge remain immutable.
pub const SOURCE_REPAIR_RECONCILED: &str = "source_repair_reconciled";

/// Append-only proof of the original consent and the separately authorized plan.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRepairReconciliation {
    pub reservation: SourceRepairAdmission,
    pub authorization: SourceRepairAdmission,
}

/// Resolve each reservation's sole effective authorization without changing its
/// charge. Refuse malformed, forged, divergent or duplicate reconciliation.
pub fn source_repair_authorizations(
    history: &[crate::task::TaskHistoryEntry],
) -> Result<Vec<SourceRepairAdmission>, serde_json::Error> {
    use serde::de::Error;
    let invalid = || serde_json::Error::custom("inconsistent source repair authorization history");
    let mut effective = source_repair_reservations(history)?;
    let mut keys = std::collections::BTreeSet::new();
    for entry in history.iter().filter(|e| e.event == SOURCE_REPAIR_RESERVED) {
        if entry.by != "system" {
            return Err(invalid());
        }
    }
    for record in &effective {
        if !keys.insert(record.request_key.clone()) {
            return Err(invalid());
        }
    }
    let mut reconciled = std::collections::BTreeSet::new();
    for entry in history
        .iter()
        .filter(|e| e.event == SOURCE_REPAIR_RECONCILED)
    {
        let record: SourceRepairReconciliation =
            serde_json::from_str(entry.note.as_deref().unwrap_or_default())?;
        let Some(original) = effective
            .iter_mut()
            .find(|r| r.request_key == record.reservation.request_key)
        else {
            return Err(invalid());
        };
        let mut unchanged = record.authorization.clone();
        unchanged.binding.task_digest = record.reservation.binding.task_digest.clone();
        unchanged.authorized_at = record.reservation.authorized_at;
        unchanged.review.captured_at = record.reservation.review.captured_at;
        if entry.by != "system"
            || !reconciled.insert(original.request_key.clone())
            || serde_json::to_value(&record.reservation)? != serde_json::to_value(&*original)?
            || serde_json::to_value(&unchanged)? != serde_json::to_value(&record.reservation)?
            || record.authorization.authorized_at < original.authorized_at
        {
            return Err(invalid());
        }
        *original = record.authorization;
    }
    Ok(effective)
}
