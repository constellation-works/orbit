//! `orbit run auto --auto-fix`: the completion watcher's repair hand-off.
//!
//! `--auto-fix` authorizes repair, never merging. Completion keeps watching
//! the pull request it already watches and reads its merge state the same
//! way; only a `BLOCKED` state is read more finely here:
//!
//! - a required review, an unavailable review decision or an unreadable
//!   check rollup is an administrative blocker: the watcher stops and no
//!   agent runs;
//! - a conclusive required check still running waits, within the ordinary
//!   wait budget;
//! - failed checks are filtered to the ones branch
//!   protection requires on that head (an optional failure alone leaves the
//!   state `UNSTABLE`, which is mergeable, and beside a `BLOCKED` state it is
//!   not the blocker); none required, or an unreadable answer, stops. The
//!   required failures' jobs and logs are read on the exact head the rollup
//!   reports ([`head_check_evidence`]); a check no job answers, an unreadable
//!   job or log, or a runner or network failure stops without an agent. A
//!   failed job's log read the provider throttled (an authenticated rate
//!   limit) instead waits, read-only and recorded, for the provider's reset
//!   or a bounded backoff, never past the completion deadline
//!   ([`defer_throttled_read`]) [ORB-20178]. A
//!   required check run outside a workflow (a provider app's aggregate) is
//!   repaired only from its authenticated source failure annotations, which
//!   are retained with the task before the repair is admitted [ORB-16185].
//!   Before repair admission, current-base push jobs decide whether to refresh
//!   a stale PR without an agent attempt or hold a matching known-red failure.
//!   The latest push run of each workflow on that exact tip supplies the jobs;
//!   historical fork-point logs are unnecessary for a green-tip refresh.
//!   A base commit whose CI skipped the job, or lists no such job, says
//!   nothing about it, so that read moves back along the base branch's
//!   first-parent history to the newest commit whose same job ran to success
//!   or failure. The inherited-base hold also waits while the moved tip
//!   cannot prove the failure fixed (its same job pending, skipped, missing
//!   or unreadable), and a head an earlier poll recorded as inherited keeps
//!   that hold through a read it cannot repeat. Other failures retain the
//!   ordinary repair path.
//!
//! An optional-only failure first passes the narrowly evidenced prerequisite
//! recovery in `infrastructure`: one durable rerun, no crew or source repair.
//!
//! A `DIRTY` pull request takes the existing conflict path (pinned rebase,
//! `pr_conflict_recovery`, lease-checked push, or a re-review round for a
//! reviewed head).
//!
//! Every repair is admitted by the host first ([`admit`]): the run's crew is
//! drawn once from `workflow.purpose_crews.auto_fix` (the low complexity
//! pool while that purpose is unset) and the pull request's
//! durable ledger dedupes the head and blocker and bounds the attempts. A
//! check repair is then handed to the step's repair activity as a failure
//! led by [`orbit_types::workflow::PR_AUTO_FIX_CHECK_REPAIR_MARKER`]; the executor routes it to the
//! job's `step_failure_recovery`, commits the repair and reruns completion.
//! That rerun finds the repair on the branch ([`publish_local_repair`]),
//! runs the workspace's required validation on it, and publishes it on the
//! pull request (or, for a reviewed head, hands it to the re-review round)
//! before it watches the new head. The published head is recorded in the
//! ledger, so a later round or a resume pins the head this run published.
//!
//! [`head_check_evidence`]: super::super::landing::checks::head_check_evidence

mod admission;
mod checks;
mod ledger;
mod publication;
mod published_head;
mod read_deferral;
mod verdict;

pub(super) use admission::{AutoFix, admit};
pub(super) use checks::{
    CheckRepair, base_tip_checks, recover_ci_rerun, red_base_wait, repair_failed_checks,
};
pub(super) use ledger::{
    attach_evidence, record_base_path, record_landed_repair, record_published_repair,
    reserve_base_refresh,
};
pub(super) use publication::{LocalRepair, publish_local_repair};
pub(super) use published_head::adopt_published_repairs;
pub(crate) use published_head::{
    AUTHENTICATED_PUBLISHED_HEAD_SHA, apply_authenticated_published_head,
    provider_authenticated_published_head, recovery_published_head,
};
pub(super) use read_deferral::{defer_throttled_read, owed_read_wait};
pub(super) use verdict::{
    BlockedStopReason, BlockedVerdict, UNSETTLED_BLOCKED_POLLS, blocked_verdict,
    hold_auto_fix_stop, stopped, stopped_by,
};
