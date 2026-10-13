//! The frozen local-pull feature registry.

use orbit_common::storage::sqlite::sqlite_store_error;

use super::FeatureMigration;

pub(super) const FEATURE: &str = "local_pull";

pub(super) const MIGRATIONS: &[FeatureMigration] = &[FeatureMigration::breaking(
    1,
    "pending_requests_and_unique_leaves",
    |conn| {
        conn.execute_batch("CREATE TABLE local_pull_admissions (
            workspace_id TEXT NOT NULL, owner_machine TEXT NOT NULL,
            owner_workspace TEXT NOT NULL, execution_machine TEXT NOT NULL,
            request_id TEXT NOT NULL, claim_id TEXT, leaf_run_id TEXT, record_json TEXT NOT NULL,
            PRIMARY KEY(workspace_id, owner_machine, owner_workspace, execution_machine, request_id),
            UNIQUE(workspace_id, owner_machine, owner_workspace, claim_id),
            UNIQUE(workspace_id, leaf_run_id));")
            .map_err(sqlite_store_error)
    },
)];
