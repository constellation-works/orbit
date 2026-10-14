//! [ORB-15613] The scheduled sweep removes a settled run's dirty worktree only
//! once its uncommitted candidate is recoverable elsewhere, and reclaims build
//! output from every kept terminal worktree without touching its source.

use super::*;

/// A done or archived task's checkout with uncommitted work in it: a tracked
/// edit (binary bytes included), a new file, and gigabyte-class build output
/// stood in for by small ignored directories.
struct DirtyCheckout {
    checkout: Checkout,
    run_id: String,
    task_id: String,
}

impl DirtyCheckout {
    fn new(host: &LifecycleHost, case: &str, status: TaskStatus) -> Self {
        // Build output is ignored, as in a real checkout.
        fs::create_dir_all(host.repo.join(".git/info")).unwrap();
        fs::write(host.repo.join(".git/info/exclude"), "target/\n").unwrap();
        let task_id = format!("T-RESCUE-{case}");
        let run_id = format!("jrun-rescue-{case}");
        host.add_task(&task_id, TaskStatus::Backlog);
        let input = setup_input(&[&task_id], &run_id);
        let checkout = Checkout::from_setup(&action(host, "worktree_setup", &input).unwrap());
        fs::write(
            checkout.path.join("README.md"),
            b"edited\n\x00\xff binary tail\n",
        )
        .unwrap();
        fs::create_dir_all(checkout.path.join("src")).unwrap();
        fs::write(checkout.path.join("src/new.rs"), "fn candidate() {}\n").unwrap();
        for build in [
            "target",
            ".orbit/tmp/lint-baseline-target",
            ".orbit/tmp/base-wt",
        ] {
            fs::create_dir_all(checkout.path.join(build)).unwrap();
            fs::write(checkout.path.join(build).join("output"), vec![0; 64]).unwrap();
        }
        host.set_status(&task_id, status);
        host.add_run(job_run(&run_id, JobRunState::Failed, input));
        Self {
            checkout,
            run_id,
            task_id,
        }
    }

    fn sweep(&self, host: &LifecycleHost) -> Value {
        let result = action(host, "worktree_gc", &json!({"target_run_id": self.run_id})).unwrap();
        result["reports"][0].clone()
    }
}

/// The candidate tree as the carry stages it, committed without moving any
/// ref or touching the worktree's index.
fn snapshot_commit(checkout: &Path) -> String {
    let index = checkout.parent().unwrap().join("snapshot.index");
    let scratch = |args: &[&str]| {
        let output = crate::git_test_support::command()
            .args(args)
            .current_dir(checkout)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    scratch(&["read-tree", "HEAD"]);
    scratch(&["add", "--all", "--", "."]);
    let tree = scratch(&["write-tree"]);
    fs::remove_file(&index).unwrap();
    git(
        checkout,
        &[
            "commit-tree",
            &tree,
            "-p",
            "HEAD",
            "-m",
            "partial candidate",
        ],
    )
}

fn a_settled_dirty_worktree_goes_only_once_its_candidate_is_recoverable() {
    isolated_in(
        module_path!(),
        "a_settled_dirty_worktree_goes_only_once_its_candidate_is_recoverable",
        || {
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            let rescue_store = fixture.repo.join(".orbit/state/rescue/worktree-gc");

            // A durable carry ref holding exactly the candidate is enough: no
            // patch is written and the report names the ref.
            let carried = DirtyCheckout::new(&host, "carry", TaskStatus::Done);
            let carry_commit = snapshot_commit(&carried.checkout.path);
            let carry_ref = format!(
                "refs/orbit/candidates/{}/{}",
                carried.task_id, carried.run_id
            );
            git(
                &carried.checkout.path,
                &["push", "origin", &format!("{carry_commit}:{carry_ref}")],
            );
            let report = carried.sweep(&host);
            assert_eq!(report["action"], "removed", "{report}");
            assert_eq!(report["recovery"]["source"], "carry_ref", "{report}");
            assert_eq!(report["recovery"]["location"], carry_ref, "{report}");
            assert_eq!(report["recovery"]["commit"], carry_commit, "{report}");
            assert!(!carried.checkout.path.exists());
            assert!(!registered_worktrees(&fixture.repo).contains(&carried.checkout.path));
            assert!(
                !rescue_store.exists(),
                "a carried candidate needs no rescue patch"
            );
            assert_eq!(
                git(&fixture.repo, &["for-each-ref", "refs/orbit/rescue/"]),
                "",
                "a carried candidate needs no rescue base ref"
            );

            // With nothing on `origin`, GC writes a patch, proves it, and only
            // then removes the worktree; the patch rebuilds the candidate.
            let patched = DirtyCheckout::new(&host, "patch", TaskStatus::Archived);
            // Staging a path whose working content matches is still rescuable,
            // alongside the independent unstaged README edit.
            git(&patched.checkout.path, &["add", "src/new.rs"]);
            let head = git(&patched.checkout.path, &["rev-parse", "HEAD"]);
            let report = patched.sweep(&host);
            assert_eq!(report["action"], "removed", "{report}");
            assert_eq!(report["recovery"]["source"], "rescue_patch", "{report}");
            assert_eq!(report["recovery"]["commit"], head, "{report}");
            assert!(!patched.checkout.path.exists());
            let patch = PathBuf::from(report["recovery"]["location"].as_str().unwrap());
            assert!(patch.starts_with(&rescue_store), "{report}");
            let restored = fixture.root.path().join("restored");
            git(
                &fixture.repo,
                &["worktree", "add", "--detach", path_str(&restored), &head],
            );
            git(&restored, &["apply", path_str(&patch)]);
            assert_eq!(
                fs::read(restored.join("README.md")).unwrap(),
                b"edited\n\x00\xff binary tail\n"
            );
            assert_eq!(
                fs::read_to_string(restored.join("src/new.rs")).unwrap(),
                "fn candidate() {}\n"
            );

            // A candidate GC cannot capture exactly — here a nested repository,
            // whose history a tree cannot carry — keeps its worktree.
            let nested = DirtyCheckout::new(&host, "nested", TaskStatus::Done);
            let inner = nested.checkout.path.join("vendor/inner");
            fs::create_dir_all(&inner).unwrap();
            super::super::git_fixture::init(&inner);
            git(&inner, &["config", "user.name", "Orbit Test"]);
            git(
                &inner,
                &["config", "user.email", "orbit-test@example.invalid"],
            );
            commit_file(&inner, "history.txt", "only here\n");
            let report = nested.sweep(&host);
            assert_eq!(
                report["action"], "skipped:dirty_rescue_candidate",
                "{report}"
            );
            assert!(report["detail"].as_str().is_some(), "{report}");
            assert!(report.get("recovery").is_none(), "{report}");
            assert_eq!(
                fs::read_to_string(inner.join("history.txt")).unwrap(),
                "only here\n"
            );
            assert!(registered_worktrees(&fixture.repo).contains(&nested.checkout.path));
            // Its build output is still reclaimed, and the size it keeps is reported.
            assert!(!nested.checkout.path.join("target").exists(), "{report}");
            assert!(report["retained_bytes"].as_u64().unwrap() > 0, "{report}");
        },
    );
}

fn distinct_staged_and_working_versions_keep_the_settled_worktree() {
    isolated_in(
        module_path!(),
        "distinct_staged_and_working_versions_keep_the_settled_worktree",
        || {
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            for recovery in ["patch", "carry", "branch"] {
                let dirty = DirtyCheckout::new(&host, recovery, TaskStatus::Done);
                let path = &dirty.checkout.path;
                fs::write(path.join("base.txt"), "unique staged version\n").unwrap();
                git(path, &["add", "base.txt"]);
                fs::write(path.join("base.txt"), "different working version\n").unwrap();
                // DirtyCheckout also supplies an independent ordinary README
                // edit and an untracked file, so the working tree differs from HEAD.
                let status = git(path, &["status", "--porcelain", "--untracked-files=all"]);
                assert!(status.lines().any(|line| line == "MM base.txt"), "{status}");
                let staged = git_bytes(path, &["diff", "--cached", "--binary"]);
                let working = git_bytes(path, &["diff", "HEAD", "--binary"]);
                if recovery != "patch" {
                    let candidate = snapshot_commit(path);
                    assert_eq!(
                        git(path, &["show", &format!("{candidate}:base.txt")]),
                        "different working version"
                    );
                    let remote_ref = if recovery == "carry" {
                        format!("refs/orbit/candidates/{}/{}", dirty.task_id, dirty.run_id)
                    } else {
                        format!("refs/heads/{}", dirty.checkout.branch)
                    };
                    git(
                        path,
                        &["push", "origin", &format!("{candidate}:{remote_ref}")],
                    );
                }

                let report = dirty.sweep(&host);
                assert_eq!(
                    report["action"], "skipped:dirty_rescue_candidate",
                    "{report}"
                );
                assert!(
                    report["detail"]
                        .as_str()
                        .unwrap()
                        .contains("staged paths differ"),
                    "{report}"
                );
                assert!(report.get("recovery").is_none(), "{report}");
                assert!(
                    registered_worktrees(&fixture.repo).contains(path),
                    "{report}"
                );
                assert_eq!(git(path, &["show", ":base.txt"]), "unique staged version");
                assert_eq!(
                    fs::read(path.join("base.txt")).unwrap(),
                    b"different working version\n"
                );
                assert_eq!(git_bytes(path, &["diff", "--cached", "--binary"]), staged);
                assert_eq!(git_bytes(path, &["diff", "HEAD", "--binary"]), working);
                assert_eq!(
                    git(path, &["status", "--porcelain", "--untracked-files=all"]),
                    status
                );
            }
            assert!(
                !fixture
                    .repo
                    .join(".orbit/state/rescue/worktree-gc")
                    .exists()
            );
        },
    );
}

fn a_symlinked_rescue_store_keeps_the_worktree_and_writes_nothing_outside() {
    isolated_in(
        module_path!(),
        "a_symlinked_rescue_store_keeps_the_worktree_and_writes_nothing_outside",
        || {
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            let outside = fixture.root.path().join("outside");
            fs::create_dir_all(&outside).unwrap();
            fs::create_dir_all(fixture.repo.join(".orbit/state")).unwrap();
            std::os::unix::fs::symlink(&outside, fixture.repo.join(".orbit/state/rescue")).unwrap();
            let dirty = DirtyCheckout::new(&host, "redirected", TaskStatus::Done);
            let report = dirty.sweep(&host);
            assert_eq!(
                report["action"], "skipped:dirty_rescue_candidate",
                "{report}"
            );
            assert!(
                report["detail"]
                    .as_str()
                    .unwrap()
                    .contains("rescue store refused"),
                "{report}"
            );
            assert!(registered_worktrees(&fixture.repo).contains(&dirty.checkout.path));
            assert_eq!(
                fs::read(dirty.checkout.path.join("README.md")).unwrap(),
                b"edited\n\x00\xff binary tail\n"
            );
            assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
            assert_eq!(
                git(&fixture.repo, &["for-each-ref", "refs/orbit/rescue/"]),
                "",
                "a refused store must not pin a base"
            );
        },
    );
}

fn unpublished_head_content_recovers_after_branch_deletion_and_object_expiry() {
    isolated_in(
        module_path!(),
        "unpublished_head_content_recovers_after_branch_deletion_and_object_expiry",
        || {
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            let dirty = DirtyCheckout::new(&host, "unpublished", TaskStatus::Done);
            let path = &dirty.checkout.path;
            // A commit that exists only on the run branch. The later working
            // edits stay unstaged, so the rescue patch does not repeat this file.
            fs::write(path.join("local-only.txt"), "only on the run branch\n").unwrap();
            git(path, &["add", "local-only.txt"]);
            git(path, &["commit", "-m", "unpublished local file"]);
            let head = git(path, &["rev-parse", "HEAD"]);
            let published = git(
                &fixture.repo,
                &["rev-list", "agent-main", "origin/agent-main"],
            );
            assert!(
                !published.lines().any(|line| line == head),
                "fixture HEAD must be unpublished:\n{published}"
            );

            let report = dirty.sweep(&host);
            assert_eq!(report["action"], "removed", "{report}");
            assert_eq!(report["recovery"]["source"], "rescue_patch", "{report}");
            assert_eq!(report["recovery"]["commit"], head, "{report}");
            assert!(!path.exists());
            assert!(
                !git_ok(
                    &fixture.repo,
                    &[
                        "show-ref",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{}", dirty.checkout.branch),
                    ],
                ),
                "run branch still exists"
            );
            let patch = PathBuf::from(report["recovery"]["location"].as_str().unwrap());
            let patch_bytes = fs::read(&patch).unwrap();
            assert!(
                !patch_bytes
                    .windows(b"local-only.txt".len())
                    .any(|window| window == b"local-only.txt"),
                "unchanged unpublished file must not be stored in the patch"
            );
            let header = String::from_utf8_lossy(&patch_bytes);
            let base_ref = header
                .lines()
                .find_map(|line| line.strip_prefix("Base-Ref: "))
                .expect("patch names its base ref")
                .to_string();
            assert!(
                base_ref.starts_with("refs/orbit/rescue/worktree-gc/"),
                "{base_ref}"
            );
            assert_eq!(
                git(
                    &fixture.repo,
                    &["rev-parse", &format!("{base_ref}^{{commit}}")]
                ),
                head
            );

            // The branch and its reflog are gone. The rescue ref is the root.
            expire_unreachable_objects(&fixture.repo);
            assert!(
                git_ok(&fixture.repo, &["cat-file", "-e", &head]),
                "patch base was pruned"
            );
            let reachable = git(&fixture.repo, &["rev-list", "--all"]);
            assert!(
                reachable.lines().any(|line| line == head),
                "patch base is not reachable:\n{reachable}"
            );

            let restored = fixture.root.path().join("restored");
            git(
                &fixture.repo,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    path_str(&restored),
                    &base_ref,
                ],
            );
            git(&restored, &["apply", path_str(&patch)]);
            assert_eq!(
                fs::read_to_string(restored.join("local-only.txt")).unwrap(),
                "only on the run branch\n"
            );
            assert_eq!(
                fs::read(restored.join("README.md")).unwrap(),
                b"edited\n\x00\xff binary tail\n"
            );
            assert_eq!(
                fs::read_to_string(restored.join("src/new.rs")).unwrap(),
                "fn candidate() {}\n"
            );

            // The pin lasts as long as the patch. Once the file is gone, a
            // deleting sweep drops the ref and expiry can remove the commit.
            git(
                &fixture.repo,
                &["worktree", "remove", "--force", path_str(&restored)],
            );
            fs::remove_file(&patch).unwrap();
            action(
                &host,
                "worktree_gc",
                &json!({"target_run_id": dirty.run_id}),
            )
            .unwrap();
            assert!(
                !git_ok(
                    &fixture.repo,
                    &["show-ref", "--verify", "--quiet", &base_ref],
                ),
                "rescue base ref outlived its patch"
            );
            expire_unreachable_objects(&fixture.repo);
            assert!(
                !git_ok(&fixture.repo, &["cat-file", "-e", &head]),
                "released patch base is still in the object store"
            );
        },
    );
}

fn an_open_task_keeps_its_dirty_source_byte_for_byte_and_loses_only_build_output() {
    isolated_in(
        module_path!(),
        "an_open_task_keeps_its_dirty_source_byte_for_byte_and_loses_only_build_output",
        || {
            let fixture = Fixture::new();
            // The host's own default patterns, not a test list.
            let host = LifecycleHost::new(&fixture.repo);
            let dirty = DirtyCheckout::new(&host, "open", TaskStatus::InProgress);
            let path = &dirty.checkout.path;
            fs::write(path.join("base.txt"), "staged edit\n").unwrap();
            git(path, &["add", "base.txt"]);
            fs::write(path.join(".orbit/tmp/evidence.json"), "evidence").unwrap();
            let diff_before = git_bytes(path, &["diff", "HEAD", "--binary"]);
            let staged_before = git_bytes(path, &["diff", "--cached", "--binary"]);
            let status_before = git(path, &["status", "--porcelain", "--untracked-files=all"]);
            let untracked_before = fs::read(path.join("src/new.rs")).unwrap();

            let report = dirty.sweep(&host);
            assert_eq!(
                report["action"], "skipped:task_status_ineligible",
                "{report}"
            );
            for build in [
                "target",
                ".orbit/tmp/lint-baseline-target",
                ".orbit/tmp/base-wt",
            ] {
                assert!(!path.join(build).exists(), "{build} reclaimed: {report}");
            }
            assert_eq!(report["bytes_reclaimed"], 3 * 64, "{report}");
            assert_eq!(git_bytes(path, &["diff", "HEAD", "--binary"]), diff_before);
            assert_eq!(
                git_bytes(path, &["diff", "--cached", "--binary"]),
                staged_before
            );
            assert_eq!(
                git(path, &["status", "--porcelain", "--untracked-files=all"]),
                status_before
            );
            assert_eq!(fs::read(path.join("src/new.rs")).unwrap(), untracked_before);
            assert_eq!(
                fs::read_to_string(path.join(".orbit/tmp/evidence.json")).unwrap(),
                "evidence"
            );
            assert!(report["retained_bytes"].as_u64().unwrap() > 0, "{report}");
            assert!(registered_worktrees(&fixture.repo).contains(path));
        },
    );
}

fn a_live_or_too_recent_dirty_worktree_is_never_removed() {
    isolated_in(
        module_path!(),
        "a_live_or_too_recent_dirty_worktree_is_never_removed",
        || {
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            let live = DirtyCheckout::new(&host, "live", TaskStatus::Done);
            host.runs
                .lock()
                .unwrap()
                .retain(|run| run.run_id != live.run_id);
            let input = setup_input(&[&live.task_id], &live.run_id);
            host.add_run(job_run(&live.run_id, JobRunState::Running, input));
            let report = live.sweep(&host);
            assert_eq!(report["action"], "skipped:run_not_terminal", "{report}");
            assert!(live.checkout.path.join("target/output").exists());
            assert!(report.get("retained_bytes").is_none(), "{report}");

            let recent = DirtyCheckout::new(&host, "recent", TaskStatus::Done);
            let result = action(
                &host,
                "worktree_gc",
                &json!({"target_run_id": recent.run_id, "older_than_hours": 1}),
            )
            .unwrap();
            assert_eq!(
                result["reports"][0]["action"], "skipped:too_recent",
                "{result}"
            );

            for checkout in [&live.checkout.path, &recent.checkout.path] {
                assert!(registered_worktrees(&fixture.repo).contains(checkout));
                assert_eq!(
                    fs::read(checkout.join("README.md")).unwrap(),
                    b"edited\n\x00\xff binary tail\n"
                );
            }
            assert!(!fixture.repo.join(".orbit/state/rescue").exists());
        },
    );
}

fn expire_unreachable_objects(repo: &Path) {
    git(
        repo,
        &[
            "reflog",
            "expire",
            "--expire=now",
            "--expire-unreachable=now",
            "--all",
        ],
    );
    git(repo, &["gc", "--prune=now"]);
}

fn git_ok(current_dir: &Path, args: &[&str]) -> bool {
    crate::git_test_support::command()
        .args(args)
        .current_dir(current_dir)
        .status()
        .unwrap()
        .success()
}

fn git_bytes(current_dir: &Path, args: &[&str]) -> Vec<u8> {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    output.stdout
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn rescue_contracts_1() {
    isolated_in(module_path!(), "rescue_contracts_1", || {
        crate::git_fixture::run_scenarios(&[
            (
                "a_settled_dirty_worktree_goes_only_once_its_candidate_is_recoverable",
                a_settled_dirty_worktree_goes_only_once_its_candidate_is_recoverable,
            ),
            (
                "distinct_staged_and_working_versions_keep_the_settled_worktree",
                distinct_staged_and_working_versions_keep_the_settled_worktree,
            ),
            (
                "a_symlinked_rescue_store_keeps_the_worktree_and_writes_nothing_outside",
                a_symlinked_rescue_store_keeps_the_worktree_and_writes_nothing_outside,
            ),
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn rescue_contracts_2() {
    isolated_in(module_path!(), "rescue_contracts_2", || {
        crate::git_fixture::run_scenarios(&[
            (
                "unpublished_head_content_recovers_after_branch_deletion_and_object_expiry",
                unpublished_head_content_recovers_after_branch_deletion_and_object_expiry,
            ),
            (
                "an_open_task_keeps_its_dirty_source_byte_for_byte_and_loses_only_build_output",
                an_open_task_keeps_its_dirty_source_byte_for_byte_and_loses_only_build_output,
            ),
            (
                "a_live_or_too_recent_dirty_worktree_is_never_removed",
                a_live_or_too_recent_dirty_worktree_is_never_removed,
            ),
        ]);
    });
}
