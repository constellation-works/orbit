//! Legacy auto-task scheduling cursor contracts.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// On-disk shape of `<orbit_dir>/state/auto-tasks.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AutoTaskCursorState {
    /// Definition name → cursor.
    #[serde(default)]
    pub definitions: BTreeMap<String, AutoTaskCursor>,
    /// Inactive-plugin skips already warned about in this workspace. Each
    /// clock tick is a new process, so the record lives here, not in memory.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub inactive_plugin_warnings: BTreeSet<InactivePluginWarning>,
}

/// One plugin-inactive auto-task skip that has been warned about once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct InactivePluginWarning {
    /// Skipped definition name.
    pub definition: String,
    /// Plugin namespace that seeded the definition.
    pub plugin: String,
    /// Seeded plugin version.
    pub version: String,
}

/// One definition's scheduling cursor on this host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoTaskCursor {
    /// First-observed slot (RFC 3339, UTC): the exclusive floor for the very
    /// first fire, so a definition never mints tasks for slots predating its
    /// registration here.
    pub baseline_at: String,
    /// Most recently consumed scheduled slot (RFC 3339, UTC), when the
    /// definition has fired at least once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_slot: Option<String>,
    /// Wall-clock time of the last fire (RFC 3339, UTC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<String>,
    /// Task id minted by the last fire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_task_id: Option<String>,
    /// Durable in-flight slot claim. Present between claim and a successful
    /// consumed-slot checkpoint so a retry can reconcile or refuse to remint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<AutoTaskPendingClaim>,
    /// Most recent `skip_if_unchanged` decision, so an operator surface can
    /// explain why a due definition minted nothing. Cleared by the next fire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_skip: Option<AutoTaskSkipRecord>,
}

/// One recorded mint-time precondition skip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoTaskSkipRecord {
    /// Wall-clock time of the skip (RFC 3339, UTC).
    pub at: String,
    /// Scheduled slot that was left unconsumed.
    pub slot: String,
    /// Machine-readable reason token, e.g. `unchanged_since_last_sweep`.
    pub reason: String,
    /// Branch whose tip was compared.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Cursor commit recorded by the last completed sweep.
    pub cursor_sha: String,
    /// Tip commit of `ref` at the time of the skip.
    pub tip_sha: String,
    /// Sweep task the cursor was read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_task_id: Option<String>,
}

/// In-flight admission evidence for one scheduled slot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoTaskPendingClaim {
    /// Slot this host has claimed (RFC 3339, UTC).
    pub slot: String,
    /// Task id when mint succeeded but the consumed-slot checkpoint has not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}
