//! Landing a handoff through its pull request: check waits, recovery and the merge.

mod checks;
mod merge;
mod poll;

pub(super) use poll::land_pull_request;
