//! A delivery push the forge kept refusing for a reason that is the forge's,
//! not the candidate's [ORB-14617].
//!
//! A GitHub incident can refuse every push with `[remote rejected] …
//! (Internal Server Error)` for minutes while reads and the API keep working.
//! No repair of the candidate changes that, so once the push's own bounded
//! backoff is spent the run does not fail: it ends `held` at the push step
//! with a [`ForgeUnavailableHold`] in its state. Step and final recovery and
//! the failure handoff do not run, the task stays in progress, and the
//! candidate, its worktree and the review certificate are kept. The clock
//! resumes the held run from its checkpoints, so the retry pushes the same
//! head and continues to `pr_open` without implementing or reviewing again.
//!
//! The push operation writes the hold as JSON straight after
//! [`FORGE_UNAVAILABLE_MARKER`] in its failure text, the form the engine
//! reads it back from.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Error code of a run step held because the forge refused its push.
pub const FORGE_UNAVAILABLE_ERROR_CODE: &str = "forge_unavailable";

/// The bracketed marker form of [`FORGE_UNAVAILABLE_ERROR_CODE`].
pub const FORGE_UNAVAILABLE_MARKER: &str = "[forge_unavailable]";

/// Task history event that blocks a task whose held push outlasted the
/// clock's retry window. Its note names the held run (`run=<id>`), so
/// resuming that run by hand re-admits the task.
pub const FORGE_UNAVAILABLE_EXPIRED_EVENT: &str = "forge_unavailable_expired";

/// Whether a step failure says the forge refused its push past the budget.
#[must_use]
pub fn is_forge_unavailable(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(FORGE_UNAVAILABLE_ERROR_CODE)
        || message.is_some_and(|message| message.contains(FORGE_UNAVAILABLE_MARKER))
}

/// Which push the forge refused, and for how long.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeUnavailableHold {
    /// The remote ref the push targeted (`refs/heads/<branch>`).
    pub target_ref: String,
    /// The candidate head the push carried. A retry pushes this commit.
    pub head_sha: String,
    /// Push attempts the holding run made.
    pub attempts: u32,
    /// Backoff the holding run waited between those attempts, in milliseconds.
    pub waited_ms: u64,
    /// The forge's last refusal, bounded.
    #[serde(default)]
    pub diagnostic: String,
    /// The pipeline step that held. The engine fills it in.
    #[serde(default)]
    pub step_id: String,
    /// When this run held.
    pub held_at: DateTime<Utc>,
    /// When the first run of this retry lineage held. A resumed run that
    /// holds again keeps it, so the clock's retry window is bounded from the
    /// start of the outage rather than renewed by every retry.
    pub held_since: DateTime<Utc>,
}

impl ForgeUnavailableHold {
    /// `detail` prefixed with the marker and this hold.
    #[must_use]
    pub fn text(&self, detail: &str) -> String {
        let hold = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        format!("{FORGE_UNAVAILABLE_MARKER} {hold} {}", detail.trim())
    }

    /// The hold a failure message carries, if any.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let (_, rest) = text.split_once(FORGE_UNAVAILABLE_MARKER)?;
        serde_json::Deserializer::from_str(rest.trim_start())
            .into_iter::<Self>()
            .next()?
            .ok()
            .filter(|hold| !hold.target_ref.trim().is_empty() && !hold.head_sha.trim().is_empty())
    }
}
