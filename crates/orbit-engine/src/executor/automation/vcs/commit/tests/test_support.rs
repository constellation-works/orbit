use chrono::Utc;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};

pub fn task_with_file(id: &str, title: &str, path: &str, implemented_by: &str) -> Task {
    let now = Utc::now();
    Task {
        job_run_machine: None,
        id: id.to_string(),
        title: title.to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        // ORB-10313: the delivery gate reads the durable outcome before touching
        // the checkout.
        execution_summary: "Outcome: success".to_string(),
        context_files: vec![format!("file:{path}")],
        created_by: None,
        planned_by: None,
        implemented_by: Some(implemented_by.to_string()),
        status: TaskStatus::InProgress,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: Some("batch-1".to_string()),
        crew: None,
        crew_source: None,
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}
