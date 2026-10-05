use orbit_common::OrbitError;
use rusqlite::Connection;

use super::introspect::{add_column_if_missing, table_exists};

/// v12 `friction_records_sqlite` migration (ORB-10680): hub friction records
/// move from the per-workspace Markdown tree into the host-global store.
///
/// Identity is composite `(workspace_id, friction_id)` (L-0072): friction IDs
/// stay workspace-local, so the same `F2026-05-001` in two workspaces are two
/// distinct rows. Indexes cover the read shapes the scan path used to satisfy
/// by parsing the whole corpus — workspace + status + creation order, and
/// workspace + tag.
///
/// `legacy_path` is the read-only evidence pointer for an imported record and
/// is NULL for anything written after cutover (ADR-0345).
pub(super) fn apply_friction_records_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS friction_records (
                workspace_id     TEXT NOT NULL,
                friction_id      TEXT NOT NULL,
                month            TEXT NOT NULL,
                seq              INTEGER NOT NULL,
                title            TEXT,
                model            TEXT NOT NULL,
                status           TEXT NOT NULL,
                created_at       TEXT NOT NULL,
                resolved_at      TEXT,
                during_task      TEXT,
                resolved_by_task TEXT,
                tags_json        TEXT NOT NULL DEFAULT '[]',
                body             TEXT NOT NULL,
                legacy_path      TEXT,
                PRIMARY KEY(workspace_id, friction_id),
                CHECK (length(workspace_id) > 0 AND workspace_id = trim(workspace_id)),
                CHECK (length(friction_id) > 0 AND friction_id = trim(friction_id)),
                CHECK (length(month) = 7),
                CHECK (typeof(seq) = 'integer' AND seq > 0),
                CHECK (status IN ('open', 'triaged', 'resolved')),
                CHECK (length(model) > 0),
                CHECK (length(created_at) > 0),
                CHECK (json_valid(tags_json) AND json_type(tags_json) = 'array')
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_friction_records_month_seq
                ON friction_records(workspace_id, month, seq);
            CREATE INDEX IF NOT EXISTS idx_friction_records_status_created
                ON friction_records(workspace_id, status, created_at, friction_id);
            CREATE INDEX IF NOT EXISTS idx_friction_records_created
                ON friction_records(workspace_id, created_at, friction_id);

            CREATE TABLE IF NOT EXISTS friction_record_tags (
                workspace_id TEXT NOT NULL,
                friction_id  TEXT NOT NULL,
                tag          TEXT NOT NULL,
                PRIMARY KEY(workspace_id, friction_id, tag),
                CHECK (length(tag) > 0 AND tag = trim(tag)),
                FOREIGN KEY(workspace_id, friction_id)
                    REFERENCES friction_records(workspace_id, friction_id)
                    ON UPDATE CASCADE ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_friction_record_tags_lookup
                ON friction_record_tags(workspace_id, tag, friction_id);

            CREATE TABLE IF NOT EXISTS friction_import_state (
                workspace_id   TEXT NOT NULL,
                source_key     TEXT NOT NULL,
                record_count   INTEGER NOT NULL,
                imported_count INTEGER NOT NULL,
                schema_version INTEGER NOT NULL,
                completed_at   TEXT NOT NULL,
                PRIMARY KEY(workspace_id, source_key),
                CHECK (length(workspace_id) > 0),
                CHECK (typeof(record_count) = 'integer' AND record_count >= 0),
                CHECK (typeof(imported_count) = 'integer' AND imported_count >= 0),
                CHECK (typeof(schema_version) = 'integer' AND schema_version > 0),
                CHECK (length(completed_at) > 0)
            );
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v29 `friction_rehome_target` migration: the workspace that owns a friction
/// recorded somewhere else — curation's `rehome_required` disposition, and the
/// forwarding pointer `orbit.friction.rehome` leaves on the record it moved.
/// Additive: an older binary ignores the column, and its upsert leaves it
/// untouched because the column is absent from that binary's `SET` list.
pub(super) fn apply_friction_rehome_target(conn: &Connection) -> Result<(), OrbitError> {
    if !table_exists(conn, "friction_records")? {
        return Ok(());
    }
    add_column_if_missing(
        conn,
        "ALTER TABLE friction_records ADD COLUMN rehome_to TEXT",
    )
}
