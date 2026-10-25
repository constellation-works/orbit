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
            let host = prepared.with_provider("exit 0");

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

/// Provider stand-in for the leaf's pre-rewrite check.
///
/// Touches `launched` as soon as it is dispatched, then edits only when the
/// host pin equals `ORIG_HEAD` and the raw publication fields are still
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
if pin != orig or published != expected or (head and head != expected):
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
