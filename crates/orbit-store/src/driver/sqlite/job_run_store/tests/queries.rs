use chrono::Utc;
use tempfile::TempDir;

use super::super::SqliteJobRunStore;
use crate::Store;
use crate::contracts::JobRunStoreBackend;

#[test]
fn legacy_role_model_row_loads_as_flat_crew_model() {
    let temp = TempDir::new().expect("tempdir");
    let db_path = temp.path().join("orbit.db");
    drop(Store::open(&db_path).expect("create current schema"));

    let conn = rusqlite::Connection::open(&db_path).expect("open raw db");
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO job_runs(
                run_id, workspace_id, job_id, attempt, state, scheduled_at, created_at,
                resolved_crew, implementer_model
             ) VALUES (?1, ?2, ?3, 1, 'success', ?4, ?4, ?5, ?6)",
        rusqlite::params![
            "legacy-run",
            "ws_a",
            "legacy-job",
            now,
            "legacy-crew",
            "legacy-implementer-model"
        ],
    )
    .expect("insert legacy-shaped row");
    drop(conn);

    let loaded = SqliteJobRunStore::new(
        Store::open(&db_path).expect("reopen migrated store"),
        "ws_a",
    )
    .get_job_run("legacy-run")
    .expect("read legacy run")
    .expect("legacy run exists");

    assert_eq!(loaded.resolved_crew.as_deref(), Some("legacy-crew"));
    assert_eq!(
        loaded.crew_model.as_deref(),
        Some("legacy-implementer-model")
    );
}
