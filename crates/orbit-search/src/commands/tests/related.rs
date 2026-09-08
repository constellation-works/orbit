//! Unit tests for `related` — sibling layout under commands/tests/.

use super::super::related::{SemanticRelatedParams, run_with_embedder};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};

use crate::Embedder;
use crate::vector::VectorStore;

#[derive(Default)]
struct KeywordEmbedder;

impl Embedder for KeywordEmbedder {
    fn model_id(&self) -> &str {
        "keyword"
    }

    fn dim(&self) -> usize {
        3
    }

    fn max_input_tokens(&self) -> usize {
        512
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        Ok(texts.iter().map(|text| vector_for(text)).collect())
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        Ok(text.split_whitespace().count().max(1))
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        Ok(text
            .match_indices(char::is_whitespace)
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
            .collect())
    }
}

fn vector_for(text: &str) -> Vec<f32> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("semantic") {
        vec![1.0, 0.0, 0.0]
    } else {
        vec![0.0, 1.0, 0.0]
    }
}

fn task(id: &str, title: &str, description: &str) -> Task {
    Task {
        id: id.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
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
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn related_excludes_self_and_uses_cosine_only() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = KeywordEmbedder;
    let tasks = vec![
        task("T1", "semantic search", "design"),
        task("T2", "semantic related", "retrieval"),
        task("T3", "billing", "invoices"),
    ];
    for task in &tasks {
        store.index_task(task, &embedder, false).unwrap();
    }

    let result = run_with_embedder(
        &store,
        &tasks,
        &embedder,
        SemanticRelatedParams {
            task_id: "T1".to_string(),
            limit: 3,
            model: None,
        },
    )
    .unwrap();

    assert!(!result.results.iter().any(|hit| hit.source_id == "T1"));
    assert_eq!(result.results[0].source_id, "T2");
    assert!(result.results[0].score_breakdown.cosine_rank.is_some());
    assert!(result.results[0].score_breakdown.bm25_rank.is_none());
    assert!(result.results[0].score_breakdown.rrf.is_none());
}
