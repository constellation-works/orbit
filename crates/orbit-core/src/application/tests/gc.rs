use orbit_common::OrbitError;

use crate::OrbitRuntime;

#[test]
fn gc_worktrees_rejects_overflow_and_out_of_range_hours_without_unwinding() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");

    // i64::MAX causes Duration::hours to panic in chrono without checked conversion.
    let err = runtime
        .gc_worktrees(false, None, Some(i64::MAX as u64), false)
        .expect_err("i64::MAX must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("--older-than-hours is too large")),
        "expected InvalidInput for i64::MAX, got: {err:?}"
    );

    // u64::MAX exceeds i64 range.
    let err = runtime
        .gc_worktrees(false, None, Some(u64::MAX), false)
        .expect_err("u64::MAX must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("--older-than-hours is too large")),
        "expected InvalidInput for u64::MAX, got: {err:?}"
    );

    // Out-of-range value where Duration::try_hours returns None (overflows seconds in chrono::Duration)
    let err = runtime
        .gc_worktrees(false, None, Some(3_000_000_000), false)
        .expect_err("3B hours must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("--older-than-hours is too large")),
        "expected InvalidInput for 3B hours, got: {err:?}"
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
fn gc_worktrees_accepts_zero_ordinary_ages_and_none() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    init_git_repo(&runtime.paths().repo_root);

    // Zero hours
    let res = runtime.gc_worktrees(false, None, Some(0), false);
    assert!(res.is_ok(), "zero hours must succeed, got: {res:?}");

    // Ordinary ages
    let res = runtime.gc_worktrees(false, None, Some(1), false);
    assert!(res.is_ok(), "1 hour must succeed, got: {res:?}");

    let res = runtime.gc_worktrees(false, None, Some(24), false);
    assert!(res.is_ok(), "24 hours must succeed, got: {res:?}");

    let res = runtime.gc_worktrees(false, None, Some(168), false);
    assert!(res.is_ok(), "168 hours must succeed, got: {res:?}");

    // None (no age restriction)
    let res = runtime.gc_worktrees(false, None, None, false);
    assert!(res.is_ok(), "None must succeed, got: {res:?}");
}

#[test]
fn gc_worktrees_attempts_no_deletion_for_rejected_values() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");

    // Create a mock worktree directory that would otherwise be eligible for deletion if GC proceeded
    let worktree_dir = runtime
        .paths()
        .repo_root
        .join(".orbit")
        .join("state")
        .join("worktrees")
        .join("candidate-worktree");
    std::fs::create_dir_all(&worktree_dir).expect("create candidate worktree dir");
    assert!(worktree_dir.exists());

    // When delete: true is requested with an overflowing older_than_hours, validation must fail
    // before any collection or deletion is attempted.
    let err = runtime
        .gc_worktrees(true, None, Some(i64::MAX as u64), false)
        .expect_err("i64::MAX must return InvalidInput even with delete: true");
    assert!(
        matches!(err, OrbitError::InvalidInput(_)),
        "expected InvalidInput, got: {err:?}"
    );

    // Verify the directory is untouched
    assert!(
        worktree_dir.exists(),
        "worktree directory must not be deleted when input is rejected"
    );

    // Also check for u64::MAX
    let err = runtime
        .gc_worktrees(true, None, Some(u64::MAX), false)
        .expect_err("u64::MAX must return InvalidInput even with delete: true");
    assert!(
        matches!(err, OrbitError::InvalidInput(_)),
        "expected InvalidInput, got: {err:?}"
    );
    assert!(
        worktree_dir.exists(),
        "worktree directory must not be deleted when input is rejected"
    );
}
