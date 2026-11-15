//! Request bindings and candidate/revision invalidation.

use serde::{Deserialize, Serialize};

use crate::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, DESCRIPTION_DOCUMENT, TASK_FIELDS_UPDATED_EVENT, Task,
    TaskHistoryEntry, TaskStatus, task_fields_updated_documents,
};
use crate::workflow::{HeldCandidate, ReviewGateHistoryNote};

use super::model::{DecisionRequest, PRECEDENT_BOOKKEEPING_NOTE};

/// Stale-guard binding of a [`DecisionRequest`]: the episode, operation or
/// revision the request was raised against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionBinding {
    /// Block episode key (the `episode=` a triage marker records).
    BlockEpisode {
        /// The episode key.
        episode: String,
    },
    /// Pilot `operation_id` of the holding assessment.
    PilotOperation {
        /// The pilot receipt operation id.
        operation_id: String,
    },
    /// Task revision and status an approval request was raised against.
    TaskRevision {
        /// The task spec digest (description and acceptance criteria, see
        /// [`Task::spec_digest`]) when the request was raised. Records
        /// written before the digest form carry the task `updated_at`
        /// instead; both spellings are compared on read.
        revision: String,
        /// The task status when the request was raised.
        status: TaskStatus,
        /// Identity of the candidate under review when a
        /// `review_approval` request was raised (design §5): the latest
        /// candidate the task history names, when it names one. Absent
        /// on `proposed_approval` requests and on records written before
        /// the field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        candidate: Option<String>,
    },
    /// The model answer a [`DecisionGate::ModelAnswerReview`](super::gates::DecisionGate::ModelAnswerReview) request
    /// reviews: the original request and the time the model answered it.
    /// Current while that answer stands; a veto, reversal or later answer
    /// supersedes the review.
    ModelAnswer {
        /// The model-answered request.
        request_id: String,
        /// `answered_at` of the model's resolution.
        answered_at: chrono::DateTime<chrono::Utc>,
    },
    /// A resident grant request: current while the mandate's authority is
    /// still at `revision`, so any authority change supersedes it.
    ResidentAuthority {
        /// The authority revision the request was raised against.
        revision: u64,
    },
    /// A script grant request: one script at one content digest with one
    /// requested capability set. A later script grant request on the same
    /// holder task (a new digest or capability set) supersedes it, and the
    /// answer path refuses it once the script's current digest differs.
    ScriptGrant {
        /// The script name.
        script: String,
        /// The content digest the answer grants.
        digest: String,
        /// [`crate::workflow::activity_job::ScriptGrantCapabilities::key`]
        /// of the requested set.
        capabilities: String,
    },
}

impl DecisionBinding {
    /// The approval binding for `task` as it now stands: its spec digest
    /// and status, plus the candidate under review for a review task.
    /// The spec digest, not `updated_at`, is the revision: appending the
    /// raise itself to the task history moves `updated_at`, which would
    /// supersede every approval request the moment it was raised, while
    /// the digest changes exactly when the description or acceptance
    /// criteria the digest rendered change (design §5).
    pub fn for_task(task: &Task, candidate: Option<String>) -> Self {
        Self::TaskRevision {
            revision: task.spec_digest(),
            status: task.status,
            candidate,
        }
    }

    /// The value a newer binding is compared against for supersession.
    pub fn key(&self) -> String {
        match self {
            Self::BlockEpisode { episode } => format!("block_episode:{episode}"),
            Self::PilotOperation { operation_id } => {
                format!("pilot_operation:{operation_id}")
            }
            Self::TaskRevision {
                revision,
                status,
                candidate: None,
            } => {
                format!("task_revision:{revision}:{status}")
            }
            Self::TaskRevision {
                revision,
                status,
                candidate: Some(candidate),
            } => {
                format!("task_revision:{revision}:{status}:{candidate}")
            }
            Self::ModelAnswer {
                request_id,
                answered_at,
            } => format!(
                "model_answer:{request_id}:{}",
                answered_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
            ),
            Self::ResidentAuthority { revision } => format!("resident_authority:{revision}"),
            Self::ScriptGrant {
                script,
                digest,
                capabilities,
            } => format!("script_grant:{script}:{digest}:{capabilities}"),
        }
    }
}

/// The candidate a task's history currently names: the head of the
/// latest held candidate, or the candidate the latest review-gate note
/// names, whichever entry is newest. `None` when the history names no
/// candidate, as for a review task whose candidate lives only in its
/// pull request.
pub fn current_candidate_identity(history: &[TaskHistoryEntry]) -> Option<String> {
    history.iter().rev().find_map(entry_candidate_identity)
}

/// The candidate one history entry names, if it names one.
fn entry_candidate_identity(entry: &TaskHistoryEntry) -> Option<String> {
    if let Some(held) = HeldCandidate::from_entry(entry) {
        return Some(held.head_sha);
    }
    entry
        .note
        .as_deref()
        .and_then(ReviewGateHistoryNote::from_text)
        .and_then(|note| note.candidate)
        .filter(|candidate| !candidate.is_empty())
}

/// Whether an approval raised in `status` binds to the candidate under review.
/// Only `review` approvals do, matching the raise: a `proposed` approval is
/// raised without a candidate, so a candidate that history carries from an
/// earlier review must not read as a change to it.
pub(super) fn binds_candidate(status: TaskStatus) -> bool {
    status == TaskStatus::Review
}

/// Whether `history` records a change after `request` was raised that
/// permanently invalidates its [`DecisionBinding::TaskRevision`]: an edit to
/// the description or acceptance criteria, a status move, or, for a review
/// approval, a different candidate. Superseded is terminal, so restoring the
/// earlier text, status or candidate does not revive the request. The
/// request's own raise and bookkeeping entries (comments, plan or summary
/// writes) change none of these.
pub(super) fn task_revision_changed_since(
    request: &DecisionRequest,
    history: &[TaskHistoryEntry],
) -> bool {
    let DecisionBinding::TaskRevision {
        status, candidate, ..
    } = &request.binding
    else {
        return false;
    };
    let binds_candidate = binds_candidate(*status);
    history
        .iter()
        .filter(|entry| entry.at > request.raised_at)
        .filter(|entry| !is_precedent_bookkeeping(entry))
        .any(|entry| {
            entry.to_status.is_some()
                || (entry.event == TASK_FIELDS_UPDATED_EVENT
                    && entry.note.as_deref().is_some_and(|note| {
                        task_fields_updated_documents(note).iter().any(|document| {
                            *document == DESCRIPTION_DOCUMENT
                                || *document == ACCEPTANCE_CRITERIA_DOCUMENT
                        })
                    }))
                || (binds_candidate
                    && entry_candidate_identity(entry)
                        .is_some_and(|identity| candidate.as_deref() != Some(identity.as_str())))
        })
}

/// Whether a history entry is a precedent answer's bookkeeping status move
/// or the matching reversal. Other status moves still supersede a request.
pub fn is_precedent_bookkeeping(entry: &TaskHistoryEntry) -> bool {
    entry.note.as_deref().is_some_and(|note| {
        note == PRECEDENT_BOOKKEEPING_NOTE
            || note.starts_with(&format!("{PRECEDENT_BOOKKEEPING_NOTE}:"))
    })
}
