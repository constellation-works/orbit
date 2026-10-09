//! Public filesystem boundary for mkdir/mtime locking and legacy migration.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

use std::fs::{self, File, FileTimes};
use std::io;
use std::time::{Duration, Instant, SystemTime};

use fs2::FileExt;
use orbit_common::fs::directory_lock::{DirectoryLockOptions, with_directory_lock_options};

fn options() -> DirectoryLockOptions {
    DirectoryLockOptions {
        timeout: Duration::from_millis(200),
        stale_after: Duration::from_secs(10),
        update_interval: Duration::from_millis(100),
    }
}

fn age(path: &std::path::Path) {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(0x0080 | 0x0100)
            .custom_flags(0x0200_0000);
    }
    options
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(60)))
        .unwrap();
}

#[test]
fn stale_locks_are_reclaimed_but_active_writers_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("state.json");
    let lock = temp.path().join("state.json.lock");
    for directory in [true, false] {
        if directory {
            fs::create_dir(&lock).unwrap();
        } else {
            fs::write(&lock, "").unwrap();
        }
        age(&lock);
        let active = if !directory {
            let active = File::open(&lock).unwrap();
            FileExt::lock_exclusive(&active).unwrap();
            let result: io::Result<()> =
                with_directory_lock_options(&target, "active legacy", options(), || {
                    panic!("an active legacy writer must not be unlinked");
                });
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
            assert!(lock.is_file());
            Some(active)
        } else {
            None
        };
        drop(active);
        with_directory_lock_options(&target, "stale", options(), || {
            assert!(lock.is_dir(), "the replacement lock must use mkdir");
            Ok::<_, io::Error>(())
        })
        .unwrap();
        assert!(!lock.exists());
        fs::create_dir(&lock).expect("a mkdir peer can lock after release");
        fs::remove_dir(&lock).unwrap();
    }

    fs::create_dir(&lock).unwrap();
    let result: io::Result<()> = with_directory_lock_options(&target, "fresh", options(), || {
        panic!("a fresh peer directory must exclude Orbit");
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(lock.is_dir(), "timeout must preserve the peer's directory");
}

#[test]
fn held_directory_is_refreshed_and_removed_on_error_or_unwind() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("state.json");
    let lock = temp.path().join("state.json.lock");
    let result: io::Result<()> =
        with_directory_lock_options(&target, "heartbeat", options(), || {
            let initial = fs::metadata(&lock)?.modified()?;
            let deadline = Instant::now() + Duration::from_secs(3);
            while fs::metadata(&lock)?.modified()? == initial {
                assert!(
                    Instant::now() < deadline,
                    "held directory mtime must be refreshed"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(io::Error::other("operation failed"))
        });
    assert!(result.is_err());
    assert!(
        !lock.exists(),
        "operation errors must release the directory"
    );

    assert!(
        std::panic::catch_unwind(|| {
            let _: io::Result<()> =
                with_directory_lock_options(&target, "unwind", options(), || {
                    panic!("operation unwound");
                });
        })
        .is_err()
    );
    assert!(!lock.exists(), "unwinding must release the directory");
}

#[test]
fn release_preserves_a_replacement_lock() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("state.json");
    let lock = temp.path().join("state.json.lock");
    let retired = temp.path().join("retired");
    let result: io::Result<()> =
        with_directory_lock_options(&target, "replaced", options(), || {
            fs::rename(&lock, &retired)?;
            fs::create_dir(&lock)?;
            Ok(())
        });
    assert!(
        result.is_err(),
        "a replaced lock must be reported as compromised"
    );
    assert!(
        lock.is_dir(),
        "release must leave the new owner's lock intact"
    );
}

#[cfg(unix)]
#[test]
fn symlink_lock_does_not_redirect_legacy_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("state.json");
    let lock = temp.path().join("state.json.lock");
    let victim = temp.path().join("victim");
    fs::write(&victim, "preserve").unwrap();
    age(&victim);
    std::os::unix::fs::symlink(&victim, &lock).unwrap();
    let result: io::Result<()> = with_directory_lock_options(&target, "symlink", options(), || {
        panic!("symlinks must be rejected");
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert_eq!(fs::read_to_string(victim).unwrap(), "preserve");
    assert!(fs::symlink_metadata(lock).unwrap().file_type().is_symlink());
}
