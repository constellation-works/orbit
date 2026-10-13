//! The task registry (`tasks/index.sqlite`) on its own versioned ledger,
//! through the public registry open boundary.
//!
//! - Every shipped pre-ledger shape (path-coupled bindings, v5 with or without
//!   action keys and envelope stamps, the recognized v6) adopts the ledger
//!   without losing a row, and keeps `user_version` 5.
//! - An adopted registry still passes the acceptance rules of pre-ledger
//!   binaries, which read only `user_version` and the action-keys column.
//! - A pre-ledger v5 registry with action keys stays readable read-only.
//! - A newer registry is decided from its compatibility record.
//!
//! Every test re-runs itself in a child of this binary with inherited Orbit
//! authority cleared and a disposable `HOME`.
#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_common::test_env;
use orbit_store::contracts::{CompatibilityRecord, MigrationCompatibility};
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};
use rusqlite::{Connection, types::Value};

const PARTITION: &str = "ws_prior";

fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TASK_REGISTRY_LEDGER_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    let output = test_env::run_child_test(&mut command, test, root.path());
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}

fn bind(registry: &TaskRegistryStore, root: &Path, partition: &str) -> Result<(), String> {
    let repo = root.join("repos").join(partition);
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).unwrap();
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(partition.to_string()),
            slug: partition.to_string(),
            repo_root: repo.clone(),
            workspace_path: repo,
            orbit_dir,
            repo_fingerprint: None,
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// A registry at `root` holding one bound workspace, one allocated task id
/// and its action reservation, written by this binary.
fn populated(root: &Path) -> PathBuf {
    let path = task_registry_path(root);
    let registry = TaskRegistryStore::open(&path).unwrap();
    bind(&registry, root, PARTITION).unwrap();
    let task = registry.allocate_task_id(PARTITION).unwrap();
    drop(registry);
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO task_action_keys VALUES (?1, 'delivery-batch', ?2, 'digest')",
            [PARTITION, task.as_str()],
        )
        .unwrap();
    path
}

/// Strip what the ledger added, leaving the registry a pre-ledger binary
/// wrote at `user_version` with or without action keys. Envelope stamps go
/// too: pre-ledger binaries created them only on first record.
fn as_pre_ledger(path: &Path, user_version: u32, action_keys: bool) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("DROP TABLE schema_meta; DROP TABLE task_envelope_stamps;")
        .unwrap();
    if !action_keys {
        conn.execute_batch("DROP TABLE task_action_keys").unwrap();
    }
    conn.pragma_update(None, "user_version", user_version)
        .unwrap();
}

const RETAINED: [&str; 7] = [
    "allocator_state",
    "workspace_bindings",
    "workspace_checkout_bindings",
    "task_bundle_bindings",
    "task_bundle_index",
    "task_bundle_tags",
    "task_bundle_relations",
];

fn rows(conn: &Connection, table: &str) -> Vec<Vec<Value>> {
    let mut statement = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
        .unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns).map(|column| row.get(column)).collect()
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn user_version(conn: &Connection) -> u32 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

fn has_table(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .any(|name| name.unwrap() == column)
}

/// The registry ledger rows, as `(key, value)`.
fn ledger(conn: &Connection) -> Vec<(String, String)> {
    conn.prepare("SELECT key, value FROM schema_meta WHERE key LIKE 'migration.v%' ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn compat_record(conn: &Connection) -> CompatibilityRecord {
    let raw: String = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'migration.compat'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    CompatibilityRecord::decode(&raw).unwrap()
}

/// What a binary from before the registry ledger requires to open a registry
/// without setup (`schema_is_current`), and so to read and write it.
fn passes_pre_ledger_acceptance(conn: &Connection) -> bool {
    user_version(conn) == 5 && has_column(conn, "task_action_keys", "action_key")
}

fn assert_adopted(conn: &Connection) {
    assert_eq!(
        ledger(conn),
        [(
            "migration.v0001".to_string(),
            "registry_baseline".to_string()
        )]
    );
    let record = compat_record(conn);
    assert_eq!(record.version, 1);
    assert!(record.breaking.is_empty(), "{record:?}");
    assert_eq!(user_version(conn), 5, "adoption keeps the pre-ledger fence");
    assert!(
        has_table(conn, "task_envelope_stamps"),
        "the registry baseline creates envelope stamps"
    );
    assert!(passes_pre_ledger_acceptance(conn));
    assert!(
        conn.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
}

#[test]
fn every_pre_ledger_v5_shape_adopts_the_ledger_without_losing_rows() {
    if !isolated("every_pre_ledger_v5_shape_adopts_the_ledger_without_losing_rows") {
        return;
    }
    for (shape, user_version_before, action_keys) in [
        ("v5 before action keys", 5, false),
        ("v5 with action keys", 5, true),
        ("recognized v6", 6, true),
    ] {
        let root = std::env::current_dir()
            .unwrap()
            .join(shape.replace(' ', "-"));
        let path = populated(&root);
        as_pre_ledger(&path, user_version_before, action_keys);
        let retained = |conn: &Connection| -> Vec<_> {
            RETAINED
                .iter()
                .chain(action_keys.then_some(&"task_action_keys"))
                .map(|table| rows(conn, table))
                .collect()
        };
        let before = retained(&Connection::open(&path).unwrap());

        let registry = TaskRegistryStore::open(&path)
            .unwrap_or_else(|error| panic!("{shape} adopts the ledger: {error}"));
        let conn = Connection::open(&path).unwrap();
        assert_eq!(retained(&conn), before, "{shape}: adoption keeps every row");
        assert_adopted(&conn);
        registry.allocate_task_id(PARTITION).unwrap();
        drop(registry);

        // Reopening an adopted registry applies nothing.
        drop(TaskRegistryStore::open(&path).unwrap());
        assert_adopted(&Connection::open(&path).unwrap());
    }
}

#[test]
fn a_path_coupled_registry_adopts_without_cascading_into_its_children() {
    if !isolated("a_path_coupled_registry_adopts_without_cascading_into_its_children") {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let path = task_registry_path(&root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(&path).unwrap();
    // Workspace bindings still hold checkout paths (before v4), and the
    // allocator has no prefix (before v5).
    conn.execute_batch(
        "
        CREATE TABLE allocator_state (
            authority TEXT PRIMARY KEY,
            next_number INTEGER NOT NULL CHECK(next_number >= 0 AND next_number <= 99999),
            updated_at TEXT NOT NULL
        );
        CREATE TABLE workspace_bindings (
            workspace_id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            repo_root TEXT NOT NULL,
            workspace_path TEXT NOT NULL,
            orbit_dir TEXT NOT NULL UNIQUE,
            repo_fingerprint TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE task_bundle_bindings (
            task_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            canonical_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
        );
        CREATE TABLE task_bundle_index (
            task_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            status TEXT NOT NULL,
            priority TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE,
            FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
        );
        INSERT INTO allocator_state VALUES ('local', 42, '2026-01-01T00:00:00Z');
        INSERT INTO workspace_bindings VALUES ('ws_prior', 'prior', '/repo', '/repo',
            '/repo/.orbit', 'fingerprint', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
        INSERT INTO task_bundle_bindings VALUES ('ORB-41', 'ws_prior', '/bundle',
            '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
        INSERT INTO task_bundle_index VALUES ('ORB-41', 'ws_prior', 'done', 'high',
            '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
        PRAGMA user_version = 3;
        ",
    )
    .unwrap();
    drop(conn);

    let registry = TaskRegistryStore::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    assert_adopted(&conn);
    let text = |value: &str| Value::Text(value.to_string());
    assert_eq!(
        rows(&conn, "workspace_checkout_bindings"),
        [vec![
            text("ws_prior"),
            text("/repo"),
            text("/repo"),
            text("/repo/.orbit"),
            text("2026-01-01T00:00:00Z"),
            text("2026-01-01T00:00:00Z"),
        ]]
    );
    assert_eq!(
        rows(&conn, "task_bundle_bindings").len(),
        1,
        "rebuilding the parent table must not cascade into its children"
    );
    assert_eq!(rows(&conn, "task_bundle_index").len(), 1);
    assert_eq!(registry.allocator_next_number().unwrap(), 42);
    assert_eq!(registry.local_task_prefix().unwrap(), "ORB");
}

#[test]
fn an_unrecognized_v6_is_refused_and_left_unadopted() {
    if !isolated("an_unrecognized_v6_is_refused_and_left_unadopted") {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let path = populated(&root);
    as_pre_ledger(&path, 6, true);
    Connection::open(&path)
        .unwrap()
        .execute_batch("ALTER TABLE task_action_keys ADD COLUMN reinterpreted TEXT")
        .unwrap();

    let error = TaskRegistryStore::open(&path).err().unwrap().to_string();
    assert!(
        error.contains("not the known compatible additive format"),
        "{error}"
    );
    let conn = Connection::open(&path).unwrap();
    assert!(!has_table(&conn, "schema_meta"));
    assert_eq!(user_version(&conn), 6);
}

/// Forward compatibility: the current release predates the registry ledger
/// and must keep reading and writing a registry this binary created or
/// adopted. It opens one without setup exactly when its acceptance rules
/// hold, and it never consults `schema_meta`.
#[test]
fn an_adopted_registry_passes_the_pre_ledger_acceptance_rules() {
    if !isolated("an_adopted_registry_passes_the_pre_ledger_acceptance_rules") {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let fresh = populated(&root.join("fresh"));
    let adopted = populated(&root.join("adopted"));
    as_pre_ledger(&adopted, 5, true);
    drop(TaskRegistryStore::open(&adopted).unwrap());

    for path in [fresh, adopted] {
        let conn = Connection::open(&path).unwrap();
        assert_adopted(&conn);
        // Its writes: an action reservation in positional column order, and
        // its first-record envelope stamp setup, which must be a no-op.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_envelope_stamps (
                task_id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                stamp TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                FOREIGN KEY(task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_action_keys VALUES (?1, 'older-binary', 'ORB-900', 'digest')",
            [PARTITION],
        )
        .unwrap();
        drop(conn);
        drop(TaskRegistryStore::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        assert_adopted(&conn);
        assert!(
            rows(&conn, "task_action_keys")
                .iter()
                .any(|row| row[2] == Value::Text("ORB-900".into())),
            "the older binary's reservation survives the next ledger-aware open"
        );
    }
}

#[test]
fn a_pre_ledger_registry_with_action_keys_stays_readable_read_only() {
    if !isolated("a_pre_ledger_registry_with_action_keys_stays_readable_read_only") {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let path = populated(&root.join("readable"));
    as_pre_ledger(&path, 5, true);
    let registry = TaskRegistryStore::open_read_only(&path).unwrap();
    assert!(registry.partition_ids().unwrap().contains(PARTITION));
    assert!(
        registry
            .envelope_stamps_for_workspace(PARTITION)
            .unwrap()
            .is_empty(),
        "a registry that never recorded a stamp has none to read"
    );
    drop(registry);
    let conn = Connection::open(&path).unwrap();
    assert!(
        !has_table(&conn, "schema_meta"),
        "a read-only open adopts nothing"
    );

    let path = populated(&root.join("without-action-keys"));
    as_pre_ledger(&path, 5, false);
    let error = TaskRegistryStore::open_read_only(&path)
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("requires writable additive setup"),
        "{error}"
    );
}

/// Leave the record a newer binary would: ledger v2 of the given class,
/// with the `user_version` fence it keeps for pre-ledger binaries.
fn advance_as_newer_binary(path: &Path, compat: MigrationCompatibility) {
    let record = CompatibilityRecord::for_registry(
        2,
        [
            (1, "registry_baseline", MigrationCompatibility::Additive),
            (2, "newer_registry_change", compat),
        ],
    );
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO schema_meta(key, value, updated_at)
         VALUES ('migration.v0002', 'newer_registry_change', '2026-10-12T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'migration.compat'",
        [record.encode().unwrap()],
    )
    .unwrap();
    let fence = if compat == MigrationCompatibility::Additive {
        5
    } else {
        7
    };
    conn.pragma_update(None, "user_version", fence).unwrap();
}

#[test]
fn a_newer_registry_is_decided_from_its_compatibility_record() {
    if !isolated("a_newer_registry_is_decided_from_its_compatibility_record") {
        return;
    }
    let root = std::env::current_dir().unwrap();

    let additive = root.join("additive");
    let path = populated(&additive);
    advance_as_newer_binary(&path, MigrationCompatibility::Additive);
    let registry = TaskRegistryStore::open(&path).unwrap();
    bind(&registry, &additive, "ws_additive").expect("additive-newer stays writable");
    registry.allocate_task_id("ws_additive").unwrap();
    drop(registry);
    assert_eq!(
        ledger(&Connection::open(&path).unwrap()).len(),
        2,
        "an older binary applies no migration to a newer registry"
    );

    let read_compatible = root.join("read-compatible");
    let path = populated(&read_compatible);
    advance_as_newer_binary(&path, MigrationCompatibility::ReadCompatible);
    let registry = TaskRegistryStore::open(&path).unwrap();
    assert!(registry.partition_ids().unwrap().contains(PARTITION));
    let refused = bind(&registry, &read_compatible, "ws_read_compatible")
        .expect_err("read-compatible-newer opens read-only");
    assert!(refused.contains("readonly"), "{refused}");
    assert!(registry.allocate_task_id(PARTITION).is_err());
    drop(registry);
    let observer = TaskRegistryStore::open_read_only(&path).unwrap();
    assert!(observer.partition_ids().unwrap().contains(PARTITION));

    let breaking = root.join("breaking");
    let path = populated(&breaking);
    advance_as_newer_binary(&path, MigrationCompatibility::Breaking);
    for error in [
        TaskRegistryStore::open(&path).err().unwrap().to_string(),
        TaskRegistryStore::open_read_only(&path)
            .err()
            .unwrap()
            .to_string(),
    ] {
        assert!(
            error.contains("task registry schema version 2 is newer"),
            "{error}"
        );
        assert!(error.contains("newer_registry_change"), "{error}");
    }
}

#[path = "task_registry/relations.rs"]
mod relations;
