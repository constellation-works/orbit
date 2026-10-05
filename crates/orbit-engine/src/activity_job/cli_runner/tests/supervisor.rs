#![allow(missing_docs)]

use std::path::Path;
use std::time::Duration;

use tempfile::tempdir;

use super::super::supervisor::{
    OutputProgress, ProgressReporter, SpawnTraceContext, SpawnWithTimeoutRequest,
    spawn_with_timeout,
};
use super::test_support::sh_args;

fn spawn_test_request<'a>(
    program: &'a str,
    args: &'a [String],
    cwd: Option<&'a Path>,
    timeout: Duration,
    trace: SpawnTraceContext<'a>,
) -> SpawnWithTimeoutRequest<'a> {
    SpawnWithTimeoutRequest {
        program,
        args,
        stdin_bytes: b"",
        env: &[],
        cwd,
        timeout,
        sandbox: None,
        trace,
        output_capture_limit: None,
        on_spawn: None,
        on_progress: None,
        wait: None,
        live_readers: None,
        spawned_child: None,
        cancel_pair: None,
    }
}

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

fn read_pid(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .expect("read pid file")
        .trim()
        .parse()
        .expect("parse pid")
}

fn wait_until<F>(timeout: Duration, mut condition: F) -> bool
where
    F: FnMut() -> bool,
{
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    condition()
}

fn process_is_live(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return false;
    }
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output();
    let Ok(output) = output else {
        return true;
    };
    if !output.status.success() {
        return false;
    }
    let status = String::from_utf8_lossy(&output.stdout);
    !status.trim_start().starts_with('Z')
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
