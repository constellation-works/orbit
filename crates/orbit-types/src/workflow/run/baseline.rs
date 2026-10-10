//! A required validation command that fails on the candidate's base exactly
//! as it fails on the candidate [ORB-14258].
//!
//! The integration branch has no merge gates of its own, so a check can be red
//! on the base every delivery starts from. A candidate that inherits that
//! failure did not cause it, and no repair of the candidate fixes it: step and
//! final recovery skip it, no `[BLOCKED]` PR is opened, and the task is held in
//! the backlog (a claimed leaf releases its claim) until the base moves to a
//! commit where the command passes.
//!
//! The failure text and the hold's task-history note carry the same typed
//! [`BaselineRedHold`], written as JSON straight after
//! [`BASELINE_RED_MARKER`], so every consumer — failure handoff, run
//! finalization, claim settlement and admission — reads the hold from the
//! text it already has.

use serde::{Deserialize, Serialize};

/// Error code of a required validation failure the base shares.
pub const BASELINE_RED_ERROR_CODE: &str = "baseline_red";

/// The bracketed marker form of [`BASELINE_RED_ERROR_CODE`].
pub const BASELINE_RED_MARKER: &str = "[baseline_red]";

/// Task history event that moves a task to `backlog` under a
/// [`BaselineRedHold`]. Admission withholds the task while the hold stands.
pub const BASELINE_RED_HOLD_EVENT: &str = "baseline_red_hold";

/// Whether a step failure says its required validation is red on the base.
#[must_use]
pub fn is_baseline_red_failure(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(BASELINE_RED_ERROR_CODE)
        || message.is_some_and(|message| message.contains(BASELINE_RED_MARKER))
}

/// Which base a required command failed on, and the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineRedHold {
    /// The ref the run synchronized onto (`origin/agent-main`, say). The hold
    /// lifts only after this ref points at a new commit where the command
    /// passes. Empty when the run did not report it; admission keeps the hold
    /// until the base ref can be resolved.
    #[serde(default)]
    pub base_ref: String,
    /// The base commit the command failed on.
    pub base_sha: String,
    /// The required command, as configured.
    pub command: String,
    /// The run that observed the failure.
    #[serde(default)]
    pub run_id: String,
    /// What the failing run reported testing, for a command that selects its
    /// own tests [ORB-15131]. A moved base is checked on this selection, so a
    /// tip that would select nothing of its own cannot lift the hold. Absent
    /// for a command that reports no selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<serde_json::Value>,
}

impl BaselineRedHold {
    /// `detail` prefixed with the marker and this hold, the form both the
    /// step failure and the hold note use.
    #[must_use]
    pub fn text(&self, detail: &str) -> String {
        let hold = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        format!("{BASELINE_RED_MARKER} {hold} {}", detail.trim())
    }

    /// The hold a failure message or history note carries, if any.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let (_, rest) = text.split_once(BASELINE_RED_MARKER)?;
        serde_json::Deserializer::from_str(rest.trim_start())
            .into_iter::<Self>()
            .next()?
            .ok()
            .filter(|hold| !hold.base_sha.trim().is_empty() && !hold.command.trim().is_empty())
    }
}
