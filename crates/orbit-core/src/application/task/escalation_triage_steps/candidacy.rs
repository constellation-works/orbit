//! Pilot-warning candidacy and assessment settlement.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskComment, TaskDecision, TaskHistoryEntry};
use serde_json::Value;

use super::super::helpers::{SYSTEM_ACTOR_LABEL, is_automation_actor};
use super::super::pilot::{HeldPilotWarnings, held_pilot_warnings};
use crate::OrbitRuntime;

use super::PILOT_WARNING_TRIAGED_EVENT;

/// Whether a task's pilot warning hold awaits triage, and why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PilotWarningCandidacy {
    /// The assessment holds and awaits triage.
    Qualifies(HeldPilotWarnings),
    /// The task is not `proposed`, or its latest assessment holds nothing on
    /// warnings.
    NotHeld,
    /// An operator recorded a task-pilot admission decision on the assessment.
    OperatorDecided {
        /// Who decided.
        by: String,
    },
    /// The lane already recorded a decision on the assessment.
    Triaged {
        /// When it did.
        at: DateTime<Utc>,
    },
}

impl PilotWarningCandidacy {
    /// Stable token for the candidacy.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Qualifies(_) => "qualifies",
            Self::NotHeld => "not_held",
            Self::OperatorDecided { .. } => "operator_decided",
            Self::Triaged { .. } => "triaged",
        }
    }

    /// Human-readable detail for the candidacy, naming the assessment,
    /// decision or triage marker behind it.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Qualifies(held) => format!(
                "assessment {} holds on {} warning finding(s)",
                held.operation_id,
                held.findings.len()
            ),
            Self::NotHeld => {
                "the task is not proposed, or its latest task-pilot assessment holds nothing on \
                 warnings"
                    .to_string()
            }
            Self::OperatorDecided { by } => {
                format!("{by} recorded a task-pilot admission decision on the latest assessment")
            }
            Self::Triaged { at } => format!(
                "the latest assessment was already triaged at {}",
                at.to_rfc3339()
            ),
        }
    }
}

/// Classify `task`'s latest pilot assessment for triage. It qualifies while
/// [`held_pilot_warnings`] reports holding findings, no trusted operator
/// decision answers that assessment, and no marker records it triaged.
pub(crate) fn pilot_warning_candidacy(
    task: &Task,
    comments: &[TaskComment],
    history: &[TaskHistoryEntry],
    is_done: &dyn Fn(&str) -> bool,
) -> PilotWarningCandidacy {
    let Some(held) = held_pilot_warnings(task.status, comments, &task.dependencies(), is_done)
    else {
        return PilotWarningCandidacy::NotHeld;
    };
    if let Some(at) = triaged_at(history, &held.operation_id) {
        return PilotWarningCandidacy::Triaged { at };
    }
    // Only a person decides admission: an agent-authored decision, like an
    // agent's `task-pilot-admission:` comment, is never one.
    if let Some(entry) = history.iter().find(|entry| {
        !entry.by.trim().is_empty()
            && !is_automation_actor(&entry.by)
            && matches!(
                &entry.decision,
                Some(TaskDecision::PilotAdmission { operation_id, .. })
                    if *operation_id == held.operation_id
            )
    }) {
        return PilotWarningCandidacy::OperatorDecided {
            by: entry.by.clone(),
        };
    }
    PilotWarningCandidacy::Qualifies(held)
}

/// When the lane's marker for the assessment `operation_id` was recorded.
pub(crate) fn triaged_at(
    history: &[TaskHistoryEntry],
    operation_id: &str,
) -> Option<DateTime<Utc>> {
    history
        .iter()
        .find(|entry| {
            entry.event == PILOT_WARNING_TRIAGED_EVENT
                && entry.by == SYSTEM_ACTOR_LABEL
                && entry
                    .note
                    .as_deref()
                    .and_then(|note| serde_json::from_str::<Value>(note).ok())
                    .is_some_and(|note| note["operation_id"] == operation_id)
        })
        .map(|entry| entry.at)
}

impl OrbitRuntime {
    /// `task`'s pilot-warning candidacy, from its current comments and history.
    pub(crate) fn pilot_warning_candidacy_of(
        &self,
        task: &Task,
    ) -> Result<PilotWarningCandidacy, OrbitError> {
        let comments = self.get_task_pilot_comments(&task.id)?;
        let history = self.get_task_history(&task.id)?;
        Ok(pilot_warning_candidacy(
            task,
            &comments,
            &history,
            &|id: &str| self.task_is_done(id),
        ))
    }

    fn task_is_done(&self, id: &str) -> bool {
        self.warning_prerequisite_is_done(id)
    }
}
