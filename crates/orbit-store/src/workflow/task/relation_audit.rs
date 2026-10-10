//! Audit canonical task relations for targets an index rebuild cannot
//! resolve.
//!
//! The bundles are read rather than the generated relation rows: a rebuild
//! that fails on a dangling edge never publishes that edge's row, so an
//! index-only audit goes blind exactly when the edge is blocking repair.

use orbit_common::OrbitError;

use crate::driver::sqlite::task_registry::{DanglingRelationTarget, TaskRegistryStore};
use crate::repository::task::v2_bundle::TaskBundleStoreV2;

/// Every canonical relation edge in `workspace_id` (or in every bound
/// workspace when `None`) whose locally known task target has no registered
/// bundle — the edges that make an index rebuild fail its validator.
///
/// Reads each registered envelope once, skipping a bundle a concurrent
/// writer holds; an unreadable envelope is reported as an error rather than
/// audited as clean.
pub fn audit_relation_targets(
    registry: &TaskRegistryStore,
    workspace_id: Option<&str>,
) -> Result<Vec<DanglingRelationTarget>, OrbitError> {
    let partitions = match workspace_id {
        Some(id) => vec![id.to_string()],
        None => registry.partition_ids()?.into_iter().collect(),
    };
    let mut dangling = Vec::new();
    for partition_id in partitions {
        let store = TaskBundleStoreV2::new(registry.clone(), partition_id.clone());
        let mut envelopes = Vec::new();
        for binding in registry.tasks_for_workspace(&partition_id)? {
            if let Some(envelope) = store.read_envelope_if_settled(&binding.task_id)? {
                envelopes.push(envelope);
            }
        }
        dangling.extend(registry.unresolved_relation_targets(&partition_id, &envelopes)?);
    }
    Ok(dangling)
}
