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

use crate::task::{TaskDecision, TaskHistoryEntry};

/// Repairs one pull request may receive across every head, run and restart.
/// Exhausting it stops the watcher with diagnostics; nothing escalates.
pub const PR_AUTO_FIX_BUDGET: u32 = 3;

/// History event recording one admitted repair dispatch.
pub const PR_AUTO_FIX_ADMITTED_EVENT: &str = "pr_auto_fix_admitted";

/// History event recording a repaired head the watcher published.
pub const PR_AUTO_FIX_PUSHED_EVENT: &str = "pr_auto_fix_pushed";

/// History event recording an admitted check repair that made no edit and
/// wrote no scope record: the repair judged the failure not caused by the
/// candidate, so the watcher reruns the failed checks once on the same head.
pub const PR_AUTO_FIX_NO_EDIT_RERUN_EVENT: &str = "pr_auto_fix_no_edit_rerun";

/// One no-edit check repair, as recorded in task history. The repair attempt
/// that produced it already spent one auto-fix attempt for the head; the
/// rerun it triggers additionally reserves the head's shared CI rerun budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrAutoFixNoEditRerun {
    pub pr_number: String,
    pub head_sha: String,
    pub run_id: String,
    pub attempt: u32,
}

/// Every no-edit repair recorded for `pr_number`, oldest first.
pub fn pr_auto_fix_no_edit_reruns(
    history: &[TaskHistoryEntry],
    pr_number: &str,
) -> Vec<PrAutoFixNoEditRerun> {
    ledger_notes(history, PR_AUTO_FIX_NO_EDIT_RERUN_EVENT)
        .filter(|record: &PrAutoFixNoEditRerun| record.pr_number == pr_number)
        .collect()
}

/// History event recording a failed head held for an inherited base failure.
pub const PR_AUTO_FIX_BASE_RED_EVENT: &str = "pr_auto_fix_base_red";

/// A failed head whose repair is held until the base proves it fixed.
///
/// Only `failed_head` supplies hold authority. The optional fields retain the
/// rich base-tip evidence or the minimal poll reason without changing the
/// historical note. New history entries also carry this record in a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrAutoFixBaseRed {
    pub failed_head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl PrAutoFixBaseRed {
    /// A poll's minimal same-head hold and its diagnostic reason.
    #[must_use]
    pub fn for_failed_head(failed_head: String, reason: String) -> Self {
        Self {
            failed_head,
            reason: Some(reason),
            landed_base: None,
            base_branch: None,
            source: None,
            checks: None,
            path: None,
        }
    }

    /// Decode the base-tip evidence boundary, selecting only known-red
    /// evidence. A refresh decision supplies no inherited-failure hold.
    #[must_use]
    pub fn from_base_tip_evidence(evidence: &serde_json::Value) -> Option<Self> {
        let record: Self = serde_json::from_value(evidence.clone()).ok()?;
        (record.path.as_deref() == Some("known_red_base")).then_some(record)
    }

    /// Read a hold once at the task-history boundary. A typed decision wins
    /// over diagnostic text; only an absent decision permits legacy decoding.
    #[must_use]
    pub fn from_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        if entry.event != PR_AUTO_FIX_BASE_RED_EVENT {
            return None;
        }
        match &entry.decision {
            Some(TaskDecision::PrAutoFixBaseRed(record)) => Some(record.clone()),
            Some(_) => None,
            None => Self::from_legacy_note(entry.note.as_deref()?),
        }
    }

    /// Both historical shapes are this record's JSON: rich base-tip evidence
    /// and a minimal failed-head/reason pair. Neither requires a rewrite.
    fn from_legacy_note(note: &str) -> Option<Self> {
        serde_json::from_str(note).ok()
    }
}

/// Leads the completion failure that hands an admitted check repair to the
/// step's repair activity. Only the watcher writes it, after admission.
pub const PR_AUTO_FIX_CHECK_REPAIR_MARKER: &str = "pr_auto_fix_check_repair";

/// The configuration key auto-fix repairs draw their crew from: the auto-fix
/// purpose pool, which falls back to the low complexity pool while unset,
/// through the ordinary activity-crew draw.
pub const PR_AUTO_FIX_CREWS_KEY: &str = "workflow.purpose_crews.auto_fix";

/// What blocks the pull request's merge, or the before-landing review that
/// advanced its published head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrAutoFixBlocker {
    /// A required hosted check failed on the head.
    FailedChecks,
    /// The branch conflicts with its base.
    Conflict,
    /// A before-landing review committed and published a fix on the PR branch.
    LandingReview,
}

impl PrAutoFixBlocker {
    /// The stable token recorded in the ledger.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FailedChecks => "failed_checks",
            Self::Conflict => "conflict",
            Self::LandingReview => "landing_review",
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

/// History event recording one bounded wait for a throttled log read.
pub const PR_AUTO_FIX_READ_DEFERRED_EVENT: &str = "pr_auto_fix_read_deferred";

/// Where a throttled read's not-before came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PrAutoFixReadReset {
    /// The provider named `Retry-After` or a rate-limit reset.
    Provider,
    /// The provider named neither, so the wait is a conservative backoff.
    #[default]
    Unknown,
}

/// One bounded wait for a throttled pull-request log read.
///
/// The history note stays this record's JSON. New entries also store it as
/// [`TaskDecision::PrAutoFixReadDeferred`]. A note written before that
/// decision still parses through [`Self::from_entry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrAutoFixReadDeferral {
    pub execution: String,
    pub pr: String,
    pub head_sha: String,
    #[serde(default)]
    pub repository: Option<String>,
    pub run_id: u64,
    pub job_id: u64,
    /// Absent when the provider's job did not name a run attempt.
    #[serde(default)]
    pub run_attempt: Option<u64>,
    #[serde(default)]
    pub attempt: u64,
    #[serde(default)]
    pub observed_at_ms: u64,
    /// Absent on a legacy note that recorded no not-before. Readers treat
    /// that as a wait that does not fit a deadline.
    #[serde(default)]
    pub not_before_ms: Option<u64>,
    #[serde(default)]
    pub deadline_ms: u64,
    #[serde(default)]
    pub reset: PrAutoFixReadReset,
    #[serde(default)]
    pub retry_after_seconds: Option<u64>,
    #[serde(default)]
    pub reset_epoch_seconds: Option<u64>,
    #[serde(default)]
    pub resource: String,
    #[serde(default)]
    pub principal: String,
    #[serde(default)]
    pub request_id: String,
    /// Absent on a legacy note that recorded no diagnostic.
    #[serde(default)]
    pub diagnostic: Option<String>,
}

impl PrAutoFixReadDeferral {
    /// Not-before instant in milliseconds, or [`u64::MAX`] when a legacy
    /// note omitted it.
    #[must_use]
    pub fn effective_not_before_ms(&self) -> u64 {
        self.not_before_ms.unwrap_or(u64::MAX)
    }

    /// Diagnostic recorded for the wait. A legacy note that omitted one
    /// still surfaces the historical placeholder.
    #[must_use]
    pub fn diagnostic_text(&self) -> &str {
        self.diagnostic.as_deref().unwrap_or("a throttled log read")
    }

    /// The deferral a `pr_auto_fix_read_deferred` entry records.
    ///
    /// A matching decision is authoritative, including when its note
    /// disagrees. An entry with no decision is read from its note. A
    /// decision of another kind is not that note.
    #[must_use]
    pub fn from_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        if entry.event != PR_AUTO_FIX_READ_DEFERRED_EVENT {
            return None;
        }
        match &entry.decision {
            Some(TaskDecision::PrAutoFixReadDeferred(record)) => Some(record.clone()),
            Some(_) => None,
            None => Self::from_legacy_note(entry.note.as_deref()?),
        }
    }

    /// Isolated compatibility reader for a note written before the typed
    /// decision. The note is the record's JSON and nothing else.
    fn from_legacy_note(note: &str) -> Option<Self> {
        serde_json::from_str(note).ok()
    }
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
