use std::path::Path;

use orbit_common::OrbitError;
use rusqlite::{Connection, TransactionBehavior, types::Value};

use super::REGISTRY_SCHEMA_VERSION;
use super::util::now_string;

pub(super) fn apply_schema(conn: &Connection) -> Result<(), OrbitError> {
    migrate_path_coupled_workspace_bindings(conn)?;
    migrate_allocator_state_v5(conn)?;
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

/// Whether the stored schema is already exactly the shape this build reads.
///
/// Required additive storage is checked even when user_version is current: a
/// complete registry then needs no write transaction, including on read-only
/// media.
fn schema_is_current(conn: &Connection) -> Result<bool, OrbitError> {
    Ok(registry_user_version(conn)? == REGISTRY_SCHEMA_VERSION
        && table_has_column(conn, "task_action_keys", "action_key")?)
}

/// Confirm a registry opened for observation is already readable as-is.
///
/// Setup, migration and v6 recovery all need a write transaction, so on
/// read-only storage there is nothing to attempt: naming the writable step the
/// registry still owes beats an opaque SQLite write error raised from a
/// transaction that could only fail.
pub(super) fn assert_readable_schema(conn: &Connection, path: &Path) -> Result<(), OrbitError> {
    if schema_is_current(conn)? {
        return Ok(());
    }

    let version = registry_user_version(conn)?;
    if version > 6 {
        return Err(unsupported_schema(version, path));
    }
    let action_keys = if table_has_column(conn, "task_action_keys", "action_key")? {
        ""
    } else {
        " without task action keys"
    };
    Err(OrbitError::Store(format!(
        "task registry '{}' requires writable additive setup/recovery: the database is read-only, \
         and observing it found schema version {version}{action_keys} instead of version \
         {REGISTRY_SCHEMA_VERSION} with task action keys. Open the registry once from writable \
         storage, then retry the observation",
        path.display()
    )))
}

pub(super) fn ensure_compatible_schema(
    conn: &mut Connection,
    path: &Path,
) -> Result<(), OrbitError> {
    if schema_is_current(conn)? {
        return Ok(());
    }
    let version = registry_user_version(conn)?;
    if version > 6 {
        return Err(unsupported_schema(version, path));
    }

    // Serialize setup/recovery and re-read the version after taking the lock:
    // another opener may have recovered v6 while this connection was waiting.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| {
            OrbitError::Store(format!(
                "task registry '{}' requires writable additive setup/recovery: {e}",
                path.display()
            ))
        })?;
    let version = registry_user_version(&tx)?;
    if version > REGISTRY_SCHEMA_VERSION && (version != 6 || !is_known_additive_v6(&tx)?) {
        return Err(unsupported_schema(version, path));
    }
    ensure_action_keys(&tx)?;
    if version == 6 {
        tx.pragma_update(None, "user_version", REGISTRY_SCHEMA_VERSION)
            .map_err(|e| {
                OrbitError::Store(format!("recover compatible task registry version: {e}"))
            })?;
    }
    tx.commit()
        .map_err(|e| OrbitError::Store(format!("commit task registry additive setup: {e}")))
}

fn ensure_action_keys(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_action_keys (
            workspace_id TEXT NOT NULL,
            action_key TEXT NOT NULL,
            task_id TEXT NOT NULL UNIQUE,
            input_digest TEXT NOT NULL,
            PRIMARY KEY(workspace_id, action_key)
        );",
    )
    .map_err(|e| OrbitError::Store(format!("ensure task action keys: {e}")))
}

/// PR1509's v6 only added action keys to v5. Match the actual columns, keys,
/// indexes and schema objects before lowering its compatibility marker. Column
/// order in older task indexes differs after ALTER TABLE; their readers name
/// columns explicitly. Action reservations use positional inserts, so retain
/// their column order. This reference must stay at v5 when a future format ships.
fn is_known_additive_v6(conn: &Connection) -> Result<bool, OrbitError> {
    let reference = Connection::open_in_memory().map_err(|e| OrbitError::Store(e.to_string()))?;
    apply_schema(&reference)?;
    ensure_action_keys(&reference)?;
    for query in [
        "SELECT name, type, wr, strict FROM pragma_table_list
         WHERE schema = 'main' AND name NOT GLOB 'sqlite_*' ORDER BY name",
        "SELECT s.type, s.name FROM sqlite_schema s
         WHERE s.name NOT GLOB 'sqlite_*' ORDER BY s.type, s.name",
        "SELECT s.name, p.name, p.type, p.[notnull], p.dflt_value, p.pk, p.hidden
         FROM sqlite_schema s, pragma_table_xinfo(s.name) p
         WHERE s.type = 'table' AND s.name NOT GLOB 'sqlite_*'
         ORDER BY s.name, CASE WHEN s.name = 'task_action_keys' THEN p.cid ELSE 0 END, p.name",
        "SELECT s.name, i.name, i.[unique], i.origin, i.partial, x.seqno, x.name, x.desc, x.coll, x.key
         FROM sqlite_schema s, pragma_index_list(s.name) i, pragma_index_xinfo(i.name) x
         WHERE s.type = 'table' AND s.name NOT GLOB 'sqlite_*'
         ORDER BY s.name, i.name, x.seqno",
    ] {
        if schema_rows(conn, query)? != schema_rows(&reference, query)? {
            return Ok(false);
        }
    }
    // Early registries did not declare all the current foreign keys. Missing
    // keys are a shipped legacy shape; changed or additional keys are not.
    let foreign_keys =
        "SELECT s.name, f.[table], f.[from], f.[to], f.on_update, f.on_delete, f.match
        FROM sqlite_schema s, pragma_foreign_key_list(s.name) f
        WHERE s.type = 'table' ORDER BY s.name, f.id, f.seq";
    let expected_keys = schema_rows(&reference, foreign_keys)?;
    if schema_rows(conn, foreign_keys)?
        .iter()
        .any(|key| !expected_keys.contains(key))
    {
        return Ok(false);
    }

    // These tables carry the allocation/admission constraints. Ignore only
    // formatting and identifier quotes introduced by SQLite table renames.
    let definition = "SELECT sql FROM sqlite_schema WHERE name IN ('allocator_state', 'task_action_keys') ORDER BY name";
    let normalize = |rows: Vec<Vec<Value>>| {
        rows.into_iter()
            .flatten()
            .map(|value| match value {
                Value::Text(sql) => sql
                    .chars()
                    .filter(|c| !c.is_ascii_whitespace() && *c != '"')
                    .collect::<String>(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
    };
    Ok(
        normalize(schema_rows(conn, definition)?)
            == normalize(schema_rows(&reference, definition)?),
    )
}

fn schema_rows(conn: &Connection, query: &str) -> Result<Vec<Vec<Value>>, OrbitError> {
    let mut stmt = conn
        .prepare(query)
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let columns = stmt.column_count();
    stmt.query_map([], |row| {
        (0..columns).map(|column| row.get(column)).collect()
    })
    .map_err(|e| OrbitError::Store(e.to_string()))?
    .collect::<Result<_, _>>()
    .map_err(|e| OrbitError::Store(e.to_string()))
}

fn unsupported_schema(version: u32, path: &Path) -> OrbitError {
    let executable = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown executable".into());
    OrbitError::Store(format!(
        "task registry schema version {version} is newer than supported version {REGISTRY_SCHEMA_VERSION} \
         or is not the known compatible additive format; registry '{}', executable '{}'. \
         Upgrade the selected Orbit executable and restart long-lived processes. \
         Check `command -v orbit` and `orbit --version` for mixed installations; \
         do not edit the database version manually",
        path.display(),
        executable
    ))
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
