//! Dispatching authorized handoffs to the owner landing job [ORB-12499].
//!
//! No drain, ship sweep or schedule runs in any of these tests: the only thing
//! that creates a landing job is an authorized handoff, and the only thing that
//! recovers one is the durable request it left behind. The submitted worker is
//! a no-op program, so what is asserted is the durable dispatch decision rather
//! than the landing itself, which is covered where it runs.

use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::*;
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::{TaskArtifact, TaskStatus};
use orbit_types::workflow::handoff::*;
use orbit_types::workflow::{ReviewTiming, automation::SourceRevision};
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
use crate::adapter::tool_host::test_support::{create_context_task, test_runtime};
use crate::application::job::JobRunListParams;
use crate::application::job::pipeline::worker_command_override;
use crate::application::landing::LANDING_JOB;

struct Owner {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    task_id: String,
    claim_id: String,
    handoff: TaskHandoff,
    observation: HandoffObservation,
    worker: ClaimInvocation,
}

/// An owner holding one accepted handoff for a task in review. `completion`
/// decides whether the ship contract carried completion authority, which is
/// what tells accepted-and-authorized work apart from review-only work.
fn owner(completion: &str, grant: Option<&str>) -> Owner {
    worker_command_override::set(["sh", "-c", "true"]);
    let (root, runtime, repo) = test_runtime();
    // The landing job has to be resolvable by name, so seed the managed
    // catalog the way an initialized host has it.
    crate::application::job::seed_default_jobs(
        &runtime.paths().global_dir.join("resources/jobs"),
        false,
    )
    .expect("seed managed jobs");
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
        completion: completion.into(),
        authorization_reference: grant.map(ToOwned::to_owned),
    };
    let admission = boundary
        .admit_task(
            &AdmissionIdentity::trusted_remote(ExecutionLocation {
                machine_id: "follower".into(),
                host_id: None,
            }),
            &AdmissionRequest {
                request_id: "pull".into(),
                caller_version: "test".into(),
                caller_schema: 1,
                caller_review_policy: "none".into(),
                run_context: AdmissionRunContext {
                    run_id: "drain".into(),
                    job_name: "auto".into(),
                    host_id: None,
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
    let run = ClaimRun {
        machine_id: "follower".into(),
        run_id: "leaf".into(),
    };
    runtime
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                task.id.clone(),
                claim.claim_id.clone(),
                "follower".into(),
                None,
            )),
            "bind",
            &ClaimMutation::Bind {
                run: run.clone(),
                ship,
            },
        )
        .expect("bind");
    let worker = ClaimInvocation::trusted_worker(
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
        delivery: HandoffDelivery::PullRequest { number: 7 },
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
            Some(&worker),
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
    Owner {
        _root: root,
        runtime,
        task_id: task.id,
        claim_id: claim.claim_id,
        handoff,
        observation: HandoffObservation {
            candidate,
            required_commands: vec!["make ci".into()],
        },
        worker,
    }
}

impl Owner {
    fn accept(&self) {
        self.runtime
            .accept_task_handoff(
                &self.worker,
                "handoff",
                self.handoff.clone(),
                self.observation.clone(),
            )
            .expect("accept");
    }

    fn approve(&self, request_id: &str) {
        let accepted = self
            .runtime
            .accepted_task_handoff(&self.claim_id)
            .expect("accepted");
        self.runtime
            .approve_task_handoff(
                &ClaimInvocation::trusted_operator(
                    self.task_id.clone(),
                    self.claim_id.clone(),
                    "owner".into(),
                ),
                request_id,
                accepted.handoff_id,
                self.handoff.candidate.clone(),
                self.observation.clone(),
            )
            .expect("approve");
    }

    fn handoff_id(&self) -> String {
        self.runtime
            .accepted_task_handoff(&self.claim_id)
            .expect("accepted")
            .handoff_id
    }

    fn landing_runs(&self) -> Vec<String> {
        self.runtime
            .list_job_runs(JobRunListParams {
                job_id: Some(LANDING_JOB.to_string()),
                ..Default::default()
            })
            .expect("runs")
            .into_iter()
            .map(|run| run.run_id)
            .collect()
    }

    fn attempt(&self) -> Option<LandingAttempt> {
        self.runtime
            .landing_attempts()
            .expect("attempts")
            .into_iter()
            .next()
    }
}

/// An owner-side grant that authorizes completion for this task, the way an
/// operator-authorized delivery run carries one.
fn completion_grant(runtime: &OrbitRuntime, id: &str, task_id: &str) {
    use chrono::Utc;
    use orbit_types::workflow::{GrantLimits, GrantRights, GrantStatus, OperationGrant};
    runtime
        .sqlite_store()
        .expect("store")
        .operation_grant_insert(&OperationGrant {
            id: id.into(),
            workspace_id: runtime.workspace_id().expect("workspace"),
            actor: "owner".into(),
            source: "test".into(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            revision: 1,
            task_ids: vec![task_id.to_string()],
            rights: GrantRights {
                complete: true,
                ..Default::default()
            },
            limits: GrantLimits {
                leaf_ceiling: 1,
                preparation_due_seconds: 60,
                recovery_episodes_per_task: 1,
                recovery_minutes_per_task: 1,
            },
            policy: serde_json::json!({}),
            policy_version: 1,
            status: GrantStatus::Active,
            stopped: None,
            revoked: None,
        })
        .expect("grant");
}

#[test]
fn an_approved_handoff_dispatches_exactly_one_owner_landing_job() {
    let owner = owner("review", None);
    owner.accept();

    // Review-only work records no request, so nothing is dispatched for it.
    assert!(
        owner
            .runtime
            .landing_start_requests()
            .expect("outbox")
            .is_empty()
    );
    assert!(owner.landing_runs().is_empty());
    assert!(owner.attempt().is_none());

    // A worker that stays alive for the rest of the test, so "this handoff is
    // already being landed" is a fact rather than a race with a process exit.
    worker_command_override::set(["sh", "-c", "sleep 30"]);
    owner.approve("approval");

    let runs = owner.landing_runs();
    assert_eq!(runs.len(), 1, "approval dispatched the landing job");
    let attempt = owner.attempt().expect("attempt");
    assert_eq!(attempt.handoff_id, owner.handoff_id());
    assert_eq!(attempt.task_id, owner.task_id);
    assert_eq!(attempt.attempt, 1);
    assert_eq!(attempt.job_run_id.as_deref(), Some(runs[0].as_str()));
    assert_eq!(attempt.state, LandingAttemptState::Dispatched);

    // Dispatching again finds live work rather than starting a second merge.
    let repeated = owner
        .runtime
        .dispatch_landing_requests()
        .expect("second pass");
    assert!(repeated.is_empty(), "{repeated:?}");
    assert_eq!(owner.landing_runs(), runs);
    assert_eq!(
        owner.runtime.landing_attempts().expect("attempts").len(),
        1,
        "one attempt row per handoff, whatever dispatch is asked to do"
    );
    assert_eq!(
        owner.runtime.get_task(&owner.task_id).expect("task").status,
        TaskStatus::Review
    );
}

#[test]
fn accepting_a_completion_authorized_handoff_dispatches_without_a_drain_or_sweep() {
    let owner = owner("done", Some("grant"));
    completion_grant(&owner.runtime, "grant", &owner.task_id);
    owner.accept();

    assert_eq!(
        owner
            .runtime
            .landing_start_requests()
            .expect("outbox")
            .len(),
        1
    );
    assert_eq!(
        owner.landing_runs().len(),
        1,
        "the authorized handoff dispatched its own landing job"
    );
    assert_eq!(owner.attempt().expect("attempt").attempt, 1);
}

#[test]
fn a_pending_request_whose_job_never_started_is_recovered_by_the_next_pass() {
    let owner = owner("review", None);
    owner.accept();
    owner.approve("approval");
    let first = owner.landing_runs();

    // The owner job died: its run is terminal without the handoff having
    // landed, so the attempt it carried is no longer live work.
    owner
        .runtime
        .cancel_job_run(&first[0])
        .expect("terminalize the dead run");
    assert!(owner.attempt().expect("attempt").job_run_id.is_some());

    let recovered = owner
        .runtime
        .dispatch_landing_requests()
        .expect("recovery pass");

    assert_eq!(recovered.len(), 1);
    assert!(recovered[0].submitted);
    let attempt = owner.attempt().expect("attempt");
    assert_eq!(
        attempt.attempt, 2,
        "a job that is gone is not resumed; the next attempt owns the handoff"
    );
    assert_eq!(
        attempt.job_run_id.as_deref(),
        Some(recovered[0].run_id.as_str())
    );
    assert_ne!(recovered[0].run_id, first[0]);
}

#[test]
fn a_revoked_request_is_never_dispatched_again() {
    let owner = owner("review", None);
    owner.accept();
    owner.approve("approval");
    let operator = ClaimInvocation::trusted_operator(
        owner.task_id.clone(),
        owner.claim_id.clone(),
        "owner".into(),
    );
    owner
        .runtime
        .revoke_task_handoff(&operator, "revoke", owner.handoff_id(), "withdrawn".into())
        .expect("revoke");

    // The job that was already live keeps its own authority rechecks; what the
    // outbox must never do is hand a revoked request to a new one.
    owner
        .runtime
        .cancel_job_run(&owner.landing_runs()[0])
        .expect("terminalize the dispatched run");
    let dispatched = owner
        .runtime
        .dispatch_landing_requests()
        .expect("dispatch pass");

    assert!(dispatched.is_empty());
    assert_eq!(
        owner.runtime.landing_start_requests().expect("outbox")[0].state,
        LandingStartState::Revoked
    );
    let refused = owner
        .runtime
        .land_handoff(&owner.handoff_id())
        .expect_err("revoked authority cannot be re-dispatched");
    assert!(
        refused.to_string().contains("landing"),
        "the refusal names the landing authority: {refused}"
    );
    assert_eq!(
        owner.landing_runs().len(),
        1,
        "no second landing job is created for revoked authority"
    );
}

#[test]
fn a_landing_decision_requires_an_observation_of_the_accepted_candidate() {
    use orbit_engine::{HandoffLandingStep, HandoffLandingUpdate};

    let owner = owner("review", None);
    owner.accept();
    owner.approve("approval");
    let handoff_id = owner.handoff_id();

    let context = owner
        .runtime
        .handoff_landing_context(&handoff_id)
        .expect("landing context");
    assert_eq!(context.task_id, owner.task_id);
    assert_eq!(context.candidate, owner.handoff.candidate);
    assert_eq!(context.unresolved_merge_intent, None);

    let mut changed = context.candidate.clone();
    changed.candidate.commit = "e".repeat(40);
    let refused = owner
        .runtime
        .record_handoff_landing(&HandoffLandingUpdate {
            handoff_id: handoff_id.clone(),
            step: HandoffLandingStep::PublishIntent {
                intent_id: "sent".into(),
            },
            observed: Some(changed),
            evidence: "provider request".into(),
        })
        .expect_err("a different candidate is not this handoff's");
    assert!(refused.to_string().contains("not the accepted one"));

    let missing = owner
        .runtime
        .record_handoff_landing(&HandoffLandingUpdate {
            handoff_id: handoff_id.clone(),
            step: HandoffLandingStep::Complete,
            observed: None,
            evidence: "merged".into(),
        })
        .expect_err("completion without an observation");
    assert!(missing.to_string().contains("require an owner candidate"));

    // The accepted candidate does record intent, and the next context read
    // reports that uncertainty back to the landing attempt.
    owner
        .runtime
        .record_handoff_landing(&HandoffLandingUpdate {
            handoff_id: handoff_id.clone(),
            step: HandoffLandingStep::PublishIntent {
                intent_id: "sent".into(),
            },
            observed: Some(context.candidate.clone()),
            evidence: "provider request for the pinned candidate".into(),
        })
        .expect("intent published");
    assert_eq!(
        owner
            .runtime
            .handoff_landing_context(&handoff_id)
            .expect("context")
            .unresolved_merge_intent
            .as_deref(),
        Some("sent")
    );
    assert_eq!(
        owner.runtime.get_task(&owner.task_id).expect("task").status,
        TaskStatus::Review
    );
}
