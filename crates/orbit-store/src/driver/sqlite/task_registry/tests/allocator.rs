//! The task-id allocator: prefixes, batches, seeding and exhaustion.

use std::fs;

use orbit_common::OrbitError;
use orbit_types::task::ORB_TASK_ID_MAX;
use rusqlite::params;
use tempfile::TempDir;

use super::super::util::now_string;
use super::super::{BindWorkspaceParams, TaskRegistryStore};
use super::{bind, registry_path, store};

#[test]
fn allocator_returns_monotonic_orb_ids() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    assert_eq!(
        store.allocate_task_id(&workspace.partition_id).expect("id"),
        "ORB-00000"
    );
    assert_eq!(
        store.allocate_task_id(&workspace.partition_id).expect("id"),
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
        store.allocate_task_id(&workspace.partition_id).expect("id"),
        "DE-99999"
    );
    assert_eq!(
        store.allocate_task_id(&workspace.partition_id).expect("id"),
        "DE-100000"
    );
}

/// Runtime construction reasserts the configured prefix on every command, so
/// the matching case has to stay observational: a registry on read-only
/// storage must answer it instead of failing on a write it never needed.
#[cfg(unix)]
#[test]
fn reasserting_the_bound_task_prefix_needs_no_write() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let path = registry_path(&temp);
    let store = TaskRegistryStore::open(&path).expect("open registry");
    store.set_task_prefix("DE").expect("bind the host prefix");
    drop(store);
    let before = fs::read(&path).expect("snapshot the bound registry");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("make read-only");

    let registry = TaskRegistryStore::open(&path).expect("observe the bound registry");
    registry
        .set_task_prefix("DE")
        .expect("reasserting the bound prefix is an observation");

    let error = registry
        .set_task_prefix("XY")
        .expect_err("a real prefix change still needs writable storage");
    assert!(error.is_readonly_or_access_failure(), "{error}");
    drop(registry);
    assert_eq!(fs::read(&path).expect("re-read the registry"), before);
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
            partition_id: Some("second-abcdef".into()),
            slug: "Second".into(),
            repo_root: second_root.clone(),
            workspace_path: second_root.clone(),
            orbit_dir: second_root.join(".orbit"),
            repo_fingerprint: None,
        })
        .expect("bind second workspace");

    assert_eq!(
        store
            .allocate_task_id(&first.partition_id)
            .expect("first id"),
        "ORB-00000"
    );
    assert_eq!(
        store
            .allocate_task_id(&second.partition_id)
            .expect("second id"),
        "ORB-00001"
    );
}

#[test]
fn batch_allocation_hands_out_consecutive_ids_with_one_counter_bump() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    assert!(
        store
            .allocate_task_ids(&workspace.partition_id, 0)
            .expect("empty reservation")
            .is_empty()
    );
    assert_eq!(
        store.allocator_next_number().expect("next number"),
        0,
        "an empty reservation must not move the counter"
    );

    let ids = store
        .allocate_task_ids(&workspace.partition_id, 4)
        .expect("reserve four ids");
    assert_eq!(ids, ["ORB-00000", "ORB-00001", "ORB-00002", "ORB-00003"]);
    assert_eq!(
        store.allocator_next_number().expect("next number"),
        4,
        "one bump covers the whole reservation"
    );
    // The batch and single-id paths share one counter, so the next single
    // allocation continues where the reservation stopped.
    assert_eq!(
        store
            .allocate_task_id(&workspace.partition_id)
            .expect("single id"),
        "ORB-00004"
    );
}

#[test]
fn batch_allocation_refuses_a_reservation_that_would_cross_the_ceiling() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    {
        let conn = store.conn.lock().expect("lock registry");
        conn.execute(
            "UPDATE allocator_state SET next_number = ?1, updated_at = ?2
             WHERE authority = 'local'",
            params![i64::from(ORB_TASK_ID_MAX) - 1, now_string()],
        )
        .expect("park the allocator below the ceiling");
    }

    // Two ids fit exactly; asking for three must be refused whole rather than
    // part-served, and a refusal must leave the counter where it was.
    assert!(matches!(
        store.allocate_task_ids(&workspace.partition_id, 3),
        Err(OrbitError::Store(message)) if message.contains("exhausted")
    ));
    assert_eq!(
        store.allocator_next_number().expect("next number"),
        ORB_TASK_ID_MAX - 1
    );
    assert_eq!(
        store
            .allocate_task_ids(&workspace.partition_id, 2)
            .expect("the last two ids")
            .len(),
        2
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
        store.allocate_task_id(&workspace.partition_id),
        Err(OrbitError::Store(message)) if message.contains("exhausted")
    ));
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
        .allocate_task_id(&workspace.partition_id)
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
