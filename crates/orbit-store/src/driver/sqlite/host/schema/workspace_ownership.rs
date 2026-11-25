use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

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
    .map_err(sqlite_store_error)
}
