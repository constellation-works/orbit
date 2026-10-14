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
mod head_lag;
mod infrastructure;
mod merge;
mod open;
mod promote;

pub(in crate::executor::automation::vcs) use answers::CheckState;
pub use answers::PullRequestStatus;
pub(in crate::executor::automation::vcs) use answers::read_pr_status;
pub(in crate::executor::automation) use attribution::ship_done_attribution;
pub(in crate::executor::automation::vcs) use body::meaningful_execution_summary;
pub(in crate::executor::automation) use complete::pr_complete;
pub(in crate::executor::automation::vcs) use delivery::{
    DeliveryPin, OwedHostedChecks, PrMergeState, classify_pr_state, owed_hosted_checks,
};
pub(in crate::executor::automation) use merge::git_merge;
pub(in crate::executor::automation::vcs) use merge::resolve_permitted_merge;
pub(in crate::executor::automation::vcs) use open::open_or_reuse_unchecked;
pub(in crate::executor::automation) use open::pr_open;
pub(in crate::executor::automation) use promote::pr_promote;
