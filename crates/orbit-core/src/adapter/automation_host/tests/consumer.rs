//! Real Git + task registry + artifact tool consumer tests, without provider/network I/O.

use crate::{
    OrbitRuntime,
    application::{auto_tasks::AutoTaskAddParams, task::TaskUpdateParams},
};
use chrono::Utc;
use orbit_automation::consumers::{
    COVERAGE_ARTIFACT, consumer_key, evaluate_auto_task, record_direct_landing_intent,
};
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

#[test]
fn replay_proves_the_exact_sep_8_double_rebase_mapping() {
    let fixture = tempfile::tempdir().unwrap();
    let fixture_root = fixture.path().join("repo");
    std::fs::create_dir_all(fixture_root.join(".orbit")).unwrap();
    git(&fixture_root, &["init", "--initial-branch=agent-main"]);
    git(&fixture_root, &["config", "user.name", "Test"]);
    git(
        &fixture_root,
        &["config", "user.email", "test@example.invalid"],
    );

    std::fs::write(fixture_root.join(".orbit/stable.toml"), "version = 1\n").unwrap();
    std::fs::write(fixture_root.join("ordinary.txt"), "base\n").unwrap();
    git(
        &fixture_root,
        &["add", ".orbit/stable.toml", "ordinary.txt"],
    );
    git(&fixture_root, &["commit", "-m", "base"]);
    let base = git(&fixture_root, &["rev-parse", "HEAD"]);

    git(&fixture_root, &["checkout", "-b", "orphan"]);
    std::fs::write(fixture_root.join("payload.txt"), b"payload\0P\n").unwrap();
    git(&fixture_root, &["add", "payload.txt"]);
    git(&fixture_root, &["commit", "-m", "payload"]);
    let orphan = git(&fixture_root, &["rev-parse", "HEAD"]);

    git(&fixture_root, &["checkout", "agent-main"]);
    std::fs::write(fixture_root.join("inserted.txt"), "disjoint Q\n").unwrap();
    git(&fixture_root, &["add", "inserted.txt"]);
    git(&fixture_root, &["commit", "-m", "inserted"]);
    let inserted = git(&fixture_root, &["rev-parse", "HEAD"]);

    std::fs::write(fixture_root.join("payload.txt"), b"payload\0P\n").unwrap();
    git(&fixture_root, &["add", "payload.txt"]);
    git(&fixture_root, &["commit", "-m", "payload"]);
    let canonical = git(&fixture_root, &["rev-parse", "HEAD"]);

    git(
        &fixture_root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/constellation-works/orbit.git",
        ],
    );

    assert_eq!(
        git(&fixture_root, &["diff", "--binary", &base, &orphan]),
        git(&fixture_root, &["diff", "--binary", &inserted, &canonical]),
        "the orphan and canonical commits have the same full parent-relative patch"
    );
    assert_eq!(
        git(&fixture_root, &["rev-parse", &format!("{orphan}:.orbit")]),
        git(
            &fixture_root,
            &["rev-parse", &format!("{canonical}:.orbit")]
        ),
        "the replay candidates have the same stable Orbit tree"
    );
    assert_eq!(
        git(&fixture_root, &["rev-parse", &format!("{orphan}^")]),
        base
    );
    assert_eq!(
        git(&fixture_root, &["rev-parse", &format!("{canonical}^")]),
        inserted
    );
    assert_eq!(
        git(&fixture_root, &["merge-base", &orphan, &canonical]),
        base
    );

    let source = orbit_automation::source::Source::new(&fixture_root);
    let old = source.revision(&orphan).unwrap();
    let covered = source.revision(&base).unwrap();
    let state = AutomationState {
        members: None,
        consumer: "ws/qa".into(),
        epoch: "v1".into(),
        trigger: None,
        repository: "constellation-works/orbit".into(),
        branch: "agent-main".into(),
        generation: 41,
        baseline: covered.clone(),
        observed: old,
        covered: covered.clone(),
        pending_commits: vec![orphan.clone()],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: [(orphan.clone(), "evidence_pending".into())]
            .into_iter()
            .collect(),
        associations: Default::default(),
        active: None,
    };
    let provider_lookups = std::cell::RefCell::new(Vec::new());
    let provider = |_: &str, sha: &str| {
        provider_lookups.borrow_mut().push(sha.to_owned());
        assert_eq!(sha, inserted, "the inserted commit needs provider proof");
        Ok(json!([{
            "number": 1586,
            "html_url": "https://github.com/constellation-works/orbit/pull/1586",
            "merge_commit_sha": inserted,
            "merged_at": "2026-09-08T00:00:00Z",
            "base": {"ref": "agent-main", "repo": {"full_name": "constellation-works/orbit"}}
        }])
        .to_string())
    };

    let (page, proof) = source
        .replay_history_with_lookup("agent-main", &state, &provider, 0)
        .unwrap();
    assert_eq!(proof.captured_generation, 41);
    assert_eq!(proof.captured_head.commit, canonical);
    assert_eq!(proof.common_base.commit, base);
    assert_eq!(proof.old_observed.commit, orphan);
    assert_eq!(proof.new_observed.commit, canonical);
    assert_eq!(proof.mappings.len(), 1);
    assert_eq!(proof.mappings[0].orphan.commit, orphan);
    assert_eq!(proof.mappings[0].canonical.commit, canonical);
    assert_eq!(proof.unchanged_baseline, covered);
    assert_eq!(proof.unchanged_covered, covered);
    assert_eq!(page.commits, vec![inserted.clone(), canonical.clone()]);
    assert_eq!(
        provider_lookups.borrow().as_slice(),
        [inserted.clone()].as_slice()
    );
    assert_eq!(page.deliveries.len(), 1);
    assert!(page.deliveries.iter().any(|delivery| {
        delivery.key == "pr:constellation-works/orbit:agent-main:1586"
            && delivery.commits == vec![inserted.clone()]
    }));
    assert_eq!(
        page.unresolved.get(&canonical).map(String::as_str),
        Some("evidence_pending")
    );
    assert!(!page.associations.contains_key(&canonical));

    git(&fixture_root, &["checkout", "--detach", &canonical]);
    git(&fixture_root, &["branch", "-f", "agent-main", &inserted]);
    let error = orbit_automation::consumers::recovery::ensure_replay_head(
        &orbit_automation::source::Source::new(&fixture_root),
        "agent-main",
        &proof.captured_head,
    )
    .expect_err("a configured-head change must refuse apply");
    assert!(matches!(
        error,
        orbit_common::OrbitError::InvalidInput(reason)
            if reason == orbit_types::workflow::automation::recovery::refusal::HISTORY_HEAD_CHANGED
    ));
    git(&fixture_root, &["branch", "-f", "agent-main", &canonical]);

    let mut missing = state.clone();
    missing.observed.commit = "0000000000000000000000000000000000000000".into();
    let error = orbit_automation::source::Source::new(&fixture_root)
        .replay_history_with_lookup("agent-main", &missing, &provider, 0)
        .expect_err("the orphan object is mandatory");
    assert!(matches!(
        error,
        orbit_automation::AutomationError::Refused(reason)
            if reason == orbit_types::workflow::automation::recovery::refusal::HISTORY_OBJECT_MISSING
    ));

    let mut unreachable = state;
    unreachable.covered = source.revision(&orphan).unwrap();
    unreachable.baseline = unreachable.covered.clone();
    let error = orbit_automation::source::Source::new(&fixture_root)
        .replay_history_with_lookup("agent-main", &unreachable, &provider, 0)
        .expect_err("an orphaned covered boundary cannot be replayed");
    assert!(matches!(
        error,
        orbit_automation::AutomationError::Refused(reason)
            if reason == orbit_types::workflow::automation::recovery::refusal::HISTORY_BOUNDARY_UNREACHABLE
    ));
}

#[test]
fn replay_refuses_ambiguous_mapping_and_bounded_traversal_exhaustion() {
    use orbit_automation::AutomationError;
    use orbit_types::workflow::automation::recovery::refusal;

    let ambiguous = vec![(1, "canonical-a".into()), (2, "canonical-b".into())];
    assert!(matches!(
        orbit_automation::source::unique_mapping_candidate(&ambiguous, None),
        Err(AutomationError::Refused(reason)) if reason == refusal::HISTORY_MAPPING_AMBIGUOUS
    ));
    assert!(matches!(
        orbit_automation::source::validate_replay_range_lengths(1001, 1),
        Err(AutomationError::Refused(reason)) if reason == refusal::HISTORY_TRAVERSAL_LIMIT
    ));
    assert!(matches!(
        orbit_automation::source::validate_replay_range_lengths(1, 1001),
        Err(AutomationError::Refused(reason)) if reason == refusal::HISTORY_TRAVERSAL_LIMIT
    ));
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
        orbit_automation::consumers::mint(&runtime, &qa, &claim).unwrap(),
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
    let source = orbit_automation::source::Source::new(&runtime.paths().repo_root);
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
    let source = orbit_automation::source::Source::new(&runtime.paths().repo_root);
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

    let inspection =
        orbit_automation::consumers::inspect_auto_task(&runtime, &definition, Utc::now()).unwrap();
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
    let adopted =
        orbit_automation::consumers::inspect_auto_task(&runtime, &definition, Utc::now()).unwrap();
    assert_eq!(
        adopted.reason, "not_due",
        "the identical resolved owner is not a definition change"
    );
    assert_eq!(adopted.state.unwrap().epoch, pinned_epoch);

    set_owner(&mut definition, Some("another-machine"));
    assert_eq!(
        orbit_automation::consumers::inspect_auto_task(&runtime, &definition, Utc::now())
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
        orbit_automation::consumers::inspect_auto_task(&runtime, &definition, Utc::now()).unwrap(),
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
        orbit_automation::consumers::inspect_auto_task(&unregistered, &definition, Utc::now())
            .unwrap(),
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

    let refused =
        orbit_automation::consumers::evaluate_routine(&runtime, &routine, false, Utc::now())
            .unwrap();
    assert_eq!(refused.reason, "owned_elsewhere");
    assert_eq!(
        ownership(&refused).owner_machine.as_deref(),
        Some("another-machine")
    );
    assert_eq!(
        ownership(
            &orbit_automation::consumers::inspect_routine(&runtime, &routine, Utc::now()).unwrap()
        ),
        ownership(&refused)
    );

    routine
        .trigger
        .deliveries_landed
        .as_mut()
        .unwrap()
        .owner_machine = Some("fixture-machine".into());
    let owned =
        orbit_automation::consumers::evaluate_routine(&runtime, &routine, false, Utc::now())
            .unwrap();
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
        orbit_automation::consumers::inspect_auto_task(&runtime, &definition, Utc::now())
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
        orbit_automation::consumers::inspect_auto_task(
            &runtime,
            &inspection_definition,
            Utc::now()
        )
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
        orbit_automation::consumers::inspect_auto_task(
            &runtime,
            &inspection_definition,
            Utc::now()
        )
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
        orbit_automation::consumers::inspect_auto_task(
            &runtime,
            &inspection_definition,
            Utc::now()
        )
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
        orbit_automation::consumers::inspect_auto_task(&runtime, &disabled, Utc::now())
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
    let source = orbit_automation::source::Source::new(&runtime.paths().repo_root);
    assert!(matches!(
        orbit_automation::consumers::job_outcome(&runtime, &source, &attempt).unwrap(),
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
        orbit_automation::consumers::job_outcome(&runtime, &source, &attempt).unwrap()
    else {
        panic!("persisted step must supply evidence");
    };
    let receipt = validate(&attempt, &facts, Utc::now()).unwrap();
    assert_eq!(
        receipt.evidence_reference,
        format!("run:{}/step:0", run.run_id)
    );
    attempt.input_digest = "changed".into();
    assert!(orbit_automation::consumers::job_outcome(&runtime, &source, &attempt).is_err());
}

/// The live incident shape: a delivery consumer whose examination task was
/// archived without evidence, then stalled by tonight's threshold retuning
/// [ORB-12295].
#[test]
fn recovery_adopts_retuned_settings_and_reissues_an_archived_unevidenced_action() {
    use orbit_types::workflow::automation::recovery::{RecoveryPreview, RecoveryRequest};

    let runtime = runtime();
    let definition = definition(&runtime, "delivery-qa", CoverageClass::IntegratedQaV1);
    let baseline = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .baseline;

    let landed = commit(&runtime.paths().repo_root, "unexamined change");
    let delivery_run = runtime
        .stores()
        .jobs()
        .insert_job_run("direct_fixture", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    record_direct_landing_intent(
        &runtime,
        &DirectLandingRequest {
            run_id: delivery_run.run_id,
            branch: "agent-main".into(),
            before_commit: baseline.commit.clone(),
            after_commit: landed.clone(),
        },
    )
    .unwrap();

    let archived_task = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .active
        .unwrap()
        .action_id
        .unwrap();

    // The examination task is closed without ever attaching coverage evidence.
    runtime
        .update_task(
            &archived_task,
            TaskUpdateParams {
                status: Some(TaskStatus::Rejected),
                ..Default::default()
            },
        )
        .unwrap();
    let settled = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(settled.state, BatchState::Failed);
    assert_eq!(
        settled.reason.as_deref(),
        Some("task_closed_without_accepted_evidence")
    );

    // Tonight's retuning stalls the consumer: it stops examining new landings.
    let mut retuned = definition.clone();
    let AutoTaskSchedule::Deliveries { deliveries_landed } = &mut retuned.schedule else {
        unreachable!("delivery fixture")
    };
    deliveries_landed.threshold = 2;
    deliveries_landed.max_wait_minutes = 30;
    assert_eq!(
        evaluate_auto_task(&runtime, &retuned, false, Utc::now())
            .unwrap()
            .reason,
        "definition_changed"
    );

    let preview = orbit_automation::consumers::recover_auto_task(
        &runtime,
        &retuned,
        &RecoveryRequest::default(),
        Utc::now(),
    )
    .unwrap();
    assert_eq!(preview.reason, "definition_changed");
    assert_eq!(
        preview.identity.changes,
        vec!["threshold".to_string(), "max_wait_minutes".to_string()]
    );
    assert_eq!(preview.debt.pending_deliveries, 1);
    assert_eq!(preview.debt.covered, baseline);
    assert!(preview.refusals.is_empty());
    assert!(preview.applied.is_empty());
    let action = preview.action.expect("the archived action is retained");
    assert_eq!(action.action_id.as_ref(), Some(&archived_task));
    assert!(action.reissuable);

    let applied = orbit_automation::consumers::recover_auto_task(
        &runtime,
        &retuned,
        &RecoveryRequest {
            adopt_settings: true,
            reissue_action: true,
            replay_history: false,
            reason: "adopt tonight's QA threshold and re-examine the unpaid landing".into(),
        },
        Utc::now(),
    )
    .unwrap();
    assert_eq!(
        applied.applied,
        vec![
            RecoveryPreview::ADOPTED_SETTINGS,
            RecoveryPreview::REISSUED_ACTION
        ]
    );
    assert_eq!(applied.history.len(), 1);

    // The reissue mints a new linked task; the archived one is left closed.
    let reissued = evaluate_auto_task(&runtime, &retuned, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .active
        .unwrap();
    let reissued_task = reissued.action_id.clone().unwrap();
    assert_ne!(reissued_task, archived_task);
    assert_eq!(reissued.state, BatchState::Admitted);
    assert_eq!(
        reissued.batch, settled.batch,
        "the obligations are unchanged"
    );
    assert_eq!(
        runtime.get_task(&archived_task).unwrap().status,
        TaskStatus::Rejected,
        "recovery never reopens a terminal task"
    );
    assert!(
        runtime
            .get_task(&reissued_task)
            .unwrap()
            .description
            .contains(&archived_task),
        "the reissued task names the action it replaces"
    );
    assert_eq!(
        evaluate_auto_task(&runtime, &retuned, false, Utc::now())
            .unwrap()
            .state
            .unwrap()
            .covered,
        baseline,
        "an authorized retry is not coverage"
    );

    // Only evidence from the reissued task's assigned executor covers the debt.
    let worker = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "examination",
            1,
            Utc::now(),
            Some(json!({ "task_id": reissued_task })),
            None,
        )
        .unwrap();
    runtime
        .update_task(
            &reissued_task,
            TaskUpdateParams {
                job_run_id: Some(Some(worker.run_id.clone())),
                ..Default::default()
            },
        )
        .unwrap();
    let mut evidence = evidence_template(&reissued);
    evidence.examination_complete = true;
    evidence.checks = vec![ExaminationCheck {
        subject: "reissued obligations".into(),
        method: "fixture examination".into(),
        observation: "examined the unpaid landing".into(),
    }];
    attach(&runtime, &reissued, Some(&worker.run_id), &evidence);

    let covered = evaluate_auto_task(&runtime, &retuned, false, Utc::now()).unwrap();
    assert_eq!(covered.state.unwrap().covered.commit, landed);
    assert_eq!(covered.receipts.len(), 1);
    assert_eq!(
        covered.receipts[0].action_id, reissued_task,
        "coverage is attributed to the reissued action"
    );
}
