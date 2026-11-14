//! The authority record, its history entries and grant requests.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{RESIDENT_AUTHORITY_CHANGED_EVENT, RESIDENT_AUTHORITY_SCHEMA, ResidentRight};
use crate::task::TaskHistoryEntry;

/// The session that relayed a grant request: who submitted it, never who
/// decided it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentRelay {
    /// The relaying caller's writer label.
    pub by: String,
    /// The orchestrator crew the session acts for, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// The relaying model, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The relaying MCP session, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// The exact addition a `resident_authority` decision request asks for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentGrantRequest {
    /// Rights the answer adds.
    #[serde(default)]
    pub rights: Vec<ResidentRight>,
    /// Workspaces the answer adds to the scope.
    #[serde(default)]
    pub workspaces: Vec<String>,
    /// The human's words as relayed, verbatim.
    pub quote: String,
    /// The session that relayed them.
    pub relay: ResidentRelay,
}

impl ResidentGrantRequest {
    /// Whether this request asks for exactly `rights` in `workspaces`.
    pub fn asks_for(&self, rights: &[ResidentRight], workspaces: &[String]) -> bool {
        sorted(&self.rights) == sorted(rights) && sorted(&self.workspaces) == sorted(workspaces)
    }
}

fn sorted<T: Ord + Clone>(values: &[T]) -> Vec<T> {
    let mut values = values.to_vec();
    values.sort();
    values.dedup();
    values
}

/// One change to a mandate's authority, as its history lists it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentAuthorityEntry {
    /// The authority revision this change produced (1 for the first).
    pub revision: u64,
    /// When it was applied.
    pub at: DateTime<Utc>,
    /// Who applied it: the human for a widening, the caller for a narrowing.
    pub by: String,
    /// How: `operator`, `decision:<request id>` or `narrowing`.
    pub via: String,
    /// Rights added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<ResidentRight>,
    /// Workspaces added to the scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces_added: Vec<String>,
    /// Rights removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<ResidentRight>,
    /// Workspaces dropped from the scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces_dropped: Vec<String>,
    /// The mandate deadline this narrowing set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,
    /// The human's words behind a widening.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    /// The session that relayed a decided grant request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<ResidentRelay>,
}

impl ResidentAuthorityEntry {
    /// Whether this change widened the authority.
    pub fn is_widening(&self) -> bool {
        !self.added.is_empty() || !self.workspaces_added.is_empty()
    }
}

/// The note of a [`RESIDENT_AUTHORITY_CHANGED_EVENT`]: the change and the
/// authority it left.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentAuthorityChange {
    /// The change.
    #[serde(flatten)]
    pub entry: ResidentAuthorityEntry,
    /// Rights held after the change.
    pub rights: Vec<ResidentRight>,
    /// Covered workspaces after the change.
    pub workspaces: Vec<String>,
}

impl ResidentAuthorityChange {
    /// The history entry recording this change, written by `entry.by`.
    pub fn history_entry(&self) -> TaskHistoryEntry {
        TaskHistoryEntry::new(
            self.entry.at,
            self.entry.by.clone(),
            RESIDENT_AUTHORITY_CHANGED_EVENT,
        )
        .with_note(serde_json::to_string(self).ok())
    }
}

/// A mandate's authority record, folded from its task history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentAuthority {
    /// Always [`RESIDENT_AUTHORITY_SCHEMA`].
    pub schema: String,
    /// How many changes the record holds; 0 when none was ever applied.
    pub revision: u64,
    /// Rights held, in contract order.
    pub rights: Vec<ResidentRight>,
    /// Covered workspaces.
    pub workspaces: Vec<String>,
    /// Widenings, oldest first.
    pub granted: Vec<ResidentAuthorityEntry>,
    /// Narrowings, oldest first.
    pub narrowed: Vec<ResidentAuthorityEntry>,
}

impl Default for ResidentAuthority {
    fn default() -> Self {
        Self {
            schema: RESIDENT_AUTHORITY_SCHEMA.to_string(),
            revision: 0,
            rights: Vec::new(),
            workspaces: Vec::new(),
            granted: Vec::new(),
            narrowed: Vec::new(),
        }
    }
}

impl ResidentAuthority {
    /// Fold the record from a task's history, oldest first. An entry whose
    /// note does not parse, or whose writer differs from the note's, makes
    /// the record unreadable rather than silently skipped.
    pub fn from_history(history: &[TaskHistoryEntry]) -> Result<Self, String> {
        let mut authority = Self::default();
        for entry in history
            .iter()
            .filter(|entry| entry.event == RESIDENT_AUTHORITY_CHANGED_EVENT)
        {
            let change: ResidentAuthorityChange = entry
                .note
                .as_deref()
                .ok_or_else(|| "an authority change has no record".to_string())
                .and_then(|note| {
                    serde_json::from_str(note)
                        .map_err(|error| format!("an authority change is unreadable: {error}"))
                })?;
            if change.entry.by != entry.by {
                return Err(format!(
                    "authority change {} names `{}` but was written by `{}`",
                    change.entry.revision, change.entry.by, entry.by
                ));
            }
            authority.revision += 1;
            if change.entry.revision != authority.revision {
                return Err(format!(
                    "authority change {} is out of order (expected revision {})",
                    change.entry.revision, authority.revision
                ));
            }
            authority.rights = canonical_rights(&change.rights);
            authority.workspaces = change.workspaces.clone();
            if change.entry.is_widening() {
                authority.granted.push(change.entry);
            } else {
                authority.narrowed.push(change.entry);
            }
        }
        Ok(authority)
    }

    /// Whether the record holds `right`.
    pub fn holds(&self, right: ResidentRight) -> bool {
        self.rights.contains(&right)
    }

    /// Whether the record covers `workspace`.
    pub fn covers(&self, workspace: &str) -> bool {
        self.workspaces
            .iter()
            .any(|candidate| candidate == workspace.trim())
    }
}

/// The authority revision a history has reached: the count of authority
/// change entries. A grant request is bound to it, so any change in between
/// supersedes the request.
pub fn resident_authority_revision(history: &[TaskHistoryEntry]) -> u64 {
    history
        .iter()
        .filter(|entry| entry.event == RESIDENT_AUTHORITY_CHANGED_EVENT)
        .count() as u64
}

/// Rights deduplicated in contract order.
pub fn canonical_rights(rights: &[ResidentRight]) -> Vec<ResidentRight> {
    ResidentRight::ALL
        .into_iter()
        .filter(|right| rights.contains(right))
        .collect()
}
