//! [ORB-10711, ADR-0351] Claim-gated remote command execution.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::test_support::{
    invalid_input_message, managed_tool_env_guard, run_tool_as_operator, test_runtime,
    unmanaged_tool_env_guard,
};
use crate::OrbitRuntime;

/// Acquire the workspace claim as `actor` and return its token.
fn acquire_claim(runtime: &OrbitRuntime, actor: &str) -> String {
    let result = run_tool_as_operator(
        runtime,
        "orbit.workspace.claim.acquire",
        json!({ "model": actor }),
    )
    .expect("acquire workspace claim");
    assert_eq!(result["acquired"], json!(true));
    result["claim_token"]
        .as_str()
        .expect("claim grant carries a token")
        .to_string()
}

#[test]
fn claim_holder_executes_and_receives_stdout_stderr_and_exit_status() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    let token = acquire_claim(&runtime, "claude");

    let result = run_tool_as_operator(
        &runtime,
        "orbit.command.exec",
        json!({
            "argv": ["echo", "hello-from-command-exec"],
            "working_directory": repo_root.display().to_string(),
            "claim_token": token,
            "model": "claude",
        }),
    )
    .expect("the claim holder must be able to execute a command");

    assert_eq!(result["success"], json!(true));
    assert_eq!(result["exit_code"], json!(0));
    assert!(
        result["stdout"]
            .as_str()
            .expect("stdout is a string")
            .contains("hello-from-command-exec"),
        "unexpected stdout: {result}"
    );
}

#[test]
fn operator_without_the_claim_is_refused() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    acquire_claim(&runtime, "claude");

    let error = run_tool_as_operator(
        &runtime,
        "orbit.command.exec",
        json!({
            "argv": ["echo", "hello"],
            "working_directory": repo_root.display().to_string(),
            "model": "codex",
        }),
    )
    .expect_err("a caller without the holder's token must be refused");

    let OrbitError::WorkspaceClaimHeld(claim) = &error else {
        panic!("expected WorkspaceClaimHeld, got {error:?}");
    };
    assert_eq!(claim.operation, "orbit.command.exec");
    assert_eq!(claim.holder, "claude");
}

#[test]
fn shell_string_argv_is_rejected() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();

    let error = run_tool_as_operator(
        &runtime,
        "orbit.command.exec",
        json!({
            "argv": "echo hello",
            "working_directory": repo_root.display().to_string(),
            "model": "codex",
        }),
    )
    .expect_err("a shell string must be rejected, not spawned or interpreted");

    let OrbitError::InvalidInput(message) = &error else {
        panic!("expected InvalidInput, got {error:?}");
    };
    assert!(
        message.contains("shell string"),
        "unexpected message: {message}"
    );
}

#[test]
fn empty_argv_is_rejected() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();

    let error = run_tool_as_operator(
        &runtime,
        "orbit.command.exec",
        json!({
            "argv": [],
            "working_directory": repo_root.display().to_string(),
            "model": "codex",
        }),
    )
    .expect_err("an empty argv names no program to run");

    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error:?}");
}

#[test]
fn managed_run_environment_denies_command_exec() {
    let _env = managed_tool_env_guard("jrun-test-managed-command-exec");
    let (_root, runtime, repo_root) = test_runtime();

    let error = run_tool_as_operator(
        &runtime,
        "orbit.command.exec",
        json!({
            "argv": ["echo", "hello"],
            "working_directory": repo_root.display().to_string(),
            "model": "codex",
        }),
    )
    .expect_err("a managed run must not execute remote commands");

    assert!(
        matches!(error, OrbitError::CapabilityDenied(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("managed runs cannot execute"));
}

#[test]
fn audit_record_carries_argv_working_directory_caller_and_workspace() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    let token = acquire_claim(&runtime, "claude");

    run_tool_as_operator(
        &runtime,
        "orbit.command.exec",
        json!({
            "argv": ["echo", "audited-command"],
            "working_directory": repo_root.display().to_string(),
            "claim_token": token,
            "model": "claude",
        }),
    )
    .expect("claim holder executes");

    let events = runtime
        .list_audit_events(None, None, None, None, 200)
        .expect("read audit events");
    let event = events
        .iter()
        .find(|event| event.command == "command.exec")
        .expect("command execution is audited");
    let payload: Value = event
        .arguments_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .expect("audit payload is recorded JSON");

    assert_eq!(payload["argv"], json!(["echo", "audited-command"]));
    let canonical = std::fs::canonicalize(&repo_root).expect("canonicalize repo");
    assert_eq!(
        payload["working_directory"],
        json!(canonical.display().to_string())
    );
    assert_eq!(payload["caller"], json!("claude"));
    assert_eq!(payload["workspace"], json!(repo_root.display().to_string()));
}

#[test]
fn secret_argv_reaches_child_but_is_redacted_in_the_audit_record() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    let token = acquire_claim(&runtime, "claude");
    let secret = "sk-abcdefghijklmnopqrstuvwxyz012345";

    let result = run_tool_as_operator(
        &runtime,
        "orbit.command.exec",
        json!({
            "argv": ["echo", secret],
            "working_directory": repo_root.display().to_string(),
            "claim_token": token,
            "model": "claude",
        }),
    )
    .expect("claim holder executes");

    assert!(
        result["stdout"]
            .as_str()
            .expect("stdout is a string")
            .contains(secret),
        "the raw secret must reach the controlled test child unchanged: {result}"
    );

    let events = runtime
        .list_audit_events(None, None, None, None, 200)
        .expect("read audit events");
    let event = events
        .iter()
        .find(|event| event.command == "command.exec")
        .expect("command execution is audited");
    let payload: Value = event
        .arguments_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .expect("audit payload is recorded JSON");

    let argv = payload["argv"]
        .as_array()
        .expect("argv is recorded as an array");
    assert_eq!(argv[0], json!("echo"));
    assert_ne!(
        argv[1],
        json!(secret),
        "the raw secret must not be persisted in the durable audit record: {payload}"
    );
    assert!(
        argv[1]
            .as_str()
            .expect("redacted arg is a string")
            .contains("REDACTED"),
        "unexpected redacted argv value: {payload}"
    );

    let raw_record = event
        .arguments_json
        .as_deref()
        .expect("audit payload is recorded as raw JSON text");
    assert!(
        !raw_record.contains(secret),
        "the raw secret must not appear anywhere in the durable audit record: {raw_record}"
    );
}

fn exec_pwd(runtime: &OrbitRuntime, cwd: &Path, token: &str) -> Result<Value, OrbitError> {
    run_tool_as_operator(
        runtime,
        "orbit.command.exec",
        json!({
            "argv": ["pwd"],
            "working_directory": cwd.display().to_string(),
            "claim_token": token,
            "model": "claude",
        }),
    )
}

fn exec_touch(runtime: &OrbitRuntime, cwd: &Path, marker: &Path) -> Result<Value, OrbitError> {
    run_tool_as_operator(
        runtime,
        "orbit.command.exec",
        json!({
            "argv": ["touch", marker.display().to_string()],
            "working_directory": cwd.display().to_string(),
            "model": "claude",
        }),
    )
}

fn assert_outside_workspace(error: OrbitError, runtime: &OrbitRuntime) -> String {
    let message = invalid_input_message::<Value>(Err(error));
    let checkout = runtime
        .paths()
        .repo_root
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().repo_root.clone());
    assert!(
        message.contains("outside workspace"),
        "refusal must name the confinement: {message}"
    );
    assert!(
        message.contains("checkout"),
        "refusal must name the checkout: {message}"
    );
    assert!(
        message.contains(&checkout.display().to_string()),
        "refusal must name the root: {message}"
    );
    assert!(
        message.contains("working_directory"),
        "refusal must name the field: {message}"
    );
    message
}

#[test]
fn working_directory_inside_the_checkout_is_ok() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    let token = acquire_claim(&runtime, "claude");
    let nested = repo_root.join("src");
    std::fs::create_dir_all(&nested).expect("nested dir");

    let result = exec_pwd(&runtime, &nested, &token).expect("inside the checkout");
    assert_eq!(result["success"], json!(true));
    let stdout = result["stdout"].as_str().expect("stdout");
    let canonical = nested.canonicalize().expect("canonicalize nested");
    assert!(
        stdout.contains(&canonical.display().to_string()),
        "command must run in the nested checkout path: {result}"
    );
}

#[test]
fn working_directory_inside_a_linked_worktree_is_ok() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    let token = acquire_claim(&runtime, "claude");
    let worktree = repo_root.join(".orbit/state/worktrees/jrun-fixture");
    std::fs::create_dir_all(&worktree).expect("linked worktree");

    let result = exec_pwd(&runtime, &worktree, &token).expect("inside a linked worktree");
    assert_eq!(result["success"], json!(true));
    let stdout = result["stdout"].as_str().expect("stdout");
    let canonical = worktree.canonicalize().expect("canonicalize worktree");
    assert!(
        stdout.contains(&canonical.display().to_string()),
        "command must run in the linked worktree: {result}"
    );
}

#[test]
fn working_directory_in_a_sibling_checkout_is_refused() {
    let _env = unmanaged_tool_env_guard();
    let (root, runtime, _repo_root) = test_runtime();
    let sibling = root.path().join("sibling");
    std::fs::create_dir_all(&sibling).expect("sibling checkout");
    let marker = sibling.join("must-not-run");

    let error = exec_touch(&runtime, &sibling, &marker)
        .expect_err("a sibling workspace checkout is outside the selected checkout");
    assert_outside_workspace(error, &runtime);
    assert!(
        !marker.exists(),
        "a refused working_directory must not spawn the command"
    );
}

#[test]
fn working_directory_in_home_is_refused() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    let home = orbit_common::fs::path::home_dir().expect("home directory");
    let home = if home.exists() {
        home.canonicalize().expect("canonicalize home")
    } else {
        home
    };
    assert!(
        !home.starts_with(repo_root.canonicalize().expect("canonicalize repo")),
        "HOME must lie outside the fixture checkout"
    );

    let error = run_tool_as_operator(
        &runtime,
        "orbit.command.exec",
        json!({
            "argv": ["true"],
            "working_directory": home.display().to_string(),
            "model": "claude",
        }),
    )
    .expect_err("$HOME is outside the checkout");
    assert_outside_workspace(error, &runtime);
}

#[cfg(unix)]
#[test]
fn working_directory_symlink_that_escapes_is_refused() {
    let _env = unmanaged_tool_env_guard();
    let (root, runtime, repo_root) = test_runtime();
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    let link = repo_root.join("escape");
    std::os::unix::fs::symlink(&outside, &link).expect("escaping symlink");
    let marker = outside.join("must-not-run");

    let error = exec_touch(&runtime, &link, &marker)
        .expect_err("a symlink resolving outside the checkout is refused");
    assert_outside_workspace(error, &runtime);
    assert!(
        !marker.exists(),
        "a refused working_directory must not spawn the command"
    );
}

#[test]
fn relative_working_directory_is_refused() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("src dir");

    let error = run_tool_as_operator(
        &runtime,
        "orbit.command.exec",
        json!({
            "argv": ["pwd"],
            "working_directory": "src",
            "model": "claude",
        }),
    )
    .expect_err("relative working_directory must be refused");

    let message = invalid_input_message::<Value>(Err(error));
    assert!(message.contains("must be an absolute path"), "{message}");
    assert!(message.contains("'src'"), "{message}");
}
