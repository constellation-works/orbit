//! State-trigger inputs and durable per-member scheduling [ORB-11331].

use super::SourceRevision;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateTriggerKind {
    PreparationEligible,
    ExecutionFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateTrigger {
    pub kind: StateTriggerKind,
    pub owner_machine: String,
    pub branch: String,
    pub debounce_minutes: u32,
    pub max_wait_minutes: u32,
    pub max_items: usize,
    pub retries: u32,
    pub deadline_minutes: u32,
}

impl StateTrigger {
    pub fn validate(&self) -> Result<(), super::super::error::WorkflowError> {
        if self.owner_machine.trim().is_empty()
            || self.branch.is_empty()
            || self.branch.starts_with('-')
            || self.branch.chars().any(char::is_whitespace)
            || self.debounce_minutes == 0
            || self.max_wait_minutes < self.debounce_minutes
            || !(1..=50).contains(&self.max_items)
            || self.retries > 5
            || self.deadline_minutes == 0
            || self.deadline_minutes > 1440
        {
            return Err(super::super::error::WorkflowError::Invalid(
                "state trigger requires owner, branch, positive debounce <= max wait, 1..50 members, retries <= 5 and deadline 1..1440 minutes".into(),
            ));
        }

        Ok(())
    }

    pub fn job_name(&self) -> &'static str {
        match self.kind {
            StateTriggerKind::PreparationEligible => "task_pilot_pipeline",
            StateTriggerKind::ExecutionFailed => "task_triage_pipeline",
        }
    }
}

/// Exact material input or causal incident; source facts are supplied by Core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateMember {
    pub key: String,
    pub task_ids: Vec<String>,
    pub fingerprint: String,
    pub source: SourceRevision,
    pub evidence: serde_json::Value,
    pub first_seen: DateTime<Utc>,
    pub changed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAttempt {
    pub consumer: String,
    pub kind: StateTriggerKind,
    pub id: String,
    pub member: StateMember,
    pub attempt: u32,
    pub max_attempts: u32,
    pub deadline: DateTime<Utc>,
    pub retry_after: DateTime<Utc>,
    pub action_key: String,
    pub action_id: Option<String>,
    pub exhausted: bool,
}

/// A single-member action is deliberately also a valid pilot partition. This
/// makes independently accepted results durable before any other member fails.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberState {
    pub pending: BTreeMap<String, StateMember>,
    pub active: Option<MemberAttempt>,
    pub failed: BTreeMap<String, MemberAttempt>,
    pub assessed: BTreeMap<String, MemberAssessment>,
    pub withheld: BTreeMap<String, String>,
    pub scan_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAssessment {
    pub input_fingerprint: String,
    pub resulting_fingerprint: String,
    pub ready: bool,
    pub receipt_id: String,
}

/// Deterministic apply evidence, never an agent-authored promotion grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberEvidence {
    pub action_id: String,
    pub attempt_id: String,
    pub member_key: String,
    pub input_fingerprint: String,
    pub resulting_fingerprint: String,
    pub ready: bool,
    pub result: serde_json::Value,
}
