//! Exercise desktop contracts through the registered production tool boundary.
use super::super::test_support::{run_tool_as_operator, test_runtime, unmanaged_tool_env_guard};
use crate::adapter::command::ToolEntryPoint;
use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
use serde_json::json;

#[test]
fn desktop_tools_require_explicit_destination_and_reject_undeclared_authority() {
    if !isolated("desktop_tools_require_explicit_destination_and_reject_undeclared_authority") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    assert!(
        runtime
            .run_tool("orbit.desktop.read", json!({"scope":"tasks"}))
            .is_err()
    );
    let result = runtime.run_tool("orbit.desktop.task.write", json!({"workspace":repo,"request_id":"spoof","operation":{"kind":"create","title":"No spoof","description":"","acceptance_criteria":["Proof"]},"actor":"human"}));
    assert!(result.is_err(), "actor cannot become a trusted grant");
}

#[test]
fn desktop_run_reads_preserve_operator_gate_at_mcp_entrypoint() {
    if !isolated("desktop_run_reads_preserve_operator_gate_at_mcp_entrypoint") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let denied = runtime.execute_tool_command_dispatch_with_session_context(
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"runs"}),
        None,
        None,
        ToolEntryPoint::Mcp,
        ToolSessionContext::default(),
    );
    assert!(
        matches!(denied, Err(OrbitError::CapabilityDenied(_))),
        "desktop is not a backdoor to operator run evidence: {denied:?}"
    );
    let read = run_tool_as_operator(
        &runtime,
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"runs"}),
    )
    .expect("operator observed read");
    assert_eq!(read["items"], json!([]));
    assert_eq!(read["total"], 0);
}

#[test]
fn desktop_create_and_comment_retry_through_tool_boundary_have_one_effect() {
    if !isolated("desktop_create_and_comment_retry_through_tool_boundary_have_one_effect") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let request = json!({"workspace":repo,"model":"codex","request_id":"create-1","operation":{"kind":"create","title":"Capture","description":"Daily work","acceptance_criteria":["Observed proof"],"priority":"medium"}});
    let created = runtime
        .run_tool("orbit.desktop.task.write", request.clone())
        .expect("create proposed task");
    let retried = runtime
        .run_tool("orbit.desktop.task.write", request.clone())
        .expect("reconcile create reply");
    assert_eq!(
        created["snapshot"]["task"]["id"],
        retried["snapshot"]["task"]["id"]
    );
    assert_eq!(created["snapshot"]["task"]["status"], "proposed");
    let mut changed = request;
    changed["operation"]["title"] = json!("Changed request");
    let refused = runtime
        .run_tool("orbit.desktop.task.write", changed)
        .expect("definite precommit refusal");
    assert_eq!(refused["mutation_applied"], false);
    assert!(refused["refusal"]["message"].is_string());
    let id = created["snapshot"]["task"]["id"].clone();
    let comment = json!({"workspace":repo,"model":"codex","request_id":"comment-1","operation":{"kind":"comment","id":id,"expected_revision":retried["snapshot"]["revision"],"comment":"One durable comment"}});
    let first = runtime
        .run_tool("orbit.desktop.task.write", comment.clone())
        .expect("comment");
    let second = runtime
        .run_tool("orbit.desktop.task.write", comment)
        .expect("same comment retry");
    assert_eq!(
        first["snapshot"]["comments_total"],
        second["snapshot"]["comments_total"]
    );
    assert_eq!(second["replayed"], true);
    let conflict = runtime.run_tool("orbit.desktop.task.write", json!({"workspace":repo,"request_id":"stale-1","operation":{"kind":"edit","id":id,"expected_revision":created["snapshot"]["revision"],"fields":{"title":"stale"}}})).expect("structured stale response");
    assert_eq!(conflict["conflict"]["code"], "revision_conflict");
    assert_eq!(conflict["snapshot"]["task"]["title"], "Capture");
    assert_eq!(
        conflict["snapshot"]["revision"],
        second["snapshot"]["revision"]
    );
}

fn isolated(name: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_DESKTOP_TOOL_CHILD";
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap();
    let exact = format!("{module}::{name}");
    if std::env::var_os(MARKER).is_some_and(|value| value == exact.as_str()) {
        return true;
    }
    let home = tempfile::tempdir().expect("fixture home");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
        .env_remove("ORBIT_WORKER_CONTEXT_REQUIRED")
        .env(MARKER, &exact)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .expect("isolated fixture child");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"));
    false
}
