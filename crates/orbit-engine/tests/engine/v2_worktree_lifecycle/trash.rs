//! Full trash inventory GC through the public collector, preserving unique work.

use super::*;
use orbit_engine::{WorktreeGcOptions, collect_worktrees};

#[cfg(unix)]
fn reaped_deleter_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("spawn deleter");
    let pid = child.id();
    child.wait().expect("reap deleter");
    pid
}

#[test]
fn gc_preserves_registered_dirty_trash_worktrees_in_dry_and_live_sweeps() {
    isolated_in(
        module_path!(),
        "gc_preserves_registered_dirty_trash_worktrees_in_dry_and_live_sweeps",
        || {
            let now_nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let deleters = vec![(std::process::id(), 0), (std::process::id(), now_nanos)];
            #[cfg(unix)]
            let deleters = {
                let mut deleters = deleters;
                let dead_pid = reaped_deleter_pid();
                deleters.extend([(dead_pid, 0), (dead_pid, now_nanos)]);
                deleters
            };
            for (pid, nanos) in deleters {
                let fixture = Fixture::new();
                let repo = fixture.repo.clone();
                let worktree = repo
                    .join(".orbit/state/worktrees")
                    .join(format!(".trash-orbit-jrun-restored-{pid}-{nanos}"));
                git(
                    &repo,
                    &[
                        "worktree",
                        "add",
                        "-b",
                        "restored",
                        path_str(&worktree),
                        "HEAD",
                    ],
                );
                fs::write(worktree.join("base.txt"), "unique tracked candidate").unwrap();
                fs::write(worktree.join("candidate.txt"), "unique untracked candidate").unwrap();
                let registration = git(&repo, &["worktree", "list", "--porcelain"]);
                let dirty = git(&worktree, &["status", "--porcelain"]);
                assert!(!dirty.is_empty());

                for delete in [false, true] {
                    let result = collect_worktrees(
                        &repo,
                        &[],
                        &LifecycleHost::new(&repo),
                        &WorktreeGcOptions {
                            delete,
                            estimate_bytes: true,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    assert_eq!(result.reports.len(), 1);
                    assert_eq!(result.reports[0].action, "skipped:registered_worktree");
                    assert_eq!(result.bytes_reclaimed, 0);
                    assert_eq!(
                        git(&repo, &["worktree", "list", "--porcelain"]),
                        registration
                    );
                    assert_eq!(git(&worktree, &["status", "--porcelain"]), dirty);
                    assert_eq!(
                        fs::read_to_string(worktree.join("base.txt")).unwrap(),
                        "unique tracked candidate",
                        "GC must preserve a registered trash checkout's unique tracked work"
                    );
                    assert_eq!(
                        fs::read_to_string(worktree.join("candidate.txt")).unwrap(),
                        "unique untracked candidate",
                        "GC must preserve a registered trash checkout's unique untracked work"
                    );
                }
            }
        },
    );
}

/// Fail-closed recovery checks are exercised through the collector, without
/// relying on permission bits (which do not refuse reads when tests run as root).
#[test]
fn gc_preserves_recoverable_or_unverifiable_trash_git_links() {
    isolated_in(
        module_path!(),
        "gc_preserves_recoverable_or_unverifiable_trash_git_links",
        || {
            let cases = [
                "local_absolute",
                "local_relative",
                "foreign_absolute",
                "foreign_relative",
                "malformed",
                "invalid_utf8",
                "directory",
                "admin_not_directory",
                "empty_target",
                "unexpected_lines",
                #[cfg(unix)]
                "symlink",
            ];
            for case in cases {
                let fixture = Fixture::new();
                let repo = fixture.repo.clone();
                let foreign = fixture.root.path().join("foreign");
                fs::create_dir_all(&foreign).unwrap();
                super::super::git_fixture::init(&foreign);
                let trash = repo
                    .join(".orbit/state/worktrees")
                    .join(format!(".trash-orbit-jrun-{case}-{}-0", std::process::id()));
                fs::create_dir_all(&trash).unwrap();
                let link = trash.join(".git");
                match case {
                    "local_absolute" => {
                        fs::write(&link, format!("gitdir: {}\n", repo.join(".git").display()))
                            .unwrap();
                    }
                    "local_relative" => fs::write(&link, "gitdir: ../../../../.git\n").unwrap(),
                    "foreign_absolute" => {
                        fs::write(
                            &link,
                            format!("gitdir: {}\n", foreign.join(".git").display()),
                        )
                        .unwrap();
                    }
                    "foreign_relative" => {
                        fs::write(&link, "gitdir: ../../../../../foreign/.git\n").unwrap();
                    }
                    "malformed" => fs::write(&link, "not a gitdir link").unwrap(),
                    "invalid_utf8" => fs::write(&link, [0xff]).unwrap(),
                    "directory" => fs::create_dir(&link).unwrap(),
                    "admin_not_directory" => {
                        fs::write(&link, "gitdir: ../../../../base.txt/child\n").unwrap();
                    }
                    "empty_target" => fs::write(&link, "gitdir: \n").unwrap(),
                    "unexpected_lines" => {
                        fs::write(&link, "unexpected\ngitdir: missing-admin\n").unwrap();
                    }
                    #[cfg(unix)]
                    "symlink" => std::os::unix::fs::symlink("missing-link", &link).unwrap(),
                    _ => unreachable!(),
                }
                fs::write(trash.join("candidate.txt"), "unique recoverable candidate").unwrap();
                let registration = git(&repo, &["worktree", "list", "--porcelain"]);
                for delete in [false, true] {
                    let result = collect_worktrees(
                        &repo,
                        &[],
                        &LifecycleHost::new(&repo),
                        &WorktreeGcOptions {
                            delete,
                            estimate_bytes: true,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    assert_eq!(result.reports.len(), 1, "case: {case}");
                    assert_eq!(
                        result.reports[0].action, "skipped:not_registered_worktree",
                        "GC must retain recoverable or unverifiable .git links: {case}"
                    );
                    assert_eq!(result.bytes_reclaimed, 0, "case: {case}");
                    assert!(fs::symlink_metadata(&link).is_ok(), "case: {case}");
                    assert_eq!(
                        fs::read_to_string(trash.join("candidate.txt")).unwrap(),
                        "unique recoverable candidate",
                        "case: {case}"
                    );
                    assert_eq!(
                        git(&repo, &["worktree", "list", "--porcelain"]),
                        registration
                    );
                }
            }
        },
    );
}
