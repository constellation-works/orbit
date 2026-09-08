use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use orbit_store::maintenance::task_registry::read_workspace_config;
use orbit_types::task::{TASK_SHOW_PUBLIC_DTO_FIELDS, TaskStatus};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use super::super::json::task_to_json;
use super::super::test_support::{
    create_task, create_task_with_crew, invalid_input_message, run_tool_as_operator, test_runtime,
};
use crate::adapter::command::ToolEntryPoint;

struct CurrentDirGuard {
    _guard: MutexGuard<'static, ()>,
    previous: PathBuf,
}

impl CurrentDirGuard {
    fn enter(path: &Path) -> Self {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::current_dir().expect("capture cwd");
        std::env::set_current_dir(path).expect("enter test cwd");
        Self {
            _guard: guard,
            previous,
        }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.previous).expect("restore cwd");
    }
}

fn assert_task_titles(output: &Value, expected: &[&str]) {
    let mut titles = output
        .as_array()
        .expect("tool returns task array")
        .iter()
        .map(|task| {
            task.get("title")
                .and_then(Value::as_str)
                .expect("task title")
                .to_string()
        })
        .collect::<Vec<_>>();
    titles.sort();

    let mut expected = expected
        .iter()
        .map(|title| (*title).to_string())
        .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(titles, expected);
}

#[test]
fn execute_tool_command_searches_tasks_for_agents_via_orbit_search() {
    // ORB-00202: `orbit.task.search` was deleted in phase 2; the substring
    // case migrates to `orbit.search --kind task`.
    let (_root, runtime, repo_root) = test_runtime();
    let title_match = create_task(
        &runtime,
        &repo_root,
        "Fix search surface",
        "Wire the tool through Orbit.",
        TaskStatus::Backlog,
        &[],
    );
    let description_match = create_task(
        &runtime,
        &repo_root,
        "Refactor task queries",
        "Preserve SEARCH parity for agents.",
        TaskStatus::Review,
        &[],
    );
    create_task(
        &runtime,
        &repo_root,
        "Unrelated maintenance",
        "Nothing to see here.",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.search",
            json!({ "query": "sEaRcH", "kind": "task" }),
            Some("codex".to_string()),
            Some("gpt-5.4".to_string()),
        )
        .expect("search tool succeeds");

    let matches = output["results"].as_array().expect("results array");
    let ids = matches
        .iter()
        .filter_map(|task| task.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>();

    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&title_match.id.as_str()));
    assert!(ids.contains(&description_match.id.as_str()));
}

#[test]
fn task_add_tool_creates_proposed_tasks_for_agents() {
    let (_root, runtime, _repo_root) = test_runtime();

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Propose task from tool",
                "description": "Exercise the agent-facing task creation path.",
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");

    assert_eq!(
        output.get("status").and_then(Value::as_str),
        Some("proposed")
    );
}

#[test]
fn mcp_task_add_uses_session_workspace_from_worktree_cwd() {
    let (_root, runtime, repo_root) = test_runtime();
    let worktree_cwd = repo_root
        .join(".orbit")
        .join("state")
        .join("worktrees")
        .join("orbit-jrun-test")
        .join("nested");
    fs::create_dir_all(&worktree_cwd).expect("create worktree cwd");
    let _cwd = CurrentDirGuard::enter(&worktree_cwd);
    let workspace_config =
        read_workspace_config(&repo_root.join(".orbit")).expect("read canonical workspace config");
    let repo_root_string = repo_root.to_string_lossy().into_owned();

    let output = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.add",
            json!({
                "title": "Ambient worktree task",
                "description": "Session workspace must beat process cwd.",
                "complexity": "low",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            ToolEntryPoint::Mcp,
            ToolSessionContext::with_workspace(repo_root_string.clone()),
        )
        .expect("task add tool succeeds")
        .value;

    let task_id = output["id"].as_str().expect("task id");
    assert!(
        runtime
            .global_root()
            .join("tasks")
            .join("workspaces")
            .join(workspace_config.workspace_id)
            .join(task_id)
            .join("task.yaml")
            .exists(),
        "task bundle must be under the canonical workspace id"
    );
}

#[test]
fn task_add_tool_rejects_dropped_task_types_and_ignores_retired_status() {
    let (_root, runtime, _repo_root) = test_runtime();

    for dropped_type in ["task", "epic", "issue", "friction"] {
        let message = invalid_input_message(runtime.execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Legacy friction type",
                "description": "Should use the new friction record surface.",
                "complexity": "low",
                "workspace": ".",
                "type": dropped_type,
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        ));
        assert!(message.contains(dropped_type), "{message}");
        assert!(
            message.contains("feature, bug, refactor, chore"),
            "{message}"
        );
    }

    // ORB-00255 retired `status` from the `orbit.task.add` schema; an input
    // value is silently stripped and the task lands as `proposed`.
    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Retired task-add status",
                "description": "Should ignore retired task-add status.",
                "complexity": "low",
                "workspace": ".",
                "status": "done",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("retired status field is ignored");
    assert_eq!(
        output.get("status").and_then(Value::as_str),
        Some("proposed")
    );
}

#[test]
fn friction_add_writes_markdown_record_and_validates_tags() {
    let (_root, runtime, _repo_root) = test_runtime();

    let output = runtime
        .execute_tool_command(
            "orbit.friction.add",
            json!({
                "body": "The tool guidance pointed at the old task path.",
                "tags": ["tooling", "skill-guidance"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("friction add succeeds");

    // ADR-0345: records written after the SQLite cutover report `path: null`
    // rather than a file location nothing could open.
    assert_eq!(output["path"], Value::Null);
    let id = output["id"].as_str().expect("record id");
    assert!(id.starts_with('F'), "{id}");
    assert_eq!(output["model"], json!("codex"));

    // ORB-10798: `show` is inactive on the agent tool surface, so it is read
    // back through the CLI / dashboard `run_tool` path, as `orbit friction
    // show` does.
    let shown = runtime
        .run_tool("orbit.friction.show", json!({ "id": id }))
        .expect("friction show succeeds");
    assert_eq!(shown["id"], json!(id));
    assert_eq!(shown["status"], json!("open"));
    assert_eq!(shown["tags"], json!(["skill-guidance", "tooling"]));
    assert_eq!(shown["path"], Value::Null);

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.friction.add",
        json!({
            "body": "Unknown tag should be rejected.",
            "tags": ["not-a-real-tag"],
        }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    ));
    assert!(message.contains("valid tags"), "{message}");
}

#[test]
fn friction_stats_does_not_write_state_scoreboard_file() {
    let (_root, runtime, _repo_root) = test_runtime();
    runtime
        .execute_tool_command(
            "orbit.friction.add",
            json!({ "body": "A friction report.", "tags": ["other"] }),
            Some("codex".to_string()),
            Some("gpt-zero".to_string()),
        )
        .expect("add friction");

    let stats = runtime
        .run_tool("orbit.friction.stats", json!({}))
        .expect("stats succeeds");
    assert_eq!(
        stats["by_family"]["codex"]["frictions_per_10_tasks"],
        json!("n/a")
    );
    assert!(
        !runtime
            .data_root()
            .join("state")
            .join("friction_stats.json")
            .exists()
    );
}

#[test]
fn task_delete_tool_rejects_unforced_protected_statuses() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Protected delete",
        "Backlog tasks require force before permanent deletion.",
        TaskStatus::Backlog,
        &[],
    );

    // ORB-00289: `orbit.task.delete` is inactive on the agent surface;
    // `execute_tool_command` now gates on `ensure_tool_agent_facing` and
    // would reject the call. The tool's business logic (guard, force
    // flag, status semantics) is still reachable through `runtime.run_tool`
    // which bypasses the agent gate, matching the CLI path
    // (`runtime.delete_task_guarded`) that admin workflows use in
    // production.
    let message = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.task.delete",
        json!({ "id": task.id.clone() }),
    ));

    assert_eq!(
        message,
        format!(
            "task '{}' is in status 'backlog'; use --force to delete tasks not in proposed or rejected status",
            task.id
        )
    );
    runtime
        .get_task(&task.id)
        .expect("unforced protected task remains");
}

#[test]
fn task_delete_tool_allows_unforced_proposed_and_rejected_tasks() {
    let (_root, runtime, repo_root) = test_runtime();

    for status in [TaskStatus::Proposed, TaskStatus::Rejected] {
        let task = create_task(
            &runtime,
            &repo_root,
            &format!("Delete {status}"),
            "Unprotected statuses can be permanently deleted without force.",
            status,
            &[],
        );

        // ORB-00289: see note above — `run_tool` exercises the tool
        // dispatch business logic without the agent-surface gate.
        let output = run_tool_as_operator(
            &runtime,
            "orbit.task.delete",
            json!({ "id": task.id.clone() }),
        )
        .expect("unprotected delete succeeds");

        assert_eq!(output, json!({ "id": task.id, "deleted": true }));
    }
}

#[test]
fn task_delete_tool_allows_forced_protected_statuses() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Forced delete",
        "Protected statuses can be permanently deleted with explicit force.",
        TaskStatus::InProgress,
        &[],
    );

    // ORB-00289: see note above — `run_tool` exercises the tool dispatch
    // business logic without the agent-surface gate.
    let output = run_tool_as_operator(
        &runtime,
        "orbit.task.delete",
        json!({ "id": task.id.clone(), "force": true }),
    )
    .expect("forced protected delete succeeds");

    assert_eq!(output, json!({ "id": task.id.clone(), "deleted": true }));
    assert!(runtime.get_task(&task.id).is_err(), "task was deleted");
}

#[test]
fn task_add_tool_ignores_retired_dependencies() {
    // ORB-00255 retired `dependencies` from the `orbit.task.add` schema; any
    // value supplied is silently stripped before persistence.
    let (_root, runtime, repo_root) = test_runtime();
    let dependency = create_task(
        &runtime,
        &repo_root,
        "Dependency task",
        "Existing task that must finish first.",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Dependent task from tool",
                "description": "Exercise dependency input on the agent-facing task creation path.",
                "complexity": "low",
                "workspace": ".",
                "dependencies": [dependency.id.clone()],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");

    assert_eq!(output.get("dependencies"), Some(&json!([])));
}

#[test]
fn task_add_and_show_tools_roundtrip_tags() {
    let (_root, runtime, _repo_root) = test_runtime();

    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Tagged task",
                "description": "Exercise tag input on the agent-facing task creation path.",
                "complexity": "low",
                "workspace": ".",
                "tags": ["perf", "bench"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    let task_id = added["id"].as_str().expect("task id");

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task_id }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool succeeds");

    assert_eq!(shown.get("tags"), Some(&json!(["perf", "bench"])));
}

#[test]
fn task_add_and_show_tools_roundtrip_crew() {
    // ORB-10123: `crew` is no longer a retired/stripped field on orbit.task.add.
    // The tool reads it from input, validates it, and persists it onto the
    // created task (pre-un-retire it was silently dropped before host execution).
    let (_root, runtime, _repo_root) = test_runtime();

    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Crew task",
                "description": "Exercise crew input on the agent-facing create path.",
                "complexity": "low",
                "workspace": ".",
                "crew": "sol",
                "orchestrator": "sol",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds with a valid crew");
    let task_id = added["id"].as_str().expect("task id");
    assert_eq!(added.get("crew"), Some(&json!("sol")));
    assert_eq!(added.get("orchestrator"), Some(&json!("sol")));

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task_id }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool succeeds");
    assert_eq!(shown.get("crew"), Some(&json!("sol")));
    assert_eq!(shown.get("orchestrator"), Some(&json!("sol")));
}

#[test]
fn task_tools_roundtrip_required_tools_and_reject_updates() {
    let (_root, runtime, _repo_root) = test_runtime();
    let agent = Some("codex".to_string());
    let model = Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string());
    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Task-scoped GitHub reads",
                "description": "Exercise required tools on every task tool surface.",
                "complexity": "low",
                "workspace": ".",
                "required_tools": [
                    "github.run.list",
                    "github.auth.status",
                    "github.run.list"
                ],
            }),
            agent.clone(),
            model.clone(),
        )
        .expect("add task with requirements");
    let task_id = added["id"].as_str().expect("task id");
    assert_eq!(
        added["required_tools"],
        json!(["github.auth.status", "github.run.list"])
    );

    let projected = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({"id": task_id, "fields": ["required_tools"]}),
            agent.clone(),
            model.clone(),
        )
        .expect("project requirements");
    assert_eq!(projected, json!(["github.auth.status", "github.run.list"]));
    let listed = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({"workspace": "."}),
            agent.clone(),
            model.clone(),
        )
        .expect("list task requirements");
    assert_eq!(
        listed[0]["required_tools"],
        json!(["github.auth.status", "github.run.list"])
    );

    let error = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({"id": task_id, "required_tools": ["github.run.view"]}),
            agent,
            model,
        )
        .expect_err("task requirements are creation-only");
    assert!(error.to_string().contains("immutable"), "{error}");
}

/// ORB-10968: crew configuration is host-local, so a stored crew this host has
/// no `[crews.*]` entry for — a legacy name, or one only the authoring machine
/// defines — must not make the task unreadable. ORB-10586 established that for
/// `orbit task show`; the tool-host projection behind CLI tool-run and every
/// MCP session still propagated the resolution error.
#[test]
fn task_read_tools_render_a_task_whose_stored_crew_is_undefined_here() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task_with_crew(
        &runtime,
        &repo_root,
        "Legacy crew task",
        "Authored when `all-grok` was still a defined crew.",
        TaskStatus::Backlog,
        &[],
        Some("all-grok"),
    );

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task.id }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool stays readable with an unresolvable crew");
    assert_eq!(
        shown.get("crew"),
        Some(&json!("all-grok")),
        "the raw stored crew stays visible: {shown}"
    );
    assert!(
        shown.get("resolved_crew").is_none() && shown.get("crew_model").is_none(),
        "resolved crew/model must be omitted rather than guessed: {shown}"
    );
    let unresolved = shown
        .get("crew_unresolved")
        .and_then(Value::as_str)
        .expect("an unresolvable crew is explicitly marked");
    assert!(
        unresolved.contains("all-grok"),
        "the non-fatal warning names the crew: {unresolved}"
    );

    // Listing and a field projection apply the same tolerant read contract.
    let listed = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({ "workspace": "." }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task list tool stays readable with an unresolvable crew");
    assert_task_titles(&listed, &["Legacy crew task"]);
    let fields = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task.id, "fields": ["crew"] }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("field projection stays readable with an unresolvable crew");
    assert_eq!(fields, json!("all-grok"));

    // Execution still resolves strictly: start must fail with the actionable
    // crew-validation error rather than inherit a fallback.
    let started = runtime.execute_tool_command(
        "orbit.task.start",
        json!({ "id": task.id }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    );
    let message = match started {
        Err(error) => error.to_string(),
        Ok(value) => panic!("expected start to reject an unresolvable crew, got {value}"),
    };
    assert!(
        message.contains("all-grok") && message.contains("not defined"),
        "starting an unresolvable crew must stay a crew-validation error: {message}"
    );
}

/// ORB-10648: `priority` is an advertised and applied update field. The record
/// layer could always persist it, but neither the tool schema nor the update
/// handler read it, so a caller's re-prioritization was discarded while the
/// tool answered with the unchanged task.
#[test]
fn task_update_tool_persists_priority() {
    let (_root, runtime, _repo_root) = test_runtime();
    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Priority update task",
                "description": "Starts at the default priority and is raised on update.",
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    let task_id = added["id"].as_str().expect("task id");
    assert_eq!(added.get("priority"), Some(&json!("medium")));

    let updated = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task_id, "priority": "high" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("priority update succeeds");
    assert_eq!(updated.get("priority"), Some(&json!("high")));

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task_id }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show succeeds");
    assert_eq!(shown.get("priority"), Some(&json!("high")));

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": task_id, "priority": "nonsense" }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    ));
    assert!(
        message.contains("priority"),
        "an unparseable priority names the field: {message}"
    );
}

#[test]
fn task_update_tool_persists_complexity_without_adding_history() {
    let (_root, runtime, _repo_root) = test_runtime();
    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Complexity update task",
                "description": "Starts assessed and receives a replacement on update.",
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    let task_id = added["id"].as_str().expect("task id");
    assert_eq!(added.get("complexity"), Some(&json!("low")));
    let history_before = runtime.get_task_history(task_id).expect("initial history");

    runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task_id, "crew": "sol" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("crew update succeeds");
    let history_after_crew = runtime
        .get_task_history(task_id)
        .expect("history after crew update");
    assert_eq!(
        history_after_crew, history_before,
        "crew update adds no history"
    );

    let updated = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task_id, "complexity": "medium" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("complexity update succeeds");
    assert_eq!(updated.get("complexity"), Some(&json!("medium")));
    assert_eq!(
        runtime
            .get_task_history(task_id)
            .expect("history after complexity update"),
        history_after_crew,
        "complexity update must match crew's no-history behavior"
    );

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task_id }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show succeeds");
    assert_eq!(shown.get("complexity"), Some(&json!("medium")));

    let omitted = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task_id, "title": "Complexity remains set" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("update omitting complexity succeeds");
    assert_eq!(omitted.get("complexity"), Some(&json!("medium")));
}

/// An MCP session started with `orbit mcp serve --orchestrator <crew>` and
/// bound to this test workspace.
fn session_with_orchestrator(workspace: &str, orchestrator: &str) -> ToolSessionContext {
    ToolSessionContext {
        orchestrator: Some(orchestrator.to_string()),
        ..ToolSessionContext::with_workspace(workspace.to_string())
    }
}

fn add_with_session(
    runtime: &crate::OrbitRuntime,
    input: Value,
    session: ToolSessionContext,
) -> Result<Value, orbit_common::OrbitError> {
    runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.add",
            input,
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            ToolEntryPoint::Mcp,
            session,
        )
        .map(|outcome| outcome.value)
}

#[test]
fn session_orchestrator_default_is_resolved_against_the_target_workspace_crews() {
    // The default is a crew name, not a persisted decision: the workspace the
    // call lands in resolves it, so an unconfigured name fails that call
    // rather than quietly attributing the task to some other crew.
    let (_root, runtime, repo_root) = test_runtime();
    let workspace = repo_root.to_string_lossy().into_owned();

    let added = add_with_session(
        &runtime,
        json!({
            "title": "Session-attributed task",
            "description": "The MCP session supplies orchestrator attribution.",
            "complexity": "low",
        }),
        session_with_orchestrator(&workspace, "sol"),
    )
    .expect("a configured session orchestrator is accepted");
    assert_eq!(added.get("orchestrator"), Some(&json!("sol")));
    assert_eq!(
        added.get("crew"),
        Some(&json!(null)),
        "attribution must not select an execution crew"
    );

    let rejected = add_with_session(
        &runtime,
        json!({
            "title": "Unconfigured session orchestrator",
            "description": "An unknown session default must fail loudly.",
            "complexity": "low",
        }),
        session_with_orchestrator(&workspace, "does-not-exist"),
    );
    let message = match rejected {
        Err(error) => format!("{error:?}"),
        Ok(value) => panic!("expected an unknown session orchestrator to be rejected, got {value}"),
    };
    assert!(
        message.contains("crew 'does-not-exist' is not defined"),
        "error should name the unresolvable crew: {message}"
    );
}

#[test]
fn an_explicit_orchestrator_beats_the_session_default_end_to_end() {
    let (_root, runtime, repo_root) = test_runtime();
    let workspace = repo_root.to_string_lossy().into_owned();

    let added = add_with_session(
        &runtime,
        json!({
            "title": "Explicit attribution",
            "description": "The call names its own orchestrator.",
            "complexity": "low",
            "orchestrator": "terra",
        }),
        session_with_orchestrator(&workspace, "sol"),
    )
    .expect("explicit orchestrator is accepted");
    assert_eq!(added.get("orchestrator"), Some(&json!("terra")));
}

#[test]
fn the_session_orchestrator_never_backfills_an_existing_task() {
    // The default applies at creation only. A task created before the session
    // was configured keeps its own attribution, and an ordinary update through
    // that session must not acquire one.
    let (_root, runtime, repo_root) = test_runtime();
    let workspace = repo_root.to_string_lossy().into_owned();

    let created = add_with_session(
        &runtime,
        json!({
            "title": "Unattributed task",
            "description": "Created before any session default existed.",
            "complexity": "low",
        }),
        ToolSessionContext::with_workspace(workspace.clone()),
    )
    .expect("task add succeeds");
    let task_id = created["id"].as_str().expect("task id").to_string();
    assert_eq!(created.get("orchestrator"), Some(&json!(null)));

    let updated = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.update",
            json!({ "id": task_id, "title": "Still unattributed" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            ToolEntryPoint::Mcp,
            session_with_orchestrator(&workspace, "sol"),
        )
        .expect("task update succeeds")
        .value;
    assert_eq!(
        updated.get("orchestrator"),
        Some(&json!(null)),
        "a configured session must not backfill attribution on an existing task"
    );
}

#[test]
fn the_session_orchestrator_respects_the_lifecycle_restriction() {
    // Attribution stays changeable only while proposed or backlog, whether it
    // came from the call or from the session.
    let (_root, runtime, repo_root) = test_runtime();
    let workspace = repo_root.to_string_lossy().into_owned();

    let created = add_with_session(
        &runtime,
        json!({
            "title": "Lifecycle-restricted attribution",
            "description": "Attribution changes stay gated by status.",
            "complexity": "low",
        }),
        session_with_orchestrator(&workspace, "sol"),
    )
    .expect("task add succeeds");
    let task_id = created["id"].as_str().expect("task id").to_string();

    for status in ["backlog", "in-progress"] {
        run_tool_as_operator(
            &runtime,
            "orbit.task.update",
            json!({ "id": task_id, "status": status, "model": "codex" }),
        )
        .unwrap_or_else(|error| panic!("advance to {status}: {error}"));
    }

    let message = invalid_input_message(
        runtime
            .execute_tool_command_dispatch_with_session_context(
                "orbit.task.update",
                json!({ "id": task_id, "orchestrator": "terra" }),
                Some("codex".to_string()),
                Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
                ToolEntryPoint::Mcp,
                session_with_orchestrator(&workspace, "sol"),
            )
            .map(|outcome| outcome.value),
    );
    assert!(
        message.contains("only be changed while proposed or backlog"),
        "error should name the lifecycle restriction: {message}"
    );
}

#[test]
fn task_add_tool_rejects_unknown_crew() {
    // Un-retiring crew also means it is validated: an unknown crew is now
    // rejected rather than silently ignored.
    let (_root, runtime, _repo_root) = test_runtime();

    let result = runtime.execute_tool_command(
        "orbit.task.add",
        json!({
            "title": "Bad crew task",
            "description": "Unknown crew must be rejected on the create path.",
            "complexity": "low",
            "workspace": ".",
            "crew": "does-not-exist",
        }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    );

    let message = match result {
        Err(error) => format!("{error:?}"),
        Ok(value) => panic!("expected unknown crew to be rejected, got {value}"),
    };
    assert!(
        message.contains("crew 'does-not-exist' is not defined"),
        "error should explain the crew is undefined: {message}"
    );
}

#[test]
fn task_show_tool_includes_empty_tags_array() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "No tags",
        "Exercise empty tag shape.",
        TaskStatus::Backlog,
        &[],
    );

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task.id }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool succeeds");

    assert_eq!(shown.get("tags"), Some(&json!([])));
}

#[test]
fn foreign_task_references_are_marked_and_do_not_block_readiness() {
    let (_root, runtime, _repo_root) = test_runtime();
    let foreign_id = "DK-00042";
    let created = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Coordinate with a foreign task",
                "description": "The target is owned by another machine.",
                "complexity": "low",
                "workspace": ".",
                "relations": [
                    {"type": "blocked_by", "target": foreign_id},
                    {"type": "related_to", "target": foreign_id}
                ],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("foreign-prefix task references are accepted");
    let task_id = created["id"].as_str().expect("created task id");

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({"id": task_id}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("show foreign-prefix task");
    assert_eq!(
        shown["resolved_dependencies"],
        json!(["DK-00042 [not verifiable here]"])
    );
    let related_to = shown["relations"]
        .as_array()
        .expect("relations array")
        .iter()
        .find(|relation| relation["type"] == "related_to")
        .expect("related_to relation");
    assert_eq!(related_to["verification"], json!("not verifiable here"));

    let ready = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({"ready": true}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("list ready tasks");
    assert!(
        ready
            .as_array()
            .expect("ready task array")
            .iter()
            .any(|task| task["id"] == task_id),
        "foreign dependencies do not gate readiness"
    );
}

#[test]
fn task_show_tool_with_context_includes_related_docs() {
    let (_root, runtime, repo_root) = test_runtime();
    fs::create_dir_all(repo_root.join("docs")).expect("docs dir");
    fs::write(
        repo_root.join("docs/cli.md"),
        "---\ntype: design\nsummary: CLI command design\npaths: [\"crates/orbit-cli/**\"]\n---\n# CLI Commands\n",
    )
    .expect("write doc");
    let task = create_task(
        &runtime,
        &repo_root,
        "Show related docs",
        "Exercise MCP context injection.",
        TaskStatus::Backlog,
        &["file:crates/orbit-cli/src/command/docs.rs"],
    );

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task.id, "with_context": true, "max_docs": 1 }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool succeeds");

    assert_eq!(
        shown.get("related_docs"),
        Some(&json!([
            {
                "path": "docs/cli.md",
                "type": "design",
                "summary": "CLI command design",
                "excerpt": "CLI Commands",
                "matched_by": ["path:crates/orbit-cli/**"]
            }
        ]))
    );
}

#[test]
fn task_add_tool_normalizes_tags_at_write_time() {
    let (_root, runtime, _repo_root) = test_runtime();

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Normalized tags",
                "description": "Exercise tag normalization.",
                "complexity": "low",
                "workspace": ".",
                "tags": ["  Perf ", "BENCH"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");

    assert_eq!(output.get("tags"), Some(&json!(["perf", "bench"])));
}

#[test]
fn task_add_tool_ignores_retired_external_refs() {
    // ORB-00255 retired `external_refs` from the `orbit.task.add` schema; any
    // value supplied is silently stripped before persistence.
    let (_root, runtime, _repo_root) = test_runtime();

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "External ref task",
                "description": "Exercise external ref input on the agent-facing task creation path.",
                "complexity": "low",
                "workspace": ".",
                "external_refs": [
                    {"system": "jira", "id": "ENG-1234", "url": "https://example.com/browse/ENG-1234"},
                    {"system": "linear", "id": "LIN-567"}
                ],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");

    assert_eq!(output.get("external_refs"), Some(&json!([])));
}

#[test]
fn task_add_tool_recovers_mcp_encoded_acceptance_and_context_arrays() {
    let (_root, runtime, repo_root) = test_runtime();
    let src_dir = repo_root.join("src");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    std::fs::write(src_dir.join("lib.rs"), "pub fn ok() {}\n").expect("write source file");

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Encoded list task",
                "description": "Exercise MCP single-element encoded array recovery.",
                "complexity": "low",
                "workspace": repo_root.to_string_lossy(),
                "acceptance_criteria": ["[\"Criterion A\", \"Criterion B\"]"],
                "context_files": ["[\"file:src/lib.rs\"]"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");

    assert_eq!(
        output.get("acceptance_criteria"),
        Some(&json!(["Criterion A", "Criterion B"]))
    );
    assert_eq!(
        output.get("context_files"),
        Some(&json!(["file:src/lib.rs"]))
    );
}

#[test]
fn task_add_tool_preserves_commas_in_acceptance_criteria_array() {
    let (_root, runtime, _repo_root) = test_runtime();

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Comma-safe criteria",
                "description": "Exercise explicit acceptance criteria arrays.",
                "complexity": "low",
                "workspace": ".",
                "acceptance_criteria": [
                    "first criterion, with a comma",
                    "second criterion"
                ],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");

    assert_eq!(
        output.get("acceptance_criteria"),
        Some(&json!([
            "first criterion, with a comma",
            "second criterion"
        ]))
    );
}

#[test]
fn task_add_tool_keeps_scalar_acceptance_criteria_as_one_value() {
    let (_root, runtime, _repo_root) = test_runtime();

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Scalar criterion",
                "description": "Exercise scalar acceptance criteria input.",
                "complexity": "low",
                "workspace": ".",
                "acceptance_criteria": "one criterion, with a comma",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");

    assert_eq!(
        output.get("acceptance_criteria"),
        Some(&json!(["one criterion, with a comma"]))
    );
}

#[test]
fn task_add_tool_infers_agent_from_model_only_input() {
    let _env =
        orbit_common::test_env::unset(orbit_common::test_env::AGENT_IDENTITY_ENV.iter().copied());
    let (_root, runtime, _repo_root) = test_runtime();

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Propose model-only task",
                "description": "Exercise model-first provenance.",
                "complexity": "low",
                "workspace": ".",
                "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
            }),
            None,
            None,
        )
        .expect("task add tool succeeds");

    assert!(output.get("agent").is_none());
    // `model` is internal execution routing; v2 does not persist it, so it
    // round-trips as null (the tool layer emits the key unconditionally).
    assert!(output.get("model").is_none_or(serde_json::Value::is_null));
    assert_eq!(
        output.get("created_by").and_then(Value::as_str),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL)
    );
}

#[test]
fn task_update_tool_infers_agent_from_model_only_input() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Update model-only task",
        "Exercise model-first update provenance.",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "comment": "record model-only update",
                "model": "gemini-3.1-pro-preview",
            }),
            None,
            None,
        )
        .expect("task update tool succeeds");

    assert!(output.get("agent").is_none());
    // `model` is internal execution routing; v2 does not persist it.
    assert!(output.get("model").is_none_or(serde_json::Value::is_null));
}

#[test]
fn task_update_tool_persists_and_clears_pr_status() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Review-ready task",
        "A task updated through the agent tool surface.",
        TaskStatus::InProgress,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "pr_status": "approved",
                "execution_summary": "Implemented and verified.",
                "status": "review",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("combined update succeeds");

    assert_eq!(
        output.get("pr_status").and_then(Value::as_str),
        Some("approved")
    );
    assert_eq!(
        output.get("execution_summary").and_then(Value::as_str),
        Some("Implemented and verified.")
    );
    assert_eq!(output.get("status").and_then(Value::as_str), Some("review"));

    let persisted = runtime.get_task(&task.id).expect("read updated task");
    assert_eq!(persisted.pr_status.as_deref(), Some("approved"));
    assert_eq!(persisted.execution_summary, "Implemented and verified.");
    assert_eq!(persisted.status, TaskStatus::Review);

    let cleared = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "pr_status": "",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("empty pr status clears the field");
    assert_eq!(cleared.get("pr_status"), Some(&Value::Null));

    let persisted = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task.id }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool succeeds after clearing pr status");
    assert_eq!(persisted.get("pr_status"), Some(&Value::Null));
}

#[test]
fn task_update_tool_leaves_all_fields_unchanged_when_composite_update_is_invalid() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Backlog task",
        "A task whose invalid status transition must not partially apply.",
        TaskStatus::Backlog,
        &[],
    );

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({
            "id": task.id,
            "title": "   ",
            "pr_status": "approved",
            "execution_summary": "This must not persist.",
            "status": "archived",
        }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    ));
    assert!(message.contains("title"), "{message}");

    let persisted = runtime.get_task(&task.id).expect("read unchanged task");
    assert_eq!(persisted.pr_status, None);
    assert!(persisted.execution_summary.is_empty());
    assert_eq!(persisted.title, "Backlog task");
    assert_eq!(persisted.status, TaskStatus::Backlog);
}

#[test]
fn task_update_tool_rejects_dropped_task_types() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Retype fixture",
        "Task type update fixture.",
        TaskStatus::Backlog,
        &[],
    );

    for dropped_type in ["task", "epic", "issue", "friction"] {
        let message = invalid_input_message(runtime.execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id.clone(),
                "type": dropped_type,
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        ));
        assert!(message.contains(dropped_type), "{message}");
        assert!(
            message.contains("feature, bug, refactor, chore"),
            "{message}"
        );
    }
}

#[test]
fn task_update_tool_replaces_dependencies() {
    let (_root, runtime, repo_root) = test_runtime();
    let first_dependency = create_task(
        &runtime,
        &repo_root,
        "First dependency",
        "Existing task that must finish first.",
        TaskStatus::Backlog,
        &[],
    );
    let second_dependency = create_task(
        &runtime,
        &repo_root,
        "Second dependency",
        "Replacement prerequisite.",
        TaskStatus::Backlog,
        &[],
    );
    let task = create_task(
        &runtime,
        &repo_root,
        "Update dependency task",
        "Exercise dependency replacement through tool input.",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id.clone(),
                "dependencies": [first_dependency.id.clone()],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool sets dependency");

    assert_eq!(
        output.get("dependencies"),
        Some(&json!([first_dependency.id.as_str()]))
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "dependencies": [second_dependency.id.clone()],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool replaces dependency");

    assert_eq!(
        output.get("dependencies"),
        Some(&json!([second_dependency.id.as_str()]))
    );
}

#[test]
fn task_update_tool_persists_source_task_id_and_history() {
    let (_root, runtime, repo_root) = test_runtime();
    let source = create_task(
        &runtime,
        &repo_root,
        "Regression source",
        "Existing task that introduced the defect.",
        TaskStatus::Done,
        &[],
    );
    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Bug without source",
                "description": "A bug whose source is discovered later.",
                "complexity": "low",
                "workspace": ".",
                "type": "bug",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    let task_id = added["id"].as_str().expect("task id").to_string();
    let created_updated_at = added["updated_at"]
        .as_str()
        .expect("created updated_at")
        .to_string();
    assert_eq!(added.get("type").and_then(Value::as_str), Some("bug"));
    assert_eq!(added.get("source_task_id"), Some(&Value::Null));

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task_id,
                "model": "claude",
                "source_task_id": source.id.clone(),
            }),
            None,
            None,
        )
        .expect("task update tool succeeds");

    assert_eq!(
        output.get("source_task_id").and_then(Value::as_str),
        Some(source.id.as_str())
    );
    assert_ne!(
        output.get("updated_at").and_then(Value::as_str),
        Some(created_updated_at.as_str())
    );
    assert!(
        output["history"]
            .as_array()
            .expect("history")
            .iter()
            .any(|event| {
                event.get("event").and_then(Value::as_str) == Some("updated")
                    && event
                        .get("note")
                        .and_then(Value::as_str)
                        .is_some_and(|note| note.contains("source_task_id"))
            })
    );

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": output["id"].as_str().expect("task id") }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool succeeds");
    assert_eq!(
        shown.get("source_task_id").and_then(Value::as_str),
        Some(source.id.as_str())
    );
}

#[test]
fn task_update_tool_clears_source_task_id_with_empty_string() {
    let (_root, runtime, repo_root) = test_runtime();
    let source = create_task(
        &runtime,
        &repo_root,
        "Regression source",
        "Existing task that introduced the defect.",
        TaskStatus::Done,
        &[],
    );
    // ORB-00255 retired `source_task_id` from the `orbit.task.add` schema, so
    // seed it via `orbit.task.update` before exercising the clear path.
    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Bug with source",
                "description": "A bug whose source should be cleared.",
                "complexity": "low",
                "workspace": ".",
                "type": "bug",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    let seeded = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": added["id"].as_str().expect("task id"),
                "source_task_id": source.id.clone(),
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool sets source task");
    assert_eq!(
        seeded.get("source_task_id").and_then(Value::as_str),
        Some(source.id.as_str())
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": added["id"].as_str().expect("task id"),
                "source_task_id": "",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool succeeds");

    assert_eq!(output.get("source_task_id"), Some(&Value::Null));
    assert!(
        output["history"]
            .as_array()
            .expect("history")
            .iter()
            .any(|event| {
                event.get("event").and_then(Value::as_str) == Some("updated")
                    && event
                        .get("note")
                        .and_then(Value::as_str)
                        .is_some_and(|note| note.contains("source_task_id"))
            })
    );
}

#[test]
fn task_update_tool_rejects_unresolved_source_task_id_atomically() {
    // ORB-00255 retired `source_task_id` from the `orbit.task.add` schema,
    // so an unresolved ID must travel via `orbit.task.update`.
    let (_root, runtime, _repo_root) = test_runtime();
    let unresolved_from_update = "ORB-99999";

    let update_target = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Bug without resolved source",
                "description": "A bug whose retired add-side source ID should be ignored.",
                "complexity": "low",
                "workspace": ".",
                "type": "bug",
                "source_task_id": "ORB-99998",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    assert_eq!(
        update_target.get("source_task_id"),
        Some(&Value::Null),
        "retired add-side source_task_id must be ignored"
    );
    let error = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": update_target["id"].as_str().expect("task id"),
                "source_task_id": unresolved_from_update,
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect_err("global task relation target must resolve");

    assert!(error.to_string().contains(unresolved_from_update));
    assert!(error.to_string().contains("coordination registry"));
    let unchanged = runtime
        .get_task(update_target["id"].as_str().expect("task id"))
        .expect("read unchanged task");
    assert_eq!(unchanged.source_task_id(), None);
}

#[test]
fn task_update_tool_replaces_tags() {
    let (_root, runtime, _repo_root) = test_runtime();

    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Replace tags",
                "description": "Exercise tag replacement through tool input.",
                "complexity": "low",
                "workspace": ".",
                "tags": ["perf", "bench"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    let task_id = added["id"].as_str().expect("task id").to_string();

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task_id,
                "tags": ["docs"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool replaces tags");

    assert_eq!(output.get("tags"), Some(&json!(["docs"])));
}

#[test]
fn task_update_tool_replaces_context_files_and_keeps_future_paths() {
    let (_root, runtime, repo_root) = test_runtime();
    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::write(src_dir.join("lib.rs"), "pub fn before() {}\n").expect("write source file");
    fs::write(src_dir.join("main.rs"), "fn main() {}\n").expect("write source file");

    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Context update",
                "description": "Exercise context_files replacement through tool input.",
                "complexity": "low",
                "workspace": repo_root.to_string_lossy(),
                "context_files": ["file:src/lib.rs"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    let task_id = added["id"].as_str().expect("task id").to_string();
    let created_updated_at = added["updated_at"]
        .as_str()
        .expect("created updated_at")
        .to_string();

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task_id,
                "context_files": ["[\"file:src/main.rs\", \"file:src/future.rs\"]"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool replaces context_files");

    assert_eq!(
        output.get("context_files"),
        Some(&json!(["file:src/main.rs", "file:src/future.rs"]))
    );
    assert_ne!(
        output.get("updated_at").and_then(Value::as_str),
        Some(created_updated_at.as_str())
    );

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": output["id"].as_str().expect("task id") }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool succeeds");
    assert_eq!(
        shown.get("context_files"),
        Some(&json!(["file:src/main.rs", "file:src/future.rs"]))
    );

    let events = runtime
        .list_session_events(10)
        .expect("session events")
        .into_iter()
        .filter(|event| event.event_type == "TaskUpdated")
        .collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .any(|event| event.payload["data"]["id"] == shown["id"]),
        "{events:#?}"
    );
}

#[test]
fn task_list_and_search_tools_filter_by_tags_with_and_semantics() {
    let (_root, runtime, _repo_root) = test_runtime();
    for (title, tags) in [
        ("Perf task", json!(["perf"])),
        ("Bench task", json!(["bench"])),
        ("Perf bench task", json!(["perf", "bench"])),
    ] {
        runtime
            .execute_tool_command(
                "orbit.task.add",
                json!({
                    "title": title,
                    "description": "Shared tag-search marker.",
                    "complexity": "low",
                    "workspace": ".",
                    "tags": tags,
                }),
                Some("codex".to_string()),
                Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            )
            .expect("create tagged task");
    }

    let perf_list = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({ "tag": ["perf"] }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("list by tag");
    assert_task_titles(&perf_list, &["Perf task", "Perf bench task"]);

    let both_list = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({ "tag": ["perf", "bench"] }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("list by both tags");
    assert_task_titles(&both_list, &["Perf bench task"]);

    // ORB-00202: `orbit.task.search` was deleted; the search+tag case
    // routes through `orbit.search --kind task --tag <...>`. Results land
    // under `output["results"]` rather than the top-level array.
    let bench_search = runtime
        .execute_tool_command(
            "orbit.search",
            json!({ "query": "tag-search", "kind": "task", "tag": ["bench"] }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("search by tag");
    assert_task_titles(&bench_search["results"], &["Bench task", "Perf bench task"]);

    let both_search = runtime
        .execute_tool_command(
            "orbit.search",
            json!({ "query": "tag-search", "kind": "task", "tag": ["perf", "bench"] }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("search by both tags");
    assert_task_titles(&both_search["results"], &["Perf bench task"]);
}

#[test]
fn task_list_tool_is_status_neutral_recent_first_and_bounded() {
    // ORB-10310: `orbit.task.list` must return every lifecycle status by
    // default (no hidden `backlog,in-progress` subset), newest-first.
    let (_root, runtime, repo_root) = test_runtime();
    let statuses = [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Review,
        TaskStatus::Done,
    ];
    let mut created = Vec::new();
    for (index, status) in statuses.iter().enumerate() {
        created.push(create_task(
            &runtime,
            &repo_root,
            &format!("Task {index} in {status}"),
            "status-neutral listing fixture",
            *status,
            &[],
        ));
    }

    let output = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task list tool succeeds");
    let listed = output.as_array().expect("task array");
    assert_eq!(
        listed.len(),
        created.len(),
        "every status must be listed by default: {listed:?}"
    );
    for task in &created {
        assert!(
            listed.iter().any(|value| value["id"] == json!(task.id)),
            "task {} ({}) missing from status-neutral list",
            task.id,
            task.status
        );
    }
    let created_ats = listed
        .iter()
        .map(|value| {
            value["created_at"]
                .as_str()
                .expect("created_at")
                .to_string()
        })
        .collect::<Vec<_>>();
    for pair in created_ats.windows(2) {
        assert!(
            pair[0] >= pair[1],
            "list must be newest-first by created_at: {created_ats:?}"
        );
    }
}

#[test]
fn task_list_tool_default_limit_returns_newest_fifty() {
    // ORB-10310: the status-neutral default is bounded to the 50 newest tasks.
    let (_root, runtime, repo_root) = test_runtime();
    let mut created = Vec::new();
    for index in 0..55 {
        created.push(create_task(
            &runtime,
            &repo_root,
            &format!("Bounded task {index:02}"),
            "default-limit fixture",
            TaskStatus::Backlog,
            &[],
        ));
    }

    let output = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task list tool succeeds");
    let listed = output.as_array().expect("task array");
    assert_eq!(
        listed.len(),
        50,
        "default limit must bound the response to 50"
    );
    // The 50 returned are the newest; the five oldest are excluded.
    let listed_ids = listed
        .iter()
        .map(|value| value["id"].as_str().expect("id").to_string())
        .collect::<std::collections::HashSet<_>>();
    for oldest in &created[..5] {
        assert!(
            !listed_ids.contains(&oldest.id),
            "oldest task {} must fall outside the newest 50",
            oldest.id
        );
    }
}

#[test]
fn task_list_tool_limit_override_and_zero_rejection() {
    let (_root, runtime, repo_root) = test_runtime();
    for index in 0..3 {
        create_task(
            &runtime,
            &repo_root,
            &format!("Override task {index}"),
            "limit-override fixture",
            TaskStatus::Backlog,
            &[],
        );
    }

    let limited = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({ "limit": 2 }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task list tool succeeds");
    assert_eq!(limited.as_array().expect("task array").len(), 2);

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.list",
        json!({ "limit": 0 }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    ));
    assert!(message.contains("at least 1"), "{message}");
}

#[test]
fn task_list_tool_applies_status_filter_before_limit() {
    // ORB-10310: an explicit filter must be applied before ordering + limiting,
    // so a `limit: 1` on a status filter returns the newest *matching* task,
    // never a newer non-matching one.
    let (_root, runtime, repo_root) = test_runtime();
    create_task(
        &runtime,
        &repo_root,
        "Older review task",
        "filter-before-limit fixture",
        TaskStatus::Review,
        &[],
    );
    let newer_review = create_task(
        &runtime,
        &repo_root,
        "Newer review task",
        "filter-before-limit fixture",
        TaskStatus::Review,
        &[],
    );
    // Created last, so newest overall — but not a review, so the status filter
    // must exclude it even though the limit is 1.
    create_task(
        &runtime,
        &repo_root,
        "Newest backlog task",
        "filter-before-limit fixture",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({ "status": "review", "limit": 1 }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task list tool succeeds");
    let listed = output.as_array().expect("task array");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], json!(newer_review.id));
    assert_eq!(listed[0]["status"], json!("review"));
}

#[test]
fn task_list_tool_accepts_comma_delimited_and_array_status_filters() {
    let (_root, runtime, repo_root) = test_runtime();
    let backlog = create_task(
        &runtime,
        &repo_root,
        "Backlog status task",
        "multi-status fixture",
        TaskStatus::Backlog,
        &[],
    );
    let in_progress = create_task(
        &runtime,
        &repo_root,
        "In-progress status task",
        "multi-status fixture",
        TaskStatus::InProgress,
        &[],
    );
    let review = create_task(
        &runtime,
        &repo_root,
        "Review status task",
        "multi-status fixture",
        TaskStatus::Review,
        &[],
    );
    create_task(
        &runtime,
        &repo_root,
        "Done status task",
        "multi-status fixture",
        TaskStatus::Done,
        &[],
    );

    let comma_delimited = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({ "status": "backlog,in-progress,review" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("comma-delimited statuses succeed");
    let array = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({ "status": ["backlog", "in-progress", "review"] }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("status array succeeds");

    for output in [&comma_delimited, &array] {
        let ids = output
            .as_array()
            .expect("task array")
            .iter()
            .map(|task| task["id"].as_str().expect("task id"))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(backlog.id.as_str()));
        assert!(ids.contains(in_progress.id.as_str()));
        assert!(ids.contains(review.id.as_str()));
    }
}

#[test]
fn task_update_tool_recovers_mcp_encoded_acceptance_array() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Update encoded list",
        "Exercise replacement through MCP encoded array shape.",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "acceptance_criteria": ["[\"Criterion A\", \"Criterion B\"]"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool succeeds");

    assert_eq!(
        output.get("acceptance_criteria"),
        Some(&json!(["Criterion A", "Criterion B"]))
    );
}

#[test]
fn task_update_tool_preserves_commas_in_acceptance_criteria_array() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Update comma-safe criteria",
        "Exercise explicit acceptance criteria replacement.",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "acceptance_criteria": [
                    "updated criterion, with a comma",
                    "another updated criterion"
                ],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool succeeds");

    assert_eq!(
        output.get("acceptance_criteria"),
        Some(&json!([
            "updated criterion, with a comma",
            "another updated criterion"
        ]))
    );
}

#[test]
fn task_update_tool_keeps_scalar_acceptance_criteria_as_one_value() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Update scalar criterion",
        "Exercise scalar acceptance criteria replacement.",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "acceptance_criteria": "updated criterion, with a comma",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool succeeds");

    assert_eq!(
        output.get("acceptance_criteria"),
        Some(&json!(["updated criterion, with a comma"]))
    );
}

#[test]
fn task_show_tool_recovers_mcp_encoded_fields_array() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Show encoded fields",
        "Exercise field projection through MCP encoded array shape.",
        TaskStatus::Backlog,
        &["file:src/lib.rs"],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({
                "id": task.id,
                "fields": ["[\"description\", \"context_files\"]"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool succeeds");

    assert_eq!(
        output,
        json!({
            "description": "Exercise field projection through MCP encoded array shape.",
            "context_files": ["file:src/lib.rs"],
        })
    );
}

#[test]
fn task_show_tool_projects_status_as_a_bare_json_string() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Show status",
        "Exercise the observed fields:[status] call.",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task.id, "fields": ["status"] }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("fields:[status] must succeed");

    assert_eq!(output, json!("backlog"));
}

#[test]
fn task_show_tool_projects_mixed_top_level_and_sidecar_fields() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Show mixed fields",
        "Exercise mixed top-level and sidecar projection.",
        TaskStatus::InProgress,
        &["file:src/lib.rs"],
    );

    runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "relations": [{"type": "related_to", "target": "DK-00042"}],
                "job_run_id": "jrun-projection",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("projection fixture metadata update succeeds");

    let output = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({
                "id": task.id,
                "fields": [
                    "status",
                    "relations",
                    "external_refs",
                    "job_run_id",
                ],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("mixed projection must succeed");

    assert_eq!(
        output,
        json!({
            "status": "in-progress",
            "relations": [{
                "type": "related_to",
                "target": "DK-00042",
                "verification": "not verifiable here",
            }],
            "external_refs": [],
            "job_run_id": "jrun-projection",
        })
    );
}

#[test]
fn task_show_public_dto_and_projection_vocabulary_cannot_drift() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "DTO vocabulary drift",
        "Keep every stable public DTO key classified.",
        TaskStatus::Backlog,
        &[],
    );
    let dto = task_to_json(
        &task,
        &runtime.task_status_index().expect("task status index"),
    );
    let mut actual = dto
        .as_object()
        .expect("task DTO object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    actual.sort_unstable();
    let mut expected = TASK_SHOW_PUBLIC_DTO_FIELDS.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

#[test]
fn task_show_tool_rejects_unknown_projection_with_the_shared_vocabulary() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Unknown projection",
        "Exercise unknown field validation.",
        TaskStatus::Backlog,
        &[],
    );

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.show",
        json!({ "id": task.id, "fields": ["not_a_field"] }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    ));
    assert!(
        message.contains("unknown field selector `not_a_field`"),
        "{message}"
    );
    assert!(
        message.contains(orbit_types::task::TASK_SHOW_PROJECTION_FIELDS_CSV),
        "{message}"
    );
}

#[test]
fn task_show_tool_rejects_terminal_with_status_guidance() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Terminal projection",
        "Exercise actionable lifecycle guidance.",
        TaskStatus::Backlog,
        &[],
    );

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.show",
        json!({ "id": task.id, "field": "terminal" }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    ));
    assert!(message.contains("use `status`"), "{message}");
}

#[test]
fn task_update_tool_allows_explicit_attribution_updates() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Update explicit attribution",
        "Exercise explicit provenance correction.",
        TaskStatus::Backlog,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id.clone(),
                "planned_by": "manual-planner",
                "implemented_by": "manual-implementer",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool succeeds");

    assert_eq!(
        output.get("planned_by").and_then(Value::as_str),
        Some("manual-planner")
    );
    assert_eq!(
        output.get("implemented_by").and_then(Value::as_str),
        Some("manual-implementer")
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "planned_by": "",
                "implemented_by": "",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool clears attribution");

    assert_eq!(output.get("planned_by"), Some(&Value::Null));
    assert_eq!(output.get("implemented_by"), Some(&Value::Null));
}

#[test]
fn task_update_tool_explicit_implemented_by_overrides_review_stamp() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Review explicit attribution",
        "Exercise explicit provenance correction on review transition.",
        TaskStatus::InProgress,
        &[],
    );

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "status": "review",
                "execution_summary": "Implemented and validated.",
                "implemented_by": "manual-implementer",
                "model": "gemini-3.1-pro-preview",
            }),
            None,
            None,
        )
        .expect("task update tool succeeds");

    assert_eq!(output.get("status").and_then(Value::as_str), Some("review"));
    assert_eq!(
        output.get("implemented_by").and_then(Value::as_str),
        Some("manual-implementer")
    );
}

#[test]
fn task_tool_rejects_mismatched_agent_and_model() {
    let (_root, runtime, _repo_root) = test_runtime();

    let error = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Reject mismatched identity",
                "description": "Exercise explicit mismatch validation.",
                "complexity": "low",
                "workspace": ".",
                "agent": "claude",
                "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
            }),
            None,
            None,
        )
        .expect_err("agent input should fail");

    assert!(error.to_string().contains("use `model`"));
}

/// End-to-end coverage for the artifact read surface: attach through the
/// canonical put tool, list compact metadata, then retrieve the payload.
mod artifact_get {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use orbit_types::task::{MAX_TASK_ARTIFACT_CONTENT_BYTES, TaskStatus};
    use serde_json::{Value, json};

    use super::super::super::test_support::{create_task, run_tool_as_operator, test_runtime};
    use crate::OrbitRuntime;
    use crate::application::task::TaskUpdateParams;

    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// A synthetic PNG: a real signature plus filler. Orbit stores bytes and
    /// classifies by signature, so no encoder is needed and no user content
    /// ever has to be copied into the test corpus.
    fn synthetic_png() -> Vec<u8> {
        let mut bytes = PNG_SIGNATURE.to_vec();
        bytes.extend_from_slice(&(0..512_u16).map(|i| (i % 251) as u8).collect::<Vec<_>>());
        bytes
    }

    fn synthetic_jpeg() -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
        bytes.extend_from_slice(b"JFIF synthetic body");
        bytes
    }

    fn attach(runtime: &OrbitRuntime, task_id: &str, path: &str, content: Vec<u8>) {
        let source = runtime.paths().repo_root.join(format!(
            "orbit-artifact-fixture-{}-{}",
            std::process::id(),
            path.replace('/', "_"),
        ));
        std::fs::write(&source, &content).expect("write artifact fixture");
        run_tool_as_operator(
            runtime,
            "orbit.task.artifact.put",
            json!({
                "id": task_id,
                "source_path": source.to_string_lossy(),
                "path": path,
                "model": "codex",
            }),
        )
        .expect("attach artifact");
        std::fs::remove_file(&source).ok();
    }

    fn get(runtime: &OrbitRuntime, task_id: &str, path: &str) -> Value {
        run_tool_as_operator(
            runtime,
            "orbit.task.artifact.get",
            json!({"id": task_id, "path": path}),
        )
        .expect("read artifact")
    }

    fn seeded_task(runtime: &OrbitRuntime, repo_root: &std::path::Path) -> String {
        create_task(
            runtime,
            repo_root,
            "artifact fixture",
            "holds synthetic artifacts",
            TaskStatus::InProgress,
            &[],
        )
        .id
        .to_string()
    }

    #[test]
    fn raster_images_survive_attach_list_and_read_with_intact_bytes() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);
        let png = synthetic_png();
        let jpeg = synthetic_jpeg();
        attach(&runtime, &id, "diagrams/flow.png", png.clone());
        attach(&runtime, &id, "diagrams/shot.jpg", jpeg.clone());

        // Listing stays compact: metadata only, no payload for binary content.
        let listed = run_tool_as_operator(
            &runtime,
            "orbit.task.show",
            json!({"id": id, "fields": "artifacts"}),
        )
        .expect("list artifacts");
        let rows = listed.as_array().expect("artifact rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["media_type"], "image/png");
        assert_eq!(rows[0]["size"], png.len());
        assert!(
            rows[0].get("content").is_none(),
            "binary payloads must not ride along in the metadata list"
        );

        for (path, media_type, expected) in [
            ("diagrams/flow.png", "image/png", &png),
            ("diagrams/shot.jpg", "image/jpeg", &jpeg),
        ] {
            let read = get(&runtime, &id, path);
            assert_eq!(read["media_type"], media_type);
            assert_eq!(read["presentation"], "image");
            assert_eq!(read["encoding"], "base64");
            assert_eq!(read["size"], expected.len());
            let decoded = BASE64_STANDARD
                .decode(read["content_base64"].as_str().expect("base64 payload"))
                .expect("payload decodes");
            assert_eq!(decoded, *expected, "{path} lost byte integrity");
        }
    }

    #[test]
    fn text_artifacts_are_returned_as_utf8_rather_than_base64() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);
        attach(&runtime, &id, "notes/summary.md", b"# heading\n".to_vec());

        let read = get(&runtime, &id, "notes/summary.md");
        assert_eq!(read["presentation"], "text");
        assert_eq!(read["encoding"], "utf8");
        assert_eq!(read["content"], "# heading\n");
        assert!(read.get("content_base64").is_none());
    }

    #[test]
    fn svg_stays_a_download_and_is_never_classified_as_a_viewable_image() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";
        attach(&runtime, &id, "diagrams/active.svg", svg.to_vec());

        let read = get(&runtime, &id, "diagrams/active.svg");
        assert_eq!(read["media_type"], "image/svg+xml");
        assert_eq!(
            read["presentation"], "opaque",
            "SVG carries active content and must never be handed to a renderer"
        );
        // Still fully retrievable — fail-closed is about rendering, not access.
        let decoded = BASE64_STANDARD
            .decode(read["content_base64"].as_str().expect("base64 payload"))
            .expect("payload decodes");
        assert_eq!(decoded, svg.to_vec());
    }

    #[test]
    fn a_png_whose_bytes_are_markup_is_downgraded_to_opaque() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);
        // The media type comes from the extension, so this is the shape a
        // mislabeled or corrupt upload actually takes.
        attach(
            &runtime,
            &id,
            "diagrams/lying.png",
            b"<html><script>alert(1)</script></html>".to_vec(),
        );

        let read = get(&runtime, &id, "diagrams/lying.png");
        assert_eq!(read["media_type"], "image/png");
        assert_eq!(read["presentation"], "opaque");
    }

    #[test]
    fn a_missing_artifact_names_the_task_and_path() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);
        attach(&runtime, &id, "notes/present.txt", b"here".to_vec());

        let error = run_tool_as_operator(
            &runtime,
            "orbit.task.artifact.get",
            json!({"id": id, "path": "notes/absent.txt"}),
        )
        .expect_err("missing artifact is an error");
        let message = error.to_string();
        assert!(
            message.contains(&id),
            "error should name the task: {message}"
        );
        assert!(
            message.contains("notes/absent.txt"),
            "error should name the path: {message}"
        );
    }

    #[test]
    fn traversal_and_absolute_paths_are_refused_before_any_read() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);

        for path in [
            "../../etc/passwd",
            "/etc/passwd",
            "notes/../../escape.txt",
            "./notes.txt",
            r"notes\escape.txt",
        ] {
            let error = run_tool_as_operator(
                &runtime,
                "orbit.task.artifact.get",
                json!({"id": id, "path": path}),
            )
            .expect_err("traversal must be refused");
            assert!(
                matches!(error, orbit_common::OrbitError::InvalidInput(_)),
                "{path} should be rejected as invalid input, got {error}"
            );
        }
    }

    #[test]
    fn an_oversize_artifact_is_a_clear_error_rather_than_a_truncated_payload() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);
        // `orbit.task.artifact.put` bounds its own source read, so an oversize
        // artifact can only reach the store through a direct update. It still
        // must not come back as a half-decodable image.
        let mut oversize = PNG_SIGNATURE.to_vec();
        oversize.resize(MAX_TASK_ARTIFACT_CONTENT_BYTES as usize + 1, 0x5A);
        runtime
            .update_task_with_identity(
                &id,
                TaskUpdateParams {
                    upsert_artifacts: vec![orbit_types::task::TaskArtifact {
                        path: "diagrams/huge.png".to_string(),
                        media_type: "image/png".to_string(),
                        content: oversize,
                        created_by: None,
                    }],
                    ..Default::default()
                },
                None,
                Some("codex".to_string()),
            )
            .expect("store oversize artifact");

        let error = run_tool_as_operator(
            &runtime,
            "orbit.task.artifact.get",
            json!({"id": id, "path": "diagrams/huge.png"}),
        )
        .expect_err("oversize read is refused");
        let message = error.to_string();
        assert!(
            message.contains("limit") && message.contains("diagrams/huge.png"),
            "oversize error should be actionable: {message}"
        );
        assert!(
            message.contains("/api/tasks/"),
            "oversize error should point at the download route: {message}"
        );
    }

    #[test]
    fn an_unknown_task_fails_closed_before_any_artifact_lookup() {
        let (_root, runtime, _repo_root) = test_runtime();
        let error = run_tool_as_operator(
            &runtime,
            "orbit.task.artifact.get",
            json!({"id": "ORB-99999", "path": "diagrams/flow.png"}),
        )
        .expect_err("unknown task is refused");
        assert!(
            matches!(
                error,
                orbit_common::OrbitError::NotFound {
                    kind: orbit_common::NotFoundKind::Task,
                    ..
                }
            ),
            "expected a task not-found, got {error}"
        );
    }
}
