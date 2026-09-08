//! Cursor-file load, lock, and atomic replace tests.

use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use orbit_types::workflow::AutoTaskCursor;
use tempfile::tempdir;

use super::{
    cursor_lock_path, cursor_state_path, inject_cursor_save_failures, load_cursor_state,
    upsert_cursor, with_cursor_lock,
};

fn cursor(baseline: &str, last_slot: Option<&str>) -> AutoTaskCursor {
    AutoTaskCursor {
        baseline_at: baseline.to_string(),
        last_slot: last_slot.map(str::to_string),
        last_fired_at: last_slot.map(|_| "2026-01-01T01:00:05+00:00".to_string()),
        last_task_id: last_slot.map(|_| "ORB-1".to_string()),
        pending: None,
    }
}

#[test]
fn missing_state_is_empty_and_first_upsert_baselines() {
    let root = tempdir().expect("tempdir");
    let path = cursor_state_path(root.path());

    let loaded = load_cursor_state(&path).expect("missing is empty");
    assert!(loaded.definitions.is_empty());

    upsert_cursor(&path, "chore", cursor("2026-01-01T00:00:00+00:00", None)).expect("baseline");
    let loaded = load_cursor_state(&path).expect("load");
    assert_eq!(
        loaded.definitions["chore"].baseline_at,
        "2026-01-01T00:00:00+00:00"
    );
    assert!(path.is_file());
    assert!(cursor_lock_path(&path).is_file());
}

#[test]
fn malformed_existing_state_errors_and_is_left_unchanged() {
    let root = tempdir().expect("tempdir");
    let path = cursor_state_path(root.path());
    fs::write(&path, "{not json").expect("corrupt");

    let error = load_cursor_state(&path).expect_err("malformed");
    assert!(
        error
            .to_string()
            .contains("malformed auto-task cursor state"),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("left unchanged for investigation"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&path).expect("raw"), "{not json");

    let error = upsert_cursor(&path, "chore", cursor("2026-01-01T00:00:00+00:00", None))
        .expect_err("upsert must not rewrite corrupt state");
    assert!(error.to_string().contains("malformed"), "{error}");
    assert_eq!(fs::read_to_string(&path).expect("raw"), "{not json");
}

#[cfg(unix)]
#[test]
fn unreadable_existing_state_errors_and_is_left_unchanged() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempdir().expect("tempdir");
    let path = cursor_state_path(root.path());
    fs::write(&path, r#"{"definitions":{}}"#).expect("write");
    let original = fs::metadata(&path).expect("meta").permissions();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod");

    let error = load_cursor_state(&path).expect_err("unreadable");
    fs::set_permissions(&path, original.clone()).expect("restore");
    assert!(
        error
            .to_string()
            .contains("unreadable auto-task cursor state"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("raw"),
        r#"{"definitions":{}}"#
    );

    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod");
    let error = upsert_cursor(&path, "chore", cursor("2026-01-01T00:00:00+00:00", None))
        .expect_err("upsert must not rewrite unreadable state");
    fs::set_permissions(&path, original).expect("restore");
    assert!(error.to_string().contains("unreadable"), "{error}");
    assert_eq!(
        fs::read_to_string(&path).expect("raw"),
        r#"{"definitions":{}}"#
    );
}

#[test]
fn empty_existing_file_is_malformed_not_a_baseline() {
    let root = tempdir().expect("tempdir");
    let path = cursor_state_path(root.path());
    fs::write(&path, "").expect("empty");

    let error = load_cursor_state(&path).expect_err("empty existing file");
    assert!(error.to_string().contains("malformed"), "{error}");
    assert_eq!(fs::read_to_string(&path).expect("raw"), "");
}

#[test]
fn concurrent_upserts_for_different_definitions_preserve_both_cursors() {
    let root = tempdir().expect("tempdir");
    let path = Arc::new(cursor_state_path(root.path()));
    let barrier = Arc::new(Barrier::new(2));

    thread::scope(|scope| {
        for name in ["alpha", "beta"] {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                upsert_cursor(
                    &path,
                    name,
                    cursor(
                        "2026-01-01T00:00:00+00:00",
                        Some("2026-01-01T01:00:00+00:00"),
                    ),
                )
                .expect("upsert");
            });
        }
    });

    let loaded = load_cursor_state(&path).expect("load");
    assert!(loaded.definitions.contains_key("alpha"), "{loaded:?}");
    assert!(loaded.definitions.contains_key("beta"), "{loaded:?}");
}

#[test]
fn atomic_replace_keeps_sidecar_lock_and_never_exposes_truncated_json() {
    let root = tempdir().expect("tempdir");
    let path = Arc::new(cursor_state_path(root.path()));
    upsert_cursor(&path, "seed", cursor("2026-01-01T00:00:00+00:00", None)).expect("seed");
    let lock_path = cursor_lock_path(&path);
    let lock_ino = fs::metadata(&lock_path).expect("lock").ino_or_zero();

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_error = Arc::new(std::sync::Mutex::new(None));
    thread::scope(|scope| {
        let path_reader = Arc::clone(&path);
        let stop_reader = Arc::clone(&stop);
        let reader_error = Arc::clone(&reader_error);
        scope.spawn(move || {
            while !stop_reader.load(std::sync::atomic::Ordering::Relaxed) {
                match load_cursor_state(&path_reader) {
                    Ok(_) => {}
                    Err(error) => {
                        *reader_error.lock().expect("lock") = Some(error.to_string());
                        break;
                    }
                }
            }
        });

        for index in 0..40 {
            upsert_cursor(
                &path,
                "seed",
                cursor(
                    "2026-01-01T00:00:00+00:00",
                    Some(&format!("2026-01-01T01:{index:02}:00+00:00")),
                ),
            )
            .expect("upsert");
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    assert!(
        reader_error.lock().expect("lock").is_none(),
        "reader observed invalid JSON: {:?}",
        reader_error.lock().expect("lock")
    );
    assert_eq!(
        fs::metadata(&lock_path).expect("lock").ino_or_zero(),
        lock_ino,
        "sidecar lock identity must survive data-file replacement"
    );
}

#[test]
fn sidecar_lock_excludes_a_second_writer() {
    let root = tempdir().expect("tempdir");
    let path = Arc::new(cursor_state_path(root.path()));
    upsert_cursor(&path, "seed", cursor("2026-01-01T00:00:00+00:00", None)).expect("seed");

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder_path = Arc::clone(&path);
    let holder = thread::spawn(move || {
        with_cursor_lock(&holder_path, |_session| {
            entered_tx.send(()).expect("entered");
            release_rx.recv().expect("release");
            Ok(())
        })
        .expect("hold lock");
    });
    entered_rx.recv().expect("holder entered");

    let blocked_path = Arc::clone(&path);
    let blocked = thread::spawn(move || {
        upsert_cursor(
            &blocked_path,
            "other",
            cursor("2026-01-01T00:00:00+00:00", None),
        )
    });
    thread::sleep(Duration::from_millis(50));
    assert!(
        !blocked.is_finished(),
        "second writer must wait on the sidecar lock"
    );
    release_tx.send(()).expect("release holder");
    blocked.join().expect("join").expect("upsert after release");
    holder.join().expect("holder");

    let loaded = load_cursor_state(&path).expect("load");
    assert!(loaded.definitions.contains_key("seed"));
    assert!(loaded.definitions.contains_key("other"));
}

#[test]
fn injected_save_failure_does_not_consume_or_truncate_state() {
    let root = tempdir().expect("tempdir");
    let path = cursor_state_path(root.path());
    upsert_cursor(&path, "seed", cursor("2026-01-01T00:00:00+00:00", None)).expect("seed");
    let before = fs::read_to_string(&path).expect("before");

    inject_cursor_save_failures(1);
    let error = upsert_cursor(
        &path,
        "other",
        cursor(
            "2026-01-01T00:00:00+00:00",
            Some("2026-01-01T01:00:00+00:00"),
        ),
    )
    .expect_err("injected save");
    inject_cursor_save_failures(0);
    assert!(error.to_string().contains("injected"), "{error}");
    assert_eq!(fs::read_to_string(&path).expect("after"), before);
}

trait InoOrZero {
    fn ino_or_zero(&self) -> u64;
}

impl InoOrZero for fs::Metadata {
    fn ino_or_zero(&self) -> u64 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            self.ino()
        }
        #[cfg(not(unix))]
        {
            0
        }
    }
}
