//! [ORB-25667] A reviewer whose envelope declared failure because a required
//! check was denied on its host is admitted to settlement only when its report
//! is a valid incomplete verdict for the open attempt whose sole gap is the
//! external check it names. The host answers through
//! `RuntimeHost::review_report_evidence_hold`; settlement then holds the
//! review, with the reviewer's repair, for that named evidence. A missing or
//! mismatched report, a reject, an open finding, an incomplete report that
//! names no evidence and one with a gap beyond it are refused, so the
//! reviewer step keeps its failure.

use orbit_engine::{DispatchError, ReviewReportCorrectionRequest, RuntimeHost};
use orbit_types::workflow::{REVIEW_CONTRACT_VERSION, REVIEW_EVIDENCE_HOLD_ARTIFACT};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

const CODEQL: &str = "scripts/codeql-rust-local.sh --ram 16384 codeql/rust-queries:codeql-suites/rust-security-extended.qls";
const EVIDENCE: &str = "evidence/codeql-rust-linux.json";
const OTHER_REQUIRED: &str = "make other-required-check";

/// A gated fixture whose owner requires `required`, admitted.
fn admitted(required: &[&str]) -> Fixture {
    let mut fixture = Fixture::new_with_required_commands(required);
    fixture.admit();
    fixture
}

fn ask(fixture: &Fixture) -> Option<String> {
    RuntimeHost::review_report_evidence_hold(
        &fixture.runtime,
        &ReviewReportCorrectionRequest {
            run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
            lineage_key: fixture.input["admission"]["lineage_key"]
                .as_str()
                .unwrap()
                .into(),
            attempt_id: fixture.input["admission"]["attempt_id"]
                .as_str()
                .unwrap()
                .into(),
            task_ids: vec![fixture.task_id.clone()],
            workspace_path: fixture.repo.clone(),
        },
    )
    .expect("judge the failed reviewer's report")
}

/// An incomplete report whose only gap is the denied check, named as
/// external evidence, with `findings` and `validation` as given.
fn denied_check_report(fixture: &Fixture, findings: Value, validation: Value) -> Value {
    json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete",
        "summary": "Reviewed the candidate; the required CodeQL check was denied on this host.",
        "findings": findings,
        "validation": validation,
        "external_evidence": [{
            "kind": "codeql", "name": "Rust CodeQL (Linux)", "command": CODEQL, "artifact": EVIDENCE,
        }],
        "escalation": "review_incomplete_denied_check: an authorized validator must run it",
    })
}

fn denied_only() -> Value {
    json!([
        {"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"},
        {"id": "V2", "command": CODEQL, "outcome": "not_run", "role": "required",
         "note": "denied on this host"},
    ])
}

/// The failed reviewer's report names the denied check as its only gap, and
/// its repair is uncommitted: the host admits it to settlement, which commits
/// the repair and holds the review for exactly that check.
fn a_denied_check_report_is_admitted_and_settles_into_its_evidence_hold() {
    let fixture = admitted(&[CODEQL]);
    std::fs::write(fixture.repo.join("candidate.txt"), "after\nfixed\n").unwrap();
    fixture.put_report(&denied_check_report(
        &fixture,
        json!([{
            "id": "F1", "severity": "low", "summary": "Trailing line missing",
            "paths": ["candidate.txt"], "disposition": {"kind": "repaired"},
            "change": "Added the trailing line",
        }]),
        denied_only(),
    ));

    let named = ask(&fixture).expect("an evidence-only report is admitted to settlement");
    assert!(named.contains(CODEQL), "{named}");

    match fixture.settle() {
        Err(DispatchError::ReviewEvidenceHold(hold)) => {
            assert_eq!(hold.attempt_id, fixture.input["admission"]["attempt_id"]);
            assert_eq!(hold.requirements.len(), 1, "{hold:?}");
            assert_eq!(hold.requirements[0].command, CODEQL);
        }
        other => panic!("settlement holds for the named evidence: {other:?}"),
    }
    assert!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .is_some(),
        "the hold is recorded for the owner's fulfilment route"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("candidate.txt")).unwrap(),
        "after\nfixed\n",
        "the reviewer's repair is kept"
    );
}

/// Nothing but an evidence-only report is admitted: the failed reviewer
/// keeps its failure for a missing report, one naming another attempt, a
/// reject, an open finding, an incomplete report naming no evidence, and one
/// whose records leave another required check short.
fn anything_but_an_evidence_only_report_keeps_the_reviewer_failed() {
    let fixture = admitted(&[CODEQL]);
    assert_eq!(ask(&fixture), None, "no report");

    let mut mismatched = denied_check_report(&fixture, json!([]), denied_only());
    mismatched["attempt_id"] = json!("rvw-another-attempt");
    let _ = fixture.try_put_report(&mismatched);
    assert_eq!(ask(&fixture), None, "a report for another attempt");

    let mut rejected = denied_check_report(
        &fixture,
        json!([{
            "id": "F1", "severity": "high", "summary": "The change is wrong",
            "paths": ["candidate.txt"], "disposition": {"kind": "open"},
        }]),
        denied_only(),
    );
    rejected["verdict"] = json!("reject");
    fixture.put_report(&rejected);
    assert_eq!(ask(&fixture), None, "a substantive reject");

    rejected["verdict"] = json!("incomplete");
    fixture.put_report(&rejected);
    assert_eq!(
        ask(&fixture),
        None,
        "an incomplete report with an open finding"
    );

    let mut unrelated = denied_check_report(&fixture, json!([]), denied_only());
    unrelated["external_evidence"] = json!([]);
    fixture.put_report(&unrelated);
    assert_eq!(
        ask(&fixture),
        None,
        "an incomplete report naming no evidence"
    );

    let fixture = admitted(&[CODEQL, OTHER_REQUIRED]);
    fixture.put_report(&denied_check_report(
        &fixture,
        json!([]),
        json!([
            {"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"},
            {"id": "V2", "command": CODEQL, "outcome": "not_run", "role": "required"},
            {"id": "V3", "command": OTHER_REQUIRED, "outcome": "failed", "role": "required"},
        ]),
    ));
    assert_eq!(
        ask(&fixture),
        None,
        "a failed required check beyond the named evidence"
    );
}

crate::scenario_table!(
    a_failed_reviewer_report_is_held_only_when_evidence_only_scenarios,
    a_denied_check_report_is_admitted_and_settles_into_its_evidence_hold,
    anything_but_an_evidence_only_report_keeps_the_reviewer_failed
);
