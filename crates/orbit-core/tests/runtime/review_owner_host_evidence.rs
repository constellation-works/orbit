//! Owner landing settlement must reach the host replay, just as a claimed
//! settlement does, without accepting sandbox deferrals as execution.

use std::os::unix::fs::PermissionsExt;

use orbit_core::TaskStatus;
use orbit_types::workflow::{HostEvidenceReason, REVIEW_GATE_ARTIFACT, ReviewCertificate};
use serde_json::{Value, json};

use super::review_before_landing::published;
use super::review_gate_audit::Fixture;

const COMMAND: &str = "cargo test -p orbit-exec --test probe confined::probe";
const BROAD: &str = "cargo test -p orbit-exec --test probe confined";
const ARTIFACT: &str = "evidence/owner-host.json";
const LOG: &str = "evidence/owner-host.log.json";
const PASS: &str = "echo \"OWNER_HOST_RAN $(git rev-parse HEAD)\"\n\
    echo 'test confined::probe ... ok'\n\
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'\n";

fn fixture(probe: &str) -> (Fixture, tempfile::TempDir) {
    let bin = tempfile::TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let cargo = bin.path().join("cargo");
    std::fs::write(&cargo, "#!/bin/sh\nexec sh tests/host/probe.sh\n").unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut fixture = Fixture::before_landing_with_config(&format!(
        "[workflow.validation_env]\npath = [\"{}\"]\n",
        bin.path().display()
    ));
    std::fs::create_dir_all(fixture.repo.join("tests/host")).unwrap();
    std::fs::write(fixture.repo.join("tests/host/probe.sh"), probe).unwrap();
    for args in [vec!["add", "tests"], vec!["commit", "-qm", "host probe"]] {
        let output = crate::git_test_support::command()
            .args(&args)
            .current_dir(&fixture.repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    published(&fixture);
    fixture.input["before_landing"] = json!(true);
    fixture.admit();
    (fixture, bin)
}

fn report(fixture: &Fixture) -> Value {
    json!({
        "schema_version": 1, "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete", "summary": "Only the nested Bubblewrap test is missing.",
        "findings": [],
        "validation": [
            {"id": "V1", "command": BROAD, "outcome": "passed", "role": "required",
             "deferred": ["DEFERRED: bubblewrap unavailable: confined::probe: bwrap: No permissions to create a new namespace"]},
            {"id": "V2", "command": COMMAND, "outcome": "not_run", "role": "required"},
        ],
        "external_evidence": [{
            "kind": "host_sandbox_test", "name": "Confined probe", "command": COMMAND,
            "artifact": ARTIFACT, "os": "linux",
        }],
        "escalation": "The agent sandbox refuses nested Bubblewrap namespaces.",
    })
}

fn certificate(fixture: &Fixture) -> ReviewCertificate {
    serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap()
}

#[test]
fn an_owner_landing_review_replays_its_bubblewrap_deferred_test() {
    if !super::dispatch_admission::isolated(
        "review_owner_host_evidence::an_owner_landing_review_replays_its_bubblewrap_deferred_test",
    ) {
        return;
    }
    let (fixture, _bin) = fixture(PASS);
    fixture.put_report(&report(&fixture));
    let settled = fixture
        .settle()
        .expect("owner landing settlement runs the admitted host test");
    assert_eq!(settled["gate"], "passed", "{settled}");
    let certificate = certificate(&fixture);
    assert!(certificate.validation_complete);
    let [ran] = certificate.host_evidence.as_slice() else {
        panic!("one host replay: {:?}", certificate.host_evidence);
    };
    assert!(ran.passed, "{ran:?}");
    assert_eq!(ran.command, COMMAND);
    let log: Value = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, LOG)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert!(
        log["output"]
            .as_str()
            .unwrap()
            .lines()
            .any(|line| line == format!("OWNER_HOST_RAN {}", certificate.final_candidate.commit)),
        "{log}"
    );
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Review
    );
}

fn owner_landing_host_replay_preserves_refusals() {
    for case in [
        "open_finding",
        "failed_check",
        "reject",
        "missing_record",
        "uncovered_deferral",
        "wrapped_broad_run",
        "whole_crate_run",
    ] {
        let (fixture, _bin) = fixture(PASS);
        let mut report = report(&fixture);
        match case {
            "open_finding" => {
                report["findings"] = json!([{
                    "id": "F1", "severity": "high", "summary": "The candidate is wrong",
                    "paths": ["candidate.txt"], "disposition": "open",
                }])
            }
            "failed_check" => report["validation"][0]["outcome"] = json!("failed"),
            "reject" => report["verdict"] = json!("reject"),
            "missing_record" => {
                report["validation"].as_array_mut().unwrap().pop();
            }
            "uncovered_deferral" => {
                report["validation"][0]["deferred"] = json!([
                    "DEFERRED: bubblewrap unavailable: confined::uncovered: namespace denied"
                ])
            }
            "wrapped_broad_run" => {
                report["validation"][0]["command"] =
                    json!(format!("scripts/build-budget.py -- {BROAD}"));
            }
            "whole_crate_run" => {
                report["validation"][0]["command"] = json!("cargo test -p orbit-exec");
            }
            _ => unreachable!(),
        }
        fixture.put_report(&report);
        let refused = fixture.settle().expect_err(case).to_string();
        assert!(refused.contains("review_gate_blocked"), "{case}: {refused}");
        assert!(
            certificate(&fixture).host_evidence.is_empty(),
            "{case}: nothing runs"
        );
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, LOG)
                .unwrap()
                .is_none(),
            "{case}"
        );
    }
    for (command, os, reason) in [
        (
            "cargo test -p orbit-exec --test probe;touch owned",
            "linux",
            HostEvidenceReason::ShellMetacharacter,
        ),
        (
            "sh tests/host/probe.sh",
            "linux",
            HostEvidenceReason::CommandNotAllowed,
        ),
        (
            // `--lib` alone is admitted. A neighbouring target selector stays refused.
            "cargo test -p orbit-exec --bins",
            "linux",
            HostEvidenceReason::ArgumentNotAllowed,
        ),
        (COMMAND, "macos", HostEvidenceReason::OsMismatch),
    ] {
        let (fixture, _bin) = fixture(PASS);
        let mut report = report(&fixture);
        report["validation"][0] = json!({
            "id": "V1", "command": "fixture check", "outcome": "passed", "role": "required",
        });
        report["validation"][1]["command"] = json!(command);
        report["external_evidence"][0]["command"] = json!(command);
        report["external_evidence"][0]["os"] = json!(os);
        fixture.put_report(&report);
        fixture
            .settle()
            .expect_err("a refused requirement leaves the review incomplete");
        let certificate = certificate(&fixture);
        let [refused] = certificate.host_evidence.as_slice() else {
            panic!("one refusal: {:?}", certificate.host_evidence);
        };
        assert_eq!(refused.reason, Some(reason), "{command}: {refused:?}");
        assert!(refused.host_command.is_none(), "{command}: nothing runs");
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, LOG)
                .unwrap()
                .is_none()
        );
    }
}

fn owner_landing_host_runs_must_execute_and_pass() {
    for (probe, reason) in [
        (
            "echo 'SKIP: a missing kernel feature'\n",
            HostEvidenceReason::SelfSkipped,
        ),
        (
            "echo 'DEFERRED: bubblewrap unavailable: probe: namespaces denied'\n",
            HostEvidenceReason::SelfSkipped,
        ),
        (
            "echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out'\n",
            HostEvidenceReason::NoTestsRan,
        ),
        (
            "echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored'\nexit 101\n",
            HostEvidenceReason::TestFailed,
        ),
    ] {
        let (fixture, _bin) = fixture(probe);
        fixture.put_report(&report(&fixture));
        let refused = fixture
            .settle()
            .expect_err("this host run proves no pass")
            .to_string();
        assert!(
            refused.contains("review_gate_blocked"),
            "{reason:?}: {refused}"
        );
        let certificate = certificate(&fixture);
        assert!(!certificate.validation_complete);
        let [ran] = certificate.host_evidence.as_slice() else {
            panic!("one host attempt: {:?}", certificate.host_evidence);
        };
        assert!(!ran.passed, "{ran:?}");
        assert_eq!(ran.reason, Some(reason), "{ran:?}");
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, ARTIFACT)
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, LOG)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            fixture.runtime.get_task(&fixture.task_id).unwrap().status,
            TaskStatus::Review
        );
    }
}

crate::scenario_table!(
    owner_landing_host_replay_refusals,
    owner_landing_host_replay_preserves_refusals,
    owner_landing_host_runs_must_execute_and_pass,
    owner_landing_host_replay_refuses_candidate_and_task_drift,
);

fn owner_landing_host_replay_refuses_candidate_and_task_drift() {
    for case in ["commit", "tree", "meaning"] {
        let (fixture, _bin) = fixture(PASS);
        fixture.put_report(&report(&fixture));
        if case == "meaning" {
            fixture
                .runtime
                .update_task_with_identity(
                    &fixture.task_id,
                    orbit_core::application::task::TaskUpdateParams {
                        description: Some("The operator changed the task's intent".into()),
                        ..Default::default()
                    },
                    Some("codex".into()),
                    None,
                )
                .unwrap();
        } else {
            if case == "tree" {
                std::fs::write(
                    fixture.repo.join("candidate.txt"),
                    "a different candidate\n",
                )
                .unwrap();
            }
            let output = crate::git_test_support::command()
                .args(["commit", "--allow-empty", "-qam", "candidate moved"])
                .current_dir(&fixture.repo)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        let refused = fixture
            .settle()
            .expect_err("stale review never runs a host test")
            .to_string();
        let reason = if case == "meaning" {
            "task_meaning_changed"
        } else {
            "candidate_changed"
        };
        assert!(refused.contains(reason), "{case}: {refused}");
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, LOG)
                .unwrap()
                .is_none(),
            "{case}"
        );
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, ARTIFACT)
                .unwrap()
                .is_none(),
            "{case}"
        );
    }
}
