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

            for (case, entries, wrong_base) in [
                ("unrecorded", vec![], false),
                (
                    "other PR",
                    vec![pushed("43", &prepared.run_id, original, &latest)],
                    false,
                ),
                (
                    "other run",
                    vec![pushed("42", "jrun-unrelated", original, &latest)],
                    false,
                ),
                (
                    "disconnected head",
                    vec![pushed("42", &prepared.run_id, &repair, &latest)],
                    false,
                ),
                ("only earlier repair recorded", vec![first.clone()], false),
                ("wrong base", vec![first.clone(), second.clone()], true),
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
if pin != orig or published != expected or head != expected:
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
