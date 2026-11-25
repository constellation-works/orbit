use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::add_column_if_missing;

/// v40 `provider_limit_observations` [ORB-14695]: the host's provider usage
/// limits, beside the invocation ledger in the host-global database, since a
/// limit belongs to the provider login on this host.
///
/// One row per provider, model scope and window; the empty string stands for
/// an account-wide scope or an unlabelled window so the key stays unique.
/// Purely additive: an older binary never reads the table.
pub(super) fn apply_provider_limit_observations(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS provider_limit_observations (
                provider TEXT NOT NULL,
                model_scope TEXT NOT NULL DEFAULT '',
                window_label TEXT NOT NULL DEFAULT '',
                exhausted INTEGER NOT NULL,
                source TEXT NOT NULL,
                resets_at TEXT,
                observed_at TEXT NOT NULL,
                run_id TEXT,
                crew TEXT,
                detail TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (provider, model_scope, window_label)
            );
        "#,
    )
    .map_err(sqlite_store_error)
}

/// v41 `provider_limit_readings` [ORB-14696]: a provider's own reading of a
/// usage window, recorded after every Codex or Claude run: the percent used,
/// the window's length, and whether the window gates the account (an overage
/// window does not).
///
/// Additive: an older binary still inserts and updates the v40 columns. Its
/// update of an existing row leaves these columns from the earlier reading,
/// so a reader trusts them only on a row whose `source` is `event`.
pub(super) fn apply_provider_limit_readings(conn: &Connection) -> Result<(), OrbitError> {
    for (column, definition) in [
        ("used_percent", "REAL"),
        ("window_minutes", "INTEGER"),
        ("gating", "INTEGER NOT NULL DEFAULT 1"),
    ] {
        add_column_if_missing(conn, "provider_limit_observations", column, definition)?;
    }
    Ok(())
}
