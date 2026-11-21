mod executor;
mod job_pipeline;
mod managed_assets;
mod vision_probe;

/// Re-execute one mutable fixture in a child with disposable user state.
///
/// Call this before constructing a runtime, with the calling function item's
/// type name. The parent verifies that libtest ran exactly the requested test;
/// the child keeps the test's existing behavioral assertions.
pub(crate) fn run_isolated_test(function_name: &str) -> bool {
    run_isolated_test_with_env(function_name, &[])
}

/// Re-execute one mutable fixture in a child with additional fixture-only env.
pub(crate) fn run_isolated_test_with_env(function_name: &str, env: &[(&str, &str)]) -> bool {
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
    for (name, value) in env {
        command.env(*name, *value);
    }
    command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output = orbit_common::test_env::run_child_test(&mut command, test_name, home.path());
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        output.stdout,
        output.stderr,
    );
    true
}
