use std::io;

#[cfg(unix)]
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::fs::io::{remove_path_if_exists, with_exclusive_file_lock};

#[cfg(unix)]
#[test]
fn private_append_rejects_a_final_symlink() {
    let root = tempfile::tempdir().expect("root tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let outside_file = outside.path().join("outside.log");
    std::fs::write(&outside_file, b"unchanged").expect("outside file");
    let link = root.path().join("log");
    std::os::unix::fs::symlink(&outside_file, &link).expect("symlink");

    let error = super::super::io::append_private_file(&link)
        .expect_err("private append must reject a symlink target");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        std::fs::read(&outside_file).expect("outside file remains"),
        b"unchanged"
    );
}

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
    let test_binary = std::env::current_exe().expect("locate test binary");
    let mut command = std::process::Command::new(test_binary);
    command
        .arg("fs::tests::io::read_file_lock_holder_fifo_reader_child")
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
        std::fs::write(&stage_path, "fifo-ready").expect("report FIFO setup completion");
    });

    if std::env::var_os("ORBIT_FIFO_READER_UNSAFE_BLOCKING_OPEN").is_some() {
        let _ = std::fs::File::open(&lock_path).expect("unsafe FIFO open");
    }

    assert!(holder.is_none(), "a swapped FIFO is not holder metadata");
}

#[cfg(unix)]
#[test]
fn remove_path_if_exists_unlinks_a_directory_symlink_without_removing_its_target() {
    let temp = TempDir::new().expect("tempdir");
    let target = temp.path().join("target");
    std::fs::create_dir(&target).expect("target directory");
    let target_file = target.join("keep");
    std::fs::write(&target_file, b"preserved").expect("target file");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");

    remove_path_if_exists(&link).expect("remove directory symlink");

    let error = std::fs::symlink_metadata(&link).expect_err("link must be removed");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(
        std::fs::read(target_file).expect("target preserved"),
        b"preserved"
    );
}

/// ORB-10988: nesting the same lock path on one thread must re-enter, not
/// deadlock. `flock(2)` belongs to the open file description, so the inner
/// call's fresh descriptor would otherwise block on the outer call's lock
/// forever. The runtime relies on this to hold a task lock across a
/// read-modify-write whose inner store writes lock the same file.
#[test]
fn exclusive_lock_is_reentrant_within_a_thread() {
    let temp = TempDir::new().expect("tempdir");
    let target = temp.path().join("bundle").join("task.yaml");

    let depth = with_exclusive_file_lock::<usize, io::Error, _>(&target, "outer", || {
        with_exclusive_file_lock::<usize, io::Error, _>(&target, "inner", || {
            with_exclusive_file_lock::<usize, io::Error, _>(&target, "innermost", || Ok(3))
        })
    })
    .expect("nested locks must re-enter");

    assert_eq!(depth, 3);
}

/// The outer call is the one that creates the parent directory, so the key
/// re-entrancy is tracked under must not depend on whether the parent existed
/// when the call started. Reaching the target through a symlink is what makes
/// the resolved and literal paths differ, and that difference used to leave a
/// nested call unable to see the lock it already held — it opened a second
/// descriptor to the same file and blocked forever.
///
/// The work runs on its own thread so a regression fails on the timeout rather
/// than hanging the suite. macOS reproduces this without the explicit symlink,
/// because its temp directories already sit under one; the symlink here is what
/// makes the case reproduce on Linux too.
#[cfg(unix)]
#[test]
fn exclusive_lock_re_enters_when_the_outer_call_creates_the_parent_under_a_symlink() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp = TempDir::new().expect("tempdir");
    let real_root = temp.path().join("real");
    std::fs::create_dir_all(&real_root).expect("real root");
    let link_root = temp.path().join("link");
    std::os::unix::fs::symlink(&real_root, &link_root).expect("symlink to the real root");

    // `bundle` deliberately does not exist yet.
    let target = link_root.join("bundle").join("task.yaml");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let depth = with_exclusive_file_lock::<usize, io::Error, _>(&target, "outer", || {
            with_exclusive_file_lock::<usize, io::Error, _>(&target, "inner", || Ok(2))
        });
        let _ = tx.send(depth);
    });

    let depth = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("nested lock deadlocked instead of re-entering")
        .expect("nested locks must re-enter");
    assert_eq!(depth, 2);
}

#[cfg(unix)]
#[test]
fn private_open_repairs_a_permissive_existing_file() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("shared.lock");
    std::fs::write(&path, b"").expect("create lock file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("make the file permissive");

    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    let _file = crate::fs::io::open_private_file(&path, &mut options).expect("open private");

    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o600, "a permissive file is restricted to its owner");
}
