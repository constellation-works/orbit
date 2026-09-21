//! State-trigger inputs and durable per-member scheduling [ORB-11331].

use super::SourceRevision;
use crate::task::{TaskStatus, TaskType};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateTriggerKind {
    PreparationEligible,
    /// Retained so persisted member state and existing definitions still
    /// deserialize; its target job is retired, so no new work dispatches
    /// through it.
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
    /// Which tasks a `preparation_eligible` trigger fingerprints [ORB-12745].
    /// Absent, it is the predicate that was hard-coded before it became
    /// configurable, so an older definition keeps its behaviour.
    #[serde(default)]
    pub eligibility: PreparationEligibility,
}

/// The task predicate a `preparation_eligible` consumer evaluates. Every
/// field defaults to the previously hard-coded rule: unstarted work
/// (`proposed` or `backlog`) that an explicit no-diff tag has not opted out.
///
/// The same resolved value gates observation, admission, promotion and the
/// material fingerprint, so a changed predicate invalidates stale
/// assessments instead of silently keeping them fresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PreparationEligibility {
    /// Statuses whose tasks carry material worth fingerprinting; a non-empty
    /// subset of `proposed` and `backlog`.
    pub statuses: Vec<TaskStatus>,
    /// A task carrying any of these tags is opted out entirely.
    pub exclude_tags: Vec<String>,
    /// A task must carry every one of these tags; empty requires none.
    pub require_tags: Vec<String>,
    /// Task types admitted; empty admits every type.
    pub task_types: Vec<TaskType>,
}

impl Default for PreparationEligibility {
    fn default() -> Self {
        Self {
            statuses: vec![TaskStatus::Proposed, TaskStatus::Backlog],
            exclude_tags: vec!["no-diff-expected".into(), "no-diff-needed".into()],
            require_tags: Vec::new(),
            task_types: Vec::new(),
        }
    }
}

impl PreparationEligibility {
    /// Whether this is the predicate a definition without an `eligibility`
    /// block resolves to, however its lists were ordered.
    pub fn is_default(&self) -> bool {
        self.normalized() == Self::default().normalized()
    }

    pub fn validate(&self) -> Result<(), super::super::error::WorkflowError> {
        let invalid = |detail: &str| {
            Err(super::super::error::WorkflowError::Invalid(format!(
                "state trigger eligibility {detail}"
            )))
        };
        if self.statuses.is_empty() {
            return invalid("requires at least one status");
        }
        if self
            .statuses
            .iter()
            .any(|status| !matches!(status, TaskStatus::Proposed | TaskStatus::Backlog))
        {
            return invalid("statuses must be a subset of proposed and backlog");
        }
        if self
            .exclude_tags
            .iter()
            .chain(&self.require_tags)
            .any(|tag| tag.trim().is_empty() || tag.trim() != tag)
        {
            return invalid("tags must be non-empty without surrounding whitespace");
        }
        if self
            .require_tags
            .iter()
            .any(|tag| self.exclude_tags.contains(tag))
        {
            return invalid("cannot both require and exclude the same tag");
        }
        Ok(())
    }

    /// Whether `task` is one this predicate fingerprints.
    pub fn admits(&self, task: &crate::task::Task) -> bool {
        self.statuses.contains(&task.status)
            && (self.task_types.is_empty() || self.task_types.contains(&task.task_type))
            && !task.tags.iter().any(|tag| self.exclude_tags.contains(tag))
            && self.require_tags.iter().all(|tag| task.tags.contains(tag))
    }

    /// The order-insensitive form: equivalent predicates authored in a
    /// different order normalize to the same value, so they fingerprint
    /// identically.
    pub fn normalized(&self) -> Self {
        let mut statuses = self.statuses.clone();
        statuses.sort_by_key(|status| status.to_string());
        statuses.dedup();
        let mut exclude_tags = self.exclude_tags.clone();
        exclude_tags.sort();
        exclude_tags.dedup();
        let mut require_tags = self.require_tags.clone();
        require_tags.sort();
        require_tags.dedup();
        let mut task_types = self.task_types.clone();
        task_types.sort_by_key(|task_type| task_type.to_string());
        task_types.dedup();
        Self {
            statuses,
            exclude_tags,
            require_tags,
            task_types,
        }
    }
}

impl StateTrigger {
    pub fn validate(&self) -> Result<(), super::super::error::WorkflowError> {
        if self.kind == StateTriggerKind::PreparationEligible {
            self.eligibility.validate()?;
        } else if !self.eligibility.is_default() {
            return Err(super::super::error::WorkflowError::Invalid(
                "state trigger eligibility applies to kind preparation_eligible only".into(),
            ));
        }
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

    /// The job a definition of this kind must target.
    ///
    /// `ExecutionFailed` still names `task_triage_pipeline`, which this Orbit
    /// no longer ships: terminal failed-run triage is retired, so an existing
    /// definition keeps validating and is skipped as retired by the routine
    /// loader (`RETIRED_ROUTINE_JOBS`) instead of failing on every clock tick.
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
