//! Real Git + task registry + artifact tool consumer tests, without provider/network I/O.

use super::super::{
    COVERAGE_ARTIFACT, consumer_key, evaluate_auto_task, record_direct_landing_intent,
};
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

/// Ownership fixtures edit the trigger in place; the definition is otherwise
/// the shared delivery fixture.
fn set_owner(definition: &mut orbit_types::workflow::AutoTaskDefinition, owner: Option<&str>) {
    let AutoTaskSchedule::Deliveries { deliveries_landed } = &mut definition.schedule else {
        unreachable!("delivery fixture")
    };
    deliveries_landed.owner_machine = owner.map(ToOwned::to_owned);
}

fn ownership(diagnostic: &AutomationDiagnostic) -> DeliveryOwnership {
    diagnostic
        .ownership
        .clone()
        .expect("delivery diagnostics report ownership")
}

#[test]
fn an_omitted_owner_resolves_to_the_registered_workspace_owner() {
    let runtime = runtime().with_workspace_owner_machine_id(Some("fixture-machine"));
    let mut definition = definition(&runtime, "workspace-owner", CoverageClass::IntegratedQaV1);
    set_owner(&mut definition, None);

    let preview = evaluate_auto_task(&runtime, &definition, true, Utc::now()).unwrap();
    assert_eq!(preview.reason, "would_baseline");
    assert_eq!(
        ownership(&preview),
        DeliveryOwnership {
            owner_machine: Some("fixture-machine".into()),
            authority: OwnerAuthority::Workspace,
            owned_here: true,
        }
    );

    let inspection = super::super::inspect_auto_task(&runtime, &definition, Utc::now()).unwrap();
    assert_eq!(inspection.reason, "awaiting_baseline");
    assert_eq!(ownership(&inspection), ownership(&preview));

    let evaluated = evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    assert_eq!(
        evaluated.reason, "baselined",
        "an unambiguously owned workspace admits without redundant owner_machine"
    );
    assert_eq!(ownership(&evaluated), ownership(&preview));
}

#[test]
fn adopting_the_workspace_default_keeps_an_explicitly_owned_consumer() {
    let runtime = runtime().with_workspace_owner_machine_id(Some("fixture-machine"));
    let mut definition = definition(&runtime, "adopt-default", CoverageClass::IntegratedQaV1);

    let pinned = evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    assert_eq!(ownership(&pinned).authority, OwnerAuthority::Definition);
    let pinned_epoch = pinned.state.unwrap().epoch;

    set_owner(&mut definition, None);
    let adopted = super::super::inspect_auto_task(&runtime, &definition, Utc::now()).unwrap();
    assert_eq!(
        adopted.reason, "not_due",
        "the identical resolved owner is not a definition change"
    );
    assert_eq!(adopted.state.unwrap().epoch, pinned_epoch);

    set_owner(&mut definition, Some("another-machine"));
    assert_eq!(
        super::super::inspect_auto_task(&runtime, &definition, Utc::now())
            .unwrap()
            .reason,
        "definition_changed",
        "a genuinely different owner still moves the epoch"
    );
}

#[test]
fn a_replica_cannot_claim_ownership_by_omitting_it() {
    let runtime = runtime().with_workspace_owner_machine_id(Some("another-machine"));
    let mut definition = definition(&runtime, "replica", CoverageClass::IntegratedQaV1);
    set_owner(&mut definition, None);

    for diagnostic in [
        evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap(),
        evaluate_auto_task(&runtime, &definition, true, Utc::now()).unwrap(),
        super::super::inspect_auto_task(&runtime, &definition, Utc::now()).unwrap(),
    ] {
        assert_eq!(diagnostic.reason, "owned_elsewhere");
        assert_eq!(
            ownership(&diagnostic),
            DeliveryOwnership {
                owner_machine: Some("another-machine".into()),
                authority: OwnerAuthority::Workspace,
                owned_here: false,
            }
        );
    }

    let consumer = consumer_key(&runtime, "auto-task", &definition.name).unwrap();
    assert!(
        runtime
            .automation_store()
            .unwrap()
            .automation_state(&consumer)
            .unwrap()
            .is_none(),
        "a refused owner records nothing, so no host admits the same work twice"
    );
}

#[test]
fn an_explicit_owner_overrides_the_registered_workspace_owner() {
    let runtime = runtime().with_workspace_owner_machine_id(Some("another-machine"));
    let definition = definition(&runtime, "explicit", CoverageClass::IntegratedQaV1);

    let evaluated = evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    assert_eq!(evaluated.reason, "baselined");
    assert_eq!(
        ownership(&evaluated),
        DeliveryOwnership {
            owner_machine: Some("fixture-machine".into()),
            authority: OwnerAuthority::Definition,
            owned_here: true,
        }
    );
}

#[test]
fn unregistered_and_contradicted_ownership_refuse_with_a_named_authority() {
    let unregistered = runtime();
    let mut definition = definition(&unregistered, "unregistered", CoverageClass::IntegratedQaV1);
    set_owner(&mut definition, None);

    for diagnostic in [
        evaluate_auto_task(&unregistered, &definition, false, Utc::now()).unwrap(),
        evaluate_auto_task(&unregistered, &definition, true, Utc::now()).unwrap(),
        super::super::inspect_auto_task(&unregistered, &definition, Utc::now()).unwrap(),
    ] {
        assert_eq!(
            diagnostic.reason, "ownership_unresolved",
            "an enabled definition nobody owns must not read as disabled"
        );
        assert_eq!(
            ownership(&diagnostic),
            DeliveryOwnership {
                owner_machine: None,
                authority: OwnerAuthority::Missing,
                owned_here: false,
            }
        );
    }

    // The workspace record claims this machine while the checkout is a replica
    // of another: neither answer may be trusted.
    let contradicted = unregistered
        .clone()
        .with_workspace_owner_machine_id(Some("fixture-machine"))
        .with_coordination_write_owner(Some("another-machine".into()));
    let diagnostic = evaluate_auto_task(&contradicted, &definition, false, Utc::now()).unwrap();
    assert_eq!(diagnostic.reason, "ownership_unresolved");
    assert_eq!(
        ownership(&diagnostic),
        DeliveryOwnership {
            owner_machine: None,
            authority: OwnerAuthority::Conflicting,
            owned_here: false,
        }
    );

    let consumer = consumer_key(&unregistered, "auto-task", &definition.name).unwrap();
    assert!(
        unregistered
            .automation_store()
            .unwrap()
            .automation_state(&consumer)
            .unwrap()
            .is_none(),
        "unresolved ownership never records a baseline"
    );
}

#[test]
fn a_delivery_routine_resolves_ownership_the_same_way() {
    let runtime = runtime().with_workspace_owner_machine_id(Some("another-machine"));
    let mut routine: orbit_types::workflow::RoutineDefinition = serde_json::from_value(json!({
        "schemaVersion": 1,
        "name": "delivery-routine",
        "enabled": true,
        "hosts": ["fixture-host"],
        "target": "job:delivery_pipeline",
        "trigger": {"deliveries_landed": {
            "branch": "agent-main",
            "threshold": 1,
            "max_wait_minutes": 60,
            "coverage": "integrated_qa_v1",
        }},
    }))
    .unwrap();

    let refused = super::super::evaluate_routine(&runtime, &routine, false, Utc::now()).unwrap();
    assert_eq!(refused.reason, "owned_elsewhere");
    assert_eq!(
        ownership(&refused).owner_machine.as_deref(),
        Some("another-machine")
    );
    assert_eq!(
        ownership(&super::super::inspect_routine(&runtime, &routine, Utc::now()).unwrap()),
        ownership(&refused)
    );

    routine
        .trigger
        .deliveries_landed
        .as_mut()
        .unwrap()
        .owner_machine = Some("fixture-machine".into());
    let owned = super::super::evaluate_routine(&runtime, &routine, false, Utc::now()).unwrap();
    assert_eq!(owned.reason, "baselined");
    assert!(ownership(&owned).owned_here);
}

#[test]
fn a_disabled_definition_reads_as_disabled_whoever_owns_it() {
    let runtime = runtime().with_workspace_owner_machine_id(Some("another-machine"));
    let mut definition = definition(&runtime, "disabled-owner", CoverageClass::IntegratedQaV1);
    definition.enabled = false;
    set_owner(&mut definition, None);

    assert_eq!(
        evaluate_auto_task(&runtime, &definition, false, Utc::now())
            .unwrap()
            .reason,
        "disabled"
    );
    assert_eq!(
        super::super::inspect_auto_task(&runtime, &definition, Utc::now())
            .unwrap()
            .reason,
        "disabled"
    );
}

#[test]
fn inspection_reports_delivery_admission_deferrals_without_mutating_state() {
    let runtime = runtime();
    let mut inspection_definition =
        definition(&runtime, "inspection", CoverageClass::IntegratedQaV1);
    let baseline = evaluate_auto_task(&runtime, &inspection_definition, false, Utc::now())
        .unwrap()
        .state
        .unwrap();
    let store = runtime.automation_store().unwrap();

    let before = store.automation_state(&baseline.consumer).unwrap();
    inspection_definition.template.title = "Changed inspection definition".into();
    assert_eq!(
        super::super::inspect_auto_task(&runtime, &inspection_definition, Utc::now())
            .unwrap()
            .reason,
        "definition_changed"
    );
    assert_eq!(
        store.automation_state(&baseline.consumer).unwrap(),
        before,
        "inspection must not repair a changed definition"
    );

    inspection_definition.template.title = "Examine batch".into();
    let mut pending = baseline.clone();
    pending.generation = 1;
    pending.pending_commits = vec!["pending-commit".into()];
    pending.pending = vec![Delivery {
        key: "direct:inspection".into(),
        repository: baseline.repository.clone(),
        branch: baseline.branch.clone(),
        before: baseline.observed.clone(),
        after: baseline.observed.clone(),
        commits: vec!["pending-commit".into()],
        task_ids: vec![],
        evidence_reference: "fixture".into(),
        evidence_digest: "fixture".into(),
        landed_at: Utc::now(),
    }];
    assert!(store.automation_commit(&baseline, &pending, None).unwrap());
    runtime.auto_task_mint(&inspection_definition.name).unwrap();
    let before = store.automation_state(&baseline.consumer).unwrap();
    assert_eq!(
        super::super::inspect_auto_task(&runtime, &inspection_definition, Utc::now())
            .unwrap()
            .reason,
        "open_instance"
    );
    assert_eq!(
        store.automation_state(&baseline.consumer).unwrap(),
        before,
        "inspection must not admit or advance an open-instance deferral"
    );

    let AutoTaskSchedule::Deliveries { deliveries_landed } = &mut inspection_definition.schedule
    else {
        unreachable!()
    };
    inspection_definition.enabled = false;
    deliveries_landed.owner_machine = None;
    assert_eq!(
        super::super::inspect_auto_task(&runtime, &inspection_definition, Utc::now())
            .unwrap()
            .reason,
        "definition_changed",
        "a changed definition takes precedence over the disabled owner state"
    );

    let baseline_definition = definition(&runtime, "disabled", CoverageClass::IntegratedQaV1);
    let mut disabled = baseline_definition;
    disabled.enabled = false;
    let AutoTaskSchedule::Deliveries { deliveries_landed } = &mut disabled.schedule else {
        unreachable!()
    };
    deliveries_landed.owner_machine = None;
    assert_eq!(
        super::super::inspect_auto_task(&runtime, &disabled, Utc::now())
            .unwrap()
            .reason,
        "disabled"
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
