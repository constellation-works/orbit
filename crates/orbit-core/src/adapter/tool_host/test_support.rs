use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_store::contracts::TaskCreateParams;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::Value;
use tempfile::tempdir;

use crate::OrbitRuntime;

/// Run a tool with an explicit operator session context.
///
/// `OrbitRuntime::run_tool` carries no session context, so the ORB-10453
/// chokepoint resolves its caller from ambient process state and refuses a
/// governed operation. A test that exercises a governed tool's *domain*
/// behaviour rather than its authorization says which caller it is here,
/// instead of depending on whatever environment the test runner happens to
/// have.
pub(crate) fn run_tool_as_operator(
    runtime: &OrbitRuntime,
    name: &str,
    input: Value,
) -> Result<Value, OrbitError> {
    runtime.run_tool_with_context_and_role(
        name,
        input,
        Role::Admin,
        ToolContext {
            session_context: ToolSessionContext {
                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                ..ToolSessionContext::default()
            },
            ..ToolContext::default()
        },
    )
}

pub(crate) fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root)
}

pub(super) fn create_task(
    runtime: &OrbitRuntime,
    workspace_path: &Path,
    title: &str,
    description: &str,
    status: TaskStatus,
    context_files: &[&str],
) -> Task {
    create_task_with_crew(
        runtime,
        workspace_path,
        title,
        description,
        status,
        context_files,
        None,
    )
}

/// Create a task straight through the record store, keeping `crew` exactly as
/// given.
///
/// `orbit.task.add` validates the crew against this host's `[crews.*]` table,
/// so it cannot author the case read surfaces must tolerate: a stored crew this
/// host has no entry for, because the task predates a config edit or was
/// created on another machine (ORB-10968).
pub(super) fn create_task_with_crew(
    runtime: &OrbitRuntime,
    _workspace_path: &Path,
    title: &str,
    description: &str,
    status: TaskStatus,
    context_files: &[&str],
    crew: Option<&str>,
) -> Task {
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
            context_files: context_files
                .iter()
                .map(|path| (*path).to_string())
                .collect(),
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
            crew: crew.map(ToString::to_string),
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task")
}

pub(crate) fn create_context_task(
    runtime: &OrbitRuntime,
    workspace_path: &Path,
    status: TaskStatus,
    context_files: &[&str],
) -> Task {
    create_task(
        runtime,
        workspace_path,
        "test task",
        "test",
        status,
        context_files,
    )
}

pub(crate) fn invalid_input_message<T>(result: Result<T, OrbitError>) -> String {
    match result {
        Err(OrbitError::InvalidInput(message)) => message,
        Err(error) => panic!("expected invalid input, got {error:?}"),
        Ok(_) => panic!("expected invalid input"),
    }
}

/// Every variable an `orbit-engine` managed run exports that a tool-host test
/// must state rather than inherit.
const TOOL_ENV: [&str; 8] = [
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_TASK_ID",
    "ORBIT_ACTIVE_TASK_ID",
    "ORBIT_AGENT_NAME",
    "ORBIT_AGENT_MODEL",
    "ORBIT_RUN_ID",
    "ORBIT_ACTIVITY_ID",
    "ORBIT_STEP_INDEX",
];

/// Clear the managed-run envelope for the guard's lifetime.
///
/// Delegates to `orbit_common::test_env` rather than holding a private lock:
/// the managed and unmanaged tool-host tests mutate the same process
/// environment, so they have to serialize against *each other* and against the
/// `test_env::unset` callers elsewhere in this crate. Two independent mutexes
/// would let a managed-env test publish `ORBIT_RUN_ID` while a sibling was
/// asserting its absence (ORB-10540).
pub(crate) fn unmanaged_tool_env_guard() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::unset(TOOL_ENV)
}

/// Populate the exact envelope `orbit-engine` exports into a managed run's
/// activity processes, leaving every other tool-env variable cleared.
///
/// This is the production input to `trusted_env_run_id`: the marker
/// authenticates the envelope and `ORBIT_RUN_ID` carries the run. A test that
/// hand-builds a host reporting a run id skips exactly this step.
pub(crate) fn managed_tool_env_guard(run_id: &str) -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped(TOOL_ENV.into_iter().map(|name| match name {
        "ORBIT_MANAGED_RUN_CONTEXT" => (name, Some("1")),
        "ORBIT_RUN_ID" => (name, Some(run_id)),
        _ => (name, None),
    }))
}
