//! Task writes spanning the canonical bundle, registry index and host database.
//! Coordinated transitions follow the commit and replay protocol in `coordination`.
//! Ordinary mutations and bulk maintenance retain their own lock and rollback rules.

pub(crate) mod coordination;
pub(crate) mod import;
pub(crate) mod journal;
mod operations;
pub(crate) mod reindex;
pub(crate) mod restore;
pub(crate) mod v2;
pub(crate) mod v2_bundle;

pub use coordination::{AdmissionContext, TaskCommitBoundary, TaskCoordination, admission_refusal};
pub use v2::TaskV2Store;

#[cfg(test)]
pub(crate) mod tests;

/// Log a failed undo step of a rollback. Rollback runs while the caller is
/// already returning the original error, so a failing step is reported here
/// instead of replacing that error or vanishing.
fn log_rollback_failure<T, E: std::fmt::Display>(step: &'static str, result: Result<T, E>) {
    if let Err(error) = result {
        orbit_common::tracing::warn!(
            target: "orbit.store.task_rollback",
            step,
            error = %error,
            "task rollback step failed; run `orbit task reindex` if the task index disagrees with the bundles on disk",
        );
    }
}
