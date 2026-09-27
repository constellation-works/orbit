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
    let handoff = super::handoff::handoff(&f, &c);
    let context = worker(&c, true).with_handoff_observation(super::handoff::observation(&handoff));
    f.boundary()
        .mutate_execution_claim(
            Some(&context),
            "handoff",
            &ClaimMutation::AcceptHandoff(handoff.clone()),
        )
        .expect("handoff");
    let accepted = f
        .boundary()
        .accepted_handoff(&c.claim_id)
        .expect("accepted");
    let owner = operator(&c).with_handoff_observation(super::handoff::observation(&handoff));
    f.boundary()
        .mutate_execution_claim(
            Some(&owner),
            "approve",
            &ClaimMutation::ApproveHandoff {
                handoff_id: accepted.handoff_id,
                candidate: handoff.candidate,
            },
        )
        .expect("approval");
    let intent = ClaimMutation::MergeIntent {
        intent_id: "external".into(),
        resolved: false,
        evidence: "pinned PR/head/base".into(),
    };
    f.boundary()
        .mutate_execution_claim(Some(&owner), "intent", &intent)
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
        handoff.execution_summary
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

fn friction_mutation() -> ClaimMutation {
    ClaimMutation::Friction(FrictionAddParams {
        model: "codex".into(),
        title: Some("Claim-scoped evidence".into()),
        body: "The isolated fixture observed a failure.".into(),
        tags: vec![],
        during_task: None,
        created_at: Utc::now(),
    })
}

fn friction_count(f: &Coordinated) -> i64 {
    f.boundary()
        .store
        .with_read_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM friction_records", [], |row| {
                row.get(0)
            })
            .map_err(|error| orbit_common::OrbitError::Store(error.to_string()))
        })
        .expect("friction count")
}

#[test]
fn claimed_friction_is_bound_and_replayed_without_duplicate_allocation() {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    let c = claim(&f);
    bind(&f, &c);
    let op = friction_mutation();
    let accepted = f
        .boundary()
        .mutate_execution_claim(Some(&worker(&c, true)), "friction", &op)
        .expect("add");
    assert_eq!(
        accepted
            .friction
            .as_ref()
            .expect("record")
            .during_task
            .as_deref(),
        Some(c.task_id.as_str())
    );
    let retried = friction_mutation();
    assert_eq!(
        accepted,
        f.boundary()
            .mutate_execution_claim(Some(&worker(&c, true)), "friction", &retried)
            .expect("replay with fresh call clock")
    );
    assert_eq!(friction_count(&f), 1);
    let mut conflicting = op.clone();
    if let ClaimMutation::Friction(params) = &mut conflicting {
        params.during_task = Some("other".into());
    }
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&worker(&c, true)), "conflict", &conflicting)
            .is_err()
    );
    f.boundary()
        .mutate_execution_claim(
            Some(&operator(&c)),
            "recover",
            &ClaimMutation::Recover {
                status: TaskStatus::Backlog,
                reason: "deliberate recovery".into(),
            },
        )
        .expect("recover");
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&worker(&c, true)), "late", &op)
            .is_err()
    );
    assert_eq!(
        accepted,
        f.boundary()
            .mutate_execution_claim(Some(&worker(&c, true)), "friction", &op)
            .expect("historical outcome only")
    );
    assert_eq!(friction_count(&f), 1);
}

#[test]
fn claimed_friction_faults_share_the_claim_commit_decision() {
    for fault in [
        CoordinationFault::BeforeCommit,
        CoordinationFault::AfterCommit,
    ] {
        let tmp = TempDir::new().expect("temp");
        let f = Coordinated::open(tmp.path());
        let c = claim(&f);
        bind(&f, &c);
        let op = friction_mutation();
        inject_coordination_faults(&[fault]);
        assert!(
            f.boundary()
                .mutate_execution_claim(Some(&worker(&c, true)), "friction", &op)
                .is_err()
        );
        assert_eq!(
            friction_count(&f),
            if fault == CoordinationFault::AfterCommit {
                1
            } else {
                0
            }
        );
        let recovered = Coordinated::open(tmp.path());
        let result = recovered
            .boundary()
            .mutate_execution_claim(Some(&worker(&c, true)), "friction", &op)
            .expect("retry interrupted commit");
        assert!(result.friction.is_some());
        assert_eq!(friction_count(&recovered), 1);
    }
}

#[test]
fn claimed_friction_revocation_between_failed_prepare_and_retry_cannot_publish() {
    let tmp = TempDir::new().expect("temp");
    let f = Coordinated::open(tmp.path());
    let c = claim(&f);
    bind(&f, &c);
    let mutation = friction_mutation();
    inject_coordination_faults(&[CoordinationFault::BeforeCommit]);
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&worker(&c, true)), "interrupted-friction", &mutation)
            .is_err()
    );
    assert_eq!(friction_count(&f), 0);
    f.boundary()
        .mutate_execution_claim(
            Some(&operator(&c)),
            "revoke",
            &ClaimMutation::Recover {
                status: TaskStatus::Backlog,
                reason: "deliberate recovery after fault".into(),
            },
        )
        .expect("revoke");
    assert!(
        f.boundary()
            .mutate_execution_claim(Some(&worker(&c, true)), "interrupted-friction", &mutation)
            .is_err()
    );
    assert_eq!(friction_count(&f), 0);
}

fn artifact_evidence(paths: &[&str], content: &[u8]) -> ClaimEvidence {
    ClaimEvidence {
        artifacts: paths
            .iter()
            .map(|path| orbit_types::task::TaskArtifact {
                path: (*path).into(),
                content: content.to_vec(),
                media_type: "text/plain".into(),
                created_by: None,
            })
            .collect(),
        ..Default::default()
    }
}

fn publish_artifacts(f: &Coordinated, c: &ExecutionClaim, id: &str, paths: &[&str], bytes: &[u8]) {
    f.boundary()
        .mutate_execution_claim(
            Some(&worker(c, true)),
            id,
            &ClaimMutation::Evidence(artifact_evidence(paths, bytes)),
        )
        .expect("publish artifacts");
}

fn journal_count(f: &Coordinated) -> i64 {
    f.boundary()
        .store
        .with_read_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM task_commit_journal", [], |row| {
                row.get(0)
            })
            .map_err(|error| orbit_common::OrbitError::Store(error.to_string()))
        })
        .expect("journal count")
}

#[test]
fn conflicting_artifact_topologies_are_refused_before_any_durable_write() {
    // Both proposed orders, manifest conflicts in both directions, and physical
    // destinations absent from the manifest must all be checked before commit.
    for (existing, manifested, proposed) in [
        (None, false, vec!["a", "a/b"]),
        (None, false, vec!["a/b", "a"]),
        (None, false, vec!["a//b", "a"]),
        (Some("a"), true, vec!["a/b"]),
        (Some("a/b"), true, vec!["a"]),
        (Some("a"), false, vec!["a/b"]),
        (Some("a/b"), false, vec!["a"]),
    ] {
        let tmp = TempDir::new().expect("temp");
        let f = Coordinated::open(tmp.path());
        let c = claim(&f);
        bind(&f, &c);
        let other = f.create_task("other task in partition");
        let root = f
            .boundary()
            .bundle_store
            .bundle_path(&c.task_id)
            .expect("bundle");
        let artifacts = root.join(orbit_types::task::TASK_ARTIFACTS_DIR_NAME);
        let files = artifacts.join("files");
        if let Some(path) = existing {
            if manifested {
                publish_artifacts(&f, &c, "initial", &[path], b"original");
            } else {
                let destination = files.join(path);
                std::fs::create_dir_all(destination.parent().expect("parent")).expect("parents");
                std::fs::write(destination, b"original").expect("unmanifested file");
            }
        }
        let manifest_path = artifacts.join(orbit_types::task::TASK_ARTIFACT_MANIFEST_FILE_NAME);
        let files_existed = files.exists();
        let manifest_before = std::fs::read(&manifest_path).ok();
        let history_before = f.history(&c.task_id);
        let journal_before = journal_count(&f);
        let mut evidence = artifact_evidence(&proposed, b"replacement");
        // A valid earlier file must not be applied before discovering a conflict.
        evidence.artifacts.insert(
            0,
            artifact_evidence(&["unrelated"], b"new")
                .artifacts
                .remove(0),
        );
        evidence.summary = Some("must not be published".into());
        evidence.comment = Some("must not be appended".into());
        let error = f
            .boundary()
            .mutate_execution_claim(
                Some(&worker(&c, true)),
                "conflict",
                &ClaimMutation::Evidence(evidence),
            )
            .expect_err("conflict must be refused");
        assert!(
            matches!(error, orbit_common::OrbitError::InvalidInput(_)),
            "{error}"
        );
        assert_eq!(
            journal_count(&f),
            journal_before,
            "refusal must precede journal preparation"
        );
        assert!(!f.boundary().pending_marker_exists());
        assert_eq!(std::fs::read(&manifest_path).ok(), manifest_before);
        assert!(!files.join("unrelated").exists());
        if let Some(path) = existing {
            assert_eq!(
                std::fs::read(files.join(path)).expect("preserved payload"),
                b"original"
            );
        } else {
            assert_eq!(files.exists(), files_existed);
            assert!(
                !files.join("a").exists(),
                "refusal must not create payloads"
            );
        }
        assert_eq!(f.history(&c.task_id), history_before);
        assert!(f.task(&c.task_id).execution_summary.is_empty());
        assert!(
            f.backends
                .task
                .history
                .get_task_comments(&c.task_id)
                .expect("comments")
                .expect("task")
                .is_empty()
        );
        // Reopening and ordinary reads must not encounter an impossible replay.
        let reopened = Coordinated::open(tmp.path());
        assert_eq!(reopened.task(&c.task_id).status, TaskStatus::InProgress);
        assert_eq!(reopened.task(&other.id).status, TaskStatus::Backlog);
        publish_artifacts(&reopened, &c, "conflict", &["valid/nested"], b"accepted");
        committed(
            reopened
                .boundary()
                .commit_task_transition(&TaskCoordinationCommitParams {
                    task_id: other.id.clone(),
                    actor: "operator".into(),
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                })
                .expect("other task remains writable"),
        );
        assert_eq!(reopened.task(&other.id).status, TaskStatus::InProgress);
    }
}

#[test]
fn nested_artifact_creation_and_replacement_recover_after_commit() {
    for fault in [
        CoordinationFault::AfterCommit,
        CoordinationFault::DuringApply,
        CoordinationFault::DuringEvidenceApply,
    ] {
        for replace in [false, true] {
            let tmp = TempDir::new().expect("temp");
            let f = Coordinated::open(tmp.path());
            let c = claim(&f);
            bind(&f, &c);
            if replace {
                publish_artifacts(&f, &c, "initial", &["a/b"], b"original");
            }
            // Shared directories and lexical prefixes are compatible.
            let evidence = artifact_evidence(&["a/b", "a/c", "ab"], b"updated");
            let mutation = ClaimMutation::Evidence(evidence);
            inject_coordination_faults(&[fault]);
            assert!(
                f.boundary()
                    .mutate_execution_claim(Some(&worker(&c, true)), "nested", &mutation,)
                    .is_err()
            );
            assert!(f.boundary().pending_marker_exists());
            let reopened = Coordinated::open(tmp.path());
            assert_eq!(reopened.task(&c.task_id).status, TaskStatus::InProgress);
            let artifacts = reopened
                .backends
                .task
                .artifact
                .get_task_artifacts(&c.task_id)
                .expect("read recovered artifacts")
                .expect("task");
            assert_eq!(artifacts.len(), 3);
            for artifact in artifacts {
                assert_eq!(artifact.content, b"updated");
            }
            assert!(!reopened.boundary().pending_marker_exists());
            let journals = journal_count(&reopened);
            reopened
                .boundary()
                .mutate_execution_claim(Some(&worker(&c, true)), "nested", &mutation)
                .expect("idempotent receipt replay");
            assert_eq!(journal_count(&reopened), journals);
        }
    }
}
