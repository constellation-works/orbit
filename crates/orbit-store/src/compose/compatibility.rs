//! The state compatibility this build was compiled with.

use orbit_common::fs::generation::CompatibilityIdentity;

use crate::driver::sqlite::migration::FeatureMigration;
use crate::driver::sqlite::{automation, job_run_store, migration, review};

/// What upgrade admission compares against the live participants: the store
/// schema and workspace layout ledgers, and every feature schema this binary
/// migrates the host store to.
pub fn compiled_compatibility() -> CompatibilityIdentity {
    let features = [
        (automation::FEATURE, automation::MIGRATIONS),
        (review::FEATURE, review::MIGRATIONS),
        (
            job_run_store::pull::FEATURE,
            job_run_store::pull::MIGRATIONS,
        ),
    ]
    .into_iter()
    .map(|(feature, migrations)| (feature.to_string(), newest(migrations)))
    .collect();
    CompatibilityIdentity {
        store_schema: migration::schema_compatibility(),
        workspace_layout: crate::workflow::layout::layout_compatibility(),
        features,
    }
}

fn newest(migrations: &[FeatureMigration]) -> u32 {
    migrations
        .iter()
        .map(|migration| migration.version())
        .max()
        .unwrap_or(0)
}
