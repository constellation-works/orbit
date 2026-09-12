use orbit_engine::{DispatchError, RuntimeHost};
use orbit_store::contracts::TaskCreateParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};

use crate::OrbitRuntime;

// These tests exercise admission of legacy persisted tasks, so they bypass
// the current creation boundary to retain deliberately invalid requirements.
fn add_task(runtime: &OrbitRuntime, title: &str, required_tools: &[&str]) -> String {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: title.to_string(),
            description: "Exercise task-scoped activity tools.".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: required_tools
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
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
        .expect("add task")
        .id
}

#[test]
fn activity_tools_are_the_deterministic_union_of_baseline_and_task_requirements() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let baseline = vec![
        "proc.spawn".to_string(),
        "orbit.task.show".to_string(),
        "proc.spawn".to_string(),
    ];
    let empty_id = add_task(&runtime, "Baseline only", &[]);
    let empty =
        RuntimeHost::resolve_activity_tools(&runtime, std::slice::from_ref(&empty_id), &baseline)
            .expect("resolve empty requirements");
    assert!(empty.requested_tools.is_empty());
    assert_eq!(empty.effective_tools, baseline);

    let required_id = add_task(
        &runtime,
        "Add GitHub reads",
        &["github.run.list", "github.auth.status", "github.run.list"],
    );
    let second_id = add_task(&runtime, "Add another GitHub read", &["github.run.view"]);
    let resolved =
        RuntimeHost::resolve_activity_tools(&runtime, &[required_id, second_id], &baseline)
            .expect("resolve required tools across every selected task");
    assert_eq!(
        resolved.requested_tools,
        vec!["github.auth.status", "github.run.list", "github.run.view"]
    );
    assert_eq!(
        resolved.effective_tools,
        vec![
            "proc.spawn",
            "orbit.task.show",
            "github.auth.status",
            "github.run.list",
            "github.run.view",
        ]
    );
}

#[test]
fn invalid_task_requirements_fail_structured_admission_before_dispatch() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    for (title, tool, expected_reason) in [
        (
            "Unknown",
            "github.does_not_exist",
            "unknown registered tool",
        ),
        ("Wildcard", "github.*", "wildcard and prefix"),
        ("Malformed", " github.run.list", "malformed canonical"),
        ("Human only", "orbit.auto_task.add", "not agent-facing"),
    ] {
        let task_id = add_task(&runtime, title, &[tool]);
        let error =
            RuntimeHost::resolve_activity_tools(&runtime, std::slice::from_ref(&task_id), &[])
                .expect_err("requirement must fail admission");
        match error {
            DispatchError::RequiredToolAdmission {
                task_id: actual_task_id,
                tool_name,
                reason,
            } => {
                assert_eq!(actual_task_id, task_id);
                assert_eq!(tool_name, tool);
                assert!(reason.contains(expected_reason), "{reason}");
            }
            other => panic!("unexpected admission error: {other}"),
        }
    }

    runtime
        .disable_tool("github.run.list")
        .expect("disable registered tool");
    let task_id = add_task(&runtime, "Disabled", &["github.run.list"]);
    let error = RuntimeHost::resolve_activity_tools(&runtime, std::slice::from_ref(&task_id), &[])
        .expect_err("disabled requirement must fail admission");
    assert!(matches!(
        error,
        DispatchError::RequiredToolAdmission {
            task_id: actual_task_id,
            tool_name,
            reason,
        } if actual_task_id == task_id
            && tool_name == "github.run.list"
            && reason.contains("inactive")
    ));
}
