//! Decision request, binding and resolution lifecycle.

use serde::{Deserialize, Serialize};

use crate::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, DESCRIPTION_DOCUMENT, TASK_FIELDS_UPDATED_EVENT, Task,
    TaskHistoryEntry, TaskStatus, task_fields_updated_documents,
};
use crate::workflow::{HeldCandidate, ReviewGateHistoryNote};

/// History event that raises a [`DecisionRequest`].
pub const DECISION_REQUESTED_EVENT: &str = "decision_requested";
/// History event that records an applied answer to a [`DecisionRequest`].
pub const DECISION_ANSWERED_EVENT: &str = "decision_answered";
/// History event that records a decision token consumed by a refused
/// submission that had already authenticated (design
/// `docs/design/human-decision-channel/2_design.md` §2.3). The note names
/// the request and the token's SHA-256 fingerprint, never the token.
pub const DECISION_TOKEN_CONSUMED_EVENT: &str = "decision_token_consumed";
/// Default lifetime of a decision request, in hours.
pub const DEFAULT_DECISION_REQUEST_TTL_HOURS: i64 = 72;
/// How close to expiry an open request must be for the decision digest
/// to return it as nearing expiry, in hours.
pub const DECISION_DIGEST_EXPIRY_WINDOW_HOURS: i64 = 24;

/// The gate a [`DecisionRequest`] waits on.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionGate {
    /// A blocked-task triage proposal.
    BlockTriage,
    /// A task-pilot admission hold.
    PilotAdmission,
    /// Approval of a proposed task.
    ProposedApproval,
    /// Approval of a task in review.
    ReviewApproval,
}

impl DecisionGate {
    /// The gate a stable gate name denotes, if it denotes one.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "block_triage" => Some(Self::BlockTriage),
            "pilot_admission" => Some(Self::PilotAdmission),
            "proposed_approval" => Some(Self::ProposedApproval),
            "review_approval" => Some(Self::ReviewApproval),
            _ => None,
        }
    }

    /// Stable gate name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BlockTriage => "block_triage",
            Self::PilotAdmission => "pilot_admission",
            Self::ProposedApproval => "proposed_approval",
            Self::ReviewApproval => "review_approval",
        }
    }

    /// The allowed answers of an approval request on this gate:
    /// `approve` / `reject`, in presentation order. Empty for the
    /// non-approval gates, whose answers come from the hold that
    /// raised them.
    pub fn approval_answers(&self) -> Vec<DecisionAnswer> {
        let approved = match self {
            Self::ProposedApproval => {
                "Approve the proposed task; it goes through task-pilot screening like a CLI approval."
            }
            Self::ReviewApproval => "Approve the task in review.",
            Self::BlockTriage | Self::PilotAdmission => return Vec::new(),
        };
        vec![
            DecisionAnswer {
                id: "approve".to_string(),
                label: "Approve".to_string(),
                consequence: approved.to_string(),
            },
            DecisionAnswer {
                id: "reject".to_string(),
                label: "Reject".to_string(),
                consequence: "Reject the task.".to_string(),
            },
        ]
    }
}

/// Lifecycle state of a [`DecisionRequest`]. `Superseded` and `Expired`
/// are derived on read; only `Open` is written when a request is raised.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRequestState {
    /// Raised, unanswered, binding still current.
    Open,
    /// An allowed answer was applied; terminal.
    Answered,
    /// The binding stopped matching before an answer arrived; terminal.
    Superseded,
    /// The expiry passed while open; terminal.
    Expired,
}

impl DecisionRequestState {
    /// Stable state name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Answered => "answered",
            Self::Superseded => "superseded",
            Self::Expired => "expired",
        }
    }
}

/// One allowed answer to a [`DecisionRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionAnswer {
    /// Stable answer id, taken from the vocabulary that raised the hold.
    pub id: String,
    /// Short label rendered beside the answer.
    pub label: String,
    /// One-line consequence of choosing this answer.
    pub consequence: String,
}

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
fn binds_candidate(status: TaskStatus) -> bool {
    status == TaskStatus::Review
}

/// Whether `history` records a change after `request` was raised that
/// permanently invalidates its [`DecisionBinding::TaskRevision`]: an edit to
/// the description or acceptance criteria, a status move, or, for a review
/// approval, a different candidate. Superseded is terminal, so restoring the
/// earlier text, status or candidate does not revive the request. The
/// request's own raise and bookkeeping entries (comments, plan or summary
/// writes) change none of these.
fn task_revision_changed_since(request: &DecisionRequest, history: &[TaskHistoryEntry]) -> bool {
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

/// What an applied answer recorded on a [`DecisionRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionResolution {
    /// Request the answer applies to. Redundant inside the request's own
    /// `resolution`, but a `decision_answered` entry carries only this
    /// record, so it names its request here: appended history entries do
    /// not retain their `operation_id` through the task store.
    #[serde(default)]
    pub request_id: String,
    /// Chosen answer id.
    pub answer_id: String,
    /// Human who answered.
    pub answered_by: String,
    /// Channel provenance, when the answer arrived through a channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    /// Free-text note kept with the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When the answer was applied.
    pub answered_at: chrono::DateTime<chrono::Utc>,
}

/// A request for a human decision on held work, stored as a
/// `decision_requested` task-history entry whose note carries this record
/// as JSON (the design names the entry's `operation_id` as the request
/// id; the task store does not retain that field on appended entries, so
/// the note record is authoritative). No new store: supersession and
/// expiry are derived on read, so they need no write.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionRequest {
    /// Opaque request id (`dr-<random>`); the token binds to it.
    pub id: String,
    /// The held task.
    pub task_id: String,
    /// The gate the request waits on.
    pub gate: DecisionGate,
    /// Allowed answers, in the order surfaces present them.
    pub answers: Vec<DecisionAnswer>,
    /// Stale-guard binding the request was raised against.
    pub binding: DecisionBinding,
    /// Mechanism that raised it (`block_triage`, `task_pilot`, `digest`).
    pub raised_by: String,
    /// When it was raised.
    pub raised_at: chrono::DateTime<chrono::Utc>,
    /// `raised_at + request_ttl_hours`; the token expires with it.
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Stored state; effective state is derived by
    /// [`decision_requests_from_history`].
    pub state: DecisionRequestState,
    /// Recorded when the request is answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<DecisionResolution>,
}

impl DecisionRequest {
    /// A new open request with a generated id, expiring after the
    /// default TTL ([`DEFAULT_DECISION_REQUEST_TTL_HOURS`]).
    pub fn new(
        task_id: String,
        gate: DecisionGate,
        answers: Vec<DecisionAnswer>,
        binding: DecisionBinding,
        raised_by: String,
        raised_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let id = Self::new_id(&task_id, &binding, raised_at);
        Self {
            id,
            task_id,
            gate,
            answers,
            binding,
            raised_by,
            raised_at,
            expires_at: raised_at + chrono::Duration::hours(DEFAULT_DECISION_REQUEST_TTL_HOURS),
            state: DecisionRequestState::Open,
            resolution: None,
        }
    }

    /// Deterministic request id for one raise, from the task, binding and
    /// raise time, so a retried raise of the same hold reuses its id.
    pub fn new_id(
        task_id: &str,
        binding: &DecisionBinding,
        raised_at: chrono::DateTime<chrono::Utc>,
    ) -> String {
        use sha2::{Digest, Sha256};
        let seed = format!(
            "{task_id}|{}|{}",
            binding.key(),
            raised_at.timestamp_nanos_opt().unwrap_or_default()
        );
        format!(
            "dr-{}",
            crate::identity::lower_hex(Sha256::digest(seed.as_bytes()))
                .chars()
                .take(16)
                .collect::<String>()
        )
    }

    /// The history entry that raises this request, written by `by`.
    pub fn history_entry(&self, by: &str) -> TaskHistoryEntry {
        TaskHistoryEntry {
            at: self.raised_at,
            by: by.to_string(),
            event: DECISION_REQUESTED_EVENT.to_string(),
            note: serde_json::to_string(self).ok(),
            from_status: None,
            to_status: None,
            operation_id: Some(self.id.clone()),
            decision: None,
        }
    }

    /// Read the request one history entry carries, or `None` when the
    /// entry is not a readable raise of one. The request id lives in the
    /// note record: appended history entries do not retain their
    /// `operation_id` through the task store, so a present one is only
    /// checked, never required.
    pub fn from_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        if entry.event != DECISION_REQUESTED_EVENT {
            return None;
        }
        let note = entry.note.as_deref()?;
        let request: Self = serde_json::from_str(note).ok()?;
        if let Some(operation_id) = entry.operation_id.as_deref()
            && operation_id != request.id
        {
            return None;
        }
        Some(request)
    }

    /// Whether `answer_id` is one of this request's allowed answers.
    pub fn allows(&self, answer_id: &str) -> bool {
        self.answers.iter().any(|answer| answer.id == answer_id)
    }
}

/// Every decision request in `history`, oldest first, with its effective
/// state: a request is `answered` when a `decision_answered` entry (or its
/// own resolution) records one, `expired` once its expiry passes, and
/// `superseded` when its binding is no longer current or a newer request
/// for the same task follows it. At most one request per task reads as `open`.
/// An approval ([`DecisionBinding::TaskRevision`]) is superseded for good by a
/// description or criteria edit, status move or, for a review approval, a
/// candidate change after its raise, so restoring the earlier state does not
/// reopen it.
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
    let block_episode = (task.status == TaskStatus::Blocked)
        .then(|| {
            history
                .iter()
                .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
        })
        .flatten()
        .map(|entry| entry.at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true));
    let current_candidate = current_candidate_identity(history);
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
                        && (!binds_candidate(*status)
                            || candidate.as_deref() == current_candidate.as_deref())
                }
            }
    };
    let answered: std::collections::BTreeMap<String, DecisionResolution> = history
        .iter()
        .filter(|entry| entry.event == DECISION_ANSWERED_EVENT)
        .filter_map(|entry| {
            let resolution: DecisionResolution =
                serde_json::from_str(entry.note.as_deref()?).ok()?;
            let request_id = if resolution.request_id.is_empty() {
                entry.operation_id.as_deref()?.to_string()
            } else {
                resolution.request_id.clone()
            };
            Some((request_id, resolution))
        })
        .collect();
    let mut requests: Vec<DecisionRequest> = history
        .iter()
        .filter_map(DecisionRequest::from_entry)
        .collect();
    let task_ids: Vec<String> = requests
        .iter()
        .map(|request| request.task_id.clone())
        .collect();
    for (index, request) in requests.iter_mut().enumerate() {
        if let Some(resolution) = answered.get(&request.id) {
            request.state = DecisionRequestState::Answered;
            request.resolution = Some(resolution.clone());
        } else if request.resolution.is_some() || request.state == DecisionRequestState::Answered {
            request.state = DecisionRequestState::Answered;
        } else if !binding_current(request)
            || task_revision_changed_since(request, history)
            || task_ids
                .iter()
                .skip(index + 1)
                .any(|task_id| task_id == &request.task_id)
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
