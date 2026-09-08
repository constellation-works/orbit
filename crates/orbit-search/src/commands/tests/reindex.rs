//! Unit tests for `reindex` — sibling layout under commands/tests/.

use std::str::FromStr;

use super::super::reindex::{
    IndexKind, SemanticIndexParams, SemanticIndexResult, SemanticReindexResult, run_with_embedder,
};

use chrono::Utc;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use serde_json::json;

use crate::NoopEmbedder;
use crate::vector::{UpsertReport, VectorStore};

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
fn semantic_index_params_serde_defaults_to_tasks_at_runtime() {
    let empty: SemanticIndexParams = serde_json::from_str("{}").unwrap();
    assert_eq!(empty.kind, None);
    assert_eq!(empty.resolved_kind(), IndexKind::Tasks);

    let model_only: SemanticIndexParams = serde_json::from_str(r#"{"model":"bge-small"}"#).unwrap();
    assert_eq!(model_only.model.as_deref(), Some("bge-small"));
    assert_eq!(model_only.kind, None);
    assert_eq!(model_only.resolved_kind(), IndexKind::Tasks);

    let docs: SemanticIndexParams = serde_json::from_str(r#"{"kind":"docs"}"#).unwrap();
    assert_eq!(docs.kind, Some(IndexKind::Docs));
    assert_eq!(docs.resolved_kind(), IndexKind::Docs);

    let all: SemanticIndexParams = serde_json::from_str(r#"{"kind":"all"}"#).unwrap();
    assert_eq!(all.kind, Some(IndexKind::All));
    assert_eq!(all.resolved_kind(), IndexKind::All);
}

#[test]
fn semantic_index_kind_rejects_singular_learning() {
    let error = IndexKind::from_str("learning").expect_err("singular kind should fail");

    assert!(error.to_string().contains("`learning`"));
    assert!(error.to_string().contains("tasks, docs, all"));
}

#[test]
fn tasks_variant_serializes_flat_like_the_task_index_result() {
    let result = SemanticIndexResult::Tasks {
        model_id: "bge-small-en-v1.5".to_string(),
        report: UpsertReport {
            embedded_chunks: 7,
            skipped_fields: 2,
        },
        stale_sources: vec!["T9".to_string()],
    };

    let expected = json!({
            "model_id": "bge-small-en-v1.5",
            "report": {
                "embedded_chunks": 7,
                "skipped_fields": 2
            },
            "stale_sources": ["T9"]
    });
    assert_eq!(serde_json::to_value(&result).unwrap(), expected);
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"model_id":"bge-small-en-v1.5","report":{"embedded_chunks":7,"skipped_fields":2},"stale_sources":["T9"]}"#
    );
}

/// A task deleted while this index was unwritable leaves rows that only the
/// reindex sweep can clear, so the run must both drop them and say so.
#[test]
fn reindex_clears_rows_left_by_a_task_that_is_no_longer_live() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    let indexed = vec![
        task("T1", "still here", "live task"),
        task("T2", "deleted elsewhere", "removed task"),
    ];
    run_with_embedder(&store, &indexed, &embedder, false).expect("index both tasks");

    let result: SemanticReindexResult =
        run_with_embedder(&store, &indexed[..1], &embedder, false).expect("reindex live corpus");
    let stats = store.stats(&["T1".to_string()]).unwrap();

    assert_eq!(result.stale_sources, vec!["T2".to_string()]);
    assert_eq!(stats.stale_rows, 0);
}
