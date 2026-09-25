use super::super::*;

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
