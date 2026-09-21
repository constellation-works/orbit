//! State-trigger inputs and durable per-member scheduling [ORB-11331].

use super::SourceRevision;
use crate::task::{TaskStatus, TaskType};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Members admitted per attempt when a definition names no `batch_size`.
pub const DEFAULT_BATCH_SIZE: usize = 5;
/// Upper bound on members in one attempt; also the largest observation page.
pub const MAX_BATCH_SIZE: usize = 50;

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
    /// How many due members one admission batches into a single attempt
    /// [ORB-12746]. Absent, [`DEFAULT_BATCH_SIZE`] capped by `max_items`; an
    /// explicit value must lie in `1..=min(50, max_items)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<usize>,
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
        if self
            .batch_size
            .is_some_and(|size| !(1..=MAX_BATCH_SIZE.min(self.max_items)).contains(&size))
        {
            return Err(super::super::error::WorkflowError::Invalid(
                "state trigger batch_size must be between 1 and the smaller of 50 and max_items"
                    .into(),
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

    /// Members one admission batches into a single attempt: the configured
    /// size, or the default, never above `max_items`.
    pub fn effective_batch_size(&self) -> usize {
        self.batch_size
            .unwrap_or(DEFAULT_BATCH_SIZE)
            .min(self.max_items)
            .max(1)
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
    /// Stored `task.crew` identity used to keep a preparation batch
    /// crew-homogeneous [ORB-12761]. `None` means no explicit crew, which the
    /// one-bundle-one-crew dispatch rule treats as distinct from any named crew.
    /// Records persisted before this field existed deserialize as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
}

/// Trimmed non-empty `task.crew`, or `None` when unset. Matches the identity
/// `resolve_crew_for_run_input` requires to be unanimous across a bundle.
pub fn bundle_crew(crew: Option<&str>) -> Option<String> {
    crew.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// One claim over a batch of due members [ORB-12746]. `member` is the first
/// member and `members` the whole batch; a record persisted before batching
/// carries `member` alone, which [`MemberAttempt::members`] reads as a batch
/// of one so it still reconciles and completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAttempt {
    pub consumer: String,
    pub kind: StateTriggerKind,
    pub id: String,
    pub member: StateMember,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<StateMember>,
    pub attempt: u32,
    pub max_attempts: u32,
    pub deadline: DateTime<Utc>,
    pub retry_after: DateTime<Utc>,
    pub action_key: String,
    pub action_id: Option<String>,
    pub exhausted: bool,
}

impl MemberAttempt {
    /// Every member of the batch, first member first.
    pub fn members(&self) -> &[StateMember] {
        if self.members.is_empty() {
            std::slice::from_ref(&self.member)
        } else {
            &self.members
        }
    }

    /// The batch member identified by `key`.
    pub fn member_for(&self, key: &str) -> Option<&StateMember> {
        self.members().iter().find(|member| member.key == key)
    }

    /// Every task the batch covers, in member order.
    pub fn task_ids(&self) -> Vec<String> {
        self.members()
            .iter()
            .flat_map(|member| member.task_ids.iter().cloned())
            .collect()
    }

    /// Whether `members` is a well-formed batch: non-empty, bounded, keys
    /// unique, and `member` is its first entry.
    pub fn batch_is_consistent(&self) -> bool {
        let members = self.members();
        let mut keys = std::collections::BTreeSet::new();
        !members.is_empty()
            && members.len() <= MAX_BATCH_SIZE
            && members[0] == self.member
            && members
                .iter()
                .all(|member| keys.insert(member.key.as_str()))
    }
}

/// Every member of an attempt is its own apply boundary: independently
/// accepted results become durable in one receipt while a sibling that did
/// not apply is recorded failed at its fingerprint.
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

/// Deterministic apply evidence for one member, never an agent-authored
/// promotion grant.
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

/// How one run settled every member of an attempt [ORB-12746]: the receipt
/// evidence for a batch. Members absent from `applied` are recorded failed at
/// their fingerprint with the reason in `failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberBatchEvidence {
    pub action_id: String,
    pub attempt_id: String,
    pub applied: Vec<MemberEvidence>,
    #[serde(default)]
    pub failed: BTreeMap<String, String>,
}

/// One member of a pending or active batch and why it is there, for
/// `orbit clock tick --dry-run` and `orbit routine show` [ORB-12746].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchMember {
    pub key: String,
    pub task_ids: Vec<String>,
    /// `settled`, `max_wait` or `grant` for a member about to be admitted;
    /// `admitted` for one already in flight.
    pub reason: String,
}
