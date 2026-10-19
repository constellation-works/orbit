//! The task revision a final-recovery decision is checked against.

use chrono::{DateTime, Utc};
use orbit_types::identity::MACHINE_ID_PREFIX;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep, Task, TaskComment,
    TaskHistoryEntry, TaskStatus,
};
use serde_json::json;

use super::super::helpers::SYSTEM_ACTOR_LABEL;

/// The task as it stood when the failure was recorded.
///
/// A later lifecycle change — a transition, an operator's or orchestrator's
/// comment, or an edit to what the task asks, who runs it, or what it
/// relates to — changes this, and the applier then refuses the decision
/// rather than overwrite a human's call with an agent's. Writes recovery is
/// allowed to cause do not count: a friction it records (on a claimed task,
/// the bridge's `claim_friction` history entry), the selectors Orbit widens
/// over a recovery agent's repair after the failure, comments Orbit writes
/// for a run, and run bookkeeping such as the execution summary, run link,
/// external refs or artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalRecoveryTaskRevision {
    /// Status at the failure.
    pub status: TaskStatus,
    /// Last write at the failure.
    pub updated_at: DateTime<Utc>,
    /// Digest of the task's lifecycle content at the failure. `None` on a
    /// revision recorded before the digest existed, which compares by
    /// `updated_at` instead.
    pub lifecycle_digest: Option<String>,
}

impl FinalRecoveryTaskRevision {
    /// The revision `task` is at now, given its comments and history: what a
    /// failure observes.
    pub fn of(task: &Task, comments: &[TaskComment], history: &[TaskHistoryEntry]) -> Self {
        Self::measured(task, comments, history, None)
    }

    /// The revision `task` is at now, measured against `observed`: the
    /// selectors a recovery widened after `observed` are set aside while they
    /// still stand where the widening appended them. Selectors an earlier
    /// recovery widened count like any other.
    pub fn after(
        observed: &Self,
        task: &Task,
        comments: &[TaskComment],
        history: &[TaskHistoryEntry],
    ) -> Self {
        Self::measured(task, comments, history, Some(observed.updated_at))
    }

    fn measured(
        task: &Task,
        comments: &[TaskComment],
        history: &[TaskHistoryEntry],
        observed_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            status: task.status,
            updated_at: task.updated_at,
            lifecycle_digest: Some(lifecycle_digest(task, comments, history, observed_at)),
        }
    }

    /// Whether this revision differs from `observed` by a lifecycle change.
    pub fn changed_since(&self, observed: &Self) -> bool {
        if self.status != observed.status {
            return true;
        }
        match (&observed.lifecycle_digest, &self.lifecycle_digest) {
            (Some(observed), Some(current)) => observed != current,
            _ => self.updated_at != observed.updated_at,
        }
    }
}

/// Digest of what a lifecycle change alters: the task's content, scope,
/// crew and relations, how many transitions its history holds, and every
/// comment not written by Orbit for a run.
///
/// With `observed_at`, the selectors recovery widenings appended after it
/// are left out when they still end the task's selectors in the order they
/// were appended; they record the current recovery's own repair. Selectors
/// edited after such a widening digest differently from any unedited list,
/// so an edit to them still counts.
fn lifecycle_digest(
    task: &Task,
    comments: &[TaskComment],
    history: &[TaskHistoryEntry],
    observed_at: Option<DateTime<Utc>>,
) -> String {
    use orbit_types::identity::lower_hex;
    use sha2::{Digest, Sha256};
    let widened_since = observed_at.map_or_else(Vec::new, |observed_at| {
        history
            .iter()
            .filter(|entry| entry.event == CONTEXT_FILES_WIDENED_EVENT && entry.at > observed_at)
            .filter_map(ContextFilesWidening::from_entry)
            .filter(|widening| widening.step == ContextWideningStep::Recovery)
            .flat_map(|widening| widening.selectors)
            .collect::<Vec<_>>()
    });
    let context_files = match task.context_files.strip_suffix(widened_since.as_slice()) {
        Some(before) => json!(before),
        None => json!({ "edited_after_recovery_widening": task.context_files }),
    };
    let content = json!({
        "title": task.title,
        "description": task.description,
        "acceptance_criteria": task.acceptance_criteria,
        "plan": task.plan,
        "tags": task.tags,
        "required_tools": task.required_tools,
        "context_files": context_files,
        "priority": task.priority,
        "complexity": task.complexity,
        "task_type": task.task_type,
        "relations": task.relations,
        "crew": task.crew,
        "orchestrator": task.orchestrator,
        "transitions": history.iter().filter(|entry| entry.to_status.is_some()).count(),
        "comments": comments
            .iter()
            .filter(|comment| !written_for_a_run(&comment.by))
            .map(|comment| json!([comment.at, comment.by, comment.message]))
            .collect::<Vec<_>>(),
    });
    lower_hex(Sha256::digest(content.to_string().as_bytes()))
}

/// Whether `by` is Orbit writing for a run — locally, or through a claimed
/// worker's bridge under its machine id — rather than an operator or agent
/// making a call about the task.
fn written_for_a_run(by: &str) -> bool {
    let by = by.trim();
    by == SYSTEM_ACTOR_LABEL || by.starts_with(MACHINE_ID_PREFIX)
}
