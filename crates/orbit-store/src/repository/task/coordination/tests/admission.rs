use super::*;
use crate::contracts::*;

fn identity() -> AdmissionIdentity {
    AdmissionIdentity::authenticated_key_bound(ExecutionLocation {
        machine_id: "machine-a".into(),
        host_id: Some("display".into()),
    })
}
fn request(id: &str) -> AdmissionRequest {
    AdmissionRequest {
        request_id: id.into(),
        caller_version: "test".into(),
        caller_schema: 1,
        caller_review_policy: "none".into(),
        run_context: AdmissionRunContext {
            run_id: "drain".into(),
            job_name: "auto".into(),
            host_id: Some("untrusted-label".into()),
        },
        ship: AdmissionShipContract {
            mode: "pr".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            review_policy: "none".into(),
            completion: "review".into(),
            authorization_reference: None,
        },
    }
}
fn pull(fixture: &Coordinated, request: &AdmissionRequest) -> AdmissionLookup {
    fixture
        .boundary()
        .admit_task(
            &identity(),
            request,
            "test",
            fixture.orbit_dir.parent().expect("repo"),
            &fixture.orbit_dir,
        )
        .expect("pull")
}
fn receipt(result: AdmissionLookup) -> AdmissionReceipt {
    match result {
        AdmissionLookup::Found { receipt, .. } => *receipt,
        other => panic!("expected receipt: {other:?}"),
    }
}

#[test]
fn lost_reply_replays_exact_claim_and_immutable_input() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let first = fixture.create_task("first");
    let original = receipt(pull(&fixture, &request("one")));
    assert_eq!(original.claim.as_ref().expect("claim").task_id, first.id);
    assert_eq!(receipt(pull(&fixture, &request("one"))), original);
    assert_eq!(
        fixture
            .history(&first.id)
            .iter()
            .filter(|h| h.event == "pulled_by")
            .count(),
        1
    );
    let reservations = fixture.active_reservations();
    assert_eq!(reservations.len(), 1);
    let created =
        chrono::DateTime::parse_from_rfc3339(&reservations[0].created_at).expect("created");
    let expires =
        chrono::DateTime::parse_from_rfc3339(&reservations[0].expires_at).expect("expires");
    assert_eq!((expires - created).num_seconds(), 14_400);
    assert_eq!(
        original
            .claim
            .as_ref()
            .expect("claim")
            .executed_on
            .host_id
            .as_deref(),
        Some("display")
    );
    let mut changed = request("one");
    changed.ship.base_branch = "different".into();
    assert!(
        fixture
            .boundary()
            .admit_task(
                &identity(),
                &changed,
                "test",
                temp.path(),
                &fixture.orbit_dir
            )
            .expect_err("mismatch")
            .to_string()
            .contains("request_mismatch")
    );
    assert!(
        !fixture
            .boundary()
            .compact_admission(&identity(), "one")
            .expect("unsettled retained")
    );
}

#[test]
fn idle_is_durable_and_compaction_cannot_readmit() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let idle = receipt(pull(&fixture, &request("idle")));
    assert!(idle.claim.is_none());
    fixture.create_task("arrived");
    assert_eq!(receipt(pull(&fixture, &request("idle"))), idle);
    assert!(
        fixture
            .boundary()
            .compact_admission(&identity(), "idle")
            .expect("compact")
    );
    assert_eq!(pull(&fixture, &request("idle")), AdmissionLookup::Expired);
    let usage = fixture.boundary().admission_storage_usage().expect("usage");
    assert_eq!(usage.tombstones, 1);
    assert_eq!(usage.receipts, 0);
    assert!(usage.tombstone_bytes > 0);
    assert!(receipt(pull(&fixture, &request("new"))).claim.is_some());
}

#[test]
fn failure_before_decision_rolls_back_and_lost_apply_recovers() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let task = fixture.create_task("work");
    inject_coordination_faults(&[CoordinationFault::BeforeCommit]);
    assert!(
        fixture
            .boundary()
            .admit_task(
                &identity(),
                &request("one"),
                "test",
                temp.path(),
                &fixture.orbit_dir
            )
            .is_err()
    );
    assert_eq!(fixture.task(&task.id).status, TaskStatus::Backlog);
    assert!(fixture.active_reservations().is_empty());
    assert_eq!(
        fixture
            .boundary()
            .lookup_admission(&identity(), "one")
            .expect("lookup"),
        AdmissionLookup::NotFound
    );
    inject_coordination_faults(&[CoordinationFault::AfterCommit]);
    assert!(
        fixture
            .boundary()
            .admit_task(
                &identity(),
                &request("one"),
                "test",
                temp.path(),
                &fixture.orbit_dir
            )
            .is_err()
    );
    let original = receipt(pull(&fixture, &request("one")));
    assert_eq!(original.claim.expect("claim").task_id, task.id);
    assert_eq!(fixture.active_reservations().len(), 1);
}

#[test]
fn simultaneous_retries_create_one_claim() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    fixture.create_task("work");
    let first = Coordinated::open(temp.path());
    let second = Coordinated::open(temp.path());
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            pull(&first, &request("same"))
        });
        let b = scope.spawn(|| {
            barrier.wait();
            pull(&second, &request("same"))
        });
        assert_eq!(a.join().expect("first"), b.join().expect("second"));
    });
    assert_eq!(fixture.active_reservations().len(), 1);
    assert_eq!(
        fixture.boundary().execution_claims().expect("claims").len(),
        1
    );
}

#[test]
fn expired_reservation_does_not_release_missing_file_or_allow_ordinary_bypass() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let mut params = create_params("future");
    params.context_files = vec!["file:future.rs".into()];
    let task = fixture
        .backends
        .task
        .task
        .create_task(params.clone())
        .expect("task");
    let original = receipt(pull(&fixture, &request("one")));
    let claim = original.claim.expect("claim");
    fixture
        .boundary()
        .store_handle()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE task_reservations SET expires_at='2000-01-01T00:00:00+00:00'",
                    [],
                )
                .expect("expire");
            Ok(())
        })
        .expect("expiry transaction");
    params.title = "overlap".into();
    let other = fixture
        .backends
        .task
        .task
        .create_task(params)
        .expect("other");
    assert!(receipt(pull(&fixture, &request("two"))).claim.is_none());
    assert!(
        fixture
            .backends
            .task
            .document
            .update_task_document(
                &task.id,
                TaskDocumentUpdateParams {
                    actor: "test".into(),
                    context_files: Some(vec!["file:elsewhere.rs".into()]),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert!(fixture.backends.task.task.delete_task(&task.id).is_err());
    assert!(
        fixture
            .backends
            .task
            .history
            .update_task_history(
                &other.id,
                TaskHistoryUpdateParams {
                    actor: "test".into(),
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert!(
        !fixture
            .backends
            .reservation
            .reserve_task_reservation(fixture.reservation_params(&other.id, "file:future.rs"))
            .expect("reservation refused")
            .reserved
    );
    assert_eq!(
        fixture.boundary().execution_claims().expect("claims")[0].claim_id,
        claim.claim_id
    );
}

#[test]
fn invalid_candidates_do_not_starve_valid_work() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let mut empty = create_params("empty");
    empty.context_files.clear();
    fixture
        .backends
        .task
        .task
        .create_task(empty)
        .expect("empty");
    let mut invalid = create_params("invalid");
    invalid.context_files = vec!["file:../../escape".into()];
    fixture
        .backends
        .task
        .task
        .create_task(invalid)
        .expect("invalid");
    let mut dependency = create_params("not done");
    dependency.status = TaskStatus::Rejected;
    let dependency = fixture
        .backends
        .task
        .task
        .create_task(dependency)
        .expect("dependency");
    let mut waiting = create_params("waiting");
    waiting.dependencies = vec![dependency.id];
    fixture
        .backends
        .task
        .task
        .create_task(waiting)
        .expect("waiting");
    let valid = fixture.create_task("valid");
    let result = receipt(pull(&fixture, &request("one")));
    assert_eq!(result.claim.expect("claim").task_id, valid.id);
    assert_eq!(result.invalid_candidates.len(), 3);
}

#[test]
fn legacy_composition_cannot_access_coordinated_partition() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let registry = TaskRegistryStore::open(&task_registry_path(temp.path())).expect("registry");
    let legacy = crate::compose::workspace_task_backends(registry, PARTITION_ID.into());
    assert!(legacy.task.create_task(create_params("bypass")).is_err());
    assert!(legacy.task.list_tasks().is_err());
    assert!(
        fixture
            .backends
            .task
            .task
            .list_tasks()
            .expect("coordinated")
            .is_empty()
    );
}

#[test]
fn different_journal_cannot_serve_an_activated_partition() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    fixture.create_task("work");
    let registry = TaskRegistryStore::open(&task_registry_path(temp.path())).expect("registry");
    let other_store = Store::open(&temp.path().join("different.sqlite")).expect("other store");
    let error = match workspace_coordinated_backends(registry, PARTITION_ID.into(), other_store) {
        Ok(_) => panic!("different journal must be refused"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("different coordination journal"));
}

#[test]
fn distinct_concurrent_requests_cannot_claim_overlapping_tasks() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    fixture.create_task("one");
    fixture.create_task("two");
    let other = Coordinated::open(temp.path());
    let barrier = std::sync::Barrier::new(2);
    let receipts = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            receipt(pull(&fixture, &request("a")))
        });
        let b = scope.spawn(|| {
            barrier.wait();
            receipt(pull(&other, &request("b")))
        });
        [a.join().expect("a"), b.join().expect("b")]
    });
    assert_eq!(receipts.iter().filter(|r| r.claim.is_some()).count(), 1);
    assert_eq!(
        fixture
            .boundary()
            .admission_storage_usage()
            .expect("usage")
            .receipts,
        2
    );
    assert_eq!(fixture.active_reservations().len(), 1);
}

#[test]
fn receipt_identity_is_machine_scoped_and_current_phase_is_not_history() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    fixture.create_task("work");
    let original = receipt(pull(&fixture, &request("one")));
    let other = AdmissionIdentity::authenticated_key_bound(ExecutionLocation {
        machine_id: "other-machine".into(),
        host_id: None,
    });
    assert_eq!(
        fixture
            .boundary()
            .lookup_admission(&other, "one")
            .expect("lookup"),
        AdmissionLookup::NotFound
    );
    let boundary = fixture.boundary();
    boundary
        .with_admission(|| {
            let row = boundary
                .coordination_rows("distributed-execution-claim-v1")?
                .remove(0);
            let mut claim: ExecutionClaim = serde_json::from_str(&row.payload_json).expect("claim");
            claim.phase = ExecutionClaimPhase::Revoked;
            boundary.store.replace_task_coordination_payload(
                PARTITION_ID,
                &row,
                &serde_json::to_string(&claim).expect("json"),
            )?;
            Ok(())
        })
        .expect("test lifecycle transition");
    match pull(&fixture, &request("one")) {
        AdmissionLookup::Found {
            receipt,
            current_claim,
        } => {
            assert_eq!(*receipt, original);
            assert_eq!(
                current_claim.expect("current").phase,
                ExecutionClaimPhase::Revoked
            );
        }
        other => panic!("expected history: {other:?}"),
    }
    assert!(
        boundary
            .compact_admission(&identity(), "one")
            .expect("settled compact")
    );
    assert_eq!(pull(&fixture, &request("one")), AdmissionLookup::Expired);
    assert_eq!(fixture.active_reservations().len(), 1);
}

#[test]
fn foreign_dependency_is_authoritative_and_recovers_its_own_pending_commit() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let registry = TaskRegistryStore::open(&task_registry_path(temp.path())).expect("registry");
    let foreign_root = temp.path().join("foreign");
    std::fs::create_dir_all(foreign_root.join(".orbit")).expect("directory");
    let binding = registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("ws_foreign".into()),
            slug: "foreign".into(),
            repo_root: foreign_root.clone(),
            workspace_path: foreign_root.clone(),
            orbit_dir: foreign_root.join(".orbit"),
            repo_fingerprint: None,
        })
        .expect("binding");
    let foreign = workspace_coordinated_backends(
        registry,
        binding.partition_id,
        fixture.boundary().store_handle().clone(),
    )
    .expect("foreign");
    let dependency = foreign
        .task
        .task
        .create_task(create_params("dependency"))
        .expect("dependency");
    let mut waiting = create_params("waiting");
    waiting.dependencies = vec![dependency.id.clone()];
    let waiting = fixture
        .backends
        .task
        .task
        .create_task(waiting)
        .expect("waiting");
    assert!(receipt(pull(&fixture, &request("before"))).claim.is_none());
    inject_coordination_faults(&[CoordinationFault::AfterCommit]);
    assert!(
        foreign
            .commit_boundary
            .commit_task_transition(&TaskCoordinationCommitParams {
                task_id: dependency.id.clone(),
                actor: "test".into(),
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .is_err()
    );
    let admitted = receipt(pull(&fixture, &request("after")));
    assert_eq!(admitted.claim.expect("claim").task_id, waiting.id);
    assert_eq!(
        foreign
            .task
            .task
            .get_task(&dependency.id)
            .expect("read")
            .expect("task")
            .status,
        TaskStatus::Done
    );
}

#[test]
fn row_finalization_failure_rolls_back_reservation_and_all_admission_artifacts() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let task = fixture.create_task("work");
    let history = fixture.history(&task.id);
    let params = fixture.admission_params(&task.id, "file:future.rs");
    let result = fixture.boundary().with_admission(|| {
        fixture
            .boundary()
            .commit_locked_with_rows(&params, &mut |reserved| {
                assert!(reserved.is_some());
                Err(OrbitError::Store(
                    "injected row serialization failure".into(),
                ))
            })
    });
    assert!(result.is_err());
    assert_eq!(fixture.task(&task.id).status, TaskStatus::Backlog);
    assert_eq!(fixture.history(&task.id), history);
    assert!(fixture.active_reservations().is_empty());
    assert!(
        fixture
            .boundary()
            .coordination_rows("admission-receipt")
            .expect("rows")
            .is_empty()
    );
}

#[test]
fn valid_part_of_legacy_active_footprint_still_protects_missing_files() {
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let mut active = create_params("active");
    active.status = TaskStatus::Review;
    active.context_files = vec!["file:future.rs".into(), "file:../../bad".into()];
    fixture
        .backends
        .task
        .task
        .create_task(active)
        .expect("legacy active");
    let mut candidate = create_params("candidate");
    candidate.context_files = vec!["file:future.rs".into()];
    let candidate = fixture
        .backends
        .task
        .task
        .create_task(candidate)
        .expect("candidate");
    let result = receipt(pull(&fixture, &request("one")));
    assert!(result.claim.is_none());
    assert_eq!(result.deferred_conflicts[0].task_id, candidate.id);
}
