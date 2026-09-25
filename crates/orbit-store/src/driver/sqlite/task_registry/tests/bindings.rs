//! Task-bundle bindings: batch registration, canonical-path checks and
//! unregistration.

use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{TaskRelation, TaskRelationType, TaskStatus};
use tempfile::TempDir;

use super::super::{BindWorkspaceParams, TaskIndexFilter};
use super::{bind, create_canonical_bundle, envelope, store};

#[test]
fn batch_registration_and_batch_index_replacement_land_as_one_unit() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    let bundles = ["ORB-00000", "ORB-00001", "ORB-00002"]
        .into_iter()
        .map(|task_id| {
            (
                task_id.to_string(),
                create_canonical_bundle(&store, &workspace, task_id),
            )
        })
        .collect::<Vec<_>>();
    store
        .register_task_bundles(&workspace.partition_id, &bundles)
        .expect("register the set");
    assert_eq!(
        store
            .tasks_for_workspace(&workspace.partition_id)
            .expect("bindings")
            .len(),
        3
    );

    // A member may point at another member the batch has not written yet.
    let envelopes = vec![
        envelope(
            "ORB-00000",
            TaskStatus::Backlog,
            Vec::new(),
            vec![TaskRelation {
                relation_type: TaskRelationType::ChildOf,
                target: "ORB-00002".into(),
            }],
        ),
        envelope(
            "ORB-00002",
            TaskStatus::Done,
            vec!["alpha".into()],
            Vec::new(),
        ),
    ];
    store
        .replace_task_indexes(&workspace.partition_id, &envelopes)
        .expect("index the subset");

    // Exactly the requested subset is indexed: the untouched third binding
    // keeps no row, which is what lets a partial repair pass use this.
    assert_eq!(
        store
            .indexed_task_versions_for_workspace(&workspace.partition_id)
            .expect("indexed versions")
            .into_keys()
            .collect::<Vec<_>>(),
        vec!["ORB-00000".to_string(), "ORB-00002".to_string()]
    );

    // A rejected member rolls the whole batch back rather than half-applying it.
    let rejected = vec![
        envelope("ORB-00001", TaskStatus::Backlog, Vec::new(), Vec::new()),
        envelope(
            "ORB-00002",
            TaskStatus::Backlog,
            Vec::new(),
            vec![TaskRelation {
                relation_type: TaskRelationType::ChildOf,
                target: "ORB-09999".into(),
            }],
        ),
    ];
    assert!(
        store
            .replace_task_indexes(&workspace.partition_id, &rejected)
            .is_err()
    );
    assert_eq!(
        store
            .indexed_task_versions_for_workspace(&workspace.partition_id)
            .expect("indexed versions")
            .into_keys()
            .collect::<Vec<_>>(),
        vec!["ORB-00000".to_string(), "ORB-00002".to_string()],
        "a refused batch leaves the previous index untouched"
    );
    assert_eq!(
        store
            .complexity_by_task_id(&workspace.partition_id)
            .expect("indexed rows")
            .len(),
        2
    );
}

#[test]
fn batch_registration_refuses_a_non_canonical_path_without_registering_the_set() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    let good = create_canonical_bundle(&store, &workspace, "ORB-00000");
    let bundles = vec![
        ("ORB-00000".to_string(), good),
        ("ORB-00001".to_string(), temp.path().join("elsewhere")),
    ];
    assert!(matches!(
        store.register_task_bundles(&workspace.partition_id, &bundles),
        Err(OrbitError::InvalidInput(message)) if message.contains("ORB-00001")
    ));
    assert!(
        store
            .tasks_for_workspace(&workspace.partition_id)
            .expect("bindings")
            .is_empty(),
        "a rejected entry must not leave its neighbours registered"
    );
}

#[test]
fn register_task_bundle_rejects_non_canonical_path() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let wrong_path = temp.path().join("other-workspace").join("ORB-00000");
    fs::create_dir_all(&wrong_path).expect("create wrong bundle");

    assert!(matches!(
        store.register_task_bundle("ORB-00000", &workspace.partition_id, &wrong_path),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn unregister_task_bundle_removes_binding_indexes_and_relation_edges() {
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
            .unregister_task_bundle("ORB-00000", &workspace.partition_id)
            .expect("unregister")
    );
    assert_eq!(
        store
            .tasks_for_workspace(&workspace.partition_id)
            .expect("tasks")
            .into_iter()
            .map(|binding| binding.task_id)
            .collect::<Vec<_>>(),
        vec!["ORB-00001"]
    );
    assert_eq!(
        store
            .indexed_task_count_for_workspace(&workspace.partition_id)
            .expect("index count"),
        0
    );
    assert_eq!(
        store
            .indexed_relation_sources(
                &workspace.partition_id,
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
                partition_id: Some(workspace_id.into()),
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
            .register_task_bundle(task_id, &workspace_b.partition_id, &bundle_dir)
            .expect("register bundle");
    }
    store
        .replace_task_index(
            &workspace_b.partition_id,
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
            &workspace_b.partition_id,
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
        .indexed_task_versions_for_workspace(&workspace_b.partition_id)
        .expect("versions before unregister");
    let tagged_before = store
        .indexed_task_ids_filtered(
            &workspace_b.partition_id,
            &TaskIndexFilter {
                tags: vec!["sibling".into()],
                ..Default::default()
            },
        )
        .expect("tagged tasks before unregister");
    let relation_sources_before = store
        .indexed_relation_sources(
            &workspace_b.partition_id,
            "ORB-1",
            TaskRelationType::BlockedBy,
        )
        .expect("relation sources before unregister");

    assert!(
        !store
            .unregister_task_bundle("ORB-1", &workspace_a.partition_id)
            .expect("unregister sibling task")
    );
    assert_eq!(
        store
            .indexed_task_versions_for_workspace(&workspace_b.partition_id)
            .expect("versions after unregister"),
        versions_before
    );
    assert_eq!(
        store
            .indexed_task_ids_filtered(
                &workspace_b.partition_id,
                &TaskIndexFilter {
                    tags: vec!["sibling".into()],
                    ..Default::default()
                },
            )
            .expect("tagged tasks after unregister"),
        tagged_before
    );
    assert_eq!(
        store
            .indexed_relation_sources(
                &workspace_b.partition_id,
                "ORB-1",
                TaskRelationType::BlockedBy,
            )
            .expect("relation sources after unregister"),
        relation_sources_before
    );
}
