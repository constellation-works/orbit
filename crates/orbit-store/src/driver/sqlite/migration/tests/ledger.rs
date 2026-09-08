// ORB-10003: versioned schema-migration ledger.
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use orbit_common::OrbitError;
use rusqlite::{Connection, Error as SqliteError, ffi};

use super::super::ledger::{self, Migration};
use super::super::*;

fn ledger_rows(conn: &Connection) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare("SELECT key, value FROM schema_meta WHERE key LIKE 'migration.v%' ORDER BY key")
        .expect("prepare ledger query");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query ledger rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect ledger rows")
}

#[test]
fn fresh_db_applies_baseline_and_records_ledger() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");

    apply_schema(&conn).expect("apply schema on fresh db");

    assert!(table_exists(&conn, "tools").expect("tools table"));
    assert!(table_exists(&conn, "adrs").expect("adrs table"));
    assert!(table_exists(&conn, "schema_meta").expect("schema_meta table"));

    assert_eq!(
        current_schema_version(&conn).expect("current version"),
        SUPPORTED_SCHEMA_VERSION
    );
    let applied = applied_migrations(&conn).expect("applied migrations");
    assert_eq!(applied.len(), SUPPORTED_SCHEMA_VERSION as usize);
    assert_eq!(applied[0].version, 1);
    assert_eq!(applied[0].name, "baseline");
    assert!(!applied[0].applied_at.is_empty());
    assert_eq!(applied[1].version, 2);
    assert_eq!(applied[1].name, "learnings_index_workspace_scope");
    assert!(!applied[1].applied_at.is_empty());
    assert_eq!(applied[2].version, 3);
    assert_eq!(applied[2].name, "flat_crew_model");
    assert!(!applied[2].applied_at.is_empty());
    assert_eq!(applied[3].version, 4);
    assert_eq!(applied[3].name, "job_run_archive_stage");
    assert!(!applied[3].applied_at.is_empty());
    assert_eq!(applied[4].version, 5);
    assert_eq!(applied[4].name, "host_registry_core");
    assert!(!applied[4].applied_at.is_empty());
    assert_eq!(applied[5].version, 6);
    assert_eq!(applied[5].name, "workspace_coordination_projections");
    assert!(!applied[5].applied_at.is_empty());
    assert_eq!(applied[6].version, 7);
    assert_eq!(applied[6].name, "trusted_mcp_audit_provenance");
    assert!(!applied[6].applied_at.is_empty());
    assert_eq!(applied[7].version, 8);
    assert_eq!(applied[7].name, "hub_registry_metadata");
    assert!(!applied[7].applied_at.is_empty());
    assert_eq!(applied[8].version, 9);
    assert_eq!(applied[8].name, "feature_schema_ledger");
    assert!(!applied[8].applied_at.is_empty());
    assert_eq!(applied[9].version, 10);
    assert_eq!(applied[9].name, "invocation_telemetry_columns");
    assert!(!applied[9].applied_at.is_empty());
    assert_eq!(applied[10].version, 11);
    assert_eq!(applied[10].name, "routine_scheduler_schema");
    assert!(!applied[10].applied_at.is_empty());
    assert_eq!(applied[14].version, 15);
    assert_eq!(applied[14].name, "invocation_audit_context");
    assert!(!applied[14].applied_at.is_empty());
    assert_eq!(applied[15].version, 16);
    assert_eq!(applied[15].name, "audit_actor_identity");
    assert!(!applied[15].applied_at.is_empty());
    assert_eq!(applied[16].version, 17);
    assert_eq!(applied[16].name, "audit_self_reported_actor");
    assert!(!applied[16].applied_at.is_empty());
    assert_eq!(applied[17].version, 18);
    assert_eq!(applied[17].name, "audit_actor_alias_v2");
    assert!(!applied[17].applied_at.is_empty());
    assert_eq!(applied[18].version, 19);
    assert_eq!(applied[18].name, "job_runs_created_index");
    assert!(!applied[18].applied_at.is_empty());
    assert_eq!(applied[19].version, 20);
    assert_eq!(applied[19].name, "invocations_ts_index");
    assert!(!applied[19].applied_at.is_empty());
}

#[test]
fn reapplying_schema_is_a_noop() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");

    apply_schema(&conn).expect("first apply");
    let first = applied_migrations(&conn).expect("applied after first apply");
    apply_schema(&conn).expect("second apply");
    let second = applied_migrations(&conn).expect("applied after second apply");

    assert_eq!(first, second);
    assert_eq!(ledger_rows(&conn).len(), SUPPORTED_SCHEMA_VERSION as usize);
}

#[test]
fn legacy_db_adopts_versioned_ledger() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");

    // Schema as the pre-ledger idempotent migrations would have left an
    // old database: legacy `tools` shape, `adrs` without tags/paths, and
    // an `agent_sessions` with a foreign key to `tasks` (the shape the
    // rename-copy-drop migration rewrites). No schema_meta table at all.
    conn.execute_batch(
        r#"
            CREATE TABLE tools (
                name TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                is_enabled INTEGER NOT NULL DEFAULT 1,
                is_builtin INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO tools(name, path, description, is_enabled, is_builtin)
            VALUES ('legacy-tool', '/bin/legacy', 'legacy tool', 0, 1);

            CREATE TABLE adrs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                title TEXT NOT NULL,
                owner TEXT NOT NULL,
                related_features TEXT NOT NULL DEFAULT '[]',
                related_tasks TEXT NOT NULL DEFAULT '[]',
                legacy_ids TEXT NOT NULL DEFAULT '[]',
                supersedes TEXT NOT NULL DEFAULT '[]',
                superseded_by TEXT,
                validation_warnings TEXT NOT NULL DEFAULT '[]',
                legacy_validation TEXT NOT NULL DEFAULT 'none',
                created_at TEXT NOT NULL,
                accepted_at TEXT,
                last_updated TEXT NOT NULL
            );

            CREATE TABLE tasks (id TEXT PRIMARY KEY);
            INSERT INTO tasks(id) VALUES ('T1');
            CREATE TABLE agent_sessions (
                session_id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                skill_names TEXT NOT NULL,
                composed_context_hash TEXT NOT NULL,
                effective_allowed_tools TEXT NOT NULL,
                tool_calls TEXT NOT NULL,
                outcome TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY(task_id) REFERENCES tasks(id)
            );
            INSERT INTO agent_sessions VALUES (
                's1', 'T1', '[]', 'hash', '[]', '[]', 'ok', 'done',
                '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
            );
        "#,
    )
    .expect("create legacy schema");

    apply_schema(&conn).expect("adopt legacy db");

    // Baseline ran idempotently: new columns exist and legacy data survived.
    assert!(table_has_column(&conn, "tools", "enabled").expect("enabled column"));
    assert!(table_has_column(&conn, "adrs", "tags").expect("tags column"));
    assert!(table_has_column(&conn, "agent_sessions", "identity_id").expect("identity column"));
    let enabled: i64 = conn
        .query_row(
            "SELECT enabled FROM tools WHERE name = 'legacy-tool'",
            [],
            |row| row.get(0),
        )
        .expect("query migrated tool");
    assert_eq!(enabled, 0);
    let session_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_sessions WHERE session_id = 's1'",
            [],
            |row| row.get(0),
        )
        .expect("query migrated session");
    assert_eq!(session_count, 1);

    // ...and the ledger now records the adoption.
    assert_eq!(
        current_schema_version(&conn).expect("current version"),
        SUPPORTED_SCHEMA_VERSION
    );
    assert_eq!(
        ledger_rows(&conn),
        vec![
            ("migration.v0001".to_string(), "baseline".to_string()),
            (
                "migration.v0002".to_string(),
                "learnings_index_workspace_scope".to_string()
            ),
            ("migration.v0003".to_string(), "flat_crew_model".to_string()),
            (
                "migration.v0004".to_string(),
                "job_run_archive_stage".to_string()
            ),
            (
                "migration.v0005".to_string(),
                "host_registry_core".to_string()
            ),
            (
                "migration.v0006".to_string(),
                "workspace_coordination_projections".to_string()
            ),
            (
                "migration.v0007".to_string(),
                "trusted_mcp_audit_provenance".to_string()
            ),
            (
                "migration.v0008".to_string(),
                "hub_registry_metadata".to_string()
            ),
            (
                "migration.v0009".to_string(),
                "feature_schema_ledger".to_string()
            ),
            (
                "migration.v0010".to_string(),
                "invocation_telemetry_columns".to_string()
            ),
            (
                "migration.v0011".to_string(),
                "routine_scheduler_schema".to_string()
            ),
            (
                "migration.v0012".to_string(),
                "friction_records_sqlite".to_string()
            ),
            (
                "migration.v0013".to_string(),
                "workspace_claim_scope".to_string()
            ),
            (
                "migration.v0014".to_string(),
                "remove_native_learning_subsystem".to_string()
            ),
            (
                "migration.v0015".to_string(),
                "invocation_audit_context".to_string()
            ),
            (
                "migration.v0016".to_string(),
                "audit_actor_identity".to_string()
            ),
            (
                "migration.v0017".to_string(),
                "audit_self_reported_actor".to_string()
            ),
            (
                "migration.v0018".to_string(),
                "audit_actor_alias_v2".to_string()
            ),
            (
                "migration.v0019".to_string(),
                "job_runs_created_index".to_string()
            ),
            (
                "migration.v0020".to_string(),
                "invocations_ts_index".to_string()
            ),
        ]
    );
}

#[test]
fn refuses_db_from_a_newer_binary() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");

    conn.execute(
        "INSERT INTO schema_meta(key, value, updated_at)
        VALUES ('migration.v0021', 'from-the-future', '2099-01-01T00:00:00Z')",
        [],
    )
    .expect("record future migration");

    let err = apply_schema(&conn).expect_err("must refuse newer schema");
    assert!(matches!(err, OrbitError::Migration(_)), "got {err:?}");
    let message = err.to_string();
    assert!(message.contains("newer"), "unexpected message: {message}");
    assert!(
        message.contains("upgrade orbit"),
        "unexpected message: {message}"
    );
}

#[test]
fn store_reopens_database_at_shipped_schema_v4_and_applies_through_latest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orbit.db");

    // Model a database last opened by the shipped v4 binary. V5 is additive
    // and must preserve that schema while installing the host registry.
    let conn = Connection::open(&path).expect("open raw store connection");
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
            CREATE TABLE job_runs (id TEXT PRIMARY KEY, archived_at TEXT);
            INSERT INTO job_runs(id, archived_at) VALUES ('preserved-run', NULL);
        "#,
    )
    .expect("seed shipped v4 database");
    drop(conn);

    let store = crate::Store::open(&path).expect("reopen shipped v4 store");
    assert_eq!(
        store.schema_version().expect("schema version"),
        SUPPORTED_SCHEMA_VERSION
    );
    let applied = store.applied_migrations().expect("applied migrations");
    assert_eq!(
        applied.last().map(|migration| migration.version),
        Some(SUPPORTED_SCHEMA_VERSION)
    );
    assert_eq!(
        applied.last().map(|migration| migration.name.as_str()),
        Some("invocations_ts_index")
    );
    let connection = store.connection();
    let conn = connection.lock().expect("connection");
    assert!(table_exists(&conn, "hosts").expect("hosts table"));
    assert!(table_exists(&conn, "host_aliases").expect("aliases table"));
    assert!(table_exists(&conn, "workspace_ownership").expect("ownership table"));
    let preserved: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM job_runs WHERE id = 'preserved-run'",
            [],
            |row| row.get(0),
        )
        .expect("preserved v4 record");
    assert_eq!(preserved, 1);
}

#[test]
fn store_reopens_shipped_v6_audit_rows_and_applies_v7_additively() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orbit.db");
    let conn = Connection::open(&path).expect("open raw store connection");
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
                ('migration.v0004', 'job_run_archive_stage', '2026-07-04T00:00:00Z'),
                ('migration.v0005', 'host_registry_core', '2026-07-05T00:00:00Z'),
                ('migration.v0006', 'workspace_coordination_projections', '2026-07-06T00:00:00Z');
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
            INSERT INTO audit_events(
                execution_id, timestamp, command, role, status, exit_code,
                duration_ms, working_directory, host, pid, session_id, task_id,
                job_run_id, activity_id, step_index
            ) VALUES (
                'exec-v6', '2026-07-06T00:00:00Z', 'tool', 'codex', 'success', 0,
                1, '/repo', 'legacy-process-host', 42, 'legacy-session', 'ORB-10228',
                'jrun-v6', 'agent_implement', 3
            );
        "#,
    )
    .expect("seed shipped v6 database");
    drop(conn);

    let store = crate::Store::open(&path).expect("open and migrate v6 store");
    assert_eq!(
        store.schema_version().expect("schema version"),
        SUPPORTED_SCHEMA_VERSION
    );
    let rows = store
        .list_audit_events(&crate::AuditEventFilter::default())
        .expect("read migrated audit rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].host.as_deref(), Some("legacy-process-host"));
    assert_eq!(rows[0].session_id.as_deref(), Some("legacy-session"));
    assert_eq!(rows[0].job_run_id.as_deref(), Some("jrun-v6"));
    assert_eq!(rows[0].workspace_id, None);
    assert!(rows[0].effective_capabilities.is_empty());
    assert_eq!(rows[0].mcp_call_id, None);
    assert_eq!(rows[0].trace_id, None);
    assert_eq!(rows[0].caller_ip, None);
    drop(store);

    let reopened = crate::Store::open(&path).expect("reopen migrated store");
    assert_eq!(
        reopened.schema_version().expect("schema version"),
        SUPPORTED_SCHEMA_VERSION
    );
    assert_eq!(
        reopened
            .list_audit_events(&crate::AuditEventFilter::default())
            .expect("read after reopen")
            .len(),
        1
    );
}

#[test]
fn non_migration_schema_meta_keys_are_ignored() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");

    // State-import markers share the schema_meta table; they must not be
    // mistaken for ledger entries.
    conn.execute(
        "INSERT INTO schema_meta(key, value, updated_at)
         VALUES ('v2_state_import.ws_a', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .expect("record import marker");

    let applied = applied_migrations(&conn).expect("applied migrations");
    assert_eq!(applied.len(), SUPPORTED_SCHEMA_VERSION as usize);
    assert_eq!(applied[0].version, 1);
    apply_schema(&conn).expect("marker must not break reopen");
}

fn migration_v1_marker(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE ledger_test_v1 (x INTEGER)")
        .map_err(|e| OrbitError::Store(e.to_string()))
}

fn migration_v2_fails_midway(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE ledger_test_half_applied (x INTEGER);\n         this is not valid migration SQL",
    )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(())
}

#[test]
fn failed_migration_rolls_back_schema_and_ledger() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    let registry = [
        Migration {
            version: 1,
            name: "marker",
            apply: migration_v1_marker,
        },
        Migration {
            version: 2,
            name: "fails-midway",
            apply: migration_v2_fails_midway,
        },
    ];

    let err = ledger::run_migrations(&conn, &registry).expect_err("v2 must fail");
    assert!(matches!(err, OrbitError::Migration(_)), "got {err:?}");
    let message = err.to_string();
    assert!(message.contains("v2 (fails-midway)"), "got {message}");

    // v1 committed; v2 rolled back completely — no half-applied schema,
    // no ledger row.
    assert!(table_exists(&conn, "ledger_test_v1").expect("v1 table"));
    assert!(!table_exists(&conn, "ledger_test_half_applied").expect("v2 table rolled back"));
    assert_eq!(current_schema_version(&conn).expect("current version"), 1);
    assert_eq!(ledger_rows(&conn).len(), 1);

    // A fixed registry can resume from where the ledger left off.
    let fixed = [
        Migration {
            version: 1,
            name: "marker",
            apply: migration_v1_marker,
        },
        Migration {
            version: 2,
            name: "fixed",
            apply: migration_v1_marker_v2,
        },
    ];
    ledger::run_migrations(&conn, &fixed).expect("resume after fix");
    assert_eq!(current_schema_version(&conn).expect("current version"), 2);
}

fn seed_pre_ledger_fixture(path: &Path) {
    let conn = Connection::open(path).expect("seed pre-ledger db");
    conn.execute_batch(
        "CREATE TABLE tools (
            name TEXT PRIMARY KEY,
            path TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            is_enabled INTEGER NOT NULL DEFAULT 1,
            is_builtin INTEGER NOT NULL DEFAULT 0
        );",
    )
    .expect("pre-ledger tools table");
    conn.pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL on fixture");
}

fn assert_full_unique_ledger(applied: &[AppliedMigration]) {
    assert_eq!(applied.len(), SUPPORTED_SCHEMA_VERSION as usize);
    for (index, row) in applied.iter().enumerate() {
        assert_eq!(row.version, u32::try_from(index).expect("index") + 1);
    }
}

#[test]
fn concurrent_store_open_on_pre_ledger_fixture_applies_each_version_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orbit.db");
    seed_pre_ledger_fixture(&path);

    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                crate::Store::open(&path)
            })
        })
        .collect();

    let stores: Vec<_> = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("join")
                .expect("concurrent Store::open")
        })
        .collect();

    for store in &stores {
        let applied = store.applied_migrations().expect("applied migrations");
        assert_full_unique_ledger(&applied);
    }
}

struct PanicMigrationProbe {
    started: Option<mpsc::Sender<()>>,
    release: Option<mpsc::Receiver<()>>,
    waiting: Option<mpsc::Sender<()>>,
}

static PANIC_PROBE: Mutex<PanicMigrationProbe> = Mutex::new(PanicMigrationProbe {
    started: None,
    release: None,
    waiting: None,
});

fn take_probe_started() -> Option<mpsc::Sender<()>> {
    PANIC_PROBE.lock().expect("panic probe").started.take()
}

fn take_probe_release() -> Option<mpsc::Receiver<()>> {
    PANIC_PROBE.lock().expect("panic probe").release.take()
}

fn take_probe_waiting() -> Option<mpsc::Sender<()>> {
    PANIC_PROBE.lock().expect("panic probe").waiting.take()
}

fn migration_holds_lock_then_panics(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE ledger_test_panic (x INTEGER)")
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    if let Some(started) = take_probe_started() {
        let _ = started.send(());
    }
    if let Some(release) = take_probe_release() {
        let _ = release.recv();
    }
    panic!("intentional migration panic");
}

fn migration_creates_panic_table(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE ledger_test_panic (x INTEGER)")
        .map_err(|e| OrbitError::Store(e.to_string()))
}

fn waiter_busy_handler(_n: i32) -> bool {
    if let Some(waiting) = take_probe_waiting() {
        let _ = waiting.send(());
    }
    true
}

#[test]
fn waiter_applies_after_holder_panics_and_rolls_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orbit.db");
    let seed = Connection::open(&path).expect("seed db");
    seed.pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL");
    drop(seed);

    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    {
        let mut probe = PANIC_PROBE.lock().expect("panic probe");
        probe.started = Some(started_tx);
        probe.release = Some(release_rx);
        probe.waiting = Some(waiting_tx);
    }

    let panic_registry = [Migration {
        version: 1,
        name: "holds-then-panics",
        apply: migration_holds_lock_then_panics,
    }];
    let success_registry = [Migration {
        version: 1,
        name: "applies-after-rollback",
        apply: migration_creates_panic_table,
    }];

    let holder_path = path.clone();
    let holder = thread::spawn(move || {
        let conn = Connection::open(&holder_path).expect("holder open");
        orbit_common::storage::sqlite::apply_default_pragmas(&conn).expect("holder pragmas");
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ledger::run_migrations(&conn, &panic_registry)
        }))
    });

    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("holder entered migration body");

    let waiter_path = path.clone();
    let waiter = thread::spawn(move || {
        let conn = Connection::open(&waiter_path).expect("waiter open");
        conn.busy_handler(Some(waiter_busy_handler))
            .expect("waiter busy handler");
        ledger::run_migrations(&conn, &success_registry)
    });

    let waited = waiting_rx.recv_timeout(Duration::from_secs(5));
    let _ = release_tx.send(());
    let holder_result = holder.join().expect("holder join");
    assert!(
        holder_result.is_err(),
        "holder thread must unwind from the migration panic"
    );

    waited.expect("waiter blocked on BEGIN IMMEDIATE while holder held the lock");
    waiter
        .join()
        .expect("waiter join")
        .expect("waiter applies after holder rollback");

    let conn = Connection::open(&path).expect("inspect");
    assert!(
        table_exists(&conn, "ledger_test_panic").expect("panic table"),
        "rolled-back holder must not leave the table; waiter must create it"
    );
    assert_eq!(current_schema_version(&conn).expect("current version"), 1);
    assert_eq!(ledger_rows(&conn).len(), 1);
    assert_eq!(
        ledger_rows(&conn)[0],
        (
            "migration.v0001".to_string(),
            "applies-after-rollback".to_string()
        )
    );
}

#[test]
fn sqlite_full_commit_error_is_a_store_resource_error() {
    let migration = Migration {
        version: 42,
        name: "disk-full-test",
        apply: migration_v1_marker,
    };
    let error = SqliteError::SqliteFailure(ffi::Error::new(ffi::SQLITE_FULL), None);

    let mapped = ledger::commit_migration_error(&migration, error);

    assert!(matches!(mapped, OrbitError::Store(_)), "got {mapped:?}");
    let message = mapped.to_string();
    assert!(message.contains("SQLITE_FULL"), "got {message}");
    assert!(message.contains("v42 (disk-full-test)"), "got {message}");
}

fn migration_v1_marker_v2(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE ledger_test_v2 (x INTEGER)")
        .map_err(|e| OrbitError::Store(e.to_string()))
}

#[test]
fn registry_last_version_matches_supported_constant() {
    // Drift guard: appending a migration requires bumping the constant.
    assert_eq!(
        ledger::MIGRATIONS.last().map(|m| m.version),
        Some(SUPPORTED_SCHEMA_VERSION)
    );
}

#[test]
fn rejects_non_increasing_registry() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    let registry = [
        Migration {
            version: 2,
            name: "second",
            apply: migration_v1_marker,
        },
        Migration {
            version: 1,
            name: "first",
            apply: migration_v1_marker,
        },
    ];

    let err = ledger::run_migrations(&conn, &registry).expect_err("must reject registry");
    assert!(matches!(err, OrbitError::Migration(_)), "got {err:?}");
}

/// [ORB-10367] Schema/insert skew regression: a database that recorded the v1
/// baseline *before* a column was added to it never re-runs baseline, so any
/// column added there alone silently never reaches it. This models the
/// dk-server-1 store — pre-telemetry `invocations` table, ledger stamped
/// through the newest pre-fix version — and asserts that after migration
/// every column the insert binds exists and a real insert lands.
///
/// Against the broken state (no v10 registry entry) the store opens at v9 and
/// the insert fails with `table invocations has no column named
/// cache_create_1h_tokens` — the exact production failure.
#[test]
fn migrated_legacy_db_carries_every_invocation_insert_column() {
    use orbit_types::telemetry::{InvocationTrace, TokenUsage};

    use crate::contracts::InvocationInsertParams;
    use crate::driver::sqlite::invocation_store::INVOCATION_INSERT_COLUMNS;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orbit.db");
    let conn = Connection::open(&path).expect("open raw store connection");
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
                ('migration.v0004', 'job_run_archive_stage', '2026-07-04T00:00:00Z'),
                ('migration.v0005', 'host_registry_core', '2026-07-05T00:00:00Z'),
                ('migration.v0006', 'workspace_coordination_projections', '2026-07-06T00:00:00Z'),
                ('migration.v0007', 'trusted_mcp_audit_provenance', '2026-07-07T00:00:00Z'),
                ('migration.v0008', 'hub_registry_metadata', '2026-07-08T00:00:00Z'),
                ('migration.v0009', 'feature_schema_ledger', '2026-07-09T00:00:00Z');

            -- `invocations` as the baseline created it before the 5m/1h cache
            -- split and the token-derived cost column were added.
            CREATE TABLE invocations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                job_run_id TEXT NOT NULL,
                activity_id TEXT NOT NULL,
                agent TEXT NOT NULL,
                model TEXT,
                slot TEXT,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_create_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                tool_call_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE invocation_tasks (
                invocation_id INTEGER NOT NULL,
                task_id TEXT NOT NULL,
                PRIMARY KEY(invocation_id, task_id),
                FOREIGN KEY(invocation_id) REFERENCES invocations(id) ON DELETE CASCADE
            );
            CREATE TABLE tool_calls (
                invocation_id INTEGER NOT NULL,
                seq INTEGER NOT NULL,
                tool_name TEXT NOT NULL,
                result_bytes INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(invocation_id, seq),
                FOREIGN KEY(invocation_id) REFERENCES invocations(id) ON DELETE CASCADE
            );
        "#,
    )
    .expect("seed pre-telemetry database");
    drop(conn);

    let store = crate::Store::open(&path).expect("open and migrate legacy store");
    assert_eq!(
        store.schema_version().expect("schema version"),
        SUPPORTED_SCHEMA_VERSION
    );

    {
        let connection = store.connection();
        let conn = connection.lock().expect("connection");
        for column in INVOCATION_INSERT_COLUMNS {
            assert!(
                table_has_column(&conn, "invocations", column).expect("read invocations columns"),
                "migrated database is missing insert-bound column `{column}`",
            );
        }
    }

    // The structural assertion above is what catches skew early; this proves
    // the production write path itself works against a migrated database.
    store
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "jrun-legacy".to_string(),
            activity_id: "implement_one".to_string(),
            agent: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            task_ids: vec!["ORB-10367".to_string()],
            trace: InvocationTrace {
                usage: TokenUsage {
                    cache_create_1h: 37_795,
                    ..TokenUsage::default()
                },
                provider_cost_usd: Some(1.25),
                ..InvocationTrace::default()
            },
        })
        .expect("insert invocation trace into migrated legacy database");
}

fn schema_column_fingerprint(conn: &Connection) -> String {
    let mut table_statement = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .expect("prepare table list");
    let tables = table_statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query table list")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect table list");

    tables
        .into_iter()
        .map(|table| {
            let mut column_statement = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .expect("prepare column list");
            let columns = column_statement
                .query_map([], |row| row.get::<_, String>(1))
                .expect("query column list")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect column list");
            format!("{table}:{}\n", columns.join(","))
        })
        .collect()
}

/// ADR-0287: v1 is a shipped historical artifact. This fingerprint is
/// deliberately strict: adding a fresh-only table or column to the baseline
/// fails here and requires an append-only migration instead.
#[test]
fn shipped_v1_baseline_structure_is_frozen() {
    let conn = Connection::open_in_memory().expect("open v1 database");
    ledger::run_migrations(&conn, &ledger::MIGRATIONS[..1]).expect("apply v1 only");

    assert_eq!(
        schema_column_fingerprint(&conn),
        concat!(
            "adrs:id,status,title,owner,related_features,related_tasks,tags,paths,legacy_ids,supersedes,superseded_by,validation_warnings,legacy_validation,created_at,accepted_at,last_updated\n",
            "agent_sessions:session_id,task_id,identity_id,identity_name,identity_role,identity_block,skill_names,composed_context_hash,effective_allowed_tools,tool_calls,outcome,status,created_at,updated_at\n",
            "audit_events:id,execution_id,timestamp,command,subcommand,tool_name,target_type,target_id,role,status,exit_code,duration_ms,working_directory,arguments_json,stdout_truncated,stderr_truncated,error_message,host,pid,session_id,task_id,job_run_id,activity_id,step_index\n",
            "invocation_tasks:invocation_id,task_id\n",
            "invocations:id,ts,job_run_id,activity_id,agent,model,slot,duration_ms,input_tokens,cache_read_tokens,cache_create_tokens,output_tokens,tool_call_count\n",
            "job_run_steps:workspace_id,run_id,step_index,target_type,target_id,state,started_at,finished_at,duration_ms,exit_code,error_code,error_message,agent_response_json\n",
            "job_runs:run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,finished_at,duration_ms,created_at,pid,pid_start_time,input_json,retry_source_run_id,knowledge_metrics_json,resolved_crew,planner_model,implementer_model,reviewer_model,pipeline_state_json\n",
            "learnings_index:id,status,paths,tags,summary,updated_at,priority\n",
            "schema_meta:key,value,updated_at\n",
            "session_learning_state:workspace_id,session_id,learning_injection_state_json,updated_at\n",
            "task_reservations:reservation_id,workspace_orbit_dir,workspace_id,task_ids_json,files_json,actor,created_at,expires_at,released_at,owner_run_id,owner_metadata_json,release_reason,release_metadata_json\n",
            "tool_calls:invocation_id,seq,tool_name,result_bytes\n",
            "tools:name,path,description,parameters_json,enabled,builtin,created_at,updated_at\n",
            "v2_audit_events:id,workspace_id,event_id,source,schema_version,event_type,ts,run_id,agent_identity,parent_event_id,workspace_path,payload_json\n",
        )
    );
}

/// A database that recorded shipped v1 and then advances through every
/// registered migration must expose the same table/column structure as a
/// database created fresh by the current binary.
#[test]
fn v1_upgrade_and_fresh_database_have_identical_columns() {
    let fresh = Connection::open_in_memory().expect("open fresh database");
    apply_schema(&fresh).expect("migrate fresh database");

    let legacy = Connection::open_in_memory().expect("open legacy database");
    ledger::run_migrations(&legacy, &ledger::MIGRATIONS[..1]).expect("record shipped v1");
    ledger::run_migrations(&legacy, ledger::MIGRATIONS).expect("upgrade through registry");

    assert_eq!(
        applied_migrations(&legacy)
            .expect("legacy migration ledger")
            .len(),
        ledger::MIGRATIONS.len()
    );
    assert_eq!(
        schema_column_fingerprint(&legacy),
        schema_column_fingerprint(&fresh)
    );
}
