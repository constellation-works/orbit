//! Append-only schema registry for review records.

use orbit_common::OrbitError;

use crate::Store;
use crate::driver::sqlite::migration::FeatureMigration;

/// This feature's schema ledger name.
pub(crate) const FEATURE: &str = "review";

/// Append-only schema registry for this feature.
pub(crate) const MIGRATIONS: &[FeatureMigration] = &[
    FeatureMigration::new(1, "lineages_certificates_landings", |conn| {
        conn.execute_batch(
                "CREATE TABLE review_lineages (workspace_id TEXT NOT NULL, lineage_key TEXT NOT NULL, revision INTEGER NOT NULL, ledger_json TEXT NOT NULL, PRIMARY KEY(workspace_id, lineage_key));
                 CREATE TABLE review_certificates (attempt_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, repository TEXT NOT NULL, candidate_commit TEXT NOT NULL, candidate_tree TEXT NOT NULL, passed INTEGER NOT NULL, certificate_json TEXT NOT NULL, issued_at TEXT NOT NULL);
                 CREATE INDEX review_certificates_tree ON review_certificates(repository, candidate_tree, issued_at);
                 CREATE TABLE review_landings (record_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL, landed_commit TEXT NOT NULL, landing_json TEXT NOT NULL, recorded_at TEXT NOT NULL);
                 CREATE INDEX review_landings_attempt ON review_landings(attempt_id, recorded_at);",
            )
            .map_err(|error| OrbitError::Store(error.to_string()))
    }),
    FeatureMigration::new(2, "lineage_holder_run", |conn| {
        conn.execute_batch(
            "ALTER TABLE review_lineages ADD COLUMN holder_run_id TEXT;
         CREATE INDEX review_lineages_holder ON review_lineages(workspace_id, holder_run_id);",
        )
        .map_err(|error| OrbitError::Store(error.to_string()))
    }),
];

pub(crate) fn initialize(store: &Store) -> Result<(), OrbitError> {
    store.apply_feature_migrations(FEATURE, MIGRATIONS)
}
