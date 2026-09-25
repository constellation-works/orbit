//! Opening the registry: directory and permission setup, read-only opens
//! and the schema migrations applied on open.

use std::fs;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use rusqlite::{Connection, OptionalExtension, params};
use tempfile::TempDir;

use super::super::REGISTRY_SCHEMA_VERSION;
use super::super::TaskRegistryStore;
use super::super::schema::registry_user_version;
use super::{registry_path, table_columns};
use crate::fs::path_safety::normalize_path;

fn index_exists(conn: &Connection, index_name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1",
        [index_name],
        |_| Ok(()),
    )
    .optional()
    .expect("query sqlite_master")
    .is_some()
}

#[test]
fn open_creates_registry_parent_and_workspaces_dir() {
    let temp = TempDir::new().expect("tempdir");
    let path = registry_path(&temp);

    let _store = TaskRegistryStore::open(&path).expect("open registry");

    assert!(path.is_file());
    assert!(temp.path().join("tasks").join("workspaces").is_dir());

    let conn = Connection::open(path).expect("open registry sqlite");
    assert_eq!(
        registry_user_version(&conn).expect("read user_version"),
        REGISTRY_SCHEMA_VERSION
    );
}

#[cfg(unix)]
#[test]
fn open_creates_private_registry_state_under_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD_MARKER: &str = "ORBIT_TEST_PRIVATE_REGISTRY_SQLITE";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let status = std::process::Command::new("sh")
            .args(["-c", "umask 000; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().expect("current test executable"))
            .arg("open_creates_private_registry_state_under_permissive_umask")
            .env(CHILD_MARKER, "1")
            .status()
            .expect("run test under permissive umask");
        assert!(status.success(), "permissive-umask child failed");
        return;
    }

    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("private/tasks/index.sqlite");
    let registry = TaskRegistryStore::open(&path).expect("open registry");
    let conn = registry.conn.lock().expect("lock registry");
    conn.execute_batch("BEGIN IMMEDIATE; COMMIT;")
        .expect("touch registry WAL");

    for directory in [
        root.path().join("private"),
        root.path().join("private/tasks"),
        root.path().join("private/tasks/workspaces"),
    ] {
        let mode = fs::metadata(&directory)
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "private directory {}", directory.display());
    }
    for suffix in ["", "-wal", "-shm"] {
        let file = PathBuf::from(format!("{}{suffix}", path.display()));
        let mode = fs::metadata(&file)
            .expect("SQLite file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "private SQLite file {}", file.display());
    }
}

#[cfg(unix)]
#[test]
fn immutable_registry_open_does_not_chmod_or_recreate_adjacent_state() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("tasks/index.sqlite");
    drop(TaskRegistryStore::open(&path).expect("create registry"));
    let workspaces = path.parent().expect("registry parent").join("workspaces");
    fs::remove_dir(&workspaces).expect("remove empty workspace state");
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
        if sidecar.exists() {
            fs::remove_file(sidecar).expect("remove registry sidecar");
        }
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("make registry immutable");

    drop(TaskRegistryStore::open(&path).expect("open immutable registry"));

    assert!(
        !workspaces.exists(),
        "read-only open must not recreate state"
    );
    assert_eq!(
        fs::metadata(&path)
            .expect("registry metadata")
            .permissions()
            .mode()
            & 0o777,
        0o400,
        "read-only database mode must remain unchanged"
    );
    assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());
    assert!(!PathBuf::from(format!("{}-shm", path.display())).exists());
}

#[test]
fn open_migrates_existing_task_index_columns_before_creating_indexes() {
    let temp = TempDir::new().expect("tempdir");
    let path = registry_path(&temp);
    fs::create_dir_all(path.parent().expect("registry parent")).expect("create parent");
    let conn = Connection::open(&path).expect("open sqlite");
    conn.execute_batch(
        "
        CREATE TABLE task_bundle_index (
            task_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            status TEXT NOT NULL,
            priority TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        PRAGMA user_version = 2;
        ",
    )
    .expect("seed old registry shape");
    drop(conn);

    let _store = TaskRegistryStore::open(&path).expect("open migrated registry");

    let conn = Connection::open(&path).expect("reopen migrated sqlite");
    let columns = table_columns(&conn, "task_bundle_index");
    assert!(columns.iter().any(|column| column == "job_run_id"));
    assert!(columns.iter().any(|column| column == "terminal_month"));
    assert!(columns.iter().any(|column| column == "complexity"));
    assert!(index_exists(
        &conn,
        "idx_task_bundle_index_workspace_job_run"
    ));
    assert!(index_exists(
        &conn,
        "idx_task_bundle_index_workspace_terminal"
    ));
    assert!(index_exists(
        &conn,
        "idx_task_bundle_index_workspace_complexity"
    ));
    assert_eq!(
        registry_user_version(&conn).expect("read user_version"),
        REGISTRY_SCHEMA_VERSION
    );
}

#[test]
fn open_migrates_path_coupled_registry_once_without_changing_coordination_state() {
    let temp = TempDir::new().expect("tempdir");
    let path = registry_path(&temp);
    fs::create_dir_all(path.parent().expect("registry parent")).expect("create parent");
    let repo_root = temp.path().join("legacy-repo");
    let orbit_dir = repo_root.join(".orbit");
    let canonical_path = temp
        .path()
        .join("tasks/workspaces/legacy-workspace-aaaaaa/ORB-00041");
    fs::create_dir_all(&canonical_path).expect("create canonical task payload");
    fs::write(canonical_path.join("payload.sentinel"), "preserve-me")
        .expect("write payload sentinel");
    let timestamp = "2026-07-17T00:00:00+00:00";

    let conn = Connection::open(&path).expect("open legacy sqlite");
    conn.execute_batch(
        "
        CREATE TABLE allocator_state (
            authority TEXT PRIMARY KEY,
            next_number INTEGER NOT NULL,
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
            updated_at TEXT NOT NULL
        );
        CREATE TABLE task_bundle_index (
            task_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            status TEXT NOT NULL,
            priority TEXT NOT NULL,
            job_run_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            terminal_month TEXT
        );
        CREATE TABLE task_bundle_tags (
            task_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            tag TEXT NOT NULL,
            PRIMARY KEY(task_id, tag)
        );
        CREATE TABLE task_bundle_relations (
            source_task_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            relation_type TEXT NOT NULL,
            target_task_id TEXT NOT NULL,
            PRIMARY KEY(source_task_id, relation_type, target_task_id)
        );
        PRAGMA user_version = 3;
        ",
    )
    .expect("create legacy schema");
    conn.execute(
        "INSERT INTO allocator_state VALUES ('local', 42, ?1)",
        [timestamp],
    )
    .expect("seed allocator");
    conn.execute(
        "INSERT INTO workspace_bindings VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?6)",
        params![
            "legacy-workspace-aaaaaa",
            "legacy-workspace",
            repo_root.to_string_lossy(),
            orbit_dir.to_string_lossy(),
            "legacy-fingerprint",
            timestamp,
        ],
    )
    .expect("seed workspace");
    conn.execute(
        "INSERT INTO task_bundle_bindings VALUES (?1, ?2, ?3, ?4, ?4)",
        params![
            "ORB-00041",
            "legacy-workspace-aaaaaa",
            normalize_path(&canonical_path).to_string_lossy(),
            timestamp,
        ],
    )
    .expect("seed task binding");
    conn.execute(
        "INSERT INTO task_bundle_index VALUES (?1, ?2, 'done', 'high', NULL, ?3, ?3, '2026-07')",
        params!["ORB-00041", "legacy-workspace-aaaaaa", timestamp],
    )
    .expect("seed task index");
    conn.execute(
        "INSERT INTO task_bundle_tags VALUES (?1, ?2, 'migration')",
        params!["ORB-00041", "legacy-workspace-aaaaaa"],
    )
    .expect("seed tag");
    conn.execute(
        "INSERT INTO task_bundle_relations VALUES (?1, ?2, 'resolves', 'F2026-07-001')",
        params!["ORB-00041", "legacy-workspace-aaaaaa"],
    )
    .expect("seed relation");
    drop(conn);

    let migrated = TaskRegistryStore::open(&path).expect("migrate registry");
    let workspace = migrated
        .find_workspace_binding("legacy-workspace-aaaaaa")
        .expect("find logical workspace")
        .expect("logical workspace exists");
    let checkout = migrated
        .find_workspace_checkout("legacy-workspace-aaaaaa")
        .expect("find checkout")
        .expect("checkout exists");
    let tasks = migrated
        .tasks_for_workspace("legacy-workspace-aaaaaa")
        .expect("task bindings");
    let statuses = migrated
        .global_task_status_index()
        .expect("status projection");
    assert_eq!(workspace.slug, "legacy-workspace");
    assert_eq!(
        workspace.repo_fingerprint.as_deref(),
        Some("legacy-fingerprint")
    );
    assert_eq!(checkout.repo_root, normalize_path(&repo_root));
    assert_eq!(checkout.orbit_dir, normalize_path(&orbit_dir));
    assert_eq!(tasks[0].task_id, "ORB-00041");
    assert_eq!(tasks[0].canonical_path, normalize_path(&canonical_path));
    assert_eq!(statuses.get("ORB-00041"), Some(&TaskStatus::Done));
    assert_eq!(migrated.allocator_next_number().expect("allocator"), 42);
    assert_eq!(
        migrated
            .allocate_task_id("legacy-workspace-aaaaaa")
            .expect("continue migrated allocator"),
        "ORB-00042"
    );
    assert_eq!(
        fs::read_to_string(canonical_path.join("payload.sentinel")).expect("read payload"),
        "preserve-me"
    );
    drop(migrated);

    let conn = Connection::open(&path).expect("inspect migrated sqlite");
    let logical_columns = table_columns(&conn, "workspace_bindings");
    assert!(!logical_columns.iter().any(|column| column == "repo_root"));
    assert!(
        !logical_columns
            .iter()
            .any(|column| column == "workspace_path")
    );
    assert!(!logical_columns.iter().any(|column| column == "orbit_dir"));
    drop(conn);

    let reopened = TaskRegistryStore::open(&path).expect("reopen migrated registry");
    assert_eq!(
        reopened
            .find_workspace_binding("legacy-workspace-aaaaaa")
            .expect("find logical workspace"),
        Some(workspace)
    );
    assert_eq!(
        reopened
            .find_workspace_checkout("legacy-workspace-aaaaaa")
            .expect("find checkout"),
        Some(checkout)
    );
    assert_eq!(
        reopened
            .tasks_for_workspace("legacy-workspace-aaaaaa")
            .expect("task bindings"),
        tasks
    );
    assert_eq!(
        reopened
            .global_task_status_index()
            .expect("status projection"),
        statuses
    );
    assert_eq!(reopened.allocator_next_number().expect("allocator"), 43);
}

#[test]
fn open_rejects_newer_registry_schema_version() {
    let temp = TempDir::new().expect("tempdir");
    let path = registry_path(&temp);
    fs::create_dir_all(path.parent().expect("registry parent")).expect("create parent");
    let conn = Connection::open(&path).expect("open sqlite");
    conn.pragma_update(None, "user_version", i64::from(REGISTRY_SCHEMA_VERSION + 1))
        .expect("set user_version");
    drop(conn);

    let err = match TaskRegistryStore::open(&path) {
        Ok(_) => panic!("opened newer registry schema"),
        Err(err) => err,
    };
    assert!(matches!(err, OrbitError::Store(message) if message.contains("newer than supported")));
}
