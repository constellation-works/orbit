//! The durable ledger behind `orbit run auto --auto-fix`.
//!
//! With auto-fix authorized, the completion watcher may send a pull request
//! whose required checks failed, or whose branch conflicts with its base, to
//! the run's existing repair pipeline. Every dispatch it admits is one
//! [`PrAutoFixAttempt`], recorded in the history of the bundle's primary task
//! (its lowest id) under the task's write lock before any agent runs, and
//! every repaired head the watcher publishes is one [`PrAutoFixPush`]. The
//! history is the record restarts, resumes and later runs read, so the per-PR
//! budget and the per-head deduplication survive all of them.

use serde::{Deserialize, Serialize};

use crate::task::TaskHistoryEntry;

/// Repairs one pull request may receive across every head, run and restart.
/// Exhausting it stops the watcher with diagnostics; nothing escalates.
pub const PR_AUTO_FIX_BUDGET: u32 = 3;

/// History event recording one admitted repair dispatch.
pub const PR_AUTO_FIX_ADMITTED_EVENT: &str = "pr_auto_fix_admitted";

/// History event recording a repaired head the watcher published.
pub const PR_AUTO_FIX_PUSHED_EVENT: &str = "pr_auto_fix_pushed";

/// Leads the completion failure that hands an admitted check repair to the
/// step's repair activity. Only the watcher writes it, after admission.
pub const PR_AUTO_FIX_CHECK_REPAIR_MARKER: &str = "pr_auto_fix_check_repair";

/// The configuration key auto-fix repairs draw their crew from: the auto-fix
/// purpose pool, which falls back to the low complexity pool while unset,
/// through the ordinary activity-crew draw.
pub const PR_AUTO_FIX_CREWS_KEY: &str = "workflow.purpose_crews.auto_fix";

/// What blocks the pull request's merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrAutoFixBlocker {
    /// A required hosted check failed on the head.
    FailedChecks,
    /// The branch conflicts with its base.
    Conflict,
}

impl PrAutoFixBlocker {
    /// The stable token recorded in the ledger.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FailedChecks => "failed_checks",
            Self::Conflict => "conflict",
        }
    }
}

/// One admitted repair dispatch, as recorded in task history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrAutoFixAttempt {
    pub pr_number: String,
    pub head_sha: String,
    pub blocker: PrAutoFixBlocker,
    /// 1-based position of this attempt in the pull request's budget.
    pub attempt: u32,
    pub budget: u32,
    pub run_id: String,
    /// The crew the run's auto-fix draw selected.
    pub crew: String,
    /// The draw's source: the pool, or the fallback and filters it applied.
    pub crew_source: String,
}

/// One repaired head the watcher published, as recorded in task history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrAutoFixPush {
    pub pr_number: String,
    pub from_head_sha: String,
    pub to_head_sha: String,
    pub blocker: PrAutoFixBlocker,
    pub run_id: String,
}

/// What the ledger allows for one requested repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrAutoFixDecision {
    /// Dispatch, as the attempt to record.
    Admit(PrAutoFixAttempt),
    /// The same run continues the conflict repair it was already admitted
    /// for on this head; nothing new is recorded or spent.
    Continue(PrAutoFixAttempt),
    /// A repair was already dispatched for this head and blocker.
    Duplicate(PrAutoFixAttempt),
    /// The pull request spent its whole budget.
    Exhausted(Vec<PrAutoFixAttempt>),
}

/// A repair the watcher asks the ledger to admit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrAutoFixClaim<'a> {
    pub pr_number: &'a str,
    pub head_sha: &'a str,
    pub blocker: PrAutoFixBlocker,
    pub run_id: &'a str,
}

/// Every attempt recorded for `pr_number`, oldest first. An entry whose note
/// does not parse is not an attempt.
pub fn pr_auto_fix_attempts(
    history: &[TaskHistoryEntry],
    pr_number: &str,
) -> Vec<PrAutoFixAttempt> {
    ledger_notes(history, PR_AUTO_FIX_ADMITTED_EVENT)
        .filter(|attempt: &PrAutoFixAttempt| attempt.pr_number == pr_number)
        .collect()
}

/// Every repaired head published for `pr_number`, oldest first.
pub fn pr_auto_fix_pushes(history: &[TaskHistoryEntry], pr_number: &str) -> Vec<PrAutoFixPush> {
    ledger_notes(history, PR_AUTO_FIX_PUSHED_EVENT)
        .filter(|push: &PrAutoFixPush| push.pr_number == pr_number)
        .collect()
}

fn ledger_notes<'a, T: serde::de::DeserializeOwned + 'a>(
    history: &'a [TaskHistoryEntry],
    event: &'a str,
) -> impl Iterator<Item = T> + 'a {
    history
        .iter()
        .filter(move |entry| entry.event == event)
        .filter_map(|entry| serde_json::from_str(entry.note.as_deref()?).ok())
}

/// Decide `claim` against the attempts already recorded for its pull request.
///
/// A head and blocker are repaired at most once: a later poll, restart or
/// run that meets them again is a duplicate. The one exception is the run
/// that holds a conflict repair on that head, whose post-recovery retry
/// re-enters the watcher before anything is published. Otherwise a new
/// attempt is admitted while the budget lasts.
pub fn decide_pr_auto_fix(
    attempts: &[PrAutoFixAttempt],
    claim: &PrAutoFixClaim<'_>,
    budget: u32,
    crew: &str,
    crew_source: &str,
) -> PrAutoFixDecision {
    if let Some(previous) = attempts
        .iter()
        .rev()
        .find(|attempt| attempt.head_sha == claim.head_sha && attempt.blocker == claim.blocker)
    {
        if claim.blocker == PrAutoFixBlocker::Conflict && previous.run_id == claim.run_id {
            return PrAutoFixDecision::Continue(previous.clone());
        }
        return PrAutoFixDecision::Duplicate(previous.clone());
    }
    let spent = u32::try_from(attempts.len()).unwrap_or(u32::MAX);
    if spent >= budget {
        return PrAutoFixDecision::Exhausted(attempts.to_vec());
    }
    PrAutoFixDecision::Admit(PrAutoFixAttempt {
        pr_number: claim.pr_number.to_string(),
        head_sha: claim.head_sha.to_string(),
        blocker: claim.blocker,
        attempt: spent + 1,
        budget,
        run_id: claim.run_id.to_string(),
        crew: crew.to_string(),
        crew_source: crew_source.to_string(),
    })
}

/// Whether a step failure is the watcher's handoff of an admitted check
/// repair: the marker, then the pull request it names. Wrappers may prefix
/// the message, so the marker need not lead it. Only the authoritative
/// failure text counts: a generic CLI exit's stderr tail is provider output
/// and may quote the marker without the watcher having admitted anything.
pub fn is_pr_auto_fix_check_repair(message: &str) -> bool {
    let authoritative = crate::workflow::failure_classification_text(message);
    authoritative
        .match_indices(PR_AUTO_FIX_CHECK_REPAIR_MARKER)
        .any(|(at, marker)| authoritative[at + marker.len()..].starts_with(": pull request #"))
}
