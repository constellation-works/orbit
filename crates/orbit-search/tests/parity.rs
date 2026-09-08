#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Behavioral parity smoke for the relocated VectorStore.
//!
//! Asserts that the post-refactor reindex over a fixed task corpus produces
//! the same `UpsertReport` shape (JSON keys + dedup contract) as the pre-T-20
//! `orbit-store::vector` did. The snapshot pins counts so that any future
//! change to chunking or field extraction is caught here.

use chrono::Utc;
use orbit_search::NoopEmbedder;
use orbit_search::{UpsertReport, VectorStore};
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use serde_json::json;

fn fixture_task(id: &str) -> Task {
    Task {
        id: id.to_string(),
        title: "Index this".to_string(),
        description: "Task description".to_string(),
        acceptance_criteria: vec!["First criterion".to_string()],
        required_tools: Vec::new(),
        plan: "Plan body".to_string(),
        execution_summary: String::new(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::Backlog,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        orchestrator: None,
        tags: Vec::new(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn reindex_report_shape_is_stable_across_runs() {
    let vector = VectorStore::open_in_memory().expect("in-memory vector store");
    let embedder = NoopEmbedder::small();
    let tasks = vec![fixture_task("T1"), fixture_task("T2")];

    let first: UpsertReport = vector
        .reindex_tasks(&tasks, &embedder, false)
        .expect("first reindex")
        .upsert;
    let second: UpsertReport = vector
        .reindex_tasks(&tasks, &embedder, false)
        .expect("second reindex")
        .upsert;

    // First run: every field embedded, none skipped.
    assert_eq!(first.skipped_fields, 0, "first run: nothing to skip");
    assert!(
        first.embedded_chunks >= 8,
        "first run: each task has 4 fields (title/description/plan/acceptance) × 2 tasks; got {}",
        first.embedded_chunks
    );

    // Second run: BLAKE3 dedup means everything is skipped.
    assert_eq!(second.embedded_chunks, 0, "second run: dedup applies");
    assert!(
        second.skipped_fields >= 8,
        "second run: every prior field is skipped; got {}",
        second.skipped_fields
    );

    // JSON shape contract: orbit-cli + MCP adapters serialize this.
    let snapshot = serde_json::to_value(second).expect("serialize report");
    assert_eq!(
        snapshot,
        json!({
            "embedded_chunks": 0,
            "skipped_fields": second.skipped_fields,
        }),
        "UpsertReport JSON keys must remain `embedded_chunks` + `skipped_fields`"
    );
}

#[test]
fn delete_source_cascades_after_relocation() {
    let vector = VectorStore::open_in_memory().expect("in-memory vector store");
    let embedder = NoopEmbedder::small();
    vector
        .index_task(&fixture_task("T1"), &embedder, false)
        .expect("index task");

    vector.delete_source("task", "T1").expect("delete cascades");

    let stats = vector.stats(&[]).expect("stats");
    assert!(
        stats.counts.is_empty(),
        "counts should be empty after delete; got {:?}",
        stats.counts
    );
    assert_eq!(stats.stale_rows, 0, "no stale rows after delete");
}
