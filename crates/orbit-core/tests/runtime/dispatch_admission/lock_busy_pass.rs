//! A drain admission pass that loses only to a busy task-store lock.
//!
//! One admission section held past a waiter's 30 s deadline used to fail the
//! drain's admission step, which ended the whole drain while its children kept
//! running; nothing new started until an operator restarted it (ORB-16376).
//! Now the pass is deferred: it admits nothing, records the lock error with its
//! holder on the drain's last pass, gives back the host load reservations
//! `enforce` gap fit wrote for its wave, and the next pass retries. A drain
//! whose passes keep losing to the lock still fails, at the deferral limit, and
//! any other admission failure still fails at once.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orbit_common::fs::io::{
    FileLockOptions, with_exclusive_file_lock_options, with_shared_file_lock_options,
};
use orbit_core::OrbitRuntime;
use orbit_core::runtime::host_resource::forecast::{ForecastDecision, HostLoadForecast};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::workflow::{DrainAdmissionPass, PipelineState};
use serde_json::{Value, json};

use super::{Seed, as_operator, isolated, running_run, runtime, seed};

const TEST: &str = "dispatch_admission::lock_busy_pass::a_drain_defers_an_admission_pass_held_out_by_the_task_commit_lock";

/// Set in the lock holder: the boundary lock target it holds exclusively.
const HOLD_TARGET: &str = "ORBIT_TEST_LOCK_BUSY_PASS_TARGET";
/// The holder writes this file once it holds the lock.
const HOLD_READY: &str = "ORBIT_TEST_LOCK_BUSY_PASS_READY";
/// The holder releases the lock once this file exists.
const HOLD_RELEASE: &str = "ORBIT_TEST_LOCK_BUSY_PASS_RELEASE";
/// Set in the lock holder to hold the lock shared, as an ordinary section
/// does, instead of exclusively, as an admission section does.
const HOLD_SHARED: &str = "ORBIT_TEST_LOCK_BUSY_PASS_SHARED";

/// The longest the holder keeps the lock if the test never releases it.
const HOLD_CEILING: Duration = Duration::from_secs(300);

/// One admission pass of `drain`, with `enforce` gap fit.
fn classify(runtime: &OrbitRuntime, drain: &str, extra: Value) -> Result<Value, String> {
    let mut input = json!({
        "run_id": drain,
        "max_active_leaf_runs": 2,
        "poll_sleep_seconds": 7,
        "auto_concurrency": "enforce",
    });
    if let (Some(input), Value::Object(extra)) = (input.as_object_mut(), extra) {
        input.extend(extra);
    }
    crate::scenarios::with_immutable_host_load(|| {
        runtime
            .run_deterministic(
                "classify_workspace_auto_tasks",
                &json!({}),
                &input,
                ToolContext::default(),
            )
            .map_err(|error| error.to_string())
    })
}

/// The tasks reserved on the host's load forecast, one entry per reservation.
fn reserved(runtime: &OrbitRuntime) -> Vec<String> {
    crate::scenarios::with_immutable_host_load(|| {
        runtime
            .admit_against_forecast(|forecast: &HostLoadForecast| {
                Ok(ForecastDecision {
                    outcome: forecast
                        .reservations
                        .iter()
                        .map(|reservation| reservation.task_id.clone())
                        .collect(),
                    admitted: Vec::new(),
                })
            })
            .unwrap()
    })
}

fn last_pass(runtime: &OrbitRuntime, drain: &str) -> Value {
    as_operator(runtime, "orbit.workflow.run.show", json!({"id": drain}))["drain_last_pass"].clone()
}

/// The task-commit boundary lock file of the workspace's task partition.
fn partition_lock(root: &Path) -> PathBuf {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name == ".task-commit.lock")
            {
                return path;
            }
        }
    }
    panic!("no task-commit lock under {}", root.display());
}

/// Hold the boundary lock, exclusively as an admission section does or
/// shared as an ordinary section does, until the test releases it.
fn hold_lock(target: &Path) {
    let ready = PathBuf::from(std::env::var_os(HOLD_READY).unwrap());
    let release = PathBuf::from(std::env::var_os(HOLD_RELEASE).unwrap());
    let options = FileLockOptions {
        record_shared_holders: true,
        prefer_exclusive_waiters: true,
        ..FileLockOptions::default()
    };
    let hold = || -> Result<(), std::io::Error> {
        std::fs::write(&ready, std::process::id().to_string())?;
        let started = Instant::now();
        while !release.exists() && started.elapsed() < HOLD_CEILING {
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    };
    if std::env::var_os(HOLD_SHARED).is_some() {
        with_shared_file_lock_options(
            target,
            "task commit boundary: ordinary (fixture holder)",
            options,
            hold,
        )
    } else {
        with_exclusive_file_lock_options(
            target,
            "task commit boundary: admission (fixture holder)",
            options,
            hold,
        )
    }
    .unwrap();
}

/// A child process of this test holding the partition's boundary lock.
struct Holder {
    child: orbit_common::process::OwnedChild,
    pid: String,
    release: PathBuf,
}

impl Holder {
    fn spawn(lock: &Path, signals: &Path, shared: bool) -> Self {
        let ready = signals.join(format!("ready-{shared}"));
        let release = signals.join(format!("release-{shared}"));
        let mut holder = std::process::Command::new(std::env::current_exe().unwrap());
        holder
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(HOLD_TARGET, lock.with_file_name("task-commit"))
            .env(HOLD_READY, &ready)
            .env(HOLD_RELEASE, &release)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if shared {
            holder.env(HOLD_SHARED, "1");
        }
        let child = orbit_common::process::OwnedChild::from_spawned(holder.spawn().unwrap());
        wait_for(&ready);
        let pid = std::fs::read_to_string(&ready).unwrap();
        Self {
            child,
            pid,
            release,
        }
    }

    fn release(mut self) {
        std::fs::write(&self.release, "").unwrap();
        let status = self.child.child_mut().wait().unwrap();
        self.child.mark_reaped();
        assert!(status.success(), "the lock holder exited {status}");
    }
}

fn wait_for(path: &Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the lock holder never reported holding the lock"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Two lock holders, each across one admission pass longer than one
/// acquisition deadline. A shared holder, as ordinary task-store work holds
/// the lock, lets an `enforce` pass read the backlog and reserve its wave but
/// holds out its admission section: the pass is deferred, the drain stays
/// running and records `lock_busy` naming the holder, and the host's
/// reservations are what they were before the pass. An exclusive holder, as
/// an admission section holds it, stops a pass at its first step, recording
/// pilot operator handoffs, the site the incident hit; a drain already one
/// deferred pass short of the limit fails there with the lock error. Once the
/// holders let go, the deferred drain admits the task with one reservation.
/// A non-lock failure still fails the pass at once and records no deferral.
#[test]
fn a_drain_defers_an_admission_pass_held_out_by_the_task_commit_lock() {
    if !isolated(TEST) {
        return;
    }
    if let Some(target) = std::env::var_os(HOLD_TARGET) {
        hold_lock(Path::new(&target));
        return;
    }
    let (root, runtime, _repo) = runtime();
    let task = seed(&runtime, Seed::default());
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let exhausted = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    // A failure that is not a lock wait fails the pass as it always did.
    let refused = classify(&runtime, &drain, json!({"max_tasks": "many"}))
        .expect_err("an invalid input still fails the pass");
    assert!(refused.contains("max_tasks"), "{refused}");
    assert_eq!(
        last_pass(&runtime, &drain),
        Value::Null,
        "no deferral recorded"
    );

    let lock = partition_lock(root.path());
    let signals = tempfile::TempDir::new_in(root.path()).unwrap();
    let before = reserved(&runtime);

    // Held out of the admission section after gap fit reserved the wave.
    let holder = Holder::spawn(&lock, signals.path(), true);
    let deferred = classify(&runtime, &drain, json!({}))
        .expect("a pass held out by the lock is deferred, not failed");
    assert_eq!(deferred["has_leaves"], false, "{deferred}");
    assert_eq!(deferred["idle"], true, "{deferred}");
    assert_eq!(deferred["loose_task_dispatches"], json!([]), "{deferred}");
    assert_eq!(
        deferred["sleep_seconds"], 7,
        "the next pass waits the drain's poll interval: {deferred}"
    );
    assert_eq!(deferred["pass_deferred"]["code"], "lock_busy", "{deferred}");
    let pass = last_pass(&runtime, &drain);
    assert_eq!(pass["last_pass_error_code"], "lock_busy", "{pass}");
    assert_eq!(pass["consecutive_pass_failures"], 1, "{pass}");
    let error = pass["last_pass_error"].as_str().unwrap_or_default();
    assert!(
        error.starts_with("admit the wave through the admission section: ")
            && error.contains(&format!("pid {} ", holder.pid)),
        "the deferred pass names the admission section and the lock's holder: {pass}"
    );
    assert_eq!(
        reserved(&runtime),
        before,
        "a deferred pass leaves no reservation of the wave it did not admit"
    );
    let shown = as_operator(&runtime, "orbit.workflow.run.show", json!({"id": drain}));
    assert_eq!(shown["state"], "running", "{shown}");
    holder.release();

    // One busy pass short of the limit, the next busy pass fails the drain
    // with the lock error.
    let holder = Holder::spawn(&lock, signals.path(), false);
    let limit = deferred["pass_deferred"]["limit"].as_u64().unwrap();
    let mut state: PipelineState = runtime.read_run_state(&exhausted).unwrap().unwrap();
    let mut streak: DrainAdmissionPass = serde_json::from_value(pass.clone()).unwrap();
    streak.consecutive_pass_failures = u32::try_from(limit - 1).unwrap();
    state.pull_drain.drain_last_pass = Some(streak);
    runtime.write_run_state(&exhausted, &state).unwrap();
    let failed =
        classify(&runtime, &exhausted, json!({})).expect_err("a pass at the deferral limit fails");
    assert!(
        failed.contains(".task-commit.lock")
            && failed.contains(&format!("held by pid {} ", holder.pid)),
        "the failure carries the lock error: {failed}"
    );
    let pass = last_pass(&runtime, &exhausted);
    assert_eq!(pass["last_pass_error_code"], "lock_busy", "{pass}");
    assert_eq!(pass["consecutive_pass_failures"], limit, "{pass}");
    assert!(
        pass["last_pass_error"]
            .as_str()
            .is_some_and(|error| error.starts_with("record pilot operator handoffs: ")),
        "an admission section stops the pass where the incident did: {pass}"
    );
    holder.release();

    let admitted = classify(&runtime, &drain, json!({})).expect("the next pass admits");
    assert_eq!(admitted["loose_task_ids"], json!([task.id]), "{admitted}");
    let pass = last_pass(&runtime, &drain);
    assert_eq!(pass["consecutive_pass_failures"], 0, "{pass}");
    assert_eq!(pass.get("last_pass_error_code"), None, "{pass}");
    let mut expected = before;
    expected.push(task.id.clone());
    assert_eq!(
        reserved(&runtime),
        expected,
        "the admitted task holds one reservation, with none left by the deferred pass"
    );
}
