//! Claimed clean-tree evidence crosses scratch import and commit verification.

use super::*;
use orbit_types::task::ExecutionLocation;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::handoff::already_landed_scope;

fn mixed_evidence(head: &str) -> (Value, Vec<(&'static str, Value)>) {
    let checks = [
        ("make test", "required", "passed", "required.json", 0),
        (
            "negative control",
            "expected_failure",
            "failed",
            "control.json",
            1,
        ),
        (
            "inspect candidate",
            "diagnostic",
            "failed",
            "diagnostic.json",
            2,
        ),
    ];
    let report = json!({
        "schema_version": 1,
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "tested_head": head,
        "covering_commit": head,
        "covering_task_id": TASK_ID,
        "scope": already_landed_scope(&fixture_task(), &[]),
        "required_commands": ["make test"],
        "validation": checks.iter().map(|(command, role, outcome, path, _)| json!({
            "command": command, "role": role, "outcome": outcome, "log_artifact": path,
        })).collect::<Vec<_>>(),
        "criteria_evidence": ["Verified criterion"],
    });
    let logs = checks
        .into_iter()
        .map(|(command, _, _, path, exit_code)| {
            (
                path,
                json!({
                    "run_id": RUN_ID, "tested_head": head, "command": command,
                    "exit_code": exit_code, "output": "captured output",
                }),
            )
        })
        .collect();
    (report, logs)
}

fn claimed_action(
    repo: &Path,
    head: &str,
    report: Value,
    logs: Vec<(&str, Value)>,
) -> (VerifierHost, Result<Value, OrbitError>) {
    let mut host = VerifierHost::new(repo, fixture_task());
    host.worker = Some(WorkerInvocation {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "ws_owner".into(),
        owner_destination: "owner/ws_owner".into(),
        task_id: TASK_ID.into(),
        claim_id: "claim-verifier".into(),
        execution: ExecutionLocation {
            machine_id: "follower".into(),
            machine_name: None,
        },
        bound_run_id: RUN_ID.into(),
    });
    let scratch = repo.join(".orbit/tmp");
    fs::write(repo.join(".git/info/exclude"), ".orbit/tmp/\n").unwrap();
    fs::create_dir_all(&scratch).unwrap();
    let mut references = Vec::new();
    for (path, content) in std::iter::once(("already-landed.json", report)).chain(logs) {
        fs::write(scratch.join(path), serde_json::to_vec(&content).unwrap()).unwrap();
        references.push(json!({"path": path, "source_path": format!(".orbit/tmp/{path}")}));
    }
    let mut input = commit_input(repo, head);
    input["implementation"] = json!({
        "execution_summary": "Outcome: success\nVerified existing delivery.",
        "no_diff_artifacts": references,
    });
    let result = action(&host, &input);
    (host, result)
}

#[test]
fn claimed_commit_accepts_required_checks_with_controls_and_diagnostics() {
    let temp = tempdir().unwrap();
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    // Diagnostics retain either exit status and any observation outcome.
    for (outcome, exit_code) in [("failed", 2), ("passed", 0), ("not_run", 0)] {
        let (mut report, mut logs) = mixed_evidence(&head);
        report["validation"][2]["outcome"] = json!(outcome);
        logs[2].1["exit_code"] = json!(exit_code);
        let (host, result) = claimed_action(temp.path(), &head, report.clone(), logs.clone());
        let result = result.expect(
            "mixed-role evidence must survive claim import and verification [F2026-10-327]",
        );
        assert_eq!(result["decision"], "verified_already_landed");
        assert_eq!(result["committed"], false);
        assert_eq!(result["already_landed"], report);
        assert_eq!(
            result["validation_provenance"],
            json!(logs.iter().map(|(_, log)| log).collect::<Vec<_>>())
        );
        assert_eq!(host.artifacts.lock().unwrap()[TASK_ID].len(), 4);
        assert_eq!(git_head(temp.path()), head);
        assert!(git_output(temp.path(), &["status", "--porcelain"]).is_empty());
    }
}

#[test]
fn claimed_commit_refuses_incomplete_or_mismatched_mixed_role_evidence() {
    type Mutation = fn(&mut Value, &mut Vec<(&'static str, Value)>);
    let cases: &[(&str, Mutation)] = &[
        ("missing required command", |r, l| {
            r["validation"].as_array_mut().unwrap().remove(0);
            l.remove(0);
        }),
        ("duplicated required command", |r, _| {
            let check = r["validation"][0].clone();
            r["validation"].as_array_mut().unwrap().push(check);
        }),
        ("failed required outcome", |r, _| {
            r["validation"][0]["outcome"] = json!("failed");
        }),
        ("required nonzero exit", |_, l| {
            l[0].1["exit_code"] = json!(1);
        }),
        ("missing required log", |_, l| {
            l.remove(0);
        }),
        ("required run mismatch", |_, l| {
            l[0].1["run_id"] = json!("another-run");
        }),
        ("required HEAD mismatch", |_, l| {
            l[0].1["tested_head"] = json!("another-head");
        }),
        ("required command mismatch", |_, l| {
            l[0].1["command"] = json!("another command");
        }),
        ("required output missing", |_, l| {
            l[0].1.as_object_mut().unwrap().remove("output");
        }),
        ("unexpected required command", |r, _| {
            r["validation"][2]["role"] = json!("required");
            r["validation"][2]["outcome"] = json!("passed");
        }),
        ("diagnostic cannot replace required", |r, _| {
            r["validation"][0]["role"] = json!("diagnostic");
        }),
        ("control exited zero", |_, l| {
            l[1].1["exit_code"] = json!(0);
        }),
        ("control lacks exit", |_, l| {
            l[1].1.as_object_mut().unwrap().remove("exit_code");
        }),
        ("control run mismatch", |_, l| {
            l[1].1["run_id"] = json!("another-run");
        }),
        ("control HEAD mismatch", |_, l| {
            l[1].1["tested_head"] = json!("another-head");
        }),
        ("diagnostic missing log", |_, l| {
            l.remove(2);
        }),
        ("diagnostic run mismatch", |_, l| {
            l[2].1["run_id"] = json!("another-run");
        }),
        ("diagnostic command mismatch", |_, l| {
            l[2].1["command"] = json!("another command");
        }),
        ("diagnostic invalid exit", |_, l| {
            l[2].1["exit_code"] = json!("unknown");
        }),
        ("excluded entry", |r, _| {
            r["validation"][2]["role"] = json!("excluded");
        }),
        ("superseded entry", |r, _| {
            r["validation"][2]["role"] = json!("superseded");
        }),
    ];
    let temp = tempdir().unwrap();
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    for (case, mutate) in cases {
        let (mut report, mut logs) = mixed_evidence(&head);
        mutate(&mut report, &mut logs);
        let (_, result) = claimed_action(temp.path(), &head, report, logs);
        let message = result.expect_err(case).to_string();
        assert!(
            message.contains("already_landed_unverified"),
            "{case} must be refused by the verifier after import, got: {message}"
        );
        assert_eq!(git_head(temp.path()), head);
    }
}
