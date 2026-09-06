//! Real Git + task registry + artifact tool consumer tests, without provider/network I/O.
use super::super::{COVERAGE_ARTIFACT, evaluate_auto_task, record_direct_landing_intent};
use crate::{
    OrbitRuntime,
    application::{auto_tasks::AutoTaskAddParams, task::TaskUpdateParams},
};
use chrono::Utc;
use orbit_tools::{ReservationOwnerContext, ToolContext};
use orbit_types::{
    policy::Role,
    task::{TaskPriority, TaskStatus, TaskType},
    workflow::automation::*,
    workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy},
};
use serde_json::json;
use std::{path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}
fn commit(root: &Path, text: &str) -> String {
    std::fs::write(root.join("sample.txt"), text).unwrap();
    git(root, &["add", "sample.txt"]);
    git(root, &["commit", "-m", "fixture change"]);
    git(root, &["rev-parse", "HEAD"])
}
fn runtime() -> OrbitRuntime {
    let runtime = OrbitRuntime::in_memory()
        .unwrap()
        .with_automation_machine_identity(Some("fixture-machine".into()));
    let root = &runtime.paths().repo_root;
    assert!(root.to_string_lossy().contains("orbit-in-memory-"));
    std::fs::create_dir_all(root).unwrap();
    git(root, &["init", "--initial-branch=agent-main"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "user.email", "test@example.invalid"]);
    commit(root, "baseline");
    runtime
}
fn definition(
    runtime: &OrbitRuntime,
    name: &str,
    coverage: CoverageClass,
) -> orbit_types::workflow::AutoTaskDefinition {
    let mut d = runtime
        .auto_task_add(AutoTaskAddParams {
            name: name.into(),
            description: "Integration fixture".into(),
            schedule: AutoTaskSchedule::Deliveries {
                deliveries_landed: DeliveryTrigger {
                    owner_machine: Some("fixture-machine".into()),
                    branch: "agent-main".into(),
                    threshold: 1,
                    max_wait_minutes: 60,
                    coverage,
                    max_items: 20,
                    retries: 0,
                },
            },
            template: AutoTaskTemplate {
                title: "Examine batch".into(),
                description: "Inspect captured input".into(),
                acceptance_criteria: vec!["All obligations examined".into()],
                task_type: TaskType::Chore,
                tags: vec![],
                required_tools: vec![],
                priority: TaskPriority::Medium,
                crew: None,
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .unwrap();
    d.enabled = true;
    d
}
fn attach(
    runtime: &OrbitRuntime,
    attempt: &BatchAttempt,
    owner: Option<&str>,
    evidence: &CoverageEvidence,
) {
    let root = &runtime.paths().repo_root;
    let file = root.join("evidence.json");
    std::fs::write(&file, serde_json::to_vec(evidence).unwrap()).unwrap();
    let id = attempt.action_id.as_ref().unwrap();
    runtime
        .run_tool_with_context_and_role(
            "orbit.task.artifact.put",
            json!({"id":id,"source_path":file,"path":COVERAGE_ARTIFACT,"model":"codex"}),
            Role::Admin,
            ToolContext {
                allowed_tools: vec!["orbit.task.*".into()],
                orbit_host: Some(crate::adapter::tool_host::build_orbit_tool_host(
                    runtime,
                    Some(id.clone()),
                    owner.map(str::to_owned),
                    orbit_types::tool::ToolSessionContext::default(),
                )),
                reservation_owner: owner.map(|id| ReservationOwnerContext {
                    owner_run_id: id.into(),
                    owner_metadata_json: None,
                }),
                ..Default::default()
            },
        )
        .unwrap();
}
#[test]
fn qa_and_review_accept_only_assigned_artifact_evidence_once() {
    let runtime = runtime();
    let qa = definition(&runtime, "qa", CoverageClass::IntegratedQaV1);
    let review = definition(&runtime, "review", CoverageClass::LandedCodeReviewV1);
    let baseline = evaluate_auto_task(&runtime, &qa, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .baseline;
    evaluate_auto_task(&runtime, &review, false, Utc::now()).unwrap();
    commit(&runtime.paths().repo_root, "first change");
    let through = commit(&runtime.paths().repo_root, "second change");
    let delivery_run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "direct_fixture",
            1,
            Utc::now(),
            Some(json!({"task_ids":[]})),
            None,
        )
        .unwrap();
    record_direct_landing_intent(
        &runtime,
        &DirectLandingRequest {
            run_id: delivery_run.run_id,
            branch: "agent-main".into(),
            before_commit: baseline.commit.clone(),
            after_commit: through.clone(),
        },
    )
    .unwrap();
    let qa_attempt = evaluate_auto_task(&runtime, &qa, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .active
        .unwrap();
    let review_attempt = evaluate_auto_task(&runtime, &review, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(
        qa_attempt.batch.deliveries.len(),
        1,
        "a multi-commit direct bundle counts once"
    );
    assert_eq!(qa_attempt.batch.commits.len(), 2);
    assert_ne!(qa_attempt.action_id, review_attempt.action_id);
    // Replaying Core's canonical task admission resolves the original task.
    let mut claim = qa_attempt.clone();
    claim.action_id = None;
    claim.state = BatchState::Claimed;
    assert_eq!(
        super::super::task::mint(&runtime, &qa, &claim).unwrap(),
        qa_attempt.action_id.clone().unwrap()
    );
    let mut evidence = evidence_template(&qa_attempt);
    evidence.examination_complete = true;
    evidence.checks = vec![ExaminationCheck {
        subject: "captured range".into(),
        method: "exercise fixture".into(),
        observation: "behavior verified".into(),
    }];
    evidence.findings = vec!["Finding remains open".into()];
    attach(&runtime, &qa_attempt, None, &evidence);
    let unauthorized = evaluate_auto_task(&runtime, &qa, false, Utc::now()).unwrap();
    assert_eq!(unauthorized.state.unwrap().covered, baseline);
    assert!(unauthorized.receipts.is_empty());
    let action = qa_attempt.action_id.as_ref().unwrap();
    let worker = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "examination",
            1,
            Utc::now(),
            Some(json!({"task_id":action})),
            None,
        )
        .unwrap();
    runtime
        .update_task(
            action,
            TaskUpdateParams {
                job_run_id: Some(Some(worker.run_id.clone())),
                ..Default::default()
            },
        )
        .unwrap();
    attach(&runtime, &qa_attempt, Some(&worker.run_id), &evidence);
    let accepted = evaluate_auto_task(&runtime, &qa, false, Utc::now()).unwrap();
    assert_eq!(accepted.state.unwrap().covered.commit, through);
    assert_eq!(accepted.receipts.len(), 1);
    assert_eq!(
        accepted.receipts[0].submitted_by,
        format!("run:{}", worker.run_id)
    );
    assert_eq!(
        evaluate_auto_task(&runtime, &review, false, Utc::now())
            .unwrap()
            .state
            .unwrap()
            .covered,
        baseline,
        "QA never certifies review"
    );
    evidence.examination_complete = false;
    attach(&runtime, &qa_attempt, Some(&worker.run_id), &evidence);
    assert_eq!(
        evaluate_auto_task(&runtime, &qa, false, Utc::now())
            .unwrap()
            .receipts,
        accepted.receipts,
        "accepted bytes outlive artifact replacement"
    );
}
#[test]
fn direct_intent_does_not_count_before_actual_landing() {
    let runtime = runtime();
    let d = definition(&runtime, "qa", CoverageClass::IntegratedQaV1);
    let baseline = evaluate_auto_task(&runtime, &d, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .baseline;
    git(&runtime.paths().repo_root, &["checkout", "-b", "feature"]);
    let after = commit(&runtime.paths().repo_root, "candidate");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("direct_fixture", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    record_direct_landing_intent(
        &runtime,
        &DirectLandingRequest {
            run_id: run.run_id,
            branch: "agent-main".into(),
            before_commit: baseline.commit,
            after_commit: after,
        },
    )
    .unwrap();
    assert!(
        evaluate_auto_task(&runtime, &d, false, Utc::now())
            .unwrap()
            .state
            .unwrap()
            .active
            .is_none()
    );
    git(&runtime.paths().repo_root, &["checkout", "agent-main"]);
    git(
        &runtime.paths().repo_root,
        &["merge", "--ff-only", "feature"],
    );
    assert!(
        evaluate_auto_task(&runtime, &d, false, Utc::now())
            .unwrap()
            .state
            .unwrap()
            .active
            .is_some(),
        "a persisted intent recovers even if the owner died after Git merged"
    );
}

#[test]
fn persisted_job_admission_is_atomic_and_replay_validates_input() {
    let runtime = runtime();
    let jobs = runtime.stores().jobs();
    let input = json!({"automation":{"input_digest":"frozen"}});
    let a = jobs
        .insert_automation_job_run("review_job", input.clone(), "batch:attempt:1")
        .unwrap();
    let b = jobs
        .insert_automation_job_run("review_job", input.clone(), "batch:attempt:1")
        .unwrap();
    assert_eq!(a.run_id, b.run_id);
    assert!(
        jobs.insert_automation_job_run("review_job", json!({"changed":true}), "batch:attempt:1")
            .is_err()
    );
    assert_eq!(jobs.list_job_runs("review_job").unwrap().len(), 1);
}

#[test]
fn provider_rebase_membership_counts_once_and_unavailable_evidence_stays_pending() {
    let runtime = runtime();
    git(
        &runtime.paths().repo_root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ],
    );
    let d = definition(&runtime, "review", CoverageClass::LandedCodeReviewV1);
    let state = evaluate_auto_task(&runtime, &d, false, Utc::now())
        .unwrap()
        .state
        .unwrap();
    let first = commit(&runtime.paths().repo_root, "first");
    let second = commit(&runtime.paths().repo_root, "second");
    let source = super::super::source::Source::new(&runtime.paths().repo_root);
    let payload=json!([{"number":17,"html_url":"https://github.com/owner/repo/pull/17","base":{"ref":"agent-main","repo":{"full_name":"owner/repo"}},"merge_commit_sha":second,"merged_at":"2026-09-06T00:00:00Z"}]).to_string();
    let page = source
        .observe_with_lookup("agent-main", &state, &|_, _| Ok(payload.clone()))
        .unwrap();
    assert_eq!(page.deliveries.len(), 1);
    assert_eq!(page.deliveries[0].commits, vec![first, second]);
    assert!(
        page.deliveries[0].task_ids.is_empty(),
        "manual landings do not need Orbit tasks"
    );
    let unavailable = source
        .observe_with_lookup("agent-main", &state, &|_, _| {
            Err(orbit_automation::AutomationError::Deferred(
                "provider down".into(),
            ))
        })
        .unwrap();
    assert!(unavailable.deliveries.is_empty());
    assert_eq!(unavailable.unresolved.len(), 2);
    assert_eq!(unavailable.commits.len(), 2);
}

#[test]
fn no_diff_pr_is_zero_but_distinct_revert_pr_is_new_delivery() {
    let runtime = runtime();
    git(
        &runtime.paths().repo_root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ],
    );
    let d = definition(&runtime, "review", CoverageClass::LandedCodeReviewV1);
    let state = evaluate_auto_task(&runtime, &d, false, Utc::now())
        .unwrap()
        .state
        .unwrap();
    let first = commit(&runtime.paths().repo_root, "change");
    let second = commit(&runtime.paths().repo_root, "baseline");
    let payload = |number: u64, anchor: &str| {
        json!([{"number":number,"html_url":format!("https://github.com/owner/repo/pull/{number}"),"base":{"ref":"agent-main","repo":{"full_name":"owner/repo"}},"merge_commit_sha":anchor,"merged_at":"2026-09-06T00:00:00Z"}]).to_string()
    };
    let source = super::super::source::Source::new(&runtime.paths().repo_root);
    let no_diff = source
        .observe_with_lookup("agent-main", &state, &|_, _| Ok(payload(17, &second)))
        .unwrap();
    assert!(no_diff.deliveries.is_empty());
    let revert = source
        .observe_with_lookup("agent-main", &state, &|_, sha| {
            Ok(if sha == first {
                payload(17, &first)
            } else {
                payload(18, &second)
            })
        })
        .unwrap();
    assert_eq!(
        revert.deliveries.len(),
        2,
        "a distinct revert counts even when aggregate tree equals baseline"
    );
}

#[test]
fn only_explicit_machine_owner_can_establish_baseline_and_preview_is_read_only() {
    let runtime = runtime();
    let mut definition = definition(&runtime, "ownership", CoverageClass::IntegratedQaV1);
    let AutoTaskSchedule::Deliveries { deliveries_landed } = &mut definition.schedule else {
        unreachable!()
    };
    deliveries_landed.owner_machine = None;
    let diagnostic = evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    assert_eq!(diagnostic.reason, "owned_elsewhere");
    assert!(diagnostic.state.is_none());
    let preview = evaluate_auto_task(&runtime, &definition, true, Utc::now()).unwrap();
    assert_eq!(preview.reason, "would_baseline");
    let consumer = &preview.state.unwrap().consumer;
    assert!(consumer.starts_with("fixture-machine/"));
    assert!(
        runtime
            .automation_store()
            .unwrap()
            .automation_state(consumer)
            .unwrap()
            .is_none()
    );
    let AutoTaskSchedule::Deliveries { deliveries_landed } = &mut definition.schedule else {
        unreachable!()
    };
    deliveries_landed.owner_machine = Some("another-machine".into());
    assert_eq!(
        evaluate_auto_task(&runtime, &definition, false, Utc::now())
            .unwrap()
            .reason,
        "owned_elsewhere"
    );
    let AutoTaskSchedule::Deliveries { deliveries_landed } = &mut definition.schedule else {
        unreachable!()
    };
    deliveries_landed.owner_machine = Some("fixture-machine".into());
    assert_eq!(
        evaluate_auto_task(&runtime, &definition, false, Utc::now())
            .unwrap()
            .reason,
        "baselined"
    );
}

#[test]
fn job_only_evidence_comes_from_persisted_step_and_matches_frozen_input() {
    use orbit_automation::delivery::{ActionOutcome, evidence::validate};
    use orbit_store::contracts::JobRunStepParams;
    use orbit_types::workflow::{JobRunState, JobTargetType};
    let runtime = runtime();
    let definition = definition(&runtime, "job-fixture", CoverageClass::IntegratedQaV1);
    let baseline = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .baseline;
    let through = commit(&runtime.paths().repo_root, "job examination");
    let jobs = runtime.stores().jobs();
    let delivery = jobs
        .insert_job_run("delivery", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    record_direct_landing_intent(
        &runtime,
        &DirectLandingRequest {
            run_id: delivery.run_id,
            branch: "agent-main".into(),
            before_commit: baseline.commit,
            after_commit: through,
        },
    )
    .unwrap();
    let mut attempt = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .active
        .unwrap();
    let run = jobs
        .insert_automation_job_run(
            "examine",
            json!({"automation":attempt}),
            "job-evidence-fixture",
        )
        .unwrap();
    attempt.action_id = Some(run.run_id.clone());
    let source = super::super::source::Source::new(&runtime.paths().repo_root);
    assert!(matches!(
        super::super::task::job_outcome(&runtime, &source, &attempt).unwrap(),
        ActionOutcome::Pending
    ));
    let mut evidence = evidence_template(&attempt);
    evidence.examination_complete = true;
    evidence.checks = vec![ExaminationCheck {
        subject: "captured range".into(),
        method: "fixture".into(),
        observation: "verified".into(),
    }];
    assert!(
        jobs.complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                step_index: 0,
                target_type: JobTargetType::Activity,
                target_id: "examine".into(),
                started_at: Utc::now(),
                finished_at: Utc::now(),
                duration_ms: Some(1),
                exit_code: Some(0),
                agent_response_json: Some(json!({"coverage_evidence":evidence})),
                state: JobRunState::Success,
                error_code: None,
                error_message: None,
            }
        )
        .unwrap()
    );
    let ActionOutcome::Evidence(facts) =
        super::super::task::job_outcome(&runtime, &source, &attempt).unwrap()
    else {
        panic!("persisted step must supply evidence");
    };
    let receipt = validate(&attempt, &facts, Utc::now()).unwrap();
    assert_eq!(
        receipt.evidence_reference,
        format!("run:{}/step:0", run.run_id)
    );
    attempt.input_digest = "changed".into();
    assert!(super::super::task::job_outcome(&runtime, &source, &attempt).is_err());
}
