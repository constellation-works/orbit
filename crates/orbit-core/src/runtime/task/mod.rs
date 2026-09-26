//! Task reservation and run-coupling behavior.

use std::path::Path;

use orbit_common::fs::selector::canonical_selector_in_workspace;

mod block_on_run_failure;
pub(crate) mod locks;
mod reservation_cleanup;

#[cfg(test)]
mod tests;

pub use block_on_run_failure::InfraBlockedTask;
pub use reservation_cleanup::StaleTaskReservation;

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
