//! Exact-test re-execution for bodies that mutate Orbit state or process authority.

use std::process::Command;

use orbit_common::test_env;

/// Run the complete body in a child, returning true when the parent is done.
/// The marker names the exact test so a nested fixture cannot bypass isolation.
pub(crate) fn run_in_child(test: &str) -> bool {
    run_in_child_with_env(test, &[])
}

/// [`run_in_child`] that also sets `env` on the child after inherited authority
/// is cleared, for an envelope the body must see, such as the agent identity.
pub(crate) fn run_in_child_with_env(test: &str, env: &[(&str, &str)]) -> bool {
    const CHILD: &str = "ORBIT_TEST_ISOLATED_BODY";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return false;
    }

    // Keep HOME outside /tmp: Bubblewrap fixtures overlay /tmp with a tmpfs.
    let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orbit/tmp");
    std::fs::create_dir_all(&scratch).expect("isolated test scratch");
    let root = tempfile::tempdir_in(scratch).expect("isolated test root");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("private child home");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", test, "--include-ignored", "--nocapture"])
        .env(CHILD, test)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(&home);
    for (name, value) in env {
        command.env(name, value);
    }
    let output = test_env::run_child_test(&mut command, test, root.path());
    test_env::assert_child_test_passed(test, output.status, &output.stdout, &output.stderr);
    true
}
