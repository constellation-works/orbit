//! Deterministic local command execution [ORB-11294].

use std::path::PathBuf;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use orbit_engine::{DispatchError, ResolvedShellExecutor};

use orbit_engine::execute_deterministic_action;

/// Minimal host: a workspace root and the shipped `local-shell` executor.
struct ShellHost {
    root: PathBuf,
}

impl orbit_engine::test_support::NullRuntimeHost for ShellHost {
    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.root.to_string_lossy().into_owned())
    }

    fn resolve_local_shell_executor(
        &self,
        _executor: &str,
    ) -> Result<ResolvedShellExecutor, DispatchError> {
        Ok(ResolvedShellExecutor::default())
    }
}

fn run(host: &ShellHost, config: Value) -> Result<Value, OrbitError> {
    execute_deterministic_action(
        host,
        "local_shell",
        &config,
        &json!({}),
        false,
        &Default::default(),
        None,
    )
}

#[test]
fn cwd_outside_the_workspace_root_is_rejected() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::cwd_outside_the_workspace_root_is_rejected"
    )) {
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let host = ShellHost {
        root: dir.path().to_path_buf(),
    };

    let error =
        run(&host, json!({ "command": "/bin/echo", "cwd": ".." })).expect_err("escape is rejected");

    assert!(
        error.to_string().contains("outside the workspace root"),
        "{error}"
    );
}
