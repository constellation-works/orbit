//! Sibling tests for `runtime/authorization.rs` — the capability chokepoint
//! [ORB-10453].
//!
//! These assert only on outcomes that a session context determines, never on
//! outcomes that ambient process state determines. Session grants outrank every
//! process signal, so a test that supplies them is deterministic whether it runs
//! under CI, under a managed Orbit run, or from a developer's terminal. The
//! process-signal precedence rules themselves are exercised as pure data in
//! `orbit_common::governance::authorization`.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_store::contracts::TaskCreateParams;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::json;

use crate::OrbitRuntime;

fn context_with(capabilities: [McpCapability; 1]) -> ToolContext {
    ToolContext {
        session_context: ToolSessionContext {
            effective_capabilities: BTreeSet::from(capabilities),
            ..ToolSessionContext::default()
        },
        ..ToolContext::default()
    }
}

fn seed_task(runtime: &OrbitRuntime) -> String {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: "Governed operation fixture".to_string(),
            description: "Exercise the capability chokepoint".to_string(),
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
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
            context_creation: Vec::new(),
        })
        .expect("create task")
        .id
}

#[test]
fn an_agent_session_cannot_delete_a_task_through_any_tool_path() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task_id = seed_task(&runtime);

    let error = runtime
        .run_tool_with_context_and_role(
            "orbit.task.delete",
            json!({ "id": task_id, "force": true }),
            Role::Admin,
            context_with([McpCapability::Agent]),
        )
        .expect_err("agent capability must not reach task deletion");

    match error {
        OrbitError::CapabilityDenied(message) => {
            assert!(message.contains("orbit.task.delete"), "{message}");
            assert!(message.contains("operator"), "{message}");
            assert!(message.contains("ORBIT_OPERATOR"), "{message}");
        }
        other => panic!("expected a capability denial, got: {other}"),
    }

    // The refusal is real, not cosmetic: the task is still there.
    assert!(runtime.get_task(&task_id).is_ok());
}
