//! [ORB-15956] What the owner's landing does when a pull request's hosted
//! check fails on its pinned head.
//!
//! The landing already waits on the pull request's checks. A failed one used
//! to stop the candidate in review until an operator rescued it, even when
//! the failure was not the candidate's. Each failed job is now classified
//! from its failed-step log, with the CI sweep's own log read and error
//! signature:
//!
//! - **inherited**: the same signature fails on the candidate's base commit,
//!   or the base branch has moved past that base and the same job passes on
//!   its tip. The landing merges the current base into the pull request's
//!   branch once, through the provider's branch update, and waits for the
//!   checks again on the new head. No agent runs. A failure that is still
//!   there after that one update, on a base tip where the same job passes,
//!   is the candidate's: the landing stops for the one automatic repair,
//!   with the failed job, its signature and a log excerpt, unless that
//!   repair is already spent, in which case it stops for a requeue (below).
//!   An unmoved base that is still red stops in review without a repair.
//! - **transient**: the log shows the network or the runner failing, by the
//!   rule the validation gates already rerun on. The landing reruns the
//!   failed jobs once and waits again.
//! - **candidate**: the failed step names a diagnostic, and the base's own
//!   jobs were read and do not explain it. The landing stops for the same
//!   single automatic repair a conflicting base gets, with the failed step's
//!   signature and log excerpt in the stop evidence the repair leaf reads.
//!   [ORB-16890] Once that repair is spent — the claim is the repair, or the
//!   task already returned to the backlog for one — it stops for a requeue
//!   instead: the owner ends the claim and returns the task to the backlog,
//!   keeping the candidate and this evidence for the task's next run, within
//!   the task's landing repair budget.
//! - **inconclusive**: the evidence for any of the above could not be read:
//!   a job query or log read failed, a log read recovered no text, or a log
//!   names no diagnostic, so its signature is only the failed step's name.
//!   A failed read is not a successful empty answer, and neither proves the
//!   base or the candidate at fault, so the landing stops in review and
//!   spends nothing.
//!
//! [ORB-16185] A required check that no failed job answers, such as a
//! provider app's aggregate result beside passing analysis jobs, is not
//! hidden by the jobs that did fail: every failed required check is
//! classified. Its evidence is its own failure annotations
//! ([`annotations`]); they make it a **candidate** failure only when each
//! annotated source is a file the candidate changed from its base. Anything
//! else, including annotations on files the candidate did not change, is
//! **inconclusive**. A failed check branch protection does not require keeps
//! its previous handling.
//! A stale rollup entry for a job already rerun or updated can wait for its
//! replacement to register, but never hides a separately failed aggregate.
//!
//! The rerun, the branch update and the repair are each spent once. A
//! failure past that budget, other than the candidate's own, or one this
//! module cannot classify, stops the landing in review, with the
//! classification in the stop evidence. The
//! repair an inherited failure earns after its one update is that same
//! single repair, not a second update or a second repair.
//!
//! A branch update moves the pull request's head off the accepted candidate.
//! [`verified_base_update`] accepts the new head only when it is a merge of
//! that candidate with a commit already on the landing branch whose tree is
//! exactly the clean merge of the two, so nothing but the base's own advance
//! reaches the merge. The provider merges the base as it is when the update
//! runs, so the caller refreshes the landing ref before asking. The owner's
//! accepted candidate, its validation evidence and the observation the store
//! rechecks stay the validated commit.
//!
//! [`verified_base_update`]: base_update::verified_base_update

mod annotations;
mod base_tip;
mod base_update;
mod head;
mod jobs;
mod log;
mod recovery;

pub(in crate::executor::automation::vcs) use base_tip::base_tip_check_evidence;
pub(in crate::executor::automation::vcs) use base_update::verify_published_base_update;
pub(in crate::executor::automation::vcs) use head::{
    HeadCheckEvidence, head_check_evidence, unmoved_red_base,
};
pub(in crate::executor::automation::vcs) use recovery::{
    CheckRecovery, CheckRecoveryStep, failed_checks, recover_failed_checks,
};
