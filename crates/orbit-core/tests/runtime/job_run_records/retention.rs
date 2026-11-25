use std::process::Command;
use std::time::Duration;

use chrono::{TimeDelta, TimeZone, Utc};
use orbit_common::{process, test_env};
use orbit_core::runtime::JobRuns;
use orbit_store::Store;
use orbit_store::contracts::{JobRunStepParams, RetentionSelection, RetentionWorkspaceScope};
use orbit_types::workflow::{JobRunState, JobTargetType, PipelineState};
use serde_json::json;

//  Bounded retention through the public retention contract: audit rows and
//  terminal run state older than a cutoff go in short batches, everything
//  else stays.

const CHILD: &str = "ORBIT_STORE_RETENTION_CHILD";

/// Run `test` again in a child process rooted in a fresh directory, or return
/// that directory when already inside the child.
fn store_retention_isolated(test: &str, timeout: Duration) -> Option<std::path::PathBuf> {
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return Some(std::env::current_dir().unwrap());
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, test)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    let output = process::run_bounded_capped(&mut command, timeout, 256 * 1024).unwrap();
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    None
}

fn workspace_only(id: &str) -> RetentionWorkspaceScope<'_> {
    RetentionWorkspaceScope {
        workspace_id: id,
        legacy_ids: None,
    }
}

fn count(store: &Store, sql: &str) -> i64 {
    store
        .connection()
        .lock()
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .expect(sql)
}

/// The calling thread's CPU time. A saturated host stretches wall-clock time
/// without changing the work a batch does, so a batch's cost is read here.
#[cfg(unix)]
fn thread_cpu_time() -> Duration {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::zeroed();
    // Safety: `clock_gettime` fills the `timespec` it is handed and reads nothing else.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, time.as_mut_ptr()) },
        0
    );
    // Safety: a zero return means the kernel filled `time`.
    let time = unsafe { time.assume_init() };
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

#[cfg(not(unix))]
fn thread_cpu_time() -> Duration {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed()
}

/// Run `batch` until it removes nothing. Returns the rows removed and the
/// slowest batch's thread CPU time.
fn drain(mut batch: impl FnMut() -> usize) -> (usize, Duration) {
    let mut removed = 0;
    let mut slowest = Duration::ZERO;
    loop {
        let started = thread_cpu_time();
        let n = batch();
        slowest = slowest.max(thread_cpu_time() - started);
        if n == 0 {
            return (removed, slowest);
        }
        removed += n;
    }
}

#[test]
fn run_retention_drops_terminal_state_and_keeps_runs_and_steps() {
    const TEST: &str =
        "job_run_records::retention::run_retention_drops_terminal_state_and_keeps_runs_and_steps";
    let Some(root) = store_retention_isolated(TEST, Duration::from_secs(60)) else {
        return;
    };
    let store = Store::open(&root.join("orbit.db")).unwrap();
    let jobs = JobRuns::shared(store.clone(), "ws_a");
    let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let old = cutoff - TimeDelta::days(10);

    let mut runs = Vec::new();
    for (name, state, finished) in [
        ("old-success", Some(JobRunState::Success), old),
        ("old-failed", Some(JobRunState::Failed), old),
        ("old-held", Some(JobRunState::Held), old),
        ("old-running", None, old),
        (
            "fresh-success",
            Some(JobRunState::Success),
            cutoff + TimeDelta::days(1),
        ),
    ] {
        let run = jobs.insert_job_run(name, 1, old, None, None).unwrap();
        let state_doc = PipelineState::new(
            run.run_id.clone(),
            run.job_id.clone(),
            json!({ "payload": "x".repeat(4096) }),
        );
        assert!(jobs.initialize_run_state(&run.run_id, &state_doc).unwrap());
        jobs.mark_job_run_running(&run.run_id, old, std::process::id())
            .unwrap();
        jobs.complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                lock_wait_ms: None,
                step_index: 0,
                target_type: JobTargetType::Activity,
                target_id: "implement".into(),
                started_at: old,
                finished_at: finished,
                duration_ms: Some(1),
                exit_code: Some(0),
                agent_response_json: Some(json!({ "summary": "done" })),
                state: JobRunState::Success,
                error_code: None,
                error_message: None,
            },
        )
        .unwrap();
        if let Some(state) = state {
            jobs.finalize_job_run(&run.run_id, state, finished, Some(1))
                .unwrap();
        }
        runs.push((name, run.run_id));
    }
    let other = JobRuns::shared(store.clone(), "ws_b");
    let foreign = other.insert_job_run("foreign", 1, old, None, None).unwrap();
    other
        .initialize_run_state(
            &foreign.run_id,
            &PipelineState::new(foreign.run_id.clone(), "foreign".into(), json!({})),
        )
        .unwrap();
    other
        .mark_job_run_running(&foreign.run_id, old, std::process::id())
        .unwrap();
    other
        .finalize_job_run(&foreign.run_id, JobRunState::Success, old, Some(1))
        .unwrap();

    let plan = store
        .read()
        .unwrap()
        .retention()
        .run_state_selection(workspace_only("ws_a"), cutoff)
        .unwrap();
    assert_eq!(plan.rows, 2, "old success and old failed only");
    assert!(plan.bytes >= 2 * 4096);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM job_run_states"), 6);

    let archived_at = Utc::now();
    let (archived, _) = drain(|| {
        store
            .write(|tx| {
                tx.retention()
                    .archive_run_states(workspace_only("ws_a"), cutoff, archived_at, 1)
            })
            .unwrap()
    });
    assert_eq!(archived, 2);

    for (name, run_id) in &runs {
        let run = jobs.get_job_run(run_id).unwrap().expect("run row kept");
        assert_eq!(run.steps.len(), 1, "{name} keeps its steps");
        let state = jobs.read_run_state(run_id).unwrap();
        let archived = count(
            &store,
            &format!(
                "SELECT COUNT(*) FROM job_runs WHERE run_id = '{run_id}' AND archived_at IS NOT NULL"
            ),
        );
        if matches!(*name, "old-success" | "old-failed") {
            assert!(state.is_none(), "{name} pipeline state dropped");
            assert_eq!(archived, 1, "{name} stamped archived");
        } else {
            assert!(state.is_some(), "{name} pipeline state kept");
            assert_eq!(archived, 0, "{name} untouched");
        }
    }
    assert!(other.read_run_state(&foreign.run_id).unwrap().is_some());
    assert_eq!(
        store
            .read()
            .unwrap()
            .retention()
            .run_state_selection(workspace_only("ws_a"), cutoff)
            .unwrap(),
        RetentionSelection::default()
    );
}
