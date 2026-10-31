//! Condition triggers: a routine's `trigger.condition` or an auto-task's
//! `schedule.condition`, a typed CEL predicate over workspace state.
//!
//! This is the persisted shape only. The expression language, its schema,
//! the load-time checker and the edge rules live in `orbit-automation`. Task
//! and workspace conditions fire on the workspace owner; host and file scopes
//! parse and are refused (`docs/design/condition-triggers/1_scope.md`).

use serde::{Deserialize, Serialize};

use super::super::error::WorkflowError;

/// Default `max_fires_per_hour`.
pub const DEFAULT_CONDITION_FIRES_PER_HOUR: u32 = 10;
/// Default `max_fires_per_day`.
pub const DEFAULT_CONDITION_FIRES_PER_DAY: u32 = 50;
/// Default `max_matches`.
pub const DEFAULT_CONDITION_MAX_MATCHES: u32 = 20;
/// Largest admitted `max_matches`, the retained-subject cap.
pub const MAX_CONDITION_MATCHES: u32 = 1_000;
/// Longest admitted `for_minutes` hold or `cooldown_minutes`: one week.
pub const MAX_CONDITION_HOLD_MINUTES: u64 = 7 * 24 * 60;
/// Deepest admitted file-scope `max_depth`.
pub const MAX_CONDITION_ROOT_DEPTH: u32 = 8;
/// Admitted file-scope settle windows, in seconds.
pub const CONDITION_SETTLE_SECONDS_RANGE: std::ops::RangeInclusive<u64> = 10..=3_600;

/// The subject set a condition ranges over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionScope {
    /// One subject per task that passes the expression's status or tag
    /// prefilter, bound as `task`.
    Task,
    /// The owning workspace as one subject; the expression reads `board`.
    Workspace,
    /// One subject per evaluating host, reading `host`. Not available yet.
    Host,
    /// One subject per file (or same-stem group) under declared roots. Not
    /// available yet.
    File,
}

impl ConditionScope {
    /// The persisted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Workspace => "workspace",
            Self::Host => "host",
            Self::File => "file",
        }
    }

    /// Whether this release can type-check and explain the scope. Host and
    /// file scopes parse, and are refused as `scope_unavailable`.
    pub fn is_available(self) -> bool {
        matches!(self, Self::Task | Self::Workspace)
    }
}

/// What enabling (or re-enabling) does with subjects already true.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionOnEnable {
    /// Record subjects true at enable as already fired, without an action.
    #[default]
    Baseline,
    /// Fire the subjects true at enable, under `max_matches`.
    FireExisting,
}

/// How a file subject is fingerprinted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionFileIdentity {
    /// Full content hash up to `hash_full_max_bytes`, sampled above it.
    #[default]
    Auto,
    /// Always hash the whole file.
    Full,
}

/// Whether file subjects are grouped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionGroupBy {
    /// Each file is its own subject.
    #[default]
    None,
    /// Same-stem files in one directory are one subject.
    Stem,
}

/// A condition block, shared by both carriers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionTrigger {
    /// The subject set and the variable bound to each subject.
    pub scope: ConditionScope,
    /// The CEL predicate; a subject fires on its false-to-true edge.
    pub when: String,
    /// The CEL predicate that re-arms a fired subject. Absent means `!(when)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rearm: Option<String>,
    /// Minutes `when` must stay true, continuously observed, before a fire.
    #[serde(default)]
    pub for_minutes: u64,
    /// Minimum minutes between two fires of one subject.
    #[serde(default)]
    pub cooldown_minutes: u64,
    /// Fires per definition per hour.
    #[serde(default = "default_fires_per_hour")]
    pub max_fires_per_hour: u32,
    /// Fires per definition per day.
    #[serde(default = "default_fires_per_day")]
    pub max_fires_per_day: u32,
    /// Matches admitted at enable, and new edges admitted in one pass.
    #[serde(default = "default_max_matches")]
    pub max_matches: u32,
    /// What enabling does with subjects already true.
    #[serde(default)]
    pub on_enable: ConditionOnEnable,
    /// Count this definition's own minted tasks in `board`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub board_includes_own: bool,
    /// Host scope only: machine names or IDs that evaluate the definition.
    /// Empty means the workspace owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// File scope only: watched roots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
    /// File scope only: the glob a file must match under a root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    /// File scope only: how deep a root is scanned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// File scope only: the window after which an unchanged file is stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_seconds: Option<u64>,
    /// File scope only: how a file is fingerprinted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<ConditionFileIdentity>,
    /// File scope only: above this size, `identity: auto` samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_full_max_bytes: Option<u64>,
    /// File scope only: whether same-stem files form one subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<ConditionGroupBy>,
    /// File scope with `group_by: stem` only: the group's settle window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_settle_seconds: Option<u64>,
}

const fn default_fires_per_hour() -> u32 {
    DEFAULT_CONDITION_FIRES_PER_HOUR
}

const fn default_fires_per_day() -> u32 {
    DEFAULT_CONDITION_FIRES_PER_DAY
}

const fn default_max_matches() -> u32 {
    DEFAULT_CONDITION_MAX_MATCHES
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ConditionTrigger {
    /// Shape checks beyond serde, independent of the expression language.
    /// `clause` is where the block sits in its file (`trigger.condition` or
    /// `schedule.condition`), so every error names the key it refuses.
    pub fn validate(&self, clause: &str) -> Result<(), WorkflowError> {
        let invalid =
            |key: &str, why: String| Err(WorkflowError::Invalid(format!("{clause}.{key}: {why}")));
        if self.when.trim().is_empty() {
            return invalid("when", "must not be empty".into());
        }
        if self
            .rearm
            .as_deref()
            .is_some_and(|rearm| rearm.trim().is_empty())
        {
            return invalid("rearm", "must not be empty when present".into());
        }
        if self.for_minutes > MAX_CONDITION_HOLD_MINUTES {
            return invalid(
                "for_minutes",
                format!("must not exceed {MAX_CONDITION_HOLD_MINUTES}"),
            );
        }
        if self.cooldown_minutes > MAX_CONDITION_HOLD_MINUTES {
            return invalid(
                "cooldown_minutes",
                format!("must not exceed {MAX_CONDITION_HOLD_MINUTES}"),
            );
        }
        if self.max_fires_per_hour == 0 {
            return invalid("max_fires_per_hour", "must be at least 1".into());
        }
        if self.max_fires_per_day < self.max_fires_per_hour {
            return invalid(
                "max_fires_per_day",
                format!(
                    "must be at least max_fires_per_hour ({})",
                    self.max_fires_per_hour
                ),
            );
        }
        if self.max_matches == 0 || self.max_matches > MAX_CONDITION_MATCHES {
            return invalid(
                "max_matches",
                format!("must be between 1 and {MAX_CONDITION_MATCHES}"),
            );
        }
        if !self.hosts.is_empty() && self.scope != ConditionScope::Host {
            return invalid("hosts", "applies to scope host only".into());
        }
        if self.hosts.iter().any(|host| host.trim().is_empty()) {
            return invalid("hosts", "must not list an empty host".into());
        }
        if let Some(key) = self.file_scope_key()
            && self.scope != ConditionScope::File
        {
            return invalid(key, "applies to scope file only".into());
        }
        if self
            .max_depth
            .is_some_and(|depth| depth > MAX_CONDITION_ROOT_DEPTH)
        {
            return invalid(
                "max_depth",
                format!("must not exceed {MAX_CONDITION_ROOT_DEPTH}"),
            );
        }
        for (key, seconds) in [
            ("settle_seconds", self.settle_seconds),
            ("group_settle_seconds", self.group_settle_seconds),
        ] {
            if seconds.is_some_and(|seconds| !CONDITION_SETTLE_SECONDS_RANGE.contains(&seconds)) {
                return invalid(
                    key,
                    format!(
                        "must be between {} and {}",
                        CONDITION_SETTLE_SECONDS_RANGE.start(),
                        CONDITION_SETTLE_SECONDS_RANGE.end()
                    ),
                );
            }
        }
        if self.group_settle_seconds.is_some() && self.group_by != Some(ConditionGroupBy::Stem) {
            return invalid("group_settle_seconds", "requires group_by: stem".into());
        }
        if !self.scope.is_available() {
            return invalid(
                "scope",
                format!(
                    "scope_unavailable: scope '{}' is not available in this Orbit; only task and \
                     workspace conditions can be checked",
                    self.scope.as_str()
                ),
            );
        }
        Ok(())
    }

    /// The first file-scope-only key this block sets, if any.
    fn file_scope_key(&self) -> Option<&'static str> {
        [
            ("roots", !self.roots.is_empty()),
            ("glob", self.glob.is_some()),
            ("max_depth", self.max_depth.is_some()),
            ("settle_seconds", self.settle_seconds.is_some()),
            ("identity", self.identity.is_some()),
            ("hash_full_max_bytes", self.hash_full_max_bytes.is_some()),
            ("group_by", self.group_by.is_some()),
            ("group_settle_seconds", self.group_settle_seconds.is_some()),
        ]
        .into_iter()
        .find_map(|(key, set)| set.then_some(key))
    }
}

/// What a false or unknown `when:` guard does to a due cron or interval slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionGuardMiss {
    /// Leave the slot unconsumed: it fires on the first later pass where the
    /// guard is true, and the next slot collapses it.
    #[default]
    Wait,
    /// Consume the slot without firing.
    Skip,
}

impl ConditionGuardMiss {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Skip => "skip",
        }
    }
}

/// An optional `when:` guard beside a typed trigger kind (`cron`,
/// `every_minutes`, `deliveries_landed`, `state`): a CEL predicate over the
/// workspace, checked at each due moment. The kind decides when to look; the
/// guard decides whether to fire (§11).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleGuard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_miss: Option<ConditionGuardMiss>,
}

impl ScheduleGuard {
    /// Whether no guard is configured.
    pub fn is_empty(&self) -> bool {
        self.when.is_none() && self.guard_miss.is_none()
    }

    /// Shape checks. `slot_based` is true for `cron` and `every_minutes`, the
    /// only kinds `guard_miss` applies to.
    pub fn validate(&self, clause: &str, slot_based: bool) -> Result<(), WorkflowError> {
        if self
            .when
            .as_deref()
            .is_some_and(|when| when.trim().is_empty())
        {
            return Err(WorkflowError::Invalid(format!(
                "{clause}.when: must not be empty when present"
            )));
        }
        if self.guard_miss.is_some() && self.when.is_none() {
            return Err(WorkflowError::Invalid(format!(
                "{clause}.guard_miss: requires a `when` guard"
            )));
        }
        if self.guard_miss.is_some() && !slot_based {
            return Err(WorkflowError::Invalid(format!(
                "{clause}.guard_miss: applies to cron and every_minutes only; a false guard \
                 keeps delivery debt and state members"
            )));
        }
        Ok(())
    }
}
