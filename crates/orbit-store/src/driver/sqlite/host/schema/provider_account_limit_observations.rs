use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::{add_column_if_missing, table_exists};

/// v56 `provider_crew_accounts` [ORB-17288]: a crew that sets `env` runs on
/// its own provider account, so its usage readings and its spend are kept
/// apart from its provider's default login's.
///
/// `provider_account_limit_observations` holds a crew account's readings,
/// one row per provider, account, model scope and window, with the columns of
/// `provider_limit_observations`, whose rows stay the default login's.
/// `invocations.account` names the crew account an invocation ran under;
/// `NULL` is the default login, as is every row recorded before it.
///
/// Additive: a new table an older binary never reads, and a nullable column
/// an older binary keeps inserting without, so its rows read as the default
/// login's.
pub(super) fn apply_provider_crew_accounts(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS provider_account_limit_observations (
                provider TEXT NOT NULL,
                account TEXT NOT NULL,
                model_scope TEXT NOT NULL DEFAULT '',
                window_label TEXT NOT NULL DEFAULT '',
                exhausted INTEGER NOT NULL,
                source TEXT NOT NULL,
                resets_at TEXT,
                observed_at TEXT NOT NULL,
                run_id TEXT,
                crew TEXT,
                detail TEXT NOT NULL DEFAULT '',
                used_percent REAL,
                window_minutes INTEGER,
                gating INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (provider, account, model_scope, window_label)
            );
        "#,
    )
    .map_err(sqlite_store_error)?;
    if table_exists(conn, "invocations")? {
        add_column_if_missing(conn, "invocations", "account", "TEXT")?;
    }
    Ok(())
}
