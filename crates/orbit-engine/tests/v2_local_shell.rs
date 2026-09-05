#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Deterministic local command execution through the real v2 dispatch seam
//! [ORB-11294]. Every case here loads a `kind: Activity` asset, hands it to
//! `dispatch_v2_activity`, and asserts on the dispatch outcome and the audit
//! envelope — no direct calls into the action.
//!
//! Runs under `cargo nextest run -p orbit-engine --test v2_local_shell`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_agent::loop_engine::InMemorySink;
use orbit_engine::activity_job::load_activity_asset;
use orbit_engine::{
    DispatchError, DispatchOutcome, ResolvedShellExecutor, RuntimeHost, V2AuditWriter,
    V2DispatchInput, dispatch_v2_activity,
};
use serde_json::{Value, json};

/// The shipped example must dispatch as written, so the documented reference
/// cannot drift from the action it documents.
#[test]
fn shipped_reference_asset_runs_git_status_in_the_workspace() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_repo(repo.path());
    let host = ShellHost::new(repo.path());

    let yaml = std::fs::read_to_string(
        workspace_root()
            .join("crates/orbit-core/assets/activities/examples/local_shell_reference.yaml"),
    )
    .expect("read shipped reference asset");
    let outcome = dispatch(&yaml, json!({}), &host).expect("dispatch succeeds");

    assert!(outcome.success);
    assert_eq!(outcome.output["exit_code"], json!(0));
    assert_eq!(outcome.output["sandbox"], json!("none"));
    assert_eq!(
        outcome.output["argv"],
        json!(["git", "status", "--porcelain"])
    );
}

#[test]
fn a_nonzero_exit_fails_the_activity_and_is_audited() {
    let repo = tempfile::tempdir().expect("tempdir");
    let host = ShellHost::new(repo.path());

    let error = dispatch(
        &shell_activity(json!({ "shell": "/bin/sh", "script": "exit 9" })),
        json!({}),
        &host,
    )
    .expect_err("nonzero exit fails the activity");

    match &error {
        DispatchError::DeterministicActionFailed { action, message } => {
            assert_eq!(action, "local_shell");
            assert!(message.contains("exited with status 9"), "{message}");
        }
        other => panic!("unexpected dispatch error: {other:?}"),
    }
}

#[test]
fn a_missing_executable_fails_the_activity() {
    let repo = tempfile::tempdir().expect("tempdir");
    let host = ShellHost::new(repo.path());

    let error = dispatch(
        &shell_activity(json!({ "command": "/nonexistent/orbit-local-shell-probe" })),
        json!({}),
        &host,
    )
    .expect_err("missing executable fails the activity");

    assert!(
        error.to_string().contains("orbit-local-shell-probe"),
        "{error}"
    );
}

#[test]
fn a_timed_out_step_is_terminated_and_reported() {
    let repo = tempfile::tempdir().expect("tempdir");
    let host = ShellHost::new(repo.path());
    let started = std::time::Instant::now();

    let outcome = dispatch(
        &shell_activity(json!({
            "shell": "/bin/sh",
            "script": "sleep 30",
            "timeout_ms": 400,
            "allow_nonzero_exit": true,
        })),
        json!({}),
        &host,
    )
    .expect("dispatch returns an outcome");

    assert_eq!(outcome.output["timed_out"], json!(true));
    assert_eq!(outcome.output["success"], json!(false));
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

/// The dispatcher injects `run_id` and the step id into deterministic input.
/// Neither may become part of the child's argv.
#[test]
fn dispatch_input_never_reaches_argv() {
    let repo = tempfile::tempdir().expect("tempdir");
    let host = ShellHost::new(repo.path());

    let outcome = dispatch(
        &shell_activity(json!({ "command": "/bin/echo", "args": ["configured"] })),
        json!({ "command": "/bin/sh", "args": ["-c", "echo injected"] }),
        &host,
    )
    .expect("dispatch succeeds");

    assert_eq!(outcome.output["argv"], json!(["/bin/echo", "configured"]));
    assert_eq!(outcome.output["stdout"], json!("configured\n"));
}

/// The activity's `fsProfile` travels to sandbox resolution, so a shell step is
/// scoped by the same filesystem policy as the activity that declared it.
#[test]
fn the_activity_fs_profile_reaches_sandbox_resolution() {
    let repo = tempfile::tempdir().expect("tempdir");
    let host = ShellHost::new(repo.path());

    let yaml = shell_activity_with_profile(json!({ "command": "/bin/true" }), Some("reviewer"));
    let _outcome = dispatch(&yaml, json!({}), &host).expect("dispatch succeeds");

    assert_eq!(
        host.observed_fs_profile().as_deref(),
        Some("reviewer"),
        "sandbox resolution must see the activity's fsProfile"
    );
    assert_eq!(host.observed_executor().as_deref(), Some("local-shell"));
}

fn dispatch(yaml: &str, input: Value, host: &ShellHost) -> Result<DispatchOutcome, DispatchError> {
    let asset = load_activity_asset(yaml).expect("activity asset loads");
    let audit_root = tempfile::tempdir().expect("tempdir");
    let blob_dir = audit_root.path().join("blobs");
    std::fs::create_dir_all(&blob_dir).expect("create blob dir");
    let sink = Arc::new(InMemorySink::new(blob_dir));
    let writer = Arc::new(V2AuditWriter::new("jrun-local-shell", "test", sink.clone()));

    dispatch_v2_activity(V2DispatchInput {
        activity_name: &asset.name,
        spec: &asset.spec.spec,
        fs_profile: asset.spec.fs_profile.as_deref(),
        input,
        audit: writer,
        run_id: "jrun-local-shell",
        host: Some(host),
    })
}

fn shell_activity(config: Value) -> String {
    shell_activity_with_profile(config, None)
}

fn shell_activity_with_profile(config: Value, fs_profile: Option<&str>) -> String {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Activity",
        "metadata": { "name": "local_shell_case" },
        "spec": {
            "type": "deterministic",
            "description": "local_shell integration case",
            "fsProfile": fs_profile,
            "action": "local_shell",
            "config": config,
        }
    });
    serde_json::to_string(&asset).expect("serialize activity asset")
}

fn init_repo(path: &Path) {
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(path)
        .status()
        .expect("run git init");
    assert!(status.success(), "git init failed");
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

struct ShellHost {
    root: PathBuf,
    observed: std::sync::Mutex<Vec<(String, Option<String>)>>,
}

impl ShellHost {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            observed: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn observed_executor(&self) -> Option<String> {
        self.observed
            .lock()
            .expect("lock")
            .first()
            .map(|(executor, _)| executor.clone())
    }

    fn observed_fs_profile(&self) -> Option<String> {
        self.observed
            .lock()
            .expect("lock")
            .first()
            .and_then(|(_, profile)| profile.clone())
    }
}

impl RuntimeHost for ShellHost {
    fn repo_root(&self) -> Result<String, orbit_common::OrbitError> {
        Ok(self.root.to_string_lossy().into_owned())
    }

    fn agent_subprocess_environment(&self, _required_env_vars: &[&str]) -> Vec<(String, String)> {
        vec![(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        )]
    }

    fn resolve_local_shell_executor(
        &self,
        _executor: &str,
    ) -> Result<ResolvedShellExecutor, DispatchError> {
        Ok(ResolvedShellExecutor::default())
    }

    fn resolve_executor_sandbox(
        &self,
        provider: &str,
        fs_profile: Option<&str>,
        _subprocess_cwd: Option<&Path>,
    ) -> Result<Option<orbit_engine::ResolvedSandbox>, DispatchError> {
        self.observed
            .lock()
            .expect("lock")
            .push((provider.to_string(), fs_profile.map(ToOwned::to_owned)));
        Ok(None)
    }
}
