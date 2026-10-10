use std::time::Duration;

use tempfile::tempdir;

use super::super::super::tests::test_support::sh_args;
use super::super::{
    OutputProgress, ProgressReporter, SpawnTraceContext, SpawnWithTimeoutRequest,
    spawn_with_timeout,
};
use super::test_support::{process_is_live, read_pid, shell_quote, spawn_test_request, wait_until};

#[test]
fn spawn_with_timeout_kills_grandchild_holding_output_pipes() {
    let pid_dir = tempdir().expect("pid tempdir");
    let pid_file = pid_dir.path().join("grandchild.pid");
    let script = format!(
        "(sleep 30) & child=$!; printf '%s\\n' \"$child\" > {}; printf '%s\\n' 'before timeout'; sleep 30",
        shell_quote(pid_file.to_string_lossy().as_ref())
    );
    let args = sh_args(&script);

    let started = std::time::Instant::now();
    let (stdout, stderr, exit_code, duration, timed_out) = spawn_with_timeout(spawn_test_request(
        "/bin/sh",
        &args,
        None,
        Duration::from_millis(150),
        SpawnTraceContext {
            provider: "codex",
            job_run_id: "job-timeout-tree",
            task_id: Some("TTREE"),
            cwd: None,
        },
    ))
    .expect("spawn succeeds");

    assert!(timed_out);
    assert_eq!(exit_code, None);
    assert_eq!(stdout.bytes(), b"before timeout\n");
    assert!(stderr.bytes().is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "timeout path should return promptly; reported duration={duration:?}"
    );

    let grandchild_pid = read_pid(&pid_file);
    assert!(
        wait_until(Duration::from_secs(2), || !process_is_live(grandchild_pid)),
        "grandchild process {grandchild_pid} should be gone after timeout"
    );
}

/// [ORB-13899] A long-running child is observable before it exits: the
/// supervisor samples what it has written so far while it still runs.
#[test]
fn a_running_childs_output_is_sampled_before_it_exits() {
    let args = sh_args("printf '%s\\n' 'reading config'; sleep 1");
    let samples = std::cell::RefCell::new(Vec::<(usize, Vec<u8>)>::new());
    let record = |progress: &OutputProgress| {
        samples
            .borrow_mut()
            .push((progress.observed_bytes, progress.recent.clone()));
    };
    let (stdout, _stderr, exit_code, _duration, timed_out) =
        spawn_with_timeout(SpawnWithTimeoutRequest {
            on_progress: Some(ProgressReporter {
                interval: Duration::from_millis(100),
                report: &record,
            }),
            ..spawn_test_request(
                "/bin/sh",
                &args,
                None,
                Duration::from_secs(10),
                SpawnTraceContext {
                    provider: "codex",
                    job_run_id: "job-progress",
                    task_id: None,
                    cwd: None,
                },
            )
        })
        .expect("spawn succeeds");

    assert!(!timed_out);
    assert_eq!(exit_code, Some(0));
    assert_eq!(stdout.bytes(), b"reading config\n");
    let samples = samples.into_inner();
    assert!(
        samples
            .iter()
            .any(|(observed, recent)| *observed == 15 && recent == b"reading config\n"),
        "the child's first line must be sampled while it sleeps: {samples:?}"
    );
}
