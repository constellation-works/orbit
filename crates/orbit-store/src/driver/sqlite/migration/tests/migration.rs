// Migrated from sqlite/migration.rs per ORB-00231
use super::super::*;

#[test]
fn task_reservation_migration_adds_owner_columns_before_owner_index() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    conn.execute_batch(
        r#"
                CREATE TABLE task_reservations (
                    reservation_id TEXT PRIMARY KEY,
                    workspace_orbit_dir TEXT NOT NULL,
                    task_ids_json TEXT NOT NULL,
                    files_json TEXT NOT NULL,
                    actor TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    released_at TEXT
                );

                INSERT INTO task_reservations(
                    reservation_id,
                    workspace_orbit_dir,
                    task_ids_json,
                    files_json,
                    actor,
                    created_at,
                    expires_at,
                    released_at
                ) VALUES (
                    'reservation-legacy',
                    '/workspace/.orbit',
                    '["T1"]',
                    '["file:src/lib.rs"]',
                    'legacy',
                    '2026-05-05T00:00:00Z',
                    '2026-05-05T01:00:00Z',
                    NULL
                );
            "#,
    )
    .expect("create legacy reservation table");

    apply_schema(&conn).expect("migrate legacy reservation table");

    assert!(
        table_has_column(&conn, "task_reservations", "workspace_id").expect("workspace column")
    );
    assert!(table_has_column(&conn, "task_reservations", "owner_run_id").expect("owner column"));
    let owner_run_id: Option<String> = conn
            .query_row(
                "SELECT owner_run_id FROM task_reservations WHERE reservation_id = 'reservation-legacy'",
                [],
                |row| row.get(0),
            )
            .expect("query migrated row");
    assert_eq!(owner_run_id, None);
    let owner_index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index'
                   AND name = 'idx_task_reservations_workspace_owner_release'",
            [],
            |row| row.get(0),
        )
        .expect("query owner index");
    assert_eq!(owner_index, 1);
}

#[test]
fn apply_schema_creates_adrs_table_and_indexes() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");

    apply_schema(&conn).expect("apply schema");

    assert!(table_exists(&conn, "adrs").expect("adrs table exists"));

    let primary_key_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(adrs)")
        .expect("prepare pragma")
        .query_map([], |row| {
            let name: String = row.get(1)?;
            let pk: i64 = row.get(5)?;
            Ok((name, pk))
        })
        .expect("query pragma")
        .filter_map(|row| {
            let (name, pk) = row.expect("pragma row");
            (pk > 0).then_some(name)
        })
        .collect();
    assert_eq!(primary_key_columns, vec!["id"]);
    assert!(table_has_column(&conn, "adrs", "tags").expect("tags column"));
    assert!(table_has_column(&conn, "adrs", "paths").expect("paths column"));

    for index_name in ["idx_adrs_status", "idx_adrs_owner"] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'index' AND name = ?1",
                [index_name],
                |row| row.get(0),
            )
            .expect("query index");
        assert_eq!(count, 1, "expected index {index_name} to exist");
    }
}

#[test]
fn run_archive_stage_migration_adds_archived_at() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");
    assert!(table_has_column(&conn, "job_runs", "archived_at").expect("archived_at column"));
}

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

#[test]
fn fresh_schema_has_no_native_learning_tables() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");

    for table in [
        "learnings_index",
        "session_learning_state",
        "id_allocations",
    ] {
        assert!(
            !table_exists(&conn, table).expect("inspect table"),
            "{table}"
        );
    }
    assert_eq!(
        current_schema_version(&conn).expect("schema version"),
        SUPPORTED_SCHEMA_VERSION
    );
}

#[test]
fn upgrade_removes_native_learning_tables_and_vector_rows() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    conn.execute_batch(
        r#"
            CREATE TABLE schema_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            INSERT INTO schema_meta(key, value, updated_at)
            VALUES ('migration.v0013', 'workspace_claim_scope', '2026-08-12T00:00:00Z');
            CREATE TABLE learnings_index (workspace_id TEXT, id TEXT);
            CREATE TABLE session_learning_state (workspace_id TEXT, session_id TEXT);
            CREATE TABLE id_allocations (kind TEXT, id TEXT);
            CREATE TABLE embeddings (source_kind TEXT, source_id TEXT);
            INSERT INTO embeddings(source_kind, source_id)
            VALUES ('learning', 'L-0001'), ('task', 'ORB-1'), ('doc', 'guide');
        "#,
    )
    .expect("seed schema v13 database");

    apply_schema(&conn).expect("apply removal migration");

    for table in [
        "learnings_index",
        "session_learning_state",
        "id_allocations",
    ] {
        assert!(
            !table_exists(&conn, table).expect("inspect table"),
            "{table}"
        );
    }
    let source_kinds = conn
        .prepare("SELECT source_kind FROM embeddings ORDER BY source_kind")
        .expect("prepare remaining vectors")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query remaining vectors")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect remaining vectors");
    assert_eq!(source_kinds, vec!["doc", "task"]);
}

// ── read-only ledger inspection for `orbit migrate --dry-run` [ORB-10012] ──

#[test]
fn read_schema_ledger_status_of_a_missing_database_lists_everything_pending() {
    let temp = tempfile::tempdir().expect("tempdir");
    let status =
        read_schema_ledger_status(&temp.path().join("orbit.db")).expect("read missing db status");

    assert_eq!(status.current_version, 0);
    assert!(!status.pending.is_empty());
    assert_eq!(
        status.pending.last().map(|m| m.version),
        Some(SUPPORTED_SCHEMA_VERSION)
    );
    // Read-only: inspecting must not create the database.
    assert!(!temp.path().join("orbit.db").exists());
}

#[test]
fn read_schema_ledger_status_of_a_migrated_database_reports_current_without_writing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("orbit.db");
    drop(crate::Store::open(&db_path).expect("open store"));

    let status = read_schema_ledger_status(&db_path).expect("read status");
    assert_eq!(status.current_version, SUPPORTED_SCHEMA_VERSION);
    assert!(status.pending.is_empty());
    assert!(pending_schema_migrations_after(SUPPORTED_SCHEMA_VERSION).is_empty());
    assert_eq!(
        pending_schema_migrations_after(0).last().map(|m| m.version),
        Some(SUPPORTED_SCHEMA_VERSION)
    );
}

/// ORB-10888: a database written before the actor projection existed must
/// aggregate identically to one written after it, or a 30d window that spans
/// the migration reports two populations for one actor.
#[test]
fn audit_actor_backfill_makes_legacy_rows_group_with_new_rows() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    conn.execute_batch(
        r#"
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
                session_id TEXT
            );

            INSERT INTO audit_events(
                execution_id, timestamp, command, role, status, exit_code,
                duration_ms, working_directory, pid
            ) VALUES
                ('exec-1', '2026-04-28T00:00:00Z', 'tool', 'claude', 'success', 0, 1, '/tmp', 1),
                ('exec-2', '2026-04-28T00:00:01Z', 'tool', 'opus', 'success', 0, 1, '/tmp', 1),
                ('exec-3', '2026-04-28T00:00:02Z', 'tool', 'claude-opus-5', 'success', 0, 1, '/tmp', 1),
                ('exec-4', '2026-04-28T00:00:03Z', 'tool', 'admin', 'success', 0, 1, '/tmp', 1),
                ('exec-5', '2026-04-28T00:00:04Z', 'tool', 'unverified', 'success', 0, 1, '/tmp', 1);
        "#,
    )
    .expect("seed legacy audit rows");

    apply_schema(&conn).expect("apply schema");

    let mut stmt = conn
        .prepare(
            "SELECT COALESCE(actor_kind, ''), COALESCE(actor_id, ''), COUNT(*) \
             FROM audit_events GROUP BY 1, 2 ORDER BY 2",
        )
        .expect("prepare grouped read");
    let grouped: Vec<(String, String, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query grouped")
        .collect::<Result<_, _>>()
        .expect("collect grouped");

    assert_eq!(
        grouped,
        vec![
            ("system".to_string(), "admin".to_string(), 1),
            ("agent".to_string(), "claude".to_string(), 3),
            ("unattributed".to_string(), "unverified".to_string(), 1),
        ]
    );

    // Every row is stamped, so re-running an aggregate is reproducible.
    let unstamped: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_events WHERE actor_alias_version IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("count unstamped");
    assert_eq!(unstamped, 0);

    // Trust classification reads `role`; the backfill must not have touched it.
    let mut roles_stmt = conn
        .prepare("SELECT role FROM audit_events ORDER BY execution_id")
        .expect("prepare roles");
    let roles: Vec<String> = roles_stmt
        .query_map([], |row| row.get(0))
        .expect("query roles")
        .collect::<Result<_, _>>()
        .expect("collect roles");
    assert_eq!(
        roles,
        vec!["claude", "opus", "claude-opus-5", "admin", "unverified"]
    );
}

/// The backfill is keyed on the alias version, so a second pass over an
/// already-migrated database is a no-op rather than a rewrite.
#[test]
fn audit_actor_backfill_is_idempotent() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");
    conn.execute(
        "INSERT INTO audit_events(
            execution_id, timestamp, command, role, status, exit_code,
            duration_ms, working_directory, pid, actor_kind, actor_id,
            actor_alias_version
        ) VALUES ('exec-1', '2026-04-28T00:00:00Z', 'tool', 'opus', 'success', 0, 1, '/tmp', 1,
                  'agent', 'claude', ?1)",
        [orbit_types::telemetry::ACTOR_ALIAS_MAP_VERSION],
    )
    .expect("insert stamped row");

    super::super::backfill_audit_actor_identity(&conn).expect("re-run backfill");

    let (kind, id): (String, String) = conn
        .query_row("SELECT actor_kind, actor_id FROM audit_events", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("read projection");
    assert_eq!(kind, "agent");
    assert_eq!(id, "claude");
}

/// Alias map v2 (migration v18) promoted `fable` to a family rule. A row the
/// v1 map stamped as an unrecognized-family agent must be re-derived under
/// the current map, while `role` stays byte-identical.
#[test]
fn audit_actor_alias_v2_rederives_rows_stamped_under_the_old_map() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");
    conn.execute(
        "INSERT INTO audit_events(
            execution_id, timestamp, command, role, status, exit_code,
            duration_ms, working_directory, pid, actor_kind, actor_id,
            actor_family, actor_model, actor_alias_version
        ) VALUES ('exec-1', '2026-08-28T00:00:00Z', 'tool', 'fable-5.1', 'success', 0, 1,
                  '/tmp', 1, 'agent', 'fable-5.1', NULL, 'fable-5.1', 1)",
        [],
    )
    .expect("insert v1-stamped row");

    super::super::apply_audit_actor_alias_v2(&conn).expect("re-derive under v2");

    let (role, id, family, version): (String, String, String, u32) = conn
        .query_row(
            "SELECT role, actor_id, actor_family, actor_alias_version FROM audit_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read projection");
    assert_eq!(role, "fable-5.1");
    assert_eq!(id, "claude");
    assert_eq!(family, "claude");
    assert_eq!(version, orbit_types::telemetry::ACTOR_ALIAS_MAP_VERSION);
}
