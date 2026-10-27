//! A published check repair can conflict with the base on completion's retry.

use super::*;
use orbit_types::workflow::{PR_AUTO_FIX_PUSHED_EVENT, PrAutoFixBlocker, PrAutoFixPush};

fn pushed(pr: &str, run: &str, from: &str, to: &str) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc::now(),
        by: "system".into(),
        event: PR_AUTO_FIX_PUSHED_EVENT.into(),
        note: Some(
            serde_json::to_string(&PrAutoFixPush {
                pr_number: pr.into(),
                run_id: run.into(),
                from_head_sha: from.into(),
                to_head_sha: to.into(),
                blocker: PrAutoFixBlocker::FailedChecks,
            })
            .unwrap(),
        ),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    }
}

#[test]
fn completion_conflict_follows_only_recorded_auto_fix_pushes() {
    isolated_in(
        module_path!(),
        "completion_conflict_follows_only_recorded_auto_fix_pushes",
        || {
            let prepared = PreparedRebase::new("jrun-auto-fix-conflict", "README.md", "README.md");
            let checkout = &prepared.checkout.path;
            git(checkout, &["push", "origin", "HEAD"]);
            let repair = commit_file(checkout, "repair.txt", "first check repair\n");
            git(checkout, &["push", "origin", "HEAD"]);
            let latest = commit_file(checkout, "second-repair.txt", "second check repair\n");
            git(checkout, &["push", "origin", "HEAD"]);

            // Completion has adopted the published repairs, but its failed-step
            // input still carries the head originally published by pr_open.
            let mut current = prepared.prepared.clone();
            current["head_sha"] = json!(latest);
            let OrbitError::RecoverableVcsConflict(conflict) = prepared
                .rebase_on(&prepared.host, &current)
                .expect_err("completion's repaired branch conflicts with its advanced base")
            else {
                panic!("expected a recoverable conflict");
            };
            let conflict = json!({
                "operation": conflict.operation,
                "original_base_sha": conflict.original_base_sha,
                "target_base_sha": conflict.target_base_sha,
                "conflicting_paths": conflict.conflicting_paths,
            });
            assert_eq!(
                git(checkout, &["branch", "--show-current"]),
                "",
                "the incident stops with detached HEAD"
            );
            let mut input = conflict_recovery_input(&prepared, &conflict);
            input["failed_step_id"] = json!("complete_pr");
            input["activity_name"] = json!("pr_complete");
            input["failed_step_input"]["completion"] = json!("done");
            input["failed_step_input"]["pr_number"] = json!("42");
            input["failed_step_input"]["published_head_sha"] = json!(prepared.candidate);
            let provider = prepared.fixture.root.path().join("codex");
            let launched = prepared.fixture.root.path().join("launched");
            // The stub applies the leaf's ORIG_HEAD rule: edit only when the
            // host pin matches the stopped rebase, and the raw publication
            // fields are still the original candidate.
            write_executable(
                &provider,
                &publication_pin_provider(&launched, &prepared.candidate),
            );
            let host = prepared.host.with_provider(&provider);
            let original = &prepared.candidate;
            let first = pushed("42", &prepared.run_id, original, &repair);
            let second = pushed("42", &prepared.run_id, &repair, &latest);
            assert_ne!(
                prepared.base_sha, prepared.target,
                "the wrong-base case needs a pin distinct from the stopped rebase"
            );

            for (case, entries, wrong_base, expected_reason) in [
                ("unrecorded", vec![], false, "expected original HEAD"),
                (
                    "other PR",
                    vec![pushed("43", &prepared.run_id, original, &latest)],
                    false,
                    "expected original HEAD",
                ),
                (
                    "other run",
                    vec![pushed("42", "jrun-unrelated", original, &latest)],
                    false,
                    "expected original HEAD",
                ),
                (
                    "disconnected head",
                    vec![pushed("42", &prepared.run_id, &repair, &latest)],
                    false,
                    "expected original HEAD",
                ),
                (
                    "only earlier repair recorded",
                    vec![first.clone()],
                    false,
                    "expected original HEAD",
                ),
                (
                    "wrong base",
                    vec![first.clone(), second.clone()],
                    true,
                    "prepared base_sha does not match target_base_sha",
                ),
            ] {
                host.clear_history("T-REBASE");
                for entry in entries {
                    host.record_history("T-REBASE", entry);
                }
                let mut case_input = input.clone();
                if wrong_base {
                    case_input["target_base_sha"] = json!(prepared.base_sha);
                }
                let error = recover(&host, &prepared.run_id, case_input)
                    .expect_err("an unauthenticated head must refuse before dispatch");
                assert!(
                    error.to_string().contains("existing rebase matching"),
                    "{case}: {error}"
                );
                assert!(
                    error.to_string().contains(expected_reason),
                    "{case}: expected '{expected_reason}' in '{error}'"
                );
                assert!(!launched.exists(), "{case}: no provider dispatch");
                assert!(
                    rebase_in_progress(checkout),
                    "{case}: preserve stopped rebase"
                );
                assert!(host.checkpoints().is_empty(), "{case}: no certificate");
            }

            host.clear_history("T-REBASE");
            host.record_history("T-REBASE", first);
            host.record_history("T-REBASE", second);
            let outcome = recover(&host, &prepared.run_id, input)
                .expect("completion recovery admits the host-recorded auto-fix head");
            assert!(outcome.success, "{:?}", outcome.message);
            assert!(launched.exists());
            assert!(!rebase_in_progress(checkout));
            assert!(is_ancestor(checkout, &prepared.target, "HEAD"));
            assert_eq!(
                git(checkout, &["show", "HEAD:repair.txt"]),
                "first check repair"
            );
            assert_eq!(
                git(checkout, &["show", "HEAD:second-repair.txt"]),
                "second check repair"
            );
            let checkpoints = host.checkpoints();
            assert_eq!(checkpoints.len(), 1);
            assert_eq!(checkpoints[0].1, "complete_pr");
            assert_eq!(checkpoints[0].2["head_sha_before"], latest);
            assert_eq!(checkpoints[0].2["remote_sha_before"], latest);
            assert_eq!(git(checkout, &["status", "--porcelain"]), "");
        },
    );
}

#[test]
fn before_landing_push_conflict_at_complete_pr_recovers_only_recorded_head() {
    isolated_in(
        module_path!(),
        "before_landing_push_conflict_at_complete_pr_recovers_only_recorded_head",
        || {
            let prepared =
                PreparedRebase::new("jrun-landing-push-conflict", "README.md", "README.md");
            let checkout = &prepared.checkout.path;
            let branch = prepared.prepared["head"].as_str().unwrap().to_string();
            git(checkout, &["push", "origin", "HEAD"]);
            let reviewed = commit_file(checkout, "review.txt", "before-landing review fix\n");

            let provider = prepared.fixture.root.path().join("codex");
            let launched = prepared.fixture.root.path().join("launched");
            write_executable(
                &provider,
                &publication_pin_provider(&launched, &prepared.candidate),
            );
            let host = prepared.host.with_provider(&provider);
            host.set_status("T-REBASE", TaskStatus::Review);
            host.set_pr_status(json!({
                "pull_request": {
                    "state": "OPEN",
                    "mergeStateStatus": "DIRTY",
                    "isDraft": false,
                    "headRefName": branch,
                    "headRefOid": reviewed,
                    "baseRefName": BASE,
                }
            }));

            let landing_push_input = json!({
                "branch": branch,
                "workspace_path": checkout,
                "job_run_id": prepared.run_id,
                "completed_task_ids": ["T-REBASE"],
                "lease_remote_sha": prepared.candidate,
                "pr_number": "42",
            });
            let pushed_out =
                action(&host, "git_push", &landing_push_input).expect("landing_push succeeds");
            assert_eq!(pushed_out["decision"], "performed_fast_forward");
            assert_eq!(pushed_out["local_sha"], reviewed);
            let recorded_pushes =
                orbit_types::workflow::pr_auto_fix_pushes(&host.history("T-REBASE"), "42");
            assert_eq!(recorded_pushes.len(), 1);
            assert_eq!(recorded_pushes[0].from_head_sha, prepared.candidate);
            assert_eq!(recorded_pushes[0].to_head_sha, reviewed);
            assert_eq!(recorded_pushes[0].blocker, PrAutoFixBlocker::LandingReview);

            let complete_input = json!({
                "job_run_id": prepared.run_id,
                "completed_task_ids": ["T-REBASE"],
                "workspace_path": checkout,
                "pr_number": "42",
                "completion": "done",
                "auto_fix": false,
                "head": branch,
                "published_head_sha": prepared.candidate,
                "landing_reviewed_head_sha": reviewed,
                "re_review_on_conflict": true,
                "base": BASE,
                "base_sync": "local",
                "max_wait_seconds": 0,
            });
            let err = action(&host, "pr_complete", &complete_input)
                .expect_err("complete_pr hits a rebase conflict on the advanced base");
            let OrbitError::RecoverableVcsConflict(conflict) = err else {
                panic!("expected RecoverableVcsConflict from pr_complete, got: {err}");
            };
            assert!(rebase_in_progress(checkout));

            // Re-rendered complete_pr input carries the initial publication in
            // published_head_sha and the post-review head in landing_reviewed_head_sha.
            let recovery_input = json!({
                "prompt": "resolve the stopped rebase",
                "task_id": "T-REBASE",
                "task_ids": ["T-REBASE"],
                "workspace_path": checkout,
                "repo_root": checkout,
                "run_id": prepared.run_id,
                "failed_step_id": "complete_pr",
                "activity_name": "pr_complete",
                "recovery_kind": "vcs_conflict",
                "operation": conflict.operation,
                "original_base_sha": conflict.original_base_sha,
                "target_base_sha": conflict.target_base_sha,
                "conflicting_paths": conflict.conflicting_paths,
                "failed_step_input": complete_input,
            });

            // Without the host-recorded landing_push entry, or when a rendered
            // input names an unrecorded head (even in landing_reviewed_head_sha
            // or authenticated_published_head_sha), admission refuses with the
            // head-mismatch reason (preserving ORB-17737).
            let saved_history = host.history("T-REBASE");
            host.clear_history("T-REBASE");
            let mut forged = recovery_input.clone();
            forged["failed_step_input"]["authenticated_published_head_sha"] = json!(reviewed);
            let error = recover(&host, &prepared.run_id, forged)
                .expect_err("unrecorded post-review head must be refused before provider dispatch");
            let expected_mismatch = format!(
                "branch 'refs/heads/{branch}' is at '{reviewed}', expected original HEAD '{}'",
                prepared.candidate
            );
            assert!(
                error.to_string().contains(&expected_mismatch),
                "expected '{expected_mismatch}' in '{error}'"
            );
            assert!(!launched.exists(), "no provider dispatch on refusal");
            assert!(rebase_in_progress(checkout), "stopped rebase preserved");

            // With the landing_push transition recorded on the task history,
            // pr_conflict_recovery is admitted and completes, and complete_pr
            // adopts the certified rewrite on retry.
            for entry in saved_history {
                host.record_history("T-REBASE", entry);
            }
            let outcome = recover(&host, &prepared.run_id, recovery_input)
                .expect("before-landing push head is admitted and recovered");
            assert!(outcome.success, "{:?}", outcome.message);
            assert!(launched.exists());
            assert!(!rebase_in_progress(checkout));
            assert!(is_ancestor(checkout, &prepared.target, "HEAD"));
            assert_eq!(
                git(checkout, &["show", "HEAD:review.txt"]),
                "before-landing review fix"
            );
            assert_eq!(
                git(checkout, &["show", "HEAD:README.md"]),
                "candidate and target"
            );
            let checkpoints = host.checkpoints();
            assert_eq!(checkpoints.len(), 1);
            assert_eq!(checkpoints[0].1, "complete_pr");
            assert_eq!(checkpoints[0].2["head_sha_before"], reviewed);
            assert_eq!(checkpoints[0].2["remote_sha_before"], reviewed);

            let completed = action(&host, "pr_complete", &complete_input)
                .expect("complete_pr adopts the certified recovery rewrite");
            let rebased_head = head(checkout);
            assert_eq!(completed["re_review_required"], true);
            assert_eq!(completed["rebased"]["head_sha_before"], reviewed);
            assert_eq!(completed["rebased"]["remote_sha_before"], reviewed);
            assert_eq!(completed["rebased"]["head_sha"], rebased_head);

            let re_pushed = action(
                &host,
                "git_push",
                &json!({
                    "job_run_id": prepared.run_id,
                    "completed_task_ids": ["T-REBASE"],
                    "workspace_path": checkout,
                    "branch": completed["rebased"]["head"],
                    "base": completed["rebased"]["base"],
                    "base_ref": completed["rebased"]["base_ref"],
                    "rewrite_performed": completed["rebased"]["rewritten"],
                    "rewrite_head_before": completed["rebased"]["head_sha_before"],
                    "expected_remote_sha": completed["rebased"]["remote_sha_before"],
                }),
            )
            .expect("re_push publishes the conflict-repaired head over the before-landing head");
            assert_eq!(re_pushed["decision"], "performed_force_with_lease");
            assert_eq!(re_pushed["local_sha"], rebased_head);
            assert_eq!(re_pushed["remote_sha_before"], reviewed);
        },
    );
}

#[test]
fn authorize_rebase_completion_reports_distinct_refusal_reasons() {
    isolated_in(
        module_path!(),
        "authorize_rebase_completion_reports_distinct_refusal_reasons",
        || {
            let prepared = PreparedRebase::new("jrun-refusal-reasons", "README.md", "README.md");
            let checkout = &prepared.checkout.path;
            let conflict = stopped_conflict(&prepared);
            let input = conflict_recovery_input(&prepared, &conflict);
            let launched = prepared
                .fixture
                .root
                .path()
                .join("refusal-provider-launched");
            let host = prepared.with_provider(&format!(": > '{}'", launched.display()));
            let original_status = git(checkout, &["status", "--porcelain"]);
            let original_file = fs::read(checkout.join("README.md")).unwrap();

            // 1. Input validation and metadata mismatches while the rebase is stopped.
            let mut bad_kind = input.clone();
            bad_kind["recovery_kind"] = json!("other");
            let err = recover(&host, &prepared.run_id, bad_kind).unwrap_err();
            assert!(
                err.to_string()
                    .contains("recovery input does not name a git_rebase vcs_conflict"),
                "{err}"
            );

            let mut bad_step = input.clone();
            bad_step["failed_step_id"] = json!("wrong_step");
            let err = recover(&host, &prepared.run_id, bad_step).unwrap_err();
            assert!(
                err.to_string().contains(
                    "failed_step_id and activity_name do not name a supported rebase step"
                ),
                "{err}"
            );

            let mut bad_merge_base = input.clone();
            bad_merge_base["original_base_sha"] = json!(prepared.target);
            let err = recover(&host, &prepared.run_id, bad_merge_base).unwrap_err();
            assert!(err.to_string().contains("merge-base"), "{err}");

            let mut bad_paths = input.clone();
            bad_paths["conflicting_paths"] = json!(["OTHER.md"]);
            let err = recover(&host, &prepared.run_id, bad_paths).unwrap_err();
            assert!(
                err.to_string()
                    .contains("worktree unmerged paths do not match conflicting_paths"),
                "{err}"
            );

            // 2. Rebase state metadata mismatches (orig-head and onto).
            let rebase_dir = PathBuf::from(git(
                checkout,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "rebase-merge",
                ],
            ));
            let orig_head_path = rebase_dir.join("orig-head");
            let saved_orig_head = fs::read_to_string(&orig_head_path).unwrap();
            fs::write(&orig_head_path, format!("{}\n", prepared.base_sha)).unwrap();
            let err = recover(&host, &prepared.run_id, input.clone()).unwrap_err();
            assert!(
                err.to_string().contains("rebase orig-head")
                    && err.to_string().contains("does not match original HEAD"),
                "{err}"
            );
            fs::write(&orig_head_path, &saved_orig_head).unwrap();

            let onto_path = rebase_dir.join("onto");
            let saved_onto = fs::read_to_string(&onto_path).unwrap();
            fs::write(&onto_path, format!("{}\n", prepared.base_sha)).unwrap();
            let err = recover(&host, &prepared.run_id, input.clone()).unwrap_err();
            assert!(
                err.to_string().contains("rebase onto")
                    && err.to_string().contains("does not match target base"),
                "{err}"
            );
            fs::write(&onto_path, &saved_onto).unwrap();

            // Missing and mismatched retained metadata must fail before dispatch.
            for name in ["head-name", "orig-head", "onto"] {
                let path = rebase_dir.join(name);
                let saved = fs::read(&path).unwrap();
                for replacement in [Some(b"foreign\n".as_slice()), None] {
                    if let Some(value) = replacement {
                        fs::write(&path, value).unwrap();
                    } else {
                        fs::remove_file(&path).unwrap();
                    }
                    let error = recover(&host, &prepared.run_id, input.clone())
                        .expect_err("invalid retained metadata must refuse");
                    assert!(error.to_string().contains("rebase"), "{name}: {error}");
                    assert!(!launched.exists(), "{name}: no provider dispatch");
                    assert_eq!(git(checkout, &["status", "--porcelain"]), original_status);
                    assert_eq!(fs::read(checkout.join("README.md")).unwrap(), original_file);
                    assert!(
                        host.checkpoints().is_empty(),
                        "{name}: no success checkpoint"
                    );
                    fs::write(&path, &saved).unwrap();
                }
            }

            // 3. Missing rebase directory reason when neither rebase-merge nor rebase-apply exists.
            fs::remove_dir_all(&rebase_dir).unwrap();
            let err = recover(&host, &prepared.run_id, input).unwrap_err();
            assert!(
                err.to_string()
                    .contains("no rebase-merge or rebase-apply directory in worktree"),
                "{err}"
            );
        },
    );
}

/// Preserved recovery refusals, exercised at the public `recover` boundary.
///
/// Host admission recomputes workspace, branch, head, publication and
/// completion authority and refuses before the provider is dispatched. A
/// caller-supplied publication pin cannot replace that head. The substitute
/// leaf refuses a terminal task before it edits. Administrative neighbours
/// stay on their existing contracts:
/// - stale checkpoint or evidence: `a_resumed_run_recovers_the_same_step_again_as_a_new_attempt`
///   refuses "no exact host-validated recovery checkpoint", and
///   `absorbed_candidate` refuses `stale_checkpoint` for another attempt, run,
///   step or task;
/// - re-authored scope: `absorbed_candidate` "a task whose criteria changed
///   since recovery" refuses `task_scope_changed`;
/// - wrong ownership or revoked authorization:
///   `earlier_repairs_are_not_continued_once_ownership_or_authorization_changed`;
/// - preparation stale or foreign workspace, run and candidate:
///   `direct_stale_and_invalid_preparations_refuse_without_mutation`;
/// - wrong or missing head-name, orig-head, onto, and a missing rebase:
///   `authorize_rebase_completion_reports_distinct_refusal_reasons`;
/// - unrecorded or unrelated pushes and a wrong base:
///   `completion_conflict_follows_only_recorded_auto_fix_pushes`.
#[test]
fn preserved_recovery_refusals_leave_no_edit_or_checkpoint() {
    isolated_in(
        module_path!(),
        "preserved_recovery_refusals_leave_no_edit_or_checkpoint",
        || {
            let prepared = PreparedRebase::new("jrun-preserved-refusals", "README.md", "README.md");
            let checkout = &prepared.checkout.path;
            let conflict = stopped_conflict(&prepared);
            let input = conflict_recovery_input(&prepared, &conflict);
            let launched = prepared.fixture.root.path().join("preserved-launched");
            let host = prepared.with_provider(&format!(": > '{}'", launched.display()));
            let readme = fs::read(checkout.join("README.md")).unwrap();

            let refuse = |case: &str, case_input: Value, needle: &str| {
                let _ = fs::remove_file(&launched);
                let before = git(checkout, &["status", "--porcelain"]);
                let error = recover(&host, &prepared.run_id, case_input).expect_err(case);
                assert!(error.to_string().contains(needle), "{case}: {error}");
                assert!(!launched.exists(), "{case}: provider must not run");
                assert_eq!(git(checkout, &["status", "--porcelain"]), before, "{case}");
                assert_eq!(
                    fs::read(checkout.join("README.md")).unwrap(),
                    readme,
                    "{case}"
                );
                assert!(
                    host.checkpoints().is_empty(),
                    "{case}: no success checkpoint"
                );
                assert!(
                    rebase_in_progress(checkout),
                    "{case}: stopped rebase preserved"
                );
            };

            let mut different_roots = input.clone();
            different_roots["repo_root"] = json!(prepared.fixture.repo);
            refuse(
                "workspace and repo root are different checkouts",
                different_roots,
                "different Git checkouts",
            );

            let mut primary = input.clone();
            primary["workspace_path"] = json!(prepared.fixture.repo);
            primary["repo_root"] = json!(prepared.fixture.repo);
            refuse(
                "workspace collapses to the registered primary",
                primary,
                "registered primary checkout",
            );

            let nested = checkout.join("nested");
            fs::create_dir_all(&nested).unwrap();
            let mut not_root = input.clone();
            not_root["workspace_path"] = json!(&nested);
            not_root["repo_root"] = json!(&nested);
            refuse(
                "workspace is not the checkout root",
                not_root,
                "exact Git checkout root",
            );
            fs::remove_dir_all(&nested).unwrap();

            let mut absent_head = input.clone();
            let failed = absent_head["failed_step_input"].as_object_mut().unwrap();
            failed.remove("head_sha");
            failed.remove("published_head_sha");
            failed.insert(
                "authenticated_published_head_sha".to_string(),
                json!(prepared.candidate),
            );
            refuse(
                "a pin cannot stand in for the prepared head",
                absent_head,
                "missing prepared head_sha or published_head_sha",
            );

            let mut forged_overrides_head = input.clone();
            forged_overrides_head["failed_step_input"]["head_sha"] = json!(prepared.base_sha);
            forged_overrides_head["failed_step_input"]["authenticated_published_head_sha"] =
                json!(prepared.candidate);
            refuse(
                "a pin cannot override the prepared head",
                forged_overrides_head,
                "expected original HEAD",
            );

            let mut auto_fix_without_completion = input.clone();
            auto_fix_without_completion["failed_step_id"] = json!("complete_pr");
            auto_fix_without_completion["activity_name"] = json!("pr_complete");
            auto_fix_without_completion["failed_step_input"]["auto_fix"] = json!(true);
            auto_fix_without_completion["failed_step_input"]["pr_number"] = json!("42");
            auto_fix_without_completion["failed_step_input"]["published_head_sha"] =
                json!(prepared.candidate);
            refuse(
                "auto_fix does not replace completion authorization",
                auto_fix_without_completion,
                "completion == 'done'",
            );

            let mut missing_pr = input.clone();
            missing_pr["failed_step_id"] = json!("complete_pr");
            missing_pr["activity_name"] = json!("pr_complete");
            missing_pr["failed_step_input"]["completion"] = json!("done");
            missing_pr["failed_step_input"]["published_head_sha"] = json!(prepared.candidate);
            refuse(
                "completion recovery without a pull request",
                missing_pr,
                "missing prepared pr_number",
            );

            let mut duplicate_paths = input.clone();
            duplicate_paths["conflicting_paths"] = json!(["README.md", "README.md"]);
            refuse(
                "duplicate conflict paths",
                duplicate_paths,
                "duplicate entries",
            );

            let mut escaped_path = input.clone();
            escaped_path["conflicting_paths"] = json!(["../README.md"]);
            refuse(
                "conflict path outside the checkout",
                escaped_path,
                "not a safe relative path",
            );

            let mut empty_paths = input.clone();
            empty_paths["conflicting_paths"] = json!([]);
            refuse(
                "empty conflict set",
                empty_paths,
                "conflicting_paths is empty",
            );

            let attached =
                PreparedRebase::new("jrun-attached-wrong-branch", "README.md", "README.md");
            let attached_checkout = &attached.checkout.path;
            let attached_conflict = stopped_conflict(&attached);
            let attached_input = conflict_recovery_input(&attached, &attached_conflict);
            let attached_launched = attached.fixture.root.path().join("attached-launched");
            let attached_host =
                attached.with_provider(&format!(": > '{}'", attached_launched.display()));
            let attached_readme = fs::read(attached_checkout.join("README.md")).unwrap();
            git(
                attached_checkout,
                &["symbolic-ref", "HEAD", &format!("refs/heads/{BASE}")],
            );
            let attached_before = git(attached_checkout, &["status", "--porcelain"]);
            let attached_error = recover(&attached_host, &attached.run_id, attached_input)
                .expect_err("an ordinary checked-out wrong branch is still rejected");
            assert!(
                attached_error
                    .to_string()
                    .contains("stopped rebase HEAD does not descend from target base"),
                "{attached_error}"
            );
            assert!(
                !attached_launched.exists(),
                "attached wrong branch: provider must not run"
            );
            assert_eq!(
                git(attached_checkout, &["status", "--porcelain"]),
                attached_before
            );
            assert_eq!(
                fs::read(attached_checkout.join("README.md")).unwrap(),
                attached_readme
            );
            assert!(attached_host.checkpoints().is_empty());
            assert!(rebase_in_progress(attached_checkout));

            let mut terminal = prepared;
            terminal.host.publish_task_context = true;
            let terminal_launched = terminal.fixture.root.path().join("terminal-launched");
            let provider = terminal.fixture.root.path().join("codex-terminal");
            write_executable(&provider, &terminal_task_provider(&terminal_launched));
            let terminal_readme = fs::read(terminal.checkout.path.join("README.md")).unwrap();
            let terminal_conflict = conflict;
            let terminal_input = conflict_recovery_input(&terminal, &terminal_conflict);
            for status in [TaskStatus::Done, TaskStatus::Archived, TaskStatus::Rejected] {
                let _ = fs::remove_file(&terminal_launched);
                terminal.host.set_status("T-REBASE", status);
                let host = terminal.host.with_provider(&provider);
                let before = git(&terminal.checkout.path, &["status", "--porcelain"]);
                let outcome = recover(&host, &terminal.run_id, terminal_input.clone())
                    .expect("a terminal refusal still finishes the invocation");
                let case = status.cli_name();
                assert!(!outcome.success, "{case}: {:?}", outcome.message);
                let message = outcome.message.unwrap_or_default();
                assert!(
                    message.contains("status=\"failed\"")
                        && message.contains("task_refuses_writes"),
                    "{case}: {message}"
                );
                assert!(terminal_launched.exists(), "{case}: leaf ran");
                assert_eq!(
                    git(&terminal.checkout.path, &["status", "--porcelain"]),
                    before,
                    "{case}"
                );
                assert_eq!(
                    fs::read(terminal.checkout.path.join("README.md")).unwrap(),
                    terminal_readme,
                    "{case}: no provider edit"
                );
                assert!(
                    host.checkpoints().is_empty(),
                    "{case}: no success checkpoint"
                );
                assert!(rebase_in_progress(&terminal.checkout.path), "{case}");
            }

            let forged = PreparedRebase::new("jrun-forged-pin", "README.md", "README.md");
            let forged_conflict = stopped_conflict(&forged);
            let mut forged_input = conflict_recovery_input(&forged, &forged_conflict);
            forged_input["failed_step_input"]["authenticated_published_head_sha"] =
                json!(forged.base_sha);
            let seen_pin = forged.fixture.root.path().join("seen-pin");
            let forged_launched = forged.fixture.root.path().join("forged-launched");
            let forged_provider = forged.fixture.root.path().join("codex-pin");
            write_executable(
                &forged_provider,
                &host_authenticated_pin_provider(&forged_launched, &seen_pin),
            );
            let forged_host = forged.host.with_provider(&forged_provider);
            let outcome = recover(&forged_host, &forged.run_id, forged_input)
                .expect("the host replaces a mismatched publication pin");
            assert!(outcome.success, "{:?}", outcome.message);
            assert_eq!(
                fs::read_to_string(&seen_pin).unwrap().trim(),
                forged.candidate,
                "the leaf must see the host pin, not the forged one"
            );
            assert!(forged_launched.exists());
            assert!(!rebase_in_progress(&forged.checkout.path));
            assert_eq!(forged_host.checkpoints().len(), 1);
        },
    );
}

/// Substitute leaf that stops before editing when the envelope says the task
/// refuses writes. A missing or non-terminal task falls through to an edit so
/// the contract fails closed in the fixture.
fn terminal_task_provider(launched: &std::path::Path) -> String {
    format!(
        r#"#!/bin/sh
set -eu
: > '{launched}'
python3 -c '
import json, sys
payload = sys.stdin.read()
marker = "Execution envelope:\n"
start = payload.find(marker)
text = payload[start + len(marker):] if start >= 0 else payload
try:
    env = json.loads(text)
except json.JSONDecodeError:
    env = {{}}
task = env.get("task") if isinstance(env.get("task"), dict) else {{}}
status = task.get("status") or ""
if task.get("terminal") is True or status in ("done", "archived", "rejected"):
    json.dump({{
        "schemaVersion": 1,
        "status": "failed",
        "result": {{}},
        "error": {{"code": "task_refuses_writes", "message": "the task refuses writes"}},
    }}, sys.stdout)
    raise SystemExit(0)
with open("README.md", "w", encoding="utf-8") as handle:
    handle.write("should not edit\n")
json.dump({{"schemaVersion": 1, "status": "success", "result": {{}}, "error": None}}, sys.stdout)
'
"#,
        launched = launched.display(),
    )
}

/// Substitute leaf that edits only when the host-injected publication pin
/// equals `ORIG_HEAD`. It records the pin it actually saw.
fn host_authenticated_pin_provider(
    launched: &std::path::Path,
    seen_pin: &std::path::Path,
) -> String {
    format!(
        r#"#!/bin/sh
set -eu
: > '{launched}'
python3 -c '
import json, subprocess, sys
payload = sys.stdin.read()
marker = "Execution envelope:\n"
start = payload.find(marker)
text = payload[start + len(marker):] if start >= 0 else payload
try:
    env = json.loads(text)
except json.JSONDecodeError:
    env = {{}}
failed = env.get("input", {{}}).get("failed_step_input", {{}})
if not isinstance(failed, dict):
    failed = {{}}
pin = failed.get("authenticated_published_head_sha") or ""
orig = subprocess.check_output(["git", "rev-parse", "ORIG_HEAD"], text=True).strip()
with open("{seen_pin}", "w", encoding="utf-8") as handle:
    handle.write(pin)
if pin != orig:
    json.dump({{
        "schemaVersion": 1,
        "status": "failed",
        "result": {{}},
        "error": {{
            "code": "pre_rewrite_head_mismatch",
            "message": "ORIG_HEAD is not the host-authenticated publication",
        }},
    }}, sys.stdout)
    raise SystemExit(0)
with open("README.md", "w", encoding="utf-8") as handle:
    handle.write("candidate and target\n")
json.dump({{"schemaVersion": 1, "status": "success", "result": {{}}, "error": None}}, sys.stdout)
'
"#,
        launched = launched.display(),
        seen_pin = seen_pin.display(),
    )
}

/// Provider stand-in for the leaf's pre-rewrite check.
///
/// Touches `launched` as soon as it is dispatched, then edits only when the
/// retained rebase branch matches and the host pin equals `ORIG_HEAD` and the raw publication fields are still
/// `original_publication`. A missing pin refuses before any edit.
fn publication_pin_provider(launched: &std::path::Path, original_publication: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
: > '{launched}'
python3 -c '
import json, subprocess, sys
payload = sys.stdin.read()
marker = "Execution envelope:\n"
start = payload.find(marker)
text = payload[start + len(marker):] if start >= 0 else payload
try:
    env = json.loads(text)
except json.JSONDecodeError:
    env = {{}}
failed = env.get("input", {{}}).get("failed_step_input", {{}})
if not isinstance(failed, dict):
    failed = {{}}
pin = failed.get("authenticated_published_head_sha") or ""
published = failed.get("published_head_sha") or ""
head = failed.get("head_sha") or ""
orig = subprocess.check_output(["git", "rev-parse", "ORIG_HEAD"], text=True).strip()
expected = sys.argv[1]
branch = subprocess.check_output(["git", "branch", "--show-current"], text=True).strip()
expected_branch = failed.get("head") or ""
if not branch:
    for backend in ("rebase-merge", "rebase-apply"):
        path = subprocess.check_output(["git", "rev-parse", "--git-path", backend], text=True).strip()
        try:
            with open(path + "/head-name", encoding="utf-8") as handle:
                branch = handle.read().strip()
            branch = branch.removeprefix("refs/heads/") if branch.startswith("refs/heads/") else ""
            break
        except FileNotFoundError:
            pass
if not expected_branch or branch != expected_branch or pin != orig or published != expected or (head and head != expected):
    json.dump({{
        "schemaVersion": 1,
        "status": "failed",
        "result": {{}},
        "error": {{
            "code": "pre_rewrite_head_mismatch",
            "message": "ORIG_HEAD is not the host-authenticated publication",
        }},
    }}, sys.stdout)
    raise SystemExit(0)
with open("README.md", "w", encoding="utf-8") as handle:
    handle.write("candidate and target\n")
json.dump({{
    "schemaVersion": 1,
    "status": "success",
    "result": {{}},
    "error": None,
}}, sys.stdout)
' '{original_publication}'
"#,
        launched = launched.display(),
        original_publication = original_publication,
    )
}
