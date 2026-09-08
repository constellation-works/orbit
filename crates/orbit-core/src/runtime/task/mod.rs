//! Task reservation and run-coupling behavior.

use std::path::Path;

use orbit_common::fs::selector::canonical_selector_in_workspace;

mod block_on_run_failure;
pub(crate) mod locks;
mod reservation_cleanup;

#[cfg(test)]
mod tests;

pub(crate) use block_on_run_failure::{failed_run_error_context, is_workflow_failure_state};
pub use reservation_cleanup::StaleTaskReservation;

pub(crate) fn canonicalize_context_files_for_read(
    candidates: &[String],
    workspace_root: &Path,
) -> Vec<String> {
    candidates
        .iter()
        .filter_map(|entry| canonical_selector_in_workspace(entry, workspace_root).ok())
        .collect()
}
