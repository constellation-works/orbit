//! `embeddings` + `chunks` + `corpus_fts` schema bootstrap.
//!
//! Idempotent — runs on every `VectorStore::open`. Kept separate from the
//! store module so future schema changes (e.g. swapping `embedding BLOB` for
//! a `sqlite-vec` virtual table) live in one place.
//!
//! `chunks` is the addressable home of every indexed chunk of text:
//! `(source_kind, source_id, field, chunk_idx)` is unique and indexed, so
//! deletes, snippet lookups and the stored-field probe are index searches.
//! `corpus_fts` is an external-content FTS5 table over `chunks`, holding only
//! the inverted index; triggers keep it in step with its content table. An
//! earlier layout stored the chunk text and its metadata inside `corpus_fts`
//! itself, where every non-`MATCH` access degraded into a full scan
//! [ORB-11695]; `adopt_inline_corpus_fts` migrates those databases in place.
//!
//! The layout is forward-only. Mixed installed binaries during an upgrade are
//! an operational fact, but they must not share a migrated `semantic.db`:
//! restoring the old FTS columns would let an older writer desynchronize the
//! index from `chunks`. Older readers that `SELECT source_kind FROM corpus_fts`
//! get [`incompatible_semantic_index_layout_error`], not a raw SQL column miss.

use orbit_common::OrbitError;
use rusqlite::Connection;

const EMBEDDINGS_DDL: &str = r#"
    CREATE TABLE IF NOT EXISTS embeddings (
        source_kind TEXT NOT NULL,
        source_id TEXT NOT NULL,
        field TEXT NOT NULL,
        chunk_idx INTEGER NOT NULL,
        content_hash TEXT NOT NULL,
        model_id TEXT NOT NULL,
        dim INTEGER NOT NULL,
        embedding BLOB NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (source_kind, source_id, field, chunk_idx, model_id)
    );

    CREATE INDEX IF NOT EXISTS embeddings_by_source
    ON embeddings(source_kind, source_id);

    CREATE INDEX IF NOT EXISTS embeddings_by_model
    ON embeddings(model_id);
"#;

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

pub fn ensure_vector_schema(conn: &Connection) -> Result<(), OrbitError> {
    if is_current_layout(conn)? {
        return Ok(());
    }

    conn.execute_batch("BEGIN IMMEDIATE").map_err(store_error)?;
    match ensure_vector_schema_in_transaction(conn) {
        Ok(()) => conn.execute_batch("COMMIT").map_err(store_error),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Whether this database carries a vector index at all — the current layout or
/// the pre-[ORB-11695] inline one.
///
/// A database with neither has nothing to read: every query would fail on a
/// missing table. It is the difference between a schema call that could not
/// re-apply DDL to an index that already exists and one that could not build
/// the index in the first place.
pub(super) fn vector_schema_present(conn: &Connection) -> Result<bool, OrbitError> {
    Ok(table_exists(conn, "embeddings")?
        || table_exists(conn, "corpus_fts")?
        || table_exists(conn, legacy_task_fts_table())?)
}

fn is_current_layout(conn: &Connection) -> Result<bool, OrbitError> {
    Ok(table_exists(conn, "embeddings")?
        && table_exists(conn, "chunks")?
        && table_exists(conn, "corpus_fts")?
        && !corpus_fts_has_inline_columns(conn)?
        && !table_exists(conn, legacy_task_fts_table())?)
}

fn ensure_vector_schema_in_transaction(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(EMBEDDINGS_DDL).map_err(store_error)?;
    conn.execute_batch(CHUNKS_DDL).map_err(store_error)?;

    let adopted_rows = adopt_inline_corpus_fts(conn)?;
    if !table_exists(conn, "corpus_fts")? {
        conn.execute_batch(CORPUS_FTS_DDL).map_err(store_error)?;
        if adopted_rows {
            conn.execute("INSERT INTO corpus_fts(corpus_fts) VALUES ('rebuild')", [])
                .map_err(store_error)?;
        }
    }

    migrate_legacy_task_fts(conn)
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

/// How `corpus_fts` is physically stored. The pre-[ORB-11695] layout kept
/// `source_kind` / `source_id` / `field` as UNINDEXED columns on the FTS
/// table; the current layout is an external-content FTS5 index over `chunks`
/// and those columns no longer exist on `corpus_fts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CorpusFtsLayout {
    Missing,
    /// Orbit 0.19.0 and earlier: `SELECT source_kind FROM corpus_fts` works.
    InlineMetadata,
    /// Post-[ORB-11695]: BM25 must join `chunks`; inline-column readers cannot.
    ExternalContent,
}

/// Exact BM25 projection shipped in Orbit 0.19.0, before `corpus_fts` lost its
/// inline metadata columns. Current runtimes must not use this SQL against a
/// migrated index; [`evaluate_legacy_inline_reader`] is the compatibility
/// boundary that names that mismatch.
#[cfg(test)]
pub(crate) const LEGACY_INLINE_BM25_SQL: &str = "SELECT source_kind, source_id, field, rowid, bm25(corpus_fts) FROM corpus_fts WHERE corpus_fts MATCH ?1 AND source_kind=?2";

/// Prefix of the operator-facing diagnostic when a reader hits a `corpus_fts`
/// layout this binary cannot use. Matched by hybrid fallback notes; do not
/// include task IDs in the rest of the message either.
pub(crate) const SEMANTIC_INDEX_LAYOUT_INCOMPATIBLE: &str =
    "semantic index layout is incompatible with this Orbit runtime";

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacyInlineHit {
    pub source_kind: String,
    pub source_id: String,
    pub field: String,
    pub rowid: i64,
}

pub(crate) fn corpus_fts_layout(conn: &Connection) -> Result<CorpusFtsLayout, OrbitError> {
    if !table_exists(conn, "corpus_fts")? {
        return Ok(CorpusFtsLayout::Missing);
    }
    if corpus_fts_has_inline_columns(conn)? {
        Ok(CorpusFtsLayout::InlineMetadata)
    } else {
        Ok(CorpusFtsLayout::ExternalContent)
    }
}

/// Run the pre-migration BM25 reader. On the live old-reader/new-layout
/// mismatch this returns [`incompatible_semantic_index_layout_error`] instead
/// of leaking `no such column: source_kind`.
#[cfg(test)]
pub(crate) fn evaluate_legacy_inline_reader(
    conn: &Connection,
    query: &str,
    kind: &str,
) -> Result<Vec<LegacyInlineHit>, OrbitError> {
    let mut stmt = conn
        .prepare(LEGACY_INLINE_BM25_SQL)
        .map_err(|error| translate_corpus_fts_sql_error(conn, error))?;
    let mut rows = stmt
        .query(rusqlite::params![query, kind])
        .map_err(|error| translate_corpus_fts_sql_error(conn, error))?;
    let mut hits = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| translate_corpus_fts_sql_error(conn, error))?
    {
        hits.push(LegacyInlineHit {
            source_kind: row
                .get(0)
                .map_err(|error| translate_corpus_fts_sql_error(conn, error))?,
            source_id: row
                .get(1)
                .map_err(|error| translate_corpus_fts_sql_error(conn, error))?,
            field: row
                .get(2)
                .map_err(|error| translate_corpus_fts_sql_error(conn, error))?,
            rowid: row
                .get(3)
                .map_err(|error| translate_corpus_fts_sql_error(conn, error))?,
        });
    }
    Ok(hits)
}

pub(crate) fn translate_corpus_fts_sql_error(
    conn: &Connection,
    error: rusqlite::Error,
) -> OrbitError {
    if !is_corpus_fts_layout_sql_error(&error) {
        return store_error(error);
    }
    match corpus_fts_layout(conn) {
        Ok(layout) => incompatible_semantic_index_layout_error(layout),
        Err(_) => store_error(error),
    }
}

pub(crate) fn incompatible_semantic_index_layout_error(layout: CorpusFtsLayout) -> OrbitError {
    let detail = match layout {
        CorpusFtsLayout::ExternalContent => {
            "corpus_fts is an external-content FTS5 table over chunks and has no source_kind \
             column. Hybrid BM25 requires a current Orbit binary. Upgrade and restart every \
             Orbit process that opens this workspace's .orbit/state/semantic.db, including \
             Homebrew installs and MCP servers; a newer cargo build that already migrated the \
             index does not upgrade those processes."
        }
        CorpusFtsLayout::InlineMetadata | CorpusFtsLayout::Missing => {
            "corpus_fts still uses inline metadata columns and has no chunks table. This binary \
             expected the external-content layout. Re-open the workspace with a writable current \
             Orbit binary so the in-place migration can run."
        }
    };
    OrbitError::Store(format!(
        "{SEMANTIC_INDEX_LAYOUT_INCOMPATIBLE}: {detail} Lexical (non-hybrid) task and doc \
         lookup does not use this index and remains valid. Do not delete or downgrade \
         semantic.db to make an older binary work."
    ))
}

fn is_corpus_fts_layout_sql_error(error: &rusqlite::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    (message.contains("no such column") && message.contains("source_kind"))
        || (message.contains("no such table") && message.contains("chunks"))
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
