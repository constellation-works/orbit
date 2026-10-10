//! Shared libtest re-exec guards at their public boundary. This separate area
//! binary supplies real ignored child entry points without storage fixtures.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::process::Command;
use std::time::Duration;

use orbit_common::test_env::{
    FixtureProgress, INHERITED_AUTHORITY_ENV, MANAGED_RUN_ENV, SCRUBBED_MARKER_ENV,
    assert_child_test_exists, assert_child_test_passed, clear_inherited_authority, run_child_test,
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

orbit_common::isolate_test_process!();

/// ORB-14926: a test process launched from a managed run's shell inherits its
/// `ORBIT_*` authority. The scrub must remove it before any test runs, and
/// leave alone what a scrubbed parent exports to a child on purpose.
#[test]
fn managed_run_env_is_absent_in_a_test_process_the_parent_exported_it_to() {
    for (expect, preset_marker) in [("absent", false), ("kept", true)] {
        let dir = tempfile::tempdir().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "env_probe_child", "--ignored", "--nocapture"])
            .env("ORBIT_ISOLATION_EXPECT", expect);
        for name in INHERITED_AUTHORITY_ENV {
            command.env(name, "exported");
        }
        if preset_marker {
            command.env(SCRUBBED_MARKER_ENV, "1");
        } else {
            command.env_remove(SCRUBBED_MARKER_ENV);
        }
        let output = run_child_test(&mut command, "env_probe_child", dir.path());
        assert_child_test_passed(
            "env_probe_child",
            output.status,
            &output.stdout,
            &output.stderr,
        );
    }
}

#[test]
#[ignore = "re-executed by the managed-run environment regression"]
fn env_probe_child() {
    let expect = std::env::var("ORBIT_ISOLATION_EXPECT").unwrap();
    for name in INHERITED_AUTHORITY_ENV {
        assert_eq!(
            std::env::var_os(name).is_some(),
            expect == "kept",
            "`{name}` must be {expect} in a test process"
        );
    }
    for name in MANAGED_RUN_ENV {
        assert!(
            INHERITED_AUTHORITY_ENV.contains(name),
            "`{name}` unscrubbed"
        );
    }
}

/// ORB-15240: nextest interrupting a test parent must not leave the fixture
/// child group it spawned running. The parent is SIGKILLed mid-run, which
/// gives it no chance to run its own cleanup.
#[cfg(unix)]
mod orphaned_fixture_children {
    use super::*;
    use orbit_common::test_env::run_child_test_within;
    use std::io::Write;
    use std::path::Path;
    use std::time::Instant;

    const DIR_ENV: &str = "ORBIT_ORPHAN_FIXTURE_DIR";
    /// Long enough that a survivor is unmistakable, short enough that a failed
    /// run does not leave processes for long.
    const SURVIVOR_SECS: &str = "120";
    const BOUND: Duration = Duration::from_secs(20);

    fn reexec(entry: &str, dir: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                &format!("orphaned_fixture_children::{entry}"),
                "--ignored",
                "--nocapture",
            ])
            .env(DIR_ENV, dir);
        command
    }

    fn wait_for_pid(path: &Path) -> u32 {
        let started = Instant::now();
        loop {
            if let Some(pid) = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            assert!(
                started.elapsed() < BOUND,
                "{} never written",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Running and not a zombie: a killed process nobody has reaped yet is gone.
    fn is_running(pid: u32) -> bool {
        let output = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&output.stdout);
        let state = state.trim();
        !state.is_empty() && !state.starts_with('Z')
    }

    fn assert_gone_within_bound(what: &str, pid: u32) {
        let started = Instant::now();
        while is_running(pid) {
            assert!(
                started.elapsed() < BOUND,
                "{what} (pid {pid}) still running {BOUND:?} after its test parent died"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn kill_leftovers(pids: &[u32]) {
        for pid in pids {
            // Best effort so a failed assertion does not leave the sleepers.
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }

    #[test]
    fn a_killed_test_parent_takes_its_fixture_child_group_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut parent = reexec("orphan_parent_entry", dir.path());
        let mut parent = parent
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let child = wait_for_pid(&dir.path().join("child.pid"));
        let grandchild = wait_for_pid(&dir.path().join("grandchild.pid"));
        assert!(
            is_running(child) && is_running(grandchild),
            "fixture started"
        );

        parent.kill().unwrap();
        parent.wait().unwrap();

        let outcome = std::panic::catch_unwind(|| {
            assert_gone_within_bound("fixture child", child);
            assert_gone_within_bound("fixture grandchild", grandchild);
        });
        if outcome.is_err() {
            kill_leftovers(&[child, grandchild]);
        }
        outcome.unwrap();
    }

    #[test]
    fn an_overrunning_child_is_reported_with_its_output_and_its_group_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = reexec("orphan_child_entry", dir.path());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_child_test_within(
                &mut command,
                "orphan_child_entry",
                dir.path(),
                Duration::from_secs(2),
            )
        }));
        let message = panic_message(result.expect_err("an overrunning child fails the caller"));
        for expected in ["orphan_child_entry", "host load", "fixture child running"] {
            assert!(message.contains(expected), "{expected:?} in {message}");
        }
        let child = wait_for_pid(&dir.path().join("child.pid"));
        let grandchild = wait_for_pid(&dir.path().join("grandchild.pid"));
        let outcome = std::panic::catch_unwind(|| {
            assert_gone_within_bound("overrun child", child);
            assert_gone_within_bound("overrun grandchild", grandchild);
        });
        if outcome.is_err() {
            kill_leftovers(&[child, grandchild]);
        }
        outcome.unwrap();
    }

    /// Plays the nextest-owned test process: runs a fixture child and waits.
    #[test]
    #[ignore = "re-executed by the killed-parent regression"]
    fn orphan_parent_entry() {
        let dir = std::path::PathBuf::from(std::env::var_os(DIR_ENV).unwrap());
        let mut command = reexec("orphan_child_entry", &dir);
        run_child_test(&mut command, "orphan_child_entry", &dir);
    }

    /// The fixture child: records its pid, spawns a descendant, then lingers.
    #[test]
    #[ignore = "re-executed by the killed-parent regression"]
    fn orphan_child_entry() {
        let dir = std::path::PathBuf::from(std::env::var_os(DIR_ENV).unwrap());
        let mut grandchild = Command::new("sleep").arg(SURVIVOR_SECS).spawn().unwrap();
        std::fs::write(dir.join("grandchild.pid"), grandchild.id().to_string()).unwrap();
        std::fs::write(dir.join("child.pid"), std::process::id().to_string()).unwrap();
        let _ = writeln!(std::io::stdout(), "fixture child running");
        grandchild.wait().unwrap();
    }
}
