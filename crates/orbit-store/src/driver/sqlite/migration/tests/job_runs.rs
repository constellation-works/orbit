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
