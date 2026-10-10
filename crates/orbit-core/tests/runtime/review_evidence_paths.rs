//! Canonical evidence locators cannot alias protected review artifacts.

use orbit_core::TaskStatus;
use orbit_types::workflow::{
    REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT, ReviewEvidenceHold,
};
use serde_json::{Value, json};

use super::review_continuation::{attach_as_operator, interrupted_report};
use super::review_gate_audit::Fixture;

fn report(fixture: &Fixture, artifacts: &[&str]) -> Value {
    let mut report = interrupted_report(fixture);
    for (index, _) in artifacts.iter().enumerate() {
        let command = format!("external check {index}");
        report["validation"].as_array_mut().unwrap().push(json!({
            "id": format!("V{}", index + 2), "command": command,
            "outcome": "not_run", "role": "required",
        }));
    }
    report["external_evidence"] = json!(artifacts.iter().enumerate().map(|(index, artifact)| {
        json!({"kind": "codeql", "name": "External check", "command": format!("external check {index}"), "artifact": artifact})
    }).collect::<Vec<_>>());
    report
}

#[test]
fn reserved_aliases_and_duplicate_canonical_requirements_cannot_create_a_hold() {
    if !super::dispatch_admission::isolated(
        "review_evidence_paths::reserved_aliases_and_duplicate_canonical_requirements_cannot_create_a_hold",
    ) {
        return;
    }
    let mut cases = Vec::new();
    for reserved in [
        REVIEW_GATE_ARTIFACT,
        REVIEW_EVIDENCE_HOLD_ARTIFACT,
        REVIEW_MANIFEST_ARTIFACT,
        REVIEW_REPORT_ARTIFACT,
        REVIEW_REPORT_HISTORY_ARTIFACT,
    ] {
        for alias in [
            reserved.to_string(),
            format!(" {reserved}"),
            format!("{reserved} "),
            format!("{reserved}/"),
            format!("./{reserved}"),
        ] {
            cases.push(vec![alias]);
        }
    }
    for alias in [
        " evidence/check.json",
        "evidence/check.json ",
        "evidence/check.json/",
        "evidence//./check.json",
    ] {
        cases.push(vec!["evidence/check.json".into(), alias.into()]);
    }
    for paths in cases {
        let mut fixture = Fixture::new();
        fixture.admit();
        let paths = paths.iter().map(String::as_str).collect::<Vec<_>>();
        fixture.put_report(&report(&fixture, &paths));
        assert!(fixture.settle().is_err(), "refuse {paths:?}");
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                .unwrap()
                .is_none(),
            "ORB-14472: reserved aliases and duplicate canonical locators must not create a hold: {paths:?}",
        );
    }
}

#[test]
fn valid_requirement_and_log_aliases_use_canonical_store_keys() {
    if !super::dispatch_admission::isolated(
        "review_evidence_paths::valid_requirement_and_log_aliases_use_canonical_store_keys",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    fixture.put_report(&report(&fixture, &[" evidence//./check.json/ "]));
    assert!(fixture.settle().is_err());
    let hold: ReviewEvidenceHold = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(hold.requirements[0].artifact, "evidence/check.json");
    let evidence = json!({
        "schema_version": 1, "attempt_id": hold.attempt_id, "candidate": hold.candidate,
        "kind": "codeql", "name": "External check", "command": "external check 0",
        "outcome": "passed", "log_artifact": " evidence//./check.log.json/ ",
    });
    attach_as_operator(
        &fixture,
        "evidence/check.log.json",
        &json!({"exit_code": 0}),
    );
    attach_as_operator(&fixture, "evidence/check.json", &evidence);
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Backlog
    );
}
