//! Reclaim through the shipped deterministic boundary and resume through a fake CLI.

use super::*;
use std::os::unix::fs::symlink;

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

/// Host validation output lives outside the checkout, keyed by its path, so
/// removing the checkout used to strand it. It goes only for attributed,
/// terminal and inactive runs; everything else is retained and reported.
fn host_validation_output_is_reclaimed_only_for_terminal_inactive_attributed_checkouts() {
    isolated_in(
        module_path!(),
        "host_validation_output_is_reclaimed_only_for_terminal_inactive_attributed_checkouts",
        || {
            // Only Linux required validation builds into a host-owned target.
            if !cfg!(target_os = "linux") {
                return;
            }
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            let build = |checkout: &Path| -> PathBuf {
                let run = orbit_engine::review_gate::run_required_validation(
                    &host,
                    checkout,
                    "mkdir -p \"$CARGO_TARGET_DIR/debug\" && printf 'host build' > \"$CARGO_TARGET_DIR/debug/output\"",
                )
                .unwrap();
                assert!(run.passed, "{}", run.output);
                PathBuf::from(run.environment["cargo_target_dir"].as_str().unwrap())
            };
            let outside = fixture.root.path().join("outside");
            fs::create_dir_all(&outside).unwrap();
            fs::write(outside.join("output"), "outside").unwrap();
            let unrelated = fixture.repo.join(".orbit/state/compiler-cache/output");
            fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
            fs::write(&unrelated, "cache").unwrap();
            // (case, report action of a dry run, of the scheduled sweep)
            let cases = [
                ("orphan", "would_reclaim", "reclaimed"),
                ("kept", "skipped:source_retained", "reclaimed"),
                (
                    "active",
                    "skipped:run_not_terminal",
                    "skipped:run_not_terminal",
                ),
                ("alive", "skipped:worker_alive", "skipped:worker_alive"),
                (
                    "shared-active",
                    "skipped:run_not_terminal",
                    "skipped:run_not_terminal",
                ),
                (
                    "symlink",
                    "skipped:not_a_real_directory",
                    "skipped:not_a_real_directory",
                ),
            ];
            let mut targets = BTreeMap::new();
            let mut kept = None;
            for (case, _, _) in cases {
                let task = format!("T-HOST-{case}");
                let run_id = format!("jrun-host-{case}");
                host.add_task(&task, TaskStatus::Backlog);
                let input = setup_input(&[&task], &run_id);
                let checkout =
                    Checkout::from_setup(&action(&host, "worktree_setup", &input).unwrap());
                host.set_status(&task, TaskStatus::Blocked);
                let target = build(&checkout.path);
                assert!(target.join("debug/output").is_file());
                if case != "kept" {
                    git(
                        &fixture.repo,
                        &["worktree", "remove", "--force", path_str(&checkout.path)],
                    );
                }
                let mut run = job_run(&run_id, JobRunState::Failed, input);
                match case {
                    "kept" => {
                        let head = commit_file(&checkout.path, "candidate.txt", "candidate\n");
                        kept = Some((checkout, head));
                    }
                    "active" => run.state = JobRunState::Running,
                    "alive" => run.pid = Some(std::process::id()),
                    "shared-active" => {
                        let mut live = run.clone();
                        live.run_id = format!("{run_id}-live");
                        live.state = JobRunState::Running;
                        host.add_run(live);
                    }
                    "symlink" => {
                        fs::remove_dir_all(&target).unwrap();
                        symlink(&outside, &target).unwrap();
                    }
                    _ => {}
                }
                host.add_run(run);
                targets.insert(case, target);
            }
            // A detached scratch checkout no run record names.
            let scratch = fixture
                .repo
                .join(".orbit/state/worktrees/orbit-baseline-fixture");
            git(
                &fixture.repo,
                &["worktree", "add", "--detach", path_str(&scratch), BASE],
            );
            let unattributed = build(&scratch);
            git(
                &fixture.repo,
                &["worktree", "remove", "--force", path_str(&scratch)],
            );
            let host_reports = |result: &Value| -> BTreeMap<PathBuf, Value> {
                result["reports"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|report| {
                        report["path"]
                            .as_str()
                            .is_some_and(|path| path.contains("host-validation-targets"))
                    })
                    .map(|report| {
                        (
                            PathBuf::from(report["path"].as_str().unwrap()),
                            report.clone(),
                        )
                    })
                    .collect()
            };

            let dry_run = orbit_engine::collect_worktrees(
                &fixture.repo,
                &host.list_job_runs_for_gc().unwrap(),
                &host,
                &orbit_engine::WorktreeGcOptions {
                    estimate_bytes: true,
                    ..Default::default()
                },
            )
            .unwrap();
            let dry_run = host_reports(&serde_json::to_value(&dry_run).unwrap());
            assert_eq!(dry_run.len(), cases.len() + 1, "{dry_run:#?}");
            let estimated = dry_run[&targets["orphan"]]["bytes_reclaimed"]
                .as_u64()
                .unwrap();
            assert!(estimated >= 10, "{dry_run:#?}");
            for (case, expected, _) in cases {
                let report = &dry_run[&targets[case]];
                assert_eq!(report["action"], expected, "{case}: {report:#}");
                assert!(targets[case].exists(), "{case}: a dry run deletes nothing");
            }
            assert_eq!(
                dry_run[&unattributed]["action"], "skipped:unattributed_host_target",
                "{dry_run:#?}"
            );

            let swept = action(&host, "worktree_gc", &json!({})).unwrap();
            let reports = host_reports(&swept);
            for (case, _, expected) in cases {
                let report = &reports[&targets[case]];
                assert_eq!(report["action"], expected, "{case}: {report:#}");
                assert_eq!(
                    targets[case].exists(),
                    expected != "reclaimed",
                    "{case}: {report:#}"
                );
            }
            assert_eq!(reports[&targets["orphan"]]["bytes_reclaimed"], estimated);
            assert!(unattributed.join("debug/output").is_file());
            assert!(
                fs::symlink_metadata(&targets["symlink"])
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                fs::read_to_string(outside.join("output")).unwrap(),
                "outside"
            );
            assert_eq!(fs::read_to_string(&unrelated).unwrap(), "cache");
            // Reclaiming the output keeps the kept run's checkout, branch and candidate.
            let (checkout, head) = kept.unwrap();
            assert_eq!(git(&checkout.path, &["rev-parse", "HEAD"]), head);
            assert_eq!(git(&fixture.repo, &["rev-parse", &checkout.branch]), head);
            assert_eq!(
                fs::read_to_string(checkout.path.join("candidate.txt")).unwrap(),
                "candidate\n"
            );
            assert!(registered_worktrees(&fixture.repo).contains(&checkout.path));
            // No duplicate accounting on the next sweep.
            let again = action(&host, "worktree_gc", &json!({})).unwrap();
            assert!(
                host_reports(&again)
                    .values()
                    .all(|report| report["bytes_reclaimed"] == 0),
                "{again:#}"
            );
        },
    );
}

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
            assert_eq!(resumed["outcome"], "resumed_carried", "{resumed}");
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

/// Behavior-distinct rows share one bounded family process.
#[test]
fn reclaim_contracts_1() {
    isolated_in(module_path!(), "reclaim_contracts_1", || {
        crate::git_fixture::run_scenarios(&[
            (
                "sweep_reclaims_declared_paths_in_kept_worktrees_and_preserves_candidates",
                sweep_reclaims_declared_paths_in_kept_worktrees_and_preserves_candidates,
            ),
            (
                "reclaim_fails_closed_for_symlinks_tracked_content_workers_and_shared_active_paths",
                reclaim_fails_closed_for_symlinks_tracked_content_workers_and_shared_active_paths,
            ),
            (
                "host_validation_output_is_reclaimed_only_for_terminal_inactive_attributed_checkouts",
                host_validation_output_is_reclaimed_only_for_terminal_inactive_attributed_checkouts,
            ),
            (
                "a_reclaimed_candidate_resumes_rebuilds_with_a_fake_agent_and_completes",
                a_reclaimed_candidate_resumes_rebuilds_with_a_fake_agent_and_completes,
            ),
        ]);
    });
}
