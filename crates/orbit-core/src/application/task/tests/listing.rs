use super::test_runtime;
use crate::application::task::{TaskAddParams, TaskListFilter, TaskListQuery};
use orbit_types::task::TaskStatus;

#[test]
fn readiness_and_projection_statuses_are_captured_after_index_repair() {
    let (_root, runtime) = test_runtime();
    let runtime = runtime.with_actor(crate::ActorIdentity::human("human"));
    let dependency = runtime
        .add_task(TaskAddParams {
            title: "Completed dependency".to_string(),
            description: "Fixture".to_string(),
            status: Some(TaskStatus::Done),
            ..Default::default()
        })
        .unwrap();
    let dependent = runtime
        .add_task(TaskAddParams {
            title: "Ready dependent".to_string(),
            description: "Fixture".to_string(),
            status: Some(TaskStatus::Backlog),
            dependencies: vec![dependency.id.clone()],
            ..Default::default()
        })
        .unwrap();
    let conn =
        rusqlite::Connection::open(runtime.global_root().join("tasks/index.sqlite")).unwrap();
    for sql in [
        "DELETE FROM task_bundle_index",
        "UPDATE task_bundle_index SET status = 'backlog', updated_at = 'stale'",
    ] {
        conn.execute(sql, []).unwrap();
        let page = runtime
            .query_task_rows(&TaskListQuery {
                ready: true,
                limit: 1,
                filter: TaskListFilter {
                    statuses: Some(vec![TaskStatus::Backlog]),
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].task.id, dependent.id);
        assert_eq!(page.status_by_id[&dependency.id], TaskStatus::Done);
    }
}

#[test]
fn readiness_and_path_filters_find_a_match_older_than_the_first_page() {
    let (_root, runtime) = test_runtime();
    let file = runtime.paths().repo_root.join("selection.rs");
    std::fs::write(&file, "// fixture").unwrap();
    let ready = runtime
        .add_task(TaskAddParams {
            title: "Buried ready task".to_string(),
            description: "Match fixture".to_string(),
            context_files: vec!["file:selection.rs".to_string()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    for n in 0..55 {
        runtime
            .add_task(TaskAddParams {
                title: format!("New blocked task {n}"),
                description: "Filter fixture".to_string(),
                dependencies: vec![ready.id.clone()],
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap();
    }
    for (ready_filter, path) in [
        (true, None),
        (false, Some("selection.rs".to_string())),
        (true, Some("selection.rs".to_string())),
    ] {
        let page = runtime
            .query_task_rows(&TaskListQuery {
                ready: ready_filter,
                path,
                limit: 1,
                filter: TaskListFilter::default(),
            })
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].task.id, ready.id);
    }
}

#[test]
fn replica_visibility_is_preserved_for_candidates_and_bounded_rows() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Local fixture".to_string(),
            description: "Visibility fixture".to_string(),
            ..Default::default()
        })
        .unwrap();
    let replica = runtime.with_coordination_write_owner(Some("remote-owner".to_string()));
    let candidates = replica
        .task_candidates(&TaskListFilter::default(), 50)
        .unwrap();
    assert_eq!(candidates.total, 0);
    assert!(candidates.items.is_empty());
    let page = replica
        .query_task_rows(&TaskListQuery {
            limit: 50,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.total, 0);
    assert!(page.items.is_empty());
    assert!(replica.get_listed_task_row(&task.id).unwrap().is_none());
}
