//! Sibling tests for `tool_exec.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use crate::OrbitRuntime;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;

use orbit_store::contracts::TaskCreateParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use serde_json::json;

#[test]
fn run_tool_context_allowlist_honors_task_wildcard() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: "Wildcard task".to_string(),
            description: "Exercise wildcard runtime allowlist".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            workspace_path: Some(runtime.paths().repo_root.to_string_lossy().into_owned()),
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
        })
        .expect("create task");

    let output = runtime
        .run_tool_with_context_and_role(
            "orbit.task.show",
            json!({ "id": task.id.clone() }),
            Role::Admin,
            ToolContext {
                allowed_tools: vec!["orbit.task.*".to_string()],
                orbit_host: Some(crate::adapter::tool_host::build_orbit_tool_host(
                    &runtime,
                    Some(task.id.clone()),
                    None,
                    orbit_types::tool::ToolSessionContext::default(),
                )),
                ..Default::default()
            },
        )
        .expect("wildcard activity context should permit orbit.task.show");

    assert_eq!(output["id"], task.id);
}
