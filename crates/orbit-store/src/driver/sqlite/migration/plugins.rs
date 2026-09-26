use orbit_common::OrbitError;
use rusqlite::Connection;

use super::audit_events::ensure_audit_events_schema;
use super::introspect::{add_column_if_missing, table_exists};

/// v24 `plugins_and_audit_plugin_provenance` migration: the host-local
/// installed-plugin record beside `tools`, and the three audit columns that
/// name the plugin behind a tool call (design `docs/design/plugins/1_scope.md`
/// §3, §4.4). Additive: an older binary ignores the table and the columns.
pub(super) fn apply_plugins_and_audit_plugin_provenance(
    conn: &Connection,
) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS plugins (
                name TEXT PRIMARY KEY,
                version TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT '',
                install_path TEXT NOT NULL,
                manifest_digest TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 0,
                grants_json TEXT NOT NULL DEFAULT '[]',
                first_party INTEGER NOT NULL DEFAULT 0,
                installed_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))?;
    ensure_audit_events_schema(conn)?;
    for column in ["plugin_name", "plugin_version", "plugin_manifest_digest"] {
        add_column_if_missing(
            conn,
            &format!("ALTER TABLE audit_events ADD COLUMN {column} TEXT"),
        )?;
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_audit_events_plugin_name
            ON audit_events(plugin_name);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v30 `audit_plugin_secrets` migration: the names (JSON array) of the
/// declared secrets a plugin-backed tool call's request carried, beside the
/// plugin provenance columns (design `docs/design/plugins/1_scope.md` §3,
/// "Plugin secrets"). Names only; a value never reaches the table. Additive:
/// an older binary ignores the column.
pub(super) fn apply_audit_plugin_secrets(conn: &Connection) -> Result<(), OrbitError> {
    ensure_audit_events_schema(conn)?;
    add_column_if_missing(
        conn,
        "ALTER TABLE audit_events ADD COLUMN plugin_secrets TEXT",
    )
}

/// v27 `plugin_certified_orbit_version` migration: the Orbit version a
/// plugin's `spec.tests` goldens last passed on, written by `orbit plugin
/// test` and printed by `orbit plugin show` (design
/// `docs/design/plugins/1_scope.md` §5). Additive: an older binary ignores
/// the column, and a host that has never run a conformance suite reads NULL.
pub(super) fn apply_plugin_certified_orbit_version(conn: &Connection) -> Result<(), OrbitError> {
    if !table_exists(conn, "plugins")? {
        return Ok(());
    }
    add_column_if_missing(
        conn,
        "ALTER TABLE plugins ADD COLUMN certified_orbit_version TEXT",
    )
}

/// v28 `plugin_archive_digest` migration: the SHA-256 of the archive a
/// digest-pinned `https://` plugin source was fetched from and verified
/// against at install time. Additive: an older binary ignores the column, and
/// every plugin installed from a directory, a `git+` clone, or a local
/// archive reads NULL because Orbit downloaded nothing for it.
pub(super) fn apply_plugin_archive_digest(conn: &Connection) -> Result<(), OrbitError> {
    if !table_exists(conn, "plugins")? {
        return Ok(());
    }
    add_column_if_missing(conn, "ALTER TABLE plugins ADD COLUMN archive_digest TEXT")
}
