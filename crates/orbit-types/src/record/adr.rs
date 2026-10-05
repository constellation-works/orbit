//! Architecture Decision Record (ADR) types: status lifecycle, legacy
//! validation hints, the [`Adr`] struct itself, and ID helpers.
//!
//! ## ADR Status Lifecycle
//!
//! ADRs move through a small, restrictive state machine — unlike tasks (which
//! are permissive by default), ADR transitions are an explicit allowlist.
//!
//! ### Allowed transitions
//! | From       | To         | Notes                                              |
//! |------------|------------|----------------------------------------------------|
//! | Proposed   | Accepted   | Standard promotion path.                           |
//! | Proposed   | Superseded | Withdrawn before acceptance.                       |
//! | Proposed   | Deleted    | Soft-discard a never-accepted decision.            |
//! | Accepted   | Superseded | Replaced by a newer decision.                      |
//! | `X`        | `X`        | Same-state transitions are idempotent no-ops.      |
//!
//! ### Rejected transitions
//! - `Accepted → Proposed` — once accepted, cannot revert to proposed.
//! - `Accepted → Deleted` — accepted decisions must be superseded, not deleted.
//! - `Superseded → *` — terminal.
//! - `Deleted → *` — terminal.
//!
//! See [`AdrStatus::validate_transition`] for the implementation.

use std::fmt::{Display, Formatter};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::RecordError;

/// Current lifecycle state of an ADR.
///
/// See the module-level doc for the full transition table.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum AdrStatus {
    /// Drafted but not yet accepted by the feature owner.
    Proposed,
    /// Active, in-force decision.
    Accepted,
    /// Replaced by a newer ADR (see `superseded_by`). Terminal.
    Superseded,
    /// Soft-discarded before acceptance. Terminal.
    Deleted,
}

impl Display for AdrStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cli_name())
    }
}

impl FromStr for AdrStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "proposed" => Ok(AdrStatus::Proposed),
            "accepted" => Ok(AdrStatus::Accepted),
            "superseded" => Ok(AdrStatus::Superseded),
            "deleted" => Ok(AdrStatus::Deleted),
            other => Err(format!("unknown ADR status: {other}")),
        }
    }
}

impl AdrStatus {
    pub fn cli_name(self) -> &'static str {
        match self {
            AdrStatus::Proposed => "proposed",
            AdrStatus::Accepted => "accepted",
            AdrStatus::Superseded => "superseded",
            AdrStatus::Deleted => "deleted",
        }
    }

    /// Validates a status transition against the ADR allowlist.
    ///
    /// Same-state transitions are idempotent OK. Everything else must match an
    /// allowed edge; otherwise returns [`RecordError::InvalidTransition`].
    pub fn validate_transition(from: AdrStatus, to: AdrStatus) -> Result<(), RecordError> {
        if from == to {
            return Ok(());
        }

        match (from, to) {
            (AdrStatus::Proposed, AdrStatus::Accepted)
            | (AdrStatus::Proposed, AdrStatus::Superseded)
            | (AdrStatus::Proposed, AdrStatus::Deleted)
            | (AdrStatus::Accepted, AdrStatus::Superseded) => Ok(()),
            (AdrStatus::Accepted, AdrStatus::Proposed) => Err(RecordError::InvalidTransition(
                format!("{from} -> {to} (accepted ADRs cannot revert to proposed)"),
            )),
            (AdrStatus::Accepted, AdrStatus::Deleted) => Err(RecordError::InvalidTransition(
                format!("{from} -> {to} (accepted ADRs must be superseded, not deleted)"),
            )),
            (AdrStatus::Superseded, _) => Err(RecordError::InvalidTransition(format!(
                "{from} -> {to} (superseded is terminal)"
            ))),
            (AdrStatus::Deleted, _) => Err(RecordError::InvalidTransition(format!(
                "{from} -> {to} (deleted is terminal)"
            ))),
            _ => Err(RecordError::InvalidTransition(format!(
                "{from} -> {to} (not an allowed transition)"
            ))),
        }
    }
}

/// Whether an ADR has been flagged with legacy-validation warnings.
///
/// `Warned` means [`Adr::validation_warnings`] is non-empty and the record was
/// admitted despite failing one or more soft checks.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum LegacyValidation {
    /// No legacy validation warnings; record passed all checks.
    #[default]
    None,
    /// One or more soft validation warnings recorded in `validation_warnings`.
    Warned,
}

impl Display for LegacyValidation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LegacyValidation::None => "none",
            LegacyValidation::Warned => "warned",
        };
        write!(f, "{s}")
    }
}

impl FromStr for LegacyValidation {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(LegacyValidation::None),
            "warned" => Ok(LegacyValidation::Warned),
            other => Err(format!("unknown legacy validation: {other}")),
        }
    }
}

/// Architecture Decision Record.
///
/// See the ADR artifact store under `.orbit/adrs/` for persisted envelopes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Adr {
    pub id: String,
    pub title: String,
    pub status: AdrStatus,
    pub owner: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<DateTime<Utc>>,
    pub last_updated: DateTime<Utc>,
    #[serde(default)]
    pub related_features: Vec<String>,
    #[serde(default)]
    pub related_tasks: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Legacy ID aliases. Array per ADR-002 (amended) to support rollup
    /// aliasing where multiple legacy IDs collapse into a single canonical ADR.
    #[serde(default)]
    pub legacy_ids: Vec<String>,
    /// Soft-validation warnings recorded during ingestion. See ADR-011.
    #[serde(default)]
    pub validation_warnings: Vec<String>,
    /// Summary flag mirroring whether `validation_warnings` is populated.
    /// See ADR-011.
    #[serde(default)]
    pub legacy_validation: LegacyValidation,
}

/// Validates a canonical ADR ID of form `ADR-NNNN` (at least 4 zero-padded
/// digits).
///
/// Rejects empty strings, missing prefix, lowercase prefix, fewer than 4
/// digits, or any non-digit suffix character. Uses character checks rather
/// than the `regex` crate so this stays usable in runtime code paths.
pub fn validate_adr_id(id: &str) -> Result<(), RecordError> {
    if id.is_empty() {
        return Err(RecordError::Invalid("ADR id must not be empty".to_string()));
    }

    let suffix = id.strip_prefix("ADR-").ok_or_else(|| {
        RecordError::Invalid(format!(
            "ADR id '{id}' must start with 'ADR-' (uppercase prefix)"
        ))
    })?;

    if suffix.len() < 4 {
        return Err(RecordError::Invalid(format!(
            "ADR id '{id}' must have at least 4 digits after 'ADR-'"
        )));
    }

    if !suffix.chars().all(|c| c.is_ascii_digit()) {
        return Err(RecordError::Invalid(format!(
            "ADR id '{id}' suffix must contain only ASCII digits"
        )));
    }

    Ok(())
}

/// Formats a legacy, feature-scoped ADR ID used in the markdown design docs,
/// e.g. `legacy_id_for("activity-job", 17) == "activity-job/ADR-017"`.
///
/// The local number is zero-padded to 3 digits to match the existing
/// `<feature>/ADR-NNN` markdown convention. Wider numbers are emitted at their
/// natural width.
pub fn legacy_id_for(feature: &str, local_number: u32) -> String {
    format!("{feature}/ADR-{local_number:03}")
}
