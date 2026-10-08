//! Shared libtest re-exec guards at their public boundary. This separate area
//! binary supplies real ignored child entry points without storage fixtures.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::process::Command;
use std::time::Duration;

use orbit_common::test_env::{
    FixtureProgress, assert_child_test_exists, assert_child_test_passed, clear_inherited_authority,
};

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

/// Every variable `clear_inherited_authority` removed when the claimed
/// executor's affected gate stopped passing the worker-binding marker to test
/// processes. Fixtures that spawn `orbit` still rely on this list; the gate
/// change must not narrow it.
const CLEARED_AUTHORITY: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "ORBIT_ROOT",
    "ORBIT_REGISTRY_ROOT",
    "ORBIT_WORKSPACE",
    "ORBIT_WORKSPACE_CLAIM_TOKEN",
    "ORBIT_WORKTREE_ROOT",
    "ORBIT_JOB_DIR",
    "ORBIT_ACTIVITY_DIR",
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_WORKER_CONTEXT_REQUIRED",
    "ORBIT_RUN_ID",
    "ORBIT_TASK_ID",
    "ORBIT_ACTIVE_TASK_ID",
    "ORBIT_SESSION_ID",
    "ORBIT_ACTIVITY_ID",
    "ORBIT_STEP_INDEX",
    "ORBIT_AGENT_NAME",
    "ORBIT_AGENT_MODEL",
    "ORBIT_ACTOR",
    "ORBIT_OPERATOR",
    "ORBIT_TASK_ACTOR_KIND",
    "ORBIT_ACTIVITY_TOOLS",
    "ORBIT_ACTIVITY_TOOL_POLICY",
    "ORBIT_ACTIVITY_TOOLS_DENY",
    "ORBIT_ACTIVITY_NAME",
    "ORBIT_ACTIVITY_FS_PROFILE",
    "ORBIT_ACTIVITY_DEADLINE_UNIX_MS",
    "ORBIT_PROC_ALLOWED_PROGRAMS",
    "ORBIT_PROC_PROGRAM_POLICY",
    "ORBIT_PROC_DISALLOWED_PROGRAMS",
    "ORBIT_BIN",
    "ORBIT_PLUGIN_BROKER",
];

#[test]
fn inherited_authority_is_removed_from_a_child_command() {
    let mut command = Command::new("true");
    for name in CLEARED_AUTHORITY {
        command.env(name, "inherited");
    }
    command.env("ORBIT_SCRATCH_DIR", "kept");
    clear_inherited_authority(|name| {
        command.env_remove(name);
    });

    let envs = command
        .get_envs()
        .map(|(name, value)| (name.to_string_lossy().into_owned(), value.is_some()))
        .collect::<std::collections::BTreeMap<_, _>>();
    for name in CLEARED_AUTHORITY {
        assert_eq!(
            envs.get(*name),
            Some(&false),
            "inherited authority `{name}` must not reach a fixture's child"
        );
    }
    assert_eq!(envs.get("ORBIT_SCRATCH_DIR"), Some(&true));
}

/// ORB-14818: a large fixture that overran its deadline printed only
/// `running 1 test`, so a slow seed could not be told from a slow measured
/// section. The overrun must name the fixture, its phase and the items done.
#[test]
fn a_fixture_overrun_names_its_phase_and_progress() {
    let mut progress = FixtureProgress::with_deadline("partition", Duration::from_millis(50));
    progress.phase("seed tasks", 5_000);
    for _ in 0..3 {
        progress.advance();
    }
    std::thread::sleep(Duration::from_millis(60));
    let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| progress.advance()))
        .expect_err("an advance past the deadline fails the fixture");
    let message = panic_message(error);
    for expected in ["fixture `partition`", "phase `seed tasks`", "4/5000 done"] {
        assert!(message.contains(expected), "{expected:?} in {message}");
    }
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
