//  Shared libtest re-exec guards at their public boundary. Ignored child entry
//  points live in this area binary beside the other orbit-common cases.

const CHILD: &str = "guard_child";
const MISSING: &str = "removed_child_entry_point";
const NESTED_ENV_GUARD_CHILD: &str = "nested_env_guard_child";
const CONTENDED_ENV_GUARD_CHILD: &str = "contended_env_guard_child";
const CONTENDED_ENV: &str = "ORBIT_COMMON_CONTENDED_ENV_TEST";

#[test]
fn a_nested_test_env_guard_panics_within_a_bounded_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        NESTED_ENV_GUARD_CHILD,
        "--ignored",
        "--nocapture",
    ]);
    let output = orbit_common::test_env::run_child_test_within(
        &mut command,
        NESTED_ENV_GUARD_CHILD,
        dir.path(),
        Duration::from_millis(500),
        Duration::from_secs(10),
    );
    assert_child_test_passed(
        NESTED_ENV_GUARD_CHILD,
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

#[test]
#[ignore = "re-executed by the nested test_env guard regression"]
fn nested_env_guard_child() {
    let _ = std::io::Write::write_all(
        &mut std::io::stdout(),
        b"attempting a nested test_env guard\n",
    );
    let _first = orbit_common::test_env::scoped([("ORBIT_COMMON_NESTED_ENV_TEST", Some("first"))]);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _second = orbit_common::test_env::unset(["ORBIT_COMMON_NESTED_ENV_TEST"]);
    }))
    .expect_err("a nested test_env guard must panic immediately");
    let message = panic_message(panic);
    assert!(
        message.contains("only one `ScopedEnv` may be held per scope")
            && message.contains("combine variables into one `scoped` call"),
        "the nested guard panic must explain the one-guard rule: {message}"
    );
}

#[test]
fn a_test_env_guard_on_another_thread_waits_and_restores_values() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            CONTENDED_ENV_GUARD_CHILD,
            "--ignored",
            "--nocapture",
        ])
        .env(CONTENDED_ENV, "baseline");
    let output = orbit_common::test_env::run_child_test_within(
        &mut command,
        CONTENDED_ENV_GUARD_CHILD,
        dir.path(),
        Duration::from_secs(2),
        Duration::from_secs(10),
    );
    assert_child_test_passed(
        CONTENDED_ENV_GUARD_CHILD,
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

#[test]
#[ignore = "re-executed by the cross-thread test_env guard regression"]
fn contended_env_guard_child() {
    let first = orbit_common::test_env::scoped([(CONTENDED_ENV, Some("outer"))]);
    assert_eq!(std::env::var(CONTENDED_ENV).as_deref(), Ok("outer"));

    let (attempting_tx, attempting_rx) = std::sync::mpsc::channel();
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
    let other = std::thread::spawn(move || {
        attempting_tx.send(()).unwrap();
        let second = orbit_common::test_env::scoped([(CONTENDED_ENV, Some("inner"))]);
        assert_eq!(std::env::var(CONTENDED_ENV).as_deref(), Ok("inner"));
        acquired_tx.send(()).unwrap();
        drop(second);
        assert_eq!(std::env::var(CONTENDED_ENV).as_deref(), Ok("baseline"));
    });

    attempting_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the other thread should attempt to acquire its guard");
    assert!(
        matches!(
            acquired_rx.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "the other thread must wait while the first guard is held"
    );
    drop(first);
    acquired_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the other thread should acquire its guard after the first drops");
    other
        .join()
        .expect("the other thread should restore the baseline value");
    assert_eq!(std::env::var(CONTENDED_ENV).as_deref(), Ok("baseline"));
}

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

/// ORB-15293: a golden fixture slowed by write latency on a loaded host was
/// failed by a deadline measured from its start while it still advanced. The
/// deadline bounds the gap between items, not the fixture's total.
#[test]
fn a_fixture_that_keeps_advancing_outlives_its_stall_deadline() {
    let deadline = Duration::from_secs(2);
    let mut progress = FixtureProgress::with_deadline("slow cases", deadline);
    progress.phase("cases", 30);
    let started = std::time::Instant::now();
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        progress.advance();
    }
    assert!(
        started.elapsed() > deadline,
        "the fixture outran its deadline"
    );
    progress.finish();
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
