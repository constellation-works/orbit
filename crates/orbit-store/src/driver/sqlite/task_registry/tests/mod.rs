// Content moved from tests.rs per ORB-00231

mod schema;

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_types::task::{
    ORB_TASK_ID_MAX, TaskComplexity, TaskEnvelopeV2, TaskPriority, TaskRelation, TaskRelationType,
    TaskStatus, TaskType, UNSET_BUCKET,
};
use rusqlite::{Connection, OptionalExtension, params};
use tempfile::TempDir;

use super::REGISTRY_SCHEMA_VERSION;
use super::schema::registry_user_version;
use super::util::now_string;
use super::{
    BindWorkspaceParams, RegisterWorkspaceParams, TaskIndexFilter, TaskRegistryStore,
    WorkspaceCheckoutBinding, task_registry_path,
};
use crate::contracts::WorkspaceConfig;
use crate::fs::path_safety::normalize_path;
use crate::{
    read_workspace_config, read_workspace_config_optional, workspace_config_path,
    workspace_id_for_orbit_dir, write_workspace_config,
};

fn registry_path(temp: &TempDir) -> PathBuf {
    task_registry_path(temp.path())
}

fn store(temp: &TempDir) -> TaskRegistryStore {
    TaskRegistryStore::open(&registry_path(temp)).expect("open registry")
}

fn bind(store: &TaskRegistryStore, root: &Path) -> WorkspaceCheckoutBinding {
    let orbit_dir = root.join(".orbit");
    fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    store
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some("orbit-test-123456".into()),
            slug: "Orbit Test".into(),
            repo_root: root.to_path_buf(),
            workspace_path: root.to_path_buf(),
            orbit_dir,
            repo_fingerprint: None,
        })
        .expect("bind workspace")
}

fn create_canonical_bundle(
    store: &TaskRegistryStore,
    workspace: &WorkspaceCheckoutBinding,
    task_id: &str,
) -> PathBuf {
    let bundle_dir = store
        .canonical_task_bundle_path(&workspace.workspace_id, task_id)
        .expect("canonical bundle path");
    fs::create_dir_all(&bundle_dir).expect("create bundle");
    bundle_dir
}

fn envelope(
    task_id: &str,
    status: TaskStatus,
    tags: Vec<String>,
    relations: Vec<TaskRelation>,
) -> TaskEnvelopeV2 {
    let now = Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap();
    TaskEnvelopeV2 {
        schema_version: orbit_types::task::TASK_ARTIFACT_SCHEMA_VERSION,
        id: task_id.to_string(),
        title: format!("Task {task_id}"),
        status,
        task_type: TaskType::Feature,
        priority: TaskPriority::High,
        complexity: None,
        pr_status: None,
        job_run_id: None,
        crew: None,
        orchestrator: None,
        relations,
        tags,
        required_tools: Vec::new(),
        context_files: Vec::new(),
        external_refs: Vec::new(),
        created_by: Some("codex:gpt-5.5".to_string()),
        planned_by: None,
        implemented_by: None,
        created_at: now,
        updated_at: now,
    }
}

fn table_columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table info");
    stmt.query_map([], |row| row.get::<_, String>(1))
        .expect("query table info")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect columns")
}

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
fn allocator_returns_monotonic_orb_ids() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    assert_eq!(
        store.allocate_task_id(&workspace.workspace_id).expect("id"),
        "ORB-00000"
    );
    assert_eq!(
        store.allocate_task_id(&workspace.workspace_id).expect("id"),
        "ORB-00001"
    );
}

#[test]
fn allocator_uses_host_prefix_and_expands_past_five_digits() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store.set_task_prefix("DE").expect("set host prefix");
    store
        .seed_allocator_start(99_999)
        .expect("seed near width boundary");
    let workspace = bind(&store, temp.path());

    assert_eq!(
        store.allocate_task_id(&workspace.workspace_id).expect("id"),
        "DE-99999"
    );
    assert_eq!(
        store.allocate_task_id(&workspace.workspace_id).expect("id"),
        "DE-100000"
    );
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

#[test]
fn allocator_is_global_across_workspaces() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = bind(&store, temp.path());
    let second_root = temp.path().join("second");
    fs::create_dir_all(second_root.join(".orbit")).expect("create second orbit dir");
    let second = store
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some("second-abcdef".into()),
            slug: "Second".into(),
            repo_root: second_root.clone(),
            workspace_path: second_root.clone(),
            orbit_dir: second_root.join(".orbit"),
            repo_fingerprint: None,
        })
        .expect("bind second workspace");

    assert_eq!(
        store
            .allocate_task_id(&first.workspace_id)
            .expect("first id"),
        "ORB-00000"
    );
    assert_eq!(
        store
            .allocate_task_id(&second.workspace_id)
            .expect("second id"),
        "ORB-00001"
    );
}

#[test]
fn checkoutless_workspaces_coordinate_cross_workspace_relations_without_paths() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = store
        .register_workspace(RegisterWorkspaceParams {
            workspace_id: "logical-first-aaaaaa".into(),
            slug: "Logical First".into(),
            repo_fingerprint: Some("first-fingerprint".into()),
        })
        .expect("register first logical workspace");
    let second = store
        .register_workspace(RegisterWorkspaceParams {
            workspace_id: "logical-second-bbbbbb".into(),
            slug: "Logical Second".into(),
            repo_fingerprint: None,
        })
        .expect("register second logical workspace");

    assert!(
        store
            .find_workspace_checkout(&first.workspace_id)
            .expect("find first checkout")
            .is_none()
    );
    assert!(
        store
            .find_workspace_checkout(&second.workspace_id)
            .expect("find second checkout")
            .is_none()
    );

    let target_id = store
        .allocate_task_id(&second.workspace_id)
        .expect("allocate target");
    let source_id = store
        .allocate_task_id(&first.workspace_id)
        .expect("allocate source");
    assert_eq!(
        (target_id.as_str(), source_id.as_str()),
        ("ORB-00000", "ORB-00001")
    );

    for (workspace_id, task_id) in [
        (second.workspace_id.as_str(), target_id.as_str()),
        (first.workspace_id.as_str(), source_id.as_str()),
    ] {
        let path = store
            .canonical_task_bundle_path(workspace_id, task_id)
            .expect("canonical coordination path");
        fs::create_dir_all(&path).expect("create canonical bundle");
        store
            .register_task_bundle(task_id, workspace_id, &path)
            .expect("register canonical bundle");
    }
    store
        .replace_task_index(
            &second.workspace_id,
            &envelope(&target_id, TaskStatus::Done, Vec::new(), Vec::new()),
        )
        .expect("index completed target");
    store
        .replace_task_index(
            &first.workspace_id,
            &envelope(
                &source_id,
                TaskStatus::Backlog,
                Vec::new(),
                vec![
                    TaskRelation {
                        relation_type: TaskRelationType::BlockedBy,
                        target: target_id.clone(),
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: target_id.clone(),
                    },
                ],
            ),
        )
        .expect("index cross-workspace relations");

    assert_eq!(
        store
            .global_task_status_index()
            .expect("global statuses")
            .get(&target_id),
        Some(&TaskStatus::Done)
    );
    assert_eq!(
        store
            .indexed_relation_targets(&first.workspace_id, &source_id, TaskRelationType::BlockedBy,)
            .expect("cross-workspace dependency"),
        vec![target_id.clone()]
    );
    assert_eq!(
        store
            .indexed_task_count_for_workspace(&first.workspace_id)
            .expect("first count"),
        1
    );
    assert_eq!(
        store
            .indexed_task_count_for_workspace(&second.workspace_id)
            .expect("second count"),
        1
    );

    let before_allocator = store.allocator_next_number().expect("allocator before");
    let missing_target = "ORB-09999";
    let error = store
        .validate_new_task_relation_targets(
            &first.workspace_id,
            &[TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target: missing_target.into(),
            }],
        )
        .expect_err("missing global target");
    assert!(error.to_string().contains(missing_target));
    assert!(error.to_string().contains(&first.workspace_id));
    assert_eq!(
        store.allocator_next_number().expect("allocator after"),
        before_allocator,
        "missing target preflight must not consume an ID"
    );

    let error = store
        .replace_task_index(
            &first.workspace_id,
            &envelope(
                &source_id,
                TaskStatus::Review,
                Vec::new(),
                vec![TaskRelation {
                    relation_type: TaskRelationType::RelatedTo,
                    target: missing_target.into(),
                }],
            ),
        )
        .expect_err("missing relation target");
    assert!(error.to_string().contains(missing_target));
    assert!(error.to_string().contains(&first.workspace_id));
    assert_eq!(
        store
            .global_task_status_index()
            .expect("status after rejected update")
            .get(&source_id),
        Some(&TaskStatus::Backlog),
        "rejected relation update must be atomic"
    );
    assert_eq!(
        store
            .indexed_relation_targets(&first.workspace_id, &source_id, TaskRelationType::BlockedBy,)
            .expect("relation after rejected update"),
        vec![target_id]
    );

    let foreign_target = "DK-00042";
    store
        .validate_new_task_relation_targets(
            &first.workspace_id,
            &[TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target: foreign_target.into(),
            }],
        )
        .expect("foreign-prefix target cannot be verified locally");
    store
        .replace_task_index(
            &first.workspace_id,
            &envelope(
                &source_id,
                TaskStatus::Review,
                Vec::new(),
                vec![
                    TaskRelation {
                        relation_type: TaskRelationType::BlockedBy,
                        target: foreign_target.into(),
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: foreign_target.into(),
                    },
                ],
            ),
        )
        .expect("index foreign-prefix relations");
    assert_eq!(
        store
            .indexed_relation_targets(&first.workspace_id, &source_id, TaskRelationType::RelatedTo)
            .expect("foreign relation target"),
        vec![foreign_target.to_string()]
    );
    assert!(
        store
            .dangling_relation_targets(Some(&first.workspace_id))
            .expect("audit foreign target")
            .is_empty(),
        "allowed foreign references are not locally dangling"
    );
}

#[test]
fn dangling_relation_targets_reports_only_grandfathered_orb_targets() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    // A resolvable target and the source both exist in the registry.
    let target_id = store
        .allocate_task_id(&workspace.workspace_id)
        .expect("allocate target");
    let source_id = store
        .allocate_task_id(&workspace.workspace_id)
        .expect("allocate source");
    for task_id in [target_id.as_str(), source_id.as_str()] {
        let path = store
            .canonical_task_bundle_path(&workspace.workspace_id, task_id)
            .expect("canonical bundle path");
        fs::create_dir_all(&path).expect("create canonical bundle");
        store
            .register_task_bundle(task_id, &workspace.workspace_id, &path)
            .expect("register bundle");
    }
    store
        .replace_task_index(
            &workspace.workspace_id,
            &envelope(&target_id, TaskStatus::Done, Vec::new(), Vec::new()),
        )
        .expect("index target");
    // Source carries a resolvable ORB relation plus a non-ORB `resolves`
    // target; the audit must flag neither.
    store
        .replace_task_index(
            &workspace.workspace_id,
            &envelope(
                &source_id,
                TaskStatus::Backlog,
                Vec::new(),
                vec![
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: target_id.clone(),
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::Resolves,
                        target: "F2026-05-001".into(),
                    },
                ],
            ),
        )
        .expect("index source relations");

    assert!(
        store
            .dangling_relation_targets(None)
            .expect("audit clean")
            .is_empty(),
        "resolvable + non-ORB targets must not be flagged"
    );

    // Grandfather a dangling ORB target. The public API forbids adding one (the
    // validator rejects it at index time), so these only exist as legacy rows —
    // inject one directly to reproduce that state.
    let missing_target = "ORB-09999";
    {
        let conn = store.conn.lock().expect("lock registry");
        conn.execute(
            "INSERT INTO task_bundle_relations(
                source_task_id, workspace_id, relation_type, target_task_id
            ) VALUES (?1, ?2, ?3, ?4)",
            params![
                source_id,
                workspace.workspace_id,
                "related_to",
                missing_target
            ],
        )
        .expect("seed grandfathered relation");
    }

    let dangling = store
        .dangling_relation_targets(None)
        .expect("audit dangling");
    assert_eq!(
        dangling.len(),
        1,
        "only the missing ORB target dangles: {dangling:?}"
    );
    assert_eq!(dangling[0].source_task_id, source_id);
    assert_eq!(dangling[0].target_task_id, missing_target);
    assert_eq!(dangling[0].relation_type, "related_to");
    assert_eq!(dangling[0].workspace_id, workspace.workspace_id);
    assert_eq!(
        store
            .dangling_relation_targets(Some(&workspace.workspace_id))
            .expect("scoped audit")
            .len(),
        1
    );

    // A second workspace with its own grandfathered target: the unscoped audit
    // spans both, each scoped audit sees exactly its own.
    let repo_two = temp.path().join("repo-two");
    let orbit_two = repo_two.join(".orbit");
    fs::create_dir_all(&orbit_two).expect("create second orbit dir");
    let workspace_two = store
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some("orbit-two-654321".into()),
            slug: "Orbit Two".into(),
            repo_root: repo_two.clone(),
            workspace_path: repo_two.clone(),
            orbit_dir: orbit_two,
            repo_fingerprint: None,
        })
        .expect("bind second workspace");
    let source_two = store
        .allocate_task_id(&workspace_two.workspace_id)
        .expect("allocate second source");
    let path_two = store
        .canonical_task_bundle_path(&workspace_two.workspace_id, &source_two)
        .expect("second canonical path");
    fs::create_dir_all(&path_two).expect("create second bundle");
    store
        .register_task_bundle(&source_two, &workspace_two.workspace_id, &path_two)
        .expect("register second source");
    store
        .replace_task_index(
            &workspace_two.workspace_id,
            &envelope(&source_two, TaskStatus::Backlog, Vec::new(), Vec::new()),
        )
        .expect("index second source");
    {
        let conn = store.conn.lock().expect("lock registry");
        conn.execute(
            "INSERT INTO task_bundle_relations(
                source_task_id, workspace_id, relation_type, target_task_id
            ) VALUES (?1, ?2, ?3, ?4)",
            params![
                source_two,
                workspace_two.workspace_id,
                "blocked_by",
                "ORB-08888"
            ],
        )
        .expect("seed second grandfathered relation");
    }

    assert_eq!(
        store
            .dangling_relation_targets(None)
            .expect("audit both")
            .len(),
        2,
        "unscoped audit spans workspaces"
    );
    assert_eq!(
        store
            .dangling_relation_targets(Some(&workspace_two.workspace_id))
            .expect("scoped to second")
            .len(),
        1
    );
    assert_eq!(
        store
            .dangling_relation_targets(Some(&workspace.workspace_id))
            .expect("scoped to first")
            .len(),
        1
    );
}

#[test]
fn allocator_reports_exhaustion() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    {
        let conn = store.conn.lock().expect("lock registry");
        conn.execute(
            "UPDATE allocator_state SET next_number = ?1, updated_at = ?2
             WHERE authority = 'local'",
            params![i64::from(ORB_TASK_ID_MAX) + 1, now_string()],
        )
        .expect("force allocator exhaustion");
    }

    assert!(matches!(
        store.allocate_task_id(&workspace.workspace_id),
        Err(OrbitError::Store(message)) if message.contains("exhausted")
    ));
}

#[test]
fn bind_workspace_is_idempotent_for_orbit_dir() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = bind(&store, temp.path());
    let second = store
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some(first.workspace_id.clone()),
            slug: "Changed".into(),
            repo_root: temp.path().join("."),
            workspace_path: temp.path().join("."),
            orbit_dir: temp.path().join(".orbit").join("..").join(".orbit"),
            repo_fingerprint: Some("changed".into()),
        })
        .expect("idempotent bind");

    assert_eq!(first.workspace_id, second.workspace_id);
    assert_eq!(first.workspace_id, second.workspace_id);
}

#[test]
fn publication_fingerprint_is_adopted_once_and_then_fails_closed() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    assert_eq!(
        store
            .find_workspace_binding(&workspace.workspace_id)
            .expect("find workspace")
            .expect("workspace")
            .repo_fingerprint,
        None
    );

    let recorded = store
        .record_workspace_repo_fingerprint(&workspace.workspace_id, "ssh://source.test/orbit.git")
        .expect("record fingerprint");
    assert_eq!(
        recorded.repo_fingerprint.as_deref(),
        Some("ssh://source.test/orbit.git")
    );
    store
        .record_workspace_repo_fingerprint(&workspace.workspace_id, "ssh://source.test/orbit.git")
        .expect("idempotent recording");

    let error = store
        .record_workspace_repo_fingerprint(&workspace.workspace_id, "ssh://source.test/other.git")
        .expect_err("mismatch must fail");
    assert!(error.to_string().contains("different source-repository"));
}

#[test]
fn bind_workspace_rebinds_same_checkout_under_a_new_orbit_dir() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = bind(&store, temp.path());

    // A later process for the same logical checkout brings its own ephemeral
    // orbit dir (ORB-10507). The bind must move the existing checkout binding
    // instead of failing with "already has a local checkout".
    let ephemeral_orbit_dir = temp.path().join(".orbit-ephemeral");
    fs::create_dir_all(&ephemeral_orbit_dir).expect("create ephemeral orbit dir");
    let second = store
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some(first.workspace_id.clone()),
            slug: "Orbit Test".into(),
            repo_root: temp.path().to_path_buf(),
            workspace_path: temp.path().to_path_buf(),
            orbit_dir: ephemeral_orbit_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("rebind under a new orbit dir");

    assert_eq!(second.workspace_id, first.workspace_id);
    assert_eq!(second.orbit_dir, normalize_path(&ephemeral_orbit_dir));
    assert_eq!(
        store
            .find_workspace_checkout(&first.workspace_id)
            .expect("find checkout")
            .expect("checkout exists")
            .orbit_dir,
        normalize_path(&ephemeral_orbit_dir),
        "the moved binding is the workspace's only checkout row"
    );
}

#[test]
fn bind_workspace_reuses_derived_id_for_same_paths_under_a_new_orbit_dir() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let repo_root = temp.path().join("repo");
    let first_orbit_dir = repo_root.join(".orbit");
    fs::create_dir_all(&first_orbit_dir).expect("create orbit dir");
    let derive = |orbit_dir: PathBuf| BindWorkspaceParams {
        workspace_id: None,
        slug: "Orbit Test".into(),
        repo_root: repo_root.clone(),
        workspace_path: repo_root.clone(),
        orbit_dir,
        repo_fingerprint: None,
    };

    let first = store
        .bind_workspace(derive(first_orbit_dir))
        .expect("derive first binding");
    let ephemeral_orbit_dir = repo_root.join(".orbit-ephemeral");
    fs::create_dir_all(&ephemeral_orbit_dir).expect("create ephemeral orbit dir");
    let second = store
        .bind_workspace(derive(ephemeral_orbit_dir.clone()))
        .expect("derive second binding");

    assert_eq!(
        second.workspace_id, first.workspace_id,
        "the same checkout paths resolve back to one logical workspace"
    );
    assert_eq!(second.orbit_dir, normalize_path(&ephemeral_orbit_dir));
}

#[test]
fn bind_workspace_rejects_reusing_an_id_for_a_different_checkout() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = bind(&store, temp.path());

    let other_root = temp.path().join("other-repo");
    let other_orbit_dir = other_root.join(".orbit");
    fs::create_dir_all(&other_orbit_dir).expect("create other orbit dir");
    let result = store.bind_workspace(BindWorkspaceParams {
        workspace_id: Some(first.workspace_id.clone()),
        slug: "Orbit Test".into(),
        repo_root: other_root.clone(),
        workspace_path: other_root,
        orbit_dir: other_orbit_dir,
        repo_fingerprint: None,
    });

    assert!(matches!(
        result,
        Err(OrbitError::Store(message)) if message.contains("already has a local checkout")
    ));
}

#[test]
fn bind_workspace_rejects_explicit_workspace_id_conflict() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    bind(&store, temp.path());

    let result = store.bind_workspace(BindWorkspaceParams {
        workspace_id: Some("other-abcdef".into()),
        slug: "Changed".into(),
        repo_root: temp.path().join("."),
        workspace_path: temp.path().join("."),
        orbit_dir: temp.path().join(".orbit").join("..").join(".orbit"),
        repo_fingerprint: Some("changed".into()),
    });

    assert!(matches!(result, Err(OrbitError::InvalidInput(_))));
}

#[test]
fn rebind_checkout_moves_orbit_dir_onto_the_requested_workspace() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let data_dir = temp.path().join("data");
    let parent = temp.path().to_path_buf();
    fs::create_dir_all(&data_dir).expect("create data dir");
    let synthetic = store
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some("tmp-5b7149".into()),
            slug: "tmp".into(),
            repo_root: parent.clone(),
            workspace_path: parent,
            orbit_dir: data_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("mint synthetic parent bind");

    let repo_root = temp.path().join("repo");
    fs::create_dir_all(&repo_root).expect("create repo");
    let rebound = store
        .rebind_checkout(BindWorkspaceParams {
            workspace_id: Some("ws_qa".into()),
            slug: "qa".into(),
            repo_root: repo_root.clone(),
            workspace_path: repo_root.clone(),
            orbit_dir: data_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("rebind orbit dir");

    assert_eq!(rebound.workspace_id, "ws_qa");
    assert_eq!(rebound.repo_root, normalize_path(&repo_root));
    assert_eq!(rebound.orbit_dir, normalize_path(&data_dir));
    assert!(
        store
            .find_workspace_checkout(&synthetic.workspace_id)
            .expect("lookup synthetic")
            .is_none(),
        "the synthetic checkout row must not keep the data dir"
    );
    let by_orbit = store
        .find_rebind_candidates(&repo_root, &repo_root, &data_dir)
        .expect("lookup rebound orbit dir");
    assert_eq!(by_orbit.len(), 1);
    assert_eq!(by_orbit[0].workspace_id, "ws_qa");
}

#[test]
fn workspace_config_round_trips_and_validates() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "orbit-test-abcdef".into(),
        },
    )
    .expect("write config");

    let read = read_workspace_config(&orbit_dir).expect("read config");
    assert_eq!(read.workspace_id, "orbit-test-abcdef");

    atomic_write_text(
        &workspace_config_path(&orbit_dir),
        "schema_version: 2\nworkspace_id: orbit-test-abcdef\n",
    )
    .expect("write wrong schema");
    assert!(matches!(
        read_workspace_config(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));

    atomic_write_text(
        &workspace_config_path(&orbit_dir),
        "schema_version: 1\nworkspace_id: ''\n",
    )
    .expect("write empty id");
    assert!(matches!(
        read_workspace_config(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));

    atomic_write_text(
        &workspace_config_path(&orbit_dir),
        "schema_version: 1\nworkspace_id: orbit-test-abcdef\nextra: nope\n",
    )
    .expect("write unknown field");
    assert!(matches!(
        read_workspace_config(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn persistence_consumers_share_workspace_id_grammar() {
    let cases = [
        ("ws_orbit", Some("ws_orbit")),
        ("ws_orbit-main_2", Some("ws_orbit-main_2")),
        ("orbit-test-abcdef", Some("orbit-test-abcdef")),
        ("  orbit-test-abcdef  ", Some("orbit-test-abcdef")),
        ("ws_", None),
        ("ws_Orbit", None),
        ("-orbit-abcdef", None),
        ("orbit--test-abcdef", None),
        ("orbit-ABCDEF", None),
        ("orbit-abcde", None),
        ("orbit-abcdef0", None),
        ("orbit/test-abcdef", None),
    ];

    for (raw, expected) in cases {
        let temp = TempDir::new().expect("tempdir");
        let orbit_dir = temp.path().join(".orbit");
        let file_result = write_workspace_config(
            &orbit_dir,
            &WorkspaceConfig {
                schema_version: 1,
                workspace_id: raw.into(),
            },
        );
        let store = store(&temp);
        let registry_result = store.register_workspace(RegisterWorkspaceParams {
            workspace_id: raw.into(),
            slug: "Workspace".into(),
            repo_fingerprint: None,
        });

        match expected {
            Some(expected) => {
                file_result.expect("file workspace binding accepts id");
                assert_eq!(
                    read_workspace_config(&orbit_dir)
                        .expect("read file workspace binding")
                        .workspace_id,
                    expected
                );
                assert_eq!(
                    registry_result.expect("registry accepts id").workspace_id,
                    expected
                );
            }
            None => {
                assert!(file_result.is_err(), "file binding accepted {raw:?}");
                assert!(registry_result.is_err(), "registry accepted {raw:?}");
            }
        }
    }
}

#[test]
fn workspace_config_optional_distinguishes_missing_file() {
    let temp = TempDir::new().expect("tempdir");

    assert_eq!(
        read_workspace_config_optional(&temp.path().join(".orbit")).expect("read optional config"),
        None
    );
}

#[test]
fn workspace_config_reads_from_a_normalized_orbit_directory() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws-test-abcdef".into(),
        },
    )
    .expect("write config");

    let normalized_alias = orbit_dir.join("..").join(".orbit");
    let config = read_workspace_config(&normalized_alias).expect("read config through alias");

    assert_eq!(config.workspace_id, "ws-test-abcdef");
}

#[cfg(unix)]
#[test]
fn workspace_config_rejects_a_symlink_outside_the_orbit_directory() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let outside_config = temp.path().join("outside.yaml");
    atomic_write_text(
        &outside_config,
        "schema_version: 1\nworkspace_id: ws-test-abcdef\n",
    )
    .expect("write outside config");
    std::os::unix::fs::symlink(&outside_config, workspace_config_path(&orbit_dir))
        .expect("link outside config");

    assert!(matches!(
        read_workspace_config_optional(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn workspace_id_for_orbit_dir_returns_id_from_config() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws-test-abcdef".into(),
        },
    )
    .expect("write config");

    assert_eq!(
        workspace_id_for_orbit_dir(&orbit_dir).expect("workspace id"),
        "ws-test-abcdef"
    );
}

#[test]
fn workspace_id_for_orbit_dir_accepts_canonical_logical_registry_id() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_orbit-main".into(),
        },
    )
    .expect("write config");

    assert_eq!(
        workspace_id_for_orbit_dir(&orbit_dir).expect("workspace id"),
        "ws_orbit-main"
    );
}

#[test]
fn workspace_id_for_orbit_dir_missing_file_names_path_and_key() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");

    let err = workspace_id_for_orbit_dir(&orbit_dir).expect_err("missing config");
    let message = err.to_string();
    let config_path = workspace_config_path(&orbit_dir).display().to_string();
    assert!(message.contains("config.yaml"));
    assert!(message.contains("workspace_id"));
    assert!(message.contains(&config_path));
    assert_eq!(message.matches(&config_path).count(), 1);
}

#[test]
fn workspace_id_for_orbit_dir_malformed_yaml_returns_invalid_input() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    atomic_write_text(&workspace_config_path(&orbit_dir), "not: [valid")
        .expect("write malformed config");

    assert!(matches!(
        workspace_id_for_orbit_dir(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn rebind_candidates_match_normalized_paths() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    let candidates = store
        .find_rebind_candidates(
            &temp.path().join("."),
            &temp.path().join("nested").join(".."),
            &workspace.orbit_dir.join("..").join(".orbit"),
        )
        .expect("candidates");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].workspace_id, workspace.workspace_id);
}

#[test]
fn register_task_bundle_rejects_non_canonical_path() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let wrong_path = temp.path().join("other-workspace").join("ORB-00000");
    fs::create_dir_all(&wrong_path).expect("create wrong bundle");

    assert!(matches!(
        store.register_task_bundle("ORB-00000", &workspace.workspace_id, &wrong_path),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn generated_task_index_filters_by_status_priority_and_tags() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    for task_id in ["ORB-00000", "ORB-00001"] {
        let bundle_dir = create_canonical_bundle(&store, &workspace, task_id);
        store
            .register_task_bundle(task_id, &workspace.workspace_id, &bundle_dir)
            .expect("register bundle");
    }

    store
        .replace_task_index(
            &workspace.workspace_id,
            &envelope(
                "ORB-00000",
                TaskStatus::Backlog,
                vec!["Task-Artifacts".into(), "v2".into()],
                Vec::new(),
            ),
        )
        .expect("index first task");
    store
        .replace_task_index(
            &workspace.workspace_id,
            &envelope(
                "ORB-00001",
                TaskStatus::Review,
                vec!["v2".into(), "review".into()],
                Vec::new(),
            ),
        )
        .expect("index second task");

    assert_eq!(
        store
            .indexed_task_count_for_workspace(&workspace.workspace_id)
            .expect("index count"),
        2
    );
    assert_eq!(
        store
            .indexed_task_ids_filtered(
                &workspace.workspace_id,
                &TaskIndexFilter {
                    status: Some(TaskStatus::Review),
                    priority: Some(TaskPriority::High),
                    job_run_id: None,
                    tags: vec!["review".into()],
                },
            )
            .expect("filtered ids"),
        vec!["ORB-00001"]
    );
    assert_eq!(
        store
            .indexed_task_ids_filtered(
                &workspace.workspace_id,
                &TaskIndexFilter {
                    status: None,
                    priority: None,
                    job_run_id: None,
                    tags: vec!["task-artifacts".into(), "v2".into()],
                },
            )
            .expect("tagged ids"),
        vec!["ORB-00000"]
    );
}

#[test]
fn generated_relation_index_supports_forward_and_inverse_lookup() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    for task_id in ["ORB-00000", "ORB-00001", "ORB-00002"] {
        let bundle_dir = create_canonical_bundle(&store, &workspace, task_id);
        store
            .register_task_bundle(task_id, &workspace.workspace_id, &bundle_dir)
            .expect("register bundle");
    }

    store
        .replace_task_index(
            &workspace.workspace_id,
            &envelope(
                "ORB-00000",
                TaskStatus::Backlog,
                Vec::new(),
                vec![
                    TaskRelation {
                        relation_type: TaskRelationType::BlockedBy,
                        target: "ORB-00001".to_string(),
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: "ORB-00002".to_string(),
                    },
                ],
            ),
        )
        .expect("index relations");

    assert_eq!(
        store
            .indexed_relation_targets(
                &workspace.workspace_id,
                "ORB-00000",
                TaskRelationType::BlockedBy,
            )
            .expect("targets"),
        vec!["ORB-00001"]
    );
    assert_eq!(
        store
            .indexed_relation_sources(
                &workspace.workspace_id,
                "ORB-00001",
                TaskRelationType::BlockedBy,
            )
            .expect("sources"),
        vec!["ORB-00000"]
    );
}

#[test]
fn unregister_task_bundle_removes_binding_indexes_and_relation_edges() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    for task_id in ["ORB-00000", "ORB-00001"] {
        let bundle_dir = create_canonical_bundle(&store, &workspace, task_id);
        store
            .register_task_bundle(task_id, &workspace.workspace_id, &bundle_dir)
            .expect("register bundle");
    }
    store
        .replace_task_index(
            &workspace.workspace_id,
            &envelope(
                "ORB-00000",
                TaskStatus::Backlog,
                vec!["v2".into()],
                vec![TaskRelation {
                    relation_type: TaskRelationType::BlockedBy,
                    target: "ORB-00001".to_string(),
                }],
            ),
        )
        .expect("index source relation");

    assert!(
        store
            .unregister_task_bundle("ORB-00000", &workspace.workspace_id)
            .expect("unregister")
    );
    assert_eq!(
        store
            .tasks_for_workspace(&workspace.workspace_id)
            .expect("tasks")
            .into_iter()
            .map(|binding| binding.task_id)
            .collect::<Vec<_>>(),
        vec!["ORB-00001"]
    );
    assert_eq!(
        store
            .indexed_task_count_for_workspace(&workspace.workspace_id)
            .expect("index count"),
        0
    );
    assert_eq!(
        store
            .indexed_relation_sources(
                &workspace.workspace_id,
                "ORB-00001",
                TaskRelationType::BlockedBy,
            )
            .expect("inverse relation"),
        Vec::<String>::new()
    );
}

#[test]
fn unregister_task_bundle_preserves_sibling_workspace_indexes() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace_a_root = temp.path().join("workspace-a");
    let workspace_b_root = temp.path().join("workspace-b");
    let bind_workspace = |workspace_id: &str, root: &Path| {
        let orbit_dir = root.join(".orbit");
        fs::create_dir_all(&orbit_dir).expect("create orbit dir");
        store
            .bind_workspace(BindWorkspaceParams {
                workspace_id: Some(workspace_id.into()),
                slug: workspace_id.into(),
                repo_root: root.to_path_buf(),
                workspace_path: root.to_path_buf(),
                orbit_dir,
                repo_fingerprint: None,
            })
            .expect("bind workspace")
    };
    let workspace_a = bind_workspace("ws_a", &workspace_a_root);
    let workspace_b = bind_workspace("ws_b", &workspace_b_root);

    for task_id in ["ORB-1", "ORB-2"] {
        let bundle_dir = create_canonical_bundle(&store, &workspace_b, task_id);
        store
            .register_task_bundle(task_id, &workspace_b.workspace_id, &bundle_dir)
            .expect("register bundle");
    }
    store
        .replace_task_index(
            &workspace_b.workspace_id,
            &envelope(
                "ORB-1",
                TaskStatus::Backlog,
                vec!["sibling".into()],
                Vec::new(),
            ),
        )
        .expect("index tagged task");
    store
        .replace_task_index(
            &workspace_b.workspace_id,
            &envelope(
                "ORB-2",
                TaskStatus::Backlog,
                Vec::new(),
                vec![TaskRelation {
                    relation_type: TaskRelationType::BlockedBy,
                    target: "ORB-1".to_string(),
                }],
            ),
        )
        .expect("index inbound relation");

    let versions_before = store
        .indexed_task_versions_for_workspace(&workspace_b.workspace_id)
        .expect("versions before unregister");
    let tagged_before = store
        .indexed_task_ids_filtered(
            &workspace_b.workspace_id,
            &TaskIndexFilter {
                status: None,
                priority: None,
                job_run_id: None,
                tags: vec!["sibling".into()],
            },
        )
        .expect("tagged tasks before unregister");
    let relation_sources_before = store
        .indexed_relation_sources(
            &workspace_b.workspace_id,
            "ORB-1",
            TaskRelationType::BlockedBy,
        )
        .expect("relation sources before unregister");

    assert!(
        !store
            .unregister_task_bundle("ORB-1", &workspace_a.workspace_id)
            .expect("unregister sibling task")
    );
    assert_eq!(
        store
            .indexed_task_versions_for_workspace(&workspace_b.workspace_id)
            .expect("versions after unregister"),
        versions_before
    );
    assert_eq!(
        store
            .indexed_task_ids_filtered(
                &workspace_b.workspace_id,
                &TaskIndexFilter {
                    status: None,
                    priority: None,
                    job_run_id: None,
                    tags: vec!["sibling".into()],
                },
            )
            .expect("tagged tasks after unregister"),
        tagged_before
    );
    assert_eq!(
        store
            .indexed_relation_sources(
                &workspace_b.workspace_id,
                "ORB-1",
                TaskRelationType::BlockedBy,
            )
            .expect("relation sources after unregister"),
        relation_sources_before
    );
}

#[test]
fn completion_by_complexity_keeps_unset_as_its_own_bucket() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    for task_id in ["ORB-00000", "ORB-00001", "ORB-00002", "ORB-00003"] {
        let bundle_dir = create_canonical_bundle(&store, &workspace, task_id);
        store
            .register_task_bundle(task_id, &workspace.workspace_id, &bundle_dir)
            .expect("register bundle");
    }

    let mut hard_done = envelope("ORB-00000", TaskStatus::Done, Vec::new(), Vec::new());
    hard_done.complexity = Some(TaskComplexity::Hard);
    store
        .replace_task_index(&workspace.workspace_id, &hard_done)
        .expect("index hard");

    let mut medium_rejected = envelope("ORB-00001", TaskStatus::Rejected, Vec::new(), Vec::new());
    medium_rejected.complexity = Some(TaskComplexity::Medium);
    store
        .replace_task_index(&workspace.workspace_id, &medium_rejected)
        .expect("index medium");

    store
        .replace_task_index(
            &workspace.workspace_id,
            &envelope("ORB-00002", TaskStatus::Archived, Vec::new(), Vec::new()),
        )
        .expect("index unset");

    // An explicitly `unassessed` task is the same "nobody assessed this"
    // concept as an unindexed one and shares its bucket [ORB-10895].
    let mut unassessed_backlog = envelope("ORB-00003", TaskStatus::Backlog, Vec::new(), Vec::new());
    unassessed_backlog.complexity = Some(TaskComplexity::Unassessed);
    store
        .replace_task_index(&workspace.workspace_id, &unassessed_backlog)
        .expect("index unassessed");

    let rows = store
        .completion_by_complexity(&workspace.workspace_id)
        .expect("aggregate");
    assert_eq!(
        rows.iter()
            .map(|row| row.complexity.as_str())
            .collect::<Vec<_>>(),
        [UNSET_BUCKET, "medium", "hard"]
    );

    let unset = rows
        .iter()
        .find(|row| row.complexity == UNSET_BUCKET)
        .unwrap();
    assert_eq!(unset.total, 2);
    assert_eq!(unset.by_status.get("archived").copied(), Some(1));
    assert_eq!(unset.by_status.get("backlog").copied(), Some(1));

    let hard = rows.iter().find(|row| row.complexity == "hard").unwrap();
    assert_eq!(hard.total, 1);
    assert_eq!(hard.by_status.get("done").copied(), Some(1));

    let map = store
        .complexity_by_task_id(&workspace.workspace_id)
        .expect("map");
    assert_eq!(map.get("ORB-00000").map(String::as_str), Some("hard"));
    assert_eq!(map.get("ORB-00002").map(String::as_str), Some(UNSET_BUCKET));
    assert_eq!(map.get("ORB-00003").map(String::as_str), Some(UNSET_BUCKET));
}

#[test]
fn seed_allocator_start_moves_counter_forward() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    assert_eq!(store.allocator_next_number().expect("read"), 0);

    let outcome = store.seed_allocator_start(10_000).expect("seed");
    assert_eq!(outcome.previous, 0);
    assert_eq!(outcome.next, 10_000);
    assert!(outcome.changed);
    assert_eq!(store.allocator_next_number().expect("read"), 10_000);

    // Re-seeding to the same value is a no-op.
    let again = store.seed_allocator_start(10_000).expect("seed again");
    assert!(!again.changed);
}

#[test]
fn seed_allocator_start_refuses_to_lower() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store.seed_allocator_start(5_000).expect("seed");
    let err = store
        .seed_allocator_start(4_999)
        .expect_err("must refuse lowering");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert_eq!(store.allocator_next_number().expect("read"), 5_000);
}

#[test]
fn seeded_allocator_hands_out_seeded_id() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    store.seed_allocator_start(10_000).expect("seed");
    let id = store
        .allocate_task_id(&workspace.workspace_id)
        .expect("allocate");
    assert_eq!(id, "ORB-10000");
}

#[test]
fn bump_allocator_never_lowers() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store.seed_allocator_start(500).expect("seed");
    store.bump_allocator_to_at_least(100).expect("bump low");
    assert_eq!(store.allocator_next_number().expect("read"), 500);
    store.bump_allocator_to_at_least(900).expect("bump high");
    assert_eq!(store.allocator_next_number().expect("read"), 900);
}
