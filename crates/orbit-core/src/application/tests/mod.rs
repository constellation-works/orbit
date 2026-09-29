mod audit_event;
mod distributed;
mod epic_retirement;
mod executor;
mod gc;
mod job_pipeline;
mod job_submission;
mod managed_asset_manifest;
mod managed_assets;
mod settlement;
mod skill;
mod workflow;
mod workspace_sync;

/// Re-execute one mutable fixture in a child with disposable user state.
///
/// Call this before constructing a runtime, with the calling function item's
/// type name. The parent verifies that libtest ran exactly the requested test;
/// the child keeps the test's existing behavioral assertions.
pub(crate) fn run_isolated_test(function_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_ISOLATED_CHILD";
    let test_name = function_name
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(function_name);
    if std::env::var(CHILD).ok().as_deref() == Some(test_name) {
        return false;
    }

    let home = tempfile::tempdir().expect("isolated test home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{test_name}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
        "child did not execute exactly one test ({test_name}): {stdout}\n{stderr}"
    );
    true
}
