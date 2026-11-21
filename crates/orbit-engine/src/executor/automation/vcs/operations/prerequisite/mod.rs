//! GitHub owns effective policy, workflow identities and source dependency reads.
//! The engine receives verified generic evidence; no workflow is special-cased.

mod evidence;
mod observe;
mod snapshot;
mod support;
mod workflow;

pub(super) use evidence::evidence;
pub(super) use observe::observe;
pub(super) use snapshot::{
    PullRequestSelector, Snapshot, checked_run, poll_interval_ms, snapshot, snapshot_once,
};
pub(super) use support::{api, complete_list, encoded, unreadable};
pub(super) use workflow::{gate_passed, gate_present, workflow};
