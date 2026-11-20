//! [ORB-25926] A base-suspect failure the base was proven innocent of.

use serde::{Deserialize, Serialize};

/// A base-suspect failure reclassified `candidate` when its base
/// observation ended: the same check passed on the base branch at a commit
/// that contains the candidate's base, with no failure on the current tip.
pub const CI_BASE_SUSPECT_RECLASSIFIED_EVENT: &str = "ci_base_suspect_reclassified";

/// The `ci_base_suspect_reclassified` history note: the failure on the
/// candidate head and the base run that proved the base green.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiBaseSuspectReclassification {
    /// The candidate head that failed.
    pub head: String,
    /// `workflow / job`, the check that failed on the head and passed on the base.
    pub check: String,
    /// The head failure's error signature.
    pub signature: String,
    /// Always `base-suspect`.
    pub from: String,
    /// Always `candidate`.
    pub to: String,
    /// The base-branch push run whose same check passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_run_id: Option<u64>,
    /// The base-branch commit that run tested.
    pub base_sha: String,
    /// The base-branch tip, at or after `base_sha`, read for a same-check failure.
    pub base_tip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
}

impl CiBaseSuspectReclassification {
    /// The JSON note this record is written as.
    pub fn to_history_note(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Parse a history note; `None` for any other shape.
    pub fn from_history_note(note: Option<&str>) -> Option<Self> {
        serde_json::from_str(note?).ok()
    }
}
