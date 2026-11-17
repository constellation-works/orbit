//! The review watch's [`LANDING_CHECKS_OBSERVED_EVENT`] note: the typed
//! observation it records, the note codec, and the replay key that keeps a
//! repeated watch from recording the same observation twice.
//!
//! The note bytes are a durable contract (the dashboard parses them too), so
//! the codec writes exactly what the untyped writer did: keys in the order
//! `state`, `pr_number`, `head_sha` (`null` when the pull request reported
//! none), then whichever of `reason`, `failed_checks`, `classification`,
//! `detail` and `hold_reason` are present. Replay compares those stored
//! bytes exactly; decoding a historical note is only a view beside them.

use serde::{Deserialize, Serialize};

use super::LANDING_CHECKS_OBSERVED_EVENT;
use crate::task::TaskHistoryEntry;

/// What the review watch saw when the pull request's checks settled or its
/// budget ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LandingChecksObservationState {
    /// The pull request is mergeable.
    Passed,
    /// One or more checks failed.
    Failed,
    /// The pull request conflicts with its base.
    Conflicting,
    /// Something other than a failed check blocks the merge.
    Blocked,
    /// The checks had not settled within the watch budget.
    Pending,
}

/// One review-watch observation, as recorded in a
/// [`LANDING_CHECKS_OBSERVED_EVENT`] history note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedLandingChecksObservation {
    state: LandingChecksObservationState,
    pr_number: String,
    head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failed_checks: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    classification: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hold_reason: Option<String>,
}

impl RecordedLandingChecksObservation {
    /// A `passed`, `conflicting`, `blocked` or `pending` observation of the
    /// pull request at `head_sha`, with the reason it names when not passed.
    pub fn settled(
        state: LandingChecksObservationState,
        pr_number: impl Into<String>,
        head_sha: Option<String>,
        reason: Option<String>,
    ) -> Self {
        Self {
            state,
            pr_number: pr_number.into(),
            head_sha,
            reason,
            failed_checks: None,
            classification: None,
            detail: None,
            hold_reason: None,
        }
    }

    /// A `failed` observation: the failed checks by name and the landing's
    /// classification of them with its detail.
    pub fn failed(
        pr_number: impl Into<String>,
        head_sha: Option<String>,
        failed_checks: Vec<String>,
        classification: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            failed_checks: Some(failed_checks),
            classification: Some(classification.into()),
            detail: Some(detail.into()),
            ..Self::settled(
                LandingChecksObservationState::Failed,
                pr_number,
                head_sha,
                None,
            )
        }
    }

    /// The observation held by the review CI retry path for `reason`.
    pub fn held(mut self, reason: impl Into<String>) -> Self {
        self.hold_reason = Some(reason.into());
        self
    }

    pub fn state(&self) -> LandingChecksObservationState {
        self.state
    }

    pub fn pr_number(&self) -> &str {
        &self.pr_number
    }

    pub fn head_sha(&self) -> Option<&str> {
        self.head_sha.as_deref()
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn failed_checks(&self) -> Option<&[String]> {
        self.failed_checks.as_deref()
    }

    pub fn classification(&self) -> Option<&str> {
        self.classification.as_deref()
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    pub fn hold_reason(&self) -> Option<&str> {
        self.hold_reason.as_deref()
    }

    /// The history note this observation is recorded as.
    pub fn to_note(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// The replay key a history entry must carry to have already recorded
    /// this observation: its exact note.
    pub fn replay_key(&self) -> Result<LandingChecksObservationReplayKey, serde_json::Error> {
        self.to_note().map(LandingChecksObservationReplayKey)
    }
}

/// The exact stored note bytes of a [`LANDING_CHECKS_OBSERVED_EVENT`] entry.
/// Two observations replay as the same only when these bytes are equal; a
/// reordered, reformatted or extended note is a different key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LandingChecksObservationReplayKey(String);

impl LandingChecksObservationReplayKey {
    /// The note as stored.
    pub fn as_note(&self) -> &str {
        &self.0
    }

    /// The note to store.
    pub fn into_note(self) -> String {
        self.0
    }
}

/// A [`LANDING_CHECKS_OBSERVED_EVENT`] history entry, decoded once: its
/// replay key and, when the note parses as an observation, the typed view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalLandingChecksObservation {
    replay_key: LandingChecksObservationReplayKey,
    observation: Option<RecordedLandingChecksObservation>,
}

impl HistoricalLandingChecksObservation {
    /// The review-watch observation `entry` recorded, or `None` for another
    /// event or an entry without a note.
    pub fn from_history_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        if entry.event != LANDING_CHECKS_OBSERVED_EVENT {
            return None;
        }
        let note = entry.note.as_deref()?;
        Some(Self {
            replay_key: LandingChecksObservationReplayKey(note.to_string()),
            observation: serde_json::from_str(note).ok(),
        })
    }

    pub fn replay_key(&self) -> &LandingChecksObservationReplayKey {
        &self.replay_key
    }

    /// The typed observation; `None` for a malformed or partial note, which
    /// never reads as any outcome.
    pub fn observation(&self) -> Option<&RecordedLandingChecksObservation> {
        self.observation.as_ref()
    }
}
