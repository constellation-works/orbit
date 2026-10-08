//! Session-log persistence through public workspace composition. This separate
//! area binary covers the file store rather than registry or SQLite admission.
//! Mutable fixtures re-execute in isolated children with bounded waits.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use std::fs;
use std::path::Path;
use std::process::Command;

use orbit_common::{process, test_env};
use orbit_store::compose::workspace_session_log_store;
use orbit_store::contracts::{
    SessionLogAppendParams, SessionLogEntry, SessionLogFilter, SessionLogKind,
};
use tempfile::TempDir;

fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_SESSION_LOG_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output =
        process::run_bounded_capped(&mut command, test_env::CHILD_TEST_DEADLINE, 256 * 1024)
            .expect("run isolated session-log fixture");
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}

fn params(body: &str) -> SessionLogAppendParams {
    SessionLogAppendParams {
        kind: SessionLogKind::CheckLater,
        body: body.to_string(),
        related_task_ids: Vec::new(),
        related_run_ids: Vec::new(),
    }
}

fn encoded(entry: &SessionLogEntry) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(entry).unwrap();
    bytes.push(b'\n');
    bytes
}

#[cfg(unix)]
#[test]
fn symlinked_log_refuses_operations_without_touching_outside_target() {
    if !isolated("symlinked_log_refuses_operations_without_touching_outside_target") {
        return;
    }
    let root = TempDir::new().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let path = workspace.join("session-log.jsonl");
    let outside = root.path().join("outside.jsonl");
    let store = workspace_session_log_store(workspace);
    std::os::unix::fs::symlink(&outside, &path).unwrap();

    // A torn sentinel catches the original truncation; a valid record catches
    // readers that follow the link but merely suppress repair. Also refuse a
    // dangling link instead of treating it as an absent log.
    let sentinel = b"unrelated outside file without a newline".to_vec();
    let valid = SessionLogEntry {
        id: "SL-0001".to_string(),
        at: chrono::Utc::now(),
        kind: SessionLogKind::CheckLater,
        body: "outside check".to_string(),
        related_task_ids: Vec::new(),
        related_run_ids: Vec::new(),
        resolved_at: None,
    };
    for contents in [
        None,
        Some(sentinel),
        Some(serde_json::to_vec(&valid).unwrap()),
    ] {
        if let Some(bytes) = &contents {
            fs::write(&outside, bytes).unwrap();
        }
        let errors = [
            store.list(SessionLogFilter::default()).unwrap_err(),
            store.append(params("new entry")).unwrap_err(),
            store.resolve(&valid.id).unwrap_err(),
        ];
        for error in errors {
            assert!(
                error.to_string().contains(&path.display().to_string()),
                "{error}"
            );
        }
        match contents {
            Some(bytes) => assert_eq!(fs::read(&outside).unwrap(), bytes),
            None => assert!(!outside.exists(), "a dangling target must not be created"),
        }
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn listing_preserves_tail_bytes_until_append_repairs_under_lock() {
    if !isolated("listing_preserves_tail_bytes_until_append_repairs_under_lock") {
        return;
    }
    for tail in [
        b" \t".as_slice(),
        b"\xe2\x82",
        b"{",
        b"{\"kind\":\"future_kind\"}",
        b"",
    ] {
        let root = TempDir::new().unwrap();
        let store = workspace_session_log_store(root.path().to_path_buf());
        let first = store.append(params("first")).unwrap();
        let path = root.path().join("session-log.jsonl");
        let prefix = encoded(&first);
        let mut raw = prefix.clone();
        raw.extend_from_slice(tail);
        // An otherwise valid final record also needs its missing newline left
        // untouched by list and restored before the next append.
        if tail.is_empty() {
            raw.pop();
        }
        fs::write(&path, &raw).unwrap();

        assert_eq!(
            store.list(SessionLogFilter::default()).unwrap(),
            vec![first]
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            raw,
            "list must never repair the log"
        );

        let second = store.append(params("second")).unwrap();
        let mut expected = prefix;
        expected.extend(encoded(&second));
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(store.list(SessionLogFilter::default()).unwrap().len(), 2);
        assert_eq!(second.id, "SL-0002");
    }
}

#[test]
fn resolution_replaces_recovered_records_without_repairing_failed_requests() {
    if !isolated("resolution_replaces_recovered_records_without_repairing_failed_requests") {
        return;
    }
    let root = TempDir::new().unwrap();
    let store = workspace_session_log_store(root.path().to_path_buf());
    let first = store.append(params("check this")).unwrap();
    let path = root.path().join("session-log.jsonl");
    let mut raw = encoded(&first);
    raw.extend_from_slice(b"\xe2\x82");
    fs::write(&path, &raw).unwrap();

    store.resolve("SL-9999").unwrap_err();
    assert_eq!(fs::read(&path).unwrap(), raw);
    let resolved = store.resolve(&first.id).unwrap();
    assert!(resolved.resolved_at.is_some());
    assert_eq!(fs::read(&path).unwrap(), encoded(&resolved));
    assert!(
        store
            .list(SessionLogFilter {
                unresolved_only: true,
                ..SessionLogFilter::default()
            })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn terminated_corruption_fails_without_modifying_the_log() {
    if !isolated("terminated_corruption_fails_without_modifying_the_log") {
        return;
    }
    let root = TempDir::new().unwrap();
    let store = workspace_session_log_store(root.path().to_path_buf());
    let first = store.append(params("first")).unwrap();
    let path = root.path().join("session-log.jsonl");
    let mut raw = encoded(&first);
    raw.extend_from_slice(b"{\n");
    fs::write(&path, &raw).unwrap();
    store.list(SessionLogFilter::default()).unwrap_err();
    store.append(params("second")).unwrap_err();
    store.resolve(&first.id).unwrap_err();
    assert_eq!(fs::read(&path).unwrap(), raw);
}

#[test]
fn missing_logs_stay_missing_and_nonregular_logs_are_refused() {
    if !isolated("missing_logs_stay_missing_and_nonregular_logs_are_refused") {
        return;
    }
    let root = TempDir::new().unwrap();
    let path = root.path().join("session-log.jsonl");
    let store = workspace_session_log_store(root.path().to_path_buf());
    assert!(store.list(SessionLogFilter::default()).unwrap().is_empty());
    assert!(!path.exists());
    fs::create_dir(&path).unwrap();
    assert_path_error(&path, store.list(SessionLogFilter::default()).unwrap_err());
}

fn assert_path_error(path: &Path, error: orbit_common::OrbitError) {
    assert!(
        error.to_string().contains(&path.display().to_string()),
        "{error}"
    );
}
