//! Unit tests for `task_fields` — sibling layout under vector/tests/.

use chrono::Utc;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};

use super::super::task_fields::task_embedding_fields;

fn task() -> Task {
    Task {
        job_run_machine: None,
        id: "ORB-00000".to_string(),
        title: "Index this".to_string(),
        description: "Task description".to_string(),
        acceptance_criteria: vec!["First criterion".to_string()],
        required_tools: Vec::new(),
        plan: "Plan body".to_string(),
        execution_summary: "Summary body".to_string(),
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
fn task_embedding_fields_use_v2_document_names() {
    let field_names = task_embedding_fields(&task())
        .into_iter()
        .map(|field| field.field)
        .collect::<Vec<_>>();

    assert_eq!(
        field_names,
        vec![
            "title",
            "description",
            "plan",
            "execution_summary",
            "acceptance",
        ]
    );
}
