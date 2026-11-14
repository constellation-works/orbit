//! Hydrated row-cache reuse through the composed public task backends.
//!
//! The same store instance serves every page, so the second identical listing
//! reuses its remembered rows and a single document update invalidates only
//! the changed row. Bundle reads are observed through the public
//! `task_bundle_lightweight_read` tracing span, not a private counter.

use super::*;
use orbit_store::contracts::{
    TaskDocumentUpdateParams, TaskListFilter, TaskListHydration, TaskPage,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

/// Counts `task_bundle_lightweight_read` spans opened on this thread.
struct LightweightReads(std::sync::Arc<AtomicUsize>);

impl<S: tracing::Subscriber> Layer<S> for LightweightReads {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: Context<'_, S>,
    ) {
        if attrs.metadata().name() == "task_bundle_lightweight_read" {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn full_page(store: &WorkspaceTaskBackends) -> (TaskPage, usize) {
    let reads = std::sync::Arc::new(AtomicUsize::new(0));
    let subscriber = Registry::default().with(LightweightReads(std::sync::Arc::clone(&reads)));
    let page = tracing::subscriber::with_default(subscriber, || {
        store
            .task
            .query_task_rows(
                &TaskListFilter::default(),
                50,
                None,
                TaskListHydration::Full,
            )
            .expect("full page")
    });
    (page, reads.load(Ordering::Relaxed))
}

#[test]
fn hydrated_page_reuses_unchanged_rows_and_rereads_only_the_changed_one() {
    if !support::isolated(
        "row_cache::hydrated_page_reuses_unchanged_rows_and_rereads_only_the_changed_one",
    ) {
        return;
    }
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let mut ids = Vec::new();
    for index in 0..3 {
        let task = store
            .task
            .create_task(create_params(&format!("Task {index}"), TaskStatus::Backlog))
            .expect("create task");
        ids.push(task.id);
    }

    let (first, first_reads) = full_page(&store);
    assert_eq!(first.items.len(), 3);
    assert_eq!(
        first_reads, 3,
        "first hydrated page reads each selected bundle once"
    );

    let (second, second_reads) = full_page(&store);
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
        .document
        .update_task_document(
            &ids[0],
            TaskDocumentUpdateParams {
                actor: "operator".to_string(),
                title: Some("Task 0 renamed".to_string()),
                ..Default::default()
            },
        )
        .expect("update one task");

    let (third, third_reads) = full_page(&store);
    assert_eq!(third_reads, 1, "only the updated task's bundle is re-read");
    assert_eq!(third.items.len(), 3);
    let renamed = third
        .items
        .iter()
        .find(|row| row.task.id == ids[0])
        .expect("renamed row");
    assert_eq!(renamed.task.title, "Task 0 renamed");
}
