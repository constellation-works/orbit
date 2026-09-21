use orbit_search::{ScoreBreakdown, SemanticHit};
use orbit_store::contracts::TaskCreateParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};

use super::*;
use crate::OrbitRuntime;

mod federated;
mod global;
mod hybrid;
mod path_match;
mod types;

fn add_task_with_status(runtime: &OrbitRuntime, title: &str, status: TaskStatus) -> String {
    add_task(runtime, title, "needle task body", status)
}

fn add_task(runtime: &OrbitRuntime, title: &str, description: &str, status: TaskStatus) -> String {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: title.to_string(),
            description: description.to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task")
        .id
}

fn seed_search_fixture(runtime: &OrbitRuntime, query: &str, task_count: usize) {
    for index in 0..task_count {
        add_task_with_status(
            runtime,
            &format!("{query} task {index:02}"),
            TaskStatus::Backlog,
        );
    }
}

fn task_semantic_hit(id: &str, score: f32) -> SemanticHit {
    SemanticHit {
        source_kind: "task".to_string(),
        source_id: id.to_string(),
        best_field: "title".to_string(),
        snippet: "semantic task snippet".to_string(),
        score,
        score_breakdown: ScoreBreakdown {
            rrf: Some(score),
            bm25_rank: Some(2),
            cosine_rank: Some(1),
        },
    }
}

fn with_task_semantic_override<T>(
    result: Result<Vec<SemanticHit>, orbit_common::OrbitError>,
    f: impl FnOnce() -> T,
) -> T {
    TASK_SEMANTIC_SEARCH_OVERRIDE.with(|cell| {
        *cell.borrow_mut() = Some(result);
    });
    let out = f();
    TASK_SEMANTIC_SEARCH_OVERRIDE.with(|cell| {
        *cell.borrow_mut() = None;
    });
    out
}
