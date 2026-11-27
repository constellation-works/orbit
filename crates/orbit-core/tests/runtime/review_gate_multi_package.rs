//! Candidate-bound ownership for multi-package integration test deferrals.

use chrono::Utc;
use orbit_engine::RunLifecycle;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewEvidenceHold,
};
use serde_json::{Value, json};

use crate::review_continuation::attach_as_operator;
use crate::review_gate_audit::Fixture;

const BROAD: &str = "cargo test -p orbit-cli -p orbit-engine --test engine";

const TESTS: [&str; 2] = ["sandbox::first", "sandbox::second"];

fn git(fixture: &Fixture, args: &[&str]) {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn fixture(ambiguous: bool) -> Fixture {
    let mut fixture = Fixture::new();
    for package in ["orbit-cli", "orbit-engine"] {
        let dir = fixture.repo.join(format!("crates/{package}/tests"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.parent().unwrap().join("Cargo.toml"),
            format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        if package == "orbit-engine" {
            std::fs::create_dir_all(dir.join("engine")).unwrap();
            std::fs::write(dir.join("engine/main.rs"), "// integration target\n").unwrap();
        } else if ambiguous {
            std::fs::write(dir.join("engine.rs"), "// competing target\n").unwrap();
        }
    }
    std::fs::write(
        fixture.repo.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/orbit-cli\", \"crates/orbit-engine\"]\n",
    )
    .unwrap();
    git(&fixture, &["add", "Cargo.toml", "crates"]);
    git(&fixture, &["commit", "-qm", "workspace targets"]);
    fixture.admit();
    fixture
}

fn command(package: &str, target: &str, test: &str) -> String {
    format!("cargo test -p {package} --test {target} {test}")
}

fn report(fixture: &Fixture, broad: &str, requirements: &[String], outcome: &str) -> Value {
    let notices = TESTS
        .iter()
        .map(|test| format!("DEFERRED: bubblewrap unavailable: {test}: no namespace"))
        .collect::<Vec<_>>();
    let mut validation = vec![
        json!({"id":"V1", "command":"fixture check", "role":"required", "outcome":"passed"}),
        json!({
            "id":"V2", "command":broad, "role":"required", "outcome":outcome,
            "deferred": notices,
        }),
    ];
    let mut external = Vec::new();
    for (index, requirement) in requirements.iter().enumerate() {
        validation.push(json!({
            "id":format!("V{}", index + 3), "command":requirement,
            "role":"required", "outcome":"not_run",
        }));
        external.push(json!({
            "kind":"host_sandbox_test", "name":format!("sandbox {index}"),
            "command":requirement, "artifact":format!("evidence/sandbox-{index}.json"),
            "os":"linux",
        }));
    }
    json!({
        "schema_version":1, "attempt_id":fixture.input["admission"]["attempt_id"],
        "verdict":"incomplete", "summary":"Host sandbox evidence needed", "findings":[],
        "validation":validation, "external_evidence":external, "escalation":"Host evidence needed",
    })
}

fn multi_package_hold_is_fulfilled_only_by_canonical_candidate_evidence() {
    let mut fixture = fixture(false);
    let requirements = TESTS.map(|test| command("orbit-engine", "engine", test));
    fixture.put_report(&report(&fixture, BROAD, &requirements, "passed"));
    let result = fixture
        .settle()
        .expect_err("formal evidence hold")
        .to_string();
    assert!(
        result.contains("review_awaiting_evidence"),
        "{result}; certificate: {}",
        String::from_utf8_lossy(
            &fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
                .unwrap()
                .unwrap()
                .content
        )
    );
    let hold: ReviewEvidenceHold = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(hold.requirements.len(), 2);
    for (index, required) in hold.requirements.iter().enumerate() {
        let log = format!("evidence/sandbox-{index}.log.json");
        let payload = json!({
            "schema_version":1, "tested_head":hold.candidate.commit,
            "tested_tree":hold.candidate.tree, "command":required.command,
            "outcome":"passed", "output":"test result: ok. 1 passed; 0 failed",
        });
        attach_as_operator(&fixture, &log, &payload);
        let mut result = json!({
            "schema_version":1, "attempt_id":hold.attempt_id, "candidate":hold.candidate,
            "kind":"host_sandbox_test", "name":required.name, "command":required.command,
            "outcome":"passed", "log_artifact":log, "os":"linux",
        });
        if index == 0 {
            result["candidate"]["tree"] = json!("0000000000000000000000000000000000000000");
            attach_as_operator(&fixture, &required.artifact, &result);
            assert_eq!(
                fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                TaskStatus::InProgress
            );
            result["candidate"] = json!(hold.candidate);
            result["outcome"] = json!("failed");
            attach_as_operator(&fixture, &required.artifact, &result);
            assert_eq!(
                fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                TaskStatus::InProgress
            );
            result["outcome"] = json!("passed");
        }
        attach_as_operator(&fixture, &required.artifact, &result);
        if index == 0 {
            assert_eq!(
                fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                TaskStatus::InProgress
            );
        }
    }
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Backlog
    );
    let previous = fixture.runtime.show_job_run(&hold.run_id).unwrap();
    let next = orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), previous.input, None)
        .unwrap();
    orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .apply_task_automation_update(
            &fixture.task_id,
            orbit_engine::TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(next.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    fixture.input["job_run_id"] = json!(next.run_id);
    fixture.admit();
    fixture.put_report(&report(&fixture, BROAD, &requirements, "passed"));
    let settled = fixture.settle().expect("same tree owner evidence settles");
    assert_eq!(settled["gate"], "passed");
    let cert: ReviewCertificate = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert!(cert.validation_complete && cert.verdict.passed());
    assert!(
        cert.validation
            .iter()
            .find(|record| record.command == BROAD)
            .unwrap()
            .deferred
            .is_empty()
    );
}

fn multi_package_unproven_or_incomplete_evidence_never_holds() {
    let exact = TESTS.map(|test| command("orbit-engine", "engine", test));
    let cases = [
        (
            false,
            BROAD.to_string(),
            vec![command("orbit-cli", "engine", TESTS[0]), exact[1].clone()],
            "passed",
        ),
        (
            false,
            BROAD.to_string(),
            vec![command("orbit-engine", "other", TESTS[0]), exact[1].clone()],
            "passed",
        ),
        (
            false,
            BROAD.to_string(),
            vec![
                command("orbit-engine", "engine", "sandbox::other"),
                exact[1].clone(),
            ],
            "passed",
        ),
        (true, BROAD.to_string(), exact.to_vec(), "passed"),
        (
            false,
            format!("{BROAD} --release"),
            exact.to_vec(),
            "passed",
        ),
        (
            false,
            format!("{BROAD} --all-targets"),
            exact.to_vec(),
            "passed",
        ),
        (false, BROAD.to_string(), exact.to_vec(), "failed"),
        (false, BROAD.to_string(), vec![exact[0].clone()], "passed"),
    ];
    for (ambiguous, broad, requirements, outcome) in cases {
        let fixture = fixture(ambiguous);
        fixture.put_report(&report(&fixture, &broad, &requirements, outcome));
        let refusal = fixture
            .settle()
            .expect_err("uncovered evidence blocks")
            .to_string();
        assert!(
            refusal.contains("review_gate_blocked"),
            "{broad}: {refusal}"
        );
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                .unwrap()
                .is_none()
        );
    }
}

crate::scenario_table!(
    multi_package_host_evidence_scenarios,
    multi_package_hold_is_fulfilled_only_by_canonical_candidate_evidence,
    multi_package_unproven_or_incomplete_evidence_never_holds,
);
