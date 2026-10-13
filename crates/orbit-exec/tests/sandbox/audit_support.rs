//! Environment-sensitive compiler fixtures re-execute one test in a child.
use std::process::{Command, Stdio};
use std::time::Duration;

pub(super) fn run_isolated(test: &str) -> bool {
    const CHILD: &str = "ORBIT_EXEC_AUDIT_FIXTURE_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return false;
    }
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, test)
        .stdin(Stdio::null());
    let output = orbit_common::process::run_bounded_capped(
        &mut command,
        Duration::from_secs(120),
        1024 * 1024,
    )
    .expect("isolated fixture");
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        output.stdout,
        output.stderr,
    );
    true
}
