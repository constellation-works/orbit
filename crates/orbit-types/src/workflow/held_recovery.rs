//! Audited authority to reconsider one retained, rejected delivery.
//!
//! Admission is prospective. It never changes a certificate, a claim's
//! settlement, or the meaning the original candidate was produced under.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::task::{Task, TaskRelation, TaskType};

/// Immutable admission history; only the owner operator boundary writes it.
pub const HELD_RECOVERY_ADMITTED: &str = "held_recovery_admitted";
/// A normal pull admission atomically consumes the recovery reservation.
pub const HELD_RECOVERY_CLAIMED: &str = "held_recovery_claimed";

/// These permissions have deliberately different consequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldRecoveryDisposition {
    /// Ask an independent reviewer to judge an unavailable external proof.
    AdjudicateEvidence,
    /// Implement an explicitly approved amendment, then validate and review.
    SourceRepair,
}

/// The task contract approved by the operator, excluding mutable audit state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldRecoveryMeaning {
    pub id: String,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub plan: String,
    pub context_files: Vec<String>,
    pub tags: Vec<String>,
    pub relations: Vec<TaskRelation>,
    pub task_type: TaskType,
    pub required_tools: Vec<String>,
}

impl HeldRecoveryMeaning {
    /// Canonicalize set-valued fields so ordering alone changes no permission.
    pub fn from_task(task: &Task) -> Self {
        fn set(values: &[String]) -> Vec<String> {
            let mut values = values.to_vec();
            values.sort();
            values.dedup();
            values
        }
        Self {
            id: task.id.clone(),
            title: task.title.clone(),
            description: task.description.clone(),
            acceptance_criteria: task.acceptance_criteria.clone(),
            plan: task.plan.clone(),
            context_files: set(&task.context_files),
            tags: set(&task.tags),
            relations: task.relations.clone(),
            task_type: task.task_type,
            required_tools: set(&task.required_tools),
        }
    }

    /// Restore the contract onto a task for the existing meaning/spec digests.
    pub fn apply_to(&self, task: &Task) -> Task {
        let mut original = task.clone();
        original.id = self.id.clone();
        original.title = self.title.clone();
        original.description = self.description.clone();
        original.acceptance_criteria = self.acceptance_criteria.clone();
        original.plan = self.plan.clone();
        original.context_files = self.context_files.clone();
        original.tags = self.tags.clone();
        original.relations = self.relations.clone();
        original.task_type = self.task_type;
        original.required_tools = self.required_tools.clone();
        original
    }

    /// Exact field changes displayed at inspection and approved at submission.
    pub fn delta(&self, proposed: &Self) -> Result<Vec<HeldRecoveryChange>, serde_json::Error> {
        let before = serde_json::to_value(self)?;
        let after = serde_json::to_value(proposed)?;
        let mut changes = Vec::new();
        if let (Some(before), Some(after)) = (before.as_object(), after.as_object()) {
            for (field, value) in before {
                if after.get(field) != Some(value) {
                    changes.push(HeldRecoveryChange {
                        field: field.clone(),
                        before: value.clone(),
                        after: after[field].clone(),
                    });
                }
            }
        }
        Ok(changes)
    }
}

/// A field-level task amendment. The original value remains durable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldRecoveryChange {
    pub field: String,
    pub before: Value,
    pub after: Value,
}

/// Exact observed source, task amendment, diagnostic and publication lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldRecoveryBinding {
    pub schema_version: u32,
    pub workspace_id: String,
    pub task_id: String,
    pub disposition: HeldRecoveryDisposition,
    pub claim_id: String,
    pub machine_id: String,
    pub source_run_id: String,
    /// Stable claim inspection digest, including the unchanged settlement.
    pub claim_digest: String,
    pub attempt_id: String,
    pub certificate_digest: String,
    pub hold_digest: Option<String>,
    /// Exact artifact inventory and hashes shown to the approving operator.
    pub diagnostics_digest: String,
    pub repository: String,
    pub remote_url: String,
    pub branch: String,
    pub pull_request: String,
    pub pull_request_url: String,
    pub candidate_sha: String,
    pub candidate_tree: String,
    /// Target branch tip at inspection, checked again before source effects.
    pub base_sha: String,
    /// Historical base of the exact retained source and its certificate.
    pub source_base_sha: String,
    pub base_branch: String,
    pub original: HeldRecoveryMeaning,
    pub proposed: HeldRecoveryMeaning,
    pub original_meaning_digest: String,
    pub proposed_meaning_digest: String,
    pub original_spec_digest: String,
    pub proposed_spec_digest: String,
    pub delta: Vec<HeldRecoveryChange>,
    pub repair_selectors: Vec<String>,
    /// Configured review and native validation policy approved at inspection.
    pub review: super::ReviewAdmission,
}

/// A request-key-bound operator decision, persisted before continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldRecoveryAdmission {
    pub request_key: String,
    pub binding: HeldRecoveryBinding,
    pub reason: String,
    /// Original certificate, hold and claim diagnostics, retained verbatim.
    pub evidence: Value,
    pub authorized_by: String,
    pub authorizer_provenance: String,
    pub authorized_at: DateTime<Utc>,
}

/// An independent reviewer's prospective judgement, never an evidence receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HeldEvidenceDecision {
    Uphold {
        reason: String,
    },
    Reject {
        reason: String,
    },
    Replace {
        reason: String,
        requirements: Vec<super::ReviewEvidenceRequirement>,
    },
}
