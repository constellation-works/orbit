//! Deterministic local command execution [ORB-11294].
//!
//! These drive the real `orbit-exec` spawn/supervision path — nothing here is
//! faked below the action itself — so exit status, capture, timeout, and child
//! process-group cleanup are asserted against actual subprocesses.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::activity_job::{DispatchError, ResolvedShellExecutor};
use crate::context::RuntimeHost;
use crate::executor::automation::StateExecutionContext;

use super::local_shell;

/// Minimal host: a workspace root, a credential-free baseline environment, and
/// one registered shell executor. Sandbox resolution keeps the trait default
/// (`None`), which is the shipped `local-shell` posture.
struct ShellHost {
    root: PathBuf,
    executor: ResolvedShellExecutor,
    baseline_env: Vec<(String, String)>,
}

impl ShellHost {
    fn new(root: &std::path::Path) -> Self {
        Self {
            root: root.to_path_buf(),
            executor: ResolvedShellExecutor::default(),
            baseline_env: vec![("ORBIT_BASELINE".to_string(), "from-policy".to_string())],
        }
    }

    fn with_executor(mut self, executor: ResolvedShellExecutor) -> Self {
        self.executor = executor;
        self
    }
}

impl RuntimeHost for ShellHost {
    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.root.to_string_lossy().into_owned())
    }

    // A host bound to a registered workspace. A CLI agent launched from it
    // would receive both of these; a shell step must not.
    fn orbit_registry_root(&self) -> Option<String> {
        Some("/authoritative/registry".to_string())
    }

    fn orbit_workspace_selector(&self) -> Option<String> {
        Some("ws_owner".to_string())
    }

    fn agent_subprocess_environment(&self, _required_env_vars: &[&str]) -> Vec<(String, String)> {
        self.baseline_env.clone()
    }

    fn resolve_local_shell_executor(
        &self,
        _executor: &str,
    ) -> Result<ResolvedShellExecutor, DispatchError> {
        Ok(self.executor.clone())
    }
}

fn workspace() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn run(host: &ShellHost, config: Value) -> Result<Value, OrbitError> {
    local_shell(host, &config, &json!({}), None)
}

#[test]
fn argv_execution_captures_stdout_and_exit_status() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let output = run(
        &host,
        json!({ "command": "/bin/echo", "args": ["hello", "orbit"] }),
    )
    .expect("step succeeds");

    assert_eq!(output["success"], json!(true));
    assert_eq!(output["exit_code"], json!(0));
    assert_eq!(output["stdout"], json!("hello orbit\n"));
    assert_eq!(output["timed_out"], json!(false));
    assert_eq!(output["sandbox"], json!("none"));
    assert_eq!(output["argv"], json!(["/bin/echo", "hello", "orbit"]));
    assert_eq!(
        output["cwd"],
        json!(
            dir.path()
                .canonicalize()
                .expect("canonicalize")
                .display()
                .to_string()
        )
    );
}

#[test]
fn arguments_reach_the_child_verbatim_without_shell_expansion() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    // A shell would word-split and glob these. Direct `execve` must not.
    let output = run(
        &host,
        json!({ "command": "/bin/echo", "args": ["a b", "*", "$HOME"] }),
    )
    .expect("step succeeds");

    assert_eq!(output["stdout"], json!("a b * $HOME\n"));
}

#[test]
fn nonzero_exit_fails_the_step_and_names_the_status() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let error = run(
        &host,
        json!({ "shell": "/bin/sh", "script": "echo to-stderr >&2; exit 7" }),
    )
    .expect_err("nonzero exit fails");

    let message = error.to_string();
    assert!(message.contains("exited with status 7"), "{message}");
    assert!(message.contains("to-stderr"), "{message}");
}

#[test]
fn allow_nonzero_exit_returns_the_failure_as_structured_output() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let output = run(
        &host,
        json!({
            "shell": "/bin/sh",
            "script": "echo oops >&2; exit 3",
            "allow_nonzero_exit": true,
        }),
    )
    .expect("step returns output");

    assert_eq!(output["success"], json!(false));
    assert_eq!(output["exit_code"], json!(3));
    assert_eq!(output["stderr"], json!("oops\n"));
}

#[test]
fn missing_executable_fails_with_the_program_name() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let error = run(
        &host,
        json!({ "command": "/nonexistent/orbit-local-shell-probe" }),
    )
    .expect_err("missing executable fails");

    assert!(
        error
            .to_string()
            .contains("/nonexistent/orbit-local-shell-probe"),
        "{error}"
    );
}

#[test]
fn timeout_terminates_the_child_and_reports_the_deadline() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let started = Instant::now();
    let output = run(
        &host,
        json!({
            "shell": "/bin/sh",
            "script": "sleep 30",
            "timeout_ms": 300,
            "allow_nonzero_exit": true,
        }),
    )
    .expect("step returns output");

    assert_eq!(output["timed_out"], json!(true));
    assert_eq!(output["success"], json!(false));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "timeout must terminate the child rather than wait it out"
    );
}

/// The supervisor kills the child's whole process group, so a background
/// grandchild that outlives the shell does not survive the step.
#[cfg(unix)]
#[test]
fn timeout_cleans_up_orphaned_grandchildren() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());
    let pid_file = dir.path().join("grandchild.pid");

    let output = run(
        &host,
        json!({
            "shell": "/bin/sh",
            "script": format!("sleep 30 & echo $! > {}; wait", pid_file.display()),
            "timeout_ms": 500,
            "allow_nonzero_exit": true,
        }),
    )
    .expect("step returns output");
    assert_eq!(output["timed_out"], json!(true));

    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("grandchild pid recorded")
        .trim()
        .parse()
        .expect("pid parses");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        // SAFETY: signal 0 performs an existence/permission check only.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("grandchild {pid} survived the step");
}

#[test]
fn step_input_cannot_supply_a_program_or_arguments() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    // Everything a templated `with:` block could carry is present in input and
    // must be ignored: argv comes from config alone.
    let error = local_shell(
        &host,
        &json!({}),
        &json!({ "command": "/bin/echo", "args": ["pwned"], "shell": "/bin/sh", "script": "id" }),
        None,
    )
    .expect_err("input cannot name a program");

    assert!(
        error.to_string().contains("local_shell needs a program"),
        "{error}"
    );
}

#[test]
fn cwd_outside_the_workspace_root_is_rejected() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let error =
        run(&host, json!({ "command": "/bin/echo", "cwd": ".." })).expect_err("escape is rejected");

    assert!(
        error.to_string().contains("outside the workspace root"),
        "{error}"
    );
}

#[test]
fn cwd_resolves_relative_to_the_workspace_root() {
    let dir = workspace();
    std::fs::create_dir(dir.path().join("nested")).expect("create nested");
    let host = ShellHost::new(dir.path());

    let output =
        run(&host, json!({ "command": "/bin/pwd", "cwd": "nested" })).expect("step succeeds");

    let expected = dir
        .path()
        .join("nested")
        .canonicalize()
        .expect("canonicalize");
    assert_eq!(
        output["stdout"].as_str().expect("stdout").trim(),
        expected.display().to_string()
    );
}

#[test]
fn environment_is_the_policy_baseline_plus_explicit_entries() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());
    // SAFETY: single-threaded assertion that ambient variables do not leak.
    unsafe { std::env::set_var("ORBIT_AMBIENT_PROBE", "leaked") };

    let output = run(
        &host,
        json!({
            "shell": "/bin/sh",
            "script": "echo \"$ORBIT_BASELINE|$ORBIT_EXPLICIT|$ORBIT_AMBIENT_PROBE\"",
            "env": { "ORBIT_EXPLICIT": "from-config" },
        }),
    )
    .expect("step succeeds");

    assert_eq!(output["stdout"], json!("from-policy|from-config|\n"));
}

/// A shell step is not an agent: it gets no Orbit registry or workspace
/// identity, so a nested `orbit` call from inside it cannot inherit the run's
/// authority the way a dispatched CLI agent deliberately does.
#[test]
fn a_shell_step_receives_no_agent_identity_variables() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());
    assert!(host.orbit_registry_root().is_some());
    assert!(host.orbit_workspace_selector().is_some());

    let output = run(
        &host,
        json!({
            "shell": "/bin/sh",
            "script": "echo \"[$ORBIT_REGISTRY_ROOT][$ORBIT_WORKSPACE]\"",
        }),
    )
    .expect("step succeeds");

    assert_eq!(output["stdout"], json!("[][]\n"));
}

#[test]
fn config_env_overrides_a_baseline_entry_of_the_same_name() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let output = run(
        &host,
        json!({
            "shell": "/bin/sh",
            "script": "echo \"$ORBIT_BASELINE\"",
            "env": { "ORBIT_BASELINE": "overridden" },
        }),
    )
    .expect("step succeeds");

    assert_eq!(output["stdout"], json!("overridden\n"));
}

/// A definition written before the rename carries `command` / `args` / `env`,
/// and those keep working as the step's defaults.
#[test]
fn legacy_executor_definition_supplies_defaults() {
    let dir = workspace();
    let host = ShellHost::new(dir.path()).with_executor(ResolvedShellExecutor {
        command: Some("/bin/echo".to_string()),
        args: vec!["prefixed".to_string()],
        env: BTreeMap::from([("ORBIT_FROM_DEF".to_string(), "yes".to_string())]),
        timeout_seconds: Some(30),
    });

    let output = run(&host, json!({})).expect("step succeeds");
    assert_eq!(output["stdout"], json!("prefixed\n"));
    assert_eq!(output["timeout_ms"], json!(30_000));

    let output =
        run(&host, json!({ "command": "/bin/echo", "args": ["own"] })).expect("step succeeds");
    assert_eq!(output["argv"], json!(["/bin/echo", "prefixed", "own"]));
}

#[test]
fn declaring_both_argv_and_shell_execution_is_rejected() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let error = run(
        &host,
        json!({ "command": "/bin/echo", "shell": "/bin/sh", "script": "id" }),
    )
    .expect_err("ambiguous declaration is rejected");
    assert!(error.to_string().contains("not both"), "{error}");
}

#[test]
fn shell_execution_rejects_positional_args() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let error = run(
        &host,
        json!({ "shell": "/bin/sh", "script": "id", "args": ["extra"] }),
    )
    .expect_err("positional args are rejected");
    assert!(error.to_string().contains("rebind `$0`"), "{error}");
}

#[test]
fn a_timeout_beyond_the_ceiling_is_rejected() {
    let dir = workspace();
    let host = ShellHost::new(dir.path());

    let error = run(
        &host,
        json!({ "command": "/bin/echo", "timeout_ms": 3_600_001u64 }),
    )
    .expect_err("timeout ceiling is enforced");
    assert!(
        error.to_string().contains("timeout must be between"),
        "{error}"
    );
}

#[test]
fn the_run_worktree_wins_over_the_repository_root() {
    let dir = workspace();
    let worktree = dir.path().join("worktree");
    std::fs::create_dir(&worktree).expect("create worktree");
    let host = ShellHost::new(dir.path());

    let output = local_shell(
        &host,
        &json!({ "command": "/bin/pwd" }),
        &json!({ "workspace_path": worktree.display().to_string() }),
        Some(&StateExecutionContext::default()),
    )
    .expect("step succeeds");

    assert_eq!(
        output["stdout"].as_str().expect("stdout").trim(),
        worktree
            .canonicalize()
            .expect("canonicalize")
            .display()
            .to_string()
    );
}
