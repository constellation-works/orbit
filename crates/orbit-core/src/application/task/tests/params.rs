use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};

use super::super::params::{TaskAddParams, TaskRecordUpdateParams, TaskUpdateParams};

#[test]
fn task_add_params_preserve_workspace_routing_and_defaults() {
    let defaults = TaskAddParams::default();
    assert_eq!(defaults.workspace_path, None);
    assert_eq!(defaults.priority, TaskPriority::Medium);
    assert_eq!(defaults.complexity, TaskComplexity::Unassessed);
    assert!(defaults.acceptance_criteria.is_empty());

    let params = TaskAddParams {
        title: "Workspace-scoped task".to_string(),
        workspace_path: Some("packages/orbit".to_string()),
        ..defaults
    };
    assert_eq!(params.title, "Workspace-scoped task");
    assert_eq!(params.workspace_path.as_deref(), Some("packages/orbit"));
}

#[test]
fn task_update_params_route_document_history_and_artifact_fields() {
    let params = TaskUpdateParams {
        title: Some("Updated title".to_string()),
        description: Some("Updated description".to_string()),
        acceptance_criteria: Some(vec!["criterion".to_string()]),
        dependencies: Some(vec!["ORB-00042".to_string()]),
        tags: Some(vec!["focused".to_string()]),
        plan: Some("1. Update".to_string()),
        execution_summary: Some("Verified".to_string()),
        status: Some(TaskStatus::Review),
        priority: Some(TaskPriority::High),
        complexity: Some(TaskComplexity::Medium),
        task_type: Some(TaskType::Refactor),
        planned_by: Some(Some("codex".to_string())),
        implemented_by: Some(None),
        pr_status: Some(Some("approved".to_string())),
        job_run_id: Some(Some("jrun-123".to_string())),
        crew: Some(Some("implementer".to_string())),
        orchestrator: Some(None),
        context_files: Some(vec!["file:src/lib.rs".to_string()]),
        upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text(
            "report.txt",
            "verified",
        )],
        ..Default::default()
    };

    let record = TaskRecordUpdateParams::from(params);
    assert_eq!(record.title.as_deref(), Some("Updated title"));
    assert_eq!(record.dependencies, Some(vec!["ORB-00042".to_string()]));
    assert_eq!(record.status, Some(TaskStatus::Review));
    assert_eq!(record.priority, Some(TaskPriority::High));
    assert_eq!(record.complexity, Some(TaskComplexity::Medium));
    assert_eq!(record.task_type, Some(TaskType::Refactor));
    assert_eq!(record.planned_by, Some(Some("codex".to_string())));
    assert_eq!(record.implemented_by, Some(None));
    assert_eq!(record.pr_status, Some(Some("approved".to_string())));
    assert_eq!(record.job_run_id, Some(Some("jrun-123".to_string())));
    assert_eq!(record.crew, Some(Some("implementer".to_string())));
    assert_eq!(record.orchestrator, Some(None));
    assert!(record.has_document_changes());
    assert!(record.has_history_changes());
    assert!(record.has_artifact_changes());
    assert_eq!(record.upsert_artifacts[0].text_content(), Some("verified"));
}

#[test]
fn empty_task_update_routes_to_no_record_changes() {
    let params = TaskUpdateParams::default();

    let record = TaskRecordUpdateParams::from(params);
    assert!(!record.has_document_changes());
    assert!(!record.has_history_changes());
    assert!(!record.has_artifact_changes());
}
