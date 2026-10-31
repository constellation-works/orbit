//! Publication proof and history readers for source-repair settling.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::TaskDecision;
use crate::task::TaskHistoryEntry;

/// The result of a source-repair publication observation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicationSettlingDecision {
    /// Reserve a bounded delay before reading again.
    Waiting,
    /// The candidate is published with all predicates satisfied.
    Settled,
    /// Publication could not be confirmed within the admitted predicates/budget.
    Refused,
}

/// Safe evidence retained in a push proof. These are the existing redacted,
/// bounded diagnostic representations, not raw provider/checkpoint payloads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PublicationProofValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    Text(String),
    Truncated {
        prefix: String,
        bytes: usize,
        truncated: bool,
    },
    Container(PublicationProofContainer),
}

/// Fixed-depth markers used by the diagnostic projection of hostile values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicationProofContainer {
    Object {
        oid: Option<Box<PublicationProofValue>>,
    },
    Array,
    Invalid,
}

impl PublicationProofValue {
    /// Only an ordinary projected string can establish an exact identity.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// Successful replacement outcomes accepted by the publication predicate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicationPushPerformed {
    PerformedForceWithLease,
    PerformedFastForward,
}

/// A push outcome, including safe evidence for unknown or malformed outcomes.
/// Unknown evidence is retained for equality and diagnostics, never authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PublicationPushDecision {
    Performed(PublicationPushPerformed),
    Other(PublicationProofValue),
}

/// A successful step output authenticated against this run's push checkpoint.
/// All projected fields participate in equality across interrupted waits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicationPushProof {
    pub step_index: u32,
    pub checkpoint: PublicationPushCheckpoint,
    pub checkpoint_sha256: String,
    pub branch: PublicationProofValue,
    pub local_sha: PublicationProofValue,
    pub remote_sha_before: PublicationProofValue,
    pub force_with_lease: Option<bool>,
    pub decision: PublicationPushDecision,
}

/// The named pipeline checkpoint that authenticated a publication push.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicationPushCheckpoint {
    Push,
}

/// Replay state recorded alongside the unchanged human diagnostic note.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicationSettlingObservation {
    pub run_id: PublicationProofValue,
    pub published_head: PublicationProofValue,
    pub successful_push: Option<PublicationPushProof>,
    pub reserved_delay_seconds: u64,
    pub decision: PublicationSettlingDecision,
}

/// A publication history record cannot safely restore a reservation.
#[derive(Debug, thiserror::Error)]
pub enum PublicationHistoryError {
    #[error("publication settling history unreadable")]
    History,
    #[error("publication settling reservation unreadable")]
    Reservation,
    #[error("publication settling head unreadable")]
    Head,
}

impl PublicationSettlingObservation {
    /// Read a matching typed decision, consulting the legacy JSON note only
    /// when no decision exists. Foreign runs cannot spend this run's budget.
    /// A mismatched decision refuses rather than falling through to prose.
    pub fn from_entry(
        entry: &TaskHistoryEntry,
        run: &str,
    ) -> Result<Option<Self>, PublicationHistoryError> {
        let observation = match &entry.decision {
            Some(TaskDecision::SourceRepairPublication(observation)) => {
                if observation.run_id.as_text() != Some(run) {
                    return Ok(None);
                }
                observation.clone()
            }
            Some(_) => return Err(PublicationHistoryError::History),
            None => return Self::from_legacy_note(entry.note.as_deref().unwrap_or(""), run),
        };
        observation.validate()?;
        Ok(Some(observation))
    }

    fn validate(&self) -> Result<(), PublicationHistoryError> {
        if self.reserved_delay_seconds > 0 && self.published_head.as_text().is_none() {
            return Err(PublicationHistoryError::Head);
        }
        Ok(())
    }

    /// Isolated compatibility boundary for the original note-only records.
    /// Keep the old run check before reservation validation: unrelated runs
    /// need not have readable reservation fields for this run to replay.
    fn from_legacy_note(note: &str, run: &str) -> Result<Option<Self>, PublicationHistoryError> {
        let value: Value =
            serde_json::from_str(note).map_err(|_| PublicationHistoryError::History)?;
        if value["run_id"] != run {
            return Ok(None);
        }
        let reserved_delay_seconds = value["settling"]["reserved_delay_seconds"]
            .as_u64()
            .ok_or(PublicationHistoryError::Reservation)?;
        let published_head = serde_json::from_value(value["published_head"].clone())
            .map_err(|_| PublicationHistoryError::Head)?;
        let decision = serde_json::from_value(value["settling"]["decision"].clone())
            .unwrap_or(PublicationSettlingDecision::Refused);
        let successful_push = if reserved_delay_seconds > 0 {
            serde_json::from_value(value["successful_push"].clone())
                .map_err(|_| PublicationHistoryError::History)?
        } else {
            None
        };
        let observation = Self {
            run_id: PublicationProofValue::Text(run.into()),
            published_head,
            successful_push,
            reserved_delay_seconds,
            decision,
        };
        observation.validate()?;
        Ok(Some(observation))
    }
}
