//! Review interruption and external evidence through runtime/tool boundaries.

use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_engine::{
    ReviewReleaseRequest, ReviewerInvocationRequest, RuntimeHost, TaskAutomationUpdate,
};
use orbit_types::workflow::{
    JobRunState, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewAttemptState,
    ReviewEvidenceHold, ReviewVerdict, ReviewerInvocationEvent,
};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

fn attach(fixture: &Fixture, path: &str, content: &Value) {
    let source = fixture.repo.join(".orbit/tmp").join(path);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, content.to_string()).unwrap();
    fixture
        .runtime
        .run_tool(
            "orbit.task.artifact.put",
            json!({
                "id": fixture.task_id, "model": "codex", "path": path, "source_path": source,
            }),
        )
        .unwrap();
}

fn interrupted_report(fixture: &Fixture) -> Value {
    json!({
        "schema_version": 1, "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete", "summary": "Inspected error paths; platform checks remain.",
        "findings": [],
        "validation": [{"command": "fixture check", "outcome": "passed", "role": "required"}],
        "escalation": "External checks pending",
    })
}

/// Run admission and settlement in the persisted worker, with every recovery
/// hook configured. Reports still arrive through the reviewer's artifact tool.
fn run_review_pipeline(fixture: &Fixture) {
    // Shipped job names resolve from the fixture's global catalog.
    let resources = fixture.runtime.paths().global_dir.join("resources");
    std::fs::create_dir_all(resources.join("jobs")).unwrap();
    std::fs::create_dir_all(resources.join("activities")).unwrap();
    std::fs::write(resources.join("activities/unexpected_recovery.yaml"), json!({
        "schemaVersion": 2, "kind": "Activity", "metadata": {"name": "unexpected_recovery"},
        "spec": {"type": "deterministic", "description": "Record any unexpected recovery or publication",
            "input_schema_json": {}, "output_schema_json": {},
            "action": "orbit_tool_call", "config": {
            "tool_name": "orbit.task.update",
            "args": {"id": fixture.task_id, "model": "codex", "comment": "Unexpected recovery ran"},
        }},
    }).to_string()).unwrap();
    let mut settle_input = fixture.input.clone();
    settle_input["admission"] = json!("{{ steps.review_gate_admit.output }}");
    std::fs::write(
        resources.join("jobs/task_pr_pipeline.yaml"),
        json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"},
            "spec": {
                "state": "enabled", "kind": "workflow",
                "failure_activity": "unexpected_recovery",
                "final_recovery_activity": "unexpected_recovery",
                "steps": [{
                    "id": "review_gate_admit", "default_input": fixture.input,
                    "spec": {"type": "deterministic", "action": "review_gate_admit", "config": {}},
                }, {
                    "id": "review_gate_settle", "default_input": settle_input,
                    "retry": {"max_attempts": 3, "initial_backoff_ms": 1, "backoff_cap_ms": 1},
                    "recovery_activity": "unexpected_recovery",
                    "spec": {"type": "deterministic", "action": "review_gate_settle", "config": {}},
                }, {
                    "id": "publish", "target": "activity:unexpected_recovery",
                }],
            },
        })
        .to_string(),
    )
    .unwrap();
    fixture
        .runtime
        .execute_pipeline_run_worker(fixture.input["job_run_id"].as_str().unwrap())
        .unwrap();
}

#[test]
fn timeout_retains_partial_report_and_budget_and_resumes_the_same_review() {
    if !super::dispatch_admission::isolated(
        "review_continuation::timeout_retains_partial_report_and_budget_and_resumes_the_same_review",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    RuntimeHost::mark_job_run_running(
        &fixture.runtime,
        fixture.input["job_run_id"].as_str().unwrap(),
        Utc::now(),
        std::process::id(),
    )
    .unwrap();
    let partial = interrupted_report(&fixture);
    fixture.put_report(&partial);
    let request = |event| ReviewerInvocationRequest {
        run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
        lineage_key: fixture.input["admission"]["lineage_key"]
            .as_str()
            .unwrap()
            .into(),
        attempt_id: fixture.input["admission"]["attempt_id"]
            .as_str()
            .unwrap()
            .into(),
        event,
    };
    let bound = RuntimeHost::record_reviewer_invocation(
        &fixture.runtime,
        &request(ReviewerInvocationEvent::Started {
            timeout_seconds: 1800,
        }),
    )
    .unwrap()
    .unwrap();
    assert!(
        bound > 0 && bound < 600,
        "a timeout must leave continuation time within the captured budget"
    );
    let finished = request(ReviewerInvocationEvent::TimedOut {
        runtime_seconds: bound,
    });
    RuntimeHost::record_reviewer_invocation(&fixture.runtime, &finished).unwrap();
    RuntimeHost::release_review_attempt(
        &fixture.runtime,
        &ReviewReleaseRequest {
            run_id: finished.run_id.clone(),
            lineage_key: finished.lineage_key.clone(),
            attempt_id: finished.attempt_id.clone(),
        },
    )
    .unwrap();
    let ledger = fixture
        .runtime
        .review_store()
        .unwrap()
        .review_ledger(
            &fixture.runtime.workspace_id().unwrap(),
            &finished.lineage_key,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        ledger.attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::Incomplete
        }
    );
    assert_eq!(
        ledger.consumed_seconds, bound,
        "only runtime actually spent counts"
    );
    assert!(ledger.remaining_at(Utc::now()).seconds > 0);
    let kept = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_REPORT_ARTIFACT)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&kept.content).unwrap(),
        partial
    );
    RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        &fixture.task_id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Backlog),
            status_event: Some("review_timeout_incomplete".into()),
            status_note: Some(format!("run={}, reviewer timed out", finished.run_id)),
            ..Default::default()
        },
    )
    .unwrap();
    RuntimeHost::finalize_job_run(
        &fixture.runtime,
        &finished.run_id,
        JobRunState::Failed,
        Utc::now(),
        None,
    )
    .unwrap();
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Backlog
    );
    fixture.admit();
    assert_eq!(fixture.input["admission"]["decision"], "resumed");
    assert_eq!(
        fixture.input["admission"]["attempt_id"],
        finished.attempt_id
    );
    let manifest = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, "review-manifest.json")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&manifest.content).unwrap()["previous_report"],
        partial
    );
    let mut completed = partial;
    completed["verdict"] = json!("accept");
    completed["escalation"] = Value::Null;
    fixture.put_report(&completed);
    assert_eq!(fixture.settle().unwrap()["gate"], "passed");
}

#[test]
fn named_external_checks_hold_until_every_matching_result_and_log_arrives() {
    if !super::dispatch_admission::isolated(
        "review_continuation::named_external_checks_hold_until_every_matching_result_and_log_arrives",
    ) {
        return;
    }
    for verdict in ["incomplete", "changes_required"] {
        let mut fixture =
            Fixture::new_with_required_commands(&["hosted windows", "native macos", "codeql"]);
        fixture.admit();
        let mut report = interrupted_report(&fixture);
        let requirements = json!([
            {"kind": "hosted_ci", "name": "Windows CI job", "command": "hosted windows", "artifact": "evidence/windows.json"},
            {"kind": "native_os", "name": "macOS native run", "command": "native macos", "artifact": "evidence/macos.json"},
            {"kind": "codeql", "name": "Rust CodeQL extraction", "command": "codeql", "artifact": "evidence/codeql.json"},
        ]);
        report["verdict"] = json!(verdict);
        report["external_evidence"] = requirements.clone();
        for command in ["hosted windows", "native macos", "codeql"] {
            report["validation"].as_array_mut().unwrap().push(json!({
                "command": command, "outcome": "not_run", "role": "required",
            }));
        }
        fixture.put_report(&report);
        run_review_pipeline(&fixture);
        let hold: ReviewEvidenceHold = serde_json::from_slice(
            &fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                .unwrap()
                .unwrap()
                .content,
        )
        .unwrap();
        let run = fixture.runtime.show_job_run(&hold.run_id).unwrap();
        assert_eq!(
            run.state,
            JobRunState::Held,
            "ORB-14313: evidence holds must not fail delivery"
        );
        assert!(run.finished_at.is_some());
        let terminal_runs = fixture
            .runtime
            .list_job_runs_observed(orbit_core::application::job::JobRunListParams {
                terminal_only: true,
                ..Default::default()
            })
            .unwrap();
        assert!(
            terminal_runs
                .iter()
                .any(|candidate| candidate.run_id == hold.run_id),
            "held delivery runs must appear in terminal-only history"
        );
        let wait = fixture
            .runtime
            .wait_pipeline_runs(std::slice::from_ref(&hold.run_id), 1, 1, None)
            .unwrap();
        assert_eq!(wait.results[0].status, "held");
        assert!(wait.results[0].error.is_none());
        let reliability = fixture
            .runtime
            .pipeline_reliability(
                &orbit_core::metrics::reliability::ReliabilityWindow::ending_at(
                    "fixture",
                    Utc::now() + chrono::Duration::seconds(1),
                    chrono::Duration::minutes(10),
                ),
            )
            .unwrap();
        assert_eq!(reliability.job_runs.overall.held, 1);
        assert_eq!(reliability.job_runs.overall.failed, 0);
        assert_eq!(reliability.job_runs.overall.excluded(), 1);
        assert!(
            run.steps
                .iter()
                .all(|step| step.state != JobRunState::Failed)
        );
        let events = fixture
            .runtime
            .collect_run_audit_events(&hold.run_id)
            .unwrap();
        assert!(
            !events.iter().any(|event| matches!(
                event.body_kind.as_deref(),
                Some("step_retry" | "step_recovery_attempted" | "final_recovery_attempted")
            )),
            "ORB-14313: evidence holds must bypass retry and both recovery stages"
        );
        let steps = fixture
            .runtime
            .collect_run_audit_steps(&hold.run_id)
            .unwrap();
        assert_eq!(
            steps.len(),
            2,
            "hold must stop before publication and failure handoff"
        );
        assert_eq!(steps[1].state.as_deref(), Some("held"));
        assert_eq!(
            fixture
                .runtime
                .get_task_history(&fixture.task_id)
                .unwrap()
                .last()
                .unwrap()
                .event,
            "review_awaiting_evidence"
        );
        fixture
            .runtime
            .execute_pipeline_run_worker(&hold.run_id)
            .unwrap();
        assert_eq!(
            fixture.runtime.show_job_run(&hold.run_id).unwrap().state,
            JobRunState::Held
        );
        assert_eq!(
            fixture.runtime.get_task(&fixture.task_id).unwrap().status,
            TaskStatus::InProgress
        );
        assert!(
            fixture
                .runtime
                .run_deterministic(
                    "review_gate_admit",
                    &json!({}),
                    &fixture.input,
                    Default::default()
                )
                .is_err()
        );
        attach(&fixture, "unrelated.json", &json!({"outcome": "passed"}));
        for (index, requirement) in hold.requirements.iter().enumerate() {
            let log = format!("evidence/log-{index}.json");
            let mut evidence = json!({
                "schema_version": 1, "attempt_id": hold.attempt_id, "candidate": hold.candidate,
                "kind": requirement.kind, "name": requirement.name, "command": requirement.command,
                "outcome": "passed", "log_artifact": log,
            });
            if index == 0 {
                let candidate = evidence["candidate"].clone();
                evidence["candidate"]["commit"] = json!("stale-head");
                attach(&fixture, &requirement.artifact, &evidence);
                attach(
                    &fixture,
                    &log,
                    &json!({"captured_output": "passing external check"}),
                );
                assert_eq!(
                    fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                    TaskStatus::InProgress
                );
                evidence["candidate"] = candidate;
            }
            attach(&fixture, &requirement.artifact, &evidence);
            if index != 0 {
                assert_eq!(
                    fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                    TaskStatus::InProgress,
                    "a result without its attached log cannot release the hold"
                );
                attach(
                    &fixture,
                    &log,
                    &json!({"captured_output": "passing external check"}),
                );
            }
            assert_eq!(
                fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                if index + 1 == hold.requirements.len() {
                    TaskStatus::Backlog
                } else {
                    TaskStatus::InProgress
                }
            );
        }
        // Receipt is a requeue, never review approval or PR publication.
        assert!(fixture.settle().is_err());
        assert_eq!(
            fixture
                .runtime
                .get_task_history(&fixture.task_id)
                .unwrap()
                .last()
                .unwrap()
                .event,
            "review_evidence_received"
        );
        // A new delivery run reviews the candidate afresh. Evidence receipt
        // itself neither approves it nor rewrites the incomplete certificate.
        let previous_run = fixture.runtime.show_job_run(&hold.run_id).unwrap();
        let next = fixture
            .runtime
            .insert_job_run("task_pr_pipeline", 1, Utc::now(), previous_run.input, None)
            .unwrap();
        fixture
            .runtime
            .update_task_with_identity(
                &fixture.task_id,
                orbit_core::application::task::TaskUpdateParams {
                    status: Some(TaskStatus::InProgress),
                    job_run_id: Some(Some(next.run_id.clone())),
                    ..Default::default()
                },
                Some("codex".into()),
                None,
            )
            .unwrap();
        fixture.input["job_run_id"] = json!(next.run_id);
        fixture.admit();
        assert_ne!(fixture.input["admission"]["attempt_id"], hold.attempt_id);
        let mut accepted = interrupted_report(&fixture);
        accepted["verdict"] = json!("accept");
        accepted["escalation"] = Value::Null;
        for requirement in &hold.requirements {
            accepted["validation"].as_array_mut().unwrap().push(json!({
                "command": requirement.command, "outcome": "passed", "role": "required",
                "log_artifact": requirement.artifact,
            }));
        }
        fixture.put_report(&accepted);
        run_review_pipeline(&fixture);
        assert_eq!(
            fixture.runtime.show_job_run(&next.run_id).unwrap().state,
            JobRunState::Success
        );
        assert_eq!(
            fixture
                .runtime
                .read_run_state(&next.run_id)
                .unwrap()
                .unwrap()
                .pipeline["review_gate_settle"]["gate"],
            "passed"
        );
    }
}

#[test]
fn external_requirement_cannot_hide_a_reject_open_defect_or_failed_local_check() {
    if !super::dispatch_admission::isolated(
        "review_continuation::external_requirement_cannot_hide_a_reject_open_defect_or_failed_local_check",
    ) {
        return;
    }
    for case in [
        "reject",
        "open_defect",
        "failed_check",
        "unnamed_check",
        "meaning_changed",
    ] {
        let mut fixture = Fixture::new();
        fixture.admit();
        let mut report = interrupted_report(&fixture);
        report["external_evidence"] = json!([{ "kind": "hosted_ci", "name": "Windows CI",
            "command": "hosted windows", "artifact": "evidence/windows.json" }]);
        report["validation"]
            .as_array_mut()
            .unwrap()
            .push(json!({"command": "hosted windows", "outcome": "not_run"}));
        match case {
            "reject" => {
                report["verdict"] = json!("changes_required");
                report["findings"] = json!([{"id": "F1", "summary": "Wrong approach", "severity": "high", "disposition": "open"}]);
            }
            "open_defect" => {
                report["findings"] = json!([{"id": "F1", "summary": "Wrong approach", "severity": "high", "disposition": "open"}])
            }
            "failed_check" => report["validation"][0]["outcome"] = json!("failed"),
            "unnamed_check" => report["validation"]
                .as_array_mut()
                .unwrap()
                .push(json!({"command": "local required", "outcome": "not_run"})),
            "meaning_changed" => {
                fixture
                    .runtime
                    .update_task_with_identity(
                        &fixture.task_id,
                        orbit_core::application::task::TaskUpdateParams {
                            description: Some("Changed intent".into()),
                            ..Default::default()
                        },
                        Some("codex".into()),
                        None,
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        fixture.put_report(&report);
        let refused = fixture.settle().unwrap_err();
        assert!(
            refused.to_string().contains("review_gate_blocked:"),
            "{case}: {refused}"
        );
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                .unwrap()
                .is_none(),
            "{case}"
        );
    }
}

#[test]
fn a_late_review_handoff_cannot_overwrite_an_operator_block() {
    if !super::dispatch_admission::isolated(
        "review_continuation::a_late_review_handoff_cannot_overwrite_an_operator_block",
    ) {
        return;
    }
    let fixture = Fixture::new();
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(TaskStatus::Blocked),
                comment: Some("Operator holds delivery".into()),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    let refused = RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        &fixture.task_id,
        TaskAutomationUpdate {
            expected_status: Some(TaskStatus::InProgress),
            status: Some(TaskStatus::Backlog),
            status_event: Some("review_timeout_incomplete".into()),
            ..Default::default()
        },
    );
    assert!(refused.is_err());
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Blocked
    );
    assert!(
        fixture
            .runtime
            .get_task_history(&fixture.task_id)
            .unwrap()
            .iter()
            .all(|entry| entry.event != "review_timeout_incomplete")
    );
}
