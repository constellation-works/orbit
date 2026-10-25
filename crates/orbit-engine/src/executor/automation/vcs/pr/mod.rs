//! PR automation split across focused seams for maintainability. `attribution`
//! owns Review/Done actor labels, `body` owns PR rendering, `open` owns
//! create-or-reuse, `promote` owns the explicit Review handoff, `merge` owns
//! approved-PR merge, remote cleanup, and Done reconciliation, and `delivery`
//! owns the candidate identity and merge evidence completion runs behind.
//! `answers` parses the provider's PR answers once into the types every
//! reader takes.

mod answers;
mod attribution;
mod auto_fix;
mod body;
mod complete;
mod delivery;
pub(in crate::executor::automation::vcs) mod diagnostics;
mod head_lag;
mod infrastructure;
mod labels;
mod merge;
mod open;
mod promote;
mod recovery;
pub(in crate::executor::automation::vcs) mod source_identity;

pub use answers::PullRequestStatus;
pub(in crate::executor::automation::vcs) use answers::{CheckState, ReviewDecision};
pub(in crate::executor::automation::vcs) use answers::{
    RequiredChecks, read_required_checks_observed,
};
pub(in crate::executor::automation::vcs) use answers::{read_pr_status, read_pr_status_before};
pub(in crate::executor::automation) use attribution::ship_done_attribution;
pub(crate) use auto_fix::{
    AUTHENTICATED_PUBLISHED_HEAD_SHA, apply_authenticated_published_head,
    provider_authenticated_published_head, recovery_published_head,
};
pub(in crate::executor::automation::vcs) use body::{
    meaningful_execution_summary, render_acceptance_criteria, task_url,
};
pub(in crate::executor::automation) use complete::pr_complete;
pub(in crate::executor::automation::vcs) use delivery::{
    BlockedCheckWait, DeliveryPin, OwedHostedChecks, PrMergeState, UnreportedChecks,
    checked_pr_state, classify_pr_state, owed_hosted_checks,
};
pub(in crate::executor::automation) use merge::git_merge;
pub(in crate::executor::automation::vcs) use merge::resolve_permitted_merge;
pub(in crate::executor::automation) use open::pr_open;
pub(in crate::executor::automation::vcs) use open::{
    close_earlier_task_pull_requests, open_or_reuse_unchecked,
};
pub(in crate::executor::automation) use promote::pr_promote;
pub(crate) use recovery::completion_recovery_checks;
