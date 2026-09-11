use chrono::Utc;
use orbit_store::contracts::TaskCreateParams;
use orbit_types::task::{
    TaskArtifact, TaskComment, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType,
};

use super::super::params::TaskRecordUpdateParams;
use super::test_runtime;

fn create_params(_runtime: &crate::OrbitRuntime) -> TaskCreateParams {
    TaskCreateParams {
        actor: "test".to_string(),
        parent_id: None,
        title: "Stored task".to_string(),
        description: "Initial description".to_string(),
        acceptance_criteria: vec!["initial criterion".to_string()],
        dependencies: Vec::new(),
        relations: Vec::new(),
        tags: vec!["initial".to_string()],
        required_tools: Vec::new(),
        plan: "Initial plan".to_string(),
        execution_summary: String::new(),
        context_files: vec!["file:src/lib.rs".to_string()],
        repo_root: None,
        created_by: Some("test".to_string()),
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::Backlog,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        external_refs: Vec::new(),
        source_task_id: None,
        crew: None,
        orchestrator: None,
        comments: Vec::new(),
    }
}

#[test]
fn task_records_round_trip_document_history_and_artifact_updates() {
    let (_root, runtime) = test_runtime();
    let service = runtime.stores().task_records();
    let task = service
        .create(create_params(&runtime))
        .expect("create task record");

    let updated = service
        .update(
            &task.id,
            TaskRecordUpdateParams {
                actor: "codex".to_string(),
                title: Some("Updated title".to_string()),
                description: Some("Updated description".to_string()),
                tags: Some(vec!["updated".to_string()]),
                priority: Some(TaskPriority::High),
                status: Some(TaskStatus::Review),
                status_event: Some("status_changed".to_string()),
                status_note: Some("Ready for review".to_string()),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "codex".to_string(),
                    event: "verified".to_string(),
                    note: Some("record conversion".to_string()),
                    from_status: None,
                    to_status: None,
                }],
                append_comments: vec![TaskComment {
                    at: Utc::now(),
                    by: "codex".to_string(),
                    message: "Stored comment".to_string(),
                }],
                upsert_artifacts: vec![TaskArtifact::from_text("reports/result.txt", "passed")],
                ..Default::default()
            },
        )
        .expect("update task record");

    assert_eq!(updated.title, "Updated title");
    assert_eq!(updated.description, "Updated description");
    assert_eq!(updated.tags, vec!["updated"]);
    assert_eq!(updated.priority, TaskPriority::High);
    assert_eq!(updated.status, TaskStatus::Review);

    let persisted = runtime.get_task(&task.id).expect("read persisted task");
    assert_eq!(persisted.title, "Updated title");
    assert!(
        runtime
            .get_task_history(&task.id)
            .expect("read task history")
            .iter()
            .any(|entry| entry.event == "verified"
                && entry.note.as_deref() == Some("record conversion"))
    );
    assert!(
        runtime
            .get_task_comments(&task.id)
            .expect("read task comments")
            .iter()
            .any(|comment| comment.message == "Stored comment")
    );
    assert_eq!(
        runtime
            .get_task_artifacts(&task.id)
            .expect("read task artifacts")
            .iter()
            .find(|artifact| artifact.path == "reports/result.txt")
            .and_then(TaskArtifact::text_content),
        Some("passed")
    );
}

#[test]
fn task_records_delete_reports_presence_and_removes_persisted_task() {
    let (_root, runtime) = test_runtime();
    let service = runtime.stores().task_records();
    let task = service
        .create(create_params(&runtime))
        .expect("create task record");

    assert!(service.delete(&task.id).expect("delete existing task"));
    assert!(!service.delete(&task.id).expect("delete missing task"));
    let error = runtime
        .get_task(&task.id)
        .expect_err("deleted task should not be readable");
    assert!(error.to_string().contains(&task.id), "{error}");
}
