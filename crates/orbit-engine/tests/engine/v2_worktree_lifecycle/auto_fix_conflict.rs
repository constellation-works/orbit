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
            write_executable(
                &provider,
                &provider_script(&format!(
                    ": > '{}'\nprintf 'candidate and target\\n' > README.md",
                    launched.display()
                )),
            );
            let host = prepared.host.with_provider(&provider);
            let original = &prepared.candidate;
            let first = pushed("42", &prepared.run_id, original, &repair);
            let second = pushed("42", &prepared.run_id, &repair, &latest);

            for (case, entries) in [
                ("unrecorded", vec![]),
                (
                    "other PR",
                    vec![pushed("43", &prepared.run_id, original, &latest)],
                ),
                (
                    "other run",
                    vec![pushed("42", "jrun-unrelated", original, &latest)],
                ),
                (
                    "disconnected head",
                    vec![pushed("42", &prepared.run_id, &repair, &latest)],
                ),
                ("only earlier repair recorded", vec![first.clone()]),
            ] {
                host.clear_history("T-REBASE");
                for entry in entries {
                    host.record_history("T-REBASE", entry);
                }
                let error = recover(&host, &prepared.run_id, input.clone())
                    .expect_err("an unauthenticated head must refuse before dispatch");
                assert!(
                    error.to_string().contains("existing rebase matching"),
                    "{case}: {error}"
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
