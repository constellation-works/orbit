//! Unit tests for the long-lived `EmbedWorker` path.

use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};

use crate::NoopEmbedder;
use crate::vector::{EmbedWorker, SOURCE_KIND_TASK, VectorStore};

fn task(id: &str, title: &str, description: &str) -> Task {
    Task {
        id: id.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        acceptance_criteria: vec!["Indexed after mutation".to_string()],
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

fn wait_until(timeout: Duration, mut ready: impl FnMut() -> bool) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out after {timeout:?} waiting for worker indexing");
}

#[test]
fn long_lived_worker_indexes_enqueued_task_into_vector_store() {
    let store = VectorStore::open_in_memory().expect("open in-memory vector store");
    let worker = EmbedWorker::start_with_embedder(store.clone(), Box::new(NoopEmbedder::small()));
    let task = task(
        "T-worker-1",
        "Index this mutation",
        "Observed through the vector store",
    );

    worker.enqueue(task.clone());

    wait_until(Duration::from_secs(2), || {
        store
            .source_ids(SOURCE_KIND_TASK)
            .expect("read source ids")
            .contains(&task.id)
    });

    let stats = store.stats(std::slice::from_ref(&task.id)).expect("stats");
    assert_eq!(stats.stale_rows, 0);
    assert!(
        stats
            .counts
            .iter()
            .any(|count| count.source_kind == SOURCE_KIND_TASK && count.model_id == "noop"),
        "expected noop embeddings for the enqueued task, got {stats:?}"
    );
}
