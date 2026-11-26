//! Registry v1 `registry_baseline`: a frozen copy of the pre-ledger v5 schema
//! setup, its action keys and its envelope stamps.
//!
//! Frozen like every shipped ledger body: a later registry shape is a later
//! entry, never an edit here. Everything below is private to v1, including
//! the pre-v5 rewrites and the v6 recognition, which remain only as the path
//! by which a pre-ledger registry adopts the ledger.
//!
//! The ledger engine runs this body in one `BEGIN IMMEDIATE` transaction with
//! foreign-key enforcement off, then writes the ledger row, the compatibility
//! record and the `user_version` fence. Allocator seeding is data, not shape:
//! the open path does it after the ledger has run.

use orbit_common::storage::sqlite::{sqlite_error, sqlite_store_error};
use orbit_common::{OrbitError, StorageLayer};
use rusqlite::{Connection, types::Value};

/// The pre-ledger reader floor: the format v1 adopts and pre-ledger binaries
/// accept.
const PRE_LEDGER_VERSION: u32 = 5;
/// The shipped PR1509 marker for v5 plus action keys, which v1 also adopts
/// once [`is_known_additive_v6`] recognizes its shape.
const PRE_LEDGER_V6: u32 = 6;

pub(super) fn apply_registry_baseline(conn: &Connection) -> Result<(), OrbitError> {
    // Re-checked inside the migration transaction: another opener may have
    // marked the registry while this one waited for the write lock.
    if !is_adoptable(conn)? {
        let version = user_version(conn)?;
        return Err(OrbitError::Store(format!(
            "task registry schema version {version} is not a pre-ledger format the registry \
             baseline can adopt"
        )));
    }
    migrate_path_coupled_workspace_bindings(conn)?;
    migrate_allocator_state_v5(conn)?;
    create_v5_tables(conn)?;
    create_action_keys(conn)?;
    create_envelope_stamps(conn)
}

/// Whether a registry without a ledger is a shape the baseline adopts: any
/// `user_version` up to 5, or 6 when its shape is the recognized PR1509 v6.
pub(super) fn is_adoptable(conn: &Connection) -> Result<bool, OrbitError> {
    match user_version(conn)? {
        version if version <= PRE_LEDGER_VERSION => Ok(true),
        PRE_LEDGER_V6 => is_known_additive_v6(conn),
        _ => Ok(false),
    }
}

/// Today's v5 tables, columns and indexes.
fn create_v5_tables(conn: &Connection) -> Result<(), OrbitError> {
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
    .map_err(sqlite_store_error)?;

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
    .map_err(sqlite_store_error)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_terminal
            ON task_bundle_index(workspace_id, terminal_month, task_id)",
        [],
    )
    .map_err(sqlite_store_error)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_complexity
            ON task_bundle_index(workspace_id, complexity, status)",
        [],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}

fn create_action_keys(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_action_keys (
            workspace_id TEXT NOT NULL,
            action_key TEXT NOT NULL,
            task_id TEXT NOT NULL UNIQUE,
            input_digest TEXT NOT NULL,
            PRIMARY KEY(workspace_id, action_key)
        );",
    )
    .map_err(|e| {
        sqlite_error(
            StorageLayer::Store,
            &e,
            format!("ensure task action keys: {e}"),
        )
    })
}

/// The freshness scan's envelope stamps, which pre-ledger binaries created
/// on first record.
fn create_envelope_stamps(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_envelope_stamps (
            task_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            stamp TEXT NOT NULL,
            fingerprint TEXT NOT NULL,
            FOREIGN KEY(task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_task_envelope_stamps_workspace
            ON task_envelope_stamps(workspace_id);",
    )
    .map_err(|e| {
        sqlite_error(
            StorageLayer::Store,
            &e,
            format!("ensure task envelope stamps: {e}"),
        )
    })
}

/// PR1509's v6 only added action keys to v5. Match the actual columns, keys,
/// indexes and schema objects before adopting it. Column order in older task
/// indexes differs after ALTER TABLE; their readers name columns explicitly.
/// Action reservations use positional inserts, so retain their column order.
/// The reference is the pre-ledger v5 shape and stays so.
fn is_known_additive_v6(conn: &Connection) -> Result<bool, OrbitError> {
    let reference = Connection::open_in_memory().map_err(sqlite_store_error)?;
    create_v5_tables(&reference)?;
    create_action_keys(&reference)?;
    // Pre-ledger binaries created envelope stamps on first use, so a marked
    // v6 may or may not carry them: mirror them when present.
    if table_exists(conn, "task_envelope_stamps")? {
        create_envelope_stamps(&reference)?;
    }
    // `schema_meta` is the ledger's own table: the engine creates it in the
    // adoption transaction before this body runs.
    for query in [
        "SELECT name, type, wr, strict FROM pragma_table_list
         WHERE schema = 'main' AND name NOT GLOB 'sqlite_*' AND name <> 'schema_meta'
         ORDER BY name",
        "SELECT s.type, s.name FROM sqlite_schema s
         WHERE s.name NOT GLOB 'sqlite_*' AND s.name <> 'schema_meta' ORDER BY s.type, s.name",
        "SELECT s.name, p.name, p.type, p.[notnull], p.dflt_value, p.pk, p.hidden
         FROM sqlite_schema s, pragma_table_xinfo(s.name) p
         WHERE s.type = 'table' AND s.name NOT GLOB 'sqlite_*' AND s.name <> 'schema_meta'
         ORDER BY s.name, CASE WHEN s.name = 'task_action_keys' THEN p.cid ELSE 0 END, p.name",
        "SELECT s.name, i.name, i.[unique], i.origin, i.partial, x.seqno, x.name, x.desc, x.coll, x.key
         FROM sqlite_schema s, pragma_index_list(s.name) i, pragma_index_xinfo(i.name) x
         WHERE s.type = 'table' AND s.name NOT GLOB 'sqlite_*' AND s.name <> 'schema_meta'
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
    let mut stmt = conn.prepare(query).map_err(sqlite_store_error)?;
    let columns = stmt.column_count();
    stmt.query_map([], |row| {
        (0..columns).map(|column| row.get(column)).collect()
    })
    .map_err(sqlite_store_error)?
    .collect::<Result<_, _>>()
    .map_err(sqlite_store_error)
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
        ",
    )
    .map_err(|error| {
        sqlite_error(
            StorageLayer::Store,
            &error,
            format!("migrate task allocator to v5: {error}"),
        )
    })
}

/// Schema v4 splits stable coordination identity from machine-local checkout
/// paths. Rebuild the parent table under the same final name so existing child
/// foreign keys keep referencing `workspace_bindings` without rewriting any
/// task, index, tag, relation, or allocator rows. Relies on the engine running
/// this body with foreign keys off, so the drop does not cascade.
fn migrate_path_coupled_workspace_bindings(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "workspace_bindings", "repo_root")? {
        return Ok(());
    }

    conn.execute_batch(
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
    )
    .map_err(|error| {
        sqlite_error(
            StorageLayer::Store,
            &error,
            format!("migrate task registry workspace bindings to v4: {error}"),
        )
    })
}

fn user_version(conn: &Connection) -> Result<u32, OrbitError> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sqlite_store_error)?;
    u32::try_from(version)
        .map_err(|e| OrbitError::Store(format!("invalid registry user_version {version}: {e}")))
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, OrbitError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(sqlite_store_error)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, OrbitError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sqlite_store_error)?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)?;
    Ok(columns.iter().any(|candidate| candidate == column))
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> Result<(), OrbitError> {
    if !table_has_column(conn, table, column)? {
        conn.execute(alter_sql, []).map_err(sqlite_store_error)?;
    }
    Ok(())
}
