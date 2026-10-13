//! The state compatibility this build was compiled with.

use std::collections::BTreeMap;

use orbit_common::fs::generation::CompatibilityIdentity;

use crate::driver::sqlite::{migration, task_registry};

/// What upgrade admission compares against the live participants: the store
/// schema, task registry and workspace layout ledgers, and every feature
/// schema store migration v49 froze, with its newest breaking migration.
/// Binaries from before v49 still compare the feature versions and floors.
pub fn compiled_compatibility() -> CompatibilityIdentity {
    let mut features = BTreeMap::new();
    let mut feature_floors = BTreeMap::new();
    for (feature, newest, floor) in migration::feature_compatibility() {
        features.insert(feature.to_string(), newest);
        feature_floors.insert(feature.to_string(), floor);
    }
    CompatibilityIdentity {
        store_schema: migration::schema_compatibility(),
        task_registry: task_registry::registry_compatibility(),
        workspace_layout: crate::workflow::layout::layout_compatibility(),
        features,
        feature_floors,
    }
}
