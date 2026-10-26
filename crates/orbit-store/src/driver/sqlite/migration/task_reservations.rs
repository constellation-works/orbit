use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::add_column_if_missing;

/// Durable notices and an acknowledgement cursor, additive to existing leases.
pub(super) fn apply_lease_notices(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(
        conn,
        "task_reservations",
        "notice_cursor",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS lease_notices (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id TEXT,
            workspace_orbit_dir TEXT NOT NULL,
            lease_id TEXT NOT NULL REFERENCES task_reservations(reservation_id),
            kind TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            event_key TEXT NOT NULL,
            UNIQUE(lease_id, event_key)
        );
        CREATE INDEX IF NOT EXISTS lease_notices_workspace_seq ON lease_notices(workspace_id, seq);
        CREATE INDEX IF NOT EXISTS lease_notices_dir_seq ON lease_notices(workspace_orbit_dir, seq);"
    ).map_err(sqlite_store_error)
}

/// v13 `workspace_claim_scope` migration (ORB-10709, ADR-0352).
///
/// The exclusive workspace claim reuses `task_reservations` rather than a
/// parallel table, because atomic acquisition, TTL, lazy expiry, audit, and a
/// release escape hatch already live there. `scope` is the dimension that keeps
/// the two apart — worker file reservations arbitrate over paths, the claim
/// arbitrates dispatch authority — and every existing row is a file
/// reservation, which is exactly what the column default says.
///
/// `claim_token` is the holder's bearer token. It is deliberately absent from
/// `select_reservation_columns()`, so no reservation read path can carry it
/// into a response.
pub(super) fn apply_workspace_claim_scope(conn: &Connection) -> Result<(), OrbitError> {
    // A database that recorded a ledger version without ever running the
    // baseline has no reservation table to widen. Create it at its baseline
    // shape first so this migration widens a table that exists rather than
    // wedging the ledger; on every ordinary database this is a no-op.
    ensure_task_reservations_schema_v13(conn)?;

    add_column_if_missing(
        conn,
        "task_reservations",
        "scope",
        "TEXT NOT NULL DEFAULT 'files'",
    )?;
    add_column_if_missing(conn, "task_reservations", "claim_token", "TEXT")?;

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_task_reservations_scope_release
            ON task_reservations(workspace_id, scope, released_at);
        "#,
    )
    .map_err(sqlite_store_error)?;

    Ok(())
}

fn ensure_task_reservations_schema_v13(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS task_reservations (
                reservation_id TEXT PRIMARY KEY,
                workspace_orbit_dir TEXT NOT NULL,
                task_ids_json TEXT NOT NULL,
                files_json TEXT NOT NULL,
                actor TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                released_at TEXT,
                owner_run_id TEXT,
                owner_metadata_json TEXT,
                release_reason TEXT,
                release_metadata_json TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_expires
            ON task_reservations(workspace_orbit_dir, expires_at);

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_release
            ON task_reservations(workspace_orbit_dir, released_at);
        "#,
    )
    .map_err(sqlite_store_error)?;

    add_column_if_missing(conn, "task_reservations", "workspace_id", "TEXT")?;
    add_column_if_missing(conn, "task_reservations", "owner_run_id", "TEXT")?;
    add_column_if_missing(conn, "task_reservations", "owner_metadata_json", "TEXT")?;
    add_column_if_missing(conn, "task_reservations", "release_reason", "TEXT")?;
    add_column_if_missing(conn, "task_reservations", "release_metadata_json", "TEXT")?;

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_owner_release
            ON task_reservations(workspace_orbit_dir, owner_run_id, released_at);

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_id_release
            ON task_reservations(workspace_id, released_at);

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_id_expires
            ON task_reservations(workspace_id, expires_at);
        "#,
    )
    .map_err(sqlite_store_error)?;

    Ok(())
}

/// v61 `task_reservation_leases` migration: path leases (`docs/design/path-leases`).
///
/// A lease is a `task_reservations` row held by a principal rather than a task.
/// Its rows keep `scope = 'files'`, so a binary without this migration reads
/// every lease, advisory ones included, as an exclusive file reservation: it
/// over-blocks rather than letting an overlap through. Every existing row and
/// every row such a binary writes later reads through the column defaults as a
/// hard task reservation, which is exactly what it is.
pub(super) fn apply_task_reservation_leases(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(
        conn,
        "task_reservations",
        "kind",
        "TEXT NOT NULL DEFAULT 'task_reservation'",
    )?;
    add_column_if_missing(
        conn,
        "task_reservations",
        "mode",
        "TEXT NOT NULL DEFAULT 'hard'",
    )?;
    for column in [
        "principal",
        "principal_class",
        "machine_id",
        "priority",
        "tags_json",
        "base_branch",
        "bound_branch",
        "heartbeat_at",
    ] {
        add_column_if_missing(conn, "task_reservations", column, "TEXT")?;
    }

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_task_reservations_kind_release
            ON task_reservations(workspace_id, kind, released_at);
        "#,
    )
    .map_err(sqlite_store_error)?;

    Ok(())
}

/// v66 `task_reservation_lease_token` migration: the lease token id a lease
/// intake request authenticated with (`docs/design/path-leases`, §7.2).
///
/// One nullable column. Rows written before it, or by a binary without it,
/// read as leases requested without a token.
pub(super) fn apply_task_reservation_lease_token(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(conn, "task_reservations", "token_id", "TEXT")
}
