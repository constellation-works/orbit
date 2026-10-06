//! An implementation activity cannot set task status `blocked` [ORB-14269].
//!
//! The refusal is on the tool, where the activity deny policy is visible.
//! The host update path does not receive that policy.

#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

use orbit_common::security::child_env::{
    ACTIVITY_NAME_ENV, ACTIVITY_TOOL_POLICY_ENV, ACTIVITY_TOOLS_DENY_ENV,
};
use orbit_common::{OrbitError, test_env};
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use tempfile::TempDir;

fn run_isolated_test(test_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_IMPLEMENT_BLOCKER_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }

    let home = TempDir::new().expect("isolated test home");
    let mut command = std::process::Command::new(
        std::env::current_exe().expect("locate integration test binary"),
    );
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .expect("isolated test child");
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    true
}

fn test_runtime() -> (TempDir, OrbitRuntime, std::path::PathBuf) {
    let root = TempDir::new().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root)
}

fn in_progress_task(runtime: &OrbitRuntime, repo_root: &std::path::Path) -> String {
    let workspace = repo_root.to_string_lossy().to_string();
    let task = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Implementation blocker refusal",
                "description": "Fixture for the implementation status gate.",
                "acceptance_criteria": ["The gate refuses a direct block."],
                "complexity": "low",
                "workspace": workspace,
                "type": "bug",
                "model": "grok",
            }),
            None,
            None,
        )
        .expect("add task");
    let task_id = task["id"].as_str().expect("task id").to_string();
    runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": task_id,
                "plan": "1. Prove an implementer cannot set blocked.",
                "model": "grok",
            }),
            None,
            None,
        )
        .expect("set plan");
    runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({ "id": task_id, "status": "in-progress", "model": "grok" }),
            None,
            None,
        )
        .expect("start task");
    task_id
}

fn show(runtime: &OrbitRuntime, task_id: &str) -> Value {
    runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": task_id, "model": "grok" }),
            None,
            None,
        )
        .expect("show task")
}

fn quiet_env() -> test_env::ScopedEnv {
    test_env::unset(test_env::INHERITED_AUTHORITY_ENV.iter().copied())
}

fn implementer_env(activity: &str) -> test_env::ScopedEnv {
    test_env::scoped([
        ("ORBIT_AGENT_NAME", Some("grok")),
        ("ORBIT_TASK_ACTOR_KIND", Some("agent")),
        (ACTIVITY_NAME_ENV, Some(activity)),
        (ACTIVITY_TOOL_POLICY_ENV, Some("deny")),
        (ACTIVITY_TOOLS_DENY_ENV, Some("orbit.agent.invoke")),
    ])
}

fn assert_blocked_refused(error: OrbitError) {
    match error {
        OrbitError::InvalidInput(_) => {}
        other => panic!("expected invalid input, got {other}"),
    }
}

#[test]
fn an_implementation_activity_cannot_set_status_blocked() {
    let test_name = "implement_blocker::an_implementation_activity_cannot_set_status_blocked";
    if run_isolated_test(test_name) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let workspace = repo_root.to_string_lossy().to_string();
    let task_id = {
        let _env = quiet_env();
        in_progress_task(&runtime, &repo_root)
    };

    {
        let _env = implementer_env("implement_one");
        let refused = runtime
            .execute_tool_command(
                "orbit.task.update",
                json!({
                    "id": task_id,
                    "status": "blocked",
                    "model": "grok",
                }),
                None,
                None,
            )
            .expect_err("implement_one cannot set blocked");
        assert_blocked_refused(refused);

        let guarded = runtime
            .execute_tool_command(
                "orbit.task.update",
                json!({
                    "id": task_id,
                    "workspace": workspace,
                    "request_id": "impl-block",
                    "expected_revision": "not-a-real-revision",
                    "status": "blocked",
                    "model": "grok",
                }),
                None,
                None,
            )
            .expect_err("a guarded edit is not a bypass");
        assert_blocked_refused(guarded);

        let updated = runtime
            .execute_tool_command(
                "orbit.task.update",
                json!({
                    "id": task_id,
                    "execution_summary": "Outcome: blocked on the environment.",
                    "model": "grok",
                }),
                None,
                None,
            )
            .expect("the same envelope can still record a summary");
        assert_eq!(
            updated["execution_summary"],
            "Outcome: blocked on the environment."
        );
        assert_eq!(updated["status"], "in-progress");
    }

    {
        let _env = implementer_env("agent_implement");
        let refused = runtime
            .execute_tool_command(
                "orbit.task.update",
                json!({
                    "id": task_id,
                    "status": "Blocked",
                    "model": "grok",
                }),
                None,
                None,
            )
            .expect_err("agent_implement cannot set blocked");
        assert_blocked_refused(refused);
    }
    {
        let _env = quiet_env();
        assert_eq!(show(&runtime, &task_id)["status"], "in-progress");
    }

    {
        let _env = quiet_env();
        let blocked = runtime
            .execute_tool_command(
                "orbit.task.update",
                json!({
                    "id": task_id,
                    "status": "blocked",
                    "note": "operator block",
                    "model": "grok",
                }),
                None,
                None,
            )
            .expect("a caller outside an implementation activity can still block");
        assert_eq!(blocked["status"], "blocked");
    }
}
