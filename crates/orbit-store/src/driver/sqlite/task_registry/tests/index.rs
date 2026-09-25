//! Task index rows: filtered selection and the complexity projection.

use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, UNSET_BUCKET};
use tempfile::TempDir;

use super::super::TaskIndexFilter;
use super::{bind, create_canonical_bundle, envelope, store};

#[test]
fn generated_task_index_filters_by_status_priority_and_tags() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    for task_id in ["ORB-00000", "ORB-00001"] {
        let bundle_dir = create_canonical_bundle(&store, &workspace, task_id);
        store
            .register_task_bundle(task_id, &workspace.partition_id, &bundle_dir)
            .expect("register bundle");
    }

    store
        .replace_task_index(
            &workspace.partition_id,
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
            &workspace.partition_id,
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
            .indexed_task_count_for_workspace(&workspace.partition_id)
            .expect("index count"),
        2
    );
    assert_eq!(
        store
            .indexed_task_ids_filtered(
                &workspace.partition_id,
                &TaskIndexFilter {
                    statuses: vec![TaskStatus::Review],
                    priority: Some(TaskPriority::High),
                    tags: vec!["review".into()],
                    ..Default::default()
                },
            )
            .expect("filtered ids"),
        vec!["ORB-00001"]
    );
    assert_eq!(
        store
            .indexed_task_ids_filtered(
                &workspace.partition_id,
                &TaskIndexFilter {
                    tags: vec!["task-artifacts".into(), "v2".into()],
                    ..Default::default()
                },
            )
            .expect("tagged ids"),
        vec!["ORB-00000"]
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
            .register_task_bundle(task_id, &workspace.partition_id, &bundle_dir)
            .expect("register bundle");
    }

    let mut hard_done = envelope("ORB-00000", TaskStatus::Done, Vec::new(), Vec::new());
    hard_done.complexity = Some(TaskComplexity::Hard);
    store
        .replace_task_index(&workspace.partition_id, &hard_done)
        .expect("index hard");

    let mut medium_rejected = envelope("ORB-00001", TaskStatus::Rejected, Vec::new(), Vec::new());
    medium_rejected.complexity = Some(TaskComplexity::Medium);
    store
        .replace_task_index(&workspace.partition_id, &medium_rejected)
        .expect("index medium");

    store
        .replace_task_index(
            &workspace.partition_id,
            &envelope("ORB-00002", TaskStatus::Archived, Vec::new(), Vec::new()),
        )
        .expect("index unset");

    // An explicitly `unassessed` task is the same "nobody assessed this"
    // concept as an unindexed one and shares its bucket [ORB-10895].
    let mut unassessed_backlog = envelope("ORB-00003", TaskStatus::Backlog, Vec::new(), Vec::new());
    unassessed_backlog.complexity = Some(TaskComplexity::Unassessed);
    store
        .replace_task_index(&workspace.partition_id, &unassessed_backlog)
        .expect("index unassessed");

    let rows = store
        .completion_by_complexity(&workspace.partition_id)
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
        .complexity_by_task_id(&workspace.partition_id)
        .expect("map");
    assert_eq!(map.get("ORB-00000").map(String::as_str), Some("hard"));
    assert_eq!(map.get("ORB-00002").map(String::as_str), Some(UNSET_BUCKET));
    assert_eq!(map.get("ORB-00003").map(String::as_str), Some(UNSET_BUCKET));
}
