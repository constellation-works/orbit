//! Legacy auto-task scheduling cursor contracts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// On-disk shape of `<orbit_dir>/state/auto-tasks.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AutoTaskCursorState {
    /// Definition name → cursor.
    #[serde(default)]
    pub definitions: BTreeMap<String, AutoTaskCursor>,
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
