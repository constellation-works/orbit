//! Compact (`EnvelopeOnly`) listing pages: no bundle reads, and the same
//! envelope-derived fields a hydrated page carries.

use super::*;
use crate::contracts::{TaskDocumentUpdateParams, TaskListFilter, TaskListHydration};
use crate::repository::task::v2_bundle::{LIGHTWEIGHT_BUNDLE_READS, LISTING_BUNDLE_READS};

fn bundle_reads<T>(op: impl FnOnce() -> T) -> (T, u64) {
    let before = LISTING_BUNDLE_READS.with(std::cell::Cell::get);
    let value = op();
    (
        value,
        LISTING_BUNDLE_READS.with(std::cell::Cell::get) - before,
    )
}

fn lightweight_reads<T>(op: impl FnOnce() -> T) -> (T, u64) {
    let before = LIGHTWEIGHT_BUNDLE_READS.with(std::cell::Cell::get);
    let value = op();
    (
        value,
        LIGHTWEIGHT_BUNDLE_READS.with(std::cell::Cell::get) - before,
    )
}

fn full_page(store: &TaskV2Store) -> crate::contracts::TaskPage {
    store
        .query_task_rows(
            &TaskListFilter::default(),
            50,
            None,
            TaskListHydration::Full,
        )
        .expect("full page")
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
fn hydrated_page_reuses_unchanged_rows_and_rereads_only_the_changed_one() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let mut ids = Vec::new();
    for index in 0..3 {
        let task = store
            .create_task(create_params(&format!("Task {index}"), TaskStatus::Backlog))
            .expect("create task");
        ids.push(task.id.to_string());
    }

    let (first, first_reads) = lightweight_reads(|| full_page(&store));
    assert_eq!(first.items.len(), 3);
    assert_eq!(
        first_reads, 3,
        "first hydrated page reads each selected bundle once"
    );

    let (second, second_reads) = lightweight_reads(|| full_page(&store));
    assert_eq!(
        second_reads, 0,
        "unchanged page must perform 0 lightweight bundle reads"
    );
    assert_eq!(second.total, first.total);
    for (cached, fresh) in second.items.iter().zip(&first.items) {
        assert_eq!(cached.task, fresh.task);
        assert_eq!(cached.comments, fresh.comments);
        assert_eq!(cached.history, fresh.history);
        assert_eq!(cached.artifacts, fresh.artifacts);
    }

    store
        .update_task_document(
            &ids[0],
            &TaskDocumentUpdateParams {
                actor: "operator".to_string(),
                title: Some("Task 0 renamed".to_string()),
                ..Default::default()
            },
        )
        .expect("update one task");

    let (third, third_reads) = lightweight_reads(|| full_page(&store));
    assert_eq!(third_reads, 1, "only the updated task's bundle is re-read");
    assert_eq!(third.items.len(), 3);
    let renamed = third
        .items
        .iter()
        .find(|row| row.task.id == ids[0])
        .expect("renamed row");
    assert_eq!(renamed.task.title, "Task 0 renamed");
}
