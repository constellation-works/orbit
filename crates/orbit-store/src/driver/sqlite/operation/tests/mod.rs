//! Grant lifecycle, recovery ledgers, and the transactional admission recheck
//! [ORB-11332].

use chrono::{Duration, Utc};
use orbit_types::workflow::{
    ChildDispatchPhase, GrantLimits, GrantRights, GrantStatus, JobRunState, OperationGrant,
    PipelineState, RecoveryEpisodeKind, RecoveryReservation,
};
use serde_json::json;

use crate::Store;
use crate::contracts::{
    ChildAdmissionAuthority, ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams,
    GrantInsertOutcome, GrantTransitionKind, GrantTransitionOutcome, GrantTransitionRequest,
    JobRunStoreBackend, OperationStoreBackend, RecoveryBudget, RecoveryReserveRequest,
};
use crate::driver::sqlite::job_run_store::SqliteJobRunStore;

const WORKSPACE: &str = "ws_a";

fn grant(id: &str, tasks: &[&str], window_seconds: i64) -> OperationGrant {
    let now = Utc::now();
    let mut task_ids = tasks.iter().map(|id| id.to_string()).collect::<Vec<_>>();
    task_ids.sort();
    OperationGrant {
        id: id.to_string(),
        workspace_id: WORKSPACE.to_string(),
        actor: "operator".to_string(),
        source: "test".to_string(),
        created_at: now,
        expires_at: now + Duration::seconds(window_seconds),
        revision: 1,
        task_ids,
        rights: GrantRights {
            prepare: true,
            promote: true,
            complete: false,
        },
        limits: GrantLimits {
            leaf_ceiling: 2,
            preparation_due_seconds: 300,
            recovery_episodes_per_task: 2,
            recovery_minutes_per_task: 30,
        },
        policy: json!({ "version": 1 }),
        policy_version: 1,
        status: GrantStatus::Active,
        stopped: None,
        revoked: None,
    }
}

fn started_parent(backend: &SqliteJobRunStore) -> String {
    let run = backend
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert parent");
    backend
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start parent");
    let state = PipelineState::new(
        run.run_id.clone(),
        "workspace_auto_pipeline".to_string(),
        json!({ "max_active_leaf_runs": 5 }),
    );
    backend
        .write_run_state(&run.run_id, &state)
        .expect("write parent state");
    run.run_id
}

fn admission(parent: &str, task_id: &str, grant: &OperationGrant) -> ChildJobRunAdmissionParams {
    ChildJobRunAdmissionParams {
        parent_run_id: parent.to_string(),
        parent_step_id: Some("ship_leaves".to_string()),
        job_id: "task_auto_pipeline".to_string(),
        action: "invoke_detached".to_string(),
        blocking: false,
        attempt: 1,
        scheduled_at: Utc::now(),
        input: Some(json!({ "task_ids": [task_id] })),
        authority: Some(ChildAdmissionAuthority {
            grant_id: grant.id.clone(),
            grant_revision: grant.revision,
            task_id: Some(task_id.to_string()),
            leaf_ceiling: Some(grant.limits.leaf_ceiling),
            now: Utc::now(),
        }),
    }
}

fn refusal(outcome: ChildJobRunAdmissionOutcome) -> String {
    match outcome {
        ChildJobRunAdmissionOutcome::Refused { reason } => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn transition<'a>(kind: GrantTransitionKind, expected: Option<u32>) -> GrantTransitionRequest<'a> {
    GrantTransitionRequest {
        kind,
        actor: "operator",
        reason: Some("test"),
        expected_revision: expected,
        now: Utc::now(),
    }
}

#[test]
fn one_active_grant_per_workspace_and_stop_revoke_compare_and_set() {
    let store = Store::open_in_memory().expect("store");
    let first = grant("ogrant-1", &["ORB-1"], 3_600);
    assert_eq!(
        store.operation_grant_insert(&first).expect("insert"),
        GrantInsertOutcome::Inserted
    );
    assert_eq!(
        store
            .operation_grant_insert(&grant("ogrant-2", &["ORB-2"], 3_600))
            .expect("second insert"),
        GrantInsertOutcome::ActiveGrantExists("ogrant-1".to_string())
    );
    assert_eq!(
        store
            .operation_active_grant(WORKSPACE, Utc::now())
            .expect("active")
            .map(|grant| grant.id),
        Some("ogrant-1".to_string())
    );

    // A stale revision loses; the stored grant is returned for a reread.
    let conflict = store
        .operation_grant_transition(
            WORKSPACE,
            "ogrant-1",
            &transition(GrantTransitionKind::Stop, Some(7)),
        )
        .expect("transition");
    assert!(matches!(conflict, GrantTransitionOutcome::RevisionConflict(ref g) if g.revision == 1));

    let stopped = match store
        .operation_grant_transition(
            WORKSPACE,
            "ogrant-1",
            &transition(GrantTransitionKind::Stop, Some(1)),
        )
        .expect("stop")
    {
        GrantTransitionOutcome::Applied(grant) => grant,
        other => panic!("{other:?}"),
    };
    assert_eq!(stopped.status, GrantStatus::Stopped);
    assert_eq!(stopped.revision, 2);
    assert!(stopped.stopped.is_some());

    // Replayed stop is success without another transition.
    assert!(matches!(
        store
            .operation_grant_transition(
                WORKSPACE,
                "ogrant-1",
                &transition(GrantTransitionKind::Stop, None)
            )
            .expect("replay"),
        GrantTransitionOutcome::Unchanged(ref g) if g.revision == 2
    ));

    // A stopped workspace may enable a replacement grant.
    assert_eq!(
        store
            .operation_grant_insert(&grant("ogrant-2", &["ORB-2"], 3_600))
            .expect("replacement"),
        GrantInsertOutcome::Inserted
    );

    // Revocation strengthens a stop and keeps the stop evidence.
    let revoked = match store
        .operation_grant_transition(
            WORKSPACE,
            "ogrant-1",
            &transition(GrantTransitionKind::Revoke, None),
        )
        .expect("revoke")
    {
        GrantTransitionOutcome::Applied(grant) => grant,
        other => panic!("{other:?}"),
    };
    assert_eq!(revoked.status, GrantStatus::Revoked);
    assert_eq!(revoked.revision, 3);
    assert!(revoked.stopped.is_some() && revoked.revoked.is_some());
    assert!(!revoked.privileged_actions_allowed());

    assert_eq!(
        store
            .operation_grant_transition(
                WORKSPACE,
                "missing",
                &transition(GrantTransitionKind::Stop, None)
            )
            .expect("missing"),
        GrantTransitionOutcome::NotFound
    );
    assert_eq!(
        store.operation_grants(WORKSPACE, 10).expect("list").len(),
        2
    );
}

#[test]
fn admission_under_a_valid_grant_links_the_child_and_enforces_scope_claims_and_capacity() {
    let store = Store::open_in_memory().expect("store");
    let backend = SqliteJobRunStore::new(store.clone(), WORKSPACE);
    let grant = self::grant("ogrant-1", &["ORB-1", "ORB-2", "ORB-3"], 3_600);
    store.operation_grant_insert(&grant).expect("insert");
    let parent = started_parent(&backend);

    let child = match backend
        .admit_child_job_run(&admission(&parent, "ORB-1", &grant))
        .expect("admit")
    {
        ChildJobRunAdmissionOutcome::Admitted(child) => child,
        other => panic!("{other:?}"),
    };
    let state = backend
        .read_run_state(&parent)
        .expect("parent state")
        .expect("state");
    assert_eq!(state.child_dispatches[0].child_run_id, child.run_id);
    assert_eq!(
        state.child_dispatches[0].phase,
        ChildDispatchPhase::Submitted
    );

    // The same task cannot be claimed twice while its child is live.
    assert_eq!(
        refusal(
            backend
                .admit_child_job_run(&admission(&parent, "ORB-1", &grant))
                .expect("second claim")
        ),
        "task_claimed"
    );
    // Outside the finite scope: refused even though the grant is valid.
    assert_eq!(
        refusal(
            backend
                .admit_child_job_run(&admission(&parent, "ORB-9", &grant))
                .expect("outside scope")
        ),
        "outside_grant_scope"
    );
    // Second slot fills; the third is over the captured ceiling.
    assert!(matches!(
        backend
            .admit_child_job_run(&admission(&parent, "ORB-2", &grant))
            .expect("second child"),
        ChildJobRunAdmissionOutcome::Admitted(_)
    ));
    assert_eq!(
        refusal(
            backend
                .admit_child_job_run(&admission(&parent, "ORB-3", &grant))
                .expect("capacity")
        ),
        "capacity_saturated"
    );

    // A finished child releases both its claim and its slot.
    backend
        .mark_job_run_running(&child.run_id, Utc::now(), std::process::id())
        .expect("start child");
    backend
        .finalize_job_run(&child.run_id, JobRunState::Success, Utc::now(), Some(1))
        .expect("finish child");
    assert!(matches!(
        backend
            .admit_child_job_run(&admission(&parent, "ORB-3", &grant))
            .expect("after release"),
        ChildJobRunAdmissionOutcome::Admitted(_)
    ));
}

#[test]
fn stop_expiry_and_revocation_refuse_admission_after_the_fact_but_keep_admitted_children() {
    let store = Store::open_in_memory().expect("store");
    let backend = SqliteJobRunStore::new(store.clone(), WORKSPACE);
    let grant = self::grant("ogrant-1", &["ORB-1", "ORB-2", "ORB-3"], 3_600);
    store.operation_grant_insert(&grant).expect("insert");
    let parent = started_parent(&backend);

    let child = match backend
        .admit_child_job_run(&admission(&parent, "ORB-1", &grant))
        .expect("admit")
    {
        ChildJobRunAdmissionOutcome::Admitted(child) => child,
        other => panic!("{other:?}"),
    };

    // Stop lands after the coordinator observed eligibility for ORB-2.
    let stopped = match store
        .operation_grant_transition(
            WORKSPACE,
            &grant.id,
            &transition(GrantTransitionKind::Stop, Some(grant.revision)),
        )
        .expect("stop")
    {
        GrantTransitionOutcome::Applied(grant) => grant,
        other => panic!("{other:?}"),
    };
    // The stale coordinator still carries revision 1: it learns the stop
    // itself, not merely that the revision moved, and no child is created.
    assert_eq!(
        refusal(
            backend
                .admit_child_job_run(&admission(&parent, "ORB-2", &grant))
                .expect("stale revision")
        ),
        "grant_stopped"
    );
    // A coordinator that reread the grant sees the stop itself.
    assert_eq!(
        refusal(
            backend
                .admit_child_job_run(&admission(&parent, "ORB-2", &stopped))
                .expect("stopped")
        ),
        "grant_stopped"
    );
    assert_eq!(
        backend
            .list_job_runs("task_auto_pipeline")
            .expect("children")
            .len(),
        1
    );
    // The already admitted child is untouched: stop is not cancellation.
    assert_eq!(
        backend
            .get_job_run(&child.run_id)
            .expect("child")
            .expect("child run")
            .state,
        JobRunState::Pending
    );

    // Revocation is the stronger refusal.
    let revoked = match store
        .operation_grant_transition(
            WORKSPACE,
            &grant.id,
            &transition(GrantTransitionKind::Revoke, None),
        )
        .expect("revoke")
    {
        GrantTransitionOutcome::Applied(grant) => grant,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        refusal(
            backend
                .admit_child_job_run(&admission(&parent, "ORB-3", &revoked))
                .expect("revoked")
        ),
        "grant_revoked"
    );

    // Expiry is derived from the absolute deadline, never from a status write.
    let expired = self::grant("ogrant-expired", &["ORB-5"], 1);
    let mut params = admission(&parent, "ORB-5", &expired);
    store
        .operation_grant_insert(&expired)
        .expect("insert expired");
    if let Some(authority) = params.authority.as_mut() {
        authority.now = expired.expires_at + Duration::seconds(1);
    }
    assert_eq!(
        refusal(backend.admit_child_job_run(&params).expect("expired")),
        "grant_expired"
    );
    assert_eq!(
        refusal(
            backend
                .admit_child_job_run(&ChildJobRunAdmissionParams {
                    authority: Some(ChildAdmissionAuthority {
                        grant_id: "nope".to_string(),
                        grant_revision: 1,
                        task_id: Some("ORB-5".to_string()),
                        leaf_ceiling: Some(5),
                        now: Utc::now(),
                    }),
                    ..admission(&parent, "ORB-5", &expired)
                })
                .expect("missing grant")
        ),
        "grant_missing"
    );
}

#[test]
fn a_child_without_authority_keeps_the_stop_only_guard() {
    let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), WORKSPACE);
    let parent = started_parent(&backend);
    let mut params = admission(&parent, "ORB-1", &grant("unused", &["ORB-1"], 60));
    params.authority = None;
    assert!(matches!(
        backend
            .admit_child_job_run(&params)
            .expect("legacy admission"),
        ChildJobRunAdmissionOutcome::Admitted(_)
    ));
}

#[test]
fn recovery_ledger_reserves_episodes_before_dispatch_and_never_resets() {
    let store = Store::open_in_memory().expect("store");
    let budget = RecoveryBudget {
        episodes: 2,
        seconds: 1_800,
    };
    let reserve = |run_id: &str, kind: RecoveryEpisodeKind| {
        store
            .operation_recovery_reserve(
                WORKSPACE,
                &RecoveryReserveRequest {
                    task_id: "ORB-1",
                    run_id,
                    step_id: (kind == RecoveryEpisodeKind::StepRecovery).then_some("sync_base"),
                    kind,
                    budget,
                    now: Utc::now(),
                },
            )
            .expect("reserve")
            .0
    };

    assert!(matches!(
        reserve("jrun-1", RecoveryEpisodeKind::StepRecovery),
        RecoveryReservation::Reserved {
            episode: 1,
            remaining_episodes: 1,
            ..
        }
    ));
    // Crashes count: settling records wall time against the same lineage.
    let ledger = store
        .operation_recovery_settle(WORKSPACE, "ORB-1", 1, 600, Utc::now())
        .expect("settle");
    assert_eq!(ledger.consumed_seconds, 600);
    // Settling the same episode again records the larger observation once.
    let ledger = store
        .operation_recovery_settle(WORKSPACE, "ORB-1", 1, 900, Utc::now())
        .expect("settle again");
    assert_eq!(ledger.consumed_seconds, 900);

    // Triage after a terminal run shares the budget with step recovery.
    assert!(matches!(
        reserve("jrun-2", RecoveryEpisodeKind::Triage),
        RecoveryReservation::Reserved {
            episode: 2,
            remaining_episodes: 0,
            ..
        }
    ));
    assert!(matches!(
        reserve("jrun-3", RecoveryEpisodeKind::StepRecovery),
        RecoveryReservation::Exhausted {
            reason: "recovery_episodes_exhausted",
            episodes_consumed: 2,
            ..
        }
    ));

    // Wall-time exhaustion is a separate reason.
    let (outcome, _) = store
        .operation_recovery_reserve(
            WORKSPACE,
            &RecoveryReserveRequest {
                task_id: "ORB-2",
                run_id: "jrun-4",
                step_id: None,
                kind: RecoveryEpisodeKind::Triage,
                budget: RecoveryBudget {
                    episodes: 5,
                    seconds: 60,
                },
                now: Utc::now(),
            },
        )
        .expect("reserve");
    assert!(matches!(outcome, RecoveryReservation::Reserved { .. }));
    store
        .operation_recovery_settle(WORKSPACE, "ORB-2", 1, 61, Utc::now())
        .expect("settle");
    let (outcome, ledger) = store
        .operation_recovery_reserve(
            WORKSPACE,
            &RecoveryReserveRequest {
                task_id: "ORB-2",
                run_id: "jrun-5",
                step_id: None,
                kind: RecoveryEpisodeKind::Triage,
                budget: RecoveryBudget {
                    episodes: 5,
                    seconds: 60,
                },
                now: Utc::now(),
            },
        )
        .expect("reserve");
    assert!(matches!(
        outcome,
        RecoveryReservation::Exhausted {
            reason: "recovery_minutes_exhausted",
            ..
        }
    ));
    assert_eq!(ledger.episodes.len(), 1);
    assert!(
        store
            .operation_recovery_ledger(WORKSPACE, "ORB-2")
            .expect("ledger")
            .is_some()
    );
    // A zero-episode budget is exhausted before any dispatch.
    let (outcome, _) = store
        .operation_recovery_reserve(
            WORKSPACE,
            &RecoveryReserveRequest {
                task_id: "ORB-3",
                run_id: "jrun-6",
                step_id: None,
                kind: RecoveryEpisodeKind::StepRecovery,
                budget: RecoveryBudget {
                    episodes: 0,
                    seconds: 60,
                },
                now: Utc::now(),
            },
        )
        .expect("reserve");
    assert!(matches!(outcome, RecoveryReservation::Exhausted { .. }));
}
