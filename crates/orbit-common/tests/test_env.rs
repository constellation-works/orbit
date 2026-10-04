//! Shared libtest re-exec guards at their public boundary. This separate area
//! binary supplies real ignored child entry points without storage fixtures.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::process::Command;

use orbit_common::test_env::{assert_child_test_exists, assert_child_test_passed};

const CHILD: &str = "guard_child";
const MISSING: &str = "removed_child_entry_point";

#[test]
fn reexec_guard_rejects_missing_ignored_and_failed_children() {
    // ORB-13911: libtest exits zero when an exact filter no longer exists.
    for (name, ignored, fail, should_pass) in [
        (CHILD, true, false, true),
        (MISSING, true, false, false),
        (CHILD, false, false, false),
        (CHILD, true, true, false),
    ] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", name, "--nocapture"]);
        if ignored {
            command.arg("--ignored");
        }
        if fail {
            command.env("ORBIT_GUARD_CHILD_FAIL", "1");
        } else {
            command.env_remove("ORBIT_GUARD_CHILD_FAIL");
        }
        let output = command.output().unwrap();
        if !fail {
            assert!(
                output.status.success(),
                "libtest's zero-test run exits zero"
            );
        }
        let result = std::panic::catch_unwind(|| {
            assert_child_test_passed(name, output.status, &output.stdout, &output.stderr);
        });
        assert_eq!(result.is_ok(), should_pass, "child selection: {name}");
        if let Err(error) = result {
            assert!(
                panic_message(error).contains(name),
                "guard must name `{name}`"
            );
        }
    }

    assert_child_test_exists(CHILD);
    let error = std::panic::catch_unwind(|| assert_child_test_exists(MISSING)).unwrap_err();
    assert!(panic_message(error).contains(MISSING));
}

fn panic_message(error: Box<dyn std::any::Any + Send>) -> String {
    match error.downcast::<String>() {
        Ok(message) => *message,
        Err(error) => error
            .downcast::<&str>()
            .map(|message| message.to_string())
            .unwrap(),
    }
}

#[test]
#[ignore = "re-executed by the shared child-test guard regression"]
fn guard_child() {
    assert!(
        std::env::var_os("ORBIT_GUARD_CHILD_FAIL").is_none(),
        "deliberate child failure"
    );
}
