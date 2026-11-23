//! Requeue a blocked task onto the candidate its stopped run published.
//!
//! A run whose failure handoff published a `[BLOCKED]` pull request leaves
//! the task blocked on an escalation. Returning it to the backlog by hand
//! used to strand it: the next run found a publication it could not
//! authenticate and refused, and only archive and refile got the work moving.
//!
//! A requeue records exactly what the published-owner resume path
//! authenticates: a `requeued` final-recovery decision for the stopped run,
//! which supersedes its escalation, and the publication-preservation record
//! for the handoff's pull request. Nothing here trusts the requeue itself:
//! the record is written only after the handoff receipt authenticates in full
//! (owner-local source, conclusively stopped, newest execution, no discard,
//! unchanged task specification), and the next run re-authenticates it from
//! the immutable source before adopting the branch. Everything after
//! adoption — validation, review, landing — runs afresh.
//!
//! A claimed leaf publishes nothing through a failure handoff: its claim's
//! settlement keeps its committed candidate on the owner, which records it on
//! the task and hands it to the task's next run, claimed or owner-local.
//! A requeue reports the candidate the next admission offers — the latest
//! settlement that preserved one, which a later claim that kept none does not
//! replace — read from the same record admission reads, and refuses it, as a
//! published one, once the task's specification changed.
//!
//! Two callers share [`OrbitRuntime::write_candidate_requeue`]: the operator
//! command (`orbit task requeue`, `orbit.task.requeue`) and the cleared-infra
//! requeue of a transient infrastructure block (`transient_infra`).
//! Plain attributed blocked/review-to-backlog updates authenticate through
//! the same publication contract and append preservation atomically with
//! their status change.
//!
//! A run that published its own pull request through its pipeline steps and
//! then escalated has no handoff receipt: its failure handoff kept that pull
//! request open (`review_completion_failure`, `landing_review_failure`). A
//! requeue resumes it the same way, once the completion-failure receipt names
//! the run, the task and its unchanged specification, the checkpoints
//! authenticate the pull request open and unmerged at the recorded head, and
//! every required check on that head has passed. Anything else is refused
//! with nothing written, naming the exit that applies. A block-triage option
//! whose rewrite changes that specification discards the candidate instead,
//! including when the checks are not green. A live run is still refused, and
//! that refusal is judged on the unchanged task.
//!
//! A requeue waits for the task's run to stop: final recovery blocks the task
//! before the run's failure handoff publishes, and a requeue in between would
//! bind nothing to the pull request the handoff then opens. A task with a
//! pull request or preserved candidate on record that no handoff receipt
//! binds is refused, as its next admission would refuse it.
//!
//! A specification edited since the publication is refused: that candidate
//! implements the old task. The refusal names the operator CLI route,
//! `orbit task update <id> --status backlog --discard-candidate`, which is
//! CLI-only: `orbit.task.update` does not accept `discard_candidate`. That
//! command records the discard, returns the task to the backlog so the next
//! run implements fresh, and closes the task's undelivered Orbit pull requests.
//! A blocked task's requeue carries the same discard (the dashboard and
//! `orbit task requeue <id> --discard-candidate`), and a block-triage decision
//! answer whose rewrite changes the specification discards the candidate with
//! it, so the answer is not refused.
//!
//! The decision and preservation records are written by `system`, since the
//! published-owner resume trusts only Orbit's own records; the operator's
//! name and reason are on the status change and its comment.
mod authentication;
mod candidate;
mod operator;
#[cfg(test)]
mod tests;

use orbit_types::task::{TaskComment, TaskHistoryEntry};

pub(crate) use authentication::current_run_requeue_stands;
use operator::operator_discard_hint;

/// Status event of an operator's requeue of a blocked task.
pub(crate) const OPERATOR_REQUEUED_EVENT: &str = "operator_requeued";

/// The publication a failure handoff recorded, read from its receipt without
/// authenticating it. It names what a requeue would resume; the requeue
/// itself authenticates before writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandoffReceipt {
    pub(crate) run_id: String,
    pub(crate) pull_request: String,
    pub(crate) branch: String,
    pub(crate) head_sha: String,
    pub(crate) task_spec_digest: String,
}

/// What a requeue of a blocked task writes beside its status change.
pub(crate) struct CandidateRequeue {
    pub(crate) actor: String,
    pub(crate) status_event: &'static str,
    pub(crate) status_note: String,
    /// The final-recovery decision kind recorded with `requeued`, which is
    /// written only together with a preservation record.
    pub(crate) decision: &'static str,
    pub(crate) comment: Option<TaskComment>,
    /// The publication-preservation record the next run authenticates.
    pub(crate) preserved: Option<TaskHistoryEntry>,
    pub(crate) history: Vec<TaskHistoryEntry>,
}
