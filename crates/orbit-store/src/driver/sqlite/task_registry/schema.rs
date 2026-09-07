use orbit_common::OrbitError;
use rusqlite::Connection;

use super::REGISTRY_SCHEMA_VERSION;
use super::util::now_string;

pub(super) fn apply_schema(conn: &Connection) -> Result<(), OrbitError> {
    migrate_path_coupled_workspace_bindings(conn)?;
    migrate_allocator_state_v5(conn)?;
    // Schema v6: permanent action-to-task reservations survive bundle recovery.
    // Some fresh v5 registries already contain this table. Keep their mappings
    // intact, and allow replay if opening stopped before user_version was set.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_action_keys (
            workspace_id TEXT NOT NULL,
            action_key TEXT NOT NULL,
            task_id TEXT NOT NULL UNIQUE,
            input_digest TEXT NOT NULL,
            PRIMARY KEY(workspace_id, action_key)
        );",
    )
    .map_err(|e| OrbitError::Store(format!("migrate task action keys to registry v6: {e}")))?;
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS allocator_state (
            authority TEXT PRIMARY KEY,
            next_number INTEGER NOT NULL CHECK(next_number >= 0),
            task_prefix TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS workspace_bindings (
            workspace_id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            repo_fingerprint TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS workspace_checkout_bindings (
            workspace_id TEXT PRIMARY KEY,
            repo_root TEXT NOT NULL,
            workspace_path TEXT NOT NULL,
            orbit_dir TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_workspace_checkout_bindings_paths
            ON workspace_checkout_bindings(repo_root, workspace_path, orbit_dir);

        CREATE TABLE IF NOT EXISTS task_bundle_bindings (
            task_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            canonical_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_task_bundle_bindings_workspace
            ON task_bundle_bindings(workspace_id, task_id);

        CREATE TABLE IF NOT EXISTS task_bundle_index (
            task_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            status TEXT NOT NULL,
            priority TEXT NOT NULL,
            job_run_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            terminal_month TEXT,
            complexity TEXT,
            FOREIGN KEY(task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE,
            FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_created
            ON task_bundle_index(workspace_id, created_at DESC, task_id ASC);
        CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_status
            ON task_bundle_index(workspace_id, status, created_at DESC, task_id ASC);
        CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_priority
            ON task_bundle_index(workspace_id, priority, created_at DESC, task_id ASC);

        CREATE TABLE IF NOT EXISTS task_bundle_tags (
            task_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            tag TEXT NOT NULL,
            PRIMARY KEY(task_id, tag),
            FOREIGN KEY(task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE,
            FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_task_bundle_tags_workspace_tag
            ON task_bundle_tags(workspace_id, tag, task_id);

        CREATE TABLE IF NOT EXISTS task_bundle_relations (
            source_task_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            relation_type TEXT NOT NULL,
            target_task_id TEXT NOT NULL,
            PRIMARY KEY(source_task_id, relation_type, target_task_id),
            FOREIGN KEY(source_task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE,
            FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_task_bundle_relations_workspace_type_target
            ON task_bundle_relations(workspace_id, relation_type, target_task_id, source_task_id);
        ",
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    add_column_if_missing(
        conn,
        "task_bundle_index",
        "job_run_id",
        "ALTER TABLE task_bundle_index ADD COLUMN job_run_id TEXT",
    )?;
    add_column_if_missing(
        conn,
        "task_bundle_index",
        "terminal_month",
        "ALTER TABLE task_bundle_index ADD COLUMN terminal_month TEXT",
    )?;
    // Nullable on purpose: existing rows stay NULL until the first aggregate
    // rebuilds them from bundles. Empty string is the indexed "unset" value.
    add_column_if_missing(
        conn,
        "task_bundle_index",
        "complexity",
        "ALTER TABLE task_bundle_index ADD COLUMN complexity TEXT",
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_job_run
            ON task_bundle_index(workspace_id, job_run_id, created_at DESC, task_id ASC)",
        [],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_terminal
            ON task_bundle_index(workspace_id, terminal_month, task_id)",
        [],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_complexity
            ON task_bundle_index(workspace_id, complexity, status)",
        [],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    conn.execute(
        "INSERT OR IGNORE INTO allocator_state(authority, next_number, task_prefix, updated_at)
         VALUES ('local', 0, 'ORB', ?1)",
        [now_string()],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    conn.pragma_update(None, "user_version", i64::from(REGISTRY_SCHEMA_VERSION))
        .map_err(|e| OrbitError::Store(format!("failed to set registry user_version: {e}")))?;
    Ok(())
}

/// Schema v5 stores the machine's immutable minting prefix alongside its one
/// monotonic allocator and removes the obsolete five-digit ceiling.
fn migrate_allocator_state_v5(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "allocator_state", "next_number")?
        || table_has_column(conn, "allocator_state", "task_prefix")?
    {
        return Ok(());
    }

    conn.execute_batch(
        "
        BEGIN IMMEDIATE;
        CREATE TABLE allocator_state_v5 (
            authority TEXT PRIMARY KEY,
            next_number INTEGER NOT NULL CHECK(next_number >= 0),
            task_prefix TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        INSERT INTO allocator_state_v5(authority, next_number, task_prefix, updated_at)
        SELECT authority, next_number, 'ORB', updated_at FROM allocator_state;
        DROP TABLE allocator_state;
        ALTER TABLE allocator_state_v5 RENAME TO allocator_state;
        COMMIT;
        ",
    )
    .map_err(|error| OrbitError::Store(format!("migrate task allocator to v5: {error}")))
}

/// Schema v4 splits stable coordination identity from machine-local checkout
/// paths. Rebuild the parent table under the same final name so existing child
/// foreign keys keep referencing `workspace_bindings` without rewriting any
/// task, index, tag, relation, or allocator rows.
fn migrate_path_coupled_workspace_bindings(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "workspace_bindings", "repo_root")? {
        return Ok(());
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")
        .map_err(|e| OrbitError::Store(format!("start task registry v4 migration: {e}")))?;
    let migration = conn.execute_batch(
        "
        CREATE TABLE workspace_bindings_v4 (
            workspace_id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            repo_fingerprint TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        INSERT INTO workspace_bindings_v4(
            workspace_id, slug, repo_fingerprint, created_at, updated_at
        )
        SELECT workspace_id, slug, repo_fingerprint, created_at, updated_at
        FROM workspace_bindings;

        CREATE TABLE workspace_checkout_bindings (
            workspace_id TEXT PRIMARY KEY,
            repo_root TEXT NOT NULL,
            workspace_path TEXT NOT NULL,
            orbit_dir TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
        );
        INSERT INTO workspace_checkout_bindings(
            workspace_id, repo_root, workspace_path, orbit_dir, created_at, updated_at
        )
        SELECT workspace_id, repo_root, workspace_path, orbit_dir, created_at, updated_at
        FROM workspace_bindings;

        DROP TABLE workspace_bindings;
        ALTER TABLE workspace_bindings_v4 RENAME TO workspace_bindings;
        ",
    );
    if let Err(error) = migration {
        let _ = conn.execute_batch("ROLLBACK; PRAGMA foreign_keys = ON;");
        return Err(OrbitError::Store(format!(
            "migrate task registry workspace bindings to v4: {error}"
        )));
    }
    conn.execute_batch("COMMIT; PRAGMA foreign_keys = ON;")
        .map_err(|e| OrbitError::Store(format!("finish task registry v4 migration: {e}")))?;
    Ok(())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, OrbitError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| OrbitError::Store(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(columns.iter().any(|candidate| candidate == column))
}

pub(super) fn reject_unsupported_registry_schema(conn: &Connection) -> Result<(), OrbitError> {
    let version = registry_user_version(conn)?;
    if version > REGISTRY_SCHEMA_VERSION {
        return Err(OrbitError::Store(format!(
            "task registry schema version {version} is newer than supported version {REGISTRY_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> Result<(), OrbitError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| OrbitError::Store(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    if !columns.iter().any(|candidate| candidate == column) {
        conn.execute(alter_sql, [])
            .map_err(|e| OrbitError::Store(e.to_string()))?;
    }
    Ok(())
}

pub(super) fn registry_user_version(conn: &Connection) -> Result<u32, OrbitError> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| OrbitError::Store(format!("failed to read registry user_version: {e}")))?;
    u32::try_from(version)
        .map_err(|e| OrbitError::Store(format!("invalid registry user_version {version}: {e}")))
}

pub(super) fn assert_registry_user_version(conn: &Connection) -> Result<(), OrbitError> {
    let version = registry_user_version(conn)?;
    if version != REGISTRY_SCHEMA_VERSION {
        return Err(OrbitError::Store(format!(
            "task registry schema version {version} did not match expected version {REGISTRY_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}
