use orbit_store::contracts::TaskCreateParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};

use super::*;
use crate::OrbitRuntime;

mod federated;
mod global;
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
