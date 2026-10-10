//! Task reservation and run-coupling behavior.

use std::path::Path;

use orbit_common::fs::selector::canonical_selector_in_workspace;

mod block_on_run_failure;
pub(crate) mod locks;
pub(crate) mod provider_hold;
mod reservation_cleanup;

#[cfg(test)]
mod tests;

pub use block_on_run_failure::InfraBlockedTask;
pub(crate) use block_on_run_failure::resumed_task_run_id;
pub use reservation_cleanup::StaleTaskReservation;

/// History event a final-recovery requeue records; cleanup preserves that
/// decision and the applier counts these events toward its requeue bound.
pub const FINAL_RECOVERY_REQUEUED_EVENT: &str = "final_recovery_requeued";

impl crate::OrbitRuntime {
    /// Whether a task's linked run may be read from this machine's store.
    ///
    /// Run IDs are machine-local. An explicit binding must match this
    /// runtime's identity; an unknown identity cannot establish a match.
    /// Legacy records without a machine binding retain their local behavior.
    pub fn task_run_is_local(&self, task: &orbit_types::task::Task) -> bool {
        task.job_run_machine.as_ref().is_none_or(|location| {
            self.automation_machine_identity() == Some(location.machine_id.as_str())
        })
    }
}

/// One task's declared context selectors, canonicalized against a workspace
/// root without consulting the filesystem for the target's existence.
///
/// This is the shared non-pruning footprint calculation: task reads,
/// projections, reservations, and status-derived locks all resolve a
/// declaration the same way, and the admission work that freezes a claim's
/// footprint reuses it rather than recomputing a checkout-dependent surface.
/// A selector for a file or symbol the task has not created yet is a valid
/// declaration and stays in `retained`; only selectors that cannot be
/// canonicalized at all — malformed, or escaping the repository boundary —
/// land in `invalid`, where callers report them instead of silently dropping
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredContextFiles {
    /// Canonical selectors, missing targets included.
    pub retained: Vec<String>,
    /// Declarations that could not be canonicalized, verbatim as stored.
    pub invalid: Vec<String>,
}

/// Canonicalize a declared context set without dropping selectors whose
/// filesystem target does not exist.
pub fn declared_context_files(
    candidates: &[String],
    workspace_root: &Path,
) -> DeclaredContextFiles {
    let mut declared = DeclaredContextFiles::default();
    for entry in candidates {
        match canonical_selector_in_workspace(entry, workspace_root) {
            Ok(canonical) => declared.retained.push(canonical),
            Err(_) => declared.invalid.push(entry.clone()),
        }
    }
    declared
}

pub(crate) fn canonicalize_context_files_for_read(
    candidates: &[String],
    workspace_root: &Path,
) -> Vec<String> {
    declared_context_files(candidates, workspace_root).retained
}
