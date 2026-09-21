//! Captured review admission [ORB-11333].

use orbit_types::workflow::{REVIEW_ADMISSION_KEY, ReviewAdmission, ReviewTiming};
use serde_json::json;

use super::{GATED_CONFIG, fixture, seed_task};
use crate::application::review::install_review_admission;

#[test]
fn delivery_submissions_capture_the_effective_policy_with_its_sources() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let mut input = json!({ "task_ids": ["ORB-1"] });

    install_review_admission(runtime, "task_pr_pipeline", &mut input, None, false)
        .expect("captured");
    let admission = ReviewAdmission::from_run_input(&input)
        .expect("readable")
        .expect("present");
    assert_eq!(admission.timing, ReviewTiming::BeforePr);
    assert_eq!(admission.timing_source, "workspace");
    assert_eq!(admission.crew.as_deref(), Some("reviewers"));
    assert_eq!(admission.budget.reviewer_starts, 2);
    assert_eq!(admission.budget.minutes, 30);
    assert_eq!(
        admission.policy_version,
        orbit_config::OPERATION_POLICY_VERSION
    );

    // Jobs outside the delivery family carry nothing.
    let mut other = json!({ "task_ids": ["ORB-1"] });
    install_review_admission(runtime, "task_pilot_pipeline", &mut other, None, false)
        .expect("ignored");
    assert!(other.get(REVIEW_ADMISSION_KEY).is_none());
}

#[test]
fn ordinary_input_cannot_supply_or_widen_the_review_admission() {
    let fixture = fixture("[operation]\nreview_policy = \"none\"\n");
    let runtime = &fixture.runtime;
    let mut forged = json!({
        "task_ids": ["ORB-1"],
        "review": { "contract_version": 1, "policy_version": 2, "timing": "none",
                    "timing_source": "forged", "crew_source": "forged",
                    "budget": { "reviewer_starts": 9, "repair_cycles": 9, "minutes": 999 },
                    "captured_at": "2026-09-07T00:00:00Z" },
    });
    let error = install_review_admission(runtime, "task_pr_pipeline", &mut forged, None, false)
        .expect_err("reserved key is refused");
    assert!(
        error.to_string().contains("reserved `review` field"),
        "{error}"
    );

    // A resume keeps whatever its persisted input carries.
    let mut resumed = forged.clone();
    install_review_admission(runtime, "task_pr_pipeline", &mut resumed, None, true)
        .expect("resume keeps persisted input");
    assert_eq!(resumed["review"]["timing_source"], "forged");
}

#[test]
fn children_inherit_their_parents_snapshot_even_after_the_preference_changes() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "child");
    let parent = super::admitted_run(
        runtime,
        "task_auto_pipeline",
        std::slice::from_ref(&task.id),
    );

    // Rewrite the workspace preference to `none`: the running lineage must
    // keep the gate it was admitted with, so a rollback cannot weaken it.
    std::fs::write(
        fixture.repo.join(".orbit/config.toml"),
        "[operation]\nreview_policy = \"none\"\n",
    )
    .expect("edit config");

    let mut child = json!({ "task_ids": [task.id] });
    install_review_admission(
        runtime,
        "task_pr_pipeline",
        &mut child,
        Some(&parent.run_id),
        false,
    )
    .expect("inherit");
    let inherited = ReviewAdmission::from_run_input(&child)
        .expect("readable")
        .expect("present");
    let captured = ReviewAdmission::from_run_input(parent.input.as_ref().expect("parent input"))
        .expect("readable")
        .expect("present");
    assert_eq!(inherited, captured);
    assert_eq!(inherited.timing, ReviewTiming::BeforePr);
}

#[test]
fn before_pr_is_refused_on_the_local_only_route() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let mut input = json!({ "task_ids": ["ORB-1"] });
    let error = install_review_admission(runtime, "task_local_pipeline", &mut input, None, false)
        .expect_err("local route cannot honour before-pr");
    assert!(
        error
            .to_string()
            .contains("no meaning on the local-only delivery route"),
        "{error}"
    );

    let after_landing = fixture;
    std::fs::write(
        after_landing.repo.join(".orbit/config.toml"),
        "[operation]\nreview_policy = \"after-landing\"\n",
    )
    .expect("edit config");
    let (_root, runtime, _) =
        crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_config(Some(
            "[operation]\nreview_policy = \"after-landing\"\n",
        ));
    let mut local = json!({ "task_ids": ["ORB-1"] });
    install_review_admission(&runtime, "task_local_pipeline", &mut local, None, false)
        .expect("after-landing is fine locally");
    assert_eq!(local["review"]["timing"], "after-landing");
}

/// [ORB-12491] There is no assembly exemption any more: a `before-pr` policy
/// is refused on the local-only route for every parent, without exception.
#[test]
fn no_parent_can_assemble_a_local_child_under_before_pr() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let leaf = seed_task(runtime, "leaf");
    let parent = super::admitted_run(
        runtime,
        "task_auto_pipeline",
        std::slice::from_ref(&leaf.id),
    );

    let mut child = json!({ "task_ids": [leaf.id] });
    let error = install_review_admission(
        runtime,
        "task_local_pipeline",
        &mut child,
        Some(&parent.run_id),
        false,
    )
    .expect_err("ordinary local-only child still refused");
    assert!(
        error
            .to_string()
            .contains("no meaning on the local-only delivery route"),
        "{error}"
    );
}

#[test]
fn owner_domain_accepts_typed_handoff_but_only_operator_can_approve() {
    use crate::adapter::tool_host::test_support::{create_context_task, test_runtime};
    use orbit_store::TaskCommitBoundary;
    use orbit_store::contracts::*;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::{TaskArtifact, TaskStatus};
    use orbit_types::workflow::{automation::SourceRevision, handoff::*};
    use sha2::{Digest, Sha256};

    let (_root, runtime, repo) = test_runtime();
    let task = create_context_task(&runtime, &repo, TaskStatus::Backlog, &["src/a.rs"]);
    let workspace_id = runtime.workspace_id().expect("workspace");
    let boundary = TaskCommitBoundary::new(
        runtime.sqlite_store().expect("store"),
        TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).expect("registry"),
        workspace_id.clone(),
    )
    .expect("boundary");
    let ship = AdmissionShipContract {
        mode: "pr".into(),
        base_branch: "agent-main".into(),
        landing_branch: "agent-main".into(),
        review_policy: "none".into(),
        completion: "review".into(),
        authorization_reference: None,
    };
    let admission = boundary
        .admit_task(
            &AdmissionIdentity::trusted_remote(ExecutionLocation {
                machine_id: "follower".into(),
                machine_name: None,
            }),
            &AdmissionRequest {
                request_id: "pull".into(),
                caller_version: "test".into(),
                caller_schema: 1,
                caller_review_policy: "none".into(),
                run_context: AdmissionRunContext {
                    run_id: "drain".into(),
                    job_name: "auto".into(),
                    machine_name: None,
                },
                ship: ship.clone(),
            },
            "test",
            &repo,
            &runtime.data_root(),
        )
        .expect("admission");
    let AdmissionLookup::Found { receipt, .. } = admission else {
        panic!("receipt")
    };
    let claim = receipt.claim.expect("claim");
    let context = ClaimInvocation::trusted_worker(
        task.id.clone(),
        claim.claim_id.clone(),
        "follower".into(),
        None,
    );
    let run = ClaimRun {
        machine_id: "follower".into(),
        run_id: "leaf".into(),
    };
    runtime
        .mutate_execution_claim(
            Some(&context),
            "bind",
            &ClaimMutation::Bind {
                run: run.clone(),
                ship,
            },
        )
        .expect("bind");
    let context = ClaimInvocation::trusted_worker(
        task.id.clone(),
        claim.claim_id.clone(),
        "follower".into(),
        Some(run),
    );
    let candidate = HandoffCandidate {
        repository: "owner/repository".into(),
        source_branch: "attempt/leaf".into(),
        base_branch: "agent-main".into(),
        landing_branch: "agent-main".into(),
        candidate: SourceRevision {
            commit: "a".repeat(40),
            tree: "b".repeat(40),
        },
        base: SourceRevision {
            commit: "c".repeat(40),
            tree: "d".repeat(40),
        },
        delivery: HandoffDelivery::PullRequest { number: 1 },
    };
    let log = HandoffValidationLog {
        schema_version: 1,
        workspace_id: workspace_id.clone(),
        task_id: task.id.clone(),
        claim_id: claim.claim_id.clone(),
        machine_id: "follower".into(),
        run_id: "leaf".into(),
        candidate: candidate.clone(),
        tested_head: candidate.candidate.commit.clone(),
        command: "make ci".into(),
        exit_code: 0,
        output: "captured successful checks".into(),
    };
    let content = serde_json::to_vec(&log).expect("log");
    let reference = HandoffArtifactRef {
        path: "checks.json".into(),
        sha256: format!("{:x}", Sha256::digest(&content)),
    };
    runtime
        .mutate_execution_claim(
            Some(&context),
            "evidence",
            &ClaimMutation::Evidence(ClaimEvidence {
                artifacts: vec![TaskArtifact {
                    path: reference.path.clone(),
                    content,
                    media_type: "application/json".into(),
                    created_by: None,
                }],
                ..Default::default()
            }),
        )
        .expect("owner artifact");
    let handoff = TaskHandoff {
        schema_version: 1,
        workspace_id,
        task_id: task.id.clone(),
        claim_id: claim.claim_id.clone(),
        machine_id: "follower".into(),
        run_id: "leaf".into(),
        candidate: candidate.clone(),
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary: "Outcome: success\nValidated exact candidate".into(),
        validation: vec![reference],
    };
    let observation = HandoffObservation {
        candidate: candidate.clone(),
        required_commands: vec!["make ci".into()],
    };
    runtime
        .accept_task_handoff(&context, "handoff", handoff, observation.clone())
        .expect("accept");
    assert!(runtime.landing_start_requests().expect("outbox").is_empty());
    let accepted = runtime
        .accepted_task_handoff(&claim.claim_id)
        .expect("accepted");
    assert!(
        runtime
            .approve_task_handoff(
                &context,
                "approve",
                accepted.handoff_id.clone(),
                candidate.clone(),
                observation.clone()
            )
            .is_err()
    );
    let operator =
        ClaimInvocation::trusted_operator(task.id.clone(), claim.claim_id, "owner".into());
    runtime
        .approve_task_handoff(
            &operator,
            "approve",
            accepted.handoff_id.clone(),
            candidate,
            observation,
        )
        .expect("operator approval");
    assert_eq!(runtime.landing_start_requests().expect("outbox").len(), 1);
    runtime
        .revoke_task_handoff(&operator, "revoke", accepted.handoff_id, "withdraw".into())
        .expect("revoke");
    assert_eq!(
        runtime.landing_start_requests().expect("outbox")[0].state,
        LandingStartState::Revoked
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Review
    );
}
