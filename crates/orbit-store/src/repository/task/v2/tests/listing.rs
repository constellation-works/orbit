//! Compact (`EnvelopeOnly`) listing pages: no bundle reads, and the same
//! envelope-derived fields a hydrated page carries.

use super::*;
use crate::contracts::{TaskListFilter, TaskListHydration};
use crate::repository::task::v2_bundle::LISTING_BUNDLE_READS;

fn bundle_reads<T>(op: impl FnOnce() -> T) -> (T, u64) {
    let before = LISTING_BUNDLE_READS.with(std::cell::Cell::get);
    let value = op();
    (
        value,
        LISTING_BUNDLE_READS.with(std::cell::Cell::get) - before,
    )
}

#[test]
fn envelope_only_page_reads_no_bundles_and_matches_hydrated_envelope_fields() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    for index in 0..3 {
        store
            .create_task(create_params(&format!("Task {index}"), TaskStatus::Backlog))
            .expect("create task");
    }
    // Warm the index/envelope caches with a hydrated page, as any earlier
    // list in the process would, so the counts below measure only the page
    // under test.
    let (full, full_reads) = bundle_reads(|| {
        store
            .query_task_rows(
                &TaskListFilter::default(),
                50,
                None,
                TaskListHydration::Full,
            )
            .expect("full page")
    });
    assert_eq!(full.items.len(), 3);
    assert!(
        full_reads >= 3,
        "hydrated page reads each selected bundle (got {full_reads})"
    );
    assert!(
        full.items
            .iter()
            .all(|row| row.task.description == "Detailed task description"
                && !row.comments.is_empty())
    );

    let (compact, compact_reads) = bundle_reads(|| {
        store
            .query_task_rows(
                &TaskListFilter::default(),
                50,
                None,
                TaskListHydration::EnvelopeOnly,
            )
            .expect("compact page")
    });
    assert_eq!(
        compact_reads, 0,
        "envelope-only page must perform 0 listing bundle reads"
    );
    assert_eq!(compact.total, full.total);
    assert_eq!(compact.total_without_cursor, full.total_without_cursor);
    assert_eq!(compact.status_by_id, full.status_by_id);
    assert_eq!(compact.items.len(), full.items.len());
    for (compact_row, full_row) in compact.items.iter().zip(&full.items) {
        let (compact_task, full_task) = (&compact_row.task, &full_row.task);
        // Every field a compact task-list row keeps is envelope-derived.
        assert_eq!(compact_task.id, full_task.id);
        assert_eq!(compact_task.title, full_task.title);
        assert_eq!(compact_task.status, full_task.status);
        assert_eq!(compact_task.priority, full_task.priority);
        assert_eq!(compact_task.task_type, full_task.task_type);
        assert_eq!(compact_task.dependencies(), full_task.dependencies());
        assert_eq!(compact_task.implemented_by, full_task.implemented_by);
        assert_eq!(compact_task.not_before, full_task.not_before);
        assert_eq!(compact_task.created_at, full_task.created_at);
        assert_eq!(compact_task.updated_at, full_task.updated_at);
        // And nothing bundle-backed leaks into the compact row.
        assert!(compact_task.description.is_empty());
        assert!(compact_task.acceptance_criteria.is_empty());
        assert!(compact_task.plan.is_empty());
        assert!(compact_task.execution_summary.is_empty());
        assert!(compact_row.comments.is_empty());
        assert!(compact_row.history.is_empty());
        assert!(compact_row.artifacts.is_empty());
    }
}

#[test]
fn small_status_lookup_returns_only_its_targets_across_workspaces() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let mut ids = Vec::new();
    for index in 0..6 {
        ids.push(
            store
                .create_task(create_params(&format!("Task {index}"), TaskStatus::Backlog))
                .expect("create task")
                .id,
        );
    }
    let registry =
        TaskRegistryStore::open(&task_registry_path(temp.path())).expect("open registry");
    let other = bound_store(&registry, &temp, "orbit-other-123456", "other");
    let foreign = other
        .create_task(create_params("Foreign", TaskStatus::Done))
        .expect("create foreign task")
        .id;

    // Two workspace targets: exactly those two, not the whole workspace.
    let targets = [ids[0].clone(), ids[1].clone()]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let index = store
        .task_status_index_for(&store.workspace_id, &targets)
        .expect("workspace status lookup");
    assert_eq!(index.len(), 2, "bounded lookup returned {index:?}");
    assert_eq!(index.get(&ids[0]), Some(&TaskStatus::Backlog));
    assert_eq!(index.get(&ids[1]), Some(&TaskStatus::Backlog));

    // One cross-workspace target resolves where it is registered.
    let targets = [foreign.clone()]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let index = store
        .task_status_index_for(&store.workspace_id, &targets)
        .expect("cross-workspace status lookup");
    assert_eq!(index, [(foreign, TaskStatus::Done)].into_iter().collect());

    // A missing target under a prefix no workspace uses stays absent, and a
    // missing target under this workspace's own prefix is represented by one
    // indexed task of that prefix (never by the target itself), so its label
    // stays `missing` exactly as under the whole-workspace projection.
    let own_prefix = ids[0].split('-').next().expect("id prefix").to_string();
    let missing_own = format!("{own_prefix}-999999");
    let targets = [missing_own.clone(), "ZZZ-999999".to_string()]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let index = store
        .task_status_index_for(&store.workspace_id, &targets)
        .expect("missing-target status lookup");
    assert!(!index.contains_key(&missing_own));
    assert!(!index.contains_key("ZZZ-999999"));
    assert_eq!(index.len(), 1, "one representative only: {index:?}");
    let representative = index.keys().next().expect("representative");
    assert!(representative.starts_with(&format!("{own_prefix}-")));
}
