use orbit_common::OrbitError;
use rusqlite::Connection;

use super::introspect::{add_column_if_missing, table_exists, table_has_column};

/// v2 `learnings_index_workspace_scope` migration (ORB-10113): re-key the
/// learning envelope index by `(workspace_id, id)`.
///
/// The index previously had no workspace discriminator and was keyed only by
/// learning ID, so in the shared host-global database rows written by one
/// workspace leaked into another workspace's searches and reminders. YAML
/// under each `.orbit/learnings/` is the source of truth, and legacy rows
/// cannot be attributed to a workspace reliably, so this migration discards
/// every indexed row and lets each runtime rebuild its own rows from YAML via
/// `sync_learnings`. It touches only SQLite — no `learning.yaml` file is read
/// or modified.
pub(super) fn apply_learning_index_workspace_scope(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            DROP TABLE IF EXISTS learnings_index;

            CREATE TABLE learnings_index (
                workspace_id TEXT NOT NULL,
                id           TEXT NOT NULL,
                status       TEXT NOT NULL,
                paths        TEXT NOT NULL,
                tags         TEXT NOT NULL,
                summary      TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                priority     INTEGER,
                PRIMARY KEY (workspace_id, id)
            );

            CREATE INDEX IF NOT EXISTS learnings_active
                ON learnings_index(workspace_id, status) WHERE status = 'active';
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))
}

pub(super) fn ensure_learning_index_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            -- Project-learnings envelope index. YAML records live on disk under
            -- `<root>/<id>/learning.yaml`; status lives in the YAML body.
            -- this table indexes the envelope fields for fast scope-glob
            -- lookups. Arrays are stored as JSON strings for the same reason
            -- the ADR index does it: phase-1 corpora are small and a junction
            -- table is overkill. Per ADR-004, ranking and FTS over body
            -- content are deferred to phase 2.
            CREATE TABLE IF NOT EXISTS learnings_index (
                id          TEXT PRIMARY KEY,
                status      TEXT NOT NULL,
                paths       TEXT NOT NULL,
                tags        TEXT NOT NULL,
                summary     TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS learnings_active
                ON learnings_index(status) WHERE status = 'active';
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    // C2 (T20260511-6) adds an optional `priority` column used as the
    // secondary ranking key in `search`. NULL is acceptable; the search
    // path orders Some(N) ahead of None and falls back to updated_at.
    add_column_if_missing(
        conn,
        "ALTER TABLE learnings_index ADD COLUMN priority INTEGER",
    )?;

    Ok(())
}

/// v14 `remove_native_learning_subsystem` (ORB-10736): remove every SQLite
/// projection owned by the retired native learning resource. Existing files
/// under `.orbit/learnings/` are deliberately outside the database migration
/// and remain untouched as inert historical data.
pub(super) fn apply_remove_native_learning_subsystem(conn: &Connection) -> Result<(), OrbitError> {
    if table_exists(conn, "embeddings")? && table_has_column(conn, "embeddings", "source_kind")? {
        conn.execute("DELETE FROM embeddings WHERE source_kind = 'learning'", [])
            .map_err(|error| OrbitError::Store(error.to_string()))?;
    }
    conn.execute_batch(
        r#"
            DROP TABLE IF EXISTS learnings_index;
            DROP TABLE IF EXISTS session_learning_state;
            DROP TABLE IF EXISTS id_allocations;
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}
