use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use orbit_store::compose::coordination_task_backends;
use orbit_store::contracts::TaskCreateParams;
use orbit_store::maintenance::task_registry::{
    RegisterWorkspaceParams, TaskRegistryStore, read_workspace_config, task_registry_path,
};
use orbit_types::task::{
    TASK_SHOW_PUBLIC_DTO_FIELDS, TaskComplexity, TaskPriority, TaskStatus, TaskType,
};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use super::super::json::task_to_json;
use super::super::test_support::{
    create_task, create_task_with_crew, invalid_input_message, managed_tool_identity_env_guard,
    run_tool_as_operator, test_runtime, unmanaged_tool_env_guard,
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

fn task_list_items(output: &Value) -> &[Value] {
    output
        .get("tasks")
        .and_then(Value::as_array)
        .expect("task list envelope")
}

fn assert_task_list_titles(output: &Value, expected: &[&str]) {
    let mut titles = task_list_items(output)
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
fn task_add_tool_rejects_unknown_required_tools_with_suggestions() {
    let (_root, runtime, _repo_root) = test_runtime();

    let error = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Reject unknown tool",
                "description": "An invalid requirement must not be persisted.",
                "complexity": "low",
                "workspace": ".",
                "required_tools": ["orbit.task.shwo"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect_err("unknown required tool must be rejected");

    assert!(
        error
            .to_string()
            .contains("unregistered tool 'orbit.task.shwo'")
    );
    assert!(
        error
            .did_you_mean()
            .is_some_and(|names| { names.iter().any(|name| name == "orbit.task.show") })
    );
    assert!(runtime.list_tasks().expect("list tasks").is_empty());
}

#[test]
fn task_add_tool_accepts_disabled_required_tools_with_a_warning() {
    let (_root, runtime, _repo_root) = test_runtime();
    runtime
        .disable_tool("orbit.task.list")
        .expect("disable tool");

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Keep disabled tool requirement",
                "description": "The requirement remains durable.",
                "complexity": "low",
                "workspace": ".",
                "required_tools": ["orbit.task.list"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("disabled registered tool is accepted");

    assert_eq!(output["required_tools"], json!(["orbit.task.list"]));
    assert!(output["warnings"].as_array().is_some_and(|warnings| {
        warnings.iter().any(|warning| {
            warning.as_str().is_some_and(|message| {
                message.contains("orbit.task.list") && message.contains("disabled")
            })
        })
    }));
}

/// ORB-12208: `orbit.task.add` must validate `context_files` against the same
/// root `add_task` stores them relative to (the repository root), not against
/// a sub-directory path-form `workspace` selector. `orbit.task.update`
/// (ORB-12197) already passes `None`; this covers the tool that was left
/// behind, in both directions: a selector valid only relative to the
/// sub-directory must be rejected rather than stored dead, and a selector
/// valid at the repository root must be accepted rather than false-rejected.
#[test]
fn task_add_tool_validates_context_selectors_against_repo_root_not_workspace_subdir() {
    let (_root, runtime, repo_root) = test_runtime();
    fs::create_dir_all(repo_root.join("src")).expect("create repo-root src dir");
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n").expect("write repo-root file");
    let sub_dir = repo_root.join("crates").join("orbit-cli").join("src");
    fs::create_dir_all(&sub_dir).expect("create workspace sub-directory");
    fs::write(sub_dir.join("lib.rs"), "pub fn ok() {}\n").expect("write sub-directory file");
    let workspace = repo_root.join("crates").join("orbit-cli");

    // A selector that only resolves relative to the sub-directory workspace
    // (`crates/orbit-cli/src/lib.rs` as `file:src/lib.rs`) does not exist at
    // the repository root `add_task` stores it against, so it must be
    // rejected rather than accepted and stored dead.
    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.add",
        json!({
            "title": "Rejects sub-directory-only selector",
            "description": "file:src/lib.rs only resolves under the workspace sub-directory.",
            "complexity": "low",
            "workspace": workspace.to_string_lossy(),
            "context_files": ["file:src/lib.rs"],
        }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    ));
    assert!(
        message.contains("file:src/lib.rs") && message.contains("does not resolve"),
        "{message}"
    );

    // A selector valid at the repository root (`file:src/main.rs`) must be
    // accepted even though the call's `workspace` is a sub-directory, because
    // `add_task` canonicalizes and stores selectors relative to the
    // repository root.
    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Accepts repo-root selector",
                "description": "file:src/main.rs resolves at the repository root.",
                "complexity": "low",
                "workspace": workspace.to_string_lossy(),
                "context_files": ["file:src/main.rs"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("repo-root-valid selector is accepted from a sub-directory workspace");
    assert_eq!(
        output.get("context_files"),
        Some(&json!(["file:src/main.rs"]))
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
fn task_add_tool_rejects_dropped_task_types_and_retired_status() {
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

    let message = invalid_input_message(runtime.execute_tool_command(
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
    ));
    assert!(message.contains("status"), "{message}");
    assert!(message.contains("orbit.task.update"), "{message}");
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

/// [ORB-12245] The agent-facing update surface is governed by the same
/// lifecycle table the CLI and dashboard use, and it has no override: `force`
/// is refused outright, so an agent cannot grant itself one.
#[test]
fn task_update_tool_enforces_the_lifecycle_and_refuses_force() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Fabricated completion",
        "An agent must not mark unstarted work done.",
        TaskStatus::Proposed,
        &[],
    );
    let agent = Some("codex".to_string());
    let model = Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string());

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": task.id.clone(), "status": "done" }),
        agent.clone(),
        model.clone(),
    ));
    assert_eq!(
        message,
        format!(
            "task '{}' cannot move from 'proposed' to 'done': 'done' is reachable only from 'review'",
            task.id
        )
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("reread task").status,
        TaskStatus::Proposed
    );

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": task.id.clone(), "status": "done", "force": true }),
        agent,
        model,
    ));
    assert!(message.contains("does not accept `force`"), "{message}");
    assert_eq!(
        runtime.get_task(&task.id).expect("reread task").status,
        TaskStatus::Proposed
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
fn task_add_tool_rejects_retired_dependencies() {
    let (_root, runtime, repo_root) = test_runtime();
    let dependency = create_task(
        &runtime,
        &repo_root,
        "Dependency task",
        "Existing task that must finish first.",
        TaskStatus::Backlog,
        &[],
    );

    let message = invalid_input_message(runtime.execute_tool_command(
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
    ));
    assert!(message.contains("dependencies"), "{message}");
    assert!(message.contains("orbit.task.update"), "{message}");
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
        listed["tasks"][0]["required_tools"],
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
    assert_task_list_titles(&listed, &["Legacy crew task"]);
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
        "orbit.task.update",
        json!({ "id": task.id, "status": "in_progress" }),
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

#[test]
fn task_update_routes_approval_start_and_blocked_restart_through_transition_bodies() {
    let (_root, runtime, _repo_root) = test_runtime();
    let task = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Lifecycle update task".to_string(),
            description: "Exercise the single registered lifecycle surface.".to_string(),
            plan: "Run the lifecycle checks.".to_string(),
            ..Default::default()
        })
        .expect("add proposed task");
    let agent = Some("codex".to_string());
    let model = Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string());

    let approved = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "status": "backlog",
                "note": "approved through update",
                "fields": ["status", "history"],
            }),
            agent.clone(),
            model.clone(),
        )
        .expect("approve through update");
    assert_eq!(approved["status"], "backlog");
    assert!(approved["history"].as_array().is_some_and(|history| {
        history.iter().any(|entry| {
            entry["event"] == "proposal_approved" && entry["note"] == "approved through update"
        })
    }));

    runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "status": "in_progress",
                "note": "picked up through update"
            }),
            agent.clone(),
            model.clone(),
        )
        .expect("start through update");
    assert!(
        runtime
            .list_session_events(20)
            .expect("events")
            .iter()
            .any(
                |event| event.event_type == "TaskStarted" && event.payload["data"]["id"] == task.id
            )
    );

    runtime
        .update_task(
            &task.id,
            crate::application::task::TaskUpdateParams {
                status: Some(TaskStatus::Blocked),
                ..Default::default()
            },
        )
        .expect("seed blocked status");
    let restarted = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task.id, "status": "in_progress" }),
            agent.clone(),
            model.clone(),
        )
        .expect("restart blocked task through update");
    assert_eq!(restarted["status"], "in-progress");

    runtime
        .update_task(
            &task.id,
            crate::application::task::TaskUpdateParams {
                status: Some(TaskStatus::Rejected),
                ..Default::default()
            },
        )
        .expect("seed rejected status");
    let error = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task.id, "status": "in_progress" }),
            agent,
            model,
        )
        .expect_err("rejected is not a pickup state");
    assert!(error.to_string().contains("start requires"), "{error}");
}

/// ORB-12338: a non-approval `status: backlog` write is an ordinary governed
/// update. Intercepting every backlog request as approval refused field edits
/// on `someday → backlog` and named a transition that body would not run.
#[test]
fn task_update_combines_non_approval_backlog_with_field_edits() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Someday task",
        "Needs a combined backlog + priority write.",
        TaskStatus::Someday,
        &[],
    );
    let agent = Some("codex".to_string());
    let model = Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string());

    let updated = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "status": "backlog",
                "priority": "high",
                "tags": ["qa"],
            }),
            agent.clone(),
            model.clone(),
        )
        .expect("someday → backlog may include field edits");
    assert_eq!(updated["status"], "backlog");
    assert_eq!(updated["priority"], "high");
    assert_eq!(updated["tags"], json!(["qa"]));

    let proposed = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Still proposed".to_string(),
            description: "Approval still refuses extras.".to_string(),
            ..Default::default()
        })
        .expect("add proposed task");
    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({
            "id": proposed.id,
            "status": "backlog",
            "priority": "high",
        }),
        agent,
        model,
    ));
    assert!(
        message.contains("proposed")
            && message.contains("approval")
            && message.contains("priority"),
        "approval extras must name the proposed → backlog transition: {message}"
    );
    assert!(
        !message.contains("guarded start"),
        "a backlog write must not be labelled a start: {message}"
    );
}

/// ORB-12338: the start body must accept the plan the lifecycle precondition
/// reads from the same write. A blocked task with an empty plan can move to
/// in-progress in one call, and that write still records `TaskStarted`.
#[test]
fn task_update_start_accepts_plan_on_the_same_write() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Blocked without a plan",
        "Needs plan + start in one tool call.",
        TaskStatus::Blocked,
        &[],
    );
    let agent = Some("codex".to_string());
    let model = Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string());

    let missing = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": task.id, "status": "in-progress" }),
        agent.clone(),
        model.clone(),
    ));
    assert!(
        missing.contains("execution plan"),
        "an empty plan still blocks start: {missing}"
    );

    let started = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task.id,
                "status": "in-progress",
                "plan": "1. probe",
            }),
            agent,
            model,
        )
        .expect("plan + in-progress is one start write");
    assert_eq!(started["status"], "in-progress");
    assert_eq!(started["plan"], "1. probe");
    assert!(
        runtime
            .list_session_events(20)
            .expect("events")
            .iter()
            .any(
                |event| event.event_type == "TaskStarted" && event.payload["data"]["id"] == task.id
            ),
        "absorbing plan must still run the start body: {started}"
    );
}

/// ORB-12344: `status: in-progress` routing is decided by the transition, not
/// by which extra keys the payload carries. A non-absorbable field must not
/// turn a pickup-state refusal into an ordinary success, and must not drop
/// `TaskStarted` on a real start.
#[test]
fn task_update_in_progress_outcome_does_not_depend_on_extra_fields() {
    let (_root, runtime, repo_root) = test_runtime();
    let agent = Some("codex".to_string());
    let model = Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string());
    let seed_plan = |id: &str| {
        runtime
            .update_task(
                id,
                crate::application::task::TaskUpdateParams {
                    plan: Some(
                        "Keep a plan so an ordinary in-progress write would be allowed."
                            .to_string(),
                    ),
                    ..Default::default()
                },
            )
            .expect("seed plan");
    };

    let rejected = create_task(
        &runtime,
        &repo_root,
        "Rejected task",
        "Extra fields must not bypass the start refusal.",
        TaskStatus::Rejected,
        &[],
    );
    seed_plan(&rejected.id);
    let rejected_plain = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": rejected.id, "status": "in-progress" }),
        agent.clone(),
        model.clone(),
    ));
    let rejected_with_priority = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({
            "id": rejected.id,
            "status": "in-progress",
            "priority": "high",
        }),
        agent.clone(),
        model.clone(),
    ));
    assert_eq!(
        rejected_plain, rejected_with_priority,
        "rejected → in-progress must refuse the same way with or without extra fields"
    );
    assert!(
        rejected_plain.contains("start requires"),
        "rejected is not a pickup state: {rejected_plain}"
    );

    let review = create_task(
        &runtime,
        &repo_root,
        "Review task",
        "Extra fields must not bypass the start refusal.",
        TaskStatus::Review,
        &[],
    );
    seed_plan(&review.id);
    let review_plain = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": review.id, "status": "in-progress" }),
        agent.clone(),
        model.clone(),
    ));
    let review_with_priority = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({
            "id": review.id,
            "status": "in-progress",
            "priority": "high",
        }),
        agent.clone(),
        model.clone(),
    ));
    assert_eq!(
        review_plain, review_with_priority,
        "review → in-progress must refuse the same way with or without extra fields"
    );
    assert!(
        review_plain.contains("start requires"),
        "review is not a pickup state: {review_plain}"
    );

    let backlog = create_task(
        &runtime,
        &repo_root,
        "Backlog task",
        "Start with an extra field edit must still record TaskStarted.",
        TaskStatus::Backlog,
        &[],
    );
    seed_plan(&backlog.id);
    let started = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": backlog.id,
                "status": "in-progress",
                "priority": "high",
                "tags": ["x"],
                "crew": " sol ",
            }),
            agent.clone(),
            model.clone(),
        )
        .expect("backlog start may include field edits");
    assert_eq!(started["status"], "in-progress");
    assert_eq!(started["priority"], "high");
    assert_eq!(started["tags"], json!(["x"]));
    assert_eq!(started["crew"], "sol");
    assert!(
        runtime
            .list_session_events(20)
            .expect("events")
            .iter()
            .any(|event| {
                event.event_type == "TaskStarted" && event.payload["data"]["id"] == backlog.id
            }),
        "extra fields on a pickup start must still run the start body: {started}"
    );

    let proposed = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Proposed start with extras".to_string(),
            description: "Approval-plus-pickup must keep proposal_approved.".to_string(),
            plan: "1. start with field edits.".to_string(),
            ..Default::default()
        })
        .expect("add proposed task");
    let picked_up = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": proposed.id,
                "status": "in-progress",
                "priority": "high",
                "tags": ["x"],
                "fields": ["status", "priority", "tags", "history"],
            }),
            agent,
            model,
        )
        .expect("proposed start may include field edits");
    assert_eq!(picked_up["status"], "in-progress");
    assert_eq!(picked_up["priority"], "high");
    assert_eq!(picked_up["tags"], json!(["x"]));
    assert!(
        picked_up["history"].as_array().is_some_and(|history| {
            history
                .iter()
                .any(|entry| entry["event"] == "proposal_approved")
        }),
        "proposed → in-progress must still record proposal_approved: {picked_up}"
    );
    assert!(
        runtime
            .list_session_events(20)
            .expect("events")
            .iter()
            .any(|event| {
                event.event_type == "TaskStarted" && event.payload["data"]["id"] == proposed.id
            }),
        "proposed start with extra fields must still emit TaskStarted: {picked_up}"
    );
}

/// A start from `proposed` is an approval followed by a start, and the
/// history must chain that way: `proposal_approved (proposed → backlog)` then
/// `started (backlog → in_progress)`. The plan supplied on that same write is
/// attributed to the supplied agent family, as the worker path does.
#[test]
fn task_update_start_from_proposed_chains_history_edges_and_attributes_plan() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo_root) = test_runtime();
    let proposed = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Proposed start provenance".to_string(),
            description: "Plan arrives on the start write.".to_string(),
            ..Default::default()
        })
        .expect("add proposed task without a plan");
    assert_eq!(proposed.planned_by, None);

    let started = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": proposed.id,
                "status": "in-progress",
                "plan": "Probe gates, then reject.",
                "model": "claude",
                "note": "approved on pickup",
                "fields": ["status", "planned_by", "history"],
            }),
            None,
            None,
        )
        .expect("proposed start with a plan succeeds");
    assert_eq!(started["status"], "in-progress");
    assert_eq!(
        started["planned_by"].as_str(),
        Some("claude"),
        "a plan written on the start write is attributed to the supplied model family: {started}"
    );

    let history = started["history"].as_array().expect("history array");
    let lifecycle = history
        .iter()
        .filter(|entry| {
            matches!(
                entry["event"].as_str(),
                Some("proposal_approved" | "started")
            )
        })
        .map(|entry| {
            (
                entry["event"].as_str().unwrap_or_default().to_string(),
                entry["from_status"].as_str().map(str::to_string),
                entry["to_status"].as_str().map(str::to_string),
                entry["note"].as_str().map(str::to_string),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lifecycle,
        vec![
            (
                "proposal_approved".to_string(),
                Some("proposed".to_string()),
                Some("backlog".to_string()),
                Some("approved on pickup".to_string()),
            ),
            (
                "started".to_string(),
                Some("backlog".to_string()),
                Some("in_progress".to_string()),
                None,
            ),
        ],
        "history must replay as proposed → backlog → in_progress: {started}"
    );
}

/// A managed worker identity outranks a model string carried in the task
/// payload. This fixture mirrors the engine envelope without changing the
/// production precedence rule.
#[test]
fn task_update_start_uses_managed_worker_identity_over_payload_model() {
    let _env = managed_tool_identity_env_guard(
        "jrun-test-managed-task-provenance",
        "codex",
        orbit_common::test_fixtures::TEST_CODEX_MODEL,
    );
    let (_root, runtime, _repo_root) = test_runtime();
    let proposed = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Managed provenance start".to_string(),
            description: "The worker identity must win over payload data.".to_string(),
            ..Default::default()
        })
        .expect("add proposed task without a plan");

    let started = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": proposed.id,
                "status": "in-progress",
                "plan": "Preserve trusted worker attribution.",
                "model": "claude",
            }),
            None,
            None,
        )
        .expect("managed proposed start succeeds");

    assert_eq!(started["planned_by"].as_str(), Some("codex"), "{started}");
}

/// The start write only infers `planned_by`; an existing planner and an
/// explicit override both win over the supplied model.
#[test]
fn task_update_start_keeps_existing_and_explicit_planned_by() {
    let (_root, runtime, _repo_root) = test_runtime();
    let planned = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Already planned".to_string(),
            description: "Planner recorded at creation.".to_string(),
            plan: "Original plan.".to_string(),
            ..Default::default()
        })
        .expect("add planned task");
    let original_planner = planned
        .planned_by
        .clone()
        .expect("a plan at creation records its planner");

    let started = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": planned.id,
                "status": "in-progress",
                "plan": "Revised on pickup.",
                "model": "claude",
            }),
            None,
            None,
        )
        .expect("start with a revised plan succeeds");
    assert_eq!(started["plan"], "Revised on pickup.");
    assert_eq!(
        started["planned_by"].as_str(),
        Some(original_planner.as_str()),
        "an existing planner is not overwritten by the start write: {started}"
    );

    let unplanned = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Explicit planner".to_string(),
            description: "Caller names the planner.".to_string(),
            ..Default::default()
        })
        .expect("add unplanned task");
    let started = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": unplanned.id,
                "status": "in-progress",
                "plan": "Plan from a human.",
                "planned_by": "manual-planner",
                "model": "claude",
            }),
            None,
            None,
        )
        .expect("start with an explicit planner succeeds");
    assert_eq!(
        started["planned_by"].as_str(),
        Some("manual-planner"),
        "an explicit planned_by wins over inference: {started}"
    );
}

/// ORB-12474: the guarded start body must pass field edits through the same
/// dependency validator as an ordinary update before either reaches storage.
#[test]
fn task_update_start_rejects_self_dependency_like_an_ordinary_update() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "Self dependency",
        "A start cannot create its own readiness cycle.",
        TaskStatus::Backlog,
        &[],
    );
    runtime
        .update_task(
            &task.id,
            crate::application::task::TaskUpdateParams {
                plan: Some("1. refuse the invalid dependency.".to_string()),
                ..Default::default()
            },
        )
        .expect("seed plan");
    let agent = Some("codex".to_string());
    let model = Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string());

    let ordinary = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": task.id, "dependencies": [task.id] }),
        agent.clone(),
        model.clone(),
    ));
    let start = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({
            "id": task.id,
            "status": "in-progress",
            "dependencies": [task.id],
        }),
        agent,
        model,
    ));

    assert_eq!(start, ordinary);
    assert!(
        start.contains("cannot declare a self-dependency"),
        "self-dependency refusal should explain the cycle: {start}"
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("task remains").status,
        TaskStatus::Backlog
    );
}

/// ORB-12474: opting into missing context skips only the existence check. It
/// does not skip canonicalization or workspace containment on a start write.
#[test]
fn task_update_start_normalizes_and_contains_context_like_an_ordinary_update() {
    let (_root, runtime, repo_root) = test_runtime();
    let ordinary_task = create_task(
        &runtime,
        &repo_root,
        "Ordinary context update",
        "Control arm for selector normalization.",
        TaskStatus::Backlog,
        &[],
    );
    let start_task = create_task(
        &runtime,
        &repo_root,
        "Start context update",
        "Start arm for selector normalization.",
        TaskStatus::Backlog,
        &[],
    );
    runtime
        .update_task(
            &start_task.id,
            crate::application::task::TaskUpdateParams {
                plan: Some("1. normalize the selector.".to_string()),
                ..Default::default()
            },
        )
        .expect("seed plan");
    let agent = Some("codex".to_string());
    let model = Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string());
    let selector = "future/../future/new.rs";

    let ordinary = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": ordinary_task.id,
                "context_files": [selector],
                "allow_missing_context": true,
            }),
            agent.clone(),
            model.clone(),
        )
        .expect("ordinary update normalizes missing selector");
    let started = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": start_task.id,
                "status": "in-progress",
                "context_files": [selector],
                "allow_missing_context": true,
            }),
            agent.clone(),
            model.clone(),
        )
        .expect("start update normalizes missing selector");
    assert_eq!(started["context_files"], ordinary["context_files"]);
    assert_eq!(started["context_files"], json!(["file:future/new.rs"]));

    let outside = create_task(
        &runtime,
        &repo_root,
        "Outside context",
        "Containment applies even when existence does not.",
        TaskStatus::Backlog,
        &[],
    );
    runtime
        .update_task(
            &outside.id,
            crate::application::task::TaskUpdateParams {
                plan: Some("1. refuse the escaping selector.".to_string()),
                ..Default::default()
            },
        )
        .expect("seed plan");
    let ordinary_error = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({
            "id": outside.id,
            "context_files": ["../outside.rs"],
            "allow_missing_context": true,
        }),
        agent.clone(),
        model.clone(),
    ));
    let start_error = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({
            "id": outside.id,
            "status": "in-progress",
            "context_files": ["../outside.rs"],
            "allow_missing_context": true,
        }),
        agent,
        model,
    ));
    assert_eq!(start_error, ordinary_error);
    assert!(
        start_error.contains("must remain inside workspace"),
        "{start_error}"
    );
}

/// ORB-12474: orchestrator edits retain their ordinary update rules when the
/// same write also starts the task.
#[test]
fn task_update_start_validates_canonicalizes_and_gates_orchestrator() {
    let (_root, runtime, repo_root) = test_runtime();
    let ordinary_task = create_task(
        &runtime,
        &repo_root,
        "Ordinary orchestrator update",
        "Control arm for canonicalization.",
        TaskStatus::Backlog,
        &[],
    );
    let start_task = create_task(
        &runtime,
        &repo_root,
        "Start orchestrator update",
        "Start arm for canonicalization.",
        TaskStatus::Backlog,
        &[],
    );
    runtime
        .update_task(
            &start_task.id,
            crate::application::task::TaskUpdateParams {
                plan: Some("1. start with canonical attribution.".to_string()),
                ..Default::default()
            },
        )
        .expect("seed plan");
    let agent = Some("codex".to_string());
    let model = Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string());
    let crew_error_message = |result: Result<Value, orbit_common::OrbitError>| match result {
        Err(orbit_common::OrbitError::InvalidInput(message))
        | Err(orbit_common::OrbitError::InvalidInputDiagnostic { message, .. }) => message,
        Err(error) => panic!("expected invalid crew input, got {error:?}"),
        Ok(value) => panic!("expected invalid crew input, got {value}"),
    };

    let ordinary = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": ordinary_task.id, "orchestrator": " sol " }),
            agent.clone(),
            model.clone(),
        )
        .expect("ordinary update canonicalizes orchestrator");
    let started = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": start_task.id,
                "status": "in-progress",
                "orchestrator": " sol ",
            }),
            agent.clone(),
            model.clone(),
        )
        .expect("start update canonicalizes orchestrator");
    assert_eq!(ordinary["orchestrator"], "sol");
    assert_eq!(started["orchestrator"], ordinary["orchestrator"]);

    let unknown = create_task(
        &runtime,
        &repo_root,
        "Unknown orchestrator",
        "Both paths reject an unknown crew.",
        TaskStatus::Backlog,
        &[],
    );
    runtime
        .update_task(
            &unknown.id,
            crate::application::task::TaskUpdateParams {
                plan: Some("1. refuse an unknown crew.".to_string()),
                ..Default::default()
            },
        )
        .expect("seed plan");
    let ordinary_unknown = crew_error_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": unknown.id, "orchestrator": "does-not-exist" }),
        agent.clone(),
        model.clone(),
    ));
    let start_unknown = crew_error_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({
            "id": unknown.id,
            "status": "in-progress",
            "orchestrator": "does-not-exist",
        }),
        agent.clone(),
        model.clone(),
    ));
    assert_eq!(start_unknown, ordinary_unknown);

    let someday = create_task(
        &runtime,
        &repo_root,
        "Someday orchestrator",
        "Orchestrator is immutable in someday.",
        TaskStatus::Someday,
        &[],
    );
    runtime
        .update_task(
            &someday.id,
            crate::application::task::TaskUpdateParams {
                plan: Some("1. enforce the attribution gate.".to_string()),
                ..Default::default()
            },
        )
        .expect("seed plan");
    let ordinary_gate = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": someday.id, "orchestrator": "sol" }),
        agent.clone(),
        model.clone(),
    ));
    let start_gate = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({
            "id": someday.id,
            "status": "in-progress",
            "orchestrator": "sol",
        }),
        agent,
        model,
    ));
    assert_eq!(start_gate, ordinary_gate);
    assert!(
        start_gate.contains("orchestrator can only be changed while proposed or backlog"),
        "{start_gate}"
    );
}

/// ORB-12474: the task update handler must forward the authenticated caller's
/// run id to artifact storage on both ordinary and start writes.
#[test]
fn task_update_start_preserves_artifact_owner_run_id() {
    use orbit_tools::ReservationOwnerContext;
    use orbit_types::workflow::automation::{EVIDENCE_AUTHORITY_ARTIFACT, EvidenceSubmission};

    let (_root, runtime, repo_root) = test_runtime();
    let ordinary_task = create_task(
        &runtime,
        &repo_root,
        "Ordinary artifact update",
        "Control arm for owner attribution.",
        TaskStatus::Backlog,
        &[],
    );
    let start_task = create_task(
        &runtime,
        &repo_root,
        "Start artifact update",
        "Start arm for owner attribution.",
        TaskStatus::Backlog,
        &[],
    );
    runtime
        .update_task(
            &start_task.id,
            crate::application::task::TaskUpdateParams {
                plan: Some("1. attach owned evidence while starting.".to_string()),
                ..Default::default()
            },
        )
        .expect("seed plan");
    let run_id = "jrun-owned-artifact";
    let owner = || ReservationOwnerContext {
        owner_run_id: run_id.to_string(),
        owner_metadata_json: Some(r#"{"source":"test"}"#.to_string()),
    };
    let input = |id: &str, start: bool| {
        let mut value = json!({
            "id": id,
            "artifacts": {"automation-coverage.json": "coverage"},
        });
        if start {
            value["status"] = json!("in-progress");
        }
        value
    };

    super::super::task_tools::update(
        &runtime,
        input(&ordinary_task.id, false),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        Some(owner()),
        None,
    )
    .expect("ordinary update stores owned artifact");
    super::super::task_tools::update(
        &runtime,
        input(&start_task.id, true),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        Some(owner()),
        None,
    )
    .expect("start update stores owned artifact");

    for task_id in [&ordinary_task.id, &start_task.id] {
        let witness = runtime
            .get_task_artifact(task_id, EVIDENCE_AUTHORITY_ARTIFACT)
            .expect("read authority witness")
            .expect("owner run creates authority witness");
        let submission: EvidenceSubmission =
            serde_json::from_slice(&witness.content).expect("parse authority witness");
        assert_eq!(submission.run_id, run_id);
        assert_eq!(submission.action_id, *task_id);
    }
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

/// ORB-12116: the create-time assessment contract also holds on update, so an
/// agent cannot assess a task at creation and clear it one call later.
#[test]
fn task_update_tool_rejects_unassessed_complexity_and_keeps_the_stored_value() {
    let (_root, runtime, _repo_root) = test_runtime();
    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Assessed on create",
                "description": "Update must not be able to undo the assessment.",
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    let task_id = added["id"].as_str().expect("task id");

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.update",
        json!({ "id": task_id, "complexity": "unassessed" }),
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    ));
    assert_eq!(
        message,
        TaskComplexity::Unassessed
            .require_assessed()
            .expect_err("unassessed is not an assessed value")
    );

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task_id }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show succeeds");
    assert_eq!(
        shown.get("complexity"),
        Some(&json!("low")),
        "a rejected update leaves the stored complexity alone"
    );
}

/// [ORB-12605] `xhard` is an ordinary assessed value on the agent-facing
/// create and update surfaces; only `unassessed` stays reserved there.
#[test]
fn task_tools_accept_xhard_on_create_and_update() {
    let (_root, runtime, _repo_root) = test_runtime();
    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Reserved tier work",
                "description": "The top tier is assignable through the tool surface.",
                "complexity": "xhard",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add accepts xhard");
    assert_eq!(added.get("complexity"), Some(&json!("xhard")));
    let task_id = added["id"].as_str().expect("task id");

    let updated = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task_id, "complexity": "hard" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update accepts an assessed tier");
    assert_eq!(updated.get("complexity"), Some(&json!("hard")));

    let raised = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task_id, "complexity": "xhard" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update accepts xhard");
    assert_eq!(raised.get("complexity"), Some(&json!("xhard")));
}

/// Automated callers write through the application layer, not the tool
/// surface, so task-pilot, auto-task mint and other system paths keep the
/// ability to store the explicit non-answer.
#[test]
fn automated_update_paths_may_still_store_unassessed_complexity() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "System owned complexity",
        "Automated paths keep the explicit non-answer.",
        TaskStatus::Backlog,
        &[],
    );

    let updated = runtime
        .update_task(
            &task.id,
            crate::application::task::TaskUpdateParams {
                complexity: Some(TaskComplexity::Unassessed),
                ..Default::default()
            },
        )
        .expect("application-layer update succeeds");
    assert_eq!(updated.complexity, Some(TaskComplexity::Unassessed));
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
    assert_ne!(
        added.get("crew"),
        Some(&json!("sol")),
        "attribution must not select an execution crew; [ORB-12717] creation \
         assigns one from the pools or the default instead"
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
fn task_write_responses_omit_sidecars_unless_projected() {
    let (_root, runtime, _repo_root) = test_runtime();
    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Write response shape",
                "description": "Sidecars are opt-in on mutation responses.",
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
    assert!(added.get("comments").is_none());
    assert!(added.get("history").is_none());

    let task_id = added["id"].as_str().expect("task id").to_string();
    let updated = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task_id, "comment": "record a comment" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update tool succeeds");
    assert!(updated.get("comments").is_none());
    assert!(updated.get("history").is_none());

    let projected = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task_id,
                "fields": ["comments", "history"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("projected task update succeeds");
    assert!(projected["comments"].as_array().is_some_and(|comments| {
        comments
            .iter()
            .any(|comment| comment["message"] == "record a comment")
    }));
    assert!(
        projected["history"]
            .as_array()
            .is_some_and(|history| { history.iter().any(|entry| entry["event"] == "created") })
    );

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task_id }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task show tool succeeds");
    assert!(shown["comments"].as_array().is_some());
    assert!(shown["history"].as_array().is_some());
}

#[test]
fn task_write_response_projects_relations_with_a_friction_target() {
    // DANI-10379 regression: `resolves` relations may legitimately target a
    // friction id (e.g. `F2026-05-001`) rather than a task id. Projecting
    // `relations` on a write response must skip point-reading non-task
    // targets instead of sending them through `get_task_row`, which rejects
    // them with `InvalidInput` rather than reporting `NotFound`.
    let (_root, runtime, _repo_root) = test_runtime();
    let friction_target = "F2026-05-001";
    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Resolve a friction via relation",
                "description": "Write responses must not point-read non-task relation targets.",
                "complexity": "low",
                "workspace": ".",
                "relations": [
                    {"type": "resolves", "target": friction_target}
                ],
                "fields": ["id", "relations"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("projecting relations with a friction target does not error");

    let relations = added["relations"].as_array().expect("relations array");
    let resolves = relations
        .iter()
        .find(|relation| relation["type"] == "resolves")
        .expect("resolves relation");
    assert_eq!(resolves["target"], json!(friction_target));
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
        task_list_items(&ready)
            .iter()
            .any(|task| task["id"] == task_id),
        "foreign dependencies do not gate readiness"
    );
}

#[test]
fn mcp_task_show_and_update_resolve_cross_workspace_references_from_status_index() {
    let (_root, runtime, repo_root) = test_runtime();
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root()))
        .expect("open shared task registry");
    let foreign_partition = "foreign-workspace-aaaaaa";
    registry
        .register_workspace(RegisterWorkspaceParams {
            partition_id: foreign_partition.to_string(),
            slug: "Foreign workspace".to_string(),
            repo_fingerprint: None,
        })
        .expect("register foreign workspace");

    let foreign = coordination_task_backends(registry, foreign_partition.to_string());
    let target = foreign
        .task
        .create_task(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: "Cross-workspace target".to_string(),
            description: "The source task lives in another partition.".to_string(),
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
            status: TaskStatus::Done,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create foreign target");
    let source = create_task(
        &runtime,
        &repo_root,
        "Cross-workspace source",
        "The MCP projections must resolve the foreign target status.",
        TaskStatus::Backlog,
        &[],
    );
    let workspace = repo_root.to_string_lossy().into_owned();
    let update = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.update",
            json!({
                "id": source.id.clone(),
                "dependencies": [target.id.clone()],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            ToolEntryPoint::Mcp,
            ToolSessionContext::with_workspace(workspace.clone()),
        )
        .expect("MCP update resolves cross-workspace target")
        .value;

    assert_eq!(
        update["resolved_dependencies"],
        json!([format!("{} [done]", target.id)])
    );

    let updated_with_relation = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.update",
            json!({
                "id": source.id.clone(),
                "relations": [
                    {"type": "blocked_by", "target": target.id.clone()},
                    {"type": "related_to", "target": target.id.clone()},
                ],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            ToolEntryPoint::Mcp,
            ToolSessionContext::with_workspace(workspace.clone()),
        )
        .expect("MCP relation update resolves cross-workspace target")
        .value;
    assert_eq!(
        updated_with_relation["resolved_dependencies"],
        json!([format!("{} [done]", target.id)])
    );
    assert!(updated_with_relation["relations"][0]["verification"].is_null());
    assert!(updated_with_relation["relations"][1]["verification"].is_null());

    let shown = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.show",
            json!({"id": source.id.clone()}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            ToolEntryPoint::Mcp,
            ToolSessionContext::with_workspace(workspace),
        )
        .expect("MCP show resolves cross-workspace target")
        .value;
    assert_eq!(
        shown["resolved_dependencies"],
        json!([format!("{} [done]", target.id)])
    );
    assert!(shown["relations"][0]["verification"].is_null());
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
fn task_show_tool_composes_field_projection_with_context() {
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
        "Projected related docs",
        "Exercise projected MCP context injection.",
        TaskStatus::Backlog,
        &["file:crates/orbit-cli/src/command/docs.rs"],
    );

    let shown = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({
                "id": task.id,
                "fields": ["title"],
                "with_context": true,
                "max_docs": 1,
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("projected task show with context succeeds");

    assert_eq!(shown["title"], "Projected related docs");
    assert_eq!(shown["related_docs"][0]["path"], "docs/cli.md");
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
fn task_add_tool_rejects_retired_external_refs() {
    let (_root, runtime, _repo_root) = test_runtime();

    let message = invalid_input_message(runtime.execute_tool_command(
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
    ));
    assert!(message.contains("external_refs"), "{message}");
    assert!(message.contains("orbit.task.update"), "{message}");
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
        Some("codex")
    );
}

#[test]
fn task_add_tool_refuses_unrecognized_model() {
    let _env =
        orbit_common::test_env::unset(orbit_common::test_env::AGENT_IDENTITY_ENV.iter().copied());
    let (_root, runtime, _repo_root) = test_runtime();

    let message = invalid_input_message(runtime.execute_tool_command(
        "orbit.task.add",
        json!({
            "title": "Refuse llama provenance",
            "description": "llama is not a family.",
            "complexity": "low",
            "workspace": ".",
            "model": "llama",
        }),
        None,
        None,
    ));
    assert!(message.contains("llama"), "{message}");
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
                "fields": ["id", "source_task_id", "updated_at", "history"],
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
                "fields": ["source_task_id", "status"],
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
                "fields": ["source_task_id", "history"],
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
    // `source_task_id` is update-only; seed the update target without it.
    let (_root, runtime, _repo_root) = test_runtime();
    let unresolved_from_update = "ORB-99999";

    let update_target = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Bug without resolved source",
                "description": "A bug whose unresolved source ID should be rejected atomically.",
                "complexity": "low",
                "workspace": ".",
                "type": "bug",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool succeeds");
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

    // `file:src/future.rs` does not exist yet, so the tool's default existence
    // guard would refuse it; the explicit escape is how a caller records a
    // target the task is about to create.
    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task_id,
                "context_files": ["[\"file:src/main.rs\", \"file:src/future.rs\"]"],
                "allow_missing_context": true,
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

/// The existence guard lives on the tool surface, not in `add_task` /
/// `update_task`: an agent typo must not ship a task whose context is dead on
/// arrival, and the rejected write must leave the task untouched.
#[test]
fn task_tools_reject_context_selectors_that_do_not_exist() {
    let (_root, runtime, repo_root) = test_runtime();
    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::write(src_dir.join("lib.rs"), "pub fn before() {}\n").expect("write source file");

    let add_error = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Missing context",
                "description": "Exercise the tool-surface existence guard.",
                "complexity": "low",
                "workspace": repo_root.to_string_lossy(),
                "context_files": ["file:src/typo.rs"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect_err("task add tool must reject a missing selector");
    assert!(
        add_error.to_string().contains("file:src/typo.rs"),
        "{add_error}"
    );
    assert!(
        runtime.list_tasks().expect("list tasks").is_empty(),
        "a rejected add must not create a task"
    );

    let added = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Existing context",
                "description": "Exercise the tool-surface existence guard.",
                "complexity": "low",
                "workspace": repo_root.to_string_lossy(),
                "context_files": ["file:src/lib.rs"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add tool accepts an existing selector");
    let task_id = added["id"].as_str().expect("task id").to_string();

    let update_error = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task_id,
                "context_files": ["file:src/typo.rs"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect_err("task update tool must reject a missing selector");
    assert!(
        update_error.to_string().contains("file:src/typo.rs"),
        "{update_error}"
    );

    let preserved = runtime.get_task(&task_id).expect("reload task");
    assert_eq!(
        preserved.context_files,
        vec!["file:src/lib.rs".to_string()],
        "a rejected update must leave context_files unchanged"
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
    assert_task_list_titles(&perf_list, &["Perf task", "Perf bench task"]);

    let both_list = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({ "tag": ["perf", "bench"] }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("list by both tags");
    assert_task_list_titles(&both_list, &["Perf bench task"]);

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
fn task_list_tool_is_status_aware_and_bounded() {
    // ORB-10310: `orbit.task.list` must return every lifecycle status by
    // default (no hidden `backlog,in-progress` subset), with active work first
    // and newest-first within each status bucket.
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
            "status-aware listing fixture",
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
    let listed = task_list_items(&output);
    assert_eq!(
        listed.len(),
        created.len(),
        "every status must be listed by default: {listed:?}"
    );
    for task in &created {
        assert!(
            listed.iter().any(|value| value["id"] == json!(task.id)),
            "task {} ({}) missing from default list",
            task.id,
            task.status
        );
    }
    assert_eq!(
        listed
            .iter()
            .map(|value| value["id"].as_str())
            .collect::<Vec<_>>(),
        vec![
            Some(created[3].id.as_str()),
            Some(created[2].id.as_str()),
            Some(created[1].id.as_str()),
            Some(created[0].id.as_str()),
            Some(created[4].id.as_str()),
        ],
        "default ordering must put non-terminal tasks before terminal tasks"
    );
    assert_eq!(output["total"], json!(created.len()));
    assert_eq!(output["truncated"], json!(false));
}

#[test]
fn task_list_tool_default_limit_returns_newest_fifty() {
    // ORB-10310: the status-aware default is bounded to 50 tasks.
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
    let listed = task_list_items(&output);
    assert_eq!(
        listed.len(),
        50,
        "default limit must bound the response to 50"
    );
    assert_eq!(output["total"], json!(55));
    assert_eq!(output["truncated"], json!(true));
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
    assert_eq!(task_list_items(&limited).len(), 2);
    assert_eq!(limited["total"], json!(3));
    assert_eq!(limited["truncated"], json!(true));

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
    let listed = task_list_items(&output);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], json!(newer_review.id));
    assert_eq!(listed[0]["status"], json!("review"));
    assert_eq!(output["total"], json!(2));
    assert_eq!(output["truncated"], json!(true));
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
        let ids = task_list_items(output)
            .iter()
            .map(|task| task["id"].as_str().expect("task id"))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(backlog.id.as_str()));
        assert!(ids.contains(in_progress.id.as_str()));
        assert!(ids.contains(review.id.as_str()));
        assert_eq!(output["total"], json!(3));
        assert_eq!(output["truncated"], json!(false));
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

/// [ORB-12113] A key the unprojected readout emits is a key a caller must be
/// able to ask for. `resolved_crew` / `crew_model` / `crew_unresolved` were
/// printed in full output and rejected as `fields` selectors, with a
/// valid-values list that did not mention them.
#[test]
fn task_show_projects_every_key_its_unprojected_readout_emits() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task_with_crew(
        &runtime,
        &repo_root,
        "Crew enrichment projection",
        "The readout marks this crew unresolvable on this host.",
        TaskStatus::Backlog,
        &[],
        Some("all-grok"),
    );
    let show = |input: Value| {
        runtime.execute_tool_command(
            "orbit.task.show",
            input,
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
    };

    let shown = show(json!({ "id": task.id })).expect("unprojected task readout");
    let keys = shown
        .as_object()
        .expect("task readout object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert!(keys.iter().any(|key| key == "crew_unresolved"));
    for key in keys {
        let projected = show(json!({ "id": task.id, "field": key })).unwrap_or_else(|error| {
            panic!("`{key}` is emitted unprojected but rejected as a selector: {error}")
        });
        assert_eq!(projected, shown[&key], "projection of `{key}` disagrees");
    }

    // A crew key whose case does not apply answers `null` rather than failing:
    // the readout omits it, and a projection always has an answer.
    assert_eq!(
        show(json!({ "id": task.id, "field": "resolved_crew" })).expect("resolved_crew projects"),
        json!(null)
    );
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
fn task_show_tool_projects_terminal_from_write_refusal_statuses() {
    let (_root, runtime, repo_root) = test_runtime();
    for (status, expected) in [(TaskStatus::Backlog, false), (TaskStatus::Done, true)] {
        let task = create_task(
            &runtime,
            &repo_root,
            "Terminal projection",
            "Exercise derived lifecycle state.",
            status,
            &[],
        );

        let shown = runtime
            .execute_tool_command(
                "orbit.task.show",
                json!({ "id": task.id, "fields": ["terminal"] }),
                Some("codex".to_string()),
                Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            )
            .expect("terminal field projects");
        assert_eq!(shown, json!({ "terminal": expected }), "status {status}");
    }
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
    fn task_artifact_discovery_is_metadata_only_and_bounded() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);
        let png = synthetic_png();
        let large_text = "line of text\n".repeat(10_000); // 130,000 bytes
        attach(&runtime, &id, "diagrams/flow.png", png.clone());
        attach(
            &runtime,
            &id,
            "logs/large.txt",
            large_text.as_bytes().to_vec(),
        );

        // Baseline comparison task with tiny text artifact
        let tiny_id = seeded_task(&runtime, &repo_root);
        attach(&runtime, &tiny_id, "diagrams/flow.png", png.clone());
        attach(&runtime, &tiny_id, "logs/large.txt", b"tiny\n".to_vec());

        let listed = run_tool_as_operator(
            &runtime,
            "orbit.task.show",
            json!({"id": id, "fields": "artifacts"}),
        )
        .expect("list artifacts");
        let rows = listed.as_array().expect("artifact rows");
        assert_eq!(rows.len(), 2);

        for row in rows {
            assert!(
                row.get("content").is_none(),
                "content must not ride along in metadata listing"
            );
            assert!(
                row.get("content_base64").is_none(),
                "content_base64 must not ride along in metadata listing"
            );
            assert!(row.get("path").is_some());
            assert!(row.get("media_type").is_some());
            assert!(row.get("size").is_some());
            assert!(row.get("created_by").is_some());
        }

        assert_eq!(rows[0]["path"], "diagrams/flow.png");
        assert_eq!(rows[0]["size"], png.len());
        assert_eq!(rows[1]["path"], "logs/large.txt");
        assert_eq!(rows[1]["size"], large_text.len());

        // Serialized response growth depends on metadata formatting, not blob size
        let tiny_listed = run_tool_as_operator(
            &runtime,
            "orbit.task.show",
            json!({"id": tiny_id, "fields": "artifacts"}),
        )
        .expect("list artifacts for tiny task");
        let large_json_str = serde_json::to_string(&listed).expect("serialize large listing");
        let tiny_json_str = serde_json::to_string(&tiny_listed).expect("serialize tiny listing");
        let len_diff = (large_json_str.len() as isize - tiny_json_str.len() as isize).abs();
        assert!(
            len_diff < 20,
            "serialized response growth must not depend on blob size ({len_diff} byte diff for 130KB blob)"
        );

        // artifact.get still retrieves payload and preserves byte integrity
        let read_text = get(&runtime, &id, "logs/large.txt");
        assert_eq!(read_text["presentation"], "text");
        assert_eq!(read_text["encoding"], "utf8");
        assert_eq!(read_text["content"], large_text);

        let read_png = get(&runtime, &id, "diagrams/flow.png");
        assert_eq!(read_png["presentation"], "image");
        assert_eq!(read_png["encoding"], "base64");
        let decoded = BASE64_STANDARD
            .decode(read_png["content_base64"].as_str().expect("base64"))
            .expect("decode");
        assert_eq!(decoded, png);

        // Unknown path is rejected
        let error = run_tool_as_operator(
            &runtime,
            "orbit.task.artifact.get",
            json!({"id": id, "path": "nonexistent/file.txt"}),
        )
        .expect_err("unknown artifact path must fail");
        assert!(
            matches!(error, orbit_common::OrbitError::NotFound { .. }),
            "expected NotFound, got {error}"
        );
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

#[test]
fn artifact_provenance_uses_verified_session_identity_only() {
    use orbit_types::tool::McpTransport;
    let (_root, runtime, _workspace) = test_runtime();

    // Local process identity and process host are recorded; caller labels are ignored.
    let mut session = ToolSessionContext {
        process_machine_id: Some("local-process".into()),
        process_host_id: Some("local-host".into()),
        caller_machine_id: Some("claimed-machine".into()),
        caller_host_id: Some("claimed-host".into()),
        ..Default::default()
    };
    let local_origin = runtime
        .artifact_origin(&session)
        .expect("local process identity");
    assert_eq!(local_origin.machine_id, "local-process");
    assert_eq!(local_origin.host_id.as_deref(), Some("local-host"));

    // When process identity is absent, local trusted runtime identity is recorded.
    let runtime_with_identity =
        runtime.with_automation_machine_identity(Some("runtime-machine".into()));
    let session_no_process = ToolSessionContext {
        process_machine_id: None,
        process_host_id: Some("local-host".into()),
        caller_machine_id: Some("claimed-machine".into()),
        caller_host_id: Some("claimed-host".into()),
        ..Default::default()
    };
    let runtime_origin = runtime_with_identity
        .artifact_origin(&session_no_process)
        .expect("local runtime identity");
    assert_eq!(runtime_origin.machine_id, "runtime-machine");
    assert_eq!(runtime_origin.host_id.as_deref(), Some("local-host"));

    // For SSH MCP transport, caller's self-asserted machine/host labels do not
    // become artifact origin, and the destination process is not misattributed.
    session.transport = Some(McpTransport::SshMcp);
    assert!(runtime_with_identity.artifact_origin(&session).is_none());

    // For SSH MCP without process identity, destination runtime is also not misattributed.
    let mut ssh_no_process = session_no_process;
    ssh_no_process.transport = Some(McpTransport::SshMcp);
    assert!(
        runtime_with_identity
            .artifact_origin(&ssh_no_process)
            .is_none()
    );
}

#[test]
fn task_artifacts_retain_trusted_local_provenance_and_reject_ssh_mcp_attribution() {
    use orbit_tools::ToolContext;
    use orbit_types::policy::Role;
    use orbit_types::tool::{McpCapability, McpTransport};
    use std::collections::BTreeSet;

    let (_root, runtime, repo_root) = test_runtime();
    let runtime = runtime.with_automation_machine_identity(Some("local-runtime-box".into()));
    let task = create_task(
        &runtime,
        &repo_root,
        "artifact provenance task",
        "verifies trusted provenance retention",
        TaskStatus::InProgress,
        &[],
    );

    // 1. Local session with process identity records local process provenance.
    let local_source = repo_root.join("local-proc.txt");
    std::fs::write(&local_source, "from local process").expect("write local fixture");
    let local_process_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        process_machine_id: Some("worker-proc-1".into()),
        process_host_id: Some("worker-host-1".into()),
        caller_machine_id: Some("untrusted-caller-box".into()),
        caller_host_id: Some("untrusted-caller-host".into()),
        ..Default::default()
    };
    runtime
        .run_tool_with_context_and_role(
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "source_path": local_source.to_string_lossy(),
                "path": "reports/local-proc.txt",
            }),
            Role::Admin,
            ToolContext {
                session_context: local_process_session,
                cwd: Some(repo_root.to_string_lossy().to_string()),
                ..ToolContext::default()
            },
        )
        .expect("local process artifact put");

    // 2. Local session without process identity falls back to trusted runtime identity.
    let runtime_source = repo_root.join("local-runtime.txt");
    std::fs::write(&runtime_source, "from local runtime").expect("write runtime fixture");
    let local_runtime_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        process_machine_id: None,
        process_host_id: Some("worker-host-1".into()),
        caller_machine_id: Some("untrusted-caller-box".into()),
        ..Default::default()
    };
    runtime
        .run_tool_with_context_and_role(
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "source_path": runtime_source.to_string_lossy(),
                "path": "reports/local-runtime.txt",
            }),
            Role::Admin,
            ToolContext {
                session_context: local_runtime_session,
                cwd: Some(repo_root.to_string_lossy().to_string()),
                ..ToolContext::default()
            },
        )
        .expect("local runtime artifact put");

    // 3. SSH MCP session carries self-asserted caller labels and destination process id;
    // preloaded payload is accepted, but origin remains None.
    let ssh_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        transport: Some(McpTransport::SshMcp),
        process_machine_id: Some("destination-machine".into()),
        process_host_id: Some("destination-host".into()),
        caller_machine_id: Some("remote-spoke-box".into()),
        caller_host_id: Some("remote-spoke-host".into()),
        ..Default::default()
    };
    runtime
        .run_tool_with_context_and_role(
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "artifacts": [{
                    "path": "reports/ssh-remote.txt",
                    "media_type": "text/plain",
                    "content": "from ssh remote",
                }],
            }),
            Role::Admin,
            ToolContext {
                session_context: ssh_session,
                cwd: Some(repo_root.to_string_lossy().to_string()),
                ..ToolContext::default()
            },
        )
        .expect("ssh mcp artifact put");

    let manifest = runtime
        .get_task_artifact_manifest(&task.id)
        .expect("artifact manifest");

    let proc_art = manifest
        .iter()
        .find(|a| a.path == "reports/local-proc.txt")
        .expect("local proc artifact");
    assert_eq!(
        proc_art.origin,
        Some(orbit_types::task::ExecutionLocation {
            machine_id: "worker-proc-1".into(),
            host_id: Some("worker-host-1".into()),
        })
    );

    let runtime_art = manifest
        .iter()
        .find(|a| a.path == "reports/local-runtime.txt")
        .expect("local runtime artifact");
    assert_eq!(
        runtime_art.origin,
        Some(orbit_types::task::ExecutionLocation {
            machine_id: "local-runtime-box".into(),
            host_id: Some("worker-host-1".into()),
        })
    );

    let remote_art = manifest
        .iter()
        .find(|a| a.path == "reports/ssh-remote.txt")
        .expect("remote artifact");
    assert_eq!(remote_art.origin, None);
}
