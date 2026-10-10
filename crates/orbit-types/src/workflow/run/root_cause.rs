//! What a run's unsuccessful outcome actually was [ORB-15202].
//!
//! A gate or auto parent fails in `pipeline_success_guard` whenever a child
//! does, so one leaf failure used to read as three failed runs, each nesting
//! the text of the one below. A cascaded parent failure records the leaf it
//! echoes as a typed [`RunRootCause`] instead, so a reader can fold it into
//! one incident.
//!
//! Some unsuccessful leaves are not failures at all: a required check red on
//! the base kept the candidate, and a before-landing review refusal left the
//! pull request open for a recorded decision. [`HeldFailure`] names those, so
//! the run ends `held` rather than `failed`.

use serde::{Deserialize, Serialize};

use super::baseline::{BASELINE_RED_ERROR_CODE, is_baseline_red_failure};
use crate::workflow::review::REVIEW_LANDING_DECISION_PENDING;

/// Error code a held run's diagnostic step records when a before-landing
/// review refusal left its pull request open for a recorded decision.
pub const REVIEW_DECISION_PENDING_ERROR_CODE: &str = "review_decision_pending";

/// Error code a parent's diagnostic step records when it ends `cancelled`
/// because a child it waited on was cancelled.
pub const CHILD_CANCELLED_ERROR_CODE: &str = "child_cancelled";

/// The leaf failure a cascaded parent failure echoes.
///
/// Every field is always serialized, null when unknown, so readers can rely
/// on the shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRootCause {
    /// The run that failed on its own, not because a child did.
    pub leaf_run_id: String,
    /// The task that run delivered, when its input names one.
    #[serde(default)]
    pub task_id: Option<String>,
    /// The leaf step that failed, when it is known.
    #[serde(default)]
    pub step: Option<String>,
    /// The typed failure code, when the leaf recorded one.
    #[serde(default)]
    pub code: Option<String>,
}

/// A run outcome that stopped delivery without a failure: the run ends
/// `held` and its parents' success guards pass it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldFailure {
    /// A required check fails on the base exactly as on the candidate; the
    /// candidate is kept and its task held until the base passes.
    BaselineRed,
    /// A before-landing review did not approve; the pull request stays open
    /// and unmerged until a recorded decision lands it.
    DecisionPending,
}

impl HeldFailure {
    /// The hold an unsuccessful run's diagnostic names, if it is one.
    #[must_use]
    pub fn of(error_code: Option<&str>, message: Option<&str>) -> Option<Self> {
        if is_baseline_red_failure(error_code, message) {
            return Some(Self::BaselineRed);
        }
        let pending = error_code == Some(REVIEW_DECISION_PENDING_ERROR_CODE)
            || message.is_some_and(|message| message.contains(REVIEW_LANDING_DECISION_PENDING));
        pending.then_some(Self::DecisionPending)
    }

    /// The error code the held run's diagnostic step records.
    #[must_use]
    pub fn error_code(self) -> &'static str {
        match self {
            Self::BaselineRed => BASELINE_RED_ERROR_CODE,
            Self::DecisionPending => REVIEW_DECISION_PENDING_ERROR_CODE,
        }
    }
}
