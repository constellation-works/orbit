//! Durable condition-trigger state: per-subject arm states, holds and fire
//! records, kept in the owner's automation store in their own rows
//! (`docs/design/condition-triggers/1_scope.md` §3, §10).
//!
//! This is persisted shape only. The edge rules that move it live in
//! `orbit-automation`'s `condition` module.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::super::error::WorkflowError;

/// Schema version of [`ConditionState`]. A row with another version is never
/// read as a fresh baseline; it fails closed as a diagnostic.
pub const CONDITION_STATE_SCHEMA_VERSION: u32 = 1;

/// Days a fire record, and a fired subject that is no longer seen, is kept.
pub const CONDITION_FIRED_RETENTION_DAYS: i64 = 30;

/// Most fire records and most subject records one condition retains.
pub const CONDITION_RETAINED_RECORDS: usize = 1_000;

/// Most subjects one condition evaluates per pass.
pub const CONDITION_SUBJECTS_PER_PASS: usize = 200;

/// The deepest chain a fire may extend: a fire whose subject was created by a
/// fire at this depth is refused as `loop_suspected`.
pub const MAX_CONDITION_CHAIN_DEPTH: u32 = 5;

/// Fires of one subject in one hour past which further edges are refused as
/// `oscillating`.
pub const CONDITION_OSCILLATION_FIRES_PER_HOUR: usize = 3;

/// Attempts at admitting one fire's action before it is recorded as failed.
pub const CONDITION_ADMISSION_ATTEMPTS: u32 = 5;

/// The task-history event a condition fire writes on the task it mints. Its
/// note is the JSON of [`ConditionOrigin`]; only the system actor writes it.
pub const CONDITION_FIRED_EVENT: &str = "condition_fired";

/// The subject key of a workspace-scope condition.
pub const CONDITION_WORKSPACE_SUBJECT: &str = "workspace";

/// Where a condition-created task came from, read as `task.origin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionOrigin {
    /// `routine:<name>` or `auto-task:<name>`.
    pub trigger: String,
    pub fire_id: String,
    /// The fire's chain depth: one more than its subject's origin depth.
    pub depth: u32,
    /// The subject the fire observed.
    pub subject: String,
}

impl ConditionOrigin {
    /// Parse a `condition_fired` history note. Anything else is not an origin.
    pub fn from_note(note: &str) -> Option<Self> {
        serde_json::from_str(note).ok()
    }

    /// The history note that records this origin.
    pub fn to_note(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Whether a subject may fire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionArm {
    /// Fires on its next held true observation.
    #[default]
    Armed,
    /// Fired (or baselined) and waiting for `rearm`.
    Fired,
}

impl ConditionArm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Fired => "fired",
        }
    }
}

/// One subject's edge memory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionSubject {
    pub arm: ConditionArm,
    /// How many times this subject re-armed; part of its fire identity.
    #[serde(default)]
    pub arm_sequence: u64,
    /// Start of the current uninterrupted true observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub true_since: Option<DateTime<Utc>>,
    /// The latest pass that evaluated this subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    /// Why a held edge is waiting (`cooldown` or `fire_cap`), and since when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_since: Option<DateTime<Utc>>,
    /// The latest fire of this subject that admitted, or meant to admit, an
    /// action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_id: Option<String>,
    /// Fired by a baseline at enable or an epoch change, with no action.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub baselined: bool,
}

/// Where a fire's action stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionFireState {
    /// Recorded, action not yet acknowledged.
    Intent,
    /// The action exists; `action_id` names it.
    Acknowledged,
    /// Admission kept failing; nothing ran.
    Failed,
    /// A guard refused the fire; nothing ran.
    Refused,
}

impl ConditionFireState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Acknowledged => "acknowledged",
            Self::Failed => "failed",
            Self::Refused => "refused",
        }
    }

    /// Whether the fire admitted, or will admit, an action.
    pub fn admits(self) -> bool {
        matches!(self, Self::Intent | Self::Acknowledged)
    }
}

/// One fire: its immutable identity, the subject it captured, and its action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionFire {
    pub fire_id: String,
    pub subject: String,
    /// The observation revision at the edge: a task's history position, or a
    /// workspace subject's arm sequence. Never rewritten.
    pub revision: String,
    pub arm_sequence: u64,
    pub epoch: String,
    pub observed_at: DateTime<Utc>,
    /// One more than the subject's origin depth.
    pub depth: u32,
    /// The fire that created the subject, when a fire did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_fire_id: Option<String>,
    /// The subject's fields as observed at the edge, passed to the action.
    pub captured: serde_json::Value,
    /// `condition:<fire_id>:0`, the key the action is admitted under.
    pub action_key: String,
    pub state: ConditionFireState,
    /// The minted task or the submitted run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub attempts: u32,
    /// Why admission failed or the fire was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Why a condition stopped firing until an operator clears it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionStall {
    /// `match_storm`, `too_many_matches`, `loop_suspected` or `oscillating`.
    pub reason: String,
    pub at: DateTime<Utc>,
    pub detail: String,
    /// Up to 20 subjects that caused it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sample: Vec<String>,
}

/// What the latest pass saw.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionPass {
    pub at: Option<DateTime<Utc>>,
    pub evaluated: usize,
    pub matched: usize,
    pub unknown: usize,
    pub fired: usize,
    /// Whether the task lap reached the end of the prefilter.
    #[serde(default)]
    pub lap_complete: bool,
}

/// The latest skip, with its stable reason code (§9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionSkip {
    pub at: DateTime<Utc>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One condition consumer's durable state, fenced by `generation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionState {
    pub schema_version: u32,
    /// `<machine>/<workspace>/<routine|auto-task>/<name>`.
    pub consumer: String,
    pub generation: u64,
    /// Digest of the definition parts that give an edge its meaning.
    pub epoch: String,
    /// Whether the latest pass saw the definition enabled; a pass that sees it
    /// enabled after `false` is an enable.
    pub enabled: bool,
    #[serde(default)]
    pub subjects: BTreeMap<String, ConditionSubject>,
    /// Fire records, oldest first. They survive edits and disable/re-enable:
    /// `self` reads them.
    #[serde(default)]
    pub fires: Vec<ConditionFire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall: Option<ConditionStall>,
    #[serde(default)]
    pub last_pass: ConditionPass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_skip: Option<ConditionSkip>,
    /// Skips by reason code, counted rather than written to task history.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skip_counts: BTreeMap<String, u64>,
    /// Task scope: the last subject of an unfinished lap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lap_after: Option<String>,
}

impl ConditionState {
    /// A consumer seen for the first time: nothing armed, nothing fired.
    pub fn new(consumer: &str, epoch: &str) -> Self {
        Self {
            schema_version: CONDITION_STATE_SCHEMA_VERSION,
            consumer: consumer.to_string(),
            generation: 0,
            epoch: epoch.to_string(),
            enabled: false,
            subjects: BTreeMap::new(),
            fires: Vec::new(),
            stall: None,
            last_pass: ConditionPass::default(),
            last_skip: None,
            skip_counts: BTreeMap::new(),
            lap_after: None,
        }
    }

    /// Refuse a row this binary cannot interpret.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.schema_version != CONDITION_STATE_SCHEMA_VERSION {
            return Err(WorkflowError::Invalid(format!(
                "condition state for '{}' has schema_version {}; this Orbit reads {}",
                self.consumer, self.schema_version, CONDITION_STATE_SCHEMA_VERSION
            )));
        }
        if self.subjects.len() > CONDITION_RETAINED_RECORDS
            || self.fires.len() > CONDITION_RETAINED_RECORDS
        {
            return Err(WorkflowError::Invalid(format!(
                "condition state for '{}' holds more than {CONDITION_RETAINED_RECORDS} records",
                self.consumer
            )));
        }
        Ok(())
    }

    /// The fire with this ID.
    pub fn fire(&self, fire_id: &str) -> Option<&ConditionFire> {
        self.fires.iter().find(|fire| fire.fire_id == fire_id)
    }
}

/// An audited operator action on a condition consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionAudit {
    pub consumer: String,
    /// `clear_stall`.
    pub action: String,
    pub actor: String,
    pub at: DateTime<Utc>,
    pub reason: String,
    /// The stall the action cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall: Option<ConditionStall>,
}
