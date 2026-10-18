//! Provenance for selectors Orbit appends when an agent changes a path its
//! task's selectors did not cover.
//!
//! Implementers, recovery agents and reviewers may change any path the work
//! requires. Footprint locks stay a scheduling hint; delivery widens the
//! task's `context_files` instead of refusing, and each widening appends one
//! [`CONTEXT_FILES_WIDENED_EVENT`] history entry whose decision is a
//! [`ContextFilesWidening`] naming the step that introduced the paths. Its note
//! is the same record as JSON, for people and older binaries.

use serde::{Deserialize, Serialize};

use super::{TaskDecision, TaskHistoryEntry};

/// History event recording selectors appended for agent-changed paths.
pub const CONTEXT_FILES_WIDENED_EVENT: &str = "context_files_widened";

/// The kind of agent step whose change a widening covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextWideningStep {
    /// The task's implementer.
    Implement,
    /// A step-failure, conflict or final recovery agent.
    Recovery,
    /// The before-PR reviewer.
    Review,
}

impl ContextWideningStep {
    /// The agent step whose changed paths widen its task's selectors as the
    /// agent exits, or `None`. The reviewer's changes widen at review
    /// settlement, and a conflict recovery's when the host continues its
    /// rebase; both own the commit their changes land in.
    pub fn for_activity(activity: &str) -> Option<Self> {
        match activity {
            "agent_implement" => Some(Self::Implement),
            "step_failure_recovery" | "final_recovery" => Some(Self::Recovery),
            _ => None,
        }
    }

    /// The stable name recorded in history notes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implement => "implement",
            Self::Recovery => "recovery",
            Self::Review => "review",
        }
    }
}

/// The decision of one [`CONTEXT_FILES_WIDENED_EVENT`] history entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextFilesWidening {
    /// The run whose agent changed the paths.
    pub run_id: String,
    /// The agent step that introduced them.
    pub step: ContextWideningStep,
    /// The activity (or delivery step) that observed them.
    pub activity: String,
    /// The exact `file:` selectors appended, one per path.
    pub selectors: Vec<String>,
}

impl ContextFilesWidening {
    /// The widening a history entry records: its decision, or for an entry
    /// written before typed decisions, its note.
    #[must_use]
    pub fn from_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        match &entry.decision {
            Some(TaskDecision::ContextFilesWidened(widening)) => Some(widening.clone()),
            Some(_) => None,
            None => Self::from_legacy_note(entry.note.as_deref()?),
        }
    }

    /// Legacy reader: a note written before typed decisions is the widening's
    /// JSON; `None` for a note of another shape.
    fn from_legacy_note(note: &str) -> Option<Self> {
        serde_json::from_str(note).ok()
    }
}
