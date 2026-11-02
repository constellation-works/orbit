//! Versioned resident memory and manager direction, carried as task comments.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ResidentRight;
use crate::task::is_valid_orb_task_id;
use crate::workflow::ResidentCommentError;

/// Maximum encoded size of any resident comment.
pub const RESIDENT_COMMENT_MAX_BYTES: usize = 64 * 1024;

type RecordResult = Result<(), ResidentCommentError>;

/// The three reserved task-comment shapes. Unknown fields are refused even
/// inside nested objects, so a direction cannot smuggle an authority edit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum ResidentComment {
    #[serde(rename = "resident.checkpoint")]
    Checkpoint(Box<ResidentCheckpoint>),
    #[serde(rename = "resident.request")]
    Request(ResidentRequest),
    #[serde(rename = "resident.answer")]
    Answer(ResidentAnswer),
}

/// The bounded memory a cycle leaves for its successor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentCheckpoint {
    pub schema: u8,
    pub mandate: String,
    pub cycle: String,
    pub run_id: String,
    pub crew: String,
    pub recovery_of: Option<String>,
    pub wake: ResidentCheckpointWake,
    pub authority: ResidentCheckpointAuthority,
    pub actions: Vec<ResidentCheckpointAction>,
    pub decisions: Vec<ResidentCheckpointDecision>,
    pub requests: Vec<ResidentCheckpointRequest>,
    pub costs: Vec<ResidentCostEvent>,
    pub notes: String,
    pub next_wake: ResidentNextWake,
    pub continuity: String,
    pub outcome: String,
    pub probe: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentCheckpointWake {
    pub reasons: Vec<String>,
    pub digest: String,
}

/// Observed authority is a report, never a grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentCheckpointAuthority {
    pub rights: Vec<ResidentRight>,
    pub observe_only: bool,
    pub narrowed_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentCheckpointAction {
    pub key: String,
    pub kind: String,
    pub subject: String,
    pub result: String,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentCheckpointDecision {
    pub id: String,
    pub summary: String,
    pub class: String,
    pub reversible: bool,
    pub p_wrong: String,
    pub rework_units: u64,
    pub precedent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentCheckpointRequest {
    pub id: String,
    pub decision_request: String,
    pub task: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentCostEvent {
    pub kind: String,
    pub what: String,
    pub minutes: u64,
    pub retries: u64,
    pub slot_minutes: u64,
    pub wished: Option<String>,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentNextWake {
    pub reason: String,
    pub not_before: DateTime<Utc>,
}

/// Index of an escalation; the decision request remains answer authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentRequest {
    pub schema: u8,
    pub mandate: String,
    pub cycle: String,
    pub id: String,
    pub decision_request: String,
    pub task: String,
    pub class: String,
    pub question: String,
    pub recommended: String,
    pub blocking: bool,
    pub blocks: Vec<String>,
}

/// A direction or answer, with the human's words preserved verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentAnswer {
    pub schema: u8,
    pub mandate: String,
    pub request_id: Option<String>,
    pub reverses: Option<String>,
    pub quote: String,
    pub source: ResidentAnswerSource,
    /// Null for a human writing directly; required for a relay.
    pub relayed_by: Option<ResidentAnswerRelay>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentAnswerSource {
    pub human: String,
    pub via: String,
    pub session: Option<String>,
    pub said_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentAnswerRelay {
    pub crew: String,
    pub model: String,
}

const ESCALATION_CLASSES: &[&str] = &[
    "product_scope",
    "architecture",
    "security",
    "material_cost",
    "irreversible",
    "authority",
    "cross_mandate_conflict",
    "human_reserved",
];

fn invalid(reason: impl Into<String>) -> ResidentCommentError {
    ResidentCommentError::Invalid(reason.into())
}

fn nonempty(value: &str, field: &str) -> RecordResult {
    if value.trim().is_empty() {
        return Err(invalid(format!("`{field}` must not be empty")));
    }
    Ok(())
}

fn member(value: &str, allowed: &[&str], field: &str) -> RecordResult {
    if !allowed.contains(&value) {
        return Err(invalid(format!("unsupported `{field}`: {value}")));
    }
    Ok(())
}

fn task_id(value: &str, field: &str) -> RecordResult {
    if !is_valid_orb_task_id(value) {
        return Err(invalid(format!("`{field}` must be an assignment task id")));
    }
    Ok(())
}

fn one_line(value: &str, field: &str) -> RecordResult {
    nonempty(value, field)?;
    if value.contains(['\n', '\r']) {
        return Err(invalid(format!("`{field}` must be one line")));
    }
    Ok(())
}

impl ResidentComment {
    /// Recognize the reserved kinds without interpreting ordinary task prose.
    pub fn is_resident_comment(message: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(message)
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.get("kind").and_then(|kind| kind.as_str()),
                    Some("resident.checkpoint" | "resident.request" | "resident.answer")
                )
            })
    }

    /// Parse a complete JSON object and validate its version, bounds and content.
    pub fn parse(message: &str) -> Result<Self, ResidentCommentError> {
        if message.len() > RESIDENT_COMMENT_MAX_BYTES {
            return Err(invalid("record exceeds 64 KiB"));
        }
        let record: Self = serde_json::from_str(message).map_err(|e| invalid(e.to_string()))?;
        record.validate()?;
        Ok(record)
    }

    pub fn mandate(&self) -> &str {
        match self {
            Self::Checkpoint(record) => &record.mandate,
            Self::Request(record) => &record.mandate,
            Self::Answer(record) => &record.mandate,
        }
    }

    pub fn cycle(&self) -> Option<&str> {
        match self {
            Self::Checkpoint(record) => Some(&record.cycle),
            Self::Request(record) => Some(&record.cycle),
            Self::Answer(_) => None,
        }
    }

    pub fn validate(&self) -> RecordResult {
        let schema = match self {
            Self::Checkpoint(record) => record.schema,
            Self::Request(record) => record.schema,
            Self::Answer(record) => record.schema,
        };
        if schema != 1 {
            return Err(invalid("`schema` must be 1"));
        }
        task_id(self.mandate(), "mandate")?;
        if let Some(cycle) = self.cycle() {
            nonempty(cycle, "cycle")?;
        }
        match self {
            Self::Checkpoint(record) => record.validate_checkpoint(),
            Self::Request(record) => {
                nonempty(&record.id, "id")?;
                nonempty(&record.decision_request, "decision_request")?;
                task_id(&record.task, "task")?;
                member(&record.class, ESCALATION_CLASSES, "class")?;
                nonempty(&record.question, "question")?;
                nonempty(&record.recommended, "recommended")?;
                for block in &record.blocks {
                    task_id(block, "blocks")?;
                }
                Ok(())
            }
            Self::Answer(record) => {
                nonempty(&record.quote, "quote")?;
                if record.quote.chars().count() > 4000 {
                    return Err(invalid("`quote` exceeds 4000 characters"));
                }
                nonempty(&record.source.human, "source.human")?;
                nonempty(&record.source.via, "source.via")?;
                if let Some(relay) = &record.relayed_by {
                    nonempty(&relay.crew, "relayed_by.crew")?;
                    nonempty(&relay.model, "relayed_by.model")?;
                }
                for (value, field) in [
                    (&record.request_id, "request_id"),
                    (&record.reverses, "reverses"),
                ] {
                    if let Some(value) = value {
                        nonempty(value, field)?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl ResidentCheckpoint {
    fn validate_checkpoint(&self) -> RecordResult {
        nonempty(&self.run_id, "run_id")?;
        nonempty(&self.crew, "crew")?;
        if self.actions.len() > 50 || self.decisions.len() > 20 || self.costs.len() > 20 {
            return Err(invalid(
                "record allows at most 50 actions, 20 decisions and 20 costs",
            ));
        }
        if self.notes.chars().count() > 2000 {
            return Err(invalid("`notes` exceeds 2000 characters"));
        }
        if self.wake.reasons.is_empty() {
            return Err(invalid("`wake.reasons` must not be empty"));
        }
        for reason in &self.wake.reasons {
            nonempty(reason, "wake.reasons")?;
        }
        let digest = self.wake.digest.strip_prefix("sha256:").unwrap_or_default();
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid("`wake.digest` must be a sha256 digest"));
        }
        nonempty(&self.next_wake.reason, "next_wake.reason")?;
        member(
            &self.continuity,
            &["resumed", "rebuilt", "off"],
            "continuity",
        )?;
        member(
            &self.outcome,
            &[
                "progress",
                "idle",
                "waiting",
                "incomplete",
                "handoff",
                "ended",
            ],
            "outcome",
        )?;
        if self.outcome == "incomplete" {
            nonempty(self.error.as_deref().unwrap_or_default(), "error")?;
        }
        if let Some(recovery_of) = &self.recovery_of {
            nonempty(recovery_of, "recovery_of")?;
            if recovery_of == &self.cycle {
                return Err(invalid("`recovery_of` must name a different cycle"));
            }
        }
        let prefix = format!("resident:{}:{}:", self.mandate, self.cycle);
        let recovery_prefix = self
            .recovery_of
            .as_ref()
            .map(|cycle| format!("resident:{}:{cycle}:", self.mandate));
        let mut keys = BTreeSet::new();
        for action in &self.actions {
            let index = action
                .key
                .strip_prefix(&prefix)
                .or_else(|| {
                    recovery_prefix
                        .as_deref()
                        .and_then(|prefix| action.key.strip_prefix(prefix))
                })
                .and_then(|suffix| suffix.parse::<u64>().ok());
            if !index.is_some_and(|index| index > 0) || !keys.insert(&action.key) {
                return Err(invalid(
                    "action keys must be unique resident:<mandate>:<cycle>:<positive integer> keys for this cycle or recovery_of",
                ));
            }
            nonempty(&action.kind, "actions.kind")?;
            nonempty(&action.subject, "actions.subject")?;
            member(
                &action.result,
                &["ok", "refused", "failed", "already_done"],
                "actions.result",
            )?;
        }
        let mut ids = BTreeSet::new();
        for decision in &self.decisions {
            nonempty(&decision.id, "decisions.id")?;
            if !ids.insert(&decision.id) {
                return Err(invalid("decision ids must be unique"));
            }
            nonempty(&decision.summary, "decisions.summary")?;
            if !["routine", "reversible"].contains(&decision.class.as_str()) {
                member(&decision.class, ESCALATION_CLASSES, "decisions.class")?;
            }
            member(
                &decision.p_wrong,
                &["low", "medium", "high"],
                "decisions.p_wrong",
            )?;
        }
        for request in &self.requests {
            nonempty(&request.id, "requests.id")?;
            nonempty(&request.decision_request, "requests.decision_request")?;
            task_id(&request.task, "requests.task")?;
        }
        for cost in &self.costs {
            member(
                &cost.kind,
                &["workaround", "manual_step", "retry", "wait"],
                "costs.kind",
            )?;
            one_line(&cost.what, "costs.what")?;
            if ["workaround", "manual_step"].contains(&cost.kind.as_str()) {
                one_line(cost.wished.as_deref().unwrap_or_default(), "costs.wished")?;
            } else if let Some(wished) = &cost.wished {
                one_line(wished, "costs.wished")?;
            }
            if let Some(signature) = &cost.signature
                && (!(3..=64).contains(&signature.len())
                    || signature.split('-').any(|word| {
                        word.is_empty()
                            || !word
                                .bytes()
                                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                    }))
            {
                return Err(invalid(
                    "`costs.signature` must be a kebab-case key of 3-64 characters",
                ));
            }
        }
        Ok(())
    }
}
