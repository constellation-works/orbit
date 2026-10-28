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
                "unverifiable_or_claimed_reports_record_nothing",
                unverifiable_or_claimed_reports_record_nothing,
            ),
        ]);
    });
}
