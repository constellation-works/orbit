//! Unit tests for `search` — sibling layout under commands/tests/.

use super::super::search::{SemanticSearchParams, run_with_embedder};

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
    if lower.contains("semantic design") {
        vec![1.0, 0.0, 0.0]
    } else if lower.contains("semantic") {
        vec![0.8, 0.2, 0.0]
    } else {
        vec![0.0, 1.0, 0.0]
    }
}

fn task(id: &str, title: &str, description: &str, plan: &str) -> Task {
    Task {
        id: id.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: plan.to_string(),
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
fn search_runs_both_retrievers_and_rolls_up_fields() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = KeywordEmbedder;
    store
        .index_task(
            &task(
                "T1",
                "semantic design",
                "semantic notes",
                "semantic design appears again in plan",
            ),
            &embedder,
            false,
        )
        .unwrap();
    store
        .index_task(
            &task("T2", "unrelated", "other text", "other plan"),
            &embedder,
            false,
        )
        .unwrap();

    let result = run_with_embedder(
        &store,
        &embedder,
        SemanticSearchParams {
            query: "semantic design".to_string(),
            limit: 10,
            field: None,
            kind: Some("task".to_string()),
            model: None,
        },
    )
    .unwrap();

    let t1_hits = result
        .results
        .iter()
        .filter(|hit| hit.source_id == "T1")
        .collect::<Vec<_>>();
    assert_eq!(t1_hits.len(), 1);
    let breakdown = &t1_hits[0].score_breakdown;
    assert!(breakdown.rrf.is_some());
    assert!(breakdown.bm25_rank.is_some());
    assert!(breakdown.cosine_rank.is_some());
}

#[test]
fn field_filter_retrieves_title_hits_before_fusion() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = KeywordEmbedder;

    store
        .index_task(
            &task("BEST", "semantic design", "unrelated", "unrelated"),
            &embedder,
            false,
        )
        .unwrap();
    for index in 0..50 {
        store
            .index_task(
                &task(
                    &format!("WEAK-{index}"),
                    "semantic notes",
                    "semantic design",
                    "unrelated",
                ),
                &embedder,
                false,
            )
            .unwrap();
    }

    let result = run_with_embedder(
        &store,
        &embedder,
        SemanticSearchParams {
            query: "semantic design".to_string(),
            limit: 1,
            field: Some("title".to_string()),
            kind: Some("task".to_string()),
            model: None,
        },
    )
    .unwrap();

    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].source_id, "BEST");
    assert_eq!(result.results[0].best_field, "title");
}
