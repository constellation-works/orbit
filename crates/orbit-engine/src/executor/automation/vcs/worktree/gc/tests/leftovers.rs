#![allow(missing_docs)]
use super::super::super::tests::support::*;
use super::super::sweep::{WorktreeGcOptions, collect_worktrees};
#[cfg(unix)]
use orbit_types::task::TaskStatus;
#[cfg(unix)]
use orbit_types::workflow::JobRunState;
use std::fs;
#[cfg(unix)]
use std::process::Command;
use tempfile::tempdir;

#[cfg(unix)]
#[test]
fn gc_reclaims_the_leftover_of_a_removal_that_failed_partway() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-leftover", JobRunState::Success, &["ORB-LEFTOVER"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/leftover");
    let locked = leave_read_only_fixture(&repo, &worktree);
    let _unlock = Unlock(locked.clone());
    // Reproduce the failure directly: Git unregisters the worktree, then
    // cannot delete the read-only directory.
    let failed = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree)
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(worktree.exists() && !listed_worktrees(&repo).contains("leftover"));
    assert_eq!(
        fs::metadata(&locked).unwrap().permissions().mode() & 0o777,
        0o555
    );
    let unmapped = repo.join(".orbit/state/worktrees/orbit-jrun-unmapped");
    fs::create_dir_all(&unmapped).unwrap();
    let host = FakeTaskHost::new(vec![task_fixture("ORB-LEFTOVER", TaskStatus::Done)]);
    let options = |delete| WorktreeGcOptions {
        delete,
        estimate_bytes: true,
        ..Default::default()
    };

    let dry = collect_worktrees(&repo, std::slice::from_ref(&run), &host, &options(false)).unwrap();
    assert!(
        dry.reports
            .iter()
            .any(|report| report.path == worktree && report.action == "would_remove"),
        "{:?}",
        dry.reports
    );
    assert!(worktree.exists(), "a dry run must not delete");

    let result = collect_worktrees(&repo, &[run], &host, &options(true)).unwrap();

    let reclaimed = result
        .reports
        .iter()
        .find(|report| report.path == worktree)
        .unwrap();
    assert_eq!(reclaimed.action, "removed");
    assert!(reclaimed.bytes_reclaimed > 0);
    assert!(!worktree.exists());
    let untouched = result
        .reports
        .iter()
        .find(|report| report.path == unmapped)
        .unwrap();
    assert_eq!(untouched.action, "skipped:unrecognized");
    assert!(unmapped.exists());
}

#[cfg(unix)]
#[test]
fn gc_reclaims_trash_directory_when_deleter_pid_is_dead() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let dead_pid = reaped_child_pid();
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let trash_dir = repo
        .join(".orbit/state/worktrees")
        .join(format!(".trash-orbit-jrun-dead-{dead_pid}-{now_nanos}"));
    fs::create_dir_all(&trash_dir).unwrap();
    fs::write(trash_dir.join("leftover.txt"), "leftover payload").unwrap();

    let host = FakeTaskHost::new(Vec::new());

    // Dry run
    let dry = collect_worktrees(
        &repo,
        &[],
        &host,
        &WorktreeGcOptions {
            delete: false,
            estimate_bytes: true,
            ..Default::default()
        },
    )
    .unwrap();
    let would_remove = dry
        .reports
        .iter()
        .find(|report| report.path == trash_dir)
        .expect("trash report present in dry run");
    assert_eq!(would_remove.action, "would_remove");
    assert!(would_remove.bytes_reclaimed > 0);
    assert!(trash_dir.exists());

    // Live deletion
    let result = collect_worktrees(
        &repo,
        &[],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();
    let reclaimed = result
        .reports
        .iter()
        .find(|report| report.path == trash_dir)
        .expect("trash report present in live gc");
    assert_eq!(reclaimed.action, "removed");
    assert!(reclaimed.bytes_reclaimed > 0);
    assert_eq!(result.bytes_reclaimed, reclaimed.bytes_reclaimed);
    assert!(!trash_dir.exists());
}

#[test]
fn gc_reclaims_trash_directory_when_age_exceeds_timeout() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let live_pid = std::process::id();
    // A timestamp from UNIX_EPOCH (0 nanos) exceeds the 600,000ms deletion timeout.
    let expired_nanos = 0u128;
    let trash_dir = repo.join(".orbit/state/worktrees").join(format!(
        ".trash-orbit-jrun-expired-{live_pid}-{expired_nanos}"
    ));
    fs::create_dir_all(&trash_dir).unwrap();
    fs::write(trash_dir.join("timedout.txt"), "timed out leftover").unwrap();
    // A stale relative gitdir left after Git removed its administrative entry
    // is still reclaimable, unlike a relative link to a recoverable checkout.
    fs::write(
        trash_dir.join(".git"),
        "gitdir: ../../../../.git/worktrees/removed\n",
    )
    .unwrap();

    let host = FakeTaskHost::new(Vec::new());
    let result = collect_worktrees(
        &repo,
        &[],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    let reclaimed = result
        .reports
        .iter()
        .find(|report| report.path == trash_dir)
        .expect("trash report present in gc");
    assert_eq!(reclaimed.action, "removed");
    assert!(reclaimed.bytes_reclaimed > 0);
    assert_eq!(result.bytes_reclaimed, reclaimed.bytes_reclaimed);
    assert!(!trash_dir.exists());
}

#[test]
fn gc_keeps_trash_directory_when_deleter_pid_is_alive_and_within_timeout() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let live_pid = std::process::id();
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let trash_dir = repo
        .join(".orbit/state/worktrees")
        .join(format!(".trash-orbit-jrun-active-{live_pid}-{now_nanos}"));
    fs::create_dir_all(&trash_dir).unwrap();
    fs::write(trash_dir.join("active.txt"), "in progress deletion").unwrap();

    let host = FakeTaskHost::new(Vec::new());
    let result = collect_worktrees(
        &repo,
        &[],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    let kept = result
        .reports
        .iter()
        .find(|report| report.path == trash_dir)
        .expect("trash report present in gc");
    assert_eq!(kept.action, "skipped:deleter_alive");
    assert_eq!(kept.bytes_reclaimed, 0);
    assert!(trash_dir.exists());
}

#[test]
fn gc_leaves_malformed_trash_directory_as_unrecognized() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let malformed = repo
        .join(".orbit/state/worktrees")
        .join(".trash-not-matching-format");
    fs::create_dir_all(&malformed).unwrap();

    let host = FakeTaskHost::new(Vec::new());
    let result = collect_worktrees(
        &repo,
        &[],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    let report = result
        .reports
        .iter()
        .find(|report| report.path == malformed)
        .expect("malformed directory reported");
    assert_eq!(report.action, "skipped:unrecognized");
    assert!(malformed.exists());
}

#[cfg(unix)]
fn reaped_child_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("spawn child");
    let pid = child.id();
    child.wait().expect("reap child");
    pid
}
