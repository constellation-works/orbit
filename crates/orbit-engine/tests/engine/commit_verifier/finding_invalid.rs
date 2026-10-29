//! An implementer's `finding-invalid.json` on a clean tree rejects its task
//! through the host and lands nothing; a report whose citations do not check
//! out at the tested HEAD, or one a claimed leaf hands off, records nothing.

use super::*;
use orbit_types::task::ExecutionLocation;
use orbit_types::tool::WorkerInvocation;

fn report(head: &str, location: &str) -> Value {
    json!({
        "schema_version": 1,
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "tested_head": head,
        "reasoning": "README.md holds one line; nothing parses it twice.",
        "evidence": [{ "location": location, "note": "The only line of the file." }],
    })
}

fn verified_report_rejects_the_finding_and_lands_nothing() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());
    host.set_artifacts(
        TASK_ID,
        vec![artifact(
            "finding-invalid.json",
            report(&head, "README.md:1"),
        )],
    );

    let result = action(&host, &commit_input(temp.path(), &head)).expect("report accepted");
    assert_eq!(result["decision"], "verified_finding_invalid", "{result}");
    assert_eq!(result["committed"], false);
    assert_eq!(result["skipped_no_diff_expected"], true);
    assert_eq!(git_head(temp.path()), head);
    let rejected = host.rejected_findings.lock().unwrap();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].evidence[0].location, "README.md:1");
}

fn directory_citation_records_nothing() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    std::fs::create_dir_all(temp.path().join("nested")).expect("create nested directory");
    std::fs::write(temp.path().join("nested/README.md"), "nested file\n")
        .expect("write nested file");
    git_output(temp.path(), &["add", "nested/README.md"]);
    git_output(temp.path(), &["commit", "-m", "add nested file"]);
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());
    host.set_artifacts(
        TASK_ID,
        vec![artifact("finding-invalid.json", report(&head, "nested:1"))],
    );

    let error = action(&host, &commit_input(temp.path(), &head))
        .expect_err("a directory citation must not verify");
    assert!(
        error.to_string().contains("finding_invalid_unverified"),
        "directory citation must be refused, got: {error}"
    );
    assert!(host.rejected_findings.lock().unwrap().is_empty());
    assert_eq!(git_head(temp.path()), head);
}

fn valid_no_diff(head: &str) -> (Value, Value) {
    let report = json!({
        "schema_version": 1,
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "tested_head": head,
        "reason": "Genuine no-op.",
        "validation": [{
            "command": "make test",
            "exit_code": 0,
            "log_artifact": "validation.json",
        }],
    });
    let log = json!({
        "run_id": RUN_ID,
        "tested_head": head,
        "command": "make test",
        "exit_code": 0,
        "output": "ok",
    });
    (report, log)
}

/// A schema-invalid report is still JSON, so its `run_id` decodes
/// leniently. `evidence` here is a string (not an array) and the extra key
/// is denied, so strict parsing fails while the named run stays readable.
fn malformed_finding_invalid(head: &str, run_id: &str) -> String {
    format!(
        "{{\"schema_version\":1,\"task_id\":\"{TASK_ID}\",\"run_id\":\"{run_id}\",\
         \"tested_head\":\"{head}\",\"reasoning\":\"stale report\",\
         \"evidence\":\"not-an-array\",\"unexpected_field\":1}}"
    )
}

/// A syntactically invalid report that still textually names `run_id`. The
/// stray trailing comma makes the bytes unparseable as JSON, so neither the
/// strict nor the lenient `Value` parse decodes `run_id`.
fn syntactically_invalid_finding_invalid(head: &str, run_id: &str) -> String {
    format!(
        "{{\"schema_version\":1,\"task_id\":\"{TASK_ID}\",\"run_id\":\"{run_id}\",\
         \"tested_head\":\"{head}\",\"reasoning\":\"stale report\",\
         \"evidence\":[{{\"location\":\"README.md:1\",\"note\":\"The only line.\"}},]}}"
    )
}

fn unparseable_report_from_another_run_does_not_shadow_no_diff() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());
    let (no_diff, log) = valid_no_diff(&head);
    host.set_artifacts(
        TASK_ID,
        vec![
            TaskArtifact::from_text(
                "finding-invalid.json",
                malformed_finding_invalid(&head, "another-run"),
            ),
            artifact("no-diff.json", no_diff),
            artifact("validation.json", log),
        ],
    );

    let result = action(&host, &commit_input(temp.path(), &head))
        .expect("a malformed report from another run must not shadow this run's no-diff evidence");
    assert_eq!(result["decision"], "verified_no_diff", "{result}");
    assert_eq!(result["committed"], false);
    assert_eq!(result["skipped_no_diff_expected"], true);
    assert_eq!(git_head(temp.path()), head);
    assert!(host.rejected_findings.lock().unwrap().is_empty());
}

fn unparseable_report_naming_this_run_still_refuses_with_parse_error() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());
    let (no_diff, log) = valid_no_diff(&head);
    host.set_artifacts(
        TASK_ID,
        vec![
            TaskArtifact::from_text(
                "finding-invalid.json",
                syntactically_invalid_finding_invalid(&head, RUN_ID),
            ),
            artifact("no-diff.json", no_diff),
            artifact("validation.json", log),
        ],
    );

    let error = action(&host, &commit_input(temp.path(), &head))
        .expect_err("a malformed report naming this run must still refuse");
    let message = error.to_string();
    assert!(
        message.contains("finding_invalid_unverified"),
        "own malformed report must refuse as finding-invalid, got: {message}"
    );
    assert!(
        message.contains("invalid finding-invalid.json"),
        "refusal must name the parse error, got: {message}"
    );
    assert!(host.rejected_findings.lock().unwrap().is_empty());
    assert_eq!(git_head(temp.path()), head);
}

fn valid_json_without_run_id_naming_this_run_does_not_shadow_no_diff() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), fixture_task());
    let (no_diff, log) = valid_no_diff(&head);
    let finding_invalid = json!({
        "note": format!("stale report mentions {RUN_ID}"),
    });
    host.set_artifacts(
        TASK_ID,
        vec![
            TaskArtifact::from_text("finding-invalid.json", finding_invalid.to_string()),
            artifact("no-diff.json", no_diff),
            artifact("validation.json", log),
        ],
    );

    let result = action(&host, &commit_input(temp.path(), &head))
        .expect("valid JSON without a run_id must not shadow this run's no-diff evidence");
    assert_eq!(result["decision"], "verified_no_diff", "{result}");
    assert_eq!(result["committed"], false);
    assert_eq!(result["skipped_no_diff_expected"], true);
    assert!(host.rejected_findings.lock().unwrap().is_empty());
    assert_eq!(git_head(temp.path()), head);
}

fn unverifiable_or_claimed_reports_record_nothing() {
    for (case, location, claimed) in [
        ("past end of file", "README.md:2", false),
        ("absent path", "src/missing.rs:1", false),
        ("escaping path", "../README.md:1", false),
        ("claimed leaf", "README.md:1", true),
    ] {
        let temp = tempdir().expect("create tempdir");
        init_git_repo(temp.path());
        let head = git_head(temp.path());
        let mut host = VerifierHost::new(temp.path(), fixture_task());
        if claimed {
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
        }
        host.set_artifacts(
            TASK_ID,
            vec![artifact("finding-invalid.json", report(&head, location))],
        );

        let error = action(&host, &commit_input(temp.path(), &head))
            .expect_err("unverifiable report must be refused");
        assert!(
            error.to_string().contains("finding_invalid_unverified"),
            "{case}: {error}"
        );
        assert!(host.rejected_findings.lock().unwrap().is_empty(), "{case}");
        assert_eq!(git_head(temp.path()), head, "{case}");
    }
}

#[test]
fn finding_invalid_contracts() {
    crate::git_fixture::isolated(module_path!(), "finding_invalid_contracts", || {
        crate::git_fixture::run_scenarios(&[
            (
                "verified_report_rejects_the_finding_and_lands_nothing",
                verified_report_rejects_the_finding_and_lands_nothing,
            ),
            (
                "directory_citation_records_nothing",
                directory_citation_records_nothing,
            ),
            (
                "unverifiable_or_claimed_reports_record_nothing",
                unverifiable_or_claimed_reports_record_nothing,
            ),
            (
                "unparseable_report_from_another_run_does_not_shadow_no_diff",
                unparseable_report_from_another_run_does_not_shadow_no_diff,
            ),
            (
                "unparseable_report_naming_this_run_still_refuses_with_parse_error",
                unparseable_report_naming_this_run_still_refuses_with_parse_error,
            ),
            (
                "valid_json_without_run_id_naming_this_run_does_not_shadow_no_diff",
                valid_json_without_run_id_naming_this_run_does_not_shadow_no_diff,
            ),
        ]);
    });
}
