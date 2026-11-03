//! Effective decision-request states folded from task history.

use serde::{Deserialize, Serialize};

use crate::task::{Task, TaskHistoryEntry, TaskStatus};
use crate::workflow::resident_authority_revision;

use super::binding::{self, DecisionBinding};
use super::gates::DecisionGate;
use super::model::{
    DECISION_ANSWERED_EVENT, DECISION_MODEL_CONFIRMED_EVENT, DECISION_REOPENED_EVENT,
    DecisionRequest, DecisionRequestState, DecisionResolution, MODEL_ANSWER_ACTOR,
    ModelAnswerConfirmation, PRECEDENT_ACTOR,
};

/// Note body of a [`DECISION_REOPENED_EVENT`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DecisionReopenNote {
    request_id: String,
}

/// Every decision request in `history`, oldest first, with its effective
/// state: a request is `answered` when the latest of a `decision_answered`
/// entry and a later [`DECISION_REOPENED_EVENT`] for the same request is the
/// answer (or its own resolution records one), `expired` once its expiry
/// passes, and `superseded` when its binding is no longer current or a newer
/// request for the same task follows it. At most one request per task reads
/// as `open`, besides a [`DecisionGate::ModelAnswerReview`] request, which is
/// current while the model answer it reviews stands and never supersedes or
/// is superseded by another request. A [`DECISION_MODEL_CONFIRMED_EVENT`]
/// written by a human sets the reviewed answer's derived `confirmation`. An
/// approval ([`DecisionBinding::TaskRevision`]) is superseded
/// for good by a description or criteria edit, status move or, for a review
/// approval, a candidate change after its raise, so restoring the earlier
/// state does not reopen it. A precedent answer's bookkeeping status moves
/// ([`is_precedent_bookkeeping`](binding::is_precedent_bookkeeping)) are not that status move: reversing the
/// answer with [`DECISION_REOPENED_EVENT`] and restoring the prior status
/// makes the request readable as open again.
///
/// Supply the current task and its history. `pilot_operation` is the latest
/// assessment operation id only while its admission hold still applies;
/// pass `None` when the hold is cleared. Block episodes use the latest move
/// to `blocked`, with the same nanosecond timestamp key as block triage.
pub fn decision_requests_from_history(
    task: &Task,
    history: &[TaskHistoryEntry],
    pilot_operation: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<DecisionRequest> {
    // Oldest first. A later answer clears a reopen; a later reopen clears the
    // answer. Anything else leaves the latest of the two in place.
    let mut folded: std::collections::BTreeMap<String, (DecisionResolution, bool)> =
        std::collections::BTreeMap::new();
    for entry in history {
        if entry.event == DECISION_ANSWERED_EVENT {
            let Some(note) = entry.note.as_deref() else {
                continue;
            };
            let Ok(resolution) = serde_json::from_str::<DecisionResolution>(note) else {
                continue;
            };
            let request_id = if resolution.request_id.is_empty() {
                let Some(operation_id) = entry.operation_id.clone() else {
                    continue;
                };
                operation_id
            } else {
                resolution.request_id.clone()
            };
            folded.insert(request_id, (resolution, false));
        } else if entry.event == DECISION_REOPENED_EVENT {
            let Some(request_id) = entry.note.as_deref().and_then(|note| {
                serde_json::from_str::<DecisionReopenNote>(note)
                    .ok()
                    .map(|reopen| reopen.request_id)
                    .filter(|request_id| !request_id.is_empty())
            }) else {
                continue;
            };
            if let Some(state) = folded.get_mut(&request_id) {
                state.1 = true;
            }
        } else if entry.event == DECISION_MODEL_CONFIRMED_EVENT {
            let Some(confirmation) = entry
                .note
                .as_deref()
                .and_then(|note| serde_json::from_str::<ModelAnswerConfirmation>(note).ok())
            else {
                continue;
            };
            if confirmation.confirmed_by != entry.by
                || crate::identity::is_automation_actor(&entry.by)
                || entry.by.trim() == PRECEDENT_ACTOR
            {
                continue;
            }
            if let Some((resolution, false)) = folded.get_mut(&confirmation.request_id)
                && resolution.answered_by == MODEL_ANSWER_ACTOR
                && resolution.answered_at == confirmation.answered_at
            {
                resolution.confirmation = Some(confirmation);
            }
        }
    }
    // A review request is current while the model answer it reviews stands:
    // answered by the model at the bound time and not re-opened since.
    let standing_model_answer = |request_id: &str, answered_at: chrono::DateTime<chrono::Utc>| {
        folded
            .get(request_id)
            .is_some_and(|(resolution, reopened)| {
                !reopened
                    && resolution.answered_by == MODEL_ANSWER_ACTOR
                    && resolution.answered_at == answered_at
            })
    };
    let block_episode = (task.status == TaskStatus::Blocked)
        .then(|| {
            history
                .iter()
                .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
        })
        .flatten()
        .map(|entry| entry.at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true));
    let current_candidate = binding::current_candidate_identity(history);
    let authority_revision = resident_authority_revision(history);
    let binding_current = |request: &DecisionRequest| {
        request.task_id == task.id
            && match &request.binding {
                DecisionBinding::BlockEpisode { episode } => {
                    block_episode.as_ref() == Some(episode)
                }
                DecisionBinding::PilotOperation { operation_id } => {
                    task.status == TaskStatus::Backlog
                        && pilot_operation == Some(operation_id.as_str())
                }
                DecisionBinding::TaskRevision {
                    revision,
                    status,
                    candidate,
                } => {
                    *status == task.status
                        && (revision == &task.spec_digest()
                            || chrono::DateTime::parse_from_rfc3339(revision)
                                .is_ok_and(|revision| revision == task.updated_at))
                        && (!binding::binds_candidate(*status)
                            || candidate.as_deref() == current_candidate.as_deref())
                }
                DecisionBinding::ModelAnswer {
                    request_id,
                    answered_at,
                } => standing_model_answer(request_id, *answered_at),
                // A grant request lapses with any authority change and with
                // the mandate's end.
                DecisionBinding::ResidentAuthority { revision } => {
                    *revision == authority_revision
                        && !matches!(
                            task.status,
                            TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
                        )
                }
                // Current while the holder task stays open; a newer script
                // grant request on it supersedes this one (below), and the
                // answer path checks the script's live digest.
                DecisionBinding::ScriptGrant { .. } => !matches!(
                    task.status,
                    TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
                ),
            }
    };
    let mut requests: Vec<DecisionRequest> = history
        .iter()
        .filter_map(DecisionRequest::from_entry)
        .collect();
    // A model-answer review rides beside the request it reviews, and a
    // resident grant request beside the assignment's other requests: neither
    // supersedes an older request on the task nor is superseded by a newer
    // one; only its own binding retires it.
    let rides_beside = |gate: DecisionGate| {
        matches!(
            gate,
            DecisionGate::ModelAnswerReview
                | DecisionGate::ResidentAuthority
                | DecisionGate::ScriptGrant
        )
    };
    // A script grant request is superseded by any later script grant request
    // on the same holder task: the script's bytes or requests moved on.
    let script_grant_keys: Vec<Option<String>> = requests
        .iter()
        .map(|request| (request.gate == DecisionGate::ScriptGrant).then(|| request.binding.key()))
        .collect();
    let task_ids: Vec<Option<String>> = requests
        .iter()
        .map(|request| (!rides_beside(request.gate)).then(|| request.task_id.clone()))
        .collect();
    for (index, request) in requests.iter_mut().enumerate() {
        if let Some((resolution, reopened)) = folded.get(&request.id) {
            if *reopened {
                request.resolution = None;
            } else {
                request.state = DecisionRequestState::Answered;
                request.resolution = Some(resolution.clone());
                continue;
            }
        } else if request.resolution.is_some() || request.state == DecisionRequestState::Answered {
            request.state = DecisionRequestState::Answered;
            continue;
        }
        if !binding_current(request)
            || binding::task_revision_changed_since(request, history)
            || (!rides_beside(request.gate)
                && task_ids
                    .iter()
                    .skip(index + 1)
                    .flatten()
                    .any(|task_id| task_id == &request.task_id))
            || (request.gate == DecisionGate::ScriptGrant
                && script_grant_keys
                    .iter()
                    .skip(index + 1)
                    .flatten()
                    .any(|key| *key != request.binding.key()))
        {
            request.state = DecisionRequestState::Superseded;
        } else if now >= request.expires_at {
            request.state = DecisionRequestState::Expired;
        } else {
            request.state = DecisionRequestState::Open;
        }
    }
    requests
}

/// The open decision requests in `task`'s history, oldest first.
/// Current binding inputs follow [`decision_requests_from_history`].
pub fn open_decision_requests(
    task: &Task,
    history: &[TaskHistoryEntry],
    pilot_operation: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<DecisionRequest> {
    decision_requests_from_history(task, history, pilot_operation, now)
        .into_iter()
        .filter(|request| request.state == DecisionRequestState::Open)
        .collect()
}
