//! CI failure identity evidence readers.

use serde::{Deserialize, Serialize};

use super::TaskDecision;
use crate::task::TaskHistoryEntry;

/// Immutable CI failure identity carried by a sweep task's creation history.
/// Description edits do not change the failure or its supplying runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CiFailureIdentity {
    /// Generated failure key, without the tag prefix.
    pub failure_key: String,
    /// Digest of the complete compiler cause, including source coordinates.
    pub compiler_cause_digest: Option<String>,
    /// Digest of the complete compiler diagnostic set without coordinates.
    pub open_compiler_set: Option<String>,
    /// Normalized signature as rendered for people; absent for a step fallback.
    /// Compiler failures retain this for display and match on their cause digest.
    pub signature: Option<String>,
    /// Commit actually checked out by the supplying job.
    pub tested_commit: String,
    /// Every supplying run/job/checkout, including sources omitted from display.
    pub sources: Vec<CiFailureSource>,
}

/// One job that supplied a CI failure, bound to its observed checkout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CiFailureSource {
    pub run_id: String,
    pub job_id: String,
    pub checkout: String,
}

/// A foreign creation decision cannot be interpreted as CI failure identity.
#[derive(Debug, thiserror::Error)]
#[error("creation history carries a mismatched decision")]
pub struct CiFailureIdentityError;

impl CiFailureIdentity {
    /// Read only the decision on the task's creation event. `Ok(None)` allows
    /// the caller's isolated legacy description reader; a foreign decision
    /// refuses that fallback. Later history events cannot replace identity.
    pub fn from_history(
        history: &[TaskHistoryEntry],
    ) -> Result<Option<Self>, CiFailureIdentityError> {
        match history
            .iter()
            .find(|entry| entry.event == "created")
            .and_then(|entry| entry.decision.as_ref())
        {
            Some(TaskDecision::CiFailureIdentity(identity)) => Ok(Some(identity.clone())),
            Some(_) => Err(CiFailureIdentityError),
            None => Ok(None),
        }
    }
}
