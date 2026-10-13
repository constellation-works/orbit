//! v49 `absorb_feature_ledgers`: the automation, review and local-pull
//! feature registries, frozen, and the store entry that runs each of them to
//! completion.
//!
//! Before v49 a feature store applied its own registry the first time it
//! opened, so a store could lack a feature's tables. The entry runs every
//! registry through the feature engine on the migration's connection, inside
//! the store migration's transaction, so the `feature_schema_meta` rows and
//! `feature.compat.<feature>` records are exactly what a feature open wrote.
//! These registries never grow: the next change to a feature table is a
//! store ledger entry.

use orbit_common::OrbitError;
use rusqlite::Connection;

use super::feature::{FeatureMigration, apply_feature_registry};

mod automation;
mod local_pull;
mod review;

/// Every frozen feature registry, by its `feature_schema_meta` name.
pub(super) const FEATURE_REGISTRIES: &[(&str, &[FeatureMigration])] = &[
    (automation::FEATURE, automation::MIGRATIONS),
    (review::FEATURE, review::MIGRATIONS),
    (local_pull::FEATURE, local_pull::MIGRATIONS),
];

/// v49 body: bring each feature to its final version (automation v5, review
/// v4, local pull v1), skipping what an earlier feature open applied.
pub(super) fn apply_absorb_feature_ledgers(conn: &Connection) -> Result<(), OrbitError> {
    for (feature, migrations) in FEATURE_REGISTRIES {
        apply_feature_registry(conn, feature, migrations)?;
    }
    Ok(())
}
