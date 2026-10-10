//! The state compatibility this build was compiled with.

use std::collections::BTreeMap;

use orbit_common::fs::generation::{CompatibilityIdentity, FeatureFloor};

use crate::driver::sqlite::migration::FeatureMigration;
use crate::driver::sqlite::{automation, job_run_store, migration, review};

/// What upgrade admission compares against the live participants: the store
/// schema and workspace layout ledgers, and every feature schema this binary
/// migrates the host store to with its newest breaking migration.
pub fn compiled_compatibility() -> CompatibilityIdentity {
    let registries = [
        (automation::FEATURE, automation::MIGRATIONS),
        (review::FEATURE, review::MIGRATIONS),
        (
            job_run_store::pull::FEATURE,
            job_run_store::pull::MIGRATIONS,
        ),
    ];
    let mut features = BTreeMap::new();
    let mut feature_floors = BTreeMap::new();
    for (feature, migrations) in registries {
        features.insert(feature.to_string(), newest(migrations));
        feature_floors.insert(feature.to_string(), floor(migrations));
    }
    CompatibilityIdentity {
        store_schema: migration::schema_compatibility(),
        workspace_layout: crate::workflow::layout::layout_compatibility(),
        features,
        feature_floors,
    }
}

fn newest(migrations: &[FeatureMigration]) -> u32 {
    migrations
        .iter()
        .map(|migration| migration.version())
        .max()
        .unwrap_or(0)
}

/// The newest migration older binaries cannot run beside.
fn floor(migrations: &[FeatureMigration]) -> FeatureFloor {
    migrations
        .iter()
        .filter(|migration| migration.is_breaking())
        .max_by_key(|migration| migration.version())
        .map_or(
            FeatureFloor {
                version: 0,
                name: None,
            },
            |migration| FeatureFloor {
                version: migration.version(),
                name: Some(migration.name().to_string()),
            },
        )
}
