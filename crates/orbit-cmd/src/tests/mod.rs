//! Sibling unit tests mirroring source filenames (see
//! `docs/design-patterns/test_layout.md`); moved with their modules from
//! orbit-core in [ORB-10016].

mod agent_rules;
mod diagnostics;
mod migrate;

/// Run mutable Orbit fixtures with disposable user state and no inherited
/// routing authority, preserving the requested test's behavioral assertions.
pub(crate) fn run_isolated_test(function_name: &str) -> bool {
    const CHILD: &str = "ORBIT_CMD_TEST_ISOLATED_CHILD";
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
    command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output = orbit_common::process::run_bounded_capped(
        &mut command,
        std::time::Duration::from_secs(120),
        1024 * 1024,
    )
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
