use super::super::*;

#[test]
fn coordination_migrations_create_typed_tables_without_touching_existing_records() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    conn.execute_batch(
        r#"
            CREATE TABLE schema_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            INSERT INTO schema_meta VALUES
                ('migration.v0001', 'baseline', '2026-07-01T00:00:00Z'),
                ('migration.v0002', 'learnings_index_workspace_scope', '2026-07-02T00:00:00Z'),
                ('migration.v0003', 'flat_crew_model', '2026-07-03T00:00:00Z'),
                ('migration.v0004', 'job_run_archive_stage', '2026-07-04T00:00:00Z');
            CREATE TABLE audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                execution_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                command TEXT NOT NULL,
                subcommand TEXT,
                tool_name TEXT,
                target_type TEXT,
                target_id TEXT,
                role TEXT NOT NULL,
                status TEXT NOT NULL,
                exit_code INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                working_directory TEXT NOT NULL,
                arguments_json TEXT,
                stdout_truncated TEXT,
                stderr_truncated TEXT,
                error_message TEXT,
                host TEXT,
                pid INTEGER NOT NULL,
                session_id TEXT,
                task_id TEXT,
                job_run_id TEXT,
                activity_id TEXT,
                step_index INTEGER
            );
            CREATE TABLE existing_records (id TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO existing_records VALUES ('keep-me', 'unchanged');
        "#,
    )
    .expect("seed v4 database");

    apply_schema(&conn).expect("apply host registry migration");

    for table in ["hosts", "host_aliases"] {
        assert!(table_exists(&conn, table).expect("table lookup"));
    }
    for column in [
        "machine_id",
        // The v5 `hosts` table is shipped history: its column keeps the
        // spelling that migration created.
        "host_id",
        "labels_json",
        "status",
        "registered_at",
        "updated_at",
        "retired_at",
        "last_seen_at",
    ] {
        assert!(
            table_has_column(&conn, "hosts", column).expect("host column"),
            "missing hosts.{column}"
        );
    }
    let preserved: String = conn
        .query_row(
            "SELECT value FROM existing_records WHERE id = 'keep-me'",
            [],
            |row| row.get(0),
        )
        .expect("existing row");
    assert_eq!(preserved, "unchanged");
    assert_eq!(
        current_schema_version(&conn).expect("version"),
        SUPPORTED_SCHEMA_VERSION
    );
    for table in [
        "workspace_ownership",
        "host_workspace_presence",
        "workspace_execution_profiles",
    ] {
        assert!(table_exists(&conn, table).expect("coordination table"));
    }

    let first = applied_migrations(&conn).expect("first ledger");
    apply_schema(&conn).expect("reapply");
    assert_eq!(applied_migrations(&conn).expect("second ledger"), first);
}

#[test]
fn failed_host_registry_migration_rolls_back_schema_and_ledger() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    conn.execute_batch(
        r#"
            CREATE TABLE schema_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            INSERT INTO schema_meta VALUES
                ('migration.v0001', 'baseline', '2026-07-01T00:00:00Z'),
                ('migration.v0002', 'learnings_index_workspace_scope', '2026-07-02T00:00:00Z'),
                ('migration.v0003', 'flat_crew_model', '2026-07-03T00:00:00Z'),
                ('migration.v0004', 'job_run_archive_stage', '2026-07-04T00:00:00Z');
            CREATE TABLE hosts (machine_id TEXT PRIMARY KEY);
            INSERT INTO hosts VALUES ('preexisting-sentinel');
        "#,
    )
    .expect("seed incompatible v4 database");

    let error = apply_schema(&conn).expect_err("v5 must fail on incompatible table");
    assert!(error.to_string().contains("status"), "unexpected: {error}");
    assert_eq!(current_schema_version(&conn).expect("version"), 4);
    assert!(
        !table_exists(&conn, "host_aliases").expect("alias table rolled back"),
        "partially created alias table must roll back"
    );
    let sentinel: String = conn
        .query_row("SELECT machine_id FROM hosts", [], |row| row.get(0))
        .expect("sentinel remains");
    assert_eq!(sentinel, "preexisting-sentinel");
}
