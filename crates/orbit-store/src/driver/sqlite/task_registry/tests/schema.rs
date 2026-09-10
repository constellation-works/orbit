//! Upgrade coverage uses the schema shipped before action-key admission existed.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

use orbit_common::OrbitError;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use rusqlite::{Connection, params, types::Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::super::schema::registry_user_version;
use super::super::{REGISTRY_SCHEMA_VERSION, TaskRegistryStore};
use super::{registry_path, table_columns};
use crate::contracts::TaskCreateParams;
use crate::repository::task::TaskV2Store;

const WORKSPACE: &str = "ws_prior";

fn prior_v5(temp: &TempDir) -> PathBuf {
    let path = registry_path(temp);
    fs::create_dir_all(path.parent().expect("registry parent")).expect("create parent");
    let conn = Connection::open(&path).expect("create prior registry");
    conn.execute_batch(include_str!("prior_v5.sql"))
        .expect("create historical schema");
    conn.execute_batch(
        "INSERT INTO allocator_state VALUES ('local', 100005, 'DE', '2026-09-01T00:00:00Z');
         INSERT INTO workspace_bindings VALUES
             ('ws_prior', 'prior', 'repo-fingerprint', '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z');
         INSERT INTO task_bundle_bindings VALUES
             ('DE-100004', 'ws_prior', '/retained/bundle', '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z');
         INSERT INTO task_bundle_index VALUES
             ('DE-100004', 'ws_prior', 'done', 'high', 'prior-run', '2026-09-01T00:00:00Z',
              '2026-09-01T00:00:00Z', '2026-09', 'hard');
         INSERT INTO task_bundle_tags VALUES ('DE-100004', 'ws_prior', 'retained');
         INSERT INTO task_bundle_relations VALUES ('DE-100004', 'ws_prior', 'produces', 'external-result');",
    )
    .expect("seed historical records");
    let root = temp.path().join("repo");
    conn.execute(
        "INSERT INTO workspace_checkout_bindings VALUES
             ('ws_prior', ?1, ?1, ?2, '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z')",
        params![root.to_str(), root.join(".orbit").to_str()],
    )
    .expect("seed checkout");
    assert_eq!(registry_user_version(&conn).expect("version"), 5);
    assert!(table_columns(&conn, "task_action_keys").is_empty());
    path
}

fn retained_rows(conn: &Connection) -> Vec<Vec<Vec<Value>>> {
    [
        "allocator_state",
        "workspace_bindings",
        "workspace_checkout_bindings",
        "task_bundle_bindings",
        "task_bundle_index",
        "task_bundle_tags",
        "task_bundle_relations",
    ]
    .iter()
    .map(|table| {
        let mut statement = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
            .expect("prepare retained rows");
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns).map(|column| row.get(column)).collect()
            })
            .expect("query retained rows")
            .collect::<Result<_, _>>()
            .expect("read retained rows")
    })
    .collect()
}

#[test]
fn prior_v5_upgrade_preserves_records_and_enables_action_reservations() {
    let temp = TempDir::new().expect("tempdir");
    let path = prior_v5(&temp);
    let before = retained_rows(&Connection::open(&path).expect("read old registry"));

    let registry = TaskRegistryStore::open(&path).expect("upgrade v5 registry");
    assert_eq!(
        retained_rows(&Connection::open(&path).expect("read upgraded registry")),
        before
    );
    assert_eq!(
        registry
            .reserve_task_action(WORKSPACE, "delivery-batch", "digest")
            .expect("reserve after upgrade"),
        "DE-100005"
    );
    let conn = Connection::open(&path).expect("read upgraded registry");
    assert_eq!(registry_user_version(&conn).expect("version"), 5);
    assert_eq!(
        table_columns(&conn, "task_action_keys"),
        ["workspace_id", "action_key", "task_id", "input_digest"]
    );
    assert!(
        conn.prepare("PRAGMA foreign_key_check")
            .expect("prepare integrity check")
            .query([])
            .expect("query integrity check")
            .next()
            .expect("integrity row")
            .is_none()
    );
}

fn admission_params() -> TaskCreateParams {
    TaskCreateParams {
        actor: "codex".into(),
        parent_id: None,
        title: "Review retained delivery".into(),
        description: "Examine the retained batch".into(),
        acceptance_criteria: vec!["Record examination evidence".into()],
        dependencies: vec![],
        relations: vec![],
        tags: vec![],
        required_tools: vec![],
        plan: String::new(),
        execution_summary: String::new(),
        context_files: vec![],
        workspace_path: None,
        repo_root: None,
        created_by: Some("codex".into()),
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::Backlog,
        priority: TaskPriority::High,
        complexity: None,
        task_type: TaskType::Chore,
        external_refs: vec![],
        source_task_id: None,
        crew: None,
        orchestrator: None,
        comments: vec![],
    }
}

#[test]
fn upgraded_registry_replays_admission_after_reservation_and_bundle_creation() {
    let temp = TempDir::new().expect("tempdir");
    let path = prior_v5(&temp);
    let params = admission_params();
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&params).expect("serialize input"))
    );
    let key = "automation:retained-batch:1";

    // Simulate process exit after reservation, before any bundle is official.
    let registry = TaskRegistryStore::open(&path).expect("upgrade registry");
    let reserved = registry
        .reserve_task_action(WORKSPACE, key, &digest)
        .expect("reserve action");
    assert!(
        registry
            .find_task_binding(&reserved)
            .expect("binding")
            .is_none()
    );
    drop(registry);

    let registry = TaskRegistryStore::open(&path).expect("reopen for admission");
    let tasks = TaskV2Store::new(registry.clone(), WORKSPACE.into());
    let created = tasks
        .create_task_with_key(params.clone(), Some(key))
        .expect("recover reserved action");
    assert_eq!(created.id, reserved);
    drop(tasks);
    drop(registry);

    // Simulate lost acknowledgement after bundle creation. Reopen and admit
    // repeatedly without allocating another ID or rewriting creation evidence.
    for _ in 0..2 {
        let registry = TaskRegistryStore::open(&path).expect("reopen after admission");
        let tasks = TaskV2Store::new(registry.clone(), WORKSPACE.into());
        let replay = tasks
            .create_task_with_key(params.clone(), Some(key))
            .expect("replay action");
        assert_eq!(replay.id, reserved);
        assert_eq!(replay.created_at, created.created_at);
        assert_eq!(replay.status, TaskStatus::Backlog);
        assert_eq!(registry.allocator_next_number().expect("allocator"), 100006);

        let mut changed = params.clone();
        changed.title = "Different input".into();
        assert!(matches!(
            tasks.create_task_with_key(changed, Some(key)),
            Err(OrbitError::InvalidInput(message)) if message.contains("action key input changed")
        ));
    }
    let conn = Connection::open(&path).expect("inspect admission");
    let mappings: i64 = conn
        .query_row("SELECT COUNT(*) FROM task_action_keys", [], |r| r.get(0))
        .expect("mapping count");
    let tasks: i64 = conn
        .query_row("SELECT COUNT(*) FROM task_bundle_bindings", [], |r| {
            r.get(0)
        })
        .expect("task count");
    assert_eq!(mappings, 1);
    assert_eq!(tasks, 2, "one retained task plus one admitted task");
}

#[test]
fn v5_with_existing_action_mapping_keeps_reserved_identity() {
    let temp = TempDir::new().expect("tempdir");
    let path = prior_v5(&temp);
    // The defective release also created fresh v5 registries WITH this table.
    // Preserve their mappings when upgrading, even before bundle registration.
    let conn = Connection::open(&path).expect("open fresh-v5 fixture");
    conn.execute_batch(
        "CREATE TABLE task_action_keys (
            workspace_id TEXT NOT NULL, action_key TEXT NOT NULL, task_id TEXT NOT NULL UNIQUE,
            input_digest TEXT NOT NULL, PRIMARY KEY(workspace_id, action_key));
         INSERT INTO task_action_keys VALUES ('ws_prior', 'retained', 'DE-100005', 'digest');
         UPDATE allocator_state SET next_number = 100006 WHERE authority = 'local';",
    )
    .expect("seed defective fresh-v5 reservation");
    let before = retained_rows(&conn);
    drop(conn);
    let registry = TaskRegistryStore::open(&path).expect("upgrade fresh-v5 fixture");
    assert_eq!(
        registry
            .reserve_task_action(WORKSPACE, "retained", "digest")
            .expect("replay mapping"),
        "DE-100005"
    );
    let conn = Connection::open(&path).expect("read upgraded fixture");
    assert_eq!(retained_rows(&conn), before);
    assert_eq!(registry_user_version(&conn).expect("version"), 5);
}

#[cfg(unix)]
#[test]
fn readonly_prior_v5_requires_upgrade_and_current_registry_remains_readable() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let path = prior_v5(&temp);
    let before = fs::read(&path).expect("snapshot prior database");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("make read-only");
    let error = match TaskRegistryStore::open(&path) {
        Ok(_) => panic!("read-only v5 registry needs migration"),
        Err(error) => error,
    };
    assert!(error.is_readonly_or_access_failure(), "{error}");
    assert_eq!(fs::read(&path).expect("read failed upgrade"), before);
    assert!(!path.parent().expect("parent").join("workspaces").exists());

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("allow upgrade");
    drop(TaskRegistryStore::open(&path).expect("retry supported upgrade"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400))
        .expect("make upgraded registry read-only");
    let registry = TaskRegistryStore::open(&path).expect("read current registry");
    assert_eq!(
        registry.allocator_next_number().expect("read allocator"),
        100005
    );
    assert!(
        registry
            .find_task_binding("DE-100004")
            .expect("read retained task")
            .is_some()
    );
    let error = registry
        .reserve_task_action(WORKSPACE, "new", "digest")
        .expect_err("readonly cannot admit");
    assert!(error.is_readonly_or_access_failure(), "{error}");
}

#[test]
fn newer_registry_is_rejected_without_migrating_retained_records() {
    let temp = TempDir::new().expect("tempdir");
    let path = prior_v5(&temp);
    let conn = Connection::open(&path).expect("open fixture");
    let newer = 7;
    conn.pragma_update(None, "user_version", newer)
        .expect("set future version");
    let before = retained_rows(&conn);
    drop(conn);

    let error = match TaskRegistryStore::open(&path) {
        Ok(_) => panic!("opened unsupported registry"),
        Err(error) => error,
    };
    assert!(error.to_string().contains(&format!(
        "schema version {newer} is newer than supported version {REGISTRY_SCHEMA_VERSION}"
    )));
    let conn = Connection::open(&path).expect("inspect rejected fixture");
    assert_eq!(retained_rows(&conn), before);
    assert_eq!(registry_user_version(&conn).expect("version"), newer);
    assert!(table_columns(&conn, "task_action_keys").is_empty());
}

// Frozen DDL added by PR1509. Do not derive the historical v6 fixture from
// the repaired opener, which must prove it can recover an already migrated DB.
const V6_ACTION_KEYS: &str = "CREATE TABLE task_action_keys (
    workspace_id TEXT NOT NULL,
    action_key TEXT NOT NULL,
    task_id TEXT NOT NULL UNIQUE,
    input_digest TEXT NOT NULL,
    PRIMARY KEY(workspace_id, action_key)
);";

fn known_v6(temp: &TempDir) -> PathBuf {
    let path = prior_v5(temp);
    let conn = Connection::open(&path).expect("open prior registry");
    conn.execute_batch(V6_ACTION_KEYS)
        .expect("apply shipped v6 DDL");
    conn.execute_batch("PRAGMA user_version = 6;")
        .expect("mark shipped version");
    path
}

#[test]
fn known_v6_recovers_preserving_reservations_and_bundle_replay() {
    let temp = TempDir::new().expect("tempdir");
    let path = known_v6(&temp);
    let params = admission_params();
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&params).expect("input"))
    );
    let conn = Connection::open(&path).expect("open v6");
    conn.execute(
        "INSERT INTO task_action_keys VALUES (?1, 'retained', 'DE-100005', ?2)",
        params![WORKSPACE, digest],
    )
    .expect("seed v6 reservation");
    conn.execute_batch("UPDATE allocator_state SET next_number = 100006;")
        .expect("retain allocation");
    let before = retained_rows(&conn);
    drop(conn);

    let registry = TaskRegistryStore::open(&path).expect("recover already migrated v6");
    let conn = Connection::open(&path).expect("inspect recovery");
    assert_eq!(registry_user_version(&conn).expect("version"), 5);
    assert_eq!(retained_rows(&conn), before);
    assert_eq!(
        registry
            .reserve_task_action(WORKSPACE, "retained", &digest)
            .expect("replay"),
        "DE-100005"
    );
    let tasks = TaskV2Store::new(registry.clone(), WORKSPACE.into());
    let created = tasks
        .create_task_with_key(params.clone(), Some("retained"))
        .expect("recover bundle");
    assert_eq!(created.id, "DE-100005");
    drop(tasks);
    drop(registry);

    let registry = TaskRegistryStore::open(&path).expect("repeat recovery");
    let tasks = TaskV2Store::new(registry.clone(), WORKSPACE.into());
    assert_eq!(
        tasks
            .create_task_with_key(params, Some("retained"))
            .expect("replay bundle")
            .created_at,
        created.created_at
    );
    assert_eq!(registry.allocator_next_number().expect("allocator"), 100006);
    assert!(
        conn.execute(
            "INSERT INTO task_action_keys VALUES ('ws_prior', 'different', 'DE-100005', 'digest')",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO task_action_keys VALUES ('ws_prior', 'retained', 'DE-100006', 'digest')",
            []
        )
        .is_err()
    );
}

#[test]
fn concurrent_additive_setup_and_v6_recovery_reserve_one_identity() {
    for version in [5, 6] {
        let temp = TempDir::new().expect("tempdir");
        let path = if version == 5 {
            prior_v5(&temp)
        } else {
            known_v6(&temp)
        };
        // WAL setup is not under test here; synchronize contenders after the
        // fixture is in the same journal mode as an ordinary existing registry.
        let conn = Connection::open(&path).expect("open fixture");
        conn.pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let registry = TaskRegistryStore::open(&path).expect("concurrent open");
                    registry
                        .reserve_task_action(WORKSPACE, "concurrent", "digest")
                        .expect("reserve")
                })
            })
            .collect();
        let ids: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("join"))
            .collect();
        assert!(ids.iter().all(|id| id == &ids[0]));
        assert_eq!(registry_user_version(&conn).expect("version"), 5);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM task_action_keys", [], |r| r
                .get::<_, i64>(0))
                .expect("count"),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM task_bundle_bindings", [], |r| r
                .get::<_, i64>(0))
                .expect("count"),
            1
        );
        let registry = TaskRegistryStore::open(&path).expect("reopen");
        let next = registry.allocator_next_number().expect("allocator");
        assert_eq!(
            registry
                .reserve_task_action(WORKSPACE, "concurrent", "digest")
                .expect("replay"),
            ids[0]
        );
        assert_eq!(registry.allocator_next_number().expect("allocator"), next);
    }
}

#[test]
fn interrupted_additive_setup_and_recovery_can_be_retried() {
    for version in [5, 6] {
        let temp = TempDir::new().expect("tempdir");
        let path = if version == 5 {
            prior_v5(&temp)
        } else {
            known_v6(&temp)
        };
        let conn = Connection::open(&path).expect("open fixture");
        let before = retained_rows(&conn);
        conn.execute_batch("BEGIN IMMEDIATE;")
            .expect("begin interrupted setup");
        if version == 5 {
            conn.execute_batch(V6_ACTION_KEYS)
                .expect("uncommitted additive table");
        } else {
            conn.execute_batch("PRAGMA user_version = 5;")
                .expect("uncommitted recovery");
        }
        drop(conn); // Connection close rolls back an interrupted transaction.
        let conn = Connection::open(&path).expect("inspect rollback");
        assert_eq!(registry_user_version(&conn).expect("version"), version);
        assert_eq!(retained_rows(&conn), before);
        drop(TaskRegistryStore::open(&path).expect("retry after interruption"));
        assert_eq!(registry_user_version(&conn).expect("version"), 5);
        assert_eq!(retained_rows(&conn), before);
    }
}

#[test]
fn unrecognized_v6_shapes_are_rejected_without_lowering_version() {
    for mutation in [
        "DROP TABLE task_action_keys;",
        "ALTER TABLE task_action_keys ADD COLUMN future TEXT;",
        "DROP TABLE task_action_keys; CREATE TABLE task_action_keys (workspace_id TEXT NOT NULL, action_key TEXT NOT NULL, task_id TEXT NOT NULL, input_digest TEXT NOT NULL, PRIMARY KEY(workspace_id, action_key));",
        "CREATE TABLE future_format (payload BLOB);",
        "CREATE TRIGGER future_trigger AFTER INSERT ON task_action_keys BEGIN DELETE FROM task_action_keys; END;",
        "ALTER TABLE allocator_state RENAME COLUMN task_prefix TO incompatible_prefix;",
    ] {
        let temp = TempDir::new().expect("tempdir");
        let path = known_v6(&temp);
        let conn = Connection::open(&path).expect("open v6");
        conn.execute_batch(mutation).expect("alter fixture format");
        let before = retained_rows(&conn);
        let error = match TaskRegistryStore::open(&path) {
            Ok(_) => panic!("accepted incompatible v6: {mutation}"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("known compatible additive format"),
            "{error}"
        );
        assert!(error.contains(path.to_str().expect("path")), "{error}");
        assert!(error.contains("command -v orbit"), "{error}");
        assert_eq!(registry_user_version(&conn).expect("version"), 6);
        assert_eq!(retained_rows(&conn), before);
    }
}

#[test]
fn known_v6_with_legacy_altered_column_order_recovers() {
    let temp = TempDir::new().expect("tempdir");
    let path = known_v6(&temp);
    let conn = Connection::open(&path).expect("open fixture");
    // Before job_run_id was added, timestamps preceded it. Reproduce the
    // shipped ALTER TABLE path, which also lacked the newer foreign keys.
    conn.execute_batch(
        "DROP TABLE task_bundle_index;
        CREATE TABLE task_bundle_index (
            task_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
            status TEXT NOT NULL, priority TEXT NOT NULL,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
        ALTER TABLE task_bundle_index ADD COLUMN job_run_id TEXT;
        ALTER TABLE task_bundle_index ADD COLUMN terminal_month TEXT;
        ALTER TABLE task_bundle_index ADD COLUMN complexity TEXT;",
    )
    .expect("legacy index table");
    // Restore exactly the indexes shipped on this table, independently of the
    // repaired production setup.
    for statement in include_str!("prior_v5.sql").split(';') {
        if statement.trim_start().starts_with("CREATE INDEX")
            && statement.contains("ON task_bundle_index(")
        {
            conn.execute_batch(statement).expect("historical index");
        }
    }
    let before = retained_rows(&conn);
    drop(TaskRegistryStore::open(&path).expect("recover legacy v6"));
    assert_eq!(registry_user_version(&conn).expect("version"), 5);
    assert_eq!(retained_rows(&conn), before);
}

#[cfg(unix)]
#[test]
fn readonly_known_v6_waits_for_writable_recovery() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let path = known_v6(&temp);
    let before = fs::read(&path).expect("snapshot");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("read-only");
    let error = match TaskRegistryStore::open(&path) {
        Ok(_) => panic!("readonly v6 cannot persist recovery"),
        Err(error) => error,
    };
    assert!(error.is_readonly_or_access_failure(), "{error}");
    assert!(error.to_string().contains("requires writable"), "{error}");
    assert_eq!(fs::read(&path).expect("unchanged DB"), before);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("writable");
    drop(TaskRegistryStore::open(&path).expect("recover v6"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("read-only again");
    let registry = TaskRegistryStore::open(&path).expect("read recovered registry");
    assert!(
        registry
            .find_task_binding("DE-100004")
            .expect("read task")
            .is_some()
    );
}
