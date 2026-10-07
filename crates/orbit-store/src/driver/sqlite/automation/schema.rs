//! Append-only schema registry for automation records.

use crate::Store;
use crate::driver::sqlite::migration::FeatureMigration;
use orbit_common::OrbitError;

/// This feature's schema ledger name.
pub(crate) const FEATURE: &str = "automation";

/// Append-only schema registry for this feature.
pub(crate) const MIGRATIONS: &[FeatureMigration] = &[
    FeatureMigration::new(1, "consumer_checkpoints_and_coverage", |conn| {
        conn.execute_batch("CREATE TABLE automation_consumers (consumer TEXT PRIMARY KEY, generation INTEGER NOT NULL, state_json TEXT NOT NULL);
            CREATE TABLE automation_coverage (batch_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, batch_json TEXT NOT NULL, receipt_json TEXT NOT NULL, accepted_at TEXT NOT NULL);
            CREATE INDEX automation_coverage_consumer ON automation_coverage(consumer, accepted_at);
            CREATE TABLE automation_delivery_intents (record_id TEXT PRIMARY KEY, repository TEXT NOT NULL, branch TEXT NOT NULL, delivery_json TEXT NOT NULL);
            CREATE TABLE automation_delivery_members (repository TEXT NOT NULL, branch TEXT NOT NULL, commit_id TEXT NOT NULL, record_id TEXT NOT NULL, PRIMARY KEY(repository,branch,commit_id,record_id));
            CREATE TABLE automation_waivers (batch_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, batch_json TEXT NOT NULL, waiver_json TEXT NOT NULL);
            CREATE TABLE automation_job_keys (workspace_id TEXT NOT NULL, action_key TEXT NOT NULL, run_id TEXT NOT NULL, PRIMARY KEY(workspace_id,action_key));")
                    .map_err(|e| OrbitError::Store(e.to_string()))
    }),
    FeatureMigration::new(2, "retry_lineage_index", |conn| {
        conn.execute_batch(
                    "CREATE INDEX IF NOT EXISTS job_runs_retry_lineage ON job_runs(workspace_id,retry_source_run_id)",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))
    }),
    FeatureMigration::new(3, "consumer_recovery_records", |conn| {
        conn.execute_batch(
                    "CREATE TABLE automation_recoveries (record_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, recorded_at TEXT NOT NULL, record_json TEXT NOT NULL);
            CREATE INDEX automation_recoveries_consumer ON automation_recoveries(consumer, recorded_at);",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))
    }),
    FeatureMigration::new(
        4,
        "release_stale_source_failures",
        super::members::release_stale_source_failures,
    ),
];

pub(crate) fn initialize(store: &Store) -> Result<(), OrbitError> {
    store.apply_feature_migrations(FEATURE, MIGRATIONS)
}
