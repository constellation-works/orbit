//! Deterministic local command execution [ORB-11294].

use orbit_engine::test_support::impl_test_host;
use orbit_engine::test_support::local_shell;
use std::path::PathBuf;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use orbit_engine::{DispatchError, ResolvedShellExecutor};

/// Minimal host: a workspace root and the shipped `local-shell` executor.
struct ShellHost {
    root: PathBuf,
}

impl_test_host! {
    impl Dispatch for ShellHost {
        fn resolve_local_shell_executor(
            &self,
            _executor: &str,
        ) -> Result<ResolvedShellExecutor, DispatchError> {
            Ok(ResolvedShellExecutor::default())
        }
    }
}

impl_test_host! {
    impl HostEnvironment for ShellHost {
        fn run_settings(&self) -> orbit_engine::RunSettings {
            orbit_engine::RunSettings {
                repo_root: Some(self.root.to_string_lossy().into_owned()),
                ..orbit_engine::RunSettings::default()
            }
        }
    }
}

impl_test_host! {
    impl RuntimeHostProcessEnvironment for ShellHost {}
}

fn run(host: &ShellHost, config: Value) -> Result<Value, OrbitError> {
    local_shell(host, &config, &json!({}), None)
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
