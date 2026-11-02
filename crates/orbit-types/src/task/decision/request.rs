//! Decision request, binding and resolution lifecycle.

use serde::{Deserialize, Serialize};

use crate::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, DESCRIPTION_DOCUMENT, TASK_FIELDS_UPDATED_EVENT, Task,
    TaskHistoryEntry, TaskStatus, task_fields_updated_documents,
};
use crate::workflow::activity_job::ScriptGrantRequest;
use crate::workflow::{
    HeldCandidate, ResidentGrantRequest, ReviewGateHistoryNote, resident_authority_revision,
};

/// History event that raises a [`DecisionRequest`].
pub const DECISION_REQUESTED_EVENT: &str = "decision_requested";
/// History event that records an applied answer to a [`DecisionRequest`].
pub const DECISION_ANSWERED_EVENT: &str = "decision_answered";
/// History event that re-opens a precedent answer. A later human answer
/// clears it. Restoring status without this event does not reopen a request.
pub const DECISION_REOPENED_EVENT: &str = "decision_reopened";
/// Writer label of an answer applied from an operator-approved corpus rule.
/// It is not a human answer and the human apply path refuses it.
pub const PRECEDENT_ACTOR: &str = "precedent";
/// Writer label of an answer the decision model applied. It is an automation
/// actor ([`crate::identity::is_automation_actor`]), so no guard that needs a
/// human resolution accepts it, and the human apply path refuses it.
pub const MODEL_ANSWER_ACTOR: &str = "model";
/// History event a human writes when confirming a model answer through its
/// [`DecisionGate::ModelAnswerReview`] request. The note is a
/// [`ModelAnswerConfirmation`]; only a confirmed model answer is precedent.
pub const DECISION_MODEL_CONFIRMED_EVENT: &str = "decision_model_confirmed";
/// Status-note marker on the bookkeeping status moves a precedent answer
/// and its reversal write. Those moves must not permanently supersede the
/// request: the reversal puts the task back and the request reads open again.
pub const PRECEDENT_BOOKKEEPING_NOTE: &str = "precedent-bookkeeping";
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
    /// A multiple-choice question an agent raised on a human-crew task. An
    /// answer records the chosen option and completes the task.
    AgentRequest,
    /// Human review of an answer the decision model applied: `confirm`
    /// keeps it and makes it precedent, `veto` re-opens the original
    /// request. The model never answers this gate.
    ModelAnswerReview,
    /// A request to widen a resident mandate's authority, relayed by a
    /// caller without operator authority: `grant` applies exactly the
    /// requested addition, `reject` changes nothing. Only a human answers
    /// it; precedent and the decision model never do.
    ResidentAuthority,
    /// Consent to run one workspace script at one content digest with one
    /// capability set (script steps §2): `grant` writes exactly that grant
    /// with the answering human as grantor, `reject` writes nothing. Anyone
    /// may raise one; only a human answers it, never precedent or the model.
    ScriptGrant,
}

impl DecisionGate {
    /// The gate a stable gate name denotes, if it denotes one.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "block_triage" => Some(Self::BlockTriage),
            "pilot_admission" => Some(Self::PilotAdmission),
            "proposed_approval" => Some(Self::ProposedApproval),
            "review_approval" => Some(Self::ReviewApproval),
            "agent_request" => Some(Self::AgentRequest),
            "model_answer_review" => Some(Self::ModelAnswerReview),
            "resident_authority" => Some(Self::ResidentAuthority),
            "script_grant" => Some(Self::ScriptGrant),
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
            Self::AgentRequest => "agent_request",
            Self::ModelAnswerReview => "model_answer_review",
            Self::ResidentAuthority => "resident_authority",
            Self::ScriptGrant => "script_grant",
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
            Self::BlockTriage
            | Self::PilotAdmission
            | Self::AgentRequest
            | Self::ModelAnswerReview
            | Self::ResidentAuthority
            | Self::ScriptGrant => return Vec::new(),
        };
        vec![
            DecisionAnswer::plain("approve", "Approve", approved),
            DecisionAnswer::plain("reject", "Reject", "Reject the task."),
        ]
    }

    /// The two answers of a [`Self::ModelAnswerReview`] request, in
    /// presentation order. [`DecisionRequest::new`] fixes their tiers:
    /// `confirm` is routine and `veto` reversible.
    pub fn model_answer_review_answers() -> Vec<DecisionAnswer> {
        vec![
            DecisionAnswer::plain(
                MODEL_ANSWER_CONFIRM,
                "Confirm",
                "Keep the model's answer; it becomes precedent with you as the decider.",
            ),
            DecisionAnswer::plain(
                MODEL_ANSWER_VETO,
                "Veto",
                "Re-open the original request and restore the task's previous status.",
            ),
        ]
    }

    /// The two answers of a [`Self::ResidentAuthority`] request, in
    /// presentation order. [`DecisionRequest::new`] marks both as
    /// authority-tier.
    pub fn resident_authority_answers() -> Vec<DecisionAnswer> {
        vec![
            DecisionAnswer::plain(
                RESIDENT_AUTHORITY_GRANT,
                "Grant",
                "Add exactly the requested rights and workspaces to the resident's authority.",
            ),
            DecisionAnswer::plain(
                RESIDENT_AUTHORITY_REJECT,
                "Reject",
                "Leave the resident's authority unchanged.",
            ),
        ]
    }

    /// The two answers of a [`Self::ScriptGrant`] request, in presentation
    /// order. [`DecisionRequest::new`] marks both as authority-tier.
    pub fn script_grant_answers() -> Vec<DecisionAnswer> {
        vec![
            DecisionAnswer::plain(
                SCRIPT_GRANT_GRANT,
                "Grant",
                "Grant the script at exactly this digest and capability set; its step runs on the next attempt.",
            ),
            DecisionAnswer::plain(
                SCRIPT_GRANT_REJECT,
                "Reject",
                "Grant nothing; the step keeps refusing until the script changes or an operator grants it.",
            ),
        ]
    }
}

/// Answer id that writes the grant a script grant request asks for.
pub const SCRIPT_GRANT_GRANT: &str = "grant";
/// Answer id that refuses a script grant request.
pub const SCRIPT_GRANT_REJECT: &str = "reject";

/// Answer id that applies a resident grant request.
pub const RESIDENT_AUTHORITY_GRANT: &str = "grant";
/// Answer id that refuses a resident grant request.
pub const RESIDENT_AUTHORITY_REJECT: &str = "reject";

/// Answer id that confirms a model answer.
pub const MODEL_ANSWER_CONFIRM: &str = "confirm";
/// Answer id that vetoes a model answer.
pub const MODEL_ANSWER_VETO: &str = "veto";

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

/// Risk of an answer. Unknown is conservative for absent or future values.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRiskTier {
    /// No lasting effect.
    Routine,
    /// Reversible through the normal path.
    Reversible,
    /// Cannot be undone through the normal path.
    Irreversible,
    /// Widens authority.
    Authority,
    /// Commits additional spend.
    Spend,
    /// Undeclared or unrecognised.
    #[default]
    Unknown,
}

impl<'de> Deserialize<'de> for DecisionRiskTier {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match value.as_str() {
            Some("routine") => Self::Routine,
            Some("reversible") => Self::Reversible,
            Some("irreversible") => Self::Irreversible,
            Some("authority") => Self::Authority,
            Some("spend") => Self::Spend,
            _ => Self::Unknown,
        })
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
    /// Risk declared for this answer.
    #[serde(default)]
    pub risk_tier: DecisionRiskTier,
    /// What choosing the answer changes, when it is a one-click option:
    /// a field rewrite, an action, or both. Absent on approval and
    /// pilot-admission answers and on requests raised before options existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<DecisionOption>,
}

impl DecisionAnswer {
    /// An answer with no option payload: its gate defines what it does.
    pub fn plain(
        id: impl Into<String>,
        label: impl Into<String>,
        consequence: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            consequence: consequence.into(),
            option: None,
            risk_tier: DecisionRiskTier::Unknown,
        }
    }
}

/// The task fields a decision option rewrites. At least one is set.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionRewrite {
    /// The complete new description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The complete new acceptance-criteria list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<Vec<String>>,
}

impl DecisionRewrite {
    /// Whether the rewrite changes no field.
    pub fn is_empty(&self) -> bool {
        self.description.is_none() && self.acceptance_criteria.is_none()
    }
}

/// The closed set of actions a decision option may run after its rewrite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionAction {
    /// Return the task to `backlog` for a fresh run (approve a proposed task).
    Requeue,
    /// Explicitly submit one new pilot (no rewrite required); a blocked task returns to backlog.
    RePilot,
    /// Assign `crew`, then return the task to `backlog`.
    ChangeCrew {
        /// The registered crew the task runs on next.
        crew: String,
    },
    /// File a follow-up task linked `spawned_from` this one; the option's
    /// rewrite narrows this task, which returns to `backlog`.
    Split {
        /// Title of the follow-up task.
        title: String,
        /// Description of the follow-up task.
        description: String,
        /// Acceptance criteria of the follow-up task.
        acceptance_criteria: Vec<String>,
    },
    /// Reject the task.
    Reject,
    /// Archive the task.
    Archive,
}

impl DecisionAction {
    /// Every action tag, in contract order.
    pub const KINDS: [&'static str; 6] = [
        "requeue",
        "re_pilot",
        "change_crew",
        "split",
        "reject",
        "archive",
    ];

    /// The stable tag of this action.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Requeue => "requeue",
            Self::RePilot => "re_pilot",
            Self::ChangeCrew { .. } => "change_crew",
            Self::Split { .. } => "split",
            Self::Reject => "reject",
            Self::Archive => "archive",
        }
    }
}

/// The payload of a one-click decision option.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionOption {
    /// One sentence naming what the option decides.
    pub summary: String,
    /// Field rewrite applied when the option is chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<DecisionRewrite>,
    /// Action run when the option is chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<DecisionAction>,
}

/// Why a proposed set of decision options is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecisionOptionError {
    /// The set does not hold two or three options.
    #[error("a decision needs two or three options, not {0}")]
    OptionCount(usize),
    /// `recommended` names no option.
    #[error("`recommended` must name one of the {0} options (1-based)")]
    Recommended(usize),
    /// A text field is empty.
    #[error("{0} must not be empty")]
    EmptyField(String),
    /// An option has neither a rewrite nor an action.
    #[error("option {0} has neither a rewrite nor an action, so choosing it would change nothing")]
    NoEffect(usize),
    /// An option's action is not one of the closed set.
    #[error("option {index} has an unusable action: {reason}")]
    Action {
        /// 1-based option index.
        index: usize,
        /// What is wrong.
        reason: String,
    },
}

/// One option as an agent or tool proposes it, before validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionOptionInput {
    /// One-sentence summary of the option.
    pub summary: String,
    /// One-line consequence of choosing it.
    pub consequence: String,
    /// Optional field rewrite.
    #[serde(default)]
    pub rewrite: Option<DecisionRewrite>,
    /// Optional action, tagged by `kind`; unknown kinds are refused when the
    /// input is parsed.
    #[serde(default)]
    pub action: Option<DecisionAction>,
}

/// Validate a proposed option set and render it as answers `1`, `2`, `3`.
/// Two or three options, every text non-empty, each with a rewrite, an
/// action or both, an action that carries what it needs, and `recommended`
/// (1-based) naming one of them.
pub fn decision_option_answers(
    options: &[DecisionOptionInput],
    recommended: usize,
) -> Result<Vec<DecisionAnswer>, DecisionOptionError> {
    if !(2..=3).contains(&options.len()) {
        return Err(DecisionOptionError::OptionCount(options.len()));
    }
    if !(1..=options.len()).contains(&recommended) {
        return Err(DecisionOptionError::Recommended(options.len()));
    }
    options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let number = index + 1;
            let text = |value: &str, field: &str| {
                let value = value.trim();
                if value.is_empty() {
                    Err(DecisionOptionError::EmptyField(format!(
                        "option {number} {field}"
                    )))
                } else {
                    Ok(value.to_string())
                }
            };
            let summary = text(&option.summary, "summary")?;
            let consequence = text(&option.consequence, "consequence")?;
            let rewrite = match &option.rewrite {
                Some(rewrite) => {
                    Some(validated_rewrite(number, rewrite)?).filter(|r| !r.is_empty())
                }
                None => None,
            };
            let action = match &option.action {
                Some(action) => Some(validated_action(number, action)?),
                None => None,
            };
            if rewrite.is_none() && action.is_none() {
                return Err(DecisionOptionError::NoEffect(number));
            }
            if rewrite.is_some()
                && matches!(
                    action,
                    Some(DecisionAction::Reject | DecisionAction::Archive)
                )
            {
                return Err(DecisionOptionError::Action {
                    index: number,
                    reason: "reject and archive end the task, so they take no rewrite".to_string(),
                });
            }
            Ok(DecisionAnswer {
                id: number.to_string(),
                label: summary.clone(),
                consequence,
                risk_tier: DecisionRiskTier::Unknown,
                option: Some(DecisionOption {
                    summary,
                    rewrite,
                    action,
                }),
            })
        })
        .collect()
}

fn validated_rewrite(
    number: usize,
    rewrite: &DecisionRewrite,
) -> Result<DecisionRewrite, DecisionOptionError> {
    let description = match &rewrite.description {
        Some(value) if value.trim().is_empty() => {
            return Err(DecisionOptionError::EmptyField(format!(
                "option {number} rewrite.description"
            )));
        }
        Some(value) => Some(value.trim().to_string()),
        None => None,
    };
    let acceptance_criteria = match &rewrite.acceptance_criteria {
        Some(criteria) => {
            let criteria: Vec<String> = criteria
                .iter()
                .map(|criterion| criterion.trim().to_string())
                .collect();
            if criteria.is_empty() || criteria.iter().any(String::is_empty) {
                return Err(DecisionOptionError::EmptyField(format!(
                    "option {number} rewrite.acceptance_criteria"
                )));
            }
            Some(criteria)
        }
        None => None,
    };
    Ok(DecisionRewrite {
        description,
        acceptance_criteria,
    })
}

fn validated_action(
    number: usize,
    action: &DecisionAction,
) -> Result<DecisionAction, DecisionOptionError> {
    let bad = |reason: &str| DecisionOptionError::Action {
        index: number,
        reason: reason.to_string(),
    };
    Ok(match action {
        DecisionAction::ChangeCrew { crew } => {
            if crew.trim().is_empty() {
                return Err(bad("change_crew needs a crew"));
            }
            DecisionAction::ChangeCrew {
                crew: crew.trim().to_string(),
            }
        }
        DecisionAction::Split {
            title,
            description,
            acceptance_criteria,
        } => {
            if title.trim().is_empty() || description.trim().is_empty() {
                return Err(bad("split needs a title and a description"));
            }
            let criteria: Vec<String> = acceptance_criteria
                .iter()
                .map(|criterion| criterion.trim().to_string())
                .collect();
            if criteria.is_empty() || criteria.iter().any(String::is_empty) {
                return Err(bad("split needs acceptance criteria"));
            }
            DecisionAction::Split {
                title: title.trim().to_string(),
                description: description.trim().to_string(),
                acceptance_criteria: criteria,
            }
        }
        other => other.clone(),
    })
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
    /// The model answer a [`DecisionGate::ModelAnswerReview`] request
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

/// One human answer a precedent rule cites.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrecedentSource {
    /// Task whose human answer the rule cites.
    pub task: String,
    /// Calendar date of that answer (`YYYY-MM-DD`).
    pub date: String,
    /// Option id that answer chose.
    pub option: String,
}

/// Audit fields that separate a precedent answer from a human one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrecedentCitation {
    /// Corpus rule that supplied the option.
    pub rule_id: String,
    /// Human answers the rule was curated from.
    pub citations: Vec<PrecedentSource>,
    /// The operator set `auto` on the rule. Consolidation never does.
    pub operator_approved: bool,
    /// Task status to restore when the operator reverses the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_status: Option<TaskStatus>,
}

/// One precedent a model answer cites, as the precedent provider supplied it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCitedPrecedent {
    /// Provider that supplied the precedent.
    pub provider: String,
    /// Provider-stable precedent id.
    pub precedent_id: String,
    /// When the cited human decision was made.
    pub decided_at: chrono::DateTime<chrono::Utc>,
    /// Citation source name.
    pub source: String,
    /// Stable reference inside the source.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Content digest of the cited record.
    pub digest: String,
}

/// The category enablement a model answer ran under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelAnswerEnablement {
    /// Operator who enabled the category.
    pub operator: String,
    /// The readout the enablement cites (`<task>/<artifact>`).
    pub readout: String,
}

/// Audit fields that separate a model answer from a human one. Recorded
/// only when `answered_by` is [`MODEL_ANSWER_ACTOR`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelAnswerCitation {
    /// Assessment that produced the answer.
    pub assessment_id: String,
    /// Logical id of that assessment.
    pub logical_id: String,
    /// Category the request belongs to.
    pub category: String,
    /// Assistance policy version.
    pub policy_version: String,
    /// Assistance policy digest.
    pub policy_digest: String,
    /// Question-set version.
    pub question_set: String,
    /// Assistance contract version.
    pub contract_version: String,
    /// Adapter that served the assessment.
    pub adapter: String,
    /// Model identity requested.
    pub requested_model: String,
    /// Model identity the service returned.
    pub returned_model: String,
    /// Model revision epoch.
    pub model_revision: String,
    /// Probability of the chosen answer.
    pub probability: f64,
    /// The operating threshold the probability met.
    pub threshold: f64,
    /// Probability that the supplied precedents conflict.
    pub conflict_probability: f64,
    /// Precedents the answer cites.
    pub cited_precedents: Vec<ModelCitedPrecedent>,
    /// The enablement the answer ran under.
    pub enablement: ModelAnswerEnablement,
    /// Task status to restore when the answer is vetoed or reversed. The
    /// apply step records it; any value the caller supplies is replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_status: Option<TaskStatus>,
}

// Probabilities are checked to lie in [0, 1] before an answer is applied, so
// no stored citation holds a NaN and equality is total.
impl Eq for ModelAnswerCitation {}

/// Note body of a [`DECISION_MODEL_CONFIRMED_EVENT`]: the human who
/// confirmed the model answer `answered_at` on `request_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelAnswerConfirmation {
    /// The model-answered request.
    pub request_id: String,
    /// `answered_at` of the confirmed model resolution.
    pub answered_at: chrono::DateTime<chrono::Utc>,
    /// The confirming human; the event's writer.
    pub confirmed_by: String,
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
    /// Set only when `answered_by` is [`PRECEDENT_ACTOR`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precedent: Option<PrecedentCitation>,
    /// Set only when `answered_by` is [`MODEL_ANSWER_ACTOR`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelAnswerCitation>,
    /// The human confirmation of a model answer, derived on read from a
    /// later [`DECISION_MODEL_CONFIRMED_EVENT`]; never written with the
    /// answer itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<ModelAnswerConfirmation>,
}

impl DecisionResolution {
    /// Whether an automation writer (the `precedent` rule actor, the
    /// `model` actor or any other automation label) applied this answer.
    pub fn is_automation_answer(&self) -> bool {
        let actor = self.answered_by.trim();
        actor == PRECEDENT_ACTOR || crate::identity::is_automation_actor(actor)
    }
}

/// Note body of a [`DECISION_REOPENED_EVENT`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DecisionReopenNote {
    request_id: String,
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
    /// Most severe answer risk, or unknown for a legacy request.
    #[serde(default)]
    pub risk_tier: DecisionRiskTier,
    /// Category fixed when raised; absent on legacy requests.
    #[serde(default)]
    pub category: Option<String>,
    /// One or two plain-language sentences naming the decision needed,
    /// readable without opening the code. Absent on older requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Id of the answer the raiser recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
    /// The exact action outside Orbit that unblocks the task, when no option
    /// can: the request then carries one "Done — requeue" answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_action: Option<String>,
    /// Structured topic (or corpus rule id) an agent-raised request carries,
    /// so a later reader matches it by exact value, never by wording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Writer label of the agent that raised an agent-raised request; the
    /// per-filer cap counts open requests by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filer: Option<String>,
    /// Stale-guard binding the request was raised against.
    pub binding: DecisionBinding,
    /// Mechanism that raised it (`block_triage`, `task_pilot`, `digest`,
    /// `agent_request`).
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
    /// The exact addition a [`DecisionGate::ResidentAuthority`] request asks
    /// for, with the relayed quote verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_grant: Option<ResidentGrantRequest>,
    /// The script, digest, capability set and diff a
    /// [`DecisionGate::ScriptGrant`] request asks consent for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_grant: Option<ScriptGrantRequest>,
}

impl DecisionRequest {
    /// A new open request with a generated id, expiring after the
    /// default TTL ([`DEFAULT_DECISION_REQUEST_TTL_HOURS`]).
    pub fn new(
        task_id: String,
        gate: DecisionGate,
        mut answers: Vec<DecisionAnswer>,
        binding: DecisionBinding,
        raised_by: String,
        raised_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let id = Self::new_id(&task_id, &binding, raised_at);
        for answer in &mut answers {
            answer.risk_tier = match gate {
                DecisionGate::ReviewApproval => DecisionRiskTier::Irreversible,
                DecisionGate::ProposedApproval | DecisionGate::PilotAdmission => {
                    DecisionRiskTier::Authority
                }
                DecisionGate::BlockTriage => match answer
                    .option
                    .as_ref()
                    .and_then(|option| option.action.as_ref())
                {
                    Some(DecisionAction::Reject) => DecisionRiskTier::Irreversible,
                    Some(_) => DecisionRiskTier::Reversible,
                    None if answer.id == "dismiss" => DecisionRiskTier::Routine,
                    None if answer
                        .option
                        .as_ref()
                        .is_some_and(|option| option.rewrite.is_some()) =>
                    {
                        DecisionRiskTier::Reversible
                    }
                    None => DecisionRiskTier::Unknown,
                },
                DecisionGate::AgentRequest => answer.risk_tier,
                DecisionGate::ModelAnswerReview => match answer.id.as_str() {
                    MODEL_ANSWER_CONFIRM => DecisionRiskTier::Routine,
                    MODEL_ANSWER_VETO => DecisionRiskTier::Reversible,
                    _ => DecisionRiskTier::Unknown,
                },
                // Either answer is the human's call on authority.
                DecisionGate::ResidentAuthority | DecisionGate::ScriptGrant => {
                    DecisionRiskTier::Authority
                }
            };
        }
        let risk_tier = answers
            .iter()
            .map(|answer| answer.risk_tier)
            .max()
            .unwrap_or_default();
        Self {
            id,
            task_id,
            gate,
            answers,
            risk_tier,
            category: None,
            summary: None,
            recommended: None,
            human_action: None,
            topic: None,
            filer: None,
            binding,
            raised_by,
            raised_at,
            expires_at: raised_at + chrono::Duration::hours(DEFAULT_DECISION_REQUEST_TTL_HOURS),
            state: DecisionRequestState::Open,
            resolution: None,
            resident_grant: None,
            script_grant: None,
        }
    }

    /// Attach the script, digest, capabilities and diff a script grant
    /// request asks consent for.
    pub fn with_script_grant(mut self, grant: ScriptGrantRequest) -> Self {
        self.script_grant = Some(grant);
        self
    }

    /// Attach the exact addition a resident grant request asks for.
    pub fn with_resident_grant(mut self, grant: ResidentGrantRequest) -> Self {
        self.resident_grant = Some(grant);
        self
    }

    /// Fix the category at raise time for this workspace.
    pub fn with_category(mut self, workspace: &str) -> Self {
        let kind = if self.gate == DecisionGate::AgentRequest {
            let file = self
                .topic
                .as_deref()
                .map(|topic| topic.split('/').next().unwrap_or(topic))
                .unwrap_or("untopiced");
            format!("agent_request/{file}")
        } else {
            self.gate.as_str().to_string()
        };
        self.category = Some(format!("{workspace}/{kind}"));
        self
    }

    /// This request with the short summary and the recommended answer id.
    pub fn with_summary(mut self, summary: String, recommended: Option<String>) -> Self {
        self.summary = Some(summary);
        self.recommended = recommended;
        self
    }

    /// This request carrying the structured `topic` and the `filer` that raised it.
    pub fn with_topic_and_filer(mut self, topic: Option<String>, filer: String) -> Self {
        self.topic = topic;
        self.filer = Some(filer);
        self
    }

    /// This request carrying the exact action only a human outside Orbit can take.
    pub fn with_human_action(mut self, human_action: String) -> Self {
        self.human_action = Some(human_action);
        self
    }

    /// The answer `answer_id` names, if this request allows it.
    pub fn answer(&self, answer_id: &str) -> Option<&DecisionAnswer> {
        self.answers.iter().find(|answer| answer.id == answer_id)
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
/// ([`is_precedent_bookkeeping`]) are not that status move: reversing the
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
    let current_candidate = current_candidate_identity(history);
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
                        && (!binds_candidate(*status)
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
            || task_revision_changed_since(request, history)
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
