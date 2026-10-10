//! Reclaim through the shipped deterministic boundary and resume through a fake CLI.

use super::*;
use std::os::unix::fs::symlink;

#[test]
fn sweep_reclaims_declared_paths_in_kept_worktrees_and_preserves_candidates() {
    isolated_in(
        module_path!(),
        "sweep_reclaims_declared_paths_in_kept_worktrees_and_preserves_candidates",
        || {
            let fixture = Fixture::new();
            let mut host = LifecycleHost::new(&fixture.repo);
            host.reclaim_patterns = Some(vec![
                "target".into(),
                "target/**".into(),
                "node_modules".into(),
                "website/**/node_modules".into(),
                ".orbit/tmp/*target*".into(),
                ".orbit/tmp/codeql-*".into(),
                "base.txt".into(),
            ]);
            for (index, status) in [
                TaskStatus::Blocked,
                TaskStatus::Review,
                TaskStatus::InProgress,
            ]
            .into_iter()
            .enumerate()
            {
                let task = format!("T-RECLAIM-{index}");
                let run = format!("jrun-reclaim-{index}");
                host.add_task(&task, TaskStatus::Backlog);
                let input = setup_input(&[&task], &run);
                let checkout =
                    Checkout::from_setup(&action(&host, "worktree_setup", &input).unwrap());
                let head = commit_file(&checkout.path, "candidate.txt", "candidate\n");
                host.set_status(&task, status);
                host.add_run(job_run(&run, JobRunState::Failed, input));
                for directory in [
                    "target",
                    "node_modules",
                    "website/app/node_modules",
                    "website/node_modules",
                    ".orbit/tmp/baseline-target",
                    ".orbit/tmp/codeql-rust-local.fixture",
                ] {
                    fs::create_dir_all(checkout.path.join(directory)).unwrap();
                    fs::write(checkout.path.join(directory).join("output"), vec![0; 31]).unwrap();
                }
                fs::create_dir_all(checkout.path.join(".orbit/tmp")).unwrap();
                fs::write(checkout.path.join(".orbit/tmp/evidence.json"), "evidence").unwrap();
                fs::write(checkout.path.join("unmatched.txt"), "uncommitted work").unwrap();
                let result = action(&host, "worktree_gc", &json!({"target_run_id": run})).unwrap();
                let report = &result["reports"][0];
                assert_eq!(
                    report["action"], "skipped:task_status_ineligible",
                    "{report}"
                );
                let paths = report["reclaim"].as_array().unwrap();
                assert_eq!(
                    paths
                        .iter()
                        .filter(|path| path["action"] == "removed")
                        .count(),
                    6,
                    "{report}"
                );
                assert_eq!(report["bytes_reclaimed"], 186);
                for path in paths.iter().filter(|path| path["action"] == "removed") {
                    assert_eq!(path["bytes_reclaimed"], 31);
                    assert!(
                        host.reclaim_patterns
                            .as_ref()
                            .unwrap()
                            .iter()
                            .any(|pattern| path["pattern"] == *pattern)
                    );
                    assert!(!checkout.path.join(path["path"].as_str().unwrap()).exists());
                }
                assert_eq!(git(&checkout.path, &["rev-parse", "HEAD"]), head);
                assert_eq!(git(&fixture.repo, &["rev-parse", &checkout.branch]), head);
                assert_eq!(
                    fs::read_to_string(checkout.path.join("candidate.txt")).unwrap(),
                    "candidate\n"
                );
                assert!(checkout.path.join("base.txt").exists());
                assert_eq!(
                    fs::read_to_string(checkout.path.join(".orbit/tmp/evidence.json")).unwrap(),
                    "evidence"
                );
                assert_eq!(
                    fs::read_to_string(checkout.path.join("unmatched.txt")).unwrap(),
                    "uncommitted work"
                );
                assert!(registered_worktrees(&fixture.repo).contains(&checkout.path));
                // Every subsequent sweep is safe and has no duplicate accounting.
                assert_eq!(
                    action(&host, "worktree_gc", &json!({"target_run_id": run})).unwrap()["bytes_reclaimed"],
                    0
                );
            }
        },
    );
}

#[test]
fn reclaim_fails_closed_for_symlinks_tracked_content_workers_and_shared_active_paths() {
    isolated_in(
        module_path!(),
        "reclaim_fails_closed_for_symlinks_tracked_content_workers_and_shared_active_paths",
        || {
            let fixture = Fixture::new();
            let mut host = LifecycleHost::new(&fixture.repo);
            host.reclaim_patterns = Some(vec!["cache".into(), "linked/cache".into()]);
            let outside = fixture.root.path().join("outside");
            fs::create_dir_all(outside.join("cache")).unwrap();
            fs::write(outside.join("cache/output"), "outside").unwrap();
            for case in [
                "symlink",
                "ancestor",
                "tracked",
                "alive",
                "undecidable",
                "active",
                "shared-active",
                "unregistered",
            ] {
                let task = format!("T-SAFETY-{case}");
                let run_id = format!("jrun-safety-{case}");
                host.add_task(&task, TaskStatus::Backlog);
                let input = setup_input(&[&task], &run_id);
                let checkout =
                    Checkout::from_setup(&action(&host, "worktree_setup", &input).unwrap());
                host.set_status(&task, TaskStatus::Blocked);
                let mut run = job_run(&run_id, JobRunState::Failed, input);
                let protected = if case == "ancestor" {
                    checkout.path.join("linked/cache")
                } else {
                    checkout.path.join("cache")
                };
                if case == "symlink" {
                    symlink(outside.join("cache"), &protected).unwrap();
                } else if case == "ancestor" {
                    symlink(&outside, checkout.path.join("linked")).unwrap();
                } else {
                    fs::create_dir_all(&protected).unwrap();
                    fs::write(protected.join("output"), "protected").unwrap();
                }
                match case {
                    "tracked" => {
                        git(&checkout.path, &["add", "cache/output"]);
                        git(&checkout.path, &["commit", "-m", "tracked work"]);
                    }
                    "alive" => {
                        run.pid = Some(std::process::id());
                    }
                    "undecidable" => {
                        run.pid = Some(std::process::id());
                        run.pid_start_time = Some("ps-lstart-utc-v2:pidns=foreign:test".into());
                        assert_eq!(
                            orbit_common::process::identity::probe_process_liveness(
                                std::process::id(),
                                run.pid_start_time.as_deref()
                            ),
                            orbit_common::process::identity::ProcessLiveness::Unknown
                        );
                    }
                    "active" => {
                        run.state = JobRunState::Running;
                    }
                    "shared-active" => {
                        let mut live = run.clone();
                        live.run_id = format!("{run_id}-live");
                        live.state = JobRunState::Running;
                        host.add_run(live);
                    }
                    "unregistered" => {
                        git(
                            &fixture.repo,
                            &[
                                "worktree",
                                "move",
                                path_str(&checkout.path),
                                path_str(&fixture.root.path().join("moved")),
                            ],
                        );
                        fs::create_dir_all(&protected).unwrap();
                        fs::write(protected.join("output"), "protected").unwrap();
                    }
                    _ => {}
                }
                host.add_run(run);
                let result =
                    action(&host, "worktree_gc", &json!({"target_run_id": run_id})).unwrap();
                assert_eq!(result["bytes_reclaimed"], 0, "{case}: {result}");
                assert!(
                    protected.exists(),
                    "{case}: protected path must survive: {result}"
                );
                assert_eq!(
                    fs::read_to_string(outside.join("cache/output")).unwrap(),
                    "outside"
                );
            }
        },
    );
}

#[test]
fn a_reclaimed_candidate_resumes_rebuilds_with_a_fake_agent_and_completes() {
    isolated_in(
        module_path!(),
        "a_reclaimed_candidate_resumes_rebuilds_with_a_fake_agent_and_completes",
        || {
            let preserved = PreservedCandidate::new("feature.txt", "feature\n");
            let paths = orbit_engine::run_worktree_paths(
                &preserved.fixture.repo,
                &preserved.host.runs.lock().unwrap()[0],
            )
            .unwrap();
            let old = &paths[0];
            fs::create_dir_all(old.join("target")).unwrap();
            fs::write(old.join("target/compiled"), "old build").unwrap();
            let reclaimed = action(&preserved.host, "worktree_gc", &json!({})).unwrap();
            assert_eq!(reclaimed["bytes_reclaimed"], 9, "{reclaimed}");
            assert!(!old.join("target").exists());
            assert_eq!(git(old, &["rev-parse", "HEAD"]), preserved.candidate);
            let setup = preserved.next_setup();
            let resumed = preserved.resume(&setup).unwrap();
            assert_eq!(resumed["outcome"], "resumed_validated", "{resumed}");
            let checkout = Checkout::from_setup(&setup);
            let provider = preserved.fixture.root.path().join("codex");
            write_executable(
                &provider,
                &provider_script(
                    "test -f feature.txt\ntest ! -e target\nmkdir target\nprintf rebuilt > target/compiled",
                ),
            );
            let host = preserved.host.with_provider(&provider);
            let outcome =
                dispatch_linked_provider(&host, NEXT_RUN, RESUME_TASK, &checkout.path).unwrap();
            assert!(outcome.success, "{outcome:?}");
            assert_eq!(
                fs::read_to_string(checkout.path.join("target/compiled")).unwrap(),
                "rebuilt"
            );
            // Commit the candidate through the real deterministic boundary. The
            // fake build is ignored so it cannot accidentally enter the delivery.
            fs::write(checkout.path.join(".gitignore"), ".orbit/\ntarget/\n").unwrap();
            let committed = action(
                &host,
                "git_commit",
                &json!({"job_run_id": NEXT_RUN,
            "scope": "all", "workspace_path": checkout.path,
            "base_ref": setup["base_ref"], "base_sha": setup["base_sha"]}),
            )
            .unwrap();
            assert_eq!(
                git(
                    &checkout.path,
                    &[
                        "show",
                        &format!("{}:feature.txt", committed["commit_sha"].as_str().unwrap())
                    ]
                ),
                "feature"
            );
            assert!(git(&checkout.path, &["status", "--porcelain"]).is_empty());
        },
    );
}
