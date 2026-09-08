//! Unit tests for `schema` — sibling layout under store/tests/.

use rusqlite::Connection;

use super::super::schema::{
    CorpusFtsLayout, LEGACY_INLINE_BM25_SQL, SEMANTIC_INDEX_LAYOUT_INCOMPATIBLE, corpus_fts_layout,
    ensure_vector_schema, evaluate_legacy_inline_reader, legacy_task_fts_table, table_exists,
};

/// The pre-[ORB-11695] layout: chunk text and its metadata lived in the FTS5
/// table itself, so every non-`MATCH` access was a full scan.
const INLINE_CORPUS_FTS_DDL: &str = r#"
    CREATE VIRTUAL TABLE corpus_fts USING fts5(
        source_kind UNINDEXED,
        source_id UNINDEXED,
        field UNINDEXED,
        content,
        tokenize = 'porter unicode61 remove_diacritics 2'
    );
"#;

fn open_migrated(setup_sql: &str) -> Connection {
    let conn = Connection::open_in_memory().expect("open db");
    conn.execute_batch(setup_sql).expect("apply setup sql");
    ensure_vector_schema(&conn).expect("migrate schema");
    ensure_vector_schema(&conn).expect("migrate schema idempotently");
    conn
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).expect(sql)
}

#[test]
fn fresh_database_indexes_chunks_through_external_content_fts() {
    let conn = open_migrated("");

    conn.execute_batch(
        r#"
            INSERT INTO chunks(source_kind, source_id, field, chunk_idx, content)
            VALUES ('task', 'T1', 'purpose', 0, 'alpha neutrino');
        "#,
    )
    .expect("insert chunk");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM corpus_fts WHERE corpus_fts MATCH 'neutrino'"
        ),
        1,
        "insert trigger must index the new chunk"
    );

    conn.execute_batch("DELETE FROM chunks WHERE source_id = 'T1'")
        .expect("delete chunk");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM corpus_fts WHERE corpus_fts MATCH 'neutrino'"
        ),
        0,
        "delete trigger must retract the chunk's tokens"
    );
}

#[test]
fn inline_corpus_fts_rows_migrate_into_chunks_in_original_order() {
    let conn = open_migrated(&format!(
        r#"
            {INLINE_CORPUS_FTS_DDL}
            INSERT INTO corpus_fts(source_kind, source_id, field, content)
            VALUES
                ('task', 'T1', 'purpose', 'first chunk'),
                ('task', 'T1', 'purpose', 'second chunk'),
                ('doc', 'docs/a.md', 'body', 'gamma neutrino');
        "#
    ));

    let ordered: Vec<(String, i64, String)> = conn
        .prepare("SELECT source_id, chunk_idx, content FROM chunks ORDER BY source_id, chunk_idx")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query chunks")
        .collect::<Result<_, _>>()
        .expect("collect chunks");

    assert_eq!(
        ordered,
        vec![
            ("T1".to_string(), 0, "first chunk".to_string()),
            ("T1".to_string(), 1, "second chunk".to_string()),
            ("docs/a.md".to_string(), 0, "gamma neutrino".to_string()),
        ],
        "chunk_idx must reproduce each field's original row order"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM corpus_fts WHERE corpus_fts MATCH 'neutrino'"
        ),
        1,
        "the rebuilt FTS index must still match migrated content"
    );
}

#[test]
fn legacy_task_fts_rows_backfill_into_chunks_once() {
    let legacy = legacy_task_fts_table();
    let conn = open_migrated(&format!(
        r#"
            CREATE VIRTUAL TABLE {legacy} USING fts5(
                source_id UNINDEXED,
                field UNINDEXED,
                content,
                tokenize = 'porter unicode61 remove_diacritics 2'
            );
            INSERT INTO {legacy}(source_id, field, content)
            VALUES ('T1', 'title', 'alpha'), ('T2', 'plan', 'beta');
        "#
    ));

    assert_eq!(count(&conn, "SELECT COUNT(*) FROM chunks"), 2);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM chunks WHERE source_kind = 'task'"
        ),
        2
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM corpus_fts WHERE corpus_fts MATCH 'alpha'"
        ),
        1
    );
    assert!(!table_exists(&conn, legacy).expect("legacy lookup"));
}

/// [ORB-11695] The write and snippet paths only ever address chunks by
/// `(source_kind, source_id[, field[, chunk_idx]])` or by rowid. None of them
/// may fall back to scanning the corpus: that is what made a reindex
/// quadratic. `MATCH` queries are exempt — they are what the FTS index is for.
#[test]
fn chunk_access_paths_use_indexes_instead_of_scanning_the_corpus() {
    let conn = open_migrated("");

    let plan_for = |sql: &str| -> String {
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("prepare plan");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(3))
            .expect("query plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect plan");
        rows.join(" | ")
    };

    // The production statements with their bind parameters spelled out as
    // literals, so a single `EXPLAIN QUERY PLAN` needs no bindings.
    let addressed_queries = [
        // delete_field_rows
        "DELETE FROM chunks WHERE source_kind = 'task' AND source_id = 'T1' AND field = 'purpose'",
        // delete_source
        "DELETE FROM chunks WHERE source_kind = 'task' AND source_id = 'T1'",
        // StoredSource::load — the per-source probe run on every upsert
        "SELECT field, model_id, content_hash FROM embeddings
         WHERE source_kind = 'task' AND source_id = 'T1'
         UNION ALL
         SELECT DISTINCT field, NULL, NULL FROM chunks
         WHERE source_kind = 'task' AND source_id = 'T1'",
        // snippet_by_chunk_idx
        "SELECT content FROM chunks
         WHERE source_kind = 'task' AND source_id = 'T1' AND field = 'purpose' AND chunk_idx = 0",
        // snippet_by_rowid
        "SELECT content FROM chunks WHERE id = 1",
    ];
    for sql in addressed_queries {
        let plan = plan_for(sql);
        assert!(
            !plan.contains("SCAN corpus_fts"),
            "query must not scan the FTS corpus: {sql}\nplan: {plan}"
        );
        assert!(
            !plan.contains("SCAN chunks"),
            "query must not scan the chunk table: {sql}\nplan: {plan}"
        );
    }
}

#[test]
fn unmigrated_inline_corpus_fts_still_answers_the_pre_migration_reader() {
    let conn = Connection::open_in_memory().expect("open db");
    conn.execute_batch(&format!(
        r#"
            {INLINE_CORPUS_FTS_DDL}
            INSERT INTO corpus_fts(source_kind, source_id, field, content)
            VALUES ('task', 'T1', 'purpose', 'gamma neutrino');
        "#
    ))
    .expect("seed inline corpus");

    assert_eq!(
        corpus_fts_layout(&conn).expect("layout"),
        CorpusFtsLayout::InlineMetadata
    );
    let hits = evaluate_legacy_inline_reader(&conn, "neutrino", "task").expect("legacy reader");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source_id, "T1");
    assert_eq!(hits[0].field, "purpose");
}

/// Forward-only contract: after [ORB-11695] migrates inline `corpus_fts` into
/// `chunks` + external-content FTS, the Orbit 0.19.0 BM25 projection cannot
/// read the index. The chosen result is an explicit layout diagnostic, not a
/// leaked `no such column: source_kind`. Current JOIN/snippet SQL and the
/// migrated rows stay intact; the index is not downgraded.
#[test]
fn migrated_external_content_rejects_pre_migration_inline_reader() {
    let conn = open_migrated(&format!(
        r#"
            {INLINE_CORPUS_FTS_DDL}
            INSERT INTO corpus_fts(source_kind, source_id, field, content)
            VALUES
                ('task', 'T1', 'purpose', 'gamma neutrino'),
                ('doc', 'docs/a.md', 'body', 'unrelated');
        "#
    ));

    assert_eq!(
        corpus_fts_layout(&conn).expect("layout"),
        CorpusFtsLayout::ExternalContent
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM chunks"), 2);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM chunks WHERE source_kind = 'task' AND source_id = 'T1'"
        ),
        1
    );

    let raw = conn
        .prepare(LEGACY_INLINE_BM25_SQL)
        .expect_err("migrated corpus_fts must not expose source_kind");
    let raw_message = raw.to_string().to_ascii_lowercase();
    assert!(
        raw_message.contains("no such column") && raw_message.contains("source_kind"),
        "raw sqlite must still be the live 0.19.0 symptom, proving columns were not restored: {raw}"
    );

    let error = evaluate_legacy_inline_reader(&conn, "neutrino", "task")
        .expect_err("legacy reader against external-content layout");
    let message = error.to_string();
    assert!(
        message.contains(SEMANTIC_INDEX_LAYOUT_INCOMPATIBLE),
        "mismatched reader must name the layout contract: {message}"
    );
    assert!(
        message.contains("Upgrade and restart every Orbit process"),
        "diagnostic must say which processes to restart: {message}"
    );
    assert!(
        message.contains("Lexical (non-hybrid) task and doc"),
        "diagnostic must keep lexical lookup in scope: {message}"
    );
    assert!(
        !message.to_ascii_lowercase().contains("no such column"),
        "must not leak the unexplained SQL-column error: {message}"
    );

    let current: Vec<(String, String)> = conn
        .prepare(
            r#"
                SELECT chunks.source_id, chunks.field
                FROM corpus_fts
                JOIN chunks ON chunks.id = corpus_fts.rowid
                WHERE corpus_fts MATCH 'neutrino' AND chunks.source_kind = 'task'
            "#,
        )
        .expect("prepare current reader")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query current reader")
        .collect::<Result<_, _>>()
        .expect("collect current reader");
    assert_eq!(
        current,
        vec![("T1".to_string(), "purpose".to_string())],
        "current JOIN reader must keep working on the migrated index"
    );
    assert_eq!(
        conn.query_row(
            r#"
                SELECT content FROM chunks
                WHERE source_kind = 'task' AND source_id = 'T1'
                  AND field = 'purpose' AND chunk_idx = 0
            "#,
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("snippet"),
        "gamma neutrino"
    );
}
