use super::admission::{pull, receipt, request};
use super::*;
use crate::contracts::*;

fn claim(f: &Coordinated) -> ExecutionClaim {
    f.create_task("lifecycle");
    receipt(pull(f, &request("first"))).claim.expect("claim")
}
fn worker(c: &ExecutionClaim, bound: bool) -> ClaimInvocation {
    ClaimInvocation::trusted_worker(
        c.task_id.clone(),
        c.claim_id.clone(),
        c.executed_on.machine_id.clone(),
        bound.then(|| run(c)),
    )
}
fn operator(c: &ExecutionClaim) -> ClaimInvocation {
    ClaimInvocation::trusted_operator(c.task_id.clone(), c.claim_id.clone(), "operator".into())
}
fn run(c: &ExecutionClaim) -> ClaimRun {
    ClaimRun {
        machine_id: c.executed_on.machine_id.clone(),
        run_id: "leaf".into(),
    }
}
fn bind(f: &Coordinated, c: &ExecutionClaim) {
    f.boundary()
        .mutate_execution_claim(
            Some(&worker(c, false)),
            "bind",
            &ClaimMutation::Bind {
                run: run(c),
                ship: request("first").ship,
            },
        )
        .expect("bind");
}
fn evidence() -> ClaimEvidence {
    ClaimEvidence {
        summary: Some("Outcome: failed\nRetained branch and PR evidence".into()),
        comment: Some("failure details".into()),
        artifacts: vec![orbit_types::task::TaskArtifact {
            path: "failure.json".into(),
            content: b"{}".to_vec(),
            media_type: "application/json".into(),
            created_by: None,
        }],
    }
}

#[test]
fn authority_is_checked_for_every_mutation_before_any_write() {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    let c = claim(&f);
    bind(&f, &c);
    let history = f.history(&c.task_id);
    let operations = [
        ClaimMutation::Evidence(evidence()),
        ClaimMutation::Handoff(evidence()),
        ClaimMutation::Fail(evidence()),
        ClaimMutation::Bind {
            run: run(&c),
            ship: request("first").ship,
        },
        ClaimMutation::Recover {
            status: TaskStatus::Backlog,
            reason: "retry".into(),
        },
        ClaimMutation::MergeIntent {
            intent_id: "merge".into(),
            resolved: false,
            evidence: "intent".into(),
        },
    ];
    for op in operations {
        assert!(
            f.boundary()
                .mutate_execution_claim(None, "missing", &op)
                .is_err()
        );
        let valid = worker(&c, true);
        let mut wrong_machine = valid.clone();
        wrong_machine.machine_id = "forged".into();
        let mut wrong_claim = valid.clone();
        wrong_claim.claim_id = "forged".into();
        let mut wrong_task = valid.clone();
        wrong_task.task_id = "ORB-99999".into();
        let mut wrong_run = valid.clone();
        wrong_run.run.as_mut().expect("run").run_id = "other".into();
        let mut wrong_host = valid.clone();
        wrong_host.run.as_mut().expect("run").machine_id = "other".into();
        for bad in [
            wrong_machine,
            wrong_claim,
            wrong_task,
            wrong_run,
            wrong_host,
            worker(&c, false),
        ] {
            assert!(
                f.boundary()
                    .mutate_execution_claim(Some(&bad), "bad", &op)
                    .is_err()
            );
        }
    }
    assert_eq!(f.history(&c.task_id), history);
    assert_eq!(f.active_reservations().len(), 1);
    assert_eq!(f.task(&c.task_id).execution_summary, "");
    assert!(
        f.boundary()
            .commit_task_transition(&TaskCoordinationCommitParams {
                task_id: c.task_id.clone(),
                actor: "bypass".into(),
                status: Some(TaskStatus::Review),
                ..Default::default()
            })
            .is_err()
    );
}

#[test]
fn settlement_and_lost_reply_replay_are_atomic_and_idempotent() {
    for fault in [
        CoordinationFault::BeforeCommit,
        CoordinationFault::AfterCommit,
        CoordinationFault::DuringApply,
        CoordinationFault::DuringEvidenceApply,
    ] {
        let tmp = TempDir::new().expect("temp");
        let f = Coordinated::open(tmp.path());
        let c = claim(&f);
        bind(&f, &c);
        let op = ClaimMutation::Fail(evidence());
        inject_coordination_faults(&[fault]);
        assert!(
            f.boundary()
                .mutate_execution_claim(Some(&worker(&c, true)), "settle", &op)
                .is_err()
        );
        let reopened = Coordinated::open(tmp.path());
        if fault == CoordinationFault::BeforeCommit {
            assert_eq!(reopened.task(&c.task_id).status, TaskStatus::InProgress);
            assert_eq!(reopened.active_reservations().len(), 1);
            assert!(reopened.task(&c.task_id).execution_summary.is_empty());
        }
        let first = reopened
            .boundary()
            .mutate_execution_claim(Some(&worker(&c, true)), "settle", &op)
            .expect("retry");
        let second = reopened
            .boundary()
            .mutate_execution_claim(Some(&worker(&c, true)), "settle", &op)
            .expect("replay");
        assert_eq!(first, second);
        assert_eq!(first.phase, ExecutionClaimPhase::Failed);
        assert_eq!(reopened.task(&c.task_id).status, TaskStatus::Blocked);
        assert_eq!(
            reopened.task(&c.task_id).execution_summary,
            evidence().summary.expect("summary")
        );
        assert!(reopened.active_reservations().is_empty());
        assert_eq!(
            reopened
                .backends
                .task
                .history
                .get_task_comments(&c.task_id)
                .expect("comments")
                .expect("task")
                .len(),
            1
        );
        assert_eq!(
            reopened
                .backends
                .task
                .artifact
                .get_task_artifacts(&c.task_id)
                .expect("artifacts")
                .expect("task")[0]
                .content,
            b"{}"
        );
        assert_eq!(
            reopened
                .history(&c.task_id)
                .iter()
                .filter(|h| h.event == "claim_failed")
                .count(),
            1
        );
        assert!(
            reopened
                .boundary()
                .mutate_execution_claim(
                    Some(&worker(&c, true)),
                    "settle",
                    &ClaimMutation::Evidence(evidence())
                )
                .is_err()
        );
        assert!(
            reopened
                .boundary()
                .mutate_execution_claim(
                    Some(&worker(&c, true)),
                    "late",
                    &ClaimMutation::Evidence(evidence())
                )
                .is_err()
        );
    }
}

#[test]
fn recovery_fences_old_attempt_and_never_releases_new_reservation() {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    let c = claim(&f);
    bind(&f, &c);
    let recovery = ClaimMutation::Recover {
        status: TaskStatus::Backlog,
        reason: "explicit retry".into(),
    };
    let first = f
        .boundary()
        .mutate_execution_claim(Some(&operator(&c)), "recover", &recovery)
        .expect("recover");
    let fresh = receipt(pull(&f, &request("second")))
        .claim
        .expect("new claim");
    assert_ne!(fresh.claim_id, c.claim_id);
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&worker(&c, false)),
                "bind",
                &ClaimMutation::Bind {
                    run: run(&c),
                    ship: request("first").ship
                }
            )
            .is_err()
    );
    for op in [
        ClaimMutation::Evidence(evidence()),
        ClaimMutation::Fail(evidence()),
        ClaimMutation::Handoff(evidence()),
        ClaimMutation::Bind {
            run: run(&c),
            ship: request("first").ship,
        },
    ] {
        assert!(
            f.boundary()
                .mutate_execution_claim(Some(&worker(&c, true)), "late", &op)
                .is_err()
        );
    }
    assert_eq!(
        f.boundary()
            .mutate_execution_claim(Some(&operator(&c)), "recover", &recovery)
            .expect("replay"),
        first
    );
    assert_eq!(
        f.active_reservations()[0].reservation_id,
        fresh.reservation_id
    );
    assert_eq!(f.task(&c.task_id).status, TaskStatus::InProgress);
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&worker(&fresh, false)),
                "reuse-leaf",
                &ClaimMutation::Bind {
                    run: run(&c),
                    ship: request("second").ship
                }
            )
            .is_err()
    );
}

#[test]
fn unresolved_merge_intent_blocks_recovery_and_inspection_writes_nothing() {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    let c = claim(&f);
    bind(&f, &c);
    f.boundary()
        .mutate_execution_claim(
            Some(&worker(&c, true)),
            "handoff",
            &ClaimMutation::Handoff(evidence()),
        )
        .expect("handoff");
    let intent = ClaimMutation::MergeIntent {
        intent_id: "external".into(),
        resolved: false,
        evidence: "pinned PR/head/base".into(),
    };
    f.boundary()
        .mutate_execution_claim(Some(&operator(&c)), "intent", &intent)
        .expect("intent");
    let history = f.history(&c.task_id);
    let recovery = ClaimMutation::Recover {
        status: TaskStatus::Backlog,
        reason: "retry".into(),
    };
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&operator(&c)), "recover", &recovery)
            .expect_err("refuse")
            .to_string()
            .contains("unresolved")
    );
    let inspected = f.boundary().inspect_execution_claims().expect("inspect");
    assert_eq!(
        inspected[0].unresolved_merge_intent.as_deref(),
        Some("external")
    );
    assert!(inspected[0].age_seconds.is_some());
    assert_eq!(f.history(&c.task_id), history);
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&worker(&c, true)),
                "late-evidence",
                &ClaimMutation::Evidence(evidence())
            )
            .is_err()
    );
    f.boundary()
        .mutate_execution_claim(
            Some(&operator(&c)),
            "reconcile",
            &ClaimMutation::MergeIntent {
                intent_id: "external".into(),
                resolved: true,
                evidence: "provider reconciliation: not merged".into(),
            },
        )
        .expect("reconcile");
    f.boundary()
        .mutate_execution_claim(Some(&operator(&c)), "recover", &recovery)
        .expect("recover");
    assert!(f.boundary().inspect_execution_claims().expect("inspect")[0].landing_invalidated);
    assert_eq!(
        f.task(&c.task_id).execution_summary,
        evidence().summary.expect("summary")
    );
}

#[test]
fn repeated_running_evidence_and_immutable_bind_preserve_attempt() {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    let c = claim(&f);
    bind(&f, &c);
    bind(&f, &c);
    let op = ClaimMutation::Evidence(evidence());
    let first = f
        .backends
        .task
        .task
        .mutate_execution_claim(Some(&worker(&c, true)), "step", &op)
        .expect("step");
    assert_eq!(
        f.backends
            .task
            .task
            .mutate_execution_claim(Some(&worker(&c, true)), "step", &op)
            .expect("retry"),
        first
    );
    assert_eq!(first.phase, ExecutionClaimPhase::Running);
    assert_eq!(f.active_reservations().len(), 1);
    assert_eq!(f.task(&c.task_id).job_run_id.as_deref(), Some("leaf"));
    assert!(
        f.backends
            .task
            .document
            .update_task_document(
                &c.task_id,
                TaskDocumentUpdateParams {
                    actor: "operator".into(),
                    context_files: Some(vec!["dir:other".into()]),
                    ..Default::default()
                }
            )
            .is_err()
    );
}

#[test]
fn failed_claim_can_be_deliberately_recovered_but_old_operator_cannot_recover_new_attempt() {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    let c = claim(&f);
    // Launch failure has no bound leaf, but still needs durable atomic settlement.
    f.boundary()
        .mutate_execution_claim(
            Some(&worker(&c, false)),
            "launch-failed",
            &ClaimMutation::Fail(evidence()),
        )
        .expect("launch failure");
    let recovery = ClaimMutation::Recover {
        status: TaskStatus::Backlog,
        reason: "repair launch".into(),
    };
    f.boundary()
        .mutate_execution_claim(Some(&operator(&c)), "recover-failed", &recovery)
        .expect("recover failed claim");
    let fresh = receipt(pull(&f, &request("new")))
        .claim
        .expect("fresh claim");
    f.boundary()
        .mutate_execution_claim(
            Some(&worker(&fresh, false)),
            "new-failure",
            &ClaimMutation::Fail(evidence()),
        )
        .expect("new failure");
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&operator(&c)), "stale-recovery", &recovery)
            .is_err()
    );
    assert_eq!(f.task(&c.task_id).status, TaskStatus::Blocked);
}

#[test]
fn inspection_refuses_pending_repair_without_recovering_it() {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    let c = claim(&f);
    inject_coordination_faults(&[CoordinationFault::AfterCommit]);
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&worker(&c, false)),
                "fail",
                &ClaimMutation::Fail(evidence())
            )
            .is_err()
    );
    let pending = std::fs::read(f.boundary().pending_marker_path()).expect("marker");
    assert!(f.boundary().inspect_execution_claims().is_err());
    assert_eq!(
        std::fs::read(f.boundary().pending_marker_path()).expect("unchanged marker"),
        pending
    );
    f.boundary().recover().expect("explicit journal recovery");
    assert_eq!(
        f.boundary().inspect_execution_claims().expect("inspect")[0]
            .claim
            .phase,
        ExecutionClaimPhase::Failed
    );
}

/// [ORB-12575] The ordinary-participant read settles the interrupted commit
/// itself and then reports the same claim states inspection would, while the
/// non-repairing inspection keeps refusing until then.
#[test]
fn resolution_recovers_pending_commit_where_inspection_refuses() {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    let c = claim(&f);
    inject_coordination_faults(&[CoordinationFault::AfterCommit]);
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&worker(&c, false)),
                "fail",
                &ClaimMutation::Fail(evidence())
            )
            .is_err()
    );
    assert!(f.boundary().pending_marker_exists());
    assert!(f.boundary().inspect_execution_claims().is_err());
    assert!(f.boundary().pending_marker_exists());

    let resolved = f
        .boundary()
        .resolve_execution_claims()
        .expect("resolution replays the journal");
    assert!(!f.boundary().pending_marker_exists());
    assert_eq!(resolved[0].claim.phase, ExecutionClaimPhase::Failed);
    let inspected = f.boundary().inspect_execution_claims().expect("inspect");
    assert_eq!(inspected.len(), 1);
    assert_eq!(inspected[0].claim, resolved[0].claim);
    assert_eq!(inspected[0].last_event, resolved[0].last_event);
}
