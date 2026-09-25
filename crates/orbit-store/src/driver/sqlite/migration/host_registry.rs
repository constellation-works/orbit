use orbit_common::OrbitError;
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
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v6 `workspace_coordination_projections` migration (ORB-10257): singular
/// workspace ownership, private host-keyed presence, and owner-published
/// execution profiles. The schema is additive and keeps owner payload JSON
/// separate from hub-owned generation/receipt metadata.
pub(super) fn apply_workspace_coordination_projections(
    conn: &Connection,
) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS workspace_ownership (
                workspace_id     TEXT PRIMARY KEY,
                owner_machine_id TEXT NOT NULL,
                bound_at         TEXT NOT NULL,
                updated_at       TEXT NOT NULL,
                CHECK (length(workspace_id) > 0),
                FOREIGN KEY(owner_machine_id) REFERENCES hosts(machine_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT
            );

            CREATE INDEX IF NOT EXISTS idx_workspace_ownership_owner
                ON workspace_ownership(owner_machine_id, workspace_id);

            CREATE TABLE IF NOT EXISTS host_workspace_presence (
                machine_id   TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                root         TEXT NOT NULL,
                last_verified TEXT NOT NULL,
                PRIMARY KEY(machine_id, workspace_id),
                CHECK (length(workspace_id) > 0),
                CHECK (length(root) > 0),
                FOREIGN KEY(machine_id) REFERENCES hosts(machine_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT
            );

            CREATE INDEX IF NOT EXISTS idx_host_workspace_presence_workspace
                ON host_workspace_presence(workspace_id, machine_id);

            CREATE TABLE IF NOT EXISTS workspace_execution_profiles (
                workspace_id     TEXT PRIMARY KEY,
                owner_machine_id TEXT NOT NULL,
                generation       INTEGER NOT NULL CHECK (generation >= 1),
                payload_json     TEXT NOT NULL,
                received_at      TEXT NOT NULL,
                CHECK (json_valid(payload_json) AND json_type(payload_json) = 'object'),
                FOREIGN KEY(workspace_id) REFERENCES workspace_ownership(workspace_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT,
                FOREIGN KEY(owner_machine_id) REFERENCES hosts(machine_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT
            );

            CREATE TRIGGER IF NOT EXISTS execution_profile_owner_matches_insert
            BEFORE INSERT ON workspace_execution_profiles
            WHEN NOT EXISTS (
                SELECT 1 FROM workspace_ownership o
                WHERE o.workspace_id = NEW.workspace_id
                  AND o.owner_machine_id = NEW.owner_machine_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'execution profile owner does not match workspace ownership');
            END;

            CREATE TRIGGER IF NOT EXISTS execution_profile_owner_matches_update
            BEFORE UPDATE OF workspace_id, owner_machine_id ON workspace_execution_profiles
            WHEN NOT EXISTS (
                SELECT 1 FROM workspace_ownership o
                WHERE o.workspace_id = NEW.workspace_id
                  AND o.owner_machine_id = NEW.owner_machine_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'execution profile owner does not match workspace ownership');
            END;
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v8 `hub_registry_metadata` migration (ORB-10267): one singleton hub-global
/// metadata row carrying the configured hub `machine_id` and a monotonic
/// `registry_revision`. Every snapshot-visible host/alias/retirement,
/// ownership, presence, or execution-profile mutation advances the revision
/// exactly once inside its own transaction; per-workspace execution-profile
/// generation remains a separate concept. The `id = 0` primary-key CHECK is
/// the SQLite singleton guard, and the row is seeded here so every reader sees
/// a revision without a lazy insert path.
pub(super) fn apply_hub_registry_metadata(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS hub_registry_metadata (
                id                INTEGER PRIMARY KEY CHECK (id = 0),
                hub_machine_id    TEXT,
                registry_revision INTEGER NOT NULL DEFAULT 0
                    CHECK (
                        typeof(registry_revision) = 'integer'
                        AND registry_revision >= 0
                        AND registry_revision <= 9223372036854775807
                    ),
                updated_at        TEXT NOT NULL,
                CHECK (hub_machine_id IS NULL OR length(hub_machine_id) > 0)
            );

            INSERT OR IGNORE INTO hub_registry_metadata(
                id, hub_machine_id, registry_revision, updated_at
            ) VALUES (0, NULL, 0, datetime('now'));
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}
