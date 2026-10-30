use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v74 `script_grants` migration: the host-local operator grant for one
/// workspace script at one content digest, beside `plugins` (script steps
/// phase 1). Additive: an older binary never reads the table, and its job
/// loader still refuses `target: script:` as an unknown ref kind. The row
/// is only authority with its witness under the global root's
/// `scripts/.grants/`; the table alone grants nothing.
pub(super) fn apply_script_grants(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS script_grant (
                workspace_id TEXT NOT NULL,
                name TEXT NOT NULL,
                grant_id TEXT NOT NULL,
                digest TEXT NOT NULL,
                manifest_digest TEXT NOT NULL,
                entry_digest TEXT NOT NULL,
                interpreter_paths_json TEXT NOT NULL DEFAULT '[]',
                network TEXT NOT NULL DEFAULT 'none',
                read_roots_json TEXT NOT NULL DEFAULT '[]',
                write_roots_json TEXT NOT NULL DEFAULT '[]',
                env_pass_json TEXT NOT NULL DEFAULT '[]',
                limits_json TEXT NOT NULL DEFAULT '{}',
                host_role TEXT NOT NULL DEFAULT 'owner',
                grantor TEXT NOT NULL DEFAULT '',
                granted_at TEXT NOT NULL DEFAULT (datetime('now')),
                provenance TEXT NOT NULL DEFAULT 'cli',
                expires_at TEXT,
                PRIMARY KEY (workspace_id, name)
            );
        "#,
    )
    .map_err(sqlite_store_error)
}
