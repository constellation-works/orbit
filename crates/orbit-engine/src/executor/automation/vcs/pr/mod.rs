//! PR automation split across focused seams for maintainability. `attribution`
//! owns Review/Done actor labels, `body` owns PR rendering, `open` owns
//! create-or-reuse, `promote` owns the explicit Review handoff, and `merge`
//! owns approved-PR merge, remote cleanup, and Done reconciliation.

mod attribution;
mod body;
mod complete;
mod merge;
mod open;
mod promote;

#[cfg(test)]
mod tests;

pub(in crate::executor::automation) use attribution::ship_done_attribution;
pub(in crate::executor::automation::vcs) use body::meaningful_execution_summary;
pub(in crate::executor::automation) use complete::pr_complete;
pub(in crate::executor::automation) use merge::git_merge;
pub(in crate::executor::automation::vcs) use open::open_or_reuse_unchecked;
pub(in crate::executor::automation) use open::pr_open;
pub(in crate::executor::automation) use promote::pr_promote;
