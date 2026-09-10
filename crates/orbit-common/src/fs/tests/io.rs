use std::fs::File;
use std::io;

use tempfile::TempDir;

use crate::OrbitError;
use crate::fs::io::{
    read_file_lock_holder, remove_path_if_exists, sync_parent_dir, with_exclusive_file_lock,
};

#[test]
fn sync_parent_dir_uses_a_preopened_directory_handle() {
    let temp = TempDir::new().expect("tempdir");
    let file = temp.path().join("file");
    std::fs::write(&file, b"payload").expect("write file");
    let parent = File::open(temp.path()).expect("open parent directory");

    sync_parent_dir(&parent).expect("sync parent directory");
}

fn assert_sandbox_write_message(message: &str, path: &str) {
    assert!(
        message.contains(path),
        "expected path `{path}` in `{message}`"
    );
    assert!(
        message.contains("is not writable"),
        "expected writable attribution in `{message}`"
    );
    assert!(
        message.contains("sandbox or environment"),
        "expected sandbox/environment hint in `{message}`"
    );
    assert!(
        message.contains("not an Orbit store defect"),
        "expected store-defect negation in `{message}`"
    );
}

#[test]
fn exclusive_lock_runs_op_and_releases() {
    let temp = TempDir::new().expect("tempdir");
    let target = temp.path().join("task.yaml");
    let ran = with_exclusive_file_lock(&target, "task artifact v2", || {
        assert!(temp.path().join(".task.yaml.lock").exists());
        Ok::<_, io::Error>(true)
    })
    .expect("lock should succeed");
    assert!(ran);
}

#[test]
fn exclusive_lock_non_access_failure_stays_labeled() {
    let temp = TempDir::new().expect("tempdir");
    let blocker = temp.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let target = blocker.join("task.yaml");
    let err = with_exclusive_file_lock::<(), io::Error, _>(&target, "task artifact v2", || Ok(()))
        .expect_err("parent file must fail lock acquisition");
    let message = err.to_string();
    assert!(
        !message.contains("sandbox or environment"),
        "non-access failures must stay unlabeled: {message}"
    );
}

#[cfg(unix)]
#[test]
fn exclusive_lock_open_on_readonly_dir_names_path_and_hints_sandbox() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let dir = temp.path().join("bundle");
    std::fs::create_dir(&dir).expect("mkdir");
    let target = dir.join("task.yaml");
    let lock_path = dir.join(".task.yaml.lock");

    let mut perms = std::fs::metadata(&dir).expect("meta").permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&dir, perms).expect("chmod -w");

    struct Restore<'a>(&'a std::path::Path);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let _restore = Restore(&dir);

    let err = with_exclusive_file_lock::<(), OrbitError, _>(&target, "task artifact v2", || Ok(()))
        .expect_err("lock open must fail on a read-only directory");
    match err {
        OrbitError::Io(message) => {
            assert_sandbox_write_message(&message, &lock_path.display().to_string());
            assert!(
                !message.contains("open task artifact v2 lock"),
                "classified access errors must not use the bare lock-open wrap: {message}"
            );
        }
        other => panic!("expected Io, got {other}"),
    }
}

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

#[test]
fn read_file_lock_holder_parses_a_regular_holder_file() {
    let temp = TempDir::new().expect("tempdir");
    let lock_path = temp.path().join(".task.yaml.lock");
    std::fs::write(
        &lock_path,
        br#"{"pid":7,"acquired_at":"2026-01-01T00:00:00Z","label":"holder"}"#,
    )
    .expect("write holder file");

    let holder = read_file_lock_holder(&lock_path).expect("holder metadata");
    assert_eq!(holder.pid, 7);
    assert_eq!(holder.label, "holder");
}

/// Platform-neutral coverage for the supported non-Unix path: without
/// `O_NOFOLLOW`, the reject-a-non-regular-final-component behavior still
/// comes from the pathname pre-check and the post-open `fstat`, so a
/// directory at the lock path is refused everywhere, not just on Unix.
#[test]
fn read_file_lock_holder_returns_none_for_a_directory_lock_path() {
    let temp = TempDir::new().expect("tempdir");
    let lock_path = temp.path().join("dir.lock");
    std::fs::create_dir(&lock_path).expect("create directory");

    assert!(
        read_file_lock_holder(&lock_path).is_none(),
        "a directory must never be read as holder metadata"
    );
}

#[test]
fn read_file_lock_holder_returns_none_for_malformed_json() {
    let temp = TempDir::new().expect("tempdir");
    let lock_path = temp.path().join(".task.yaml.lock");
    std::fs::write(&lock_path, b"not json").expect("write malformed holder");

    assert!(read_file_lock_holder(&lock_path).is_none());
}

#[test]
fn read_file_lock_holder_returns_none_for_a_missing_lock_file() {
    let temp = TempDir::new().expect("tempdir");
    let lock_path = temp.path().join(".missing.lock");

    assert!(read_file_lock_holder(&lock_path).is_none());
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
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use crate::fs::file_lock::read_file_lock_holder_after_resolve;

    let root = tempfile::tempdir().expect("root tempdir");
    let lock_path = root.path().join(".task.yaml.lock");
    std::fs::write(&lock_path, br#"{"pid":1}"#).expect("write checked holder");
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let holder = read_file_lock_holder_after_resolve(&lock_path, |checked_path| {
            std::fs::remove_file(checked_path).expect("remove checked holder");
            let status = std::process::Command::new("mkfifo")
                .arg(checked_path)
                .status()
                .expect("create FIFO");
            assert!(status.success(), "mkfifo must succeed: {status}");
        });
        sender.send(holder).expect("report holder result");
    });

    let holder = receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("FIFO swap must not block the holder reader");
    assert!(holder.is_none(), "a swapped FIFO is not holder metadata");
}

#[cfg(unix)]
#[test]
fn read_file_lock_holder_rejects_a_symlinked_lock_file() {
    let root = tempfile::tempdir().expect("root tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let outside_file = outside.path().join("holder.json");
    std::fs::write(
        &outside_file,
        br#"{"pid":1,"acquired_at":"2026-01-01T00:00:00Z","label":"planted"}"#,
    )
    .expect("outside holder file");
    let link = root.path().join(".task.yaml.lock");
    std::os::unix::fs::symlink(&outside_file, &link).expect("symlink");

    assert!(
        read_file_lock_holder(&link).is_none(),
        "a symlinked lock path must not be read as holder metadata"
    );
}

#[cfg(unix)]
#[test]
fn read_file_lock_holder_reads_through_a_symlinked_parent_route() {
    let root = tempfile::tempdir().expect("root tempdir");
    let real_dir = root.path().join("real");
    std::fs::create_dir(&real_dir).expect("real directory");
    let linked_dir = root.path().join("linked");
    std::os::unix::fs::symlink(&real_dir, &linked_dir).expect("parent symlink");

    let lock_path = real_dir.join(".task.yaml.lock");
    std::fs::write(
        &lock_path,
        br#"{"pid":1,"acquired_at":"2026-01-01T00:00:00Z","label":"holder"}"#,
    )
    .expect("holder file");

    let holder = read_file_lock_holder(&linked_dir.join(".task.yaml.lock"))
        .expect("holder metadata through a symlinked parent");
    assert_eq!(holder.pid, 1);
    assert_eq!(holder.label, "holder");
}

#[cfg(unix)]
#[test]
fn private_append_preserves_a_symlinked_parent_route() {
    let root = tempfile::tempdir().expect("root tempdir");
    let real_dir = root.path().join("real");
    std::fs::create_dir(&real_dir).expect("real directory");
    let linked_dir = root.path().join("linked");
    std::os::unix::fs::symlink(&real_dir, &linked_dir).expect("parent symlink");

    let path = linked_dir.join("log");
    let file = super::super::io::append_private_file(&path).expect("append through parent link");
    drop(file);

    assert!(real_dir.join("log").is_file());
    assert!(!path.is_symlink(), "the final file must be a regular file");
}

#[cfg(unix)]
#[test]
fn remove_path_if_exists_unlinks_a_dangling_symlink() {
    let temp = TempDir::new().expect("tempdir");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(temp.path().join("missing"), &link).expect("symlink");

    remove_path_if_exists(&link).expect("remove dangling symlink");

    let error = std::fs::symlink_metadata(&link).expect_err("link must be removed");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
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

/// Re-entry must not leak the held-path bookkeeping: once the outer call
/// returns — even by unwinding — the next acquisition has to lock for real, or
/// a later caller would silently run unlocked.
#[test]
fn exclusive_lock_releases_reentrancy_bookkeeping_after_a_panic() {
    let temp = TempDir::new().expect("tempdir");
    let target = temp.path().join("bundle").join("task.yaml");

    let panicked = std::panic::catch_unwind(|| {
        let _ = with_exclusive_file_lock::<(), io::Error, _>(&target, "outer", || {
            panic!("body blew up while holding the lock");
        });
    });
    assert!(panicked.is_err(), "the panic must propagate");

    // A different thread can only take the lock if the first release actually
    // happened, and this thread can only re-lock if its held set was cleared.
    let taken = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                with_exclusive_file_lock::<bool, io::Error, _>(&target, "other thread", || Ok(true))
            })
            .join()
            .expect("thread joined")
    })
    .expect("lock after panic");
    assert!(taken);
    with_exclusive_file_lock::<(), io::Error, _>(&target, "same thread again", || Ok(()))
        .expect("re-lock on the original thread");
}

/// Re-entrancy has to survive reaching the same lock file by a second route.
/// Orbit's checkout projection links a workspace path at the canonical task
/// bundle, so a nested lock arriving via the link must recognize the outer
/// lock taken via the canonical path rather than deadlock on it.
#[cfg(unix)]
#[test]
fn exclusive_lock_is_reentrant_across_a_symlinked_route() {
    let temp = TempDir::new().expect("tempdir");
    let canonical = temp.path().join("canonical");
    std::fs::create_dir(&canonical).expect("mkdir");
    let linked = temp.path().join("projection");
    std::os::unix::fs::symlink(&canonical, &linked).expect("symlink");

    let reached = with_exclusive_file_lock::<bool, io::Error, _>(
        &canonical.join("task.yaml"),
        "canonical route",
        || {
            with_exclusive_file_lock::<bool, io::Error, _>(
                &linked.join("task.yaml"),
                "projected route",
                || Ok(true),
            )
        },
    )
    .expect("the projected route must re-enter the canonical lock");
    assert!(reached);
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

/// A byte write that cannot be committed must not leave its staging file
/// behind: temp names are never reused, so a leftover is never reclaimed.
#[test]
fn atomic_write_bytes_removes_its_temp_file_when_the_rename_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A non-empty directory at the target path makes the final rename fail
    // after the staging file has been fully written.
    let target = dir.path().join("target");
    std::fs::create_dir_all(target.join("occupied")).expect("occupy target");

    let error =
        super::super::io::atomic_write_bytes(&target, b"payload").expect_err("rename fails");
    assert!(target.is_dir(), "{error}");

    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "target")
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging files left behind: {leftovers:?}"
    );
}

#[test]
fn atomic_write_rejects_a_dot_component_as_the_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = dir.path().join("parent");
    std::fs::create_dir(&parent).expect("parent directory");

    let error = super::super::io::atomic_write_text(&parent.join(".."), "payload")
        .expect_err("atomic write must not replace a directory");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(parent.is_dir(), "the parent directory must remain intact");
}

#[cfg(unix)]
#[test]
fn atomic_write_preserves_a_symlinked_parent_route() {
    let root = tempfile::tempdir().expect("root tempdir");
    let real_dir = root.path().join("real");
    std::fs::create_dir(&real_dir).expect("real directory");
    let linked_dir = root.path().join("linked");
    std::os::unix::fs::symlink(&real_dir, &linked_dir).expect("parent symlink");

    let path = linked_dir.join("config");
    super::super::io::atomic_write_text(&path, "payload").expect("write through parent link");

    assert_eq!(
        std::fs::read_to_string(real_dir.join("config")).expect("read config"),
        "payload"
    );
    assert!(!path.is_symlink(), "the final file must be a regular file");
}

#[test]
fn staged_write_removes_partial_temp_file_when_writing_fails() {
    use std::io::Write;

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target");

    let error = match super::super::io::StagedTextFile::stage_with_for_test(&target, |file| {
        file.write_all(b"partial payload")?;
        Err(io::Error::other("injected write failure"))
    }) {
        Ok(_) => panic!("injected write must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(!target.exists(), "partial data was published at final path");

    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging files left behind: {leftovers:?}"
    );

    super::super::io::atomic_write_private_bytes(&target, b"complete payload")
        .expect("retry write");
    assert_eq!(
        std::fs::read(target).expect("read retry"),
        b"complete payload"
    );
}
