//! Telling a red base and a network flake apart from a candidate's own
//! failure [ORB-14258].
//!
//! The integration branch has no merge gates, so a required command can be
//! red on the very base every candidate starts from, and a check that calls
//! out to the network can fail for a reason no commit controls. Neither says
//! anything about the candidate. Both validation steps therefore run each
//! required command through [`run_validation_command`], which reruns a
//! network-inconclusive failure (curl status `000`, DNS, a TLS timeout) up to
//! [`NETWORK_RETRIES`] times with backoff, and judge a failure that survives
//! through [`compare_candidate_with_base`]:
//!
//! - the command is rerun on the candidate's synchronized base in a clean,
//!   detached worktree of that commit (a conclusive comparable full cache
//!   is reused first; otherwise identified non-timeout test failures replay
//!   alone with executed-identity proof), under the managed worktree root and
//!   never inside the Git common directory, whose Linux protection scan
//!   refuses the symlinks a checkout can hold;
//! - the base result is cached in the repository's Git common directory per
//!   `(base, command)` behind an exclusive file lock, so concurrent
//!   candidates on one host share a single base run. That lock's file also
//!   records the run's scratch checkout, so worktree GC can reclaim the
//!   checkout's host validation target while holding it;
//! - a comparable completed base failure with the same exit status and
//!   every candidate failure identity makes the
//!   failure typed `[baseline_red]`: no recovery repairs it, and the
//!   task is held until the base moves to a commit where the command passes
//!   ([`baseline_hold_status`]);
//! - unless every failure the candidate names is a test that also fails on
//!   the base ([`known_red`]) [ORB-15375]: the candidate introduced none of
//!   them, so it is not held, and the step records the known-red tests and
//!   proceeds. Review settlement applies the same rule to a reviewer's
//!   red-base claim [ORB-15546];
//! - missing identities, candidate-specific identities and timeouts cannot
//!   establish reproduction: both logs remain evidence for candidate repair;
//! - a comparable passing base allows one bounded candidate replay of only
//!   the failed tests ([`super::flaky`]); tests passing that isolated replay
//!   are recorded as flaky, and failures that remain are rejected as before.
//!
//! A base run that cannot be set up, or whose own result is a missing tool or
//! a network failure, is inconclusive and also leaves the failure the
//! candidate's. A base stopped by the output capture limit or by a signal is
//! never cached as a conclusive result and reports an infrastructure failure with the validation
//! environment marker, rather than attributing a verdict to either revision.
//! Owner claim settlement retains its inconclusive attempted runs separately,
//! so retries converge without restarting a deadline-cut build. These records
//! never establish known red and ordinary base comparisons ignore them.
//!
//! A command that selects what to test from the candidate's diff
//! (`make ci-test-affected`) selects nothing on the base itself, where that
//! diff is empty [ORB-15131]. When the candidate run reported its selection
//! (see [`super::required_command`]), the base run is handed that selection,
//! which also enters the cache key. A base result that ran another selection,
//! reported none, or passed without executing a counted test is *not
//! comparable* ([`compare::not_comparable`]): it neither makes the failure
//! `baseline_red` nor refutes a reviewer's claim, and never lifts a hold that
//! recorded the selection.
//!
//! A reviewer's red-base claim is checked through the same machinery in
//! [`super::base_claim`], which may hand a base run only the candidate's
//! failing tests ([`FAILING_TESTS_KEY`]) [ORB-15328].

mod cache;
mod command;
mod compare;
mod hold;

// Preserve the existing module surface even for helpers used only by this split.
#[cfg(target_os = "linux")]
pub(super) use cache::recorded_base_runs;
#[allow(unused_imports)]
pub(super) use cache::{CachePaths, cache_paths};
pub(super) use cache::{cached_base_check, in_detached_worktree};
#[allow(unused_imports)]
pub(super) use command::NETWORK_RETRIES;
pub(super) use command::{
    network_inconclusive, run_validation_command, run_validation_command_until,
};
pub use compare::BaseCommandResult;
pub(crate) use compare::FAILING_TESTS_KEY;
pub(super) use compare::{
    BaselineCheck, baseline_red_failure, candidate_failure, compare_candidate_with_base,
    compare_claimed_with_base_until, compare_with_base_until, hold_failures, known_red,
};
pub use hold::{BaselineHoldStatus, baseline_hold_status, recorded_baseline_hold_status};
