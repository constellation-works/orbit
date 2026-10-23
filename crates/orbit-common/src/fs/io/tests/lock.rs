use std::time::{Duration, Instant};

/// [ORB-12029]: the previous implementation checked `symlink_metadata` on the
/// resolved candidate and opened it in a later, unguarded `File::open` — a
/// final component swapped to a symlink in between would be followed by that
/// open, exposing the swapped target's content. This deterministically
/// installs that swap between path resolution and the open (rather than
/// relying on a real race window) to prove `O_NOFOLLOW` rejects it instead of
/// reading through.
#[cfg(unix)]
#[test]
fn read_file_lock_holder_rejects_a_final_symlink_swapped_after_the_path_check() {
    use crate::fs::file_lock::read_file_lock_holder_after_resolve;

    let root = tempfile::tempdir().expect("root tempdir");
    let lock_path = root.path().join(".task.yaml.lock");
    let checked_body = br#"{"pid":1,"acquired_at":"2026-01-01T00:00:00Z","label":"checked"}"#;
    std::fs::write(&lock_path, checked_body).expect("write checked holder");

    let outside = tempfile::tempdir().expect("outside tempdir");
    let outside_path = outside.path().join("outside.json");
    let outside_body = br#"{"pid":2,"acquired_at":"2026-01-01T00:00:00Z","label":"outside"}"#;
    std::fs::write(&outside_path, outside_body).expect("write outside holder");

    let preserved_path = root.path().join("checked-holder.json");

    let holder = read_file_lock_holder_after_resolve(&lock_path, |checked_path| {
        std::fs::rename(checked_path, &preserved_path).expect("preserve checked holder");
        std::os::unix::fs::symlink(&outside_path, checked_path).expect("install swapped symlink");
    });

    assert!(
        holder.is_none(),
        "the no-follow open must reject the swapped final symlink"
    );
    assert_eq!(
        std::fs::read(&preserved_path).expect("read preserved checked holder"),
        checked_body,
        "the originally checked file must be untouched"
    );
    assert_eq!(
        std::fs::read(&outside_path).expect("read outside holder"),
        outside_body,
        "the outside file must never be read or written through the swap"
    );
}

#[cfg(unix)]
#[test]
fn read_file_lock_holder_does_not_block_on_a_fifo_swapped_after_the_path_check() {
    let root = tempfile::tempdir().expect("root tempdir");
    let lock_path = root.path().join(".task.yaml.lock");
    std::fs::write(&lock_path, br#"{"pid":1}"#).expect("write checked holder");
    let stage_path = root.path().join("fifo-reader-stage");
    let mut child = start_fifo_reader(&lock_path, &stage_path, false);

    wait_for_fifo_stage(&mut child, &stage_path, Duration::from_secs(1));
    wait_for_fifo_reader_completion(&mut child, Duration::from_secs(1));
}

/// A deliberately blocking open exercises the timeout cleanup path used by
/// the FIFO regression without leaving a test worker behind.
#[cfg(unix)]
#[test]
fn read_file_lock_holder_fifo_controlled_block_is_terminated_and_joined() {
    let root = tempfile::tempdir().expect("root tempdir");
    let lock_path = root.path().join(".task.yaml.lock");
    std::fs::write(&lock_path, br#"{"pid":1}"#).expect("write checked holder");

    let stage_path = root.path().join("fifo-reader-stage");
    let mut child = start_fifo_reader(&lock_path, &stage_path, true);

    wait_for_fifo_stage(&mut child, &stage_path, Duration::from_secs(1));
    assert_fifo_reader_times_out_and_is_joined(&mut child, Duration::from_millis(100));
}

#[cfg(unix)]
fn start_fifo_reader(
    lock_path: &std::path::Path,
    stage_path: &std::path::Path,
    use_unsafe_blocking_open: bool,
) -> std::process::Child {
    crate::test_env::assert_child_test_exists(
        "fs::io::tests::lock::read_file_lock_holder_fifo_reader_child",
    );
    let test_binary = std::env::current_exe().expect("locate test binary");
    let mut command = std::process::Command::new(test_binary);
    command
        .arg("fs::io::tests::lock::read_file_lock_holder_fifo_reader_child")
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture")
        .env("ORBIT_FIFO_READER_LOCK_PATH", lock_path)
        .env("ORBIT_FIFO_READER_STAGE_PATH", stage_path);
    if use_unsafe_blocking_open {
        command.env("ORBIT_FIFO_READER_UNSAFE_BLOCKING_OPEN", "1");
    }
    command.spawn().expect("start controlled FIFO reader")
}

#[cfg(unix)]
fn wait_for_fifo_stage(
    child: &mut std::process::Child,
    stage_path: &std::path::Path,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(stage) = std::fs::read_to_string(stage_path) {
            if stage == "fifo-ready" {
                return;
            }
            terminate_fifo_reader(child);
            panic!("FIFO setup failed before mkfifo readiness: {stage}");
        }

        if let Some(status) = child.try_wait().expect("poll FIFO reader") {
            panic!("FIFO setup exited before mkfifo readiness: {status}");
        }
        if Instant::now() >= deadline {
            terminate_fifo_reader(child);
            panic!("FIFO setup (remove/mkfifo) did not report readiness before timeout");
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn wait_for_fifo_reader_completion(child: &mut std::process::Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll FIFO reader") {
            assert!(
                status.success(),
                "FIFO reader failed after the swap completed: {status}"
            );
            return;
        }
        if Instant::now() >= deadline {
            terminate_fifo_reader(child);
            panic!("FIFO reader did not complete after the swap before timeout");
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn assert_fifo_reader_times_out_and_is_joined(child: &mut std::process::Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll FIFO reader") {
            panic!("unsafe FIFO reader unexpectedly completed: {status}");
        }
        if Instant::now() >= deadline {
            terminate_fifo_reader(child);
            return;
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn terminate_fifo_reader(child: &mut std::process::Child) {
    child.kill().expect("terminate FIFO reader after timeout");
    child.wait().expect("join terminated FIFO reader");
}

/// Runs only as the controlled child of the FIFO regression. Keeping the
/// potentially blocking read in a child lets the parent terminate and join it
/// if a future change drops `O_NONBLOCK`.
#[cfg(unix)]
#[test]
#[ignore = "runs only inside the controlled FIFO regression child process"]
fn read_file_lock_holder_fifo_reader_child() {
    use crate::fs::file_lock::read_file_lock_holder_after_resolve;

    let Ok(lock_path) = std::env::var("ORBIT_FIFO_READER_LOCK_PATH") else {
        return;
    };
    let stage_path = std::env::var("ORBIT_FIFO_READER_STAGE_PATH")
        .expect("controlled FIFO reader needs a stage path");

    let holder = read_file_lock_holder_after_resolve(lock_path.as_ref(), |checked_path| {
        std::fs::remove_file(checked_path).expect("remove checked holder");
        let status = std::process::Command::new("mkfifo")
            .arg(checked_path)
            .status()
            .expect("start mkfifo");
        assert!(status.success(), "mkfifo must succeed: {status}");
        // Publish readiness by rename: `fs::write` truncates the stage file
        // before it writes, so the parent could poll and read it empty.
        let staged_path = format!("{stage_path}.tmp");
        std::fs::write(&staged_path, "fifo-ready").expect("stage FIFO setup completion");
        std::fs::rename(&staged_path, &stage_path).expect("report FIFO setup completion");
    });

    if std::env::var_os("ORBIT_FIFO_READER_UNSAFE_BLOCKING_OPEN").is_some() {
        let _ = std::fs::File::open(&lock_path).expect("unsafe FIFO open");
    }

    assert!(holder.is_none(), "a swapped FIFO is not holder metadata");
}
