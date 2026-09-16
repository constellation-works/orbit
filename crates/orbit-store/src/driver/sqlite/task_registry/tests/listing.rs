//! Index reads behind bounded listing: the freshness rows, the filtered and
//! bounded selection, and the workspace-scoped status projection.

use std::collections::BTreeSet;
use std::fs;

use chrono::Duration;
use orbit_types::task::{Task, TaskEnvelopeV2, TaskPriority, TaskReferenceIndex, TaskStatus};
use tempfile::TempDir;

use super::{bind, envelope, store};
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskIndexFilter, TaskRegistryStore, WorkspaceCheckoutBinding,
};

fn bind_second(store: &TaskRegistryStore, temp: &TempDir) -> WorkspaceCheckoutBinding {
    let root = temp.path().join("second");
    fs::create_dir_all(root.join(".orbit")).expect("create second orbit dir");
    store
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("second-abcdef".into()),
            slug: "Second".into(),
            repo_root: root.clone(),
            workspace_path: root.clone(),
            orbit_dir: root.join(".orbit"),
            repo_fingerprint: None,
        })
        .expect("bind second workspace")
}

/// Register `task_id` in `partition_id` and index the given envelope.
fn index(store: &TaskRegistryStore, partition_id: &str, envelope: &TaskEnvelopeV2) {
    let path = store
        .canonical_task_bundle_path(partition_id, &envelope.id)
        .expect("canonical bundle path");
    fs::create_dir_all(&path).expect("create bundle");
    store
        .register_task_bundle(&envelope.id, partition_id, &path)
        .expect("register bundle");
    store
        .replace_task_index(partition_id, envelope)
        .expect("index task");
}

/// Five tasks created one second apart, newest `ORB-00004`: statuses cycle
/// backlog, done, review, archived, backlog; even numbers carry `even`.
fn seed_listing(store: &TaskRegistryStore, partition_id: &str) {
    let statuses = [
        TaskStatus::Backlog,
        TaskStatus::Done,
        TaskStatus::Review,
        TaskStatus::Archived,
        TaskStatus::Backlog,
    ];
    for (number, status) in statuses.into_iter().enumerate() {
        let tags = if number % 2 == 0 {
            vec!["even".to_string()]
        } else {
            Vec::new()
        };
        let mut task = envelope(&format!("ORB-0000{number}"), status, tags, Vec::new());
        task.created_at += Duration::seconds(number as i64);
        task.updated_at = task.created_at;
        index(store, partition_id, &task);
    }
}

fn ids(store: &TaskRegistryStore, partition_id: &str, filter: &TaskIndexFilter) -> Vec<String> {
    store
        .indexed_task_selection(partition_id, filter, false, None)
        .expect("selection")
        .ids
}

#[test]
fn freshness_rows_carry_the_fields_listing_filters_and_orders_by() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let mut task = envelope(
        "ORB-00000",
        TaskStatus::Review,
        vec![" Mixed-Case ".into(), "v2".into()],
        Vec::new(),
    );
    task.job_run_id = Some("jrun-1".into());
    index(&store, &workspace.partition_id, &task);

    let rows = store
        .indexed_task_rows_for_workspace(&workspace.partition_id)
        .expect("index rows");
    let row = &rows["ORB-00000"];
    assert_eq!(row.status, "review");
    assert_eq!(row.priority, TaskPriority::High.to_string());
    assert_eq!(row.job_run_id.as_deref(), Some("jrun-1"));
    assert_eq!(row.created_at, task.created_at.to_rfc3339());
    assert_eq!(row.updated_at, task.updated_at.to_rfc3339());
    assert_eq!(
        row.tags,
        BTreeSet::from(["mixed-case".to_string(), "v2".to_string()])
    );
    assert!(row.matches(&task));

    // An edit that keeps `updated_at` but changes a filter field no longer
    // matches, so the freshness scan rebuilds rather than serving the index.
    let mut retagged = task.clone();
    retagged.tags = vec!["v2".into()];
    assert!(!row.matches(&retagged));
    let mut reprioritized = task.clone();
    reprioritized.priority = TaskPriority::Low;
    assert!(!row.matches(&reprioritized));
    let mut moved = task.clone();
    moved.status = TaskStatus::Done;
    assert!(!row.matches(&moved));
}

#[test]
fn selection_orders_bounds_and_counts_from_the_index() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let partition_id = workspace.partition_id.clone();
    seed_listing(&store, &partition_id);

    let all = TaskIndexFilter::default();
    assert_eq!(
        ids(&store, &partition_id, &all),
        [
            "ORB-00004",
            "ORB-00003",
            "ORB-00002",
            "ORB-00001",
            "ORB-00000"
        ]
    );

    let page = store
        .indexed_task_selection(&partition_id, &all, false, Some(2))
        .expect("bounded selection");
    assert_eq!(page.ids, ["ORB-00004", "ORB-00003"]);
    assert_eq!(page.total, 5, "the total counts past the limit");

    let terminal_last = store
        .indexed_task_selection(&partition_id, &all, true, Some(4))
        .expect("status-aware selection");
    assert_eq!(
        terminal_last.ids,
        ["ORB-00004", "ORB-00002", "ORB-00000", "ORB-00003"],
        "non-terminal newest first, then terminal newest first"
    );
    assert_eq!(terminal_last.total, 5);

    let counted_only = store
        .indexed_task_selection(&partition_id, &all, false, Some(0))
        .expect("count only");
    assert!(counted_only.ids.is_empty());
    assert_eq!(counted_only.total, 5);
}

#[test]
fn selection_applies_every_indexed_predicate() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let partition_id = workspace.partition_id.clone();
    seed_listing(&store, &partition_id);
    let rows = store
        .indexed_task_rows_for_workspace(&partition_id)
        .expect("index rows");

    let backlog_or_review = TaskIndexFilter {
        statuses: vec![TaskStatus::Backlog, TaskStatus::Review],
        ..Default::default()
    };
    assert_eq!(
        ids(&store, &partition_id, &backlog_or_review),
        ["ORB-00004", "ORB-00002", "ORB-00000"]
    );

    let even = TaskIndexFilter {
        tags: vec!["even".into()],
        ..Default::default()
    };
    assert_eq!(
        ids(&store, &partition_id, &even),
        ["ORB-00004", "ORB-00002", "ORB-00000"]
    );
    let even_and_missing = TaskIndexFilter {
        tags: vec!["even".into(), "absent".into()],
        ..Default::default()
    };
    assert!(ids(&store, &partition_id, &even_and_missing).is_empty());

    let none_low = TaskIndexFilter {
        priority: Some(TaskPriority::Low),
        ..Default::default()
    };
    assert!(ids(&store, &partition_id, &none_low).is_empty());

    let without_two = TaskIndexFilter {
        excluded_ids: vec!["ORB-00002".into()],
        ..Default::default()
    };
    let selection = store
        .indexed_task_selection(&partition_id, &without_two, false, Some(10))
        .expect("selection without an unsettled task");
    assert_eq!(
        selection.ids,
        ["ORB-00004", "ORB-00003", "ORB-00001", "ORB-00000"]
    );
    assert_eq!(selection.total, 4, "an excluded task is not counted either");

    let boundary = envelope("ORB-00002", TaskStatus::Review, Vec::new(), Vec::new());
    let created_at = chrono::DateTime::parse_from_rfc3339(&rows["ORB-00002"].created_at)
        .expect("indexed created_at")
        .with_timezone(&chrono::Utc);
    let after_two = TaskIndexFilter {
        scan_before: Some((created_at, boundary.id)),
        ..Default::default()
    };
    assert_eq!(
        ids(&store, &partition_id, &after_two),
        ["ORB-00001", "ORB-00000"],
        "continuation resumes strictly after the boundary row"
    );
}

/// The projection a listing gets: its own workspace, the targets its rows
/// name wherever they live, and one stand-in per foreign prefix the registry
/// knows — nothing else from other workspaces.
#[test]
fn listing_status_projection_covers_workspace_targets_and_known_prefixes() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let alpha = bind(&store, temp.path());
    let beta = bind_second(&store, &temp);

    index(
        &store,
        &alpha.partition_id,
        &envelope("ORB-00000", TaskStatus::Backlog, Vec::new(), Vec::new()),
    );
    index(
        &store,
        &beta.partition_id,
        &envelope("ORB-00001", TaskStatus::Done, Vec::new(), Vec::new()),
    );
    index(
        &store,
        &beta.partition_id,
        &envelope("ORB-00002", TaskStatus::Blocked, Vec::new(), Vec::new()),
    );
    index(
        &store,
        &beta.partition_id,
        &envelope("DK-00001", TaskStatus::Review, Vec::new(), Vec::new()),
    );

    let targets = BTreeSet::from([
        "ORB-00001".to_string(),
        "DK-00002".to_string(),
        "ZZ-00001".to_string(),
    ]);
    let scoped = store
        .task_status_index_for(&alpha.partition_id, &targets)
        .expect("scoped projection");
    assert_eq!(scoped.get("ORB-00000"), Some(&TaskStatus::Backlog));
    assert_eq!(
        scoped.get("ORB-00001"),
        Some(&TaskStatus::Done),
        "a referenced target resolves from another workspace"
    );
    assert_eq!(
        scoped.get("DK-00001"),
        Some(&TaskStatus::Review),
        "a missing target under a known foreign prefix is represented by one task of that prefix"
    );
    assert!(
        !scoped.contains_key("ORB-00002"),
        "unreferenced tasks of other workspaces stay out"
    );
    assert_eq!(scoped.len(), 3);

    // Both projections tell a task in alpha the same thing about each target.
    let global = store.global_task_status_index().expect("global projection");
    let source = serde_yaml::from_str::<Task>(
        r#"id: ORB-00000
title: source
description: Fixture.
context_files: []
status: backlog
priority: medium
task_type: chore
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
relations:
  - type: blocked_by
    target: DK-00002
"#,
    )
    .expect("fixture task deserializes");
    let scoped_index = TaskReferenceIndex::from_status_index(&scoped);
    let global_index = TaskReferenceIndex::from_status_index(&global);
    for target in ["ORB-00001", "ORB-00002", "DK-00002", "ZZ-00001"] {
        assert_eq!(
            scoped_index.is_not_verifiable_here(&source, target, &scoped),
            global_index.is_not_verifiable_here(&source, target, &global),
            "{target}"
        );
    }
    assert!(scoped_index.is_not_verifiable_here(&source, "ZZ-00001", &scoped));
    assert!(!scoped_index.is_not_verifiable_here(&source, "DK-00002", &scoped));

    let empty = store
        .task_status_index_for(&alpha.partition_id, &BTreeSet::new())
        .expect("workspace-only projection");
    assert_eq!(empty.keys().collect::<Vec<_>>(), ["ORB-00000"]);
}
