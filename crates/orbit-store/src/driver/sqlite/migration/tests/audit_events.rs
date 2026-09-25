use super::super::*;

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

    super::super::audit_events::backfill_audit_actor_identity(&conn).expect("re-run backfill");

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

    super::super::audit_events::apply_audit_actor_alias_v2(&conn).expect("re-derive under v2");

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
