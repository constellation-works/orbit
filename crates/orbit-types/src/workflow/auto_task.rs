//! Auto-task definition schema (v1) [ORB-10149] — a dynamically-defined
//! recurring task template.
//!
//! One YAML file under `.orbit/auto_tasks/` describes a schedule (cron or
//! interval), an `enabled` toggle, a task template, and a dedupe policy. A
//! single generic scheduler routine (orbit-core) fires the due, enabled
//! definitions and creates tasks from their templates — periodic work becomes
//! data, not bespoke code.
//!
//! Parsing is fail-closed (like [`super::routine`]): an invalid file is an
//! error, never a definition that fires with defaults. Per ADR-0217 the schema
//! is provider-neutral: the template carries crew / priority / type only —
//! there are no turn-based budget knobs anywhere in the definition.

use serde::{Deserialize, Deserializer, Serialize};

use super::error::WorkflowError;
use crate::task::{TaskPriority, TaskStatus, TaskType};

/// Auto-task YAML schema version this binary reads and writes.
pub const AUTO_TASK_SCHEMA_VERSION: u32 = 1;

/// Longest supported auto-task interval: one year in minutes. This keeps an
/// operator mistake from becoming a practically inert schedule while remaining
/// well within the scheduler's signed duration representation.
pub const MAX_AUTO_TASK_INTERVAL_MINUTES: u64 = 365 * 24 * 60;

/// Tag prefix stamped on every task an auto-task definition creates. The
/// suffix is the definition name, so `skip_if_open` dedupe and provenance
/// both key off `auto-task:<name>`.
pub const AUTO_TASK_TAG_PREFIX: &str = "auto-task:";

/// The provenance tag for a definition: `auto-task:<name>`.
pub fn auto_task_tag(name: &str) -> String {
    format!("{AUTO_TASK_TAG_PREFIX}{name}")
}

/// A parsed, validated auto-task definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskDefinition {
    /// Schema version marker (`schemaVersion: 1`).
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// Unique definition name; also the file stem (`<name>.yaml`).
    pub name: String,
    /// Human description of the recurring chore.
    #[serde(default)]
    pub description: String,
    /// Kill-switch toggle. Absent means enabled; disabling is a toggle, not a
    /// delete.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When the definition is due.
    pub schedule: AutoTaskSchedule,
    /// The task minted on each fire.
    pub template: AutoTaskTemplate,
    /// How to handle firing while a prior instance is still open.
    #[serde(default)]
    pub dedupe: DedupePolicy,
    /// Actor that created the definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// RFC 3339 creation timestamp.
    #[serde(default)]
    pub created_at: String,
    /// Actor of the last definition edit (CRUD, not a scheduler fire).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    /// RFC 3339 timestamp of the last definition edit.
    #[serde(default)]
    pub updated_at: String,
}

/// When a definition is due. Exactly one form is present per definition; the
/// scheduler's due-math (orbit-core) collapses catch-up fires either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum AutoTaskSchedule {
    /// Verified landings, independent of task completion.
    Deliveries {
        deliveries_landed: super::automation::DeliveryTrigger,
    },
    /// Standard 5-field cron expression, evaluated in host-local time.
    Cron { cron: String },
    /// Fire every N minutes, anchored at the definition's first-observed slot.
    Interval { every_minutes: u64 },
}

/// Wire representation used to preserve field-specific errors while enforcing
/// the schedule's one-form invariant.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSchedule {
    cron: Option<String>,
    every_minutes: Option<u64>,
    deliveries_landed: Option<super::automation::DeliveryTrigger>,
}

impl<'de> Deserialize<'de> for AutoTaskSchedule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawSchedule::deserialize(deserializer)?;
        let mut forms = Vec::new();

        if raw.cron.is_some() {
            forms.push("cron");
        }
        if raw.every_minutes.is_some() {
            forms.push("every_minutes");
        }
        if raw.deliveries_landed.is_some() {
            forms.push("deliveries_landed");
        }

        match (raw.cron, raw.every_minutes, raw.deliveries_landed) {
            (Some(cron), None, None) => Ok(Self::Cron { cron }),
            (None, Some(every_minutes), None) => Ok(Self::Interval { every_minutes }),
            (None, None, Some(deliveries_landed)) => Ok(Self::Deliveries { deliveries_landed }),
            _ => Err(<D::Error as serde::de::Error>::custom(format!(
                "schedule must have exactly one of cron, every_minutes, deliveries_landed; found: {}",
                if forms.is_empty() {
                    "none".to_string()
                } else {
                    forms.join(", ")
                }
            ))),
        }
    }
}

/// The task template instantiated on each fire. Provider-neutral (ADR-0217):
/// crew / priority / type only — no turn-based knobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskTemplate {
    /// Task title.
    pub title: String,
    /// Task description / instruction body.
    #[serde(default)]
    pub description: String,
    /// Acceptance criteria seeded onto the created task.
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    /// Task type (defaults to `chore`).
    #[serde(default = "default_task_type")]
    pub task_type: TaskType,
    /// Tags applied in addition to the provenance tag.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Exact canonical tools copied onto every task minted from this template.
    #[serde(
        default,
        deserialize_with = "crate::task::deserialize_required_tools",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub required_tools: Vec<String>,
    /// Priority (defaults to `medium`).
    #[serde(default = "default_priority")]
    pub priority: TaskPriority,
    /// Crew override, when the chore should route to a specific crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// Status the created task enters (defaults to `backlog`). Auto-tasks are
    /// operator-defined chores, so they skip the proposed→approved gate.
    #[serde(default = "default_status")]
    pub status: TaskStatus,
}

/// Dedupe policy for firing while a prior instance created by this definition
/// is still open.
///
/// The canonical spelling — on the wire, on disk, and in `show` — is
/// `snake_case` (`skip_if_open`, `always`), matching every other persisted
/// auto-task field. The CLI's `--dedupe` flag additionally accepts the
/// `clap`-default kebab-case spelling (`skip-if-open`) as an alias, since that
/// is what `--help` advertises as the possible values; it still normalizes to
/// the snake_case token before anything is persisted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum DedupePolicy {
    /// Skip the fire while a previously-created instance is still open, so a
    /// stalled backlog never accumulates identical tasks (the default).
    #[default]
    #[cfg_attr(feature = "clap", value(alias = "skip_if_open"))]
    SkipIfOpen,
    /// Always fire, even if a prior instance is still open.
    Always,
}

impl DedupePolicy {
    /// The canonical `snake_case` token, matching the persisted/`show` form.
    pub fn as_str(self) -> &'static str {
        match self {
            DedupePolicy::SkipIfOpen => "skip_if_open",
            DedupePolicy::Always => "always",
        }
    }
}

impl std::fmt::Display for DedupePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

const fn default_true() -> bool {
    true
}

const fn default_task_type() -> TaskType {
    TaskType::Chore
}

const fn default_priority() -> TaskPriority {
    TaskPriority::Medium
}

const fn default_status() -> TaskStatus {
    TaskStatus::Backlog
}

impl AutoTaskDefinition {
    /// Semantic checks beyond serde shape: name charset, non-empty schedule,
    /// non-empty template title. Cron parsing itself happens in the scheduler
    /// (orbit-core), which owns the cron dependency — mirroring routines.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if !is_valid_auto_task_name(&self.name) {
            return Err(WorkflowError::Invalid(format!(
                "auto-task name '{}' must be non-empty, lowercase alphanumeric \
                 with '-' or '_' separators, and start alphanumeric",
                self.name
            )));
        }
        match &self.schedule {
            AutoTaskSchedule::Deliveries { deliveries_landed } => deliveries_landed.validate()?,
            AutoTaskSchedule::Cron { cron } if cron.trim().is_empty() => {
                return Err(WorkflowError::Invalid(format!(
                    "auto-task '{}' schedule.cron must not be empty",
                    self.name
                )));
            }
            AutoTaskSchedule::Interval { every_minutes }
                if *every_minutes == 0 || *every_minutes > MAX_AUTO_TASK_INTERVAL_MINUTES =>
            {
                return Err(WorkflowError::Invalid(format!(
                    "auto-task '{}' schedule.every_minutes must be between 1 and {MAX_AUTO_TASK_INTERVAL_MINUTES}",
                    self.name
                )));
            }
            _ => {}
        }
        if self.template.title.trim().is_empty() {
            return Err(WorkflowError::Invalid(format!(
                "auto-task '{}' template.title must not be empty",
                self.name
            )));
        }
        Ok(())
    }
}

/// Definition names share the routine name charset: lowercase alphanumeric
/// plus `-`/`_`, starting alphanumeric. The name is the file stem and the
/// provenance-tag suffix, so it must be filesystem- and tag-safe.
pub fn is_valid_auto_task_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}
