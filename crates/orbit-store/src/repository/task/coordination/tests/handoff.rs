use super::admission::{identity, pull, receipt, request};
use super::*;
use crate::contracts::*;
use orbit_common::OrbitError;
use orbit_types::workflow::{ReviewTiming, automation::SourceRevision, handoff::*};
use sha2::{Digest, Sha256};

pub(super) fn worker(c: &ExecutionClaim) -> ClaimInvocation {
    ClaimInvocation::trusted_worker(
        c.task_id.clone(),
        c.claim_id.clone(),
        c.executed_on.machine_id.clone(),
        Some(ClaimRun {
            machine_id: c.executed_on.machine_id.clone(),
            run_id: "leaf".into(),
        }),
    )
}
pub(super) fn operator(c: &ExecutionClaim) -> ClaimInvocation {
    ClaimInvocation::trusted_operator(
        c.task_id.clone(),
        c.claim_id.clone(),
        "owner-operator".into(),
    )
}
pub(super) fn observation(h: &TaskHandoff) -> HandoffObservation {
    HandoffObservation {
        candidate: h.candidate.clone(),
        required_commands: vec!["build".into(), "test".into()],
    }
}
pub(super) fn handoff(f: &Coordinated, c: &ExecutionClaim) -> TaskHandoff {
    let h = TaskHandoff {
        schema_version: 1,
        workspace_id: PARTITION_ID.into(),
        task_id: c.task_id.clone(),
        claim_id: c.claim_id.clone(),
        machine_id: c.executed_on.machine_id.clone(),
        run_id: "leaf".into(),
        candidate: HandoffCandidate {
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
            delivery: HandoffDelivery::PullRequest { number: 42 },
        },
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary: "Outcome: success\nRequired checks passed for pinned candidate".into(),
        validation: vec![],
    };
    logs(f, c, h, 0)
}
pub(super) fn with_validation_logs(
    f: &Coordinated,
    c: &ExecutionClaim,
    h: TaskHandoff,
) -> TaskHandoff {
    logs(f, c, h, 0)
}

fn logs(f: &Coordinated, c: &ExecutionClaim, mut h: TaskHandoff, exit_code: i32) -> TaskHandoff {
    let artifacts: Vec<_> = ["build", "test"]
        .iter()
        .map(|command| {
            let log = HandoffValidationLog {
                schema_version: 1,
                workspace_id: h.workspace_id.clone(),
                task_id: h.task_id.clone(),
                claim_id: h.claim_id.clone(),
                machine_id: h.machine_id.clone(),
                run_id: h.run_id.clone(),
                candidate: h.candidate.clone(),
                tested_head: h.candidate.candidate.commit.clone(),
                command: (*command).into(),
                exit_code,
                output: format!("{command} captured output"),
            };
            orbit_types::task::TaskArtifact {
                path: format!("{command}.json"),
                content: serde_json::to_vec(&log).expect("log"),
                media_type: "application/json".into(),
                created_by: None,
            }
        })
        .collect();
    h.validation = artifacts
        .iter()
        .map(|a| HandoffArtifactRef {
            path: a.path.clone(),
            sha256: format!("{:x}", Sha256::digest(&a.content)),
        })
        .collect();
    f.boundary()
        .mutate_execution_claim(
            Some(&worker(c)),
            &format!("logs-{:x}", Sha256::digest(&artifacts[0].content)),
            &ClaimMutation::Evidence(ClaimEvidence {
                artifacts,
                ..Default::default()
            }),
        )
        .expect("persist owner logs");
    h
}
pub(super) fn fixture(
    ship: AdmissionShipContract,
) -> (TempDir, Coordinated, ExecutionClaim, TaskHandoff) {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    f.create_task("typed handoff");
    let mut req = request("first");
    req.ship = ship.clone();
    let admission = if ship.mode == "local" {
        f.boundary()
            .admit_task(
                &AdmissionIdentity::trusted_local(identity().location().clone()),
                &req,
                "test",
                f.orbit_dir.parent().expect("repo"),
                &f.orbit_dir,
            )
            .expect("local admission")
    } else {
        pull(&f, &req)
    };
    let c = receipt(admission).claim.expect("claim");
    let unbound = ClaimInvocation::trusted_worker(
        c.task_id.clone(),
        c.claim_id.clone(),
        c.executed_on.machine_id.clone(),
        None,
    );
    f.boundary()
        .mutate_execution_claim(
            Some(&unbound),
            "bind",
            &ClaimMutation::Bind {
                run: ClaimRun {
                    machine_id: c.executed_on.machine_id.clone(),
                    run_id: "leaf".into(),
                },
                ship,
            },
        )
        .expect("bind");
    let h = handoff(&f, &c);
    (tmp, f, c, h)
}
pub(super) fn accept(
    f: &Coordinated,
    c: &ExecutionClaim,
    h: &TaskHandoff,
) -> Result<ClaimMutationResult, OrbitError> {
    f.boundary().mutate_execution_claim(
        Some(&worker(c).with_handoff_observation(observation(h))),
        "handoff",
        &ClaimMutation::AcceptHandoff(h.clone()),
    )
}
pub(super) fn approve(
    f: &Coordinated,
    c: &ExecutionClaim,
    h: &TaskHandoff,
    id: &str,
) -> Result<ClaimMutationResult, OrbitError> {
    let accepted = f.boundary().accepted_handoff(&c.claim_id)?;
    f.boundary().mutate_execution_claim(
        Some(&operator(c).with_handoff_observation(observation(h))),
        id,
        &ClaimMutation::ApproveHandoff {
            handoff_id: accepted.handoff_id,
            candidate: h.candidate.clone(),
        },
    )
}
pub(super) fn starts(f: &Coordinated) -> Vec<LandingStartRequest> {
    f.backends
        .task
        .task
        .landing_start_requests()
        .expect("outbox")
}

#[test]
fn lost_handoff_reply_replays_one_transition_and_freezes_review_lock() {
    for fault in [
        CoordinationFault::AfterCommit,
        CoordinationFault::DuringApply,
        CoordinationFault::DuringEvidenceApply,
    ] {
        let (tmp, f, c, h) = fixture(request("first").ship);
        inject_coordination_faults(&[fault]);
        assert!(accept(&f, &c, &h).is_err());
        let restarted = Coordinated::open(tmp.path());
        let first = accept(&restarted, &c, &h).expect("replay committed handoff");
        assert_eq!(first, accept(&restarted, &c, &h).expect("replay again"));
        assert_eq!(restarted.task(&c.task_id).status, TaskStatus::Review);
        assert_eq!(
            restarted
                .history(&c.task_id)
                .iter()
                .filter(|e| e.event == "claim_handed_off")
                .count(),
            1
        );
        assert!(restarted.active_reservations().is_empty());
        assert!(
            starts(&restarted).is_empty(),
            "review is not completion permission"
        );
        assert!(
            !restarted
                .boundary()
                .frozen_claim_conflicts(&c.footprint)
                .expect("locks")
                .is_empty()
        );
        assert!(
            restarted
                .boundary()
                .mutate_execution_claim(
                    Some(&worker(&c)),
                    "late",
                    &ClaimMutation::Evidence(ClaimEvidence::default())
                )
                .is_err()
        );
        let mut changed = h.clone();
        changed.candidate.candidate.commit = "e".repeat(40);
        assert!(accept(&restarted, &c, &changed).is_err());
    }
}

#[test]
fn precommit_handoff_failure_keeps_execution_and_its_reservation() {
    let (_tmp, f, c, h) = fixture(request("first").ship);
    inject_coordination_faults(&[CoordinationFault::BeforeCommit]);
    assert!(accept(&f, &c, &h).is_err());
    assert_eq!(f.task(&c.task_id).status, TaskStatus::InProgress);
    assert_eq!(f.active_reservations().len(), 1);
    assert!(f.boundary().accepted_handoff(&c.claim_id).is_err());
    accept(&f, &c, &h).expect("retry");
}

#[test]
fn exact_identity_policy_and_owner_validation_are_required() {
    let (_tmp, f, c, h) = fixture(request("first").ship);
    let mutations: Vec<fn(&mut TaskHandoff)> = vec![
        |h| h.workspace_id = "other".into(),
        |h| h.task_id = "ORB-999".into(),
        |h| h.claim_id = "old".into(),
        |h| h.run_id = "old".into(),
        |h| h.machine_id = "other".into(),
        |h| h.candidate.repository = "other".into(),
        |h| h.candidate.base.commit = "e".repeat(40),
        |h| h.candidate.candidate.commit = "e".repeat(40),
        |h| h.candidate.delivery = HandoffDelivery::PullRequest { number: 43 },
        |h| h.candidate.landing_branch = "other".into(),
        |h| h.review.policy = ReviewTiming::BeforePr,
        |h| h.review.policy = ReviewTiming::AfterLanding,
        |h| h.validation.clear(),
        |h| {
            h.validation.pop();
        },
        |h| h.validation[0].path = "missing.json".into(),
        |h| h.validation[0].sha256 = "0".repeat(64),
        |h| h.execution_summary = "Outcome: failed".into(),
    ];
    for change in mutations {
        let mut changed = h.clone();
        change(&mut changed);
        assert!(accept(&f, &c, &changed).is_err(), "accepted {changed:?}");
    }
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&worker(&c)),
                "no-observation",
                &ClaimMutation::AcceptHandoff(h.clone())
            )
            .is_err()
    );
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&worker(&c)),
                "legacy",
                &ClaimMutation::Handoff(ClaimEvidence {
                    summary: Some("success".into()),
                    ..Default::default()
                })
            )
            .is_err()
    );
    let mut obs = observation(&h);
    obs.candidate.candidate.commit = "f".repeat(40);
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&worker(&c).with_handoff_observation(obs)),
                "stale-provider",
                &ClaimMutation::AcceptHandoff(h.clone())
            )
            .is_err()
    );
    let failed = logs(&f, &c, h, 1);
    assert!(accept(&f, &c, &failed).is_err());
    assert_eq!(f.task(&c.task_id).status, TaskStatus::InProgress);
    assert_eq!(f.active_reservations().len(), 1);
}

#[test]
fn operator_approval_is_atomic_deduplicated_and_pending_after_restart() {
    for fault in [
        CoordinationFault::BeforeCommit,
        CoordinationFault::AfterCommit,
    ] {
        let (tmp, f, c, h) = fixture(request("first").ship);
        accept(&f, &c, &h).expect("handoff");
        let accepted = f
            .boundary()
            .accepted_handoff(&c.claim_id)
            .expect("accepted");
        let approval = ClaimMutation::ApproveHandoff {
            handoff_id: accepted.handoff_id,
            candidate: h.candidate.clone(),
        };
        assert!(
            f.boundary()
                .mutate_execution_claim(
                    Some(&worker(&c).with_handoff_observation(observation(&h))),
                    "worker-approval",
                    &approval
                )
                .is_err()
        );
        inject_coordination_faults(&[fault]);
        assert!(approve(&f, &c, &h, "approve").is_err());
        let restarted = Coordinated::open(tmp.path());
        if fault == CoordinationFault::BeforeCommit {
            assert!(starts(&restarted).is_empty());
        }
        approve(&restarted, &c, &h, "approve").expect("retry approval");
        approve(&restarted, &c, &h, "approve").expect("exact replay");
        approve(&restarted, &c, &h, "second-request")
            .expect("same candidate no second authorization");
        assert_eq!(starts(&restarted).len(), 1);
        assert_eq!(starts(&restarted)[0].state, LandingStartState::Pending);
        assert_eq!(restarted.task(&c.task_id).status, TaskStatus::Review);
        assert_eq!(
            restarted
                .boundary()
                .coordination_rows("distributed-handoff-authorization-v1")
                .expect("authorizations")
                .len(),
            1
        );
    }
}

#[test]
fn revocation_and_changed_candidate_fence_landing_but_uncertain_intent_requires_reconciliation() {
    let (_tmp, f, c, h) = fixture(request("first").ship);
    accept(&f, &c, &h).expect("handoff");
    let auth = operator(&c).with_handoff_observation(observation(&h));
    let intent = ClaimMutation::MergeIntent {
        intent_id: "sent-merge".into(),
        resolved: false,
        evidence: "pinned provider request".into(),
    };
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&auth), "unapproved", &intent)
            .is_err()
    );
    approve(&f, &c, &h, "approval").expect("approve");
    let mut changed = observation(&h);
    changed.candidate.base.commit = "e".repeat(40);
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&operator(&c).with_handoff_observation(changed)),
                "stale",
                &intent
            )
            .is_err()
    );
    f.boundary()
        .mutate_execution_claim(Some(&auth), "intent", &intent)
        .expect("intent");
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&auth), "intent", &intent)
            .expect_err("lost send reply needs reconciliation")
            .to_string()
            .contains("reconciliation")
    );
    let revoke = ClaimMutation::RevokeHandoff {
        handoff_id: starts(&f)[0].handoff_id.clone(),
        reason: "withdraw".into(),
    };
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&auth), "revoke", &revoke)
            .expect_err("uncertain")
            .to_string()
            .contains("unresolved")
    );
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&auth),
                "recover",
                &ClaimMutation::Recover {
                    status: TaskStatus::Backlog,
                    reason: "retry".into()
                }
            )
            .is_err()
    );
    f.boundary()
        .mutate_execution_claim(
            Some(&auth),
            "reconcile",
            &ClaimMutation::MergeIntent {
                intent_id: "sent-merge".into(),
                resolved: true,
                evidence: "provider confirms request did not merge".into(),
            },
        )
        .expect("reconciled");
    f.boundary()
        .mutate_execution_claim(Some(&auth), "revoke", &revoke)
        .expect("revoke");
    assert_eq!(starts(&f)[0].state, LandingStartState::Revoked);
    assert!(approve(&f, &c, &h, "fresh-approval").is_err());
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&auth), "new-merge", &intent)
            .is_err()
    );
    // Historical replay reports the old outcome but does not reinstate pending authority.
    approve(&f, &c, &h, "approval").expect("historical approval receipt");
    assert_eq!(starts(&f)[0].state, LandingStartState::Revoked);
}

#[test]
fn recovery_cancels_outbox_and_revokes_old_attempt_atomically() {
    let (tmp, f, c, h) = fixture(request("first").ship);
    accept(&f, &c, &h).expect("handoff");
    approve(&f, &c, &h, "approve").expect("approval");
    let recovery = ClaimMutation::Recover {
        status: TaskStatus::Backlog,
        reason: "new attempt".into(),
    };
    inject_coordination_faults(&[CoordinationFault::AfterCommit]);
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&operator(&c)), "recover", &recovery)
            .is_err()
    );
    let restarted = Coordinated::open(tmp.path());
    assert_eq!(starts(&restarted)[0].state, LandingStartState::Revoked);
    assert_eq!(restarted.task(&c.task_id).status, TaskStatus::Backlog);
    assert!(
        restarted
            .boundary()
            .frozen_claim_conflicts(&c.footprint)
            .expect("locks")
            .is_empty()
    );
    let next = receipt(pull(&restarted, &request("next")))
        .claim
        .expect("new claim");
    assert_ne!(next.claim_id, c.claim_id);
    assert!(approve(&restarted, &c, &h, "late").is_err());
    assert_eq!(
        restarted.active_reservations()[0].reservation_id,
        next.reservation_id
    );
}

#[test]
fn local_and_already_landed_require_the_same_evidence_and_approval() {
    for already_landed in [false, true] {
        let mut ship = request("first").ship;
        ship.mode = "local".into();
        let (_tmp, f, c, mut h) = fixture(ship);
        h.candidate.delivery = HandoffDelivery::LocalCandidate;
        if already_landed {
            h.candidate.base = h.candidate.candidate.clone();
            let task = f.task(&c.task_id);
            let comments = f
                .backends
                .task
                .history
                .get_task_comments(&c.task_id)
                .expect("comments")
                .expect("task");
            let proof = AlreadyLandedEvidence {
                schema_version: 1,
                task_id: c.task_id.clone(),
                covering_task_id: c.task_id.clone(),
                run_id: "leaf".into(),
                tested_head: h.candidate.candidate.commit.clone(),
                covering_commit: "f".repeat(40),
                scope: already_landed_scope(&task, &comments),
                required_commands: observation(&h).required_commands,
                criteria_evidence: vec![
                    "Exact same-task delivery verified by owner Git observer".into(),
                ],
                validation: ["build", "test"]
                    .iter()
                    .map(|command| AlreadyLandedCheck {
                        validation: orbit_types::workflow::ReviewValidation {
                            command: (*command).into(),
                            outcome: orbit_types::workflow::ValidationOutcome::Passed,
                            role: orbit_types::workflow::ValidationRole::Required,
                            note: None,
                            check: None,
                        },
                        log_artifact: format!("{command}.json"),
                    })
                    .collect(),
            };
            let content = serde_json::to_vec(&proof).expect("proof");
            let reference = HandoffArtifactRef {
                path: "already-landed.json".into(),
                sha256: format!("{:x}", Sha256::digest(&content)),
            };
            f.boundary()
                .mutate_execution_claim(
                    Some(&worker(&c)),
                    "no-diff-proof",
                    &ClaimMutation::Evidence(ClaimEvidence {
                        artifacts: vec![orbit_types::task::TaskArtifact {
                            path: reference.path.clone(),
                            content,
                            media_type: "application/json".into(),
                            created_by: None,
                        }],
                        ..Default::default()
                    }),
                )
                .expect("proof stored");
            h.candidate.delivery = HandoffDelivery::AlreadyLanded {
                covering_commit: "f".repeat(40),
                evidence: reference,
            };
        }
        assert!(
            accept(&f, &c, &h).is_err(),
            "PR validation cannot validate a different delivery kind"
        );
        h = logs(&f, &c, h, 0);
        accept(&f, &c, &h).expect("local handoff");
        assert!(starts(&f).is_empty());
        approve(&f, &c, &h, "approve").expect("explicit local approval");
        assert_eq!(starts(&f).len(), 1);
    }
}

/// Managed completion was bound to an operation-mode grant. With grants
/// removed, a `done` ship contract has nothing to authorize it and the
/// handoff is refused rather than silently downgraded to review.
#[test]
fn done_completion_contract_is_refused_without_operation_grants() {
    let mut ship = request("first").ship;
    ship.completion = "done".into();
    ship.authorization_reference = Some("grant".into());
    let (_tmp, f, c, h) = fixture(ship);
    let error = accept(&f, &c, &h).expect_err("no authority can bind a done contract");
    assert!(
        error.to_string().contains("managed completion unsupported"),
        "{error}"
    );
    assert!(starts(&f).is_empty());
    assert_eq!(f.task(&c.task_id).status, TaskStatus::InProgress);
}

#[test]
fn evidence_replacement_prevents_approval_and_authorized_landing() {
    for approved in [false, true] {
        let (_tmp, f, c, h) = fixture(request("first").ship);
        accept(&f, &c, &h).expect("handoff");
        if approved {
            approve(&f, &c, &h, "approve").expect("approve");
        }
        // Model damaged owner storage, not an authorized post-handoff worker write.
        let path = f
            .boundary()
            .bundle_store
            .bundle_path(&c.task_id)
            .expect("bundle")
            .join("artifacts/files/build.json");
        std::fs::write(path, b"{}").expect("damage artifact");
        if approved {
            assert!(
                f.boundary()
                    .mutate_execution_claim(
                        Some(&operator(&c).with_handoff_observation(observation(&h))),
                        "merge",
                        &ClaimMutation::MergeIntent {
                            intent_id: "must-not-send".into(),
                            resolved: false,
                            evidence: "exact identity".into(),
                        }
                    )
                    .is_err()
            );
        } else {
            assert!(approve(&f, &c, &h, "approve").is_err());
        }
    }
}

#[test]
fn concurrent_exact_approvals_create_one_authorization_and_start() {
    let (tmp, f, c, h) = fixture(request("first").ship);
    accept(&f, &c, &h).expect("handoff");
    let other = Coordinated::open(tmp.path());
    std::thread::scope(|scope| {
        let a = scope.spawn(|| approve(&f, &c, &h, "same-request"));
        let b = scope.spawn(|| approve(&other, &c, &h, "same-request"));
        assert_eq!(
            a.join().expect("thread").expect("approval"),
            b.join().expect("thread").expect("replay")
        );
    });
    assert_eq!(starts(&f).len(), 1);
    assert_eq!(
        f.history(&c.task_id)
            .iter()
            .filter(|e| e.event == "handoff_approved")
            .count(),
        1
    );
}

#[test]
fn not_required_review_cannot_smuggle_a_reviewed_sha_or_verdict() {
    for extra in ["reviewed_sha", "verdict", "artifact"] {
        let mut value = serde_json::json!({"policy":"none", "disposition":"not_required"});
        value[extra] = serde_json::json!("fabricated");
        assert!(serde_json::from_value::<HandoffReview>(value).is_err());
    }
}

#[test]
fn revocation_rollback_or_restart_never_exposes_half_cancelled_authority() {
    for fault in [
        CoordinationFault::BeforeCommit,
        CoordinationFault::AfterCommit,
    ] {
        let (tmp, f, c, h) = fixture(request("first").ship);
        accept(&f, &c, &h).expect("handoff");
        approve(&f, &c, &h, "approve").expect("approval");
        let mutation = ClaimMutation::RevokeHandoff {
            handoff_id: starts(&f)[0].handoff_id.clone(),
            reason: "withdraw".into(),
        };
        inject_coordination_faults(&[fault]);
        assert!(
            f.boundary()
                .mutate_execution_claim(Some(&operator(&c)), "revoke", &mutation)
                .is_err()
        );
        let restarted = Coordinated::open(tmp.path());
        let cancelled = fault == CoordinationFault::AfterCommit;
        assert_eq!(
            starts(&restarted)[0].state == LandingStartState::Revoked,
            cancelled
        );
        assert_eq!(
            restarted
                .boundary()
                .resolve_execution_claims()
                .expect("claims")[0]
                .landing_invalidated,
            cancelled
        );
        restarted
            .boundary()
            .mutate_execution_claim(Some(&operator(&c)), "revoke", &mutation)
            .expect("retry");
        assert_eq!(starts(&restarted)[0].state, LandingStartState::Revoked);
    }
}
