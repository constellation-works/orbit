//! Scoped hooks and capability fixtures for engine tests.

#[doc(hidden)]
pub mod host;

pub use crate::activity_job::cli_runner::{
    with_bwrap_probe_unavailable, with_cli_supervision_clock,
};
pub use crate::activity_job::job_executor::loop_clock::with_loop_clock;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use crate::executor::automation::vcs::with_host_target_removal_hook;
pub use crate::executor::automation::vcs::{
    with_prerequisite_clock, with_remote_probe_backoff, with_vcs_retry_wait,
    with_worktree_setup_fetch_limits,
};
pub use crate::impl_test_host;

pub use crate::executor::automation::{
    collect_ci_evidence, collect_dependabot_alerts, git_commit, handoff_land, local_shell,
    push_batch_changes, task_complete,
};
