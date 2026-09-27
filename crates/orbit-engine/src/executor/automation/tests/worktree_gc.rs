#![allow(missing_docs)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use orbit_common::OrbitError;
use orbit_types::workflow::{EngineDeterministicAction, JobRun};
use serde_json::json;
use tempfile::tempdir;

use super::super::{execute_action, execute_engine_action};
use crate::context::RuntimeHost;

struct MockGcHost {
    repo_root: PathBuf,
    calls_list_job_runs: AtomicBool,
    calls_repo_root: AtomicBool,
}

impl MockGcHost {
    fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            calls_list_job_runs: AtomicBool::new(false),
            calls_repo_root: AtomicBool::new(false),
        }
    }
}

impl RuntimeHost for MockGcHost {
    fn repo_root(&self) -> Result<String, OrbitError> {
        self.calls_repo_root.store(true, Ordering::SeqCst);
        Ok(self.repo_root.to_string_lossy().into_owned())
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.calls_list_job_runs.store(true, Ordering::SeqCst);
        Ok(Vec::new())
    }
}

#[test]
fn worktree_gc_rejects_overflow_and_out_of_range_without_unwinding() {
    let temp = tempdir().expect("tempdir");
    let host = MockGcHost::new(temp.path().to_path_buf());

    // i64::MAX overflows chrono::Duration seconds calculation in Duration::hours.
    let input = json!({ "older_than_hours": i64::MAX });
    let err = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &input,
        None,
    )
    .expect_err("i64::MAX must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("older_than_hours is too large")),
        "expected InvalidInput for i64::MAX, got: {err:?}"
    );
    assert!(!host.calls_list_job_runs.load(Ordering::SeqCst));
    assert!(!host.calls_repo_root.load(Ordering::SeqCst));

    // u64::MAX overflows i64.
    let input = json!({ "older_than_hours": u64::MAX });
    let err = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &input,
        None,
    )
    .expect_err("u64::MAX must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("older_than_hours is too large")),
        "expected InvalidInput for u64::MAX, got: {err:?}"
    );

    // Value where Duration::try_hours returns None (> 2.5B hours)
    let input = json!({ "older_than_hours": 3_000_000_000u64 });
    let err = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &input,
        None,
    )
    .expect_err("3B hours must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("older_than_hours is too large")),
        "expected InvalidInput for 3B hours, got: {err:?}"
    );

    // Negative hours
    let input = json!({ "older_than_hours": -1 });
    let err = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &input,
        None,
    )
    .expect_err("negative hours must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(_)),
        "expected InvalidInput for negative hours, got: {err:?}"
    );
}

#[test]
fn worktree_gc_rejects_via_execute_action_dispatch() {
    let temp = tempdir().expect("tempdir");
    let host = MockGcHost::new(temp.path().to_path_buf());
    let steps_outputs = HashMap::new();

    let input = json!({ "older_than_hours": i64::MAX });
    let err = execute_action(
        &host,
        "worktree_gc",
        &json!({}),
        &input,
        false,
        &steps_outputs,
        None,
    )
    .expect_err("i64::MAX must return InvalidInput via execute_action");

    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("older_than_hours is too large")),
        "expected InvalidInput via execute_action, got: {err:?}"
    );
}

fn init_git_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
    };
    run(&["init"]);
    run(&["checkout", "-b", "agent-main"]);
    run(&["config", "user.name", "Orbit Test"]);
    run(&["config", "user.email", "orbit-test@example.com"]);
    std::fs::write(path.join("base.txt"), "base").unwrap();
    run(&["add", "base.txt"]);
    run(&["commit", "-m", "base"]);
}

#[test]
fn worktree_gc_accepts_zero_ordinary_ages_and_omitted() {
    let temp = tempdir().expect("tempdir");
    init_git_repo(temp.path());
    let host = MockGcHost::new(temp.path().to_path_buf());

    // Zero hours
    let res = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &json!({ "older_than_hours": 0 }),
        None,
    );
    assert!(res.is_ok(), "zero hours must succeed, got: {res:?}");

    // Ordinary ages
    let res = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &json!({ "older_than_hours": 1 }),
        None,
    );
    assert!(res.is_ok(), "1 hour must succeed, got: {res:?}");

    let res = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &json!({ "older_than_hours": 24 }),
        None,
    );
    assert!(res.is_ok(), "24 hours must succeed, got: {res:?}");

    let res = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &json!({ "older_than_hours": 168 }),
        None,
    );
    assert!(res.is_ok(), "168 hours must succeed, got: {res:?}");

    // Omitted older_than_hours
    let res = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &json!({}),
        None,
    );
    assert!(res.is_ok(), "omitted hours must succeed, got: {res:?}");

    // Null older_than_hours
    let res = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &json!({ "older_than_hours": null }),
        None,
    );
    assert!(res.is_ok(), "null hours must succeed, got: {res:?}");
}

#[test]
fn worktree_gc_attempts_no_deletion_for_rejected_values() {
    let temp = tempdir().expect("tempdir");
    let canary_dir = temp
        .path()
        .join(".orbit")
        .join("state")
        .join("worktrees")
        .join("canary-worktree");
    std::fs::create_dir_all(&canary_dir).expect("create canary worktree dir");
    assert!(canary_dir.exists());

    let host = MockGcHost::new(temp.path().to_path_buf());

    let err = execute_engine_action(
        &host,
        EngineDeterministicAction::WorktreeGc,
        &json!({}),
        &json!({ "older_than_hours": i64::MAX }),
        None,
    )
    .expect_err("i64::MAX must return InvalidInput");

    assert!(matches!(err, OrbitError::InvalidInput(_)));
    // Neither listing runs nor repo root was accessed before rejecting
    assert!(!host.calls_list_job_runs.load(Ordering::SeqCst));
    assert!(!host.calls_repo_root.load(Ordering::SeqCst));
    // The canary worktree is untouched
    assert!(canary_dir.exists(), "canary directory must not be deleted");
}
