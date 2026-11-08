//! `completion.hold`: workspace rules that keep a completion-authorized
//! (`--complete`) run's task in `review` instead of `done`.
//!
//! A rule can only lower a `done` completion to `review`; it never grants
//! completion, which stays an explicit per-invocation authorization
//! ([`super::CompletionPolicy`]).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::task::{TaskComplexity, TaskDecision, TaskHistoryEntry, TaskStatus, TaskType};

/// The dotted config path of the rule list.
pub const COMPLETION_HOLD_KEY: &str = "completion.hold";

/// History event recording that a rule held a completion-authorized task in
/// `review`. Its decision is [`TaskDecision::CompletionHeld`].
pub const COMPLETION_HELD_EVENT: &str = "completion_held";

/// One `[[completion.hold]]` rule as written in `config.toml`.
///
/// The rule holds a task when any one of its fields matches: `paths` against
/// the files the pull request changes, `complexity_at_least` against the
/// task's assessed complexity, `types` against its type and `tags` against its
/// tags. A rule must set at least one of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionHoldRule {
    /// Optional label naming the rule in the hold record; defaults to its
    /// 1-based position (`completion.hold[2]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Path globs matched against the pull request's changed files. A
    /// `!`-prefixed glob excludes what it matches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Hold tasks assessed at this complexity or above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity_at_least: Option<TaskComplexity>,
    /// Hold tasks of these types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<TaskType>,
    /// Hold tasks carrying any of these tags (compared case-insensitively).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl CompletionHoldRule {
    /// The label a hold record names this rule by: its `name`, else its
    /// 1-based position in the list.
    pub fn label(&self, index: usize) -> String {
        match self.name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => format!("{COMPLETION_HOLD_KEY}[{}]", index + 1),
        }
    }

    /// Whether the rule sets no field it could match on.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
            && self.complexity_at_least.is_none()
            && self.types.is_empty()
            && self.tags.is_empty()
    }
}

/// The rank of an assessed complexity on the `low < medium < hard < xhard`
/// scale; `None` for `unassessed`.
pub fn completion_hold_complexity_rank(complexity: TaskComplexity) -> Option<u8> {
    match complexity {
        TaskComplexity::Low => Some(0),
        TaskComplexity::Medium => Some(1),
        TaskComplexity::Hard => Some(2),
        TaskComplexity::XHard => Some(3),
        TaskComplexity::Unassessed => None,
    }
}

/// Which field of a rule matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionHoldField {
    Paths,
    ComplexityAtLeast,
    Types,
    Tags,
}

impl CompletionHoldField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paths => "paths",
            Self::ComplexityAtLeast => "complexity_at_least",
            Self::Types => "types",
            Self::Tags => "tags",
        }
    }
}

/// Why a completion-authorized run left a task in `review`: the rule, the
/// field that matched, the configured value and what it matched. Recorded on
/// the [`COMPLETION_HELD_EVENT`] history entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionHold {
    /// The rule's label ([`CompletionHoldRule::label`]).
    pub rule: String,
    /// The field that matched.
    pub field: CompletionHoldField,
    /// The configured value that matched: a path glob, a complexity, a type
    /// or a tag.
    pub pattern: String,
    /// What it matched: a changed file, or the task's complexity, type or
    /// tag. A path rule whose changed files could not be read holds with the
    /// reason here.
    pub matched: String,
    /// The task whose complexity, type or tags matched, when the bundle that
    /// held names another task than the one this record is on; otherwise
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_task_id: Option<String>,
    /// The run that held the completion.
    pub run_id: String,
}

impl CompletionHold {
    /// One line naming the rule and its match, as the comment and history
    /// note carry it.
    pub fn summary(&self) -> String {
        let task = self
            .matched_task_id
            .as_deref()
            .map(|id| format!(" (bundled task {id})"))
            .unwrap_or_default();
        format!(
            "held for review: {} {} matched {}{task} (rule {})",
            self.field.as_str(),
            self.pattern,
            self.matched,
            self.rule
        )
    }
}

impl fmt::Display for CompletionHold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary())
    }
}

/// The hold the task is in now: the latest [`COMPLETION_HELD_EVENT`] record,
/// while the task is still in `review` and has not entered it again since.
pub fn current_completion_hold(
    status: TaskStatus,
    history: &[TaskHistoryEntry],
) -> Option<&CompletionHold> {
    if status != TaskStatus::Review {
        return None;
    }
    let entered_review = history
        .iter()
        .rposition(|entry| entry.to_status == Some(TaskStatus::Review));
    history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, entry)| match &entry.decision {
            Some(TaskDecision::CompletionHeld(hold))
                if entry.event == COMPLETION_HELD_EVENT
                    && entered_review.is_none_or(|entered| index > entered) =>
            {
                Some(hold)
            }
            _ => None,
        })
}
