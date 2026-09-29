use super::super::*;

#[test]
fn run_archive_stage_migration_adds_archived_at() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");
    assert!(table_has_column(&conn, "job_runs", "archived_at").expect("archived_at column"));
}

fn allocations(conn: &Connection) -> Vec<(String, String)> {
    conn.prepare("SELECT workspace_id, run_id FROM job_run_id_allocations ORDER BY 1, 2")
        .expect("prepare allocations")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query allocations")
        .collect::<Result<_, _>>()
        .expect("allocation rows")
}

/// A store upgraded from v32 reserves every run id it can still attribute to
/// a workspace: live and archived-stage rows, and ids an automation key names
/// after its run row was deleted.
#[test]
fn run_id_allocations_backfill_live_runs_and_orphaned_automation_keys() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");
    conn.execute_batch(
        // Every ledger key above v32, by range, so the next migration does
        // not red this fixture.
        "DELETE FROM schema_meta WHERE key LIKE 'migration.v%' AND key > 'migration.v0032';
         DROP TABLE job_run_id_allocations;
         INSERT INTO job_runs(run_id, workspace_id, job_id, attempt, state, scheduled_at, created_at)
         VALUES ('jrun-20260101-0000-t1', 'ws_a', 'job', 1, 'success', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                ('jrun-20260101-0000-c1', 'ws_b', 'job', 1, 'pending', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         CREATE TABLE automation_job_keys (workspace_id TEXT NOT NULL, action_key TEXT NOT NULL, run_id TEXT NOT NULL, PRIMARY KEY(workspace_id,action_key));
         INSERT INTO automation_job_keys VALUES ('ws_a', 'key-1', 'jrun-20260101-0000-t2');",
    )
    .expect("rewind to v32 with run history");
    assert_eq!(current_schema_version(&conn).expect("version"), 32);

    apply_schema(&conn).expect("reopen applies the reservations");

    assert_eq!(
        allocations(&conn),
        vec![
            ("ws_a".to_string(), "jrun-20260101-0000-t1".to_string()),
            ("ws_a".to_string(), "jrun-20260101-0000-t2".to_string()),
            ("ws_b".to_string(), "jrun-20260101-0000-c1".to_string()),
        ]
    );
    apply_schema(&conn).expect("a current store reopens unchanged");
    assert_eq!(allocations(&conn).len(), 3);
}

fn job_run_index_names(conn: &Connection) -> Vec<String> {
    conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='job_runs' ORDER BY name",
    )
    .expect("prepare index names")
    .query_map([], |row| row.get(0))
    .expect("query index names")
    .collect::<Result<_, _>>()
    .expect("index names")
}

/// A store upgraded from v33 gains the per-job and retry-children indexes, and
/// reopening it changes nothing further.
#[test]
fn upgrading_from_v33_adds_the_job_created_and_retry_indexes() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");
    conn.execute_batch(
        "DELETE FROM schema_meta WHERE key LIKE 'migration.v%' AND key > 'migration.v0033';
         DROP INDEX idx_job_runs_ws_job_created;
         DROP INDEX idx_job_runs_ws_retry_created;",
    )
    .expect("rewind to v33");
    assert_eq!(current_schema_version(&conn).expect("version"), 33);
    for name in [
        "idx_job_runs_ws_job_created",
        "idx_job_runs_ws_retry_created",
    ] {
        assert!(!job_run_index_names(&conn).contains(&name.to_string()));
    }

    apply_schema(&conn).expect("reopen applies the indexes");

    let indexes = job_run_index_names(&conn);
    for name in [
        "idx_job_runs_ws_job_created",
        "idx_job_runs_ws_retry_created",
    ] {
        assert!(indexes.contains(&name.to_string()), "{indexes:?}");
    }
    apply_schema(&conn).expect("a current store reopens unchanged");
    assert_eq!(job_run_index_names(&conn), indexes);
}

/// The indexes are additive: a binary that predates them opens the upgraded
/// database and keeps writing it, and the indexes stay consistent under its
/// writes because SQLite maintains them.
#[test]
fn a_binary_without_the_job_run_indexes_still_writes_the_upgraded_database() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");
    let before_indexes = ledger::MIGRATIONS
        .iter()
        .position(|migration| migration.name == "job_runs_job_created_and_retry_indexes")
        .expect("the index migration is registered");
    let older = &ledger::MIGRATIONS[..before_indexes];
    assert!(
        older
            .last()
            .is_some_and(|migration| migration.version as usize == before_indexes)
    );

    let forward = ledger::run_migrations(&conn, older)
        .expect("an older registry opens the upgraded database")
        .expect("the database is newer than the older registry");
    assert!(
        forward.writable,
        "an additive upgrade keeps older writers safe"
    );

    conn.execute(
        "INSERT INTO job_runs(run_id, workspace_id, job_id, attempt, state, scheduled_at, created_at, retry_source_run_id)
         VALUES ('older-child', 'ws', 'job', 1, 'pending', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'older-source')",
        [],
    )
    .expect("an older binary's write");
    let via_index: String = conn
        .query_row(
            "SELECT run_id FROM job_runs INDEXED BY idx_job_runs_ws_retry_created \
             WHERE workspace_id='ws' AND retry_source_run_id='older-source'",
            [],
            |row| row.get(0),
        )
        .expect("the new index holds the older binary's row");
    assert_eq!(via_index, "older-child");
}
