//! The runtime test binary starts without the managed-run worker binding that
//! its parent exports. An inherited `ORBIT_WORKER_CONTEXT_REQUIRED` makes every
//! in-process runtime fail closed (ORB-14934).

use std::process::Command;

use orbit_common::test_env::{SCRUBBED_MARKER_ENV, assert_child_test_passed, run_child_test};

const CHILD: &str = "process_isolation::worker_binding_child";

#[test]
fn an_inherited_worker_binding_is_absent_in_the_runtime_binary() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    // This binary's own scrub set the marker, so clear it: the child then scrubs
    // itself as a fresh test process does.
    command
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .env("ORBIT_WORKER_CONTEXT_REQUIRED", "1")
        .env_remove(SCRUBBED_MARKER_ENV);
    let output = run_child_test(&mut command, CHILD, dir.path());
    assert_child_test_passed(CHILD, output.status, &output.stdout, &output.stderr);
}

#[test]
#[ignore = "re-executed by the worker binding isolation regression"]
fn worker_binding_child() {
    assert!(
        std::env::var_os("ORBIT_WORKER_CONTEXT_REQUIRED").is_none(),
        "the runtime test binary must scrub the inherited worker binding"
    );
}
