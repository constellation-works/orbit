//! Forward-only migration of the legacy semantic.db into a lexical index.
use orbit_common::OrbitError;
use rusqlite::Connection;

const CHUNKS_DDL: &str = r#"
    CREATE TABLE IF NOT EXISTS chunks (
        id INTEGER PRIMARY KEY,
        source_kind TEXT NOT NULL,
        source_id TEXT NOT NULL,
        field TEXT NOT NULL,
        chunk_idx INTEGER NOT NULL,
        content TEXT NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS chunks_by_address
    ON chunks(source_kind, source_id, field, chunk_idx);
"#;

/// The FTS5 index plus the triggers that mirror `chunks` into it. FTS5 cannot
/// remove a row's tokens without the original text, which is why the delete
/// and update triggers replay `old.content`.
const CORPUS_FTS_DDL: &str = r#"
    CREATE VIRTUAL TABLE corpus_fts USING fts5(
        content,
        content = 'chunks',
        content_rowid = 'id',
        tokenize = 'porter unicode61 remove_diacritics 2'
    );

    CREATE TRIGGER chunks_after_insert AFTER INSERT ON chunks BEGIN
        INSERT INTO corpus_fts(rowid, content) VALUES (new.id, new.content);
    END;

    CREATE TRIGGER chunks_after_delete AFTER DELETE ON chunks BEGIN
        INSERT INTO corpus_fts(corpus_fts, rowid, content)
        VALUES ('delete', old.id, old.content);
    END;

    CREATE TRIGGER chunks_after_update AFTER UPDATE ON chunks BEGIN
        INSERT INTO corpus_fts(corpus_fts, rowid, content)
        VALUES ('delete', old.id, old.content);
        INSERT INTO corpus_fts(rowid, content) VALUES (new.id, new.content);
    END;
"#;

pub fn ensure_schema(conn: &Connection) -> Result<(), OrbitError> {
    let obsolete = table_exists(conn, "embeddings")? || table_exists(conn, "id_allocations")?;
    if !obsolete
        && schema_present(conn)?
        && !corpus_fts_has_inline_columns(conn)?
        && !table_exists(conn, legacy_task_fts_table())?
    {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE").map_err(store_error)?;
    let result = (|| {
        conn.execute_batch(CHUNKS_DDL).map_err(store_error)?;
        let adopted = adopt_inline_corpus_fts(conn)?;
        if !table_exists(conn, "corpus_fts")? {
            conn.execute_batch(CORPUS_FTS_DDL).map_err(store_error)?;
            if adopted {
                conn.execute("INSERT INTO corpus_fts(corpus_fts) VALUES ('rebuild')", [])
                    .map_err(store_error)?;
            }
        }
        migrate_legacy_task_fts(conn)?;
        conn.execute_batch("DROP TABLE IF EXISTS embeddings; DROP TABLE IF EXISTS id_allocations;")
            .map_err(store_error)?;
        Ok::<_, OrbitError>(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT").map_err(store_error)?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    if obsolete {
        conn.execute_batch("VACUUM").map_err(store_error)?;
        tracing::info!("removed obsolete search vectors and reclaimed database space");
    }
    Ok(())
}

pub(super) fn schema_present(conn: &Connection) -> Result<bool, OrbitError> {
    Ok(table_exists(conn, "chunks")? && table_exists(conn, "corpus_fts")?)
}

/// Move a pre-external-content `corpus_fts` into `chunks` and drop it, so the
/// caller can recreate the FTS index over its new content table. Returns
/// whether such a table was found. The old table had no `chunk_idx`; chunk
/// order was rowid order within a `(source_kind, source_id, field)` group, so
/// that is what the numbering reproduces.
fn adopt_inline_corpus_fts(conn: &Connection) -> Result<bool, OrbitError> {
    if !table_exists(conn, "corpus_fts")? || !corpus_fts_has_inline_columns(conn)? {
        return Ok(false);
    }

    conn.execute_batch(
        r#"
            INSERT INTO chunks(source_kind, source_id, field, chunk_idx, content)
            SELECT
                source_kind,
                source_id,
                field,
                ROW_NUMBER() OVER (
                    PARTITION BY source_kind, source_id, field ORDER BY rowid
                ) - 1,
                content
            FROM corpus_fts;

            DROP TABLE corpus_fts;
        "#,
    )
    .map_err(store_error)?;
    Ok(true)
}

fn migrate_legacy_task_fts(conn: &Connection) -> Result<(), OrbitError> {
    let legacy_table = legacy_task_fts_table();
    if !table_exists(conn, legacy_table)? {
        return Ok(());
    }

    let chunk_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
        .map_err(store_error)?;
    if chunk_rows == 0 {
        let copy_sql = format!(
            r#"
                INSERT INTO chunks(source_kind, source_id, field, chunk_idx, content)
                SELECT
                    'task',
                    source_id,
                    field,
                    ROW_NUMBER() OVER (
                        PARTITION BY source_id, field ORDER BY rowid
                    ) - 1,
                    content
                FROM {legacy_table}
            "#
        );
        conn.execute(&copy_sql, []).map_err(store_error)?;
    }

    conn.execute(&format!("DROP TABLE {legacy_table}"), [])
        .map_err(store_error)?;
    Ok(())
}

pub(crate) fn translate_corpus_fts_sql_error(
    _conn: &Connection,
    error: rusqlite::Error,
) -> OrbitError {
    store_error(error)
}

/// True when `corpus_fts` still carries the chunk metadata and text itself,
/// i.e. it predates the external-content layout over `chunks`.
fn corpus_fts_has_inline_columns(conn: &Connection) -> Result<bool, OrbitError> {
    if !table_exists(conn, "corpus_fts")? {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('corpus_fts') WHERE name = 'source_kind')",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
    .map_err(store_error)
}

// pub(crate) widened for tests/ layout under ORB-00230; test reaches via
// sibling `tests/schema.rs` (see docs/design-patterns/test_layout.md).
pub(crate) fn table_exists(conn: &Connection, table: &str) -> Result<bool, OrbitError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
    .map_err(store_error)
}

// pub(crate) widened for tests/ layout under ORB-00230; test reaches via
// sibling `tests/schema.rs` (see docs/design-patterns/test_layout.md).
pub(crate) fn legacy_task_fts_table() -> &'static str {
    concat!("tasks", "_fts")
}

fn store_error(error: rusqlite::Error) -> OrbitError {
    OrbitError::Store(error.to_string())
}
