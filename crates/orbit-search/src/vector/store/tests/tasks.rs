//! Unit tests for `tasks` — sibling layout under store/tests/.

use std::sync::Mutex;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};

use crate::vector::{SOURCE_KIND_TASK, VectorStore};
use crate::{Embedder, NoopEmbedder};

fn task(id: &str, title: &str, description: &str) -> Task {
    Task {
        id: id.to_string(),
        title: title.to_string(),
        description: description.to_string(),
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
fn noop_task_indexing_populates_rows_without_companion() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    let task = task("T1", "Index this", "Task description");

    let report = store.index_task(&task, &embedder, false).unwrap();
    let stats = store.stats(&["T1".to_string()]).unwrap();

    assert!(report.embedded_chunks >= 3);
    assert_eq!(stats.stale_rows, 0);
    assert_eq!(stats.counts[0].source_kind, "task");
    assert_eq!(stats.counts[0].model_id, "noop");
}

#[test]
fn reindex_tasks_removes_legacy_field_rows_after_rename() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    store
        .upsert_embeddings(
            SOURCE_KIND_TASK,
            "T1",
            &[
                crate::vector::EmbeddingField::new("purpose", "old purpose"),
                crate::vector::EmbeddingField::new("summary", "old summary"),
                crate::vector::EmbeddingField::new("acceptance_criteria", "old acceptance"),
            ],
            &embedder,
            false,
        )
        .expect("prepopulate legacy rows");

    store
        .reindex_tasks(
            &[task("T1", "New title", "New description")],
            &embedder,
            false,
        )
        .expect("reindex task");

    let conn = store.connection();
    let conn = conn.lock().unwrap();
    let embeddings: i64 = conn
        .query_row(
            r#"
                    SELECT COUNT(*)
                    FROM embeddings
                    WHERE source_id = 'T1'
                      AND field IN ('purpose', 'summary', 'acceptance_criteria')
                "#,
            [],
            |row| row.get(0),
        )
        .unwrap();
    let fts: i64 = conn
        .query_row(
            r#"
                    SELECT COUNT(*)
                    FROM chunks
                    WHERE source_kind = 'task'
                      AND source_id = 'T1'
                      AND field IN ('purpose', 'summary', 'acceptance_criteria')
                "#,
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(embeddings, 0);
    assert_eq!(fts, 0);
}

#[test]
fn reindex_tasks_batches_chunks_across_sources_and_skips_unchanged_fields() {
    const TASKS: usize = 40;

    let store = VectorStore::open_in_memory().unwrap();
    let embedder = RecordingEmbedder::new();
    let tasks = (0..TASKS)
        .map(|index| {
            task(
                &format!("T{index:03}"),
                &format!("Title {index}"),
                &format!("Description {index}"),
            )
        })
        .collect::<Vec<_>>();

    let first = store.reindex_tasks(&tasks, &embedder, false).unwrap();
    assert_eq!(first.upsert.embedded_chunks, TASKS * 4);
    assert_eq!(first.upsert.skipped_fields, 0);
    assert_eq!(embedder.batch_sizes(), vec![64, 64, 32]);

    let second = store.reindex_tasks(&tasks, &embedder, false).unwrap();
    assert_eq!(second.upsert.embedded_chunks, 0);
    assert_eq!(second.upsert.skipped_fields, TASKS * 4);
    assert_eq!(
        embedder.batch_sizes(),
        vec![64, 64, 32],
        "unchanged fields must not issue embed RPCs"
    );
}

struct RecordingEmbedder {
    inner: NoopEmbedder,
    batch_sizes: Mutex<Vec<usize>>,
}

impl RecordingEmbedder {
    fn new() -> Self {
        Self {
            inner: NoopEmbedder::small(),
            batch_sizes: Mutex::new(Vec::new()),
        }
    }

    fn batch_sizes(&self) -> Vec<usize> {
        self.batch_sizes.lock().unwrap().clone()
    }
}

impl Embedder for RecordingEmbedder {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn max_input_tokens(&self) -> usize {
        self.inner.max_input_tokens()
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        self.batch_sizes.lock().unwrap().push(texts.len());
        self.inner.embed(texts)
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        self.inner.token_count(text)
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        self.inner.token_boundaries(text)
    }
}
