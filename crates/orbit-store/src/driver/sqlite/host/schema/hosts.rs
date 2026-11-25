use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v5 `host_registry_core` migration (ORB-10255): durable machine identity,
/// lifecycle state, and immutable historical names in the hub-global store.
///
/// This migration is strictly additive. Cross-table triggers backstop the
/// typed API's preflight so a current or tombstoned `host_id` can never exist
/// for two machines, even when the database is modified outside that API.
pub(super) fn apply_host_registry_core(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS hosts (
                machine_id    TEXT PRIMARY KEY,
                host_id       TEXT NOT NULL UNIQUE,
                labels_json   TEXT NOT NULL DEFAULT '[]',
                status        TEXT NOT NULL CHECK (status IN ('active', 'retired')),
                registered_at TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                retired_at    TEXT,
                last_seen_at  TEXT,
                CHECK (length(machine_id) > 0),
                CHECK (length(host_id) > 0),
                CHECK (json_valid(labels_json) AND json_type(labels_json) = 'array'),
                CHECK (
                    (status = 'active' AND retired_at IS NULL)
                    OR (status = 'retired' AND retired_at IS NOT NULL)
                )
            );

            CREATE TABLE IF NOT EXISTS host_aliases (
                host_id    TEXT PRIMARY KEY,
                machine_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                warning    TEXT NOT NULL,
                CHECK (length(host_id) > 0),
                CHECK (length(warning) > 0),
                FOREIGN KEY(machine_id) REFERENCES hosts(machine_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT
            );

            CREATE INDEX IF NOT EXISTS idx_hosts_status_host_id
                ON hosts(status, host_id);
            CREATE INDEX IF NOT EXISTS idx_host_aliases_machine_id
                ON host_aliases(machine_id, created_at);

            CREATE TRIGGER IF NOT EXISTS hosts_host_id_not_alias_insert
            BEFORE INSERT ON hosts
            WHEN EXISTS (
                SELECT 1 FROM host_aliases WHERE host_id = NEW.host_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'host_id is reserved by a permanent alias');
            END;

            CREATE TRIGGER IF NOT EXISTS hosts_host_id_not_alias_update
            BEFORE UPDATE OF host_id ON hosts
            WHEN EXISTS (
                SELECT 1 FROM host_aliases WHERE host_id = NEW.host_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'host_id is reserved by a permanent alias');
            END;

            CREATE TRIGGER IF NOT EXISTS host_alias_not_current_name_insert
            BEFORE INSERT ON host_aliases
            WHEN EXISTS (
                SELECT 1 FROM hosts WHERE host_id = NEW.host_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'host alias conflicts with a current host_id');
            END;

            CREATE TRIGGER IF NOT EXISTS host_aliases_immutable_update
            BEFORE UPDATE ON host_aliases
            BEGIN
                SELECT RAISE(ABORT, 'host aliases are immutable');
            END;

            CREATE TRIGGER IF NOT EXISTS host_aliases_immutable_delete
            BEFORE DELETE ON host_aliases
            BEGIN
                SELECT RAISE(ABORT, 'host aliases are permanent');
            END;
        "#,
    )
    .map_err(sqlite_store_error)
}
