//  Store migration compatibility paths through the public open boundary.
//  This separate integration area guards legacy schema repair and actor data
//  backfills; the schema golden covers the empty-database prefixes.

fn migration_freeze_isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_MIGRATION_FREEZE_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    let output = test_env::run_child_test(&mut command, test, root.path());
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}

fn rewind(conn: &Connection, version: u32) {
    conn.execute(
        "DELETE FROM schema_meta WHERE key LIKE 'migration.v%' AND key >= ?1",
        [format!("migration.v{version:04}")],
    )
    .unwrap();
}

#[test]
fn defensive_recreation_survives_a_ledger_with_missing_tables() {
    if !migration_freeze_isolated("defensive_recreation_survives_a_ledger_with_missing_tables") {
        return;
    }
    for (version, table, expected_columns) in [
        (
            13,
            "task_reservations",
            vec!["workspace_id", "scope", "claim_token"],
        ),
        (
            15,
            "audit_events",
            vec!["workspace_id", "caller_machine_id", "trace_id", "caller_ip"],
        ),
    ] {
        let path = std::env::current_dir()
            .unwrap()
            .join(format!("v{version}.sqlite"));
        drop(Store::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        rewind(&conn, version);
        conn.execute_batch(&format!("DROP TABLE {table}")).unwrap();
        drop(conn);
        // The defensive recreation must still carry a malformed legacy
        // database through the full remaining ledger, and remain idempotent.
        for _ in 0..2 {
            drop(Store::open(&path).unwrap());
            let conn = Connection::open(&path).unwrap();
            let columns: Vec<String> = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap()
                .query_map([], |row| row.get(1))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            for column in &expected_columns {
                assert!(
                    columns.iter().any(|actual| actual == column),
                    "v{version} must recreate {table}.{column}"
                );
            }
            if version == 13 {
                conn.execute_batch(
                    "INSERT OR IGNORE INTO task_reservations
                    (reservation_id, workspace_orbit_dir, task_ids_json, files_json,
                     actor, created_at, expires_at)
                    VALUES ('legacy', '.', '[]', '[]', 'codex', '2026-01-01', '2026-01-02');",
                )
                .unwrap();
                let scope: String = conn
                    .query_row(
                        "SELECT scope FROM task_reservations WHERE reservation_id = 'legacy'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(
                    scope, "files",
                    "legacy reservations retain their scope default"
                );
            }
        }
    }
}

#[test]
fn actor_backfills_stamp_the_map_and_preserve_roles_and_current_rows() {
    if !migration_freeze_isolated("actor_backfills_stamp_the_map_and_preserve_roles_and_current_rows") {
        return;
    }
    let labels = [
        " HUMAN ",
        "admin",
        "hook",
        "unverified",
        "",
        "claude",
        "Fable-5.1",
        "gpt-6-luna",
        "haiku",
        "custom-model",
    ];
    for version in [16, 18] {
        let path = std::env::current_dir()
            .unwrap()
            .join(format!("actors-v{version}.sqlite"));
        drop(Store::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        rewind(&conn, version);
        for (index, label) in labels.iter().enumerate() {
            conn.execute(
                "INSERT INTO audit_events (execution_id, timestamp, command, role, status,
                 exit_code, duration_ms, working_directory, pid, actor_alias_version)
                 VALUES (?1, '2026-01-01', 'tool', ?2, 'success', 0, 0, '.', 1, ?3)",
                rusqlite::params![
                    index.to_string(),
                    label,
                    if version == 16 { None } else { Some(1) }
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO audit_events (execution_id, timestamp, command, role, status,
             exit_code, duration_ms, working_directory, pid, actor_alias_version, actor_id)
             VALUES ('current', '2026-01-01', 'tool', 'claude', 'success', 0, 0, '.', 1, ?1, 'preserved')",
            [ACTOR_ALIAS_MAP_VERSION],
        ).unwrap();
        drop(conn);
        for _ in 0..2 {
            drop(Store::open(&path).unwrap());
            let conn = Connection::open(&path).unwrap();
            for (index, label) in labels.iter().enumerate() {
                let expected = canonical_actor_for_role_label(label);
                let actual = conn
                    .query_row(
                        "SELECT role, actor_kind, actor_id, actor_vendor, actor_family,
                     actor_model, actor_alias_version FROM audit_events WHERE execution_id = ?1",
                        [index.to_string()],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, Option<String>>(3)?,
                                row.get::<_, Option<String>>(4)?,
                                row.get::<_, Option<String>>(5)?,
                                row.get::<_, u32>(6)?,
                            ))
                        },
                    )
                    .unwrap();
                assert_eq!(
                    actual,
                    (
                        label.to_string(),
                        expected.kind.as_str().to_string(),
                        expected.id,
                        expected.vendor,
                        expected.family,
                        expected.model,
                        expected.alias_version
                    ),
                    "v{version} backfills {label:?} without rewriting its trust label"
                );
            }
            let preserved: String = conn
                .query_row(
                    "SELECT actor_id FROM audit_events WHERE execution_id = 'current'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                preserved, "preserved",
                "already-stamped actor rows are untouched"
            );
        }
    }
}

#[path = "migration_freeze/ledger.rs"]
mod ledger;
